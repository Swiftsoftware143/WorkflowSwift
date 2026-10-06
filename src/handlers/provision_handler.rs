//! FunnelSwift tag → free account: the receiver (design §3.1, kanban t_ede5f6ed).
//!
//! `POST /api/v1/internal/provision-free-account` is the frozen contract every target app answers,
//! so FunnelSwift's one generic client (`FunnelSwift/src/app_provision.rs`, caller card t_847f9d63)
//! can call them all:
//!
//!     POST /api/v1/internal/provision-free-account
//!     x-internal-key: <shared INTERNAL_SYNC_KEY>          (fail closed on empty/unset, either side)
//!     { source, source_tenant_id, tag:{name,plan_slug}, contact:{...}, idempotency_key }
//!   → 201 provisioned | 200 already_exists | 403 refused | 422 invalid
//!
//! ## Why this route exists again
//!
//! WorkflowSwift shipped a `/internal/tag-provision` handler until 2026-09-25 (t_79d7d1d2), and its
//! deletion note said: *do not re-add one "without a real caller AND a target route that accepts the
//! body"*. Both now exist — the caller is FunnelSwift's tag orchestrator, and the body is the frozen
//! contract above. The OLD handler was deleted for a real reason: it wrote into `clients`/`accounts`
//! keyed off "the oldest account in the DB", i.e. an arbitrary-tenant write that a webhook could
//! drive. **That objection does not apply to this route**: nothing here names a tenant to write
//! into. The caller sends a contact email, the entry plan is resolved in-app, and the account is
//! minted for that address by this app's own signup core — the same single writer the public
//! `POST /api/v1/auth/register` uses (`crate::auth::signup::create_account`). There is no
//! "which account?" question left to answer arbitrarily.

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{extract::Extension, extract::State, Json};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::handlers::admin_settings_handler::require_admin;
use crate::security::email_addr;
use crate::AppState;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Frozen request/response vocabulary (design §3.1)
// ─────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ProvisionFreeAccountRequest {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub source_tenant_id: Option<String>,
    #[serde(default)]
    pub tag: Option<ProvisionFreeAccountTag>,
    pub contact: ProvisionFreeAccountContact,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProvisionFreeAccountTag {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub plan_slug: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProvisionFreeAccountContact {
    pub email: String,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub company: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
}

fn refused(reason: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "status": "refused", "reason": reason })),
    )
        .into_response()
}

fn invalid(reason: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "status": "invalid", "reason": reason })),
    )
        .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// admin_settings helpers + the two values this feature owns
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Read one `admin_settings` value as a bool; a missing or non-bool value yields `default`.
async fn admin_setting_bool(db: &sqlx::PgPool, key: &str, default: bool) -> Result<bool, AppError> {
    let v: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM admin_settings WHERE key = $1")
            .bind(key)
            .fetch_optional(db)
            .await?;
    Ok(v.and_then(|j| j.as_bool()).unwrap_or(default))
}

/// Read one `admin_settings` value as a string; a missing or non-string value yields `default`.
async fn admin_setting_str(
    db: &sqlx::PgPool,
    key: &str,
    default: &str,
) -> Result<String, AppError> {
    let v: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM admin_settings WHERE key = $1")
            .bind(key)
            .fetch_optional(db)
            .await?;
    Ok(v.and_then(|j| j.as_str().map(str::to_string))
        .unwrap_or_else(|| default.to_string()))
}

async fn upsert_admin_setting(
    db: &sqlx::PgPool,
    key: &str,
    value: serde_json::Value,
    description: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO admin_settings (key, value, description, updated_at) VALUES ($1, $2::jsonb, $3, now()) \
         ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, \
           description = COALESCE(admin_settings.description, EXCLUDED.description), updated_at = now()",
    )
    .bind(key)
    .bind(value)
    .bind(description)
    .execute(db)
    .await?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Small pure helpers (unit-tested below)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A server-generated temporary password, from the same charset the app's checkout credential
/// delivery uses (12 chars; no look-alike glyphs such as `I`/`l`/`1`/`O`/`0`).
fn generate_password() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789!@#";
    let mut rng = rand::thread_rng();
    (0..12)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// A fabricated address must never mint an account (design §3.1 rule 5). The old CoreSwift
/// `fs-provision-…@placeholder` fallback is exactly what this refuses.
fn looks_like_placeholder(email: &str) -> bool {
    email.contains("placeholder")
        || email.starts_with("fs-provision")
        || email.ends_with(".invalid")
}

// The slug helper moved to `crate::auth::signup::unique_account_slug` (kanban t_bf9e00fe):
// ONE implementation of the retried, suffixed workspace slug for both account doors.

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The receiver
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The receiver FunnelSwift's tag→free-account orchestrator calls (design §3.1).
///
/// Order of checks matters: the credential fails closed first, then the master toggle refuses
/// before anything is read or written, then the address and the entry plan are validated, then
/// idempotency, then the mint.
pub async fn provision_free_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ProvisionFreeAccountRequest>,
) -> ApiResult<Response> {
    // 1. Credential — fail closed when the key is unset on EITHER side. The boundary
    //    (`auth::boundary::require_credential`) demands the same key for this path, but it also
    //    admits a recognised session credential, so a user JWT can reach this handler: it must
    //    still be refused here, not only at the edge.
    let expected = std::env::var("INTERNAL_SYNC_KEY").unwrap_or_default();
    let presented = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if expected.is_empty() || presented.is_empty() || presented != expected {
        return Err(AppError::Unauthorized);
    }

    // 2. Master toggle — ships OFF; David enables it per app from the console (design §3.1 rule 2).
    if !admin_setting_bool(&state.db, "provision_from_tags_enabled", false).await? {
        return Ok(refused("provisioning_disabled"));
    }

    // 3. Address — normalise (a non-address is refused 422) and refuse a placeholder outright.
    let email = email_addr::normalize(&req.contact.email).map_err(AppError::Validation)?;
    if looks_like_placeholder(&email) {
        return Ok(invalid("placeholder_email"));
    }

    // 4. Entry plan — resolved IN-APP by slug, never by a sibling's plan name (design §3.1 rule 1),
    //    and required to be a real active FREE tier. `plan_tiers.slug` is this app's plan identity.
    let plan_slug = admin_setting_str(&state.db, "provision_entry_plan_slug", "free").await?;
    let plan_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM plan_tiers WHERE slug = $1 AND is_active = true \
         AND COALESCE(price_monthly, 0) = 0 LIMIT 1",
    )
    .bind(&plan_slug)
    .fetch_optional(&state.db)
    .await?;
    if plan_id.is_none() {
        return Ok(invalid("no_free_plan"));
    }

    // 5. Idempotent by LOWER(email): an existing account mints nothing (design §3.1 rule 3).
    let existing: Option<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM users WHERE lower(email) = $1 ORDER BY created_at ASC, id ASC LIMIT 1",
    )
    .bind(&email)
    .fetch_optional(&state.db)
    .await?;
    if let Some(user_id) = existing {
        let account_id: Option<Uuid> = sqlx::query_scalar("SELECT aid FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?;
        return Ok((
            StatusCode::OK,
            Json(json!({
                "status": "already_exists",
                "account_id": account_id.map(|a| a.to_string()).unwrap_or_default(),
            })),
        )
            .into_response());
    }

    // 6. Mint through the ONE shared signup core (design §3.1 rule 4): accounts + tags + users +
    //    account_plans(free) + the plan's credits + the industry/dashboard seed, exactly the shape
    //    the public signup mints. A server-generated password is mailed through the app's existing
    //    `welcome` template, so the business can log in and upgrade in place.
    let first = req.contact.first_name.clone().unwrap_or_default();
    let last = req.contact.last_name.clone().unwrap_or_default();
    let name = {
        let full = format!("{} {}", first.trim(), last.trim());
        let full = full.trim().to_string();
        if full.is_empty() {
            req.contact
                .company
                .clone()
                .filter(|c| !c.trim().is_empty())
                .unwrap_or_else(|| email.clone())
        } else {
            full
        }
    };

    let raw_password = generate_password();
    let password_hash = crate::auth::api_key_auth::argon2_hash(raw_password.clone()).await?;
    let account_slug = crate::auth::signup::unique_account_slug(&state.db, &name).await?;

    let ids = match crate::auth::signup::create_account(
        &state,
        crate::auth::signup::NewAccount {
            email: &email,
            name: &name,
            password_hash: &password_hash,
            password_plain: Some(&raw_password),
            account_name: None,
            account_slug: Some(&account_slug),
            plan_slug: &plan_slug,
            industry_slug: "site-flipping",
            role: "user",
        },
    )
    .await
    {
        Ok(ids) => ids,
        // The idempotency check runs before the mint, but two concurrent calls can still race.
        // Answer `already_exists` truthfully rather than 500-ing the caller.
        Err(AppError::Duplicate(_)) => {
            let account_id: Option<Uuid> = sqlx::query_scalar(
                "SELECT aid FROM users WHERE lower(email) = $1 ORDER BY created_at ASC, id ASC LIMIT 1",
            )
            .bind(&email)
            .fetch_optional(&state.db)
            .await?;
            return Ok((
                StatusCode::OK,
                Json(json!({
                    "status": "already_exists",
                    "account_id": account_id.map(|a| a.to_string()).unwrap_or_default(),
                })),
            )
                .into_response());
        }
        Err(other) => return Err(other),
    };

    let source = req
        .source
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "funnelswift".to_string());
    tracing::info!(
        account = %ids.account_id,
        user = %ids.user_id,
        email = %email,
        source = %source,
        plan = %plan_slug,
        "provision_free_account: minted a free account from a tag"
    );

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "status": "provisioned",
            "account_id": ids.account_id.to_string(),
            "plan_slug": plan_slug,
            "login_email": email,
        })),
    )
        .into_response())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Admin console: the master toggle + the in-app entry-plan picker (design §3.3)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// GET the two provisioning settings plus this app's own free plans, for the picker.
pub async fn get_provisioning_settings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    // Platform-admin only, through the app's ONE admin gate (the admin nest carries no role
    // layer: each handler decides). Provisioning writes accounts, so a tenant user must not reach
    // the switch.
    require_admin(&claims)?;
    let enabled = admin_setting_bool(&state.db, "provision_from_tags_enabled", false).await?;
    let plan_slug = admin_setting_str(&state.db, "provision_entry_plan_slug", "free").await?;
    let free_plans: Vec<(String, String)> = sqlx::query_as(
        "SELECT slug, name FROM plan_tiers WHERE is_active = true AND COALESCE(price_monthly, 0) = 0 \
         ORDER BY sort_order ASC, name ASC",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "provision_from_tags_enabled": enabled,
        "provision_entry_plan_slug": plan_slug,
        "free_plans": free_plans
            .into_iter()
            .map(|(slug, name)| json!({ "slug": slug, "name": name }))
            .collect::<Vec<_>>(),
    })))
}

/// Update the two settings. The plan slug must resolve to a real FREE tier in THIS app — the panel
/// cannot point provisioning at a paid plan.
pub async fn update_provisioning_settings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&claims)?;
    if let Some(v) = body.get("provision_from_tags_enabled") {
        let b = v.as_bool().ok_or_else(|| {
            AppError::BadRequest("provision_from_tags_enabled must be a boolean".into())
        })?;
        upsert_admin_setting(
            &state.db,
            "provision_from_tags_enabled",
            json!(b),
            "Tag → free account: master switch (design §3.1 rule 2)",
        )
        .await?;
    }
    if let Some(v) = body.get("provision_entry_plan_slug") {
        let s = v.as_str().ok_or_else(|| {
            AppError::BadRequest("provision_entry_plan_slug must be a string".into())
        })?;
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM plan_tiers WHERE slug = $1 AND is_active = true \
             AND COALESCE(price_monthly, 0) = 0)",
        )
        .bind(s)
        .fetch_one(&state.db)
        .await?;
        if !ok {
            return Err(AppError::BadRequest(format!(
                "'{s}' is not an active free plan in WorkflowSwift"
            )));
        }
        upsert_admin_setting(
            &state.db,
            "provision_entry_plan_slug",
            json!(s),
            "Tag → free account: entry plan slug (design §3.1 rule 1)",
        )
        .await?;
    }
    // Answer with the same shape the GET does (the console re-renders from this response).
    get_provisioning_settings(State(state), Extension(claims)).await
}

#[cfg(test)]
mod tests {
    use super::{generate_password, looks_like_placeholder};

    #[test]
    fn generated_password_is_twelve_chars_from_the_credential_charset() {
        for _ in 0..50 {
            let p = generate_password();
            assert_eq!(p.len(), 12, "password length: {p}");
            assert!(
                p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "!@#".contains(c)),
                "unexpected char in {p}"
            );
            // Two draws are not the same string (a constant generator would be a defect).
        }
        let a = generate_password();
        let b = generate_password();
        assert_ne!(a, b, "two consecutive passwords were identical");
    }

    #[test]
    fn placeholder_addresses_are_refused_and_real_ones_are_not() {
        assert!(looks_like_placeholder("fs-provision-x@example.com"));
        assert!(looks_like_placeholder("someone@probe.invalid"));
        assert!(looks_like_placeholder("placeholder@example.com"));
        assert!(!looks_like_placeholder("owner@acme.example.com"));
        assert!(!looks_like_placeholder("david@swiftsoftware.dev"));
    }
}
