//! Feature limits enforcement — reads limits from plan_tiers.
//!
//! Resolution order for a numeric limit (first hit wins):
//!   1. `plan_tiers.features` JSONB, the exact key (`max_templates`) and then the
//!      bare alias (`templates`) — this is what the admin Plans UI writes.
//!   2. the dedicated `plan_tiers` column (legacy: max_workflows / max_users).
//!   3. the `feature_limits` table (legacy per-plan overrides).
//!   4. not configured -> allow (backwards compatible).
//!
//! Value semantics: `-1`, a missing value, or the string `"unlimited"` means unlimited;
//! `0` means "not included in this plan"; anything else is a hard cap.
//!
//! Plan resolution: an active `account_plans` row, else `accounts.plan_id`, else the
//! lowest `sort_order` active tier (the free tier). Accounts with no plan must NOT be
//! unlimited — that made every gate in the product decorative.

use crate::error::AppError;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// The canonical numeric limit keys the admin UI can set per plan.
pub const NUMERIC_LIMIT_KEYS: [&str; 14] = [
    "max_workflows",
    "max_templates",
    "max_instances",
    "max_users",
    "max_automations",
    "max_integrations",
    "max_api_keys",
    "max_clients",
    "max_portfolio",
    "max_tags",
    "max_industries",
    "max_plans",
    "max_credit_packages",
    "retention_days",
];

/// The canonical boolean (on/off) feature keys the admin UI can set per plan.
pub const BOOLEAN_FLAG_KEYS: [&str; 10] = [
    "n8n_deploy",
    "api_access",
    "custom_branding",
    "priority_support",
    "dedicated_support",
    "sla_guarantee",
    "audit_logs",
    "custom_reports",
    "webhook_export",
    "csv_export",
];

/// Aliases accepted for a feature key — (features JSONB keys..., dedicated column).
fn aliases(key: &str) -> (&'static [&'static str], Option<&'static str>) {
    match key {
        "max_workflows" | "workflows" => (&["max_workflows", "workflows"], Some("max_workflows")),
        "max_users" | "users" | "team_members" => (&["max_users", "users"], Some("max_users")),
        "max_templates" | "templates" => (&["max_templates", "templates"], None),
        "max_instances" | "instances" => (&["max_instances", "instances"], None),
        "max_automations" | "automations" => (&["max_automations", "automations"], None),
        "max_integrations" | "integrations" => (&["max_integrations", "integrations"], None),
        "max_api_keys" | "api_keys" => (&["max_api_keys", "api_keys"], None),
        "max_clients" | "clients" => (&["max_clients", "clients"], None),
        "max_portfolio" | "portfolio" => (&["max_portfolio", "portfolio"], None),
        "max_tags" | "tags" => (&["max_tags", "tags"], None),
        "max_industries" | "industries" => (&["max_industries", "industries"], None),
        "max_plans" | "plans" => (&["max_plans", "plans"], None),
        "max_credit_packages" | "credit_packages" => {
            (&["max_credit_packages", "credit_packages"], None)
        }
        "retention_days" => (&["retention_days"], Some("retention_days")),
        "n8n_deploy" => (&["n8n_deploy", "can_deploy_n8n"], Some("can_deploy_n8n")),
        "api_access" => (&["api_access", "has_api_access"], Some("has_api_access")),
        "csv_export" => (&["csv_export", "can_export"], Some("can_export")),
        "custom_branding" => (&["custom_branding", "branding"], None),
        "priority_support" => (&["priority_support"], None),
        "dedicated_support" => (&["dedicated_support"], None),
        "sla_guarantee" => (&["sla_guarantee"], None),
        "audit_logs" => (&["audit_logs"], None),
        "custom_reports" => (&["custom_reports"], None),
        "webhook_export" => (&["webhook_export"], None),
        other => (&[], other_column(other)),
    }
}

// `match` arms need 'static strings; this keeps the catch-all arm compile-time clean.
fn other_column(_key: &str) -> Option<&'static str> {
    None
}

/// Resolve the effective plan for an account. Never returns unlimited-by-omission:
/// a plan-less account falls back to the free tier.
pub async fn resolve_plan_id(db: &PgPool, aid: Uuid) -> Result<Option<Uuid>, AppError> {
    if let Some(pid) = sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT plan_id FROM account_plans WHERE aid = $1 AND status = 'active' \
         ORDER BY started_at DESC LIMIT 1",
    )
    .bind(aid)
    .fetch_optional(db)
    .await?
    .flatten()
    {
        return Ok(Some(pid));
    }

    if let Some(pid) =
        sqlx::query_scalar::<_, Option<Uuid>>("SELECT plan_id FROM accounts WHERE id = $1")
            .bind(aid)
            .fetch_optional(db)
            .await?
            .flatten()
    {
        return Ok(Some(pid));
    }

    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM plan_tiers WHERE is_active = true ORDER BY sort_order ASC, created_at ASC LIMIT 1",
    )
    .fetch_optional(db)
    .await?)
}

/// Resolve a numeric limit for a plan. `None` = not configured (allow).
pub async fn resolve_limit(
    db: &PgPool,
    pid: Uuid,
    feature_key: &str,
) -> Result<Option<i64>, AppError> {
    let (json_keys, column) = aliases(feature_key);

    // 1. features JSONB (admin-set), exact key first then bare alias.
    for k in json_keys {
        let raw: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT features -> $2 FROM plan_tiers WHERE id = $1")
                .bind(pid)
                .bind(k)
                .fetch_optional(db)
                .await?
                .flatten();
        if let Some(v) = raw {
            if let Some(n) = v.as_i64() {
                return Ok(Some(n));
            }
            if let Some(s) = v.as_str() {
                if s.eq_ignore_ascii_case("unlimited") {
                    return Ok(Some(-1));
                }
                if let Ok(n) = s.trim().parse::<i64>() {
                    return Ok(Some(n));
                }
            }
        }
    }

    // 2. dedicated column (legacy).
    if let Some(col) = column {
        let v: Option<i32> =
            sqlx::query_scalar(&format!("SELECT {} FROM plan_tiers WHERE id = $1", col))
                .bind(pid)
                .fetch_optional(db)
                .await?
                .flatten();
        if let Some(n) = v {
            return Ok(Some(n as i64));
        }
    }

    // 3. legacy feature_limits table.
    let fl: Option<i32> = sqlx::query_scalar(
        "SELECT limit_value FROM feature_limits WHERE plan_id = $1 AND feature_key = $2",
    )
    .bind(pid)
    .bind(feature_key)
    .fetch_optional(db)
    .await?
    .flatten();
    if let Some(n) = fl {
        return Ok(Some(n as i64));
    }

    // 3b. legacy feature_limits under the bare alias.
    for k in json_keys {
        let fl: Option<i32> = sqlx::query_scalar(
            "SELECT limit_value FROM feature_limits WHERE plan_id = $1 AND feature_key = $2",
        )
        .bind(pid)
        .bind(k)
        .fetch_optional(db)
        .await?
        .flatten();
        if let Some(n) = fl {
            return Ok(Some(n as i64));
        }
    }

    Ok(None)
}

/// Enforce a numeric limit for the account's plan.
pub async fn enforce_feature_limit(
    db: &PgPool,
    aid: Uuid,
    feature_key: &str,
    label: &str,
) -> Result<(), AppError> {
    let Some(pid) = resolve_plan_id(db, aid).await? else {
        return Ok(());
    };
    let Some(limit) = resolve_limit(db, pid, feature_key).await? else {
        return Ok(());
    };
    if limit < 0 {
        return Ok(());
    }
    if limit == 0 {
        return Err(AppError::UpgradeRequired(format!(
            "{} is not included in your current plan. Upgrade to enable it.",
            label
        )));
    }
    let usage = count_usage(db, aid, feature_key).await?;
    if usage >= limit {
        return Err(AppError::UpgradeRequired(format!(
            "{} limit reached ({}/{}). Upgrade to increase your limit.",
            label, usage, limit
        )));
    }
    Ok(())
}

/// Effective value of a boolean plan flag. `true` when the plan does not configure it.
pub async fn plan_flag(db: &PgPool, aid: Uuid, feature_key: &str) -> Result<bool, AppError> {
    let Some(pid) = resolve_plan_id(db, aid).await? else {
        return Ok(true);
    };
    let (json_keys, column) = aliases(feature_key);

    for k in json_keys {
        let raw: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT features -> $2 FROM plan_tiers WHERE id = $1")
                .bind(pid)
                .bind(k)
                .fetch_optional(db)
                .await?
                .flatten();
        if let Some(v) = raw {
            if v.is_null() {
                continue;
            }
            if let Some(b) = v.as_bool() {
                return Ok(b);
            }
            if let Some(s) = v.as_str() {
                match s.trim().to_ascii_lowercase().as_str() {
                    "true" | "yes" | "on" | "1" | "enabled" => return Ok(true),
                    "false" | "no" | "off" | "0" | "disabled" => return Ok(false),
                    _ => continue,
                }
            }
            if let Some(n) = v.as_i64() {
                return Ok(n != 0);
            }
        }
    }

    if let Some(col) = column {
        let v: Option<bool> =
            sqlx::query_scalar(&format!("SELECT {} FROM plan_tiers WHERE id = $1", col))
                .bind(pid)
                .fetch_optional(db)
                .await?
                .flatten();
        if let Some(b) = v {
            return Ok(b);
        }
    }

    // A feature_limits row of 0 disables it; anything else (or absent) allows.
    let fl: Option<i32> = sqlx::query_scalar(
        "SELECT limit_value FROM feature_limits WHERE plan_id = $1 AND feature_key = $2",
    )
    .bind(pid)
    .bind(feature_key)
    .fetch_optional(db)
    .await?
    .flatten();
    if let Some(n) = fl {
        return Ok(n != 0);
    }

    Ok(true)
}

/// Enforce an on/off plan flag: off -> 402 UpgradeRequired.
pub async fn enforce_plan_flag(
    db: &PgPool,
    aid: Uuid,
    feature_key: &str,
    label: &str,
) -> Result<(), AppError> {
    if plan_flag(db, aid, feature_key).await? {
        Ok(())
    } else {
        Err(AppError::UpgradeRequired(format!(
            "{} is not included in your current plan. Upgrade to enable it.",
            label
        )))
    }
}

pub async fn get_usage_json(db: &PgPool, aid: Uuid) -> serde_json::Value {
    let workflows = count_usage(db, aid, "max_workflows").await.unwrap_or(0);
    let users = count_usage(db, aid, "max_users").await.unwrap_or(0);
    let templates = count_usage(db, aid, "max_templates").await.unwrap_or(0);
    let automations = count_usage(db, aid, "max_automations").await.unwrap_or(0);
    let integrations = count_usage(db, aid, "max_integrations").await.unwrap_or(0);
    let api_keys = count_usage(db, aid, "max_api_keys").await.unwrap_or(0);
    let clients = count_usage(db, aid, "max_clients").await.unwrap_or(0);
    let instances = count_usage(db, aid, "max_instances").await.unwrap_or(0);
    let tags = count_usage(db, aid, "max_tags").await.unwrap_or(0);
    let industries = count_usage(db, aid, "max_industries").await.unwrap_or(0);
    serde_json::json!({
        "workflows": workflows,
        "users": users,
        "templates": templates,
        "instances": instances,
        "automations": automations,
        "integrations": integrations,
        "api_keys": api_keys,
        "clients": clients,
        "tags": tags,
        "industries": industries
    })
}

async fn count_usage(db: &PgPool, aid: Uuid, feature_key: &str) -> Result<i64, AppError> {
    match feature_key {
        "max_workflows" | "workflows" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM workflows WHERE aid = $1",
        )
        .bind(aid)
        .fetch_one(db)
        .await?),
        "max_templates" | "templates" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM workflow_templates WHERE aid = $1",
        )
        .bind(aid)
        .fetch_one(db)
        .await?),
        "max_instances" | "instances" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM workflow_instances WHERE aid = $1",
        )
        .bind(aid)
        .fetch_one(db)
        .await?),
        "max_users" | "users" | "team_members" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE aid = $1 AND is_active = true",
        )
        .bind(aid)
        .fetch_one(db)
        .await?),
        "max_automations" | "automations" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM automations WHERE aid = $1",
        )
        .bind(aid)
        .fetch_one(db)
        .await?),
        "max_integrations" | "integrations" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM integration_targets WHERE aid = $1",
        )
        .bind(aid)
        .fetch_one(db)
        .await?),
        "max_api_keys" | "api_keys" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM api_keys WHERE aid = $1",
        )
        .bind(aid)
        .fetch_one(db)
        .await?),
        "max_clients" | "clients" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM clients WHERE aid = $1 AND is_active = true",
        )
        .bind(aid)
        .fetch_one(db)
        .await?),
        "max_portfolio" | "portfolio" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM portfolio_companies WHERE aid = $1",
        )
        .bind(aid)
        .fetch_one(db)
        .await?),
        "max_tags" | "tags" => Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM tags WHERE aid = $1")
                .bind(aid)
                .fetch_one(db)
                .await?,
        ),
        "max_industries" | "industries" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM account_industries WHERE aid = $1 AND is_active = true",
        )
        .bind(aid)
        .fetch_one(db)
        .await?),
        _ => Ok(0),
    }
}

/// Backwards-compat alias
pub async fn check_feature_limit(
    db: &PgPool,
    aid: Uuid,
    feature_key: &str,
) -> Result<(), AppError> {
    enforce_feature_limit(db, aid, feature_key, feature_key).await
}

/// The numeric limits currently configured for an account's plan (admin diagnostics).
pub async fn configured_limits(db: &PgPool, aid: Uuid) -> Result<serde_json::Value, AppError> {
    let Some(pid) = resolve_plan_id(db, aid).await? else {
        return Ok(serde_json::json!({}));
    };
    let mut out = serde_json::Map::new();
    for k in NUMERIC_LIMIT_KEYS {
        if let Some(v) = resolve_limit(db, pid, k).await? {
            out.insert(k.to_string(), serde_json::json!(v));
        }
    }
    for k in BOOLEAN_FLAG_KEYS {
        out.insert(
            k.to_string(),
            serde_json::json!(plan_flag(db, aid, k).await?),
        );
    }
    let row_exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM plan_tiers WHERE id = $1")
        .bind(pid)
        .fetch_optional(db)
        .await?;
    let mut value = serde_json::Value::Object(out);
    if row_exists.is_some() {
        let plan_name: Option<String> = sqlx::query("SELECT name FROM plan_tiers WHERE id = $1")
            .bind(pid)
            .fetch_optional(db)
            .await?
            .and_then(|r| r.try_get::<String, _>("name").ok());
        if let Some(obj) = value.as_object_mut() {
            obj.insert("_plan_id".into(), serde_json::json!(pid));
            obj.insert("_plan_name".into(), serde_json::json!(plan_name));
        }
    }
    Ok(value)
}
