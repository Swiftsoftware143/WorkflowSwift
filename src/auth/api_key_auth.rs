//! API-key authentication.
//!
//! Keys minted by `handlers::api_key_handler::create_api_key` look like
//! `workflowswift_<16 hex>` and are stored **argon2-hashed** with a random salt,
//! so there is no indexable plaintext column to look the key up by. We therefore
//! iterate candidate rows and verify the presented credential against each hash
//! (constant-time comparison inside argon2).
//!
//! The raw key is never logged, never stored and never returned on a read path.

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::http::Method;
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::models::Claims;
use crate::error::AppError;
use crate::features;
use crate::AppState;

/// Prefix of every key minted by the API-key handler (see `create_api_key`).
pub const KEY_PREFIX: &str = "workflowswift_";

/// Longest credential we will even attempt to hash-verify, so a huge header
/// cannot turn into unbounded argon2 work.
const MAX_KEY_LEN: usize = 128;

/// True when a bearer credential is an API key rather than a JWT.
pub fn is_api_key(token: &str) -> bool {
    token.starts_with(KEY_PREFIX)
}

/// The scope an HTTP method requires of a key: reads vs writes.
fn required_scope(method: &Method) -> &'static str {
    match *method {
        Method::GET | Method::HEAD | Method::OPTIONS => "read",
        _ => "write",
    }
}

/// `permissions` is a jsonb array of scope strings.
///
/// * absent / `[]` (the default written at creation time) — the key carries the
///   account scope exactly like a JWT for its owning user.
/// * non-empty — the key must be granted the scope the request needs; anything
///   else is refused, so a key is never granted more than its row allows.
fn permissions_allow(permissions: &serde_json::Value, scope: &str) -> bool {
    let Some(list) = permissions.as_array() else {
        return true;
    };
    if list.is_empty() {
        return true;
    }
    list.iter().filter_map(|v| v.as_str()).any(|s| {
        let s = s.trim().to_ascii_lowercase();
        s == "*" || s == "all" || s == scope || (scope == "read" && s == "readonly")
    })
}

/// Resolve an API key to the owning account/user and build the same `Claims`
/// shape JWT auth produces, so every downstream handler stays account-scoped.
pub async fn authenticate(
    state: &AppState,
    token: &str,
    method: &Method,
) -> Result<Claims, AppError> {
    if token.len() > MAX_KEY_LEN || token.len() <= KEY_PREFIX.len() {
        return Err(AppError::Unauthorized);
    }

    let rows = sqlx::query(
        "SELECT id, aid, user_id, key_hash, permissions, is_active, expires_at FROM api_keys",
    )
    .fetch_all(&state.db)
    .await?;

    let mut matched = None;
    for row in rows {
        let hash: String = row.try_get("key_hash").unwrap_or_default();
        if hash.is_empty() {
            continue;
        }
        let Ok(parsed) = PasswordHash::new(&hash) else {
            continue;
        };
        if Argon2::default()
            .verify_password(token.as_bytes(), &parsed)
            .is_ok()
        {
            matched = Some(row);
            break;
        }
    }

    // Unknown / wrong key: indistinguishable from a bad JWT to the caller.
    let row = matched.ok_or(AppError::Unauthorized)?;

    let key_id: Uuid = row.try_get("id")?;
    let aid: Uuid = row.try_get("aid")?;
    let user_id: Uuid = row.try_get("user_id")?;
    let is_active: bool = row.try_get("is_active").unwrap_or(false);
    let expires_at: Option<DateTime<Utc>> = row.try_get("expires_at").ok().flatten();
    let permissions: serde_json::Value = row
        .try_get("permissions")
        .unwrap_or_else(|_| serde_json::json!([]));

    if !is_active {
        tracing::warn!(key_id = %key_id, "api key rejected: revoked");
        return Err(AppError::Unauthorized);
    }
    if let Some(exp) = expires_at {
        if exp <= Utc::now() {
            tracing::warn!(key_id = %key_id, "api key rejected: expired");
            return Err(AppError::Unauthorized);
        }
    }
    if !permissions_allow(&permissions, required_scope(method)) {
        tracing::warn!(key_id = %key_id, "api key rejected: scope not granted");
        return Err(AppError::Forbidden(
            "API key is not permitted to perform this action".to_string(),
        ));
    }

    // The owning user must still be active — a revoked user must not keep
    // working through a key that was minted before the revocation.
    let user_active: Option<bool> =
        sqlx::query_scalar("SELECT is_active FROM users WHERE id = $1 AND aid = $2")
            .bind(user_id)
            .bind(aid)
            .fetch_optional(&state.db)
            .await?;
    if user_active != Some(true) {
        tracing::warn!(key_id = %key_id, "api key rejected: owning user inactive");
        return Err(AppError::Unauthorized);
    }

    // Plan gate: `api_access` off means the account may not use API keys at all.
    features::enforce_plan_flag(&state.db, aid, "api_access", "API access").await?;

    // Touch last_used_at. Best-effort: a failure here must not fail the request.
    if let Err(e) = sqlx::query("UPDATE api_keys SET last_used_at = NOW() WHERE id = $1")
        .bind(key_id)
        .execute(&state.db)
        .await
    {
        tracing::warn!(error = %e, "api key last_used_at update failed");
    }

    let now = Utc::now().timestamp() as usize;
    Ok(Claims {
        sub: user_id.to_string(),
        aid: aid.to_string(),
        // Least privilege: a key is never an admin session. Admin-only handlers
        // (role/super-admin checks) stay reachable by JWT only.
        role: "api_key".to_string(),
        exp: now + 3600,
        iat: now,
        perm_is_super_admin: Some(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_permissions_mean_account_scope() {
        assert!(permissions_allow(&serde_json::json!([]), "read"));
        assert!(permissions_allow(&serde_json::json!([]), "write"));
    }

    #[test]
    fn explicit_permissions_are_enforced() {
        let read_only = serde_json::json!(["read"]);
        assert!(permissions_allow(&read_only, "read"));
        assert!(!permissions_allow(&read_only, "write"));

        let wildcard = serde_json::json!(["*"]);
        assert!(permissions_allow(&wildcard, "write"));
    }

    #[test]
    fn api_key_detection() {
        assert!(is_api_key("workflowswift_0123456789abcdef"));
        assert!(!is_api_key("eyJhbGciOiJIUzI1NiJ9.e30.x"));
    }
}
