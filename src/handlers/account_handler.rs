use axum::{
    extract::{Json, State},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::handlers::provider_keys_handler::mask_key;
use crate::models::account::Account;
use crate::security::provider_key_crypto as key_crypto;
use crate::AppState;

pub async fn get_account(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let account = sqlx::query_as::<_, Account>("SELECT * FROM accounts WHERE id = $1")
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Account not found".to_string()))?;

    Ok(Json(json!({"account": account})))
}

pub async fn update_account(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE accounts SET name = $1 WHERE id = $2")
            .bind(name)
            .bind(aid)
            .execute(&state.db)
            .await?;
    }

    // There is deliberately NO `slug` arm here (kanban t_27bd3765). It used to run
    // `UPDATE accounts SET slug = $1` while every writer that matters — registration
    // (src/auth/handlers.rs:82), the portfolio handlers, the admin-create INSERT — wrote
    // `account_slug`, and `src/handlers/bridge_handler.rs:155` read `account_slug`. Measured live
    // before the change: `PUT /api/v1/accounts {"slug": "..."}` answered 200 "Account updated" and
    // moved `slug` while `account_slug` did not move — a rename that renamed nothing, over a column
    // no reader resolved (6 of 8 rows were NULL; on the other 2 it duplicated `account_slug`).
    // `slug` was dropped by migrations/078_drop_accounts_slug_column.sql, so `account_slug` is the
    // one tenant identifier. A body carrying `slug` is now inert (the handler binds only the keys it
    // names below); `account_slug` is NOT settable through this route — renaming the identifier a
    // bridge and the admin console list resolve is not a tenant-settings action.

    // A SECOND write door into `accounts.hexomatic_key` (kanban t_08ac18ca): `PUT /api/v1/accounts`
    // accepted the raw value while `PUT /accounts/hexomatic-key` sealed it. Both doors now store the
    // same envelope — a key saved through either one is ciphertext at rest, and a writer that forgot
    // this would be refused by the `accounts_hexomatic_key_sealed` CHECK (migration 109).
    if let Some(hex_key) = req.get("hexomatic_key").and_then(|v| v.as_str()) {
        let hex_key = hex_key.trim();
        let sealed = if hex_key.is_empty() {
            None
        } else {
            Some(key_crypto::encrypt_for_storage(&state.db, hex_key).await?)
        };
        sqlx::query("UPDATE accounts SET hexomatic_key = $1 WHERE id = $2")
            .bind(sealed)
            .bind(aid)
            .execute(&state.db)
            .await?;
    }

    if let Some(year) = req.get("footer_year").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE accounts SET footer_year = $1 WHERE id = $2")
            .bind(year)
            .bind(aid)
            .execute(&state.db)
            .await?;
    }

    if let Some(company) = req.get("footer_company").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE accounts SET footer_company = $1 WHERE id = $2")
            .bind(company)
            .bind(aid)
            .execute(&state.db)
            .await?;
    }

    Ok(Json(json!({"message": "Account updated"})))
}

/// GET /api/v1/accounts/hexomatic-key — the stored Hexomatic BYOK key, MASKED.
///
/// The column is ciphertext at rest (`enc:v1:` + base64, the same envelope `provider_keys` uses —
/// see `crate::security::provider_key_crypto`). It is opened here only to build the `abc...xyz`
/// mask; the ciphertext is never returned to a client and neither is the full key. A value this
/// deployment cannot open (a row sealed under a different `PROVIDER_KEY_ENC_SECRET`) reports
/// `usable=false` and no mask, so the panel asks for the key again instead of showing the
/// ciphertext's first and last three bytes as if they were the credential.
pub async fn get_hexomatic_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let stored: Option<String> =
        sqlx::query_scalar("SELECT hexomatic_key FROM accounts WHERE id = $1")
            .bind(aid)
            .fetch_one(&state.db)
            .await?;
    let stored = stored.unwrap_or_default();

    let set = !stored.is_empty();
    let (masked, usable) = if set {
        match key_crypto::decrypt_from_storage(&state.db, &stored).await {
            Ok(plain) if !plain.trim().is_empty() => (mask_key(&plain), true),
            Ok(_) => (String::new(), false),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "hexomatic key is stored but this deployment cannot open it"
                );
                (String::new(), false)
            }
        }
    } else {
        (String::new(), false)
    };

    Ok(Json(json!({"key": masked, "set": set, "usable": usable})))
}

pub async fn set_account_industry(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let industry_slug = req
        .get("industry_slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("industry_slug is required".into()))?;

    sqlx::query("UPDATE accounts SET industry_slug = $1 WHERE id = $2")
        .bind(industry_slug)
        .bind(aid)
        .execute(&state.db)
        .await?;

    Ok(Json(
        json!({"status": "ok", "industry_slug": industry_slug}),
    ))
}

/// PUT /api/v1/accounts/hexomatic-key — store the tenant's Hexomatic BYOK key.
///
/// Sealed with the app's ONE credential envelope (`crate::security::provider_key_crypto`:
/// `enc:v1:` + `pgp_sym_encrypt` under `PROVIDER_KEY_ENC_SECRET`), exactly like
/// `provider_keys.api_key`. Fail closed: without the master key this answers 500 and stores
/// nothing — a plaintext credential is never written as a fallback. An empty / whitespace-only
/// `key` clears the slot (SQL NULL), which is what the panel's "no key" state means.
pub async fn set_hexomatic_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let key = req.get("key").and_then(|v| v.as_str()).unwrap_or("").trim();

    let sealed: Option<String> = if key.is_empty() {
        None
    } else {
        Some(key_crypto::encrypt_for_storage(&state.db, key).await?)
    };

    sqlx::query("UPDATE accounts SET hexomatic_key = $1 WHERE id = $2")
        .bind(sealed)
        .bind(aid)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({"message": "Hexomatic API key stored"})))
}
