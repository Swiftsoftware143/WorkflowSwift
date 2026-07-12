use axum::{
    extract::{State, Json, Path, Query},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use std::collections::HashMap;
use uuid::Uuid;
use sqlx::Row;
use rand::Rng;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;

// ──────────────────────────────────────────────
// Destination fetch — powers the 3-level cascade
// ──────────────────────────────────────────────

/// Get the base URL for a given provider key (from the tenant's stored provider_key or the preset)
async fn get_provider_base_url(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    provider: &str,
) -> Option<String> {
    // First check provider_keys for a stored base_url
    if let Ok(row) = sqlx::query_scalar::<_, Option<String>>(
        "SELECT base_url FROM provider_keys WHERE tenant_id = $1 AND provider = $2 AND is_active = true"
    )
    .bind(tenant_id)
    .bind(provider)
    .fetch_optional(db)
    .await
    {
        if let Some(Some(url)) = row {
            if !url.is_empty() {
                return Some(url.trim_end_matches('/').to_string());
            }
        }
    }

    // Fall back to provider presets
    if let Ok(row) = sqlx::query_scalar::<_, Option<String>>(
        "SELECT base_url FROM integration_provider_presets WHERE key = $1"
    )
    .bind(provider)
    .fetch_optional(db)
    .await
    {
        if let Some(Some(url)) = row {
            if !url.is_empty() {
                return Some(url.trim_end_matches('/').to_string());
            }
        }
    }

    // Hardcoded defaults for native Swift products
    match provider {
        "coreswift" => Some("http://localhost:8084".to_string()),
        "funnelswift" => Some("https://api.funnelswift.app/v1".to_string()),
        "incentiveswift" => Some("http://localhost:8090".to_string()),
        _ => None,
    }
}

/// Fetch API key for a given provider from the tenant's stored keys
async fn get_provider_api_key(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    provider: &str,
) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT api_key FROM provider_keys WHERE tenant_id = $1 AND provider = $2 AND is_active = true"
    )
    .bind(tenant_id)
    .bind(provider)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// GET /api/v1/integration-destinations?provider=coreswift&action=create_contact
/// Returns available destination types and the endpoint to fetch live values
pub async fn get_destinations(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let provider = params.get("provider").map(|s| s.as_str()).unwrap_or("");
    let action = params.get("action").map(|s| s.as_str()).unwrap_or("");

    // If no provider, return all unique providers available to this user
    if provider.is_empty() {
        let rows = sqlx::query(
            "SELECT DISTINCT ap.key, ap.name, ap.description, ap.icon
             FROM available_providers ap
             ORDER BY ap.name"
        )
        .fetch_all(&state.db)
        .await?;

        let providers: Vec<serde_json::Value> = rows.iter().map(|r| {
            json!({
                "key": r.try_get::<&str,_>("key").unwrap_or(""),
                "name": r.try_get::<&str,_>("name").unwrap_or(""),
                "description": r.try_get::<Option<&str>,_>("description").unwrap_or(None),
                "icon": r.try_get::<Option<&str>,_>("icon").unwrap_or(None),
                "is_native": matches!(r.try_get::<&str,_>("key"), Ok("coreswift"|"funnelswift"|"incentiveswift"))
            })
        }).collect();

        return Ok(Json(json!({"providers": providers})));
    }

    // If provider but no action, return actions for this provider
    if action.is_empty() {
        let rows = sqlx::query(
            "SELECT DISTINCT action_key, action_label
             FROM integration_destinations
             WHERE provider = $1
             ORDER BY action_label"
        )
        .bind(provider)
        .fetch_all(&state.db)
        .await?;

        let actions: Vec<serde_json::Value> = rows.iter().map(|r| {
            json!({
                "key": r.try_get::<&str,_>("action_key").unwrap_or(""),
                "label": r.try_get::<&str,_>("action_label").unwrap_or("")
            })
        }).collect();

        return Ok(Json(json!({"actions": actions})));
    }

    // Return destination types for this provider+action
    let rows = sqlx::query(
        "SELECT destination_type, destination_label, sort_order
         FROM integration_destinations
         WHERE provider = $1 AND action_key = $2
         ORDER BY sort_order"
    )
    .bind(provider)
    .bind(action)
    .fetch_all(&state.db)
    .await?;

    let destinations: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "type": r.try_get::<&str,_>("destination_type").unwrap_or(""),
            "label": r.try_get::<&str,_>("destination_label").unwrap_or("")
        })
    }).collect();

    Ok(Json(json!({"destinations": destinations})))
}

/// GET /api/v1/integration-destinations/values
/// Fetch live destination values for a given provider + destination type
/// e.g. ?provider=coreswift&destination_type=list returns the user's actual CoreSwift lists
pub async fn get_destination_values(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let provider = params.get("provider").map(|s| s.as_str()).unwrap_or("");
    let dest_type = params.get("destination_type").map(|s| s.as_str()).unwrap_or("");

    if provider.is_empty() || dest_type.is_empty() {
        return Err(AppError::BadRequest("provider and destination_type are required".into()));
    }

    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Try to fetch live values from the provider's API
    // If the provider API is unreachable, fall back to cached/static values
    let live_values = fetch_live_destination_values(&state.db, tenant_id, provider, dest_type).await;

    // If we got live values, return them
    if let Some(values) = live_values {
        return Ok(Json(json!({"values": values, "source": "live"})));
    }

    // Fall back to static/placeholder values so the UI never breaks
    let fallback = match (provider, dest_type) {
        ("coreswift", "list") => vec![
            json!({"id": "default-list", "label": "Default List"}),
        ],
        ("coreswift", "tags") => vec![
            json!({"id": "default-tag", "label": "default"}),
        ],
        ("coreswift", "pipeline_stage") => vec![
            json!({"id": "qualified", "label": "Qualified"}),
            json!({"id": "won", "label": "Won"}),
            json!({"id": "lost", "label": "Lost"}),
        ],
        ("funnelswift", "landing_page") => vec![
            json!({"id": "default-page", "label": "Default Page"}),
        ],
        ("funnelswift", "tags") => vec![
            json!({"id": "default-tag", "label": "default"}),
        ],
        ("incentiveswift", "campaign") => vec![
            json!({"id": "default-campaign", "label": "Default Campaign"}),
        ],
        ("incentiveswift", "milestone_level") => vec![
            json!({"id": "bronze", "label": "Bronze"}),
            json!({"id": "silver", "label": "Silver"}),
            json!({"id": "gold", "label": "Gold"}),
        ],
        _ => vec![json!({"id": "default", "label": "Default"})],
    };

    Ok(Json(json!({"values": fallback, "source": "fallback"})))
}

/// Attempt to fetch live destination values from the provider's API
async fn fetch_live_destination_values(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    provider: &str,
    dest_type: &str,
) -> Option<Vec<serde_json::Value>> {
    let base_url = get_provider_base_url(db, tenant_id, provider).await?;
    let api_key = match get_provider_api_key(db, tenant_id, provider).await {
        Some(k) if !k.is_empty() => k,
        _ => return None,
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;

    let url = match (provider, dest_type) {
        ("coreswift", "list") => format!("{}/api/lists", base_url),
        ("coreswift", "tags") => format!("{}/api/tags", base_url),
        ("coreswift", "pipeline_stage") => format!("{}/api/pipelines", base_url),
        ("coreswift", "category") => format!("{}/api/contacts/categories", base_url),
        ("funnelswift", "landing_page") => format!("{}/landing_pages", base_url),
        ("funnelswift", "tags") => format!("{}/tags", base_url),
        ("incentiveswift", "campaign") => format!("{}/api/campaigns", base_url),
        ("incentiveswift", "milestone_level") => format!("{}/api/milestones", base_url),
        _ => return None,
    };

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let body: serde_json::Value = resp.json().await.ok()?;

    // Parse the response based on provider and destination type
    // The response format varies by provider — extract the relevant array
    Some(parse_provider_response(provider, dest_type, &body))
}

/// Parse provider API responses into the standard {id, label} destination format
fn parse_provider_response(
    provider: &str,
    dest_type: &str,
    body: &serde_json::Value,
) -> Vec<serde_json::Value> {
    match (provider, dest_type) {
        ("coreswift", "list") => {
            // CoreSwift returns { lists: [{id, name}, ...] }
            extract_array(body, "lists", "id", "name")
                .unwrap_or_else(|| extract_array(body, "data", "id", "name")
                .unwrap_or_default())
        }
        ("coreswift", "tags") => {
            extract_array(body, "tags", "id", "name")
                .unwrap_or_else(|| extract_array(body, "data", "id", "name")
                .unwrap_or_default())
        }
        ("coreswift", "pipeline_stage") => {
            // CoreSwift pipelines: { pipelines: [{id, name, stages: [{id, name}, ...]}] }
            // Flatten all stages across all pipelines
            if let Some(pipelines) = body.get("pipelines").and_then(|v| v.as_array()) {
                let mut stages = Vec::new();
                for pipeline in pipelines {
                    if let Some(stage_list) = pipeline.get("stages").and_then(|v| v.as_array()) {
                        for stage in stage_list {
                            let id = stage.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let label = stage.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            if !id.is_empty() {
                                stages.push(json!({"id": id, "label": label}));
                            }
                        }
                    }
                }
                return stages;
            }
            Vec::new()
        }
        ("funnelswift", "landing_page") => {
            extract_array(body, "landing_pages", "id", "name")
                .unwrap_or_else(|| extract_array(body, "data", "id", "name")
                .unwrap_or_default())
        }
        ("funnelswift", "tags") | ("incentiveswift", "campaign") | ("incentiveswift", "milestone_level") => {
            extract_array(body, "data", "id", "name")
                .unwrap_or_else(|| extract_array(body, "campaigns", "id", "name")
                .unwrap_or_else(|| extract_array(body, "milestones", "id", "name")
                .unwrap_or_default()))
        }
        _ => Vec::new(),
    }
}

/// Extract an array of {id, label} objects from a JSON response given the array key and field names
fn extract_array(
    body: &serde_json::Value,
    array_key: &str,
    id_field: &str,
    label_field: &str,
) -> Option<Vec<serde_json::Value>> {
    let arr = body.get(array_key)?.as_array()?;
    if arr.is_empty() {
        return None;
    }

    // If the first element has the id_field and label_field, use those
    if let Some(first) = arr.first() {
        let has_id = first.get(id_field).and_then(|v| v.as_str()).is_some();
        let has_label = first.get(label_field).and_then(|v| v.as_str()).is_some();
        if !has_id || !has_label {
            // Try alternative field names
            for alt_id in &["id", "uuid", "slug", "key"] {
                for alt_label in &["name", "title", "label", "display_name"] {
                    if first.get(alt_id).and_then(|v| v.as_str()).is_some()
                        && first.get(alt_label).and_then(|v| v.as_str()).is_some()
                    {
                        return Some(arr.iter().map(|item| {
                            json!({
                                "id": item.get(alt_id).and_then(|v| v.as_str()).unwrap_or(""),
                                "label": item.get(alt_label).and_then(|v| v.as_str()).unwrap_or(""),
                            })
                        }).collect());
                    }
                }
            }
            // If field names don't match, use the raw id field and fallback
            if let Some(raw_id) = first.get("id").or_else(|| first.get("uuid")) {
                return Some(arr.iter().map(|_item| {
                    let id = raw_id.as_str().unwrap_or("");
                    json!({"id": id, "label": id})
                }).collect());
            }
            return None;
        }
    }

    Some(arr.iter().map(|item| {
        json!({
            "id": item.get(id_field).and_then(|v| v.as_str()).unwrap_or(""),
            "label": item.get(label_field).and_then(|v| v.as_str()).unwrap_or(""),
        })
    }).collect())
}

// ──────────────────────────────────────────────
// Integration Center — user-facing key management
// ──────────────────────────────────────────────

/// GET /api/v1/user-keys
/// List user's auto-generated API keys (masked)
pub async fn list_user_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        "SELECT id::text, key_type, key_prefix, label, is_active, last_used_at::text, created_at::text
         FROM user_api_keys
         WHERE user_id = $1
         ORDER BY created_at DESC"
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;

    let keys: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<&str,_>("id").unwrap_or(""),
            "key_type": r.try_get::<&str,_>("key_type").unwrap_or(""),
            "prefix": r.try_get::<&str,_>("key_prefix").unwrap_or(""),
            "label": r.try_get::<Option<&str>,_>("label").unwrap_or(None),
            "is_active": r.try_get::<bool,_>("is_active").unwrap_or(false),
            "last_used_at": r.try_get::<Option<&str>,_>("last_used_at").unwrap_or(None),
            "created_at": r.try_get::<&str,_>("created_at").unwrap_or("")
        })
    }).collect();

    Ok(Json(json!({"keys": keys})))
}

/// POST /api/v1/user-keys/generate
/// Generate a new API key of a given type
pub async fn generate_user_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let key_type = req.get("key_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("key_type is required (primary, webhook_secret, surface_token)".into()))?;

    if !["primary", "webhook_secret", "surface_token"].contains(&key_type) {
        return Err(AppError::BadRequest("Invalid key_type. Must be primary, webhook_secret, or surface_token".into()));
    }

    let label = req.get("label").and_then(|v| v.as_str()).unwrap_or("");

    // Generate the key
    let random_part: String = (0..32).map(|_| {
        let idx = rand::thread_rng().gen_range(0..36);
        "0123456789abcdefghijklmnopqrstuvwxyz"[idx..idx+1].to_string()
    }).collect();

    let prefix = match key_type {
        "primary" => "wf_swift_",
        "webhook_secret" => "whsec_",
        "surface_token" => "sf_",
        _ => "key_"
    };

    let raw_key = format!("{}{}", prefix, random_part);

    // Hash for storage
    let salt = argon2::password_hash::SaltString::generate(&mut rand::thread_rng());
    let hash = argon2::PasswordHasher::hash_password(&argon2::Argon2::default(), raw_key.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(format!("Hashing error: {}", e)))?;
    let key_hash = hash.serialize().to_string();

    let id = Uuid::new_v4();
    let prefix_display = &raw_key[..raw_key.len().min(12)];

    sqlx::query(
        "INSERT INTO user_api_keys (id, user_id, tenant_id, key_type, key_hash, key_prefix, label)
         VALUES ($1, $2, $3, $4, $5, $6, $7)"
    )
    .bind(id)
    .bind(user_id)
    .bind(tenant_id)
    .bind(key_type)
    .bind(&key_hash)
    .bind(prefix_display)
    .bind(if label.is_empty() { None } else { Some(label) })
    .execute(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({
        "id": id.to_string(),
        "key": raw_key,
        "prefix": prefix_display,
        "key_type": key_type,
        "message": "Save this key — it will not be shown again"
    }))))
}

/// DELETE /api/v1/user-keys/{id}
/// Revoke a user's API key
pub async fn revoke_user_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let key_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid key ID".into()))?;

    let result = sqlx::query("DELETE FROM user_api_keys WHERE id = $1 AND user_id = $2")
        .bind(key_id)
        .bind(user_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Key not found".into()));
    }

    Ok(Json(json!({"status": "revoked"})))
}

// ──────────────────────────────────────────────
// Auto-generate keys on registration
// ──────────────────────────────────────────────

/// Called after user registration to create initial keys
pub async fn seed_user_keys(
    db: &sqlx::PgPool,
    user_id: Uuid,
    tenant_id: Uuid,
) -> Result<(), sqlx::Error> {
    use argon2::password_hash::SaltString;
    use argon2::PasswordHasher;

    let key_types = ["primary", "webhook_secret", "surface_token"];
    let prefixes = ["wf_swift_", "whsec_", "sf_"];

    for (i, key_type) in key_types.iter().enumerate() {
        let random_part: String = (0..32).map(|_| {
            let idx = rand::thread_rng().gen_range(0..36);
            "0123456789abcdefghijklmnopqrstuvwxyz"[idx..idx+1].to_string()
        }).collect();

        let raw_key = format!("{}{}", prefixes[i], random_part);
        let salt = SaltString::generate(&mut rand::thread_rng());
        let hash = argon2::Argon2::default()
            .hash_password(raw_key.as_bytes(), &salt)
            .map_err(|e| sqlx::Error::Protocol(format!("Hashing error: {}", e)))?;
        let key_hash = hash.serialize().to_string();
        let prefix_display = &raw_key[..raw_key.len().min(12)];

        sqlx::query(
            "INSERT INTO user_api_keys (id, user_id, tenant_id, key_type, key_hash, key_prefix)
             VALUES ($1, $2, $3, $4, $5, $6)"
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(tenant_id)
        .bind(key_type)
        .bind(&key_hash)
        .bind(prefix_display)
        .execute(db)
        .await?;
    }

    Ok(())
}

/// POST /api/v1/integration-destinations/health-check
/// Run a health check on a connected provider and update its status
pub async fn check_provider_health(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let provider = req.get("provider")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("provider is required".into()))?;

    let base_url = get_provider_base_url(&state.db, tenant_id, provider).await;
    let api_key = get_provider_api_key(&state.db, tenant_id, provider).await;

    let health_url = base_url.as_ref().map(|u| {
        let trimmed = u.trim_end_matches('/');
        match provider {
            "incentiveswift" => format!("{}/health", trimmed),
            "funnelswift" => format!("{}/health", trimmed),
            _ => format!("{}/api/health", trimmed),
        }
    });

    let healthy = if let (Some(url), Some(_key)) = (&health_url, &api_key) {
        match reqwest::Client::new()
            .get(url)
            .header("Authorization", format!("Bearer {}", api_key.as_ref().unwrap_or(&String::new())))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    } else {
        false
    };

    // Update the provider key's metadata with health status
    if let Ok(_) = sqlx::query(
        "UPDATE provider_keys SET metadata = jsonb_set(COALESCE(metadata, '{}'), '{last_health_check}', $1::jsonb), updated_at = NOW() WHERE tenant_id = $2 AND provider = $3"
    )
    .bind(json!({"healthy": healthy, "checked_at": chrono::Utc::now().to_rfc3339()}))
    .bind(tenant_id)
    .bind(provider)
    .execute(&state.db)
    .await
    {
        // OK
    }

    Ok(Json(json!({
        "provider": provider,
        "healthy": healthy,
        "has_key": api_key.is_some(),
        "has_base_url": base_url.is_some(),
    })))
}
