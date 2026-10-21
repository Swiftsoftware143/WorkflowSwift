use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::features as plan_limits;
use crate::security::email_addr;
use crate::AppState;

/// Collect the canonical per-plan limit / flag values out of an admin plan payload.
/// They may arrive at the top level (`{"max_workflows": 1}`) or inside `features`
/// (`{"features": {"max_workflows": 1}}`); both are normalised into one map so the
/// stored `features` JSONB is the single source of truth the API enforces.
fn collect_plan_limits(req: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    if let Some(obj) = req.as_object() {
        for (k, v) in obj {
            if k == "features" {
                continue;
            }
            if plan_limits::NUMERIC_LIMIT_KEYS.contains(&k.as_str())
                || plan_limits::BOOLEAN_FLAG_KEYS.contains(&k.as_str())
                || matches!(k.as_str(), "can_deploy_n8n" | "has_api_access")
            {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    if let Some(feats) = req.get("features").and_then(|v| v.as_object()) {
        for (k, v) in feats {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// Top-level numeric limit from a normalised limit map.
fn limit_i32(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<i32> {
    map.get(key).and_then(|v| {
        v.as_i64()
            .map(|n| n as i32)
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i32>().ok()))
    })
}

/// Top-level boolean flag from a normalised limit map.
fn flag_bool(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<bool> {
    map.get(key).and_then(|v| {
        v.as_bool().or_else(|| {
            v.as_str().map(|s| {
                matches!(
                    s.trim().to_ascii_lowercase().as_str(),
                    "true" | "yes" | "on" | "1" | "enabled"
                )
            })
        })
    })
}

/// Lenient top-level i32 read from a RAW request body.
///
/// The admin console's forms submit every input as TEXT, so the strict `as_i64()` these two plan
/// handlers used before made a typed limit a silent no-op: the `COALESCE` in the UPDATE kept the
/// stored value and the panel reported "Saved!". Measured 2026-10-12: `PUT /api/v1/admin/plans/{id}`
/// with `{"max_workflows":"50"}` left the column untouched, so a tier's limits could be set once at
/// creation and never changed from the console.
fn req_i32(req: &serde_json::Value, key: &str) -> Option<i32> {
    req.get(key).and_then(|v| {
        v.as_i64()
            .map(|n| n as i32)
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i32>().ok()))
    })
}

/// Lenient top-level bool read from a RAW request body — same reason as [`req_i32`].
/// A value that is neither true-ish nor false-ish is treated as ABSENT (`None`) so the UPDATE's
/// `COALESCE` leaves the stored flag alone instead of mis-reading a typo as `false`.
fn req_bool(req: &serde_json::Value, key: &str) -> Option<bool> {
    req.get(key).and_then(|v| {
        if let Some(b) = v.as_bool() {
            return Some(b);
        }
        v.as_str()
            .map(|s| s.trim().to_ascii_lowercase())
            .and_then(|s| match s.as_str() {
                "true" | "yes" | "on" | "1" | "enabled" => Some(true),
                "false" | "no" | "off" | "0" | "disabled" => Some(false),
                _ => None,
            })
    })
}

/// Field-name hints that mark a stored value as a secret.
const SECRET_HINTS: [&str; 8] = [
    "password",
    "api_key",
    "apikey",
    "secret",
    "token",
    "private_key",
    "credential",
    "smtp_pass",
];

/// Mask a secret value — keep first/last 3 chars so an admin can tell which
/// key is configured, while the full value never leaves the server.
fn mask_secret(value: &str) -> String {
    if value.len() > 6 {
        format!("{}...{}", &value[..3], &value[value.len() - 3..])
    } else {
        "***".to_string()
    }
}

fn is_secret_field(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SECRET_HINTS.iter().any(|h| n.contains(h))
}

/// Recursively replace secret-looking string values with a masked form.
/// The raw value stays in the DB (email.rs reads it directly) — only API
/// responses are redacted.
fn redact_secrets(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if is_secret_field(k) {
                    if let serde_json::Value::String(s) = v {
                        if !s.is_empty() {
                            let masked = mask_secret(s);
                            *s = masked;
                        }
                    }
                } else {
                    redact_secrets(v);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                redact_secrets(item);
            }
        }
        _ => {}
    }
}

/// GET /api/v1/admin/settings — list all admin settings
/// The settings rows whose value carries a SEALED credential. ONE list: the key that is opened on
/// the way out (`open_row_secrets`) and sealed on the way in (`seal_row_secrets`) must be the same
/// key, or a credential would round-trip as ciphertext.
///
/// `email` (kanban t_a794cb09) and `sms` (kanban t_d3ff37ef) — the app's two outbound providers.
const SECRET_SETTINGS_KEYS: [&str; 2] = ["email", "sms"];

/// Open the credential fields of a settings row IN PLACE after a DB read, so what reaches the admin
/// panel is the credential (which `redact_secrets` then masks) and never the `enc:v1:` envelope.
/// Best-effort: a row that cannot be opened is logged and left alone rather than failing the view.
async fn open_row_secrets(pool: &sqlx::PgPool, key: &str, value: &mut serde_json::Value) {
    if !SECRET_SETTINGS_KEYS.contains(&key) {
        return;
    }
    let opened = match key {
        "email" => crate::email::open_config_secrets(pool, value).await,
        "sms" => crate::sms::open_config_secrets(pool, value).await,
        _ => return,
    };
    if let Err(e) = opened {
        tracing::error!(error = %e, key, "could not open admin_settings credentials for the admin view");
    }
}

/// Seal the credential fields of a settings row IN PLACE before it is stored, so the generic
/// `PUT /api/v1/admin/settings/{key}` route cannot store a provider key in the clear.
async fn seal_row_secrets(
    pool: &sqlx::PgPool,
    key: &str,
    value: &mut serde_json::Value,
) -> Result<(), AppError> {
    if !SECRET_SETTINGS_KEYS.contains(&key) {
        return Ok(());
    }
    let sealed = match key {
        "email" => crate::email::seal_config_secrets(pool, value).await,
        "sms" => crate::sms::seal_config_secrets(pool, value).await,
        _ => return Ok(()),
    };
    sealed.map_err(|e| AppError::Internal(format!("Failed to seal {key} credentials: {e}")))
}

/// The at-rest posture of a settings row's credential, read from the STORED value (before
/// `open_row_secrets` replaces the envelope with the plaintext). `None` for a key that carries no
/// credential. Returns `(credential_present, sealed)`.
///
/// This is what makes the panels honest: until 2026-10-16 the SMS panel asserted "sealed at rest"
/// and the Provider Keys panel asserted "encrypted at rest" unconditionally, so an operator could
/// not tell a sealed row from a legacy plaintext one (the rule CoreSwift's panel set, kanban
/// t_d1fcda7e). The panel now prints the MEASURED answer.
fn row_credential_posture(key: &str, stored: &serde_json::Value) -> Option<(bool, bool)> {
    match key {
        "email" => Some((
            crate::email::stored_credential_present(stored),
            crate::email::stored_credential_sealed(stored),
        )),
        "sms" => Some((
            crate::sms::stored_credential_present(stored),
            crate::sms::stored_credential_sealed(stored),
        )),
        _ => None,
    }
}

/// GET /api/v1/admin/settings — list all admin settings
pub async fn list_settings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let rows =
        sqlx::query("SELECT key, value, description, updated_at FROM admin_settings ORDER BY key")
            .fetch_all(&state.db)
            .await?;

    let mut settings = serde_json::Map::new();
    let mut items = Vec::new();
    for row in &rows {
        let key: String = row.try_get("key")?;
        let mut value: serde_json::Value = row.try_get("value")?;
        // At-rest posture from the STORED value, before the open below replaces the envelope.
        let posture = row_credential_posture(&key, &value);
        // Same read-path rule as `get_setting` (kanban t_a794cb09): mask the DECRYPTED credential.
        open_row_secrets(&state.db, &key, &mut value).await;
        if is_secret_field(&key) {
            if let serde_json::Value::String(s) = &value {
                if !s.is_empty() {
                    value = json!(mask_secret(s));
                }
            }
        } else {
            redact_secrets(&mut value);
        }
        let description: Option<String> = row.try_get("description").ok();
        let updated_at: chrono::DateTime<chrono::Utc> = row.try_get("updated_at")?;
        let mut item = json!({
            "key": key,
            "value": value,
            "description": description,
            "updated_at": updated_at.to_rfc3339()
        });
        if let Some((present, sealed)) = posture {
            item["credential_present"] = json!(present);
            item["sealed"] = json!(sealed);
        }
        items.push(item);
        settings.insert(
            key,
            json!({
                "value": value,
                "description": description,
                "updated_at": updated_at.to_rfc3339()
            }),
        );
    }

    Ok(Json(json!({ "settings": settings, "items": items })))
}

/// GET /api/v1/admin/settings/:key — get a specific setting
pub async fn get_setting(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(key): Path<String>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let row = sqlx::query(
        "SELECT key, value, description, updated_at FROM admin_settings WHERE key = $1",
    )
    .bind(&key)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Setting '{}' not found", key)))?;

    let key_str: String = row.try_get("key")?;
    let mut value: serde_json::Value = row.try_get("value")?;
    // The at-rest posture is read from the STORED value, BEFORE `open_row_secrets` replaces the
    // envelope with the plaintext — after the open, `sealed` is unanswerable from this object.
    let posture = row_credential_posture(&key_str, &value);
    // `admin_settings.email` carries a SEALED credential (kanban t_a794cb09): open it FIRST, so
    // the mask the admin is shown is derived from the credential and never from the envelope
    // (`enc...XYZ`), and so the panel never round-trips ciphertext as though it were the key.
    // `admin_settings.sms` is the same row shape for the SMS provider (kanban t_d3ff37ef).
    open_row_secrets(&state.db, &key_str, &mut value).await;
    if is_secret_field(&key_str) {
        if let serde_json::Value::String(s) = &value {
            if !s.is_empty() {
                value = json!(mask_secret(s));
            }
        }
    } else {
        redact_secrets(&mut value);
    }
    let description: Option<String> = row.try_get("description").ok();
    let updated_at: chrono::DateTime<chrono::Utc> = row.try_get("updated_at")?;

    let mut out = json!({
        "key": key_str,
        "value": value,
        "description": description,
        "updated_at": updated_at.to_rfc3339()
    });
    // The panel prints the MEASURED at-rest posture instead of asserting it (kanban cron-1016).
    if let Some((present, sealed)) = posture {
        out["credential_present"] = json!(present);
        out["sealed"] = json!(sealed);
    }

    Ok(Json(out))
}

/// Recursively replace values that look like our own mask (`abc...xyz`) with the
/// value already stored, so a round-trip through the admin UI can never overwrite
/// a real credential with its own mask.
fn merge_masked_secrets(new: &mut serde_json::Value, old: &serde_json::Value) {
    if let serde_json::Value::Object(map) = new {
        for (k, v) in map.iter_mut() {
            if is_secret_field(k) {
                if let serde_json::Value::String(s) = v {
                    let looks_masked = s.contains("...") || s == "***";
                    if looks_masked {
                        if let Some(serde_json::Value::String(existing)) = old.get(k) {
                            if !existing.is_empty() {
                                *s = existing.clone();
                            }
                        }
                    }
                }
            } else {
                let child = old.get(k).cloned().unwrap_or(serde_json::Value::Null);
                merge_masked_secrets(v, &child);
            }
        }
    }
}

/// PUT /api/v1/admin/settings/:key — update a setting
pub async fn update_setting(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(key): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let value = req
        .get("value")
        .ok_or_else(|| AppError::Validation("value is required".to_string()))?
        .clone();
    let description = req.get("description").and_then(|v| v.as_str());

    // Keep stored credentials when the client sends back a masked placeholder.
    let existing: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM admin_settings WHERE key = $1")
            .bind(&key)
            .fetch_optional(&state.db)
            .await?;
    let mut value = value;
    if let Some(old) = existing {
        if !is_secret_field(&key) {
            merge_masked_secrets(&mut value, &old);
        }
    }

    let admin_id = Uuid::parse_str(&claims.sub).unwrap_or(Uuid::nil());

    // `admin_settings.email` holds a credential (kanban t_a794cb09): seal it before it reaches the
    // database, so this generic route cannot store the fleet-wide Mailgun private key in the clear.
    // `admin_settings.sms` holds one too (kanban t_d3ff37ef), under the same rule.
    seal_row_secrets(&state.db, &key, &mut value).await?;

    sqlx::query(
        r#"INSERT INTO admin_settings (key, value, description, updated_at, updated_by)
           VALUES ($1, $2::jsonb, $3, NOW(), $4)
           ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, description = COALESCE($3, admin_settings.description), updated_at = NOW(), updated_by = $4"#
    )
    .bind(&key)
    .bind(value.to_string())
    .bind(description)
    .bind(admin_id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({"message": "Setting updated", "key": key})))
}

// ── Retention Settings (convenience wrapper) ──

/// GET /api/v1/admin/retention — get retention policy
pub async fn get_retention_policy(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let row = sqlx::query("SELECT value, updated_at FROM admin_settings WHERE key = 'retention'")
        .fetch_optional(&state.db)
        .await?;

    let policy = match row {
        Some(r) => {
            let val: serde_json::Value = r.try_get("value")?;
            let updated: chrono::DateTime<chrono::Utc> = r.try_get("updated_at")?;
            json!({
                "policy": val,
                "updated_at": updated.to_rfc3339()
            })
        }
        None => json!({
            "policy": {"default_days": 90, "max_days": 365, "min_days": 1},
            "updated_at": null
        }),
    };

    Ok(Json(policy))
}

/// PUT /api/v1/admin/retention — update retention policy
pub async fn update_retention_policy(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let default_days = req
        .get("default_days")
        .and_then(|v| v.as_i64())
        .unwrap_or(90);
    let max_days = req.get("max_days").and_then(|v| v.as_i64()).unwrap_or(365);
    let min_days = req.get("min_days").and_then(|v| v.as_i64()).unwrap_or(1);

    let admin_id = Uuid::parse_str(&claims.sub).unwrap_or(Uuid::nil());

    sqlx::query(
        r#"INSERT INTO admin_settings (key, value, description, updated_at, updated_by)
           VALUES ('retention', $1::jsonb, 'Global data retention policy', NOW(), $2)
           ON CONFLICT (key) DO UPDATE SET value = $1::jsonb, updated_at = NOW(), updated_by = $2"#,
    )
    .bind(
        json!({
            "default_days": default_days,
            "max_days": max_days,
            "min_days": min_days
        })
        .to_string(),
    )
    .bind(admin_id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "message": "Retention policy updated",
        "policy": {
            "default_days": default_days,
            "max_days": max_days,
            "min_days": min_days
        }
    })))
}

// ── Plan Feature Definitions ──

/// GET /api/v1/admin/feature-definitions — list all definable features
pub async fn list_feature_definitions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let rows = sqlx::query(
        "SELECT key, label, description, value_type, default_value, unit, category, sort_order, is_visible
         FROM plan_feature_definitions ORDER BY category, sort_order"
    )
    .fetch_all(&state.db)
    .await?;

    let mut features = Vec::new();
    for row in &rows {
        features.push(json!({
            "key": row.try_get::<String, _>("key")?,
            "label": row.try_get::<String, _>("label")?,
            "description": row.try_get::<Option<String>, _>("description")?,
            "value_type": row.try_get::<String, _>("value_type")?,
            "default_value": row.try_get::<serde_json::Value, _>("default_value")?,
            "unit": row.try_get::<Option<String>, _>("unit")?,
            "category": row.try_get::<String, _>("category")?,
            "sort_order": row.try_get::<i32, _>("sort_order")?,
            "is_visible": row.try_get::<bool, _>("is_visible")?
        }));
    }

    Ok(Json(json!({"features": features})))
}

// ── Plan Management (admin, full control) ──

/// GET /api/v1/admin/plans — list all plans with full details
pub async fn admin_list_plans(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let plans = sqlx::query(
        "SELECT id, name, slug, description, price_monthly::text as price_monthly,
                price_yearly::text as price_yearly, features, checkout_url,
                is_active, sort_order, created_at,
                max_workflows, max_users, retention_days, can_deploy_n8n, has_api_access,
                payment_provider, max_notify_per_hour
         FROM plan_tiers ORDER BY sort_order ASC NULLS LAST",
    )
    .fetch_all(&state.db)
    .await?;

    let mut result = Vec::new();
    for row in &plans {
        let id: Uuid = row.try_get("id")?;
        let name: String = row.try_get("name")?;
        let slug: String = row.try_get("slug")?;
        let description: Option<String> = row.try_get("description").ok();
        let price_monthly: Option<String> = row.try_get("price_monthly").ok();
        let price_yearly: Option<String> = row.try_get("price_yearly").ok();
        let features: Option<serde_json::Value> = row.try_get("features").ok();
        let checkout_url: Option<String> = row.try_get("checkout_url").ok();
        let is_active: bool = row.try_get("is_active")?;
        let sort_order: Option<i32> = row.try_get("sort_order").ok();
        let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
        // The panel must report what the GATE will do, not one of the two stores it reads.
        // These used to echo the legacy dedicated columns verbatim, while the gate resolves the
        // `features` JSONB first and only falls back to the column. On the live data that made
        // this endpoint tell the admin "no API access" for Starter/Professional/Enterprise (a
        // plan the gate let mint keys) and "no n8n deploy" for a plan the gate lets deploy.
        // Resolving through the gate's OWN helpers here -- plus the column repair in
        // migrations/069 -- is what makes panel == gate by construction (kanban t_d08c1146).
        let max_workflows = plan_limits::resolve_limit(&state.db, id, "max_workflows")
            .await?
            .map(|n| n as i32);
        let max_users = plan_limits::resolve_limit(&state.db, id, "max_users")
            .await?
            .map(|n| n as i32);
        let retention_days = plan_limits::resolve_limit(&state.db, id, "retention_days")
            .await?
            .map(|n| n as i32);
        let can_deploy_n8n = plan_limits::resolve_tier_flag(&state.db, id, "n8n_deploy").await?;
        let has_api_access = plan_limits::resolve_tier_flag(&state.db, id, "api_access").await?;
        let payment_provider: Option<String> = row.try_get("payment_provider").ok();
        // The hourly Notify cap is the ONE entitlement the gate reads from the legacy COLUMN
        // (`notify::per_hour_cap`: plan_tiers.max_notify_per_hour, else the install-wide
        // admin_settings.limits.notify_per_hour, else DEFAULT_NOTIFY_PER_HOUR), so report that
        // column -- NULL means "no plan-specific cap", which is what the panel's blank box
        // means too. It was absent from this response, so the Edit form rendered the field
        // blank for a tier whose cap WAS stored and an operator could not see it.
        let max_notify_per_hour: Option<i32> = row.try_get("max_notify_per_hour").ok();

        // Get feature_limits for this plan
        let limits =
            sqlx::query("SELECT feature_key, limit_value FROM feature_limits WHERE plan_id = $1")
                .bind(id)
                .fetch_all(&state.db)
                .await?;

        let mut limits_map = serde_json::Map::new();
        for lr in &limits {
            let fk: String = lr.try_get("feature_key")?;
            let lv: i32 = lr.try_get("limit_value")?;
            limits_map.insert(fk, json!(lv));
        }

        result.push(json!({
            "id": id.to_string(),
            "name": name,
            "slug": slug,
            "description": description,
            "price_monthly": price_monthly,
            "price_yearly": price_yearly,
            "features": features,
            "checkout_url": checkout_url,
            "is_active": is_active,
            "sort_order": sort_order,
            "created_at": created_at.to_rfc3339(),
            "max_workflows": max_workflows,
            "max_users": max_users,
            "retention_days": retention_days,
            "can_deploy_n8n": can_deploy_n8n,
            "has_api_access": has_api_access,
            "payment_provider": payment_provider,
            "max_notify_per_hour": max_notify_per_hour,
            "feature_limits": limits_map
        }));
    }

    Ok(Json(json!({"plans": result})))
}

/// POST /api/v1/admin/plans — create a new plan
pub async fn admin_create_plan(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Validation("Plan name is required".to_string()))?;
    let slug_default = name.to_lowercase().replace(' ', "-");
    let slug = req
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or(&slug_default);
    let description = req.get("description").and_then(|v| v.as_str());
    let price_monthly = req.get("price_monthly").and_then(|v| {
        v.as_str()
            .map(|s| s.to_string())
            .or_else(|| v.as_f64().map(|n| format!("{:.2}", n)))
    });
    let price_yearly = req.get("price_yearly").and_then(|v| {
        v.as_str()
            .map(|s| s.to_string())
            .or_else(|| v.as_f64().map(|n| format!("{:.2}", n)))
    });
    // Lenient: the admin panel posts form inputs as strings ("true"/"false", "50").
    let is_active = req_bool(&req, "is_active").unwrap_or(true);
    let sort_order = req_i32(&req, "sort_order");

    // Every canonical limit/flag the admin sent, in ONE place, so the stored
    // features JSONB is exactly what the API enforcement reads back.
    let limit_map = collect_plan_limits(&req);
    let features_json = serde_json::Value::Object(limit_map.clone());
    let max_workflows = limit_i32(&limit_map, "max_workflows");
    let max_users = limit_i32(&limit_map, "max_users");
    let retention_days = limit_i32(&limit_map, "retention_days");
    // The hourly outbound Notify cap. It was settable ONLY by the update route, so the panel's Add
    // form silently dropped it on a new tier while its Edit twin honoured it — one field, two
    // behaviours. Read the same way as its siblings here (kanban t_d3ff37ef introduced the cap).
    let max_notify_per_hour = req_i32(&req, "max_notify_per_hour");
    let can_deploy_n8n =
        flag_bool(&limit_map, "n8n_deploy").or(flag_bool(&limit_map, "can_deploy_n8n"));
    let has_api_access =
        flag_bool(&limit_map, "api_access").or(flag_bool(&limit_map, "has_api_access"));

    let payment_provider = req
        .get("payment_provider")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let plan = sqlx::query(
        r#"INSERT INTO plan_tiers (id, name, slug, description, price_monthly, price_yearly, features, is_active, sort_order, payment_provider,
                                  max_workflows, max_users, retention_days, can_deploy_n8n, has_api_access, max_notify_per_hour)
           VALUES ($1, $2, $3, $4, $5::numeric, $6::numeric, $7::jsonb, $8, $9, $10, $11, $12, $13, $14, $15, $16)
           RETURNING id, name, slug"#,
    )
    .bind(Uuid::new_v4())
    .bind(name)
    .bind(slug)
    .bind(description)
    .bind(price_monthly)
    .bind(price_yearly)
    .bind(features_json.to_string())
    .bind(is_active)
    .bind(sort_order)
    .bind(&payment_provider)
    .bind(max_workflows)
    .bind(max_users)
    .bind(retention_days)
    .bind(can_deploy_n8n)
    .bind(has_api_access)
    .bind(max_notify_per_hour)
    .fetch_one(&state.db)
    .await?;

    let plan_id: Uuid = plan.try_get("id")?;
    let plan_name: String = plan.try_get("name")?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "message": "Plan created",
            "plan": { "id": plan_id.to_string(), "name": plan_name, "slug": slug }
        })),
    ))
}

/// PUT /api/v1/admin/plans/:id — full plan update
pub async fn admin_update_plan_full(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    // Check exists
    let _existing = sqlx::query("SELECT id, name, slug FROM plan_tiers WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Plan not found".to_string()))?;

    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let slug = req.get("slug").and_then(|v| v.as_str()).unwrap_or("");
    let description = req.get("description").and_then(|v| v.as_str());
    let price_monthly = req.get("price_monthly").and_then(|v| {
        v.as_str()
            .map(|s| s.to_string())
            .or_else(|| v.as_f64().map(|n| format!("{:.2}", n)))
    });
    let price_yearly = req.get("price_yearly").and_then(|v| {
        v.as_str()
            .map(|s| s.to_string())
            .or_else(|| v.as_f64().map(|n| format!("{:.2}", n)))
    });
    // Lenient reads (`req_i32`/`req_bool`): the admin panel submits every field as TEXT, and the
    // previous strict `as_i64()`/`as_bool()` made a typed limit a silent no-op — the `COALESCE`
    // below kept the stored value, so a tier's price and limits could be set once at creation and
    // never changed from the console (measured 2026-10-12).
    let is_active = req_bool(&req, "is_active");
    let sort_order = req_i32(&req, "sort_order");
    let max_workflows = req_i32(&req, "max_workflows");
    let max_users = req_i32(&req, "max_users");
    let retention_days = req_i32(&req, "retention_days");
    let can_deploy_n8n = req_bool(&req, "can_deploy_n8n");
    let has_api_access = req_bool(&req, "has_api_access");
    // The hourly outbound Notify cap (kanban t_d3ff37ef). -1 = unlimited, the same convention the
    // sibling caps use; it is read live by `crate::notify::per_hour_cap`.
    let max_notify_per_hour = req_i32(&req, "max_notify_per_hour");
    let payment_provider = req
        .get("payment_provider")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let name_use = if name.is_empty() {
        let n: String = sqlx::query_scalar("SELECT name FROM plan_tiers WHERE id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
        n
    } else {
        name.to_string()
    };

    let slug_use = if slug.is_empty() {
        let s: String = sqlx::query_scalar("SELECT slug FROM plan_tiers WHERE id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
        s
    } else {
        slug.to_string()
    };

    sqlx::query(
        r#"UPDATE plan_tiers SET
            name = COALESCE($1, name),
            slug = COALESCE($2, slug),
            description = COALESCE($3, description),
            price_monthly = COALESCE($4::numeric, price_monthly),
            price_yearly = COALESCE($5::numeric, price_yearly),
            is_active = COALESCE($6, is_active),
            sort_order = COALESCE($7, sort_order),
            max_workflows = COALESCE($8, max_workflows),
            max_users = COALESCE($9, max_users),
            retention_days = COALESCE($10, retention_days),
            can_deploy_n8n = COALESCE($11, can_deploy_n8n),
            has_api_access = COALESCE($12, has_api_access),
            payment_provider = COALESCE($13, payment_provider),
            max_notify_per_hour = COALESCE($14, max_notify_per_hour)
         WHERE id = $15"#,
    )
    .bind(if name.is_empty() {
        None
    } else {
        Some(&name_use)
    })
    .bind(if slug.is_empty() {
        None
    } else {
        Some(&slug_use)
    })
    .bind(description)
    .bind(&price_monthly)
    .bind(&price_yearly)
    .bind(is_active)
    .bind(sort_order)
    .bind(max_workflows)
    .bind(max_users)
    .bind(retention_days)
    .bind(can_deploy_n8n)
    .bind(has_api_access)
    .bind(&payment_provider)
    .bind(max_notify_per_hour)
    .bind(id)
    .execute(&state.db)
    .await?;

    // Update feature limits if provided
    if let Some(feature_overrides) = req.get("feature_limits").and_then(|v| v.as_object()) {
        for (fkey, fval) in feature_overrides {
            if let Some(limit) = fval.as_i64() {
                sqlx::query(
                    r#"INSERT INTO feature_limits (plan_id, feature_key, limit_value)
                       VALUES ($1, $2, $3)
                       ON CONFLICT (plan_id, feature_key) DO UPDATE SET limit_value = $3"#,
                )
                .bind(id)
                .bind(fkey)
                .bind(limit as i32)
                .execute(&state.db)
                .await?;
            }
        }
    }

    // Persist every canonical limit/flag the admin sent into `features` JSONB.
    // MERGE, never replace: the Plans UI saves one field at a time and a replace
    // would silently wipe the limits it did not resend. The legacy dedicated
    // columns are mirrored so both views of a plan agree.
    let limit_map = collect_plan_limits(&req);
    if !limit_map.is_empty() {
        sqlx::query(
            "UPDATE plan_tiers SET features = COALESCE(features, '{}'::jsonb) || $1::jsonb \
             WHERE id = $2",
        )
        .bind(serde_json::Value::Object(limit_map.clone()).to_string())
        .bind(id)
        .execute(&state.db)
        .await?;

        sqlx::query(
            r#"UPDATE plan_tiers SET
                max_workflows  = COALESCE($1, max_workflows),
                max_users      = COALESCE($2, max_users),
                retention_days = COALESCE($3, retention_days),
                can_deploy_n8n = COALESCE($4, can_deploy_n8n),
                has_api_access = COALESCE($5, has_api_access),
                max_notify_per_hour = COALESCE($6, max_notify_per_hour)
               WHERE id = $7"#,
        )
        .bind(limit_i32(&limit_map, "max_workflows"))
        .bind(limit_i32(&limit_map, "max_users"))
        .bind(limit_i32(&limit_map, "retention_days"))
        .bind(flag_bool(&limit_map, "n8n_deploy").or(flag_bool(&limit_map, "can_deploy_n8n")))
        .bind(flag_bool(&limit_map, "api_access").or(flag_bool(&limit_map, "has_api_access")))
        .bind(limit_i32(&limit_map, "max_notify_per_hour"))
        .bind(id)
        .execute(&state.db)
        .await?;
    }

    Ok(Json(
        json!({"message": "Plan updated", "plan_id": id.to_string()}),
    ))
}

/// DELETE /api/v1/admin/plans/:id — delete a plan
pub async fn admin_delete_plan(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let result = sqlx::query("DELETE FROM plan_tiers WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Plan not found".to_string()));
    }

    Ok(Json(json!({"message": "Plan deleted"})))
}

// ── Account-level retention override ──

/// GET /api/v1/admin/accounts — list accounts with retention info
pub async fn admin_list_accounts(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    // HARNESS TRAFFIC IS NOT A CUSTOMER (measured live 2026-10-21). The fleet's probe harnesses sign
    // up through the real door, so every run mints an account tagged from the `X-Swift-Harness`
    // header (`accounts.probe_harness`); their teardown retires the identity but not the account, so
    // this list carried `Dash Probe's Workspace` (tag `cron-dash-walk`, zero users) among the real
    // workspaces. The tag is authoritative and NULL for every real customer, so hiding tagged rows
    // cannot hide a customer; `?include_probe=1` keeps them observable for support.
    let include_probe = params
        .get("include_probe")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let accounts = sqlx::query(
        r#"WITH resolved AS (
               SELECT a.id, a.name, a.account_slug, a.is_active, a.retention_days,
                      a.retention_purge_at, a.created_at,
                      COALESCE(a.probe_harness, '') AS probe_harness,
                      COALESCE(
                          (SELECT ap.plan_id FROM account_plans ap
                            WHERE ap.aid = a.id AND ap.status = 'active'
                            ORDER BY ap.started_at DESC LIMIT 1),
                          a.plan_id,
                          (SELECT pt.id FROM plan_tiers pt WHERE pt.is_active = true
                            ORDER BY pt.price_monthly ASC NULLS LAST, pt.name ASC,
                                     pt.created_at ASC LIMIT 1)
                      ) AS plan_id
               FROM accounts a
                 WHERE $1::bool
                    OR COALESCE(a.probe_harness, '') = ''
                    OR a.probe_harness ~* '^CNRY'
           )
           SELECT r.id, r.name, r.account_slug, r.is_active, r.retention_days,
                  r.retention_purge_at, r.created_at, r.probe_harness,
                  p.name as plan_name, p.slug as plan_slug,
                  (SELECT u.email FROM users u WHERE u.aid = r.id
                   ORDER BY CASE WHEN u.role IN ('owner','company_admin','admin','super_admin')
                                 THEN 0 ELSE 1 END,
                            u.created_at ASC
                   LIMIT 1) AS owner_email
           FROM resolved r
           LEFT JOIN plan_tiers p ON p.id = r.plan_id
           ORDER BY r.created_at DESC"#,
    )
    .bind(include_probe)
    .fetch_all(&state.db)
    .await?;

    let mut result = Vec::new();
    for row in &accounts {
        let id: Uuid = row.try_get("id")?;
        let name: Option<String> = row.try_get("name").ok();
        // The account's OWNER, i.e. the workspace's own user - the same value `POST
        // /admin/accounts/create` takes as `email` and stores on the `users` row it mints for the
        // new account. It used to be the literal `None`, so this list returned twelve `"email":
        // null` fields and the operator could not tell one workspace from another (kanban
        // t_81a1ba13): `email` was not a wrong COLUMN, it was never read at all. NULL now means
        // exactly one thing - an account with no user row (David Giraudy's Workspace, David Admin's
        // Workspace and the app's own account today).
        // `owner_email` carries the same value under the fleet-wide spelling the FunnelSwift and
        // missedcallrespondr operator lists use, so one operator tool reads all three.
        let email: Option<String> = row.try_get("owner_email").ok().flatten();
        let slug: Option<String> = row.try_get("account_slug").ok();
        let is_active: bool = row.try_get("is_active")?;
        let retention_days: Option<i32> = row.try_get("retention_days").ok();
        let retention_purge_at: Option<chrono::DateTime<chrono::Utc>> =
            row.try_get("retention_purge_at").ok();
        let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
        let plan_name: Option<String> = row.try_get("plan_name").ok();
        let plan_slug: Option<String> = row.try_get("plan_slug").ok();
        // The AUTHORITATIVE harness attribution (kanban t_4a744754): written at signup by the door
        // that minted the account. Published so the operator's list can badge a harness row
        // instead of GUESSING from its owner address — a harness that signed up with an
        // ordinary-looking address read as a customer. COALESCEd to '' so a pre-column row is
        // empty, never null.
        let probe_harness: String = row.try_get("probe_harness").unwrap_or_default();

        result.push(json!({
            "id": id.to_string(),
            "name": name,
            "email": email,
            "owner_email": email,
            "slug": slug,
            "is_active": is_active,
            "probe_harness": probe_harness,
            "retention_days": retention_days,
            "retention_purge_at": retention_purge_at.map(|t| t.to_rfc3339()),
            "created_at": created_at.to_rfc3339(),
            "plan": plan_name.map(|n| json!({"name": n, "slug": plan_slug})),
            "n8n_provisioned": false
        }));
    }

    Ok(Json(json!({"accounts": result})))
}

/// DELETE /api/v1/admin/accounts/:id — permanently delete account + all data
pub async fn admin_delete_account(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM accounts WHERE id = $1)")
            .bind(id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(false);

    if !exists {
        return Err(AppError::NotFound("Account not found".to_string()));
    }

    // HARD delete, so the n8n mirrors have to be retired FIRST. `workflows.aid -> accounts(id)` is
    // ON DELETE CASCADE, so `DELETE FROM accounts` below takes every `workflows` row with it; the
    // app mirrors each of those rows into n8n under the name `WFS <workflow_id>`
    // (src/n8n_converter.rs), and nothing else ever removes the mirror. Deleting the account first
    // is exactly how a `WFS <uuid>` orphan is born (kanban t_a965cf32). Refuse the whole wipe when
    // the mirrors cannot be retired — an orphaned mirror is a silent, permanent leak, while a 502
    // on an admin route is loud and retryable.
    let retired = crate::handlers::workflow_handler::retire_account_n8n_mirrors(&state, id)
        .await
        .map_err(|e| {
            AppError::Upstream(format!(
                "refusing to delete account {}: its n8n `WFS <uuid>` mirrors must be retired first and could not be ({})",
                id, e
            ))
        })?;

    sqlx::query("DELETE FROM n8n_account_config WHERE aid = $1")
        .bind(id)
        .execute(&state.db)
        .await
        .ok();

    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    tracing::info!(%id, mirrors_retired = retired, "Account and all associated data deleted");
    Ok(Json(
        json!({"status": "deleted", "account_id": id.to_string()}),
    ))
}

/// The portfolio accounts are kept by rule (never deleted from any panel): SwiftImpact, ZaarHub
/// and Giraudy Capital each hold an account in EVERY app, plus whatever the platform itself flags.
///
/// This app carries NO `is_portfolio` column to read that from — measured on the live schema
/// 2026-10-10, `accounts` is (id, name, account_slug, is_active, created_at, updated_at,
/// industry_slug, footer_year, footer_company, retention_days, retention_purge_at,
/// active_industry_sources, icon, payment_provider, hexomatic_key, sort_order, plan_id, settings),
/// and `portfolio_companies` here is a per-account CHILD table, not a tenant marker — so the marker
/// is read off the name/slug exactly as the CoreSwift leg does.
const PORTFOLIO_ACCOUNT_MARKERS: [&str; 3] = ["swiftimpact", "zaarhub", "giraudy"];

/// This app's OWN workspace — the one account carrying the app's own name. It is not recreatable
/// from the panel, so it is refused by rule. Matched by EXACT slug, never as a substring, so the
/// probe workspaces (`workflowswift-verify-probe`, `workflowswift-…`) stay deletable.
const PLATFORM_ACCOUNT_SLUG: &str = "workflowswift";

/// POST /api/v1/admin/accounts/bulk-delete — retire several accounts in one call (kanban t_265ce9f9).
///
/// This is the same operation as `DELETE /api/v1/admin/accounts/{id}` below, batched: that route
/// ALREADY hard-deletes an account and everything hanging off it (measured on the live DB
/// 2026-10-10: all 47 foreign keys referencing `accounts` are `ON DELETE CASCADE`, so no
/// hand-rolled child sweep is needed), and it retires the account's `WFS <uuid>` n8n mirrors FIRST,
/// refusing the whole wipe when they cannot be retired. This arm keeps both properties and answers
/// PER ID, so one refused or missing id cannot sink the batch — the panel shows the reason next to
/// the row that was kept.
///
/// Three deletes are refused by rule and cannot be forced from the panel:
///   * the account the caller is SIGNED IN AS — that is a lockout, not a cleanup;
///   * a portfolio company (a name/slug carrying a portfolio marker);
///   * this app's own workspace (`account_slug = 'workflowswift'`).
pub async fn admin_bulk_delete_accounts(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let raw_ids = req
        .get("ids")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let caller = Uuid::parse_str(claims.aid.trim()).ok();
    let mut deleted: Vec<String> = Vec::new();
    let mut failed: Vec<serde_json::Value> = Vec::new();

    for v in raw_ids {
        let raw = v.as_str().unwrap_or_default().trim().to_string();
        let id = match Uuid::parse_str(&raw) {
            Ok(u) => u,
            Err(_) => {
                failed.push(json!({"id": raw, "error": "not a valid id"}));
                continue;
            }
        };

        if Some(id) == caller {
            failed.push(json!({
                "id": raw,
                "error": "refusing to delete the account you are signed in as"
            }));
            continue;
        }

        let row: Option<(String, String)> =
            sqlx::query_as("SELECT name, account_slug FROM accounts WHERE id = $1")
                .bind(id)
                .fetch_optional(&state.db)
                .await?;

        let (name, slug) = match row {
            Some(r) => r,
            None => {
                failed.push(json!({"id": raw, "error": "account not found"}));
                continue;
            }
        };

        let hay = format!("{} {}", name, slug).to_lowercase();
        if let Some(hit) = PORTFOLIO_ACCOUNT_MARKERS.iter().find(|m| hay.contains(*m)) {
            failed.push(json!({
                "id": raw,
                "error": format!("refusing to delete a portfolio company ({})", hit)
            }));
            continue;
        }

        if slug.trim().eq_ignore_ascii_case(PLATFORM_ACCOUNT_SLUG) {
            failed.push(json!({
                "id": raw,
                "error": format!(
                    "refusing to delete this app's own workspace ({})",
                    PLATFORM_ACCOUNT_SLUG
                )
            }));
            continue;
        }

        // HARD delete, so the n8n mirrors have to be retired FIRST — the same rule the single-id
        // route applies, and for the same reason (a `WFS <uuid>` orphan is a silent, permanent
        // leak). Refuse this id when they cannot be retired; the rest of the batch still runs.
        if let Err(e) =
            crate::handlers::workflow_handler::retire_account_n8n_mirrors(&state, id).await
        {
            failed.push(json!({
                "id": raw,
                "error": format!(
                    "refusing to delete account {}: its n8n `WFS <uuid>` mirrors must be retired first and could not be ({})",
                    id, e
                )
            }));
            continue;
        }

        sqlx::query("DELETE FROM n8n_account_config WHERE aid = $1")
            .bind(id)
            .execute(&state.db)
            .await
            .ok();

        match sqlx::query("DELETE FROM accounts WHERE id = $1")
            .bind(id)
            .execute(&state.db)
            .await
        {
            Ok(r) if r.rows_affected() == 0 => {
                failed.push(json!({"id": raw, "error": "account not found"}))
            }
            Ok(_) => deleted.push(raw),
            Err(e) => failed.push(json!({"id": raw, "error": e.to_string()})),
        }
    }

    let status = if failed.is_empty() {
        "deleted"
    } else if deleted.is_empty() {
        "failed"
    } else {
        "partial"
    };

    Ok(Json(json!({
        "status": status,
        "deleted": deleted.len(),
        "deleted_ids": deleted,
        "failed": failed,
    })))
}

/// PUT /api/v1/admin/accounts/:id/retention — override account retention
pub async fn admin_set_account_retention(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let retention_days = req
        .get("retention_days")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| AppError::Validation("retention_days is required".to_string()))?
        as i32;

    let purge_at = req
        .get("purge_at")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    if retention_days < 1 {
        return Err(AppError::Validation(
            "retention_days must be at least 1".to_string(),
        ));
    }

    sqlx::query("UPDATE accounts SET retention_days = $1, retention_purge_at = $2 WHERE id = $3")
        .bind(retention_days)
        .bind(purge_at)
        .bind(id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({
        "message": "Account retention updated",
        "retention_days": retention_days
    })))
}

/// PUT /api/v1/admin/accounts/:id/plan — put an account on a plan (platform admin only).
///
/// This is the only supported way to change which tier an account is entitled to. Before it
/// existed there was none at all: `billing.enabled` is false (no self-serve checkout), the
/// admin console only CRUDs the tier *definitions*, and the resolver in `features.rs` reads the
/// newest active `account_plans` row first. So a signup that landed on `free` — and `free`
/// denies `api_access` — could only be moved by hand-written SQL, which made
/// `POST /api/v1/api-keys` a permanent 402 for that account.
///
/// Deliberately does NOT write `accounts.plan_id`: that column is the signup default and the
/// resolver consults it only when the account has NO active plan row, so writing both would
/// create a second, silently-diverging source of truth. It also does not touch any tier's
/// contents — what a plan grants is a product/pricing decision, not this route's business.
pub async fn admin_assign_plan(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let slug = req
        .get("plan_slug")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Validation("plan_slug is required".to_string()))?;

    let exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM accounts WHERE id = $1)")
            .bind(id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(false);
    if !exists {
        return Err(AppError::NotFound("Account not found".to_string()));
    }

    let plan: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT id, slug, name FROM plan_tiers WHERE slug = $1 AND is_active = true",
    )
    .bind(&slug)
    .fetch_optional(&state.db)
    .await?;
    let (plan_id, plan_slug, plan_name) =
        plan.ok_or_else(|| AppError::Validation(format!("No active plan with slug '{slug}'")))?;

    let mut tx = state.db.begin().await?;

    // Supersede whatever the account is on now, so the resolver (newest active row first) can
    // never be ambiguous about which tier applies. `status` is a plain varchar with no enum
    // constraint; 'superseded' is the only non-'active' value this code writes. `is_active` is
    // the legacy boolean the table carries alongside `status` and is cleared here too: nothing
    // used to write it at all, so a superseded row stayed `is_active = true` forever and any
    // reader filtering on the flag (there was one, credit_handler's next_reset) saw two live
    // rows. Migration 057 aligns the two columns for existing history and puts a partial unique
    // index on (aid) WHERE status = 'active' so the invariant is enforced by the database, not
    // by convention.
    sqlx::query(
        "UPDATE account_plans SET status = 'superseded', is_active = false, expires_at = NOW() \
         WHERE aid = $1 AND status = 'active'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;

    // Plain INSERT, never `ON CONFLICT (aid, plan_id)`: account_plans carries only its PK plus a
    // non-unique index on aid, so that conflict target is a guaranteed 500. (Migration 057 adds a
    // PARTIAL unique index on (aid) WHERE status = 'active' - targeting that from an upsert would
    // need the predicate repeated in the conflict target, which is why this stays a plain INSERT
    // inside a superseding transaction.)
    sqlx::query(
        "INSERT INTO account_plans (aid, plan_id, status, started_at) \
         VALUES ($1, $2, 'active', NOW())",
    )
    .bind(id)
    .bind(plan_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    tracing::info!(account = %id, plan = %plan_slug, "Platform admin moved an account onto a plan");

    Ok(Json(json!({
        "message": "Plan updated",
        "account_id": id.to_string(),
        "plan": { "slug": plan_slug, "name": plan_name },
    })))
}

/// GET /api/v1/admin/usage — aggregated usage stats per account
/// Shows credit balance, workflow runs, n8n provisioned status and the PLAN TIER the gates
/// enforce for that workspace (resolved exactly as `/admin/accounts` resolves it).
pub async fn admin_usage_dashboard(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let rows = sqlx::query(
        r#"WITH resolved AS (
               SELECT a.id as aid, a.name as account_name, a.created_at,
                      COALESCE(
                          (SELECT ap.plan_id FROM account_plans ap
                            WHERE ap.aid = a.id AND ap.status = 'active'
                            ORDER BY ap.started_at DESC LIMIT 1),
                          a.plan_id,
                          (SELECT pt.id FROM plan_tiers pt WHERE pt.is_active = true
                            ORDER BY pt.price_monthly ASC NULLS LAST, pt.name ASC,
                                     pt.created_at ASC LIMIT 1)
                      ) AS plan_id
               FROM accounts a
           )
           SELECT r.aid, r.account_name, r.created_at,
            COALESCE(ct.balance, 0) as credits_balance,
            COALESCE(wf.run_count, 0) as workflows_run,
            CASE WHEN n8n.aid IS NOT NULL THEN true ELSE false END as n8n_provisioned,
            p.name as plan_name,
            p.slug as plan_slug
           FROM resolved r
           LEFT JOIN (
               SELECT aid, SUM(amount) as balance
               FROM credit_transactions
               GROUP BY aid
           ) ct ON ct.aid = r.aid
           LEFT JOIN (
               SELECT aid, COUNT(*) as run_count
               FROM workflow_instances
               GROUP BY aid
           ) wf ON wf.aid = r.aid
           LEFT JOIN n8n_account_config n8n ON n8n.aid = r.aid
           LEFT JOIN plan_tiers p ON p.id = r.plan_id
           ORDER BY r.created_at DESC"#,
    )
    .fetch_all(&state.db)
    .await?;

    let mut result = Vec::new();
    for row in &rows {
        let aid: Uuid = row.try_get("aid")?;
        let account_name: Option<String> = row.try_get("account_name").ok();
        let credits_balance: i64 = row.try_get("credits_balance").unwrap_or(0);
        let workflows_run: i64 = row.try_get("workflows_run").unwrap_or(0);
        let n8n_provisioned: bool = row.try_get("n8n_provisioned").unwrap_or(false);
        let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
        // The tier the GATES enforce, resolved by the same expression /admin/accounts uses
        // (active subscription row -> accounts.plan_id -> cheapest active tier), so the plan an
        // operator reads here can never disagree with the plan a write is refused against.
        let plan_name: Option<String> = row.try_get("plan_name").ok();
        let plan_slug: Option<String> = row.try_get("plan_slug").ok();

        result.push(json!({
            "aid": aid.to_string(),
            "account_name": account_name,
            "credits_balance": credits_balance,
            "workflows_run": workflows_run,
            "n8n_provisioned": n8n_provisioned,
            "plan_name": plan_name,
            "plan_slug": plan_slug,
            "created_at": created_at.to_rfc3339()
        }));
    }

    Ok(Json(json!({"accounts": result, "total": result.len()})))
}

// ── Admin Account Creation ──

/// POST /api/v1/admin/accounts/create
/// Only super_admin can call this. Creates a new account + user with role="user",
/// assigns a plan, and sends a welcome email with login credentials.
pub async fn admin_create_account(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let email = req
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let account_name = req
        .get("account_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let plan_slug = req
        .get("plan_slug")
        .and_then(|v| v.as_str())
        .unwrap_or("starter")
        .to_string();
    let industry_slug = req
        .get("industry_slug")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if name.is_empty() || email.is_empty() || account_name.is_empty() {
        return Err(AppError::Validation(
            "name, email, and account_name are required".to_string(),
        ));
    }

    // ── Address boundary (kanban t_09e76b27) ────────────────────────────────────────────────
    // Before the duplicate SELECT and long before the INSERT. `users.email` is the login identity
    // AND the only address this account's welcome mail (which carries the temporary password) can
    // ever reach, so a malformed value would mint a customer nobody can reach. Normalises
    // (trim + lowercase) as well as validates; the normalised value is checked, stored and mailed.
    let email = email_addr::normalize(&email).map_err(AppError::Validation)?;

    // Check for existing user by email
    let existing =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE lower(email) = $1")
            .bind(&email)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

    if existing > 0 {
        return Err(AppError::Duplicate(
            "A user with this email already exists".to_string(),
        ));
    }

    // Resolve the plan BEFORE anything is created. The old order inserted the account + user and
    // SENT the welcome mail first, then looked the slug up and silently skipped `account_plans`
    // when it matched no plan — so a mistyped plan_slug answered 201 "created" with
    // email_sent=true over a tenant owned by NO plan (measured live 2026-10-11: HTTP 201,
    // account + user rows, 0 account_plans rows). Refuse instead, naming the slugs that exist,
    // and create nothing.
    let plan_id: Uuid = match sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM plan_tiers WHERE slug = $1 AND is_active = true",
    )
    .bind(&plan_slug)
    .fetch_optional(&state.db)
    .await?
    {
        Some(id) => id,
        None => {
            let valid: Vec<String> = sqlx::query_scalar(
                "SELECT slug FROM plan_tiers WHERE is_active = true ORDER BY slug",
            )
            .fetch_all(&state.db)
            .await?;
            return Err(AppError::Validation(format!(
                "unknown plan_slug '{}' — valid plans are: {}",
                plan_slug,
                valid.join(", ")
            )));
        }
    };

    // Create the account.
    // `slug` is NOT named here any more (kanban t_27bd3765): the column was dropped by
    // migrations/078_drop_accounts_slug_column.sql, and naming it would make this handler a
    // guaranteed ERROR 42703 on every admin account creation. The local was already fed from the
    // request's `account_slug`, so it is named for what it is now.
    let account_id = Uuid::new_v4();
    let account_slug = req
        .get("account_slug")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            account_name
                .to_lowercase()
                .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
                .trim_matches('-')
                .to_string()
        });

    // NOT marked with `probe_harness` on purpose (kanban t_4a744754): an AUTHENTICATED admin-console create-account
    // door, not the served, unauthenticated signup that the `X-Swift-Harness` marker exists for.
    sqlx::query(
        r#"INSERT INTO accounts (id, name, account_slug)
           VALUES ($1, $2, $3)"#,
    )
    .bind(account_id)
    .bind(&account_name)
    .bind(&account_slug)
    .execute(&state.db)
    .await?;

    // Assign the plan to the account. Plain INSERT, not
    // `ON CONFLICT (aid, plan_id)`: the live account_plans table has no unique or exclusion
    // constraint on (aid, plan_id) — only its PK on id — so that clause made every call to
    // this handler a 500 ("no unique or exclusion constraint matching the ON CONFLICT
    // specification"). account_id here is a brand-new Uuid, so there is nothing to conflict
    // with; this mirrors what `register` already does (auth/handlers.rs).
    sqlx::query(
        r#"INSERT INTO account_plans (aid, plan_id, status, started_at)
           VALUES ($1, $2, 'active', NOW())"#,
    )
    .bind(account_id)
    .bind(plan_id)
    .execute(&state.db)
    .await?;

    // Set industry if provided
    if !industry_slug.is_empty() {
        // The industry has to exist before an account can be tagged with it.
        let industry_exists: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM industries WHERE slug = $1")
                .bind(&industry_slug)
                .fetch_optional(&state.db)
                .await?
                .flatten();

        if industry_exists.is_some() {
            // `account_industries` is keyed by (aid, industry_slug) — that is the UNIQUE index
            // (`account_industries_aid_industry_slug_key`) and the pair every other writer
            // (auth/handlers.rs, workspace_handler.rs, industry_handler.rs) and every reader uses.
            // `account_id` / `industry_id` are not columns of this table; naming them made this
            // block a guaranteed ERROR 42703 on account creation (kanban t_b9c74751).
            // DO NOTHING keeps the old behaviour exactly — an existing row is left untouched, so an
            // industry the account deactivated is not silently re-activated — without the
            // check-then-insert race the deleted COUNT query carried.
            sqlx::query(
                r#"INSERT INTO account_industries (aid, industry_slug) VALUES ($1, $2)
                   ON CONFLICT (aid, industry_slug) DO NOTHING"#,
            )
            .bind(account_id)
            .bind(&industry_slug)
            .execute(&state.db)
            .await?;
        }
    }

    // Generate temp password and create user. Off the reactor, under the shared Argon2
    // semaphore (auth::api_key_auth::argon2_hash) — same bound as every other hash site.
    let temp_password = Uuid::new_v4().to_string();
    let hash = crate::auth::api_key_auth::argon2_hash(temp_password.clone()).await?;

    let user_id = Uuid::new_v4();
    let now = chrono::Utc::now();

    sqlx::query(
        r#"INSERT INTO users (id, aid, email, password_hash, name, role, is_active, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, 'user', true, $6, $6)"#,
    )
    .bind(user_id)
    .bind(account_id)
    .bind(&email)
    .bind(&hash)
    .bind(&name)
    .bind(now)
    .execute(&state.db)
    .await?;

    // Send welcome email
    let email_result = crate::email::send_email(
        &state,
        Some(account_id),
        &email,
        "welcome",
        &json!({
            "name": name,
            "email": email,
            "password": temp_password,
            "app_url": "https://app.workflowswift.com",
        }),
    )
    .await;

    match email_result {
        Ok(_) => tracing::info!("Welcome email sent to {}", email),
        Err(ref e) => tracing::error!("Failed to send welcome email to {}: {}", email, e),
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "status": "created",
            "account_id": account_id,
            "user_id": user_id,
            "email": email,
            "temporary_password": temp_password,
            "email_sent": email_result.as_ref().ok().is_some(),
        })),
    ))
}

// ── Admin Email Template Management ──

/// GET /api/v1/admin/email-templates — list all email templates
pub async fn admin_list_email_templates(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let rows = sqlx::query(
        r#"SELECT id, name, subject, body, html_body, template_type, is_html, is_default,
                      created_at, updated_at
           FROM email_templates ORDER BY name ASC"#,
    )
    .fetch_all(&state.db)
    .await?;

    let mut result = Vec::new();
    for row in &rows {
        let id: Uuid = row.try_get("id")?;
        let name: String = row.try_get("name")?;
        let subject: Option<String> = row.try_get("subject").ok();
        // The WORDING is what this screen exists to change, and the console cannot show an
        // input it never received (kanban t_ws-mailtpl, measured live 2026-10-14): without
        // these two the panel's Edit opened with empty body boxes over templates whose
        // wording is stored and sent.
        let body: Option<String> = row.try_get("body").ok();
        let html_body: Option<String> = row.try_get("html_body").ok();
        let template_type: Option<String> = row.try_get("template_type").ok();
        let is_html: Option<bool> = row.try_get("is_html").ok();
        let is_default: Option<bool> = row.try_get("is_default").ok();
        let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
        let updated_at: chrono::DateTime<chrono::Utc> = row.try_get("updated_at")?;

        result.push(json!({
            "id": id.to_string(),
            "name": name,
            "subject": subject,
            "body": body,
            "html_body": html_body,
            "template_type": template_type,
            "is_html": is_html,
            "is_default": is_default,
            "created_at": created_at.to_rfc3339(),
            "updated_at": updated_at.to_rfc3339(),
        }));
    }

    Ok(Json(json!({"templates": result, "count": result.len()})))
}

/// POST /api/v1/admin/email-templates — create a new email template
pub async fn admin_create_email_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let template_type = req
        .get("template_type")
        .and_then(|v| v.as_str())
        .unwrap_or("custom")
        .to_string();
    let subject = req.get("subject").and_then(|v| v.as_str());
    let body = req.get("body").and_then(|v| v.as_str());
    let html_body = req.get("html_body").and_then(|v| v.as_str());
    let is_html = req.get("is_html").and_then(|v| v.as_bool()).unwrap_or(true);
    let is_default = req
        .get("is_default")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if name.is_empty() {
        return Err(AppError::Validation(
            "Template name is required".to_string(),
        ));
    }

    let template_id = Uuid::new_v4();
    // The system scope — a template owned by no account — is `aid IS NULL`. It is BOUND, not written
    // into the query text: gate rule 5a (class 5) forbids a UUID literal inside SQL, and
    // UUID-into-TEXT is how a hand-copied row id silently outlives the row it named. Until
    // migrations/070_email_templates_aid_fk.sql this bound `Uuid::nil()`, the sentinel the seeded
    // system rows carried; that file normalises the sentinel to NULL and then arms
    // `email_templates_aid_fkey` (ON DELETE CASCADE), so a nil-UUID bind would now raise 23503 and
    // NULL is the value the column is meant to hold. The row this writes sits in the same place the
    // partial unique index always put it: COALESCE(aid, nil-uuid) over `aid IS NULL AND is_default`.
    // The sibling `email_templates_handler::create` that bound the same value is GONE (kanban
    // t_00e7b709): its INSERT omitted `template_type`/`subject`, so it never wrote a row, and this
    // super-admin-gated family is now the app's ONLY email-template create path.
    let aid = None::<Uuid>;
    sqlx::query(
        r#"INSERT INTO email_templates (id, aid, name, subject, body, html_body, template_type, is_html, is_default)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
    )
    .bind(template_id)
    .bind(aid)
    .bind(&name)
    .bind(subject)
    .bind(body)
    .bind(html_body)
    .bind(&template_type)
    .bind(is_html)
    .bind(is_default)
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({"id": template_id.to_string(), "status": "created"})),
    ))
}

/// PUT /api/v1/admin/email-templates/{id} — update an email template
pub async fn admin_update_email_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(template_id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let existing =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM email_templates WHERE id = $1")
            .bind(template_id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

    if existing == 0 {
        return Err(AppError::NotFound("Email template not found".to_string()));
    }

    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE email_templates SET name = $1 WHERE id = $2")
            .bind(name)
            .bind(template_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(subject) = req.get("subject").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE email_templates SET subject = $1 WHERE id = $2")
            .bind(subject)
            .bind(template_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(body) = req.get("body").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE email_templates SET body = $1 WHERE id = $2")
            .bind(body)
            .bind(template_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(html_body) = req.get("html_body").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE email_templates SET html_body = $1 WHERE id = $2")
            .bind(html_body)
            .bind(template_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(is_html) = req.get("is_html").and_then(|v| v.as_bool()) {
        sqlx::query("UPDATE email_templates SET is_html = $1 WHERE id = $2")
            .bind(is_html)
            .bind(template_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(template_type) = req.get("template_type").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE email_templates SET template_type = $1 WHERE id = $2")
            .bind(template_type)
            .bind(template_id)
            .execute(&state.db)
            .await?;
    }

    sqlx::query("UPDATE email_templates SET updated_at = NOW() WHERE id = $1")
        .bind(template_id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({"status": "updated"})))
}

/// DELETE /api/v1/admin/email-templates/{id} — delete an email template
pub async fn admin_delete_email_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(template_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let existing =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM email_templates WHERE id = $1")
            .bind(template_id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

    if existing == 0 {
        return Err(AppError::NotFound("Email template not found".to_string()));
    }

    sqlx::query("DELETE FROM email_templates WHERE id = $1")
        .bind(template_id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({"status": "deleted"})))
}

// ── Helper ──

/// POST /api/v1/impersonate — generate JWT for impersonating a tenant.
///
/// Super-admin only: without the gate ANY authenticated caller could mint a token for ANY
/// tenant (a cross-tenant takeover). The tenant registry is `accounts` — the table the rest of
/// the app uses (the legacy `tenants` table has 0 rows, so the lookup could never match).
pub async fn admin_impersonate(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let target_tenant_id = body
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("tenant_id is required".into()))?;

    let tid = Uuid::parse_str(target_tenant_id)
        .map_err(|_| AppError::BadRequest("Invalid tenant_id".into()))?;

    // Check the account (tenant) exists
    let _tenant_name: String = sqlx::query_scalar("SELECT name FROM accounts WHERE id = $1")
        .bind(tid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Tenant not found".into()))?;

    // Find a user in this tenant
    let user: Option<(String, String)> =
        sqlx::query_as("SELECT id::text, name FROM users WHERE aid = $1 LIMIT 1")
            .bind(tid)
            .fetch_optional(&state.db)
            .await?;

    let (user_id, user_name) = match user {
        Some(u) => u,
        None => return Err(AppError::NotFound("No user found in this tenant".into())),
    };

    let now = chrono::Utc::now().timestamp() as usize;
    let imp_claims = Claims {
        sub: user_id,
        aid: target_tenant_id.to_string(),
        role: "impersonated".to_string(),
        exp: now + 900,
        iat: now,
        perm_is_super_admin: Some(false),
    };

    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &imp_claims,
        &jsonwebtoken::EncodingKey::from_secret(state.config.jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(format!("JWT encode error: {}", e)))?;

    Ok(Json(serde_json::json!({
        "impersonation_token": token,
        "tenant_id": target_tenant_id,
        "user_name": user_name,
        "expires_in": 900
    })))
}

/// POST /api/v1/stop-impersonation
pub async fn admin_stop_impersonation() -> ApiResult<impl IntoResponse> {
    Ok(Json(serde_json::json!({
        "status": "impersonation_stopped",
        "note": "Drop impersonation token"
    })))
}

/// POST /api/v1/admin/settings/email/test — send a REAL test message through the
/// provider saved in Admin > Settings > Email.
///
/// The provider's actual outcome is returned (never a fabricated "OK"), so the
/// admin can prove a configuration works after entering the credentials. When
/// nothing is configured the send is skipped and the reason is reported.
pub async fn test_email_settings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let to = req
        .get("to")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if !to.contains('@') {
        return Err(AppError::Validation(
            "a valid recipient email is required".to_string(),
        ));
    }

    let vars = json!({
        "name": "Admin",
        "app_url": "https://app.workflowswift.com"
    });

    // The operator's diagnostic renders no template and resolves the GLOBAL provider, so it is
    // deliberately NOT branded (`None`): there is no account context here (see the branding skill).
    //
    // A press aimed at a reserved/harness address is a NON-SEND — the send layer suppresses it before
    // any provider request and records it as `withheld`. Answering "sent" here contradicted the
    // panel's own "Last email was NOT SENT" banner on the same screen, so the response now names the
    // truth while STILL going through `send_email` (which is what writes the history row).
    let withheld = crate::security::probe_addr::harness_domain(&to).map(str::to_string);
    match crate::email::send_email(&state, None, &to, "email_test", &vars).await {
        Ok(_) => match withheld {
            Some(domain) => Ok(Json(json!({
                "status": "withheld",
                "to": to,
                "detail": format!(
                    "'{domain}' is a reserved/harness address, so nothing was sent and no provider was contacted"
                ),
            }))),
            None => Ok(Json(json!({ "status": "sent", "to": to }))),
        },
        Err(e) => Ok(Json(json!({ "status": "error", "detail": e }))),
    }
}

/// POST /api/v1/admin/settings/sms/test — send a REAL test message through the provider saved in
/// Admin > Settings > SMS (kanban t_d3ff37ef).
///
/// The same contract as the email test: the provider's OWN outcome is returned, never a fabricated
/// "OK", so an admin can prove a configuration after entering the credentials. The number is
/// normalised to E.164 first, because that is the only shape this product dials.
pub async fn test_sms_settings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let to = req
        .get("to")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    crate::sms::normalize_phone(&to).map_err(AppError::Validation)?;

    match crate::sms::send_sms(
        &state,
        &to,
        "WorkflowSwift: your SMS provider is configured.",
    )
    .await
    {
        Ok(()) => Ok(Json(json!({ "status": "sent", "to": to }))),
        Err(e) => Ok(Json(json!({ "status": "error", "detail": e }))),
    }
}

/// `GET /api/v1/admin/email-log` — the last 50 transactional sends this installation attempted,
/// newest first (migration 091).
///
/// The Email Provider panel's `last_send_*` fields answer "what happened to the most recent
/// message?" and are overwritten by the next mail — so a credentials mail the provider refused at
/// signup used to disappear the moment any later send succeeded. This is the history behind that
/// one-line status: `withheld` = nothing was attempted (reserved/harness recipient), `failed` = the
/// transport refused it, `sent` = the provider accepted it.
pub async fn email_log(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;
    let rows = sqlx::query(
        "SELECT to_email, template, status, detail, created_at FROM email_send_log \
         ORDER BY created_at DESC LIMIT 50",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let sends: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "to": r.get::<String, _>("to_email"),
                "template": r.get::<Option<String>, _>("template"),
                "status": r.get::<String, _>("status"),
                "detail": r.get::<Option<String>, _>("detail"),
                "at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(json!({ "sends": sends })))
}

/// Tenant-admin gate. The canonical 403 for this app (`{"code":403,"error":true,
/// "message":"Admin access required"}`) — reused by surfaces_handler so there is exactly
/// one place that decides who may write tenant-wide definitions.
pub(crate) fn require_admin(claims: &Claims) -> Result<(), AppError> {
    if !claims.perm_is_super_admin.unwrap_or(false) {
        return Err(AppError::Forbidden("Admin access required".to_string()));
    }
    Ok(())
}
