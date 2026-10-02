//! Integration target handlers — CRUD with webhook security (domain allowlisting + daily limits).

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::features;
use crate::security::{provider_key_crypto as key_crypto, webhook_security};
use crate::AppState;
use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

pub async fn list_integration_targets(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let portfolio_filter = params
        .get("portfolio_company_id")
        .and_then(|v| Uuid::parse_str(v).ok());

    if let Some(pc_id) = portfolio_filter {
        let rows = sqlx::query(
            "SELECT id::text, aid::text, portfolio_company_id::text, user_id::text, \
             name, provider, provider_preset, webhook_url, events::text, is_active, \
             COALESCE(allowed_domains, ARRAY[]::TEXT[])::text[]::text as allowed_domains, \
             COALESCE(daily_limit, 1000)::int as daily_limit, \
             created_at::text \
             FROM integration_targets WHERE aid = $1 AND portfolio_company_id = $2 ORDER BY name",
        )
        .bind(aid)
        .bind(pc_id)
        .fetch_all(&state.db)
        .await?;
        let targets: Vec<serde_json::Value> = rows.iter().map(|r| {
            json!({
                "id": r.try_get::<&str,_>("id").unwrap_or(""),
                "name": r.try_get::<&str,_>("name").unwrap_or(""),
                "provider": r.try_get::<&str,_>("provider").unwrap_or(""),
                "provider_preset": r.try_get::<Option<&str>,_>("provider_preset").unwrap_or(None),
                "webhook_url": r.try_get::<&str,_>("webhook_url").unwrap_or(""),
                "is_active": r.try_get::<bool,_>("is_active").unwrap_or(false),
                "allowed_domains": r.try_get::<Vec<String>,_>("allowed_domains").unwrap_or_default(),
                "daily_limit": r.try_get::<i32,_>("daily_limit").unwrap_or(1000),
            })
        }).collect();
        return Ok(Json(json!({"integration_targets": targets})));
    }

    let rows = sqlx::query(
        "SELECT id::text, aid::text, portfolio_company_id::text, user_id::text, \
         name, provider, provider_preset, webhook_url, events::text, is_active, \
         COALESCE(allowed_domains, ARRAY[]::TEXT[])::text[]::text as allowed_domains, \
         COALESCE(daily_limit, 1000)::int as daily_limit, \
         created_at::text \
         FROM integration_targets WHERE aid = $1 ORDER BY name",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;
    let targets: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<&str,_>("id").unwrap_or(""),
            "name": r.try_get::<&str,_>("name").unwrap_or(""),
            "provider": r.try_get::<&str,_>("provider").unwrap_or(""),
            "provider_preset": r.try_get::<Option<&str>,_>("provider_preset").unwrap_or(None),
            "webhook_url": r.try_get::<&str,_>("webhook_url").unwrap_or(""),
            "is_active": r.try_get::<bool,_>("is_active").unwrap_or(false),
            "allowed_domains": r.try_get::<Vec<String>,_>("allowed_domains").unwrap_or_default(),
            "daily_limit": r.try_get::<i32,_>("daily_limit").unwrap_or(1000),
        })
    }).collect();
    Ok(Json(json!({"integration_targets": targets})))
}

pub async fn create_integration_target(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(
        &state.db,
        aid,
        "max_integration_targets",
        "Integration Targets",
    )
    .await?;

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Target")
        .to_string();
    let provider = req
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("webhook")
        .to_string();
    let webhook_url = req
        .get("webhook_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // The URL source (kanban t_cb839034). A target routes either to a literal `webhook_url` or to a
    // provider-preset key from the catalogue `GET /api/v1/provider-presets` serves —
    // `forward_dispatch` LEFT JOINs that catalogue and uses `preset_base_url` when the target has no
    // `webhook_url`. Before this, NOTHING in `src/` wrote this column (5/5 live rows NULL) while the
    // dispatch read it, so the preset half of the routing contract was unreachable through the app's
    // own API. This handler is the writer.
    let provider_preset: Option<String> = req
        .get("provider_preset")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    validate_preset_key(&state.db, provider_preset.as_deref()).await?;
    // A target with NEITHER a webhook_url nor a preset has no URL to dispatch to at all: refuse it
    // here instead of letting every later dispatch fail on the same misconfiguration.
    ensure_url_source(&webhook_url, provider_preset.as_deref())?;
    let pc_id = req
        .get("portfolio_company_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let user_id = req
        .get("user_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    // The credential is encrypted BEFORE it reaches the database, through the same choke point
    // provider_keys uses: 'enc:v1:' + single-line base64 ciphertext, AES-256 via pgcrypto, master
    // key in the process environment. Nothing is stored in the clear — a missing master key fails
    // this request instead of silently persisting a plaintext key (see
    // crate::security::provider_key_crypto). NOTE (kanban t_c603a937, wiring the column): the
    // read-for-use site is `integration_dispatch_handler::forward_dispatch`, which prefers a
    // credential stored on THIS row over the account's `provider_keys` entry — a per-target
    // override. Before that change the column was written here and read by no path at all, so a
    // target created with an `api_key` dispatched unauthenticated. A missing `api_key` stays SQL
    // NULL; an empty string stays '' (encrypt_for_storage returns '' for empty input), which is
    // what "no credential stored" looks like on this table — both mean "fall back to provider_keys".
    let api_key: Option<String> = match req.get("api_key").and_then(|v| v.as_str()) {
        Some(v) => Some(key_crypto::encrypt_for_storage(&state.db, v).await?),
        None => None,
    };
    let allowed_domains: Vec<String> = req
        .get("allowed_domains")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let daily_limit: i32 = req
        .get("daily_limit")
        .and_then(|v| v.as_i64())
        .map(|n| n as i32)
        .unwrap_or(1000);

    // Validate webhook URL against allowed domains. A preset-routed target legitimately carries no
    // webhook_url, so the empty string is not run through the URL parser (which rejects it as an
    // invalid URL) — its destination is the preset's catalogue base_url, and the dispatch gate
    // validates THAT (see integration_dispatch_handler::dispatch_integration).
    if !webhook_url.trim().is_empty() {
        webhook_security::validate_webhook_url(&webhook_url, &allowed_domains)
            .map_err(|msg| AppError::Validation(format!("Webhook URL rejected: {}", msg)))?;
    }

    let id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO integration_targets (id, aid, portfolio_company_id, user_id, name, provider, provider_preset, webhook_url, api_key, allowed_domains, daily_limit) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"
    )
    .bind(id).bind(aid).bind(pc_id).bind(user_id)
    .bind(&name).bind(&provider).bind(provider_preset.as_deref()).bind(&webhook_url).bind(api_key.as_deref())
    .bind(&allowed_domains).bind(daily_limit)
    .execute(&state.db).await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id.to_string(),
            "name": name,
            "provider": provider,
            "provider_preset": provider_preset,
            "webhook_url": webhook_url,
            "allowed_domains": allowed_domains,
            "daily_limit": daily_limit,
        })),
    ))
}

pub async fn update_integration_target(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let target_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid ID".into()))?;

    // Fetch existing to validate webhook URL if it's being changed
    let existing = sqlx::query(
        "SELECT webhook_url, provider_preset, COALESCE(allowed_domains, ARRAY[]::TEXT[])::text[] as allowed_domains \
         FROM integration_targets WHERE id = $1 AND aid = $2"
    )
    .bind(target_id).bind(aid)
    .fetch_optional(&state.db).await?
    .ok_or_else(|| AppError::NotFound("Integration target not found".into()))?;

    let existing_webhook: String = existing.try_get("webhook_url").unwrap_or_default();
    let existing_preset: Option<String> = existing.try_get("provider_preset").unwrap_or(None);
    let existing_domains: Vec<String> = existing.try_get("allowed_domains").unwrap_or_default();

    let webhook_url = req
        .get("webhook_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&existing_webhook)
        .to_string();
    // provider_preset (kanban t_cb839034): absent = leave alone, a string = set it (validated
    // against the catalogue), null = clear it. Same three-way contract as api_key below, so an
    // operator can rotate a target onto a preset and back off it again.
    let provider_preset: Option<Option<String>> = match req.get("provider_preset") {
        None => None,
        Some(v) if v.is_null() => Some(None),
        Some(v) => match v.as_str() {
            Some(s) => {
                let trimmed = s.trim();
                Some(if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                })
            }
            None => {
                return Err(AppError::BadRequest(
                    "provider_preset must be a string, or null to clear it".into(),
                ))
            }
        },
    };
    validate_preset_key(
        &state.db,
        provider_preset.as_ref().and_then(|p| p.as_deref()),
    )
    .await?;
    let effective_preset: Option<String> = match &provider_preset {
        Some(v) => v.clone(),
        None => existing_preset,
    };
    ensure_url_source(&webhook_url, effective_preset.as_deref())?;
    let allowed_domains: Vec<String> = req
        .get("allowed_domains")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or(existing_domains);

    // Validate webhook URL against allowed domains if either changed. A preset-routed target keeps
    // an empty webhook_url on purpose, so only a non-empty URL goes through the domain check.
    if !webhook_url.trim().is_empty() {
        webhook_security::validate_webhook_url(&webhook_url, &allowed_domains)
            .map_err(|msg| AppError::Validation(format!("Webhook URL rejected: {}", msg)))?;
    }

    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE integration_targets SET name = $1, updated_at = NOW() WHERE id = $2 AND aid = $3")
            .bind(name).bind(target_id).bind(aid).execute(&state.db).await?;
    }
    if req.get("webhook_url").is_some() {
        sqlx::query("UPDATE integration_targets SET webhook_url = $1, updated_at = NOW() WHERE id = $2 AND aid = $3")
            .bind(&webhook_url).bind(target_id).bind(aid).execute(&state.db).await?;
    }
    if let Some(active) = req.get("is_active").and_then(|v| v.as_bool()) {
        sqlx::query("UPDATE integration_targets SET is_active = $1, updated_at = NOW() WHERE id = $2 AND aid = $3")
            .bind(active).bind(target_id).bind(aid).execute(&state.db).await?;
    }
    // The preset writer (kanban t_cb839034). Only reached when the request carried the field, so a
    // PUT that does not mention provider_preset leaves the routing source untouched.
    if let Some(preset) = &provider_preset {
        sqlx::query(
            "UPDATE integration_targets SET provider_preset = $1, updated_at = NOW() WHERE id = $2 AND aid = $3",
        )
        .bind(preset.as_deref())
        .bind(target_id)
        .bind(aid)
        .execute(&state.db)
        .await?;
    }
    if req.get("allowed_domains").is_some() || req.get("daily_limit").is_some() {
        sqlx::query(
            "UPDATE integration_targets SET allowed_domains = $1, daily_limit = $2, updated_at = NOW() WHERE id = $3 AND aid = $4"
        )
        .bind(&allowed_domains)
        .bind(req.get("daily_limit").and_then(|v| v.as_i64()).map(|n| n as i32).unwrap_or(1000))
        .bind(target_id).bind(aid)
        .execute(&state.db).await?;
    }

    // Credential rotation / clear (kanban t_c603a937). `forward_dispatch` now reads this column as
    // the per-target override, so it must be maintainable here too: without this arm the column
    // could only ever be set at create time and an operator could never rotate a leaked key.
    //   {"api_key": "sk-..."} -> encrypted, replaces the stored credential
    //   {"api_key": null}     -> clears the slot (SQL NULL); the dispatch falls back to provider_keys
    //   {"api_key": ""}       -> stores the empty slot, same effect (encrypt_for_storage returns '')
    if let Some(v) = req.get("api_key") {
        match v.as_str() {
            Some(s) => {
                let encrypted = key_crypto::encrypt_for_storage(&state.db, s).await?;
                sqlx::query(
                    "UPDATE integration_targets SET api_key = $1, updated_at = NOW() WHERE id = $2 AND aid = $3",
                )
                .bind(encrypted)
                .bind(target_id)
                .bind(aid)
                .execute(&state.db)
                .await?;
            }
            None if v.is_null() => {
                sqlx::query(
                    "UPDATE integration_targets SET api_key = NULL, updated_at = NOW() WHERE id = $1 AND aid = $2",
                )
                .bind(target_id)
                .bind(aid)
                .execute(&state.db)
                .await?;
            }
            None => {
                return Err(AppError::BadRequest(
                    "api_key must be a string, or null to clear it".into(),
                ))
            }
        }
    }

    Ok(Json(json!({"status": "updated"})))
}

pub async fn delete_integration_target(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let target_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid ID".into()))?;
    sqlx::query("DELETE FROM integration_targets WHERE id = $1 AND aid = $2")
        .bind(target_id)
        .bind(aid)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({"status": "deleted"})))
}

/// The provider catalogue this app's own routing contract reads.
///
/// `integration_provider_presets` is served to the console by
/// `integration_dispatch_handler::list_provider_presets` (`GET /api/v1/provider-presets`)
/// and LEFT JOINed by `forward_dispatch` to turn a target with no `webhook_url` into a URL
/// (`pp.base_url AS preset_base_url`). Until kanban t_cb839034 nothing in `src/` wrote the
/// `integration_targets.provider_preset` column that join keys on, so the half of the contract that
/// makes a preset routable was unreachable through the app's own API. These are its writers.
///
/// A key that is not in the catalogue is refused as a FIELD-LEVEL 422 naming the valid keys: the
/// column is FK-bound, so storing an unknown key would otherwise surface as a 23503 on the INSERT
/// (a 500 for what is really a bad request field).
async fn validate_preset_key(db: &sqlx::PgPool, key: Option<&str>) -> Result<(), AppError> {
    let Some(key) = key else {
        return Ok(());
    };
    let keys = sqlx::query_scalar::<_, String>(
        "SELECT key FROM integration_provider_presets ORDER BY key",
    )
    .fetch_all(db)
    .await?;
    if !keys.iter().any(|k| k == key) {
        return Err(AppError::Validation(format!(
            "provider_preset '{}' is not in the provider catalogue. Valid presets: {}",
            key,
            keys.join(", ")
        )));
    }
    Ok(())
}

/// A target must name a URL source: a literal `webhook_url`, or a `provider_preset` whose catalogue
/// `base_url` the dispatch resolves. A target with neither has no destination at all, and every
/// dispatch would fail on it — refuse the write instead.
fn ensure_url_source(webhook_url: &str, provider_preset: Option<&str>) -> Result<(), AppError> {
    if webhook_url.trim().is_empty() && provider_preset.is_none() {
        return Err(AppError::Validation(
            "an integration target needs a webhook_url or a provider_preset: a target with neither \
             has no URL to dispatch to"
                .into(),
        ));
    }
    Ok(())
}
