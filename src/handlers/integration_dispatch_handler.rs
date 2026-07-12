use axum::{extract::{State, Json, Query}, response::IntoResponse, Extension};
use serde_json::json;
use std::collections::HashMap;
use uuid::Uuid;
use sqlx::Row;
use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::handlers::provider_keys_handler;

/// List available provider presets
pub async fn list_provider_presets(
    State(state): State<AppState>,
) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query("SELECT key, name, base_url, docs_url FROM integration_provider_presets ORDER BY name")
        .fetch_all(&state.db).await?;

    let presets: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "key": r.try_get::<&str,_>("key").unwrap_or(""),
            "name": r.try_get::<&str,_>("name").unwrap_or(""),
            "base_url": r.try_get::<&str,_>("base_url").unwrap_or(""),
            "docs_url": r.try_get::<Option<String>,_>("docs_url").unwrap_or(None)
        })
    }).collect();

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
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let target_id_str = params.get("target_id").ok_or_else(|| AppError::BadRequest("target_id required".into()))?;
    let target_id = Uuid::parse_str(target_id_str).map_err(|_| AppError::BadRequest("Invalid target_id".into()))?;

    let result = forward_dispatch(&state.db, target_id, tenant_id, &payload).await
        .map_err(|e| AppError::Internal(format!("Dispatch failed: {}", e)))?;

    Ok(Json(json!({
        "dispatched": true,
        "target_id": target_id_str,
        "status": result.get("status").and_then(|v| v.as_u64()),
        "response": result.get("body"),
    })))
}

/// Internal: forward a payload to an integration target using stored provider keys.
/// Used by the incoming handler to dispatch through workflow steps.
/// Returns the status code and response body from the provider.
pub async fn forward_dispatch(
    db: &sqlx::PgPool,
    target_id: Uuid,
    tenant_id: Uuid,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    // Fetch the integration target
    let row = sqlx::query(
        "SELECT it.webhook_url, it.provider_preset, pp.base_url as preset_base_url
         FROM integration_targets it
         LEFT JOIN integration_provider_presets pp ON pp.key = it.provider_preset
         WHERE it.id = $1 AND it.tenant_id = $2 AND it.is_active = true"
    )
    .bind(target_id)
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("DB error fetching integration target: {}", e))?
    .ok_or_else(|| "Integration target not found or inactive".to_string())?;

    let webhook_url: String = row.try_get("webhook_url").unwrap_or_default();
    let preset_key: Option<String> = row.try_get("provider_preset").unwrap_or(None);
    let preset_base_url: Option<String> = row.try_get("preset_base_url").unwrap_or(None);

    // Determine the provider name from the preset or webhook_url
    let provider_name = preset_key.clone().unwrap_or_else(|| {
        // Try to extract from webhook URL domain
        webhook_url.split('/').nth(2).unwrap_or("unknown").to_string()
    });

    // Look up the stored provider key from the provider_keys table
    let (api_key, stored_base_url, metadata) = provider_keys_handler::get_provider_key(db, tenant_id, &provider_name)
        .await
        .map_err(|e| format!("DB error fetching provider key: {}", e))?
        .unwrap_or_else(|| (String::new(), None, json!({})));

    // Determine the actual URL to POST to
    let effective_base_url = stored_base_url.or(preset_base_url);
    let target_url = if !webhook_url.is_empty() {
        webhook_url
    } else if let Some(ref base) = effective_base_url {
        format!("{}{}", base, payload.get("path").and_then(|v| v.as_str()).unwrap_or(""))
    } else {
        return Err("Integration target has no webhook_url or provider preset".to_string());
    };

    // Make the outbound request with stored API key injected
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let mut req = client.post(&target_url)
        .json(payload);

    // Inject stored API key as Authorization header if available
    if !api_key.is_empty() {
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
    let auth_header = payload.get("_forward_auth")
        .and_then(|v| v.as_str());
    if let Some(auth) = auth_header {
        req = req.header("Authorization", auth);
    }

    let resp = req.send().await
        .map_err(|e| format!("Failed to dispatch to provider: {}", e))?;

    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap_or(json!({"status": "ok"}));

    Ok(json!({
        "status": status,
        "body": body,
    }))
}
