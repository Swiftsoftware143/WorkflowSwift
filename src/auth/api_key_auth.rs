//! API-key authentication.
//!
//! Keys minted by `handlers::api_key_handler::create_api_key` look like
//! `workflowswift_<16 hex>` and are stored **argon2-hashed** with a random salt, so the
//! key itself is not recoverable from the row and cannot be looked up by value.
//! `api_keys.prefix` (varchar(8), shown to the key's owner by `GET /api-keys`) is
//! therefore used as a real lookup hint: it holds `discriminator()` — the first 8 hex
//! digits of sha256(raw key) — so verification is an indexed equality lookup on that
//! hint followed by **one** argon2 check, never a scan over every tenant's key rows.
//! Keys minted before the hint existed still carry the constant `LEGACY_PREFIX` and
//! are found by scanning that (shrinking) subset, and the first successful use of such
//! a key backfills its hint. See `authenticate()`.
//!
//! The raw key is never logged, never stored and never returned on a read path.

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::http::Method;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use super::models::Claims;
use crate::error::AppError;
use crate::features;
use crate::AppState;

/// Prefix of every key minted by the API-key handler (see `create_api_key`).
pub const KEY_PREFIX: &str = "workflowswift_";

/// The `api_keys.prefix` value written by every key minted before `prefix` became a
/// lookup hint. Rows still holding it have no usable discriminator, so they can only
/// be found by scanning that subset.
pub const LEGACY_PREFIX: &str = "workflo";

/// The 8-character lookup hint stored in `api_keys.prefix`: the first 8 hex digits of
/// `sha256(raw key)`.
///
/// `prefix` is varchar(8) and is returned to the key's owner by `GET /api-keys`, so it
/// is product-visible and must be worthless to anyone else. A truncated digest of the
/// whole key satisfies that: it exposes no slice of the key and cannot be inverted back
/// into it.
pub fn discriminator(raw_key: &str) -> String {
    let digest = hex::encode(Sha256::digest(raw_key.as_bytes()));
    digest[..8].to_string()
}

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

    let hint = discriminator(token);

    // 1) Indexed equality lookup on the key's own discriminator: one row (at most a
    //    handful, on a hint collision) for every key minted since `prefix` became a
    //    hint — instead of argon2-verifying every row in the table, which made one
    //    tenant's request pay for every other tenant's keys.
    let mut matched = verify_candidates(state, &hint, token).await?;
    let mut legacy_hit = false;

    // 2) Fallback for keys minted before the hint existed: they still carry the constant
    //    prefix and can only be found by scanning that subset. The subset shrinks on its
    //    own (the backfill below moves a key to the fast path the first time it is used)
    //    and is empty once the last of them is retired, which is when the scan cost of an
    //    unknown credential reaches zero.
    if matched.is_none() && hint != LEGACY_PREFIX {
        matched = verify_candidates(state, LEGACY_PREFIX, token).await?;
        legacy_hit = matched.is_some();
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

    // Self-heal: the first successful use of a pre-hint key writes its discriminator, so
    // that key stops costing a scan on every later request. Best-effort — a failure here
    // must never fail the authentication that just succeeded.
    if legacy_hit {
        let backfill = sqlx::query("UPDATE api_keys SET prefix = $2 WHERE id = $1 AND prefix = $3")
            .bind(key_id)
            .bind(&hint)
            .bind(LEGACY_PREFIX)
            .execute(&state.db)
            .await;
        if let Err(e) = backfill {
            tracing::warn!(error = %e, key_id = %key_id, "legacy prefix backfill failed");
        } else {
            tracing::info!(key_id = %key_id, "api key moved to the indexed prefix path");
        }
    }

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

/// Argon2-verify `token` against the rows carrying one `prefix`, returning the first
/// match. Every candidate costs one full Argon2 verification, which is exactly why the
/// discriminator exists: the hint normally narrows the table to a single row.
async fn verify_candidates(
    state: &AppState,
    prefix: &str,
    token: &str,
) -> Result<Option<sqlx::postgres::PgRow>, AppError> {
    let rows = sqlx::query(
        "SELECT id, aid, user_id, key_hash, permissions, is_active, expires_at \
         FROM api_keys WHERE prefix = $1",
    )
    .bind(prefix)
    .fetch_all(&state.db)
    .await?;

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
            return Ok(Some(row));
        }
    }
    Ok(None)
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

    #[test]
    fn discriminator_is_stable_short_and_not_the_legacy_constant() {
        let key = "workflowswift_0123456789abcdef";
        let d = discriminator(key);
        // deterministic, exactly varchar(8), lowercase hex
        assert_eq!(d, discriminator(key));
        assert_eq!(d.len(), 8);
        assert!(d.bytes().all(|b| b.is_ascii_hexdigit()));
        // distinguishes near-identical keys, and leaks no slice of the key
        assert_ne!(d, discriminator("workflowswift_0123456789abcdee"));
        assert_ne!(d, LEGACY_PREFIX);
        assert!(!key.contains(&d));
    }
}
