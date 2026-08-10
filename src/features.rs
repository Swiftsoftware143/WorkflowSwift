//! Feature limits enforcement — reads limits from plan_tiers table.

use crate::error::AppError;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn enforce_feature_limit(
    db: &PgPool,
    aid: Uuid,
    feature_key: &str,
    label: &str,
) -> Result<(), AppError> {
    // Get the account's plan tier
    let plan_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT plan_id FROM account_plans WHERE aid = $1 AND status = 'active'",
    )
    .bind(aid)
    .fetch_optional(db)
    .await?
    .flatten();

    let pid = match plan_id {
        Some(id) => id,
        None => return Ok(()), // No plan assigned — allow
    };

    // Check plan_tiers dedicated columns first
    let plan_col = match feature_key {
        "max_workflows" | "workflows" => Some("max_workflows"),
        "max_users" | "users" | "team_members" => Some("max_users"),
        _ => None,
    };

    if let Some(col) = plan_col {
        let limit: Option<i32> =
            sqlx::query_scalar(&format!("SELECT {} FROM plan_tiers WHERE id = $1", col))
                .bind(pid)
                .fetch_optional(db)
                .await?
                .flatten();

        if let Some(limit) = limit {
            if limit == -1 {
                return Ok(());
            }
            if limit == 0 {
                return Err(AppError::UpgradeRequired(format!(
                    "{} is not available on your current plan. Upgrade to access this feature.",
                    label
                )));
            }
            let usage = count_usage(db, aid, feature_key).await?;
            if usage >= limit as i64 {
                return Err(AppError::UpgradeRequired(format!(
                    "{} limit reached ({}/{}). Upgrade to increase your limit.",
                    label, usage, limit
                )));
            }
            return Ok(());
        }
    }

    // Fall back to feature_limits table
    let fl: Option<i32> = sqlx::query_scalar(
        "SELECT limit_value FROM feature_limits WHERE plan_id = $1 AND feature_key = $2",
    )
    .bind(pid)
    .bind(feature_key)
    .fetch_optional(db)
    .await?
    .flatten();

    if let Some(limit) = fl {
        if limit == -1 {
            return Ok(());
        }
        if limit == 0 {
            return Err(AppError::UpgradeRequired(format!(
                "{} is not available on your current plan. Upgrade to access this feature.",
                label
            )));
        }
        let usage = count_usage(db, aid, feature_key).await?;
        if usage >= limit as i64 {
            return Err(AppError::UpgradeRequired(format!(
                "{} limit reached ({}/{}). Upgrade to increase your limit.",
                label, usage, limit
            )));
        }
        return Ok(());
    }

    // Check JSONB features column for limits
    let json_limit: Option<i32> =
        sqlx::query_scalar("SELECT (features->>$2)::int FROM plan_tiers WHERE id = $1")
            .bind(pid)
            .bind(feature_key)
            .fetch_optional(db)
            .await?
            .flatten();

    if let Some(limit) = json_limit {
        if limit == -1 {
            return Ok(());
        }
        if limit == 0 {
            return Err(AppError::UpgradeRequired(format!(
                "{} is not available on your current plan. Upgrade to access this feature.",
                label
            )));
        }
        let usage = count_usage(db, aid, feature_key).await?;
        if usage >= limit as i64 {
            return Err(AppError::UpgradeRequired(format!(
                "{} limit reached ({}/{}). Upgrade to increase your limit.",
                label, usage, limit
            )));
        }
        return Ok(());
    }

    Ok(()) // No limit defined — allow
}

pub async fn get_usage_json(db: &PgPool, aid: Uuid) -> serde_json::Value {
    let workflows = count_usage(db, aid, "max_workflows").await.unwrap_or(0);
    let users = count_usage(db, aid, "max_users").await.unwrap_or(0);
    let templates = count_usage(db, aid, "max_templates").await.unwrap_or(0);
    let automations = count_usage(db, aid, "max_automations").await.unwrap_or(0);
    let integrations = count_usage(db, aid, "max_integrations").await.unwrap_or(0);
    let api_keys = count_usage(db, aid, "max_api_keys").await.unwrap_or(0);
    let clients = count_usage(db, aid, "max_clients").await.unwrap_or(0);
    serde_json::json!({
        "workflows": workflows,
        "users": users,
        "templates": templates,
        "automations": automations,
        "integrations": integrations,
        "api_keys": api_keys,
        "clients": clients
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
