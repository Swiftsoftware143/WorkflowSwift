use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::handlers::provider_keys_handler;
use crate::security::webhook_security;
use crate::AppState;
use axum::{
    extract::{Json, Query, State},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

/// List available provider presets
pub async fn list_provider_presets(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query(
        "SELECT key, name, base_url, docs_url FROM integration_provider_presets ORDER BY name",
    )
    .fetch_all(&state.db)
    .await?;

    let presets: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "key": r.try_get::<&str,_>("key").unwrap_or(""),
                "name": r.try_get::<&str,_>("name").unwrap_or(""),
                "base_url": r.try_get::<&str,_>("base_url").unwrap_or(""),
                "docs_url": r.try_get::<Option<String>,_>("docs_url").unwrap_or(None)
            })
        })
        .collect();

    Ok(Json(json!({"providers": presets})))
}

/// HTTP handler: dispatch a payload through a specific integration target.
/// n8n calls this instead of calling the provider directly.
/// WorkflowSwift looks up the stored API key from the provider_keys table
/// and forwards the request to the real provider.
pub async fn dispatch_integration(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<HashMap<String, String>>,
    Json(payload): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let target_id_str = params
        .get("target_id")
        .ok_or_else(|| AppError::BadRequest("target_id required".into()))?;
    let target_id = Uuid::parse_str(target_id_str)
        .map_err(|_| AppError::BadRequest("Invalid target_id".into()))?;

    // Security check before dispatching
    let (webhook_url, allowed_domains, daily_limit): (String, Vec<String>, i32) = sqlx::query_as::<_, (String, Vec<String>, i32)>(
        "SELECT COALESCE(webhook_url, ''), COALESCE(allowed_domains, ARRAY[]::TEXT[])::text[], COALESCE(daily_limit, 1000)
         FROM integration_targets WHERE id = $1 AND aid = $2 AND is_active = true"
    )
    .bind(target_id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?
    .ok_or_else(|| AppError::NotFound("Integration target not found or inactive".into()))?;

    webhook_security::check_webhook_security(
        &state.db,
        &target_id,
        &webhook_url,
        &allowed_domains,
        daily_limit,
    )
    .await?;

    let result = forward_dispatch(&state.db, target_id, aid, &payload).await;

    // Every attempt the guard allowed is counted against the target's daily quota, whichever way
    // it ends — that row is what check_daily_limit reads on the next call.
    match result {
        Ok(result) => {
            let status = result.get("status").and_then(|v| v.as_u64());
            webhook_security::record_delivery(
                &state.db,
                &target_id,
                &aid,
                &webhook_url,
                if status.is_some_and(|s| (200..300).contains(&s)) {
                    "success"
                } else {
                    "rejected"
                },
                status.map(|s| s as i32),
                None,
            )
            .await;

            Ok(Json(json!({
                "dispatched": true,
                "target_id": target_id_str,
                "status": status,
                "response": result.get("body"),
            })))
        }
        Err(e) => {
            webhook_security::record_delivery(
                &state.db,
                &target_id,
                &aid,
                &webhook_url,
                "failed",
                None,
                Some(&e),
            )
            .await;

            Err(AppError::Internal(format!("Dispatch failed: {}", e)))
        }
    }
}

/// Internal: forward a payload to an integration target.
/// Used by the incoming handler to dispatch through workflow steps.
///
/// Credential precedence (kanban t_c603a937): the credential stored ON THE TARGET ROW
/// (`integration_targets.api_key`) wins when it is set — a per-target operator override — and the
/// account-scoped `provider_keys` row is the fallback. Returns the status code and response body
/// from the provider.
pub async fn forward_dispatch(
    db: &sqlx::PgPool,
    target_id: Uuid,
    aid: Uuid,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    // Fetch the integration target
    let row = sqlx::query(
        "SELECT it.webhook_url, it.provider_preset, it.api_key AS target_api_key, pp.base_url as preset_base_url
         FROM integration_targets it
         LEFT JOIN integration_provider_presets pp ON pp.key = it.provider_preset
         WHERE it.id = $1 AND it.aid = $2 AND it.is_active = true",
    )
    .bind(target_id)
    .bind(aid)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("DB error fetching integration target: {}", e))?
    .ok_or_else(|| "Integration target not found or inactive".to_string())?;

    let webhook_url: String = row.try_get("webhook_url").unwrap_or_default();
    let preset_key: Option<String> = row.try_get("provider_preset").unwrap_or(None);
    let preset_base_url: Option<String> = row.try_get("preset_base_url").unwrap_or(None);
    let target_stored_key: Option<String> = row.try_get("target_api_key").unwrap_or(None);

    // Determine the provider name from the preset or webhook_url
    let provider_name = preset_key.clone().unwrap_or_else(|| {
        // Try to extract from webhook URL domain
        webhook_url
            .split('/')
            .nth(2)
            .unwrap_or("unknown")
            .to_string()
    });

    // Account-scoped routing config: base_url + metadata, and the FALLBACK credential.
    let (provider_api_key, stored_base_url, metadata) =
        provider_keys_handler::get_provider_key(db, aid, &provider_name)
            .await
            .map_err(|e| format!("DB error fetching provider key: {}", e))?
            .unwrap_or_else(|| (String::new(), None, json!({})));

    // Credential precedence (kanban t_c603a937). A credential stored ON THE TARGET ROW is the
    // per-target override and WINS; the account's `provider_keys` row is the fallback. Before this,
    // `integration_targets.api_key` was written encrypted and read by no path in `src/`, so a target
    // created WITH a credential still dispatched unauthenticated (measured on the wire: no
    // `Authorization` / `x-api-key`).
    //
    // A stored target credential that cannot be decrypted FAILS the dispatch instead of silently
    // falling back to the account key: silently dropping a credential the operator provisioned is
    // the defect class this column was carded for. (`get_provider_key` degrades the same input to
    // "no key"; that is the wrong answer for a per-target secret.)
    let (api_key, key_source) = match target_stored_key.as_deref() {
        Some(stored) if !stored.is_empty() => (
            crate::security::provider_key_crypto::decrypt_from_storage(db, stored)
                .await
                .map_err(|e| {
                    format!(
                        "integration target {} has a stored credential that cannot be decrypted: {}",
                        target_id, e
                    )
                })?,
            "target",
        ),
        _ if !provider_api_key.is_empty() => (provider_api_key, "provider"),
        _ => (String::new(), "none"),
    };

    // Determine the actual URL to POST to
    let effective_base_url = stored_base_url.or(preset_base_url);
    let target_url = if !webhook_url.is_empty() {
        webhook_url
    } else if let Some(ref base) = effective_base_url {
        format!(
            "{}{}",
            base,
            payload.get("path").and_then(|v| v.as_str()).unwrap_or("")
        )
    } else {
        return Err("Integration target has no webhook_url or provider preset".to_string());
    };

    // Make the outbound request with stored API key injected
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let mut req = client.post(&target_url).json(payload);

    // Inject the credential chosen above (target row first, provider key as fallback) as
    // Authorization + x-api-key. The value is validated as an HTTP header value first: reqwest
    // PANICS on an invalid one, so a stored value with a control character must surface as a
    // dispatch error instead of taking the process down.
    if !api_key.is_empty() {
        ensure_header_safe(&api_key).map_err(|e| {
            format!(
                "credential from {} for provider '{}' is not usable as an HTTP header: {}",
                key_source, provider_name, e
            )
        })?;
        req = req.header("Authorization", format!("Bearer {}", api_key));
        req = req.header("x-api-key", &api_key);
    }

    // If the provider expects the key in a specific header based on metadata, use that too
    if let Some(auth_type) = metadata.get("auth_type").and_then(|v| v.as_str()) {
        match auth_type {
            "basic" => req = req.header("Authorization", format!("Basic {}", api_key)),
            "x-api-key" => req = req.header("x-api-key", &api_key),
            _ => {} // Bearer is default
        }
    }

    // Forward the auth from the incoming request if the provider needs it
    let auth_header = payload.get("_forward_auth").and_then(|v| v.as_str());
    if let Some(auth) = auth_header {
        req = req.header("Authorization", auth);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Failed to dispatch to provider: {}", e))?;

    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap_or(json!({"status": "ok"}));

    Ok(json!({
        "status": status,
        "body": body,
        // Which credential authenticated the request: "target" (the row's own key),
        // "provider" (the account's provider_keys fallback) or "none" (unauthenticated).
        "credential_source": key_source,
    }))
}

/// A stored credential is about to become an HTTP header value, and reqwest panics on an invalid
/// one. Check it first so a malformed stored value surfaces as a dispatch error, not a crash.
fn ensure_header_safe(value: &str) -> Result<(), String> {
    reqwest::header::HeaderValue::from_str(value)
        .map(|_| ())
        .map_err(|_| "value carries a character an HTTP header cannot (e.g. CR/LF)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_safe_accepts_a_normal_credential() {
        assert!(ensure_header_safe("sk-abc123").is_ok());
        assert!(ensure_header_safe("key with spaces").is_ok());
    }

    #[test]
    fn header_safe_rejects_header_injection() {
        assert!(ensure_header_safe("sk-abc\r\nX-Evil: 1").is_err());
        assert!(ensure_header_safe("sk-abc\nX-Evil: 1").is_err());
        assert!(ensure_header_safe("sk-abc\rX").is_err());
    }
}
