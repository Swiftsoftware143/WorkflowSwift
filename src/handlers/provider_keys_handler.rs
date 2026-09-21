use axum::{
    extract::{Json, Path, State},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::security::provider_key_crypto as key_crypto;
use crate::AppState;
use chrono::{DateTime, Utc};

/// Mask an API key ??? show first 3 and last 3 chars, mask the rest.
fn mask_key(key: &str) -> String {
    if key.len() > 6 {
        format!("{}...{}", &key[..3], &key[key.len() - 3..])
    } else if key.len() > 3 {
        format!("{}...", &key[..3])
    } else {
        "***".to_string()
    }
}

// ============================
// CRUD endpoints
// ============================

/// GET /api/v1/provider-keys
/// List all provider keys for the authenticated account (masked).
///
/// The stored column holds ciphertext (`enc:v1:` + base64, see
/// `crate::security::provider_key_crypto`). It is decrypted here only to build the
/// `sk-...xyz` mask — ciphertext is never returned to a client, and neither is a full key.
pub async fn list_provider_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        r#"SELECT id, provider, api_key, base_url, metadata, is_active, created_at, updated_at
           FROM provider_keys
           WHERE aid = $1
           ORDER BY provider ASC"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let mut keys: Vec<serde_json::Value> = Vec::with_capacity(rows.len());

    for row in rows.iter() {
        let stored_key: &str = row.try_get("api_key").unwrap_or("");
        let provider: &str = row.try_get("provider").unwrap_or("");

        // Decrypt ONLY to build the mask. A value that cannot be decrypted still reports
        // has_key = true (the slot holds a credential) but never reveals the ciphertext.
        let raw_key = match key_crypto::decrypt_from_storage(&state.db, stored_key).await {
            Ok(k) => k,
            Err(e) => {
                tracing::warn!(
                    provider,
                    error = %e,
                    "provider key could not be decrypted for masking"
                );
                String::new()
            }
        };

        let base_url: Option<String> = row.try_get("base_url").unwrap_or(None);
        let metadata: serde_json::Value = row.try_get("metadata").unwrap_or(json!({}));
        let created_at: DateTime<Utc> = row.try_get("created_at").unwrap_or_else(|_| Utc::now());
        let updated_at: DateTime<Utc> = row.try_get("updated_at").unwrap_or_else(|_| Utc::now());

        keys.push(json!({
            "id": row.try_get::<Uuid, _>("id").map(|u| u.to_string()).unwrap_or_default(),
            "provider": provider,
            "api_key": mask_key(&raw_key),
            "has_key": !stored_key.is_empty(),
            "base_url": base_url,
            "metadata": metadata,
            "is_active": row.try_get::<bool, _>("is_active").unwrap_or(false),
            "created_at": created_at.to_rfc3339(),
            "updated_at": updated_at.to_rfc3339(),
        }));
    }

    Ok(Json(json!({ "provider_keys": keys })))
}

/// POST /api/v1/provider-keys
/// Create or update a provider key for the account.
/// If the provider already exists for this account, it's upserted.
/// The value is encrypted at rest (AES-256, master key from the environment) and is never
/// stored or echoed back in the clear.
pub async fn upsert_provider_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let provider = req
        .get("provider")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("provider is required".to_string()))?;

    let api_key = req
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("api_key is required".to_string()))?;

    if provider.is_empty() || api_key.is_empty() {
        return Err(AppError::BadRequest(
            "provider and api_key must not be empty".to_string(),
        ));
    }

    let base_url = req
        .get("base_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let metadata = req.get("metadata").cloned().unwrap_or(json!({}));
    let is_active = req
        .get("is_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // Encrypt the credential BEFORE it reaches the database. Nothing is stored in the clear:
    // a missing master key fails the request (see crate::security::provider_key_crypto).
    let encrypted_key = key_crypto::encrypt_for_storage(&state.db, api_key).await?;

    // Upsert. `aid` is the account scope in this app; `tenant_id` is the legacy column from
    // before the tenant->account rename (migration 034) and is mirrored with the account id
    // so older readers still find a value.
    // `api_key` holds `enc:v1:` + base64 ciphertext, never the plaintext value.
    let result = sqlx::query(
        r#"INSERT INTO provider_keys (id, aid, tenant_id, provider, api_key, base_url, metadata, is_active)
           VALUES ($1, $2, $2, $3, $4, $5, $6, $7)
           ON CONFLICT (aid, provider)
           DO UPDATE SET
               api_key = EXCLUDED.api_key,
               base_url = EXCLUDED.base_url,
               metadata = EXCLUDED.metadata,
               is_active = EXCLUDED.is_active,
               updated_at = NOW()"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(provider)
    .bind(&encrypted_key)
    .bind(base_url)
    .bind(&metadata)
    .bind(is_active)
    .execute(&state.db)
    .await?;

    let action = if result.rows_affected() > 0 && result.rows_affected() <= 2 {
        // rows_affected is 1 for insert, 2 for update with ON CONFLICT DO UPDATE
        // We can differentiate by checking if it was an update
        "updated"
    } else {
        "created"
    };

    // Invalidate cache
    state.provider_key_cache.invalidate(&claims.aid, provider);

    Ok(Json(json!({
        "status": "success",
        "action": action,
        "provider": provider,
        "message": format!("Provider key for '{}' has been stored", provider),
    })))
}

/// DELETE /api/v1/provider-keys/:provider
/// Remove a provider key for the authenticated account.
pub async fn delete_provider_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(provider): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM provider_keys WHERE aid = $1 AND provider = $2")
        .bind(aid)
        .bind(&provider)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "Provider key for '{}' not found",
            provider
        )));
    }

    // Invalidate cache
    state.provider_key_cache.invalidate(&claims.aid, &provider);

    Ok(Json(json!({
        "status": "deleted",
        "provider": provider,
    })))
}

/// GET /api/v1/available-providers
/// List all available providers for the dropdown.
pub async fn list_available_providers(
    State(state): State<AppState>,
) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query(
        r#"SELECT key, name, description, requires_base_url, requires_metadata::text, icon
           FROM available_providers
           ORDER BY name ASC"#,
    )
    .fetch_all(&state.db)
    .await?;

    let providers: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let metadata_raw: String = row
                .try_get("requires_metadata")
                .unwrap_or_else(|_| "[]".to_string());
            let metadata: serde_json::Value =
                serde_json::from_str(&metadata_raw).unwrap_or(json!([]));
            let requires_base_url: bool = row.try_get("requires_base_url").unwrap_or(false);

            json!({
                "key": row.try_get::<&str, _>("key").unwrap_or(""),
                "name": row.try_get::<&str, _>("name").unwrap_or(""),
                "description": row.try_get::<Option<&str>, _>("description").unwrap_or(None),
                "requires_base_url": requires_base_url,
                "requires_metadata": metadata,
                "icon": row.try_get::<Option<&str>, _>("icon").unwrap_or(None),
            })
        })
        .collect();

    Ok(Json(json!({"available_providers": providers})))
}

// ============================
// Internal helpers
// ============================

/// Look up a stored provider key for an account.
/// DB-only version — used by dispatch handlers that don't have AppState access.
/// Returns (api_key, base_url, metadata) or None.
pub async fn get_provider_key(
    db: &sqlx::PgPool,
    aid: Uuid,
    provider: &str,
) -> Result<Option<(String, Option<String>, serde_json::Value)>, sqlx::Error> {
    let row = sqlx::query(
        r#"SELECT api_key, base_url, metadata
           FROM provider_keys
           WHERE aid = $1 AND provider = $2 AND is_active = true"#,
    )
    .bind(aid)
    .bind(provider)
    .fetch_optional(db)
    .await?;

    let Some(r) = row else {
        return Ok(None);
    };

    let stored: String = r.try_get("api_key").unwrap_or_default();
    let base_url: Option<String> = r.try_get("base_url").unwrap_or(None);
    let metadata: serde_json::Value = r.try_get("metadata").unwrap_or(json!({}));

    // Decrypt: the column holds ciphertext at rest, and this value is about to be used
    // against the provider. A value we cannot decrypt is treated as "no key" rather than
    // being passed on as garbage ciphertext.
    match key_crypto::decrypt_from_storage(db, &stored).await {
        Ok(api_key) => Ok(Some((api_key, base_url, metadata))),
        Err(e) => {
            tracing::error!(
                error = %e,
                "stored provider key could not be decrypted — treating as no key"
            );
            Ok(None)
        }
    }
}

/// Look up a stored provider key for an account, with AppState caching.
/// Used by resolution handlers (integration_center_handler, etc.).
/// Returns (api_key, base_url, metadata) or None.
pub async fn get_provider_key_cached(
    state: &AppState,
    db: &sqlx::PgPool,
    aid: Uuid,
    provider: &str,
) -> Result<Option<(String, Option<String>, serde_json::Value)>, sqlx::Error> {
    let aid_str = aid.to_string();

    // Check cache first
    if let Some(cached) = state.provider_key_cache.get(&aid_str, provider) {
        return Ok(Some(cached));
    }

    // Cache miss — fetch from DB
    let row = sqlx::query(
        r#"SELECT api_key, base_url, metadata
           FROM provider_keys
           WHERE aid = $1 AND provider = $2 AND is_active = true"#,
    )
    .bind(aid)
    .bind(provider)
    .fetch_optional(db)
    .await?;

    if let Some(r) = row {
        let stored: String = r.try_get("api_key").unwrap_or_default();
        let base_url: Option<String> = r.try_get("base_url").unwrap_or(None);
        let metadata: serde_json::Value = r.try_get("metadata").unwrap_or(json!({}));

        // Decrypt once, then cache the plaintext for the TTL (the cache is what readers use;
        // the database never hands out the usable credential).
        let api_key = match key_crypto::decrypt_from_storage(db, &stored).await {
            Ok(k) => k,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "stored provider key could not be decrypted — treating as no key"
                );
                return Ok(None);
            }
        };

        // Populate cache
        state.provider_key_cache.set(
            &aid_str,
            provider,
            api_key.clone(),
            base_url.clone(),
            metadata.clone(),
        );

        Ok(Some((api_key, base_url, metadata)))
    } else {
        Ok(None)
    }
}
