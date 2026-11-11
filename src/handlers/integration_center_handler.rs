use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use rand::Rng;
use serde_json::json;
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::AppState;

// ──────────────────────────────────────────────
// Destination fetch — powers the 3-level cascade
// ──────────────────────────────────────────────

/// Get the base URL for a given provider key (from the account stored provider_key or the preset)
pub(crate) async fn get_provider_base_url(
    db: &sqlx::PgPool,
    aid: Uuid,
    provider: &str,
) -> Option<String> {
    // First check provider_keys for a stored base_url
    if let Ok(row) = sqlx::query_scalar::<_, Option<String>>(
        "SELECT base_url FROM provider_keys WHERE aid = $1 AND provider = $2 AND is_active = true",
    )
    .bind(aid)
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
        "SELECT base_url FROM integration_provider_presets WHERE key = $1",
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

/// Fetch API key for a given provider from the account stored keys.
///
/// The column is ciphertext at rest (`enc:v1:` + base64, see
/// `crate::security::provider_key_crypto`) and every caller here uses the value against the
/// provider, so it is decrypted. A value that cannot be decrypted yields `None` — ciphertext
/// is never handed to an outbound request.
pub(crate) async fn get_provider_api_key(
    db: &sqlx::PgPool,
    aid: Uuid,
    provider: &str,
) -> Option<String> {
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT api_key FROM provider_keys WHERE aid = $1 AND provider = $2 AND is_active = true",
    )
    .bind(aid)
    .bind(provider)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    match stored {
        Some(s) => crate::security::provider_key_crypto::decrypt_from_storage(db, &s)
            .await
            .ok(),
        None => None,
    }
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
             ORDER BY ap.name",
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
             ORDER BY action_label",
        )
        .bind(provider)
        .fetch_all(&state.db)
        .await?;

        let actions: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "key": r.try_get::<&str,_>("action_key").unwrap_or(""),
                    "label": r.try_get::<&str,_>("action_label").unwrap_or("")
                })
            })
            .collect();

        return Ok(Json(json!({"actions": actions})));
    }

    // Return destination types for this provider+action
    let rows = sqlx::query(
        "SELECT destination_type, destination_label, sort_order
         FROM integration_destinations
         WHERE provider = $1 AND action_key = $2
         ORDER BY sort_order",
    )
    .bind(provider)
    .bind(action)
    .fetch_all(&state.db)
    .await?;

    let destinations: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "type": r.try_get::<&str,_>("destination_type").unwrap_or(""),
                "label": r.try_get::<&str,_>("destination_label").unwrap_or("")
            })
        })
        .collect();

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
    let dest_type = params
        .get("destination_type")
        .map(|s| s.as_str())
        .unwrap_or("");

    if provider.is_empty() || dest_type.is_empty() {
        return Err(AppError::BadRequest(
            "provider and destination_type are required".into(),
        ));
    }

    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Try to fetch live values from the provider's API
    // If the provider API is unreachable, fall back to cached/static values
    let live_values = fetch_live_destination_values(&state.db, aid, provider, dest_type).await;

    // If we got live values, return them
    if let Some(values) = live_values {
        return Ok(Json(json!({"values": values, "source": "live"})));
    }

    // Fall back to static/placeholder values so the UI never breaks
    let fallback = match (provider, dest_type) {
        ("coreswift", "list") => vec![json!({"id": "default-list", "label": "Default List"})],
        ("coreswift", "tags") => vec![json!({"id": "default-tag", "label": "default"})],
        ("coreswift", "pipeline_stage") => vec![
            json!({"id": "qualified", "label": "Qualified"}),
            json!({"id": "won", "label": "Won"}),
            json!({"id": "lost", "label": "Lost"}),
        ],
        ("funnelswift", "landing_page") => {
            vec![json!({"id": "default-page", "label": "Default Page"})]
        }
        ("funnelswift", "tags") => vec![json!({"id": "default-tag", "label": "default"})],
        ("incentiveswift", "campaign") => {
            vec![json!({"id": "default-campaign", "label": "Default Campaign"})]
        }
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
    aid: Uuid,
    provider: &str,
    dest_type: &str,
) -> Option<Vec<serde_json::Value>> {
    let base_url = get_provider_base_url(db, aid, provider).await?;
    let api_key = match get_provider_api_key(db, aid, provider).await {
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
                .unwrap_or_else(|| extract_array(body, "data", "id", "name").unwrap_or_default())
        }
        ("coreswift", "tags") => extract_array(body, "tags", "id", "name")
            .unwrap_or_else(|| extract_array(body, "data", "id", "name").unwrap_or_default()),
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
        ("funnelswift", "landing_page") => extract_array(body, "landing_pages", "id", "name")
            .unwrap_or_else(|| extract_array(body, "data", "id", "name").unwrap_or_default()),
        ("funnelswift", "tags")
        | ("incentiveswift", "campaign")
        | ("incentiveswift", "milestone_level") => extract_array(body, "data", "id", "name")
            .unwrap_or_else(|| {
                extract_array(body, "campaigns", "id", "name").unwrap_or_else(|| {
                    extract_array(body, "milestones", "id", "name").unwrap_or_default()
                })
            }),
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
                return Some(
                    arr.iter()
                        .map(|_item| {
                            let id = raw_id.as_str().unwrap_or("");
                            json!({"id": id, "label": id})
                        })
                        .collect(),
                );
            }
            return None;
        }
    }

    Some(
        arr.iter()
            .map(|item| {
                json!({
                    "id": item.get(id_field).and_then(|v| v.as_str()).unwrap_or(""),
                    "label": item.get(label_field).and_then(|v| v.as_str()).unwrap_or(""),
                })
            })
            .collect(),
    )
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

    let keys: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<&str,_>("id").unwrap_or(""),
                "key_type": r.try_get::<&str,_>("key_type").unwrap_or(""),
                "prefix": r.try_get::<&str,_>("key_prefix").unwrap_or(""),
                "label": r.try_get::<Option<&str>,_>("label").unwrap_or(None),
                "is_active": r.try_get::<bool,_>("is_active").unwrap_or(false),
                "last_used_at": r.try_get::<Option<&str>,_>("last_used_at").unwrap_or(None),
                "created_at": r.try_get::<&str,_>("created_at").unwrap_or("")
            })
        })
        .collect();

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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let key_type = req
        .get("key_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            AppError::BadRequest(
                "key_type is required (primary, webhook_secret, surface_token)".into(),
            )
        })?;

    if !["primary", "webhook_secret", "surface_token"].contains(&key_type) {
        return Err(AppError::BadRequest(
            "Invalid key_type. Must be primary, webhook_secret, or surface_token".into(),
        ));
    }

    let label = req.get("label").and_then(|v| v.as_str()).unwrap_or("");

    // Generate the key
    let random_part: String = (0..32)
        .map(|_| {
            let idx = rand::thread_rng().gen_range(0..36);
            "0123456789abcdefghijklmnopqrstuvwxyz"[idx..idx + 1].to_string()
        })
        .collect();

    let prefix = match key_type {
        "primary" => "wf_swift_",
        "webhook_secret" => "whsec_",
        "surface_token" => "sf_",
        _ => "key_",
    };

    let raw_key = format!("{}{}", prefix, random_part);

    // Hash for storage
    // Off the reactor, under the shared Argon2 semaphore (auth::api_key_auth::argon2_hash) —
    // this route is authenticated, but the hash is the same 19 MiB job as everywhere else.
    let key_hash = crate::auth::api_key_auth::argon2_hash(raw_key.clone()).await?;

    let id = Uuid::new_v4();
    let prefix_display = &raw_key[..raw_key.len().min(12)];

    sqlx::query(
        "INSERT INTO user_api_keys (id, user_id, aid, key_type, key_hash, key_prefix, label)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(user_id)
    .bind(aid)
    .bind(key_type)
    .bind(&key_hash)
    .bind(prefix_display)
    .bind(if label.is_empty() { None } else { Some(label) })
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id.to_string(),
            "key": raw_key,
            "prefix": prefix_display,
            "key_type": key_type,
            "message": "Save this key — it will not be shown again"
        })),
    ))
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
    aid: Uuid,
) -> Result<(), sqlx::Error> {
    let key_types = ["primary", "webhook_secret", "surface_token"];
    let prefixes = ["wf_swift_", "whsec_", "sf_"];

    for (i, key_type) in key_types.iter().enumerate() {
        let random_part: String = (0..32)
            .map(|_| {
                let idx = rand::thread_rng().gen_range(0..36);
                "0123456789abcdefghijklmnopqrstuvwxyz"[idx..idx + 1].to_string()
            })
            .collect();

        let raw_key = format!("{}{}", prefixes[i], random_part);
        // Three 19 MiB Argon2 hashes per call, and the only caller is `register` — an
        // UNAUTHENTICATED route. They run off the reactor under the shared Argon2 semaphore
        // (auth::api_key_auth::argon2_hash) like every other hash site, so a registration
        // flood queues in the scheduler instead of occupying the tokio worker threads.
        let key_hash = crate::auth::api_key_auth::argon2_hash(raw_key.clone())
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
        let prefix_display = &raw_key[..raw_key.len().min(12)];

        sqlx::query(
            "INSERT INTO user_api_keys (id, user_id, aid, key_type, key_hash, key_prefix)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(aid)
        .bind(key_type)
        .bind(&key_hash)
        .bind(prefix_display)
        .execute(db)
        .await?;
    }

    Ok(())
}

/// POST /api/v1/user-keys/health-check
/// Run a health check on a connected provider and update its status
/// (mounted in the `/user-keys` nest in src/routes.rs; the path was previously documented wrong).
pub async fn check_provider_health(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let provider = req
        .get("provider")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("provider is required".into()))?;

    let base_url = get_provider_base_url(&state.db, aid, provider).await;
    let api_key = get_provider_api_key(&state.db, aid, provider).await;

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
            .header(
                "Authorization",
                format!("Bearer {}", api_key.as_ref().unwrap_or(&String::new())),
            )
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
        "UPDATE provider_keys SET metadata = jsonb_set(COALESCE(metadata, '{}'), '{last_health_check}', $1::jsonb), updated_at = NOW() WHERE aid = $2 AND provider = $3"
    )
    .bind(json!({"healthy": healthy, "checked_at": chrono::Utc::now().to_rfc3339()}))
    .bind(aid)
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

// ── Auto-provisioned, retrievable keys (kanban t_7863696b) ─────────────────────────────────────
//
// David, 2026-11-11 (withdrawing "shown once, never again"): every account is created with a key,
// and the key stays viewable. `key_hash` (argon2) is the only thing the api-key authenticator
// reads; `key_encrypted` (fleet provider-key envelope) exists ONLY so the owner can see and copy
// the key again.

/// A key that was just minted. `raw_key` is the plaintext: sealed before the row is written,
/// handed to the caller's UI, never logged.
pub struct MintedUserKey {
    pub id: Uuid,
    pub raw_key: String,
    pub prefix: String,
    pub key_type: String,
}

/// Mint one key and INSERT it on the caller's connection. The ONE writer for every mint in this
/// app: the signup transaction, the integration-centre "generate" action, regenerate and the
/// one-time backfill.
pub async fn mint_user_key_on(
    conn: &mut sqlx::PgConnection,
    user_id: Uuid,
    aid: Uuid,
    key_type: &str,
    label: Option<&str>,
) -> Result<MintedUserKey, AppError> {
    let random_part: String = (0..32)
        .map(|_| {
            let idx = rand::thread_rng().gen_range(0..36);
            "0123456789abcdefghijklmnopqrstuvwxyz"[idx..idx + 1].to_string()
        })
        .collect();
    let type_prefix = match key_type {
        "primary" => "wf_swift_",
        "webhook_secret" => "whsec_",
        "surface_token" => "sf_",
        _ => "key_",
    };
    let raw_key = format!("{}{}", type_prefix, random_part);
    let key_hash = crate::auth::api_key_auth::argon2_hash(raw_key.clone()).await?;
    let prefix_display = raw_key[..raw_key.len().min(12)].to_string();

    // Fail closed: without PROVIDER_KEY_ENC_SECRET this errors rather than storing a key nobody can
    // ever read back.
    let sealed = crate::security::provider_key_crypto::seal_in(&mut *conn, &raw_key)
        .await
        .map_err(|e| AppError::Internal(format!("could not seal the API key: {e}")))?;

    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO user_api_keys (id, user_id, aid, key_type, key_hash, key_prefix, label, key_encrypted)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(user_id)
    .bind(aid)
    .bind(key_type)
    .bind(&key_hash)
    .bind(&prefix_display)
    .bind(label.filter(|l| !l.is_empty()))
    .bind(&sealed)
    .execute(&mut *conn)
    .await?;

    Ok(MintedUserKey {
        id,
        raw_key,
        prefix: prefix_display,
        key_type: key_type.to_string(),
    })
}

fn verify_account_password(stored_hash: &str, given: &str) -> bool {
    use argon2::{Argon2, PasswordHash, PasswordVerifier};
    match PasswordHash::new(stored_hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(given.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// POST /api/v1/user-keys/{id}/reveal — show the key again (repeatable).
pub async fn reveal_user_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let password = body
        .get("password")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if password.is_empty() {
        return Err(AppError::BadRequest("password is required".into()));
    }

    let row = sqlx::query(
        "SELECT key_prefix, key_encrypted FROM user_api_keys
         WHERE id = $1 AND user_id = $2 AND is_active = true",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("API key not found".into()))?;

    let prefix: String = row.get("key_prefix");
    let sealed: Option<String> = row.get("key_encrypted");

    // ── re-authenticate ───────────────────────────────────────────────────────────────────────
    let stored: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1 AND is_active = true")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?;
    let stored = stored.ok_or(AppError::Unauthorized)?;
    if stored.is_empty() || !verify_account_password(&stored, &password) {
        return Err(AppError::Unauthorized);
    }

    let sealed = sealed.ok_or_else(|| {
        AppError::BadRequest(
            "This key was created before keys were stored recoverably, so it cannot be displayed. \
             Regenerate it to get a key you can view and copy."
                .into(),
        )
    })?;
    let raw_key = crate::security::provider_key_crypto::open_in(&state.db, &sealed)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, key = %id, "API key reveal: sealed copy could not be opened");
            AppError::Internal(
                "This key could not be opened by the server's encryption key. Regenerate it.".into(),
            )
        })?;
    if raw_key.is_empty() {
        return Err(AppError::Internal(
            "This key could not be opened by the server's encryption key. Regenerate it.".into(),
        ));
    }

    let aid = Uuid::parse_str(&claims.aid).ok();
    if let Err(e) = sqlx::query(
        "INSERT INTO api_key_reveals (id, key_id, aid, user_id, key_prefix)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind(aid)
    .bind(user_id)
    .bind(&prefix)
    .execute(&state.db)
    .await
    {
        tracing::error!(error = %e, key = %id, "API key reveal: audit row could not be written");
    }
    tracing::info!(key = %id, key_prefix = %prefix, user = %user_id, "API key REVEALED");

    Ok(Json(json!({ "id": id, "prefix": prefix, "key": raw_key })))
}

/// POST /api/v1/user-keys/{id}/regenerate — one atomic revoke-and-issue.
pub async fn regenerate_user_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let row = sqlx::query(
        "SELECT key_type, label FROM user_api_keys
         WHERE id = $1 AND user_id = $2 AND is_active = true",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("API key not found".into()))?;
    let key_type: String = row.get("key_type");
    let label: Option<String> = row.get("label");

    // Insert the replacement and delete the old row in ONE transaction: a failure at either step
    // leaves the customer with the key they already had; once it commits the old key's hash is
    // gone and the very next authentication attempt fails.
    let mut tx = state.db.begin().await?;
    let minted = mint_user_key_on(&mut tx, user_id, aid, &key_type, label.as_deref()).await?;
    sqlx::query("DELETE FROM user_api_keys WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    tracing::info!(
        new_key = %minted.id,
        old_key = %id,
        key_prefix = %minted.prefix,
        "API key REGENERATED — the previous key no longer authenticates"
    );

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": minted.id,
            "rotated_from": id,
            "key_type": minted.key_type,
            "prefix": minted.prefix,
            "key": minted.raw_key,
            "note": "The previous key no longer works. This one can be viewed and copied again at any time."
        })),
    ))
}

/// One-time backfill: every account that exists today gets a primary key it can view.
/// Gated on the `fleet_data_backfills` marker so it runs ONCE — an account whose keys were
/// deliberately revoked is never re-minted behind its back.
pub async fn backfill_missing_keys(db: &sqlx::PgPool) -> Result<usize, AppError> {
    const MARKER: &str = "api_keys_auto_v1";
    let done: Option<String> =
        sqlx::query_scalar("SELECT name FROM fleet_data_backfills WHERE name = $1")
            .bind(MARKER)
            .fetch_optional(db)
            .await?;
    if done.is_some() {
        return Ok(0);
    }

    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(7263114005)")
        .execute(&mut *tx)
        .await?;

    let users: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT u.id, u.aid FROM users u
         WHERE u.is_active = true
           AND NOT EXISTS (
                SELECT 1 FROM user_api_keys k WHERE k.user_id = u.id AND k.is_active = true
           )
         ORDER BY u.created_at",
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut minted = 0usize;
    for (user_id, aid) in users {
        mint_user_key_on(&mut tx, user_id, aid, "primary", Some("auto-provisioned")).await?;
        minted += 1;
    }

    let unrecoverable: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_api_keys WHERE is_active = true AND key_encrypted IS NULL",
    )
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO fleet_data_backfills (name, detail) VALUES ($1, $2)
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(MARKER)
    .bind(format!(
        "minted {minted} key(s) for accounts with none; {unrecoverable} pre-existing key(s) have no \
         sealed copy (plaintext unrecoverable — regenerate to hand off)"
    ))
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(minted)
}
