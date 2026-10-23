use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::email;
use crate::error::{ApiResult, AppError};
use crate::security::address_identity::{self, AddressIdentity};
use crate::security::email_addr;
use crate::security::payment_provider_secrets as provider_secrets;
use crate::AppState;
use sqlx::Row;

// ──────────────────────────────────────────────
// Admin: Payment Provider CRUD
// ──────────────────────────────────────────────

/// The ONE role predicate for the whole payment-provider surface (kanban t_592cd45d).
///
/// Reading the list, writing a provider and deleting one are the same privilege: the list carries
/// the publishable keys, the test/live mode and the `config` of this install's payment accounts,
/// and a write to `api_key_encrypted` repoints where the install collects money. Before this
/// predicate existed each arm decided for itself — and the READ silently decided nothing:
/// `GET /api/v1/payment-providers` took `State` alone, no `Claims` at all, behind a shipped comment
/// that claimed the opposite, so ANY authenticated tenant session (every account a signup mints)
/// read the platform-global `payment_providers` table. Measured live 2026-10-14 against the
/// pre-fix deploy on 127.0.0.1:8085: a fresh tenant token answered `200 {"providers":[]}` where the
/// identical shape on ADASwift (same defect, closed in t_0f5c7e3a) already answered 403.
///
/// The role list is the flag the two write arms already required, and the flag the admin console's
/// own Admin tab is gated on (`www-admin/index.html::showAdminTab` reads `ME.perm_is_super_admin`),
/// so the console can never offer a screen this API refuses and the tightening loses no UI. The
/// admin surface (`/api/v1/admin/*`, `auth::middleware`) refuses on the SAME flag, so the whole API
/// agrees on one list. An API-key credential resolves to the same `Claims` in `auth_middleware`, so
/// a key caller is covered here too — no second gate is needed.
fn can_manage_payment_providers(claims: &Claims) -> bool {
    claims.perm_is_super_admin.unwrap_or(false)
}

/// GET /api/v1/payment-providers
/// List all configured payment providers (keys masked)
///
/// Super admin only — the list is platform configuration, not tenant data; see
/// [`can_manage_payment_providers`]. The response never carries a credential VALUE: `key_status`
/// answers configured / not_configured and neither the secret key nor the webhook signing secret is
/// ever returned (unchanged by t_592cd45d).
pub async fn list_payment_providers(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    if !can_manage_payment_providers(&claims) {
        return Err(AppError::Forbidden(
            "Only super admins can manage payment providers".into(),
        ));
    }

    let rows = sqlx::query(
        r#"SELECT id, provider_type, label, is_active,
                  CASE WHEN api_key_encrypted IS NOT NULL AND api_key_encrypted != '' THEN 'configured' ELSE 'not_configured' END as key_status,
                  COALESCE(publishable_key, '') as publishable_key,
                  api_key_encrypted, webhook_secret_encrypted,
                  is_test_mode, config, created_at, updated_at
           FROM payment_providers
           ORDER BY provider_type ASC"#,
    )
    .fetch_all(&state.db)
    .await?;

    let providers: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
                "provider_type": r.try_get::<&str,_>("provider_type").unwrap_or(""),
                "label": r.try_get::<&str,_>("label").unwrap_or(""),
                "is_active": r.try_get::<bool,_>("is_active").unwrap_or(false),
                "key_status": r.try_get::<&str,_>("key_status").unwrap_or("not_configured"),
                // At-rest posture of the SAVED credential, measured off the ciphertext COLUMN and
                // never off the decrypted value (kanban t_d1fcda7e). `key_status` says a credential
                // is configured; it does NOT say the copy in the database is encrypted, and this
                // panel promised "stored encrypted" as a flat claim until 2026-10-16. The raw
                // columns are selected so the SAME `is_encrypted` predicate the write path seals
                // with decides this, instead of a second copy of the prefix literal. The webhook
                // secret had no status field at all before this.
                "api_key_set": r.try_get::<&str,_>("key_status").map(|s| s == "configured").unwrap_or(false),
                "api_key_sealed": crate::security::provider_key_crypto::is_encrypted(
                    r.try_get::<&str,_>("api_key_encrypted").unwrap_or("")),
                "webhook_secret_set": !r.try_get::<&str,_>("webhook_secret_encrypted").unwrap_or("").trim().is_empty(),
                "webhook_secret_sealed": crate::security::provider_key_crypto::is_encrypted(
                    r.try_get::<&str,_>("webhook_secret_encrypted").unwrap_or("")),
                "publishable_key": r.try_get::<&str,_>("publishable_key").unwrap_or(""),
                "is_test_mode": r.try_get::<bool,_>("is_test_mode").unwrap_or(true),
                "config": r.try_get::<serde_json::Value,_>("config").unwrap_or(json!({})),
                "created_at": r.try_get::<chrono::DateTime<chrono::Utc>,_>("created_at")
                    .map(|t| t.to_rfc3339()).unwrap_or_default(),
                "updated_at": r.try_get::<chrono::DateTime<chrono::Utc>,_>("updated_at")
                    .map(|t| t.to_rfc3339()).unwrap_or_default(),
            })
        })
        .collect();

    Ok(Json(json!({"providers": providers})))
}

/// POST /api/v1/payment-providers
/// Create or update a payment provider configuration
pub async fn upsert_payment_provider(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    // Super admin only — the SAME predicate the read arm above and the DELETE arm below use
    // (kanban t_592cd45d; the read used to check nothing at all).
    if !can_manage_payment_providers(&claims) {
        return Err(AppError::Forbidden(
            "Only super admins can manage payment providers".into(),
        ));
    }

    let provider_type = req
        .get("provider_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            AppError::BadRequest(
                "provider_type is required (stripe, paypal, square, paddle)".into(),
            )
        })?;

    if !["stripe", "paypal", "square", "paddle"].contains(&provider_type) {
        return Err(AppError::BadRequest(
            "Invalid provider_type. Must be stripe, paypal, square, or paddle".into(),
        ));
    }

    let label = req.get("label").and_then(|v| v.as_str()).unwrap_or("");
    let is_active = req
        .get("is_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let is_test_mode = req
        .get("is_test_mode")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let publishable_key = req
        .get("publishable_key")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let config = req.get("config").cloned().unwrap_or(json!({}));
    let api_key = req.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
    let webhook_secret = req
        .get("webhook_secret")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Seal both credentials BEFORE either is bound (kanban t_6104de65). Migration 041 named these
    // columns after a promise ("Encrypted API credentials (encrypted-at-rest via app-layer
    // encryption)") that this write path never kept: the raw request value went straight into
    // `api_key_encrypted` and `webhook_secret_encrypted`, so a Stripe secret key (which can charge
    // cards) and the endpoint's webhook signing secret (which alone decides whether an anonymous
    // POST /api/v1/webhooks/stripe completes a checkout) sat in the clear at rest, readable by
    // anyone holding a dump, a backup or a read-only SQL grant.
    //
    // `seal_for_write` returns an empty value unchanged — that is the "not submitted" signal the
    // four UPDATE literals below key on — and returns an already sealed value byte-identical, so a
    // masked round-trip cannot double-wrap the credential into an outer envelope that the receiver
    // would then use as its HMAC key. A missing master key fails the request instead of storing
    // plaintext (the app's standing fail-closed rule).
    let api_key_sealed = provider_secrets::seal_for_write(&state.db, api_key).await?;
    let webhook_secret_sealed = provider_secrets::seal_for_write(&state.db, webhook_secret).await?;

    // Check if provider already exists
    let existing =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM payment_providers WHERE provider_type = $1")
            .bind(provider_type)
            .fetch_optional(&state.db)
            .await?;

    if let Some(provider_id) = existing {
        // Update — only overwrite api_key/webhook_secret if provided.
        //
        // Gate rule 5d / class 14 (kanban t_15a04e9d): the statement used to be BUILT at run time
        // ("UPDATE … SET …" + a push_str per optional field + " WHERE id = $" + index), so no
        // complete query was visible at the site and the text was a function of the request. Two
        // optional fields produced exactly four texts; they are the four compile-time literals
        // below, byte-identical to what the builder emitted (the same bind order, the same
        // parameter indices). Nothing but the CHOICE of literal is decided at run time.
        const UPD_BOTH: &str = "UPDATE payment_providers SET label = $1, is_active = $2, is_test_mode = $3, publishable_key = $4, config = $5, updated_at = NOW(), api_key_encrypted = $6, webhook_secret_encrypted = $7 WHERE id = $8";
        const UPD_KEY: &str = "UPDATE payment_providers SET label = $1, is_active = $2, is_test_mode = $3, publishable_key = $4, config = $5, updated_at = NOW(), api_key_encrypted = $6 WHERE id = $7";
        const UPD_SECRET: &str = "UPDATE payment_providers SET label = $1, is_active = $2, is_test_mode = $3, publishable_key = $4, config = $5, updated_at = NOW(), webhook_secret_encrypted = $6 WHERE id = $7";
        const UPD_NONE: &str = "UPDATE payment_providers SET label = $1, is_active = $2, is_test_mode = $3, publishable_key = $4, config = $5, updated_at = NOW() WHERE id = $6";
        let query = match (api_key.is_empty(), webhook_secret.is_empty()) {
            (false, false) => UPD_BOTH,
            (false, true) => UPD_KEY,
            (true, false) => UPD_SECRET,
            (true, true) => UPD_NONE,
        };

        let mut q = sqlx::query(query)
            .bind(label)
            .bind(is_active)
            .bind(is_test_mode)
            .bind(publishable_key)
            .bind(&config);

        if !api_key.is_empty() {
            q = q.bind(&api_key_sealed);
        }
        if !webhook_secret.is_empty() {
            q = q.bind(&webhook_secret_sealed);
        }
        q = q.bind(provider_id);

        q.execute(&state.db).await?;

        Ok(Json(json!({
            "status": "updated",
            "provider_type": provider_type,
            "message": "Payment provider updated"
        })))
    } else {
        // Insert
        if api_key.is_empty() {
            return Err(AppError::BadRequest(
                "api_key is required when creating a new provider".into(),
            ));
        }

        sqlx::query(
            r#"INSERT INTO payment_providers
               (provider_type, label, is_active, api_key_encrypted, webhook_secret_encrypted,
                publishable_key, config, is_test_mode)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
        )
        .bind(provider_type)
        .bind(label)
        .bind(is_active)
        .bind(&api_key_sealed)
        .bind(&webhook_secret_sealed)
        .bind(publishable_key)
        .bind(&config)
        .bind(is_test_mode)
        .execute(&state.db)
        .await?;

        Ok(Json(json!({
            "status": "created",
            "provider_type": provider_type,
            "message": "Payment provider created"
        })))
    }
}

/// DELETE /api/v1/payment-providers/{provider_type}
/// Remove a payment provider configuration
pub async fn delete_payment_provider(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(provider_type): Path<String>,
) -> ApiResult<impl IntoResponse> {
    // The SAME predicate the read and the write arm use (kanban t_592cd45d).
    if !can_manage_payment_providers(&claims) {
        return Err(AppError::Forbidden(
            "Only super admins can manage payment providers".into(),
        ));
    }

    let result = sqlx::query("DELETE FROM payment_providers WHERE provider_type = $1")
        .bind(&provider_type)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "Payment provider '{}' not found",
            provider_type
        )));
    }

    Ok(Json(
        json!({"status": "deleted", "provider_type": provider_type}),
    ))
}

// ══════════════════════════════════════════════════════════════════════════════
// Verifying the operator's payment key (kanban t_48e4dd2a)
// ══════════════════════════════════════════════════════════════════════════════
//
// The FunnelSwift precedent is kanban t_6a192c06 (commit b153f98): the same route, adapted to this
// app's shape. Until it existed the ONLY way to learn that a payment key was wrong, truncated or
// from another account was for a CUSTOMER to press Upgrade and receive the 424 that carries the
// provider's refusal. `payment_providers` is empty on this install (measured 2026-10-16), so every
// checkout refuses — and an operator pasting a key had no way to check it before it mattered. This
// route makes the key verifiable at the moment it is pasted.
//
// Both checks are READS that create nothing at the provider: Stripe is asked for `GET /v1/balance`
// and PayPal for an OAuth `client_credentials` token. Those are exactly the first calls the two buy
// arms make (`create_stripe_session`, `create_paypal_session` — whose own comment reads "the
// credential is checked HERE"), so a key that passes here cannot then fail at the buy button for a
// credential reason. The arms match that pair one-for-one: a provider this app's checkout has no arm
// for is refused here (501) rather than reported as working.
//
// One body shape, `{ "api_key": "<key>" }`, and both arms are useful:
//   * `api_key` non-empty -> test THAT key. It is never stored, never logged and never echoed (only
//     masked), so a typo cannot overwrite a working credential the way a Save would. This is the
//     button beside the Save form.
//   * `api_key` empty/absent -> test the key STORED for this provider type, opened through the SAME
//     choke point a checkout opens it through (`provider_secrets::open_for_use`, the only reader of
//     `api_key_encrypted`). This is the per-row button.
//
// Every refusal is a 4xx class JSON body, never a 502: a CDN replaces a 502 body and the operator
// would lose the reason — the same rule `AppError::UpstreamRefused` (424) follows.
//
// Super-admin only, through the SAME predicate the list/upsert/delete arms use
// ([`can_manage_payment_providers`]): the key this route tests or reads is the install's own, and a
// tenant session must not be able to use it as an oracle for a credential it cannot see.
pub async fn test_payment_provider(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(provider_type): Path<String>,
    payload: Option<Json<serde_json::Value>>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if !can_manage_payment_providers(&claims) {
        return Err(AppError::Forbidden(
            "Only super admins can manage payment providers".into(),
        ));
    }

    let provider = provider_type.trim().to_ascii_lowercase();
    let supplied = payload
        .as_ref()
        .and_then(|j| j.0.get("api_key"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let (key, source, stored_active) = if !supplied.is_empty() {
        (supplied, "supplied", None)
    } else {
        // The stored arm. `is_active DESC` first, so the row a CHECKOUT would use is the one tested
        // when an older inactive row is still lying around; `is_active` is then reported back,
        // because a key that works on an inactive row is exactly the state that still refuses every
        // checkout — and that is not something the operator can see from this screen otherwise.
        let row = sqlx::query(
            "SELECT api_key_encrypted, is_active FROM payment_providers \
             WHERE provider_type = $1 ORDER BY is_active DESC, created_at DESC LIMIT 1",
        )
        .bind(&provider)
        .fetch_optional(&state.db)
        .await?;

        let Some(row) = row else {
            return Ok((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "ok": false,
                    "error": "payment_provider_not_configured",
                    "reason": "payment_provider_not_configured",
                    "provider": provider,
                    "message": format!(
                        "No '{}' payment provider is stored on this install, so there is no stored key to test. Paste the key and press Test key, or save a provider first.",
                        provider
                    ),
                })),
            ));
        };

        let stored: Option<String> = row
            .try_get(provider_secrets::API_KEY_COLUMN)
            .unwrap_or(None);
        let active: bool = row.try_get("is_active").unwrap_or(false);
        let opened = provider_secrets::open_for_use(
            &state.db,
            provider_secrets::API_KEY_COLUMN,
            stored.as_deref().unwrap_or(""),
        )
        .await;
        if opened.trim().is_empty() {
            return Ok((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "ok": false,
                    "error": "payment_provider_not_configured",
                    "reason": "payment_provider_not_configured",
                    "provider": provider,
                    "stored_active": active,
                    "message": format!(
                        "The stored '{}' provider row carries no secret key (or it cannot be opened by this deployment), so there is nothing to test. Paste the key and press Save first.",
                        provider
                    ),
                })),
            ));
        }
        (opened, "stored", Some(active))
    };

    // Stay in step with `create_checkout_session`: whatever that arm will not sell through, this one
    // must not claim to have verified. Square and Paddle are accepted by the upsert but have no
    // checkout arm, so their keys are refused HERE instead of being reported as working.
    if provider != "stripe" && provider != "paypal" {
        return Ok((
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "ok": false,
                "error": "checkout_not_implemented",
                "provider": provider,
                "reason": "checkout_not_implemented",
                "message": format!(
                    "'{}' cannot be verified: checkout in this app has only a Stripe and a PayPal arm, so a key for any other provider would be accepted here and then refused at the buy button.",
                    provider
                ),
            })),
        ));
    }

    let masked = crate::handlers::provider_keys_handler::mask_key(&key);
    let client = reqwest::Client::new();

    if provider == "paypal" {
        // The same first call `create_paypal_session` makes — and the credential IS what it checks
        // (a wrong or expired client id/secret answers 401 `invalid_client`). No order is created.
        // The base is this app's own `PAYPAL_API_BASE` knob, already read by the PayPal webhook arm,
        // and its default is the same live endpoint the buy arm dials.
        let base = std::env::var("PAYPAL_API_BASE")
            .unwrap_or_else(|_| "https://api-m.paypal.com".to_string())
            .trim_end_matches('/')
            .to_string();

        let sent = client
            .post(format!("{}/v1/oauth2/token", base))
            .header(
                "Authorization",
                format!("Basic {}", base64_encode_auth(&key)),
            )
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("grant_type=client_credentials")
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await;

        let resp = match sent {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(provider = %provider, key_source = source, error = %e, "payment key test could not reach the provider");
                return Ok(payment_key_unreachable(
                    &provider,
                    source,
                    &masked,
                    stored_active,
                    e.to_string(),
                ));
            }
        };
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();

        if status.is_success() {
            return Ok((
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "provider": provider,
                    "key_source": source,
                    "stored_active": stored_active,
                    // PayPal's token carries no test/live flag, and this app's buy arm dials the
                    // live PayPal API, so there is no second mode to report for it.
                    "livemode": serde_json::Value::Null,
                    "api_key_masked": masked,
                    "message": "PayPal accepted the client id and secret. This app's buy path dials the live PayPal API, so this credential can take real payments.",
                })),
            ));
        }

        // Never echo the key. The provider's own sentence is safe and is the whole point of the
        // route — PayPal carries it in `error_description`.
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error_description")
                    .and_then(|m| m.as_str())
                    .or_else(|| v.get("error").and_then(|m| m.as_str()))
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("PayPal returned {}", status));
        tracing::warn!(provider = %provider, key_source = source, http = %status, "payment key was REFUSED by the provider");
        return Ok(payment_key_refused(
            &provider,
            source,
            &masked,
            stored_active,
            msg,
        ));
    }

    // Stripe. `STRIPE_API_BASE` is the same knob `create_stripe_session` reads.
    let api_base = std::env::var("STRIPE_API_BASE")
        .unwrap_or_else(|_| "https://api.stripe.com".to_string())
        .trim_end_matches('/')
        .to_string();

    let sent = client
        .get(format!("{}/v1/balance", api_base))
        .bearer_auth(&key)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await;

    let resp = match sent {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(provider = %provider, key_source = source, error = %e, "payment key test could not reach the provider");
            return Ok(payment_key_unreachable(
                &provider,
                source,
                &masked,
                stored_active,
                e.to_string(),
            ));
        }
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();

    if status.is_success() {
        // Stripe's /v1/balance answers `{ "object": "balance", "livemode": <bool>, ... }`. Only
        // `livemode` is read.
        let live = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("livemode").and_then(|b| b.as_bool()));
        return Ok((
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "provider": provider,
                "key_source": source,
                "stored_active": stored_active,
                "livemode": live,
                "api_key_masked": masked,
                "message": stripe_livemode_message(live),
            })),
        ));
    }

    let msg = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("Stripe returned {}", status));
    tracing::warn!(provider = %provider, key_source = source, http = %status, "payment key was REFUSED by the provider");
    Ok(payment_key_refused(
        &provider,
        source,
        &masked,
        stored_active,
        msg,
    ))
}

/// What a successful Stripe check reads back to the operator.
///
/// `livemode` is most of the value of the Stripe arm: a TEST-mode key is a working key that will
/// never charge anybody, and mistaking one for the other is the exact mistake this route exists to
/// prevent. Split out so all three arms are pinned by a test — the two `Some(..)` branches cannot be
/// reached live without a real credential.
fn stripe_livemode_message(live: Option<bool>) -> String {
    match live {
        Some(true) => {
            "Stripe accepted the key and it is a LIVE key — a real customer can be charged."
                .to_string()
        }
        Some(false) => {
            "Stripe accepted the key but it is a TEST-mode key — no customer will ever be charged. Replace it with the live key before selling.".to_string()
        }
        None => "Stripe accepted the key.".to_string(),
    }
}

/// The refusal body both provider arms answer with: the provider's OWN sentence, the mask, never the
/// key.
fn payment_key_refused(
    provider: &str,
    key_source: &str,
    masked: &str,
    stored_active: Option<bool>,
    provider_message: String,
) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::FAILED_DEPENDENCY,
        Json(json!({
            "ok": false,
            "error": "provider_refused",
            "provider": provider,
            "key_source": key_source,
            "stored_active": stored_active,
            "api_key_masked": masked,
            "reason": "provider_refused",
            "message": format!(
                "{} refused the key: {}",
                provider_display(provider),
                provider_message
            ),
        })),
    )
}

/// The provider could not be reached at all — the test never ran, so nothing is claimed either way.
fn payment_key_unreachable(
    provider: &str,
    key_source: &str,
    masked: &str,
    stored_active: Option<bool>,
    error: String,
) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::FAILED_DEPENDENCY,
        Json(json!({
            "ok": false,
            "error": "provider_unreachable",
            "provider": provider,
            "key_source": key_source,
            "stored_active": stored_active,
            "api_key_masked": masked,
            "reason": "provider_unreachable",
            "message": format!(
                "Could not reach {} at all: {}",
                provider_display(provider),
                error
            ),
        })),
    )
}

/// The provider's own name, for prose. An unknown id is passed through rather than silently named
/// after a provider that did not answer.
fn provider_display(provider: &str) -> &str {
    match provider {
        "stripe" => "Stripe",
        "paypal" => "PayPal",
        "square" => "Square",
        "paddle" => "Paddle",
        other => other,
    }
}

// ──────────────────────────────────────────────
// Checkout Session Creation
// ──────────────────────────────────────────────

/// Get active payment provider configuration, with both credentials OPENED (kanban t_6104de65).
///
/// The two credential columns are ciphertext at rest (`enc:v1:` + base64, see
/// `crate::security::payment_provider_secrets`), and every caller here uses the value against the
/// provider: `api_key` is the secret key handed to Stripe / PayPal, `webhook_secret` is the HMAC key
/// `verify_stripe_signature` recomputes a Stripe delivery's signature with and the webhook id a
/// PayPal delivery is verified against. Handing any of them the stored envelope instead of the
/// credential would verify nothing and charge nothing, so the open happens HERE — at the single read
/// choke point — and never returns ciphertext to a caller.
///
/// A row written before this change holds legacy plaintext and is passed through unchanged. A row
/// that cannot be opened by this deployment (restored under a different `PROVIDER_KEY_ENC_SECRET`)
/// yields an empty credential, which leaves the receivers on their documented not-configured arms
/// instead of using the envelope as if it were the secret.
async fn get_active_provider(
    db: &sqlx::PgPool,
    provider_type: &str,
) -> Result<Option<serde_json::Value>, sqlx::Error> {
    let row = sqlx::query(
        r#"SELECT id, provider_type, api_key_encrypted, publishable_key,
                  webhook_secret_encrypted, config, is_test_mode
           FROM payment_providers
           WHERE provider_type = $1 AND is_active = true
           LIMIT 1"#,
    )
    .bind(provider_type)
    .fetch_optional(db)
    .await?;

    let Some(r) = row else {
        return Ok(None);
    };

    let api_key_stored: Option<String> = r.try_get("api_key_encrypted").unwrap_or(None);
    let webhook_secret_stored: Option<String> =
        r.try_get("webhook_secret_encrypted").unwrap_or(None);

    Ok(Some(json!({
        "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
        "provider_type": r.try_get::<&str,_>("provider_type").unwrap_or(""),
        "api_key": provider_secrets::open_for_use(
            db,
            provider_secrets::API_KEY_COLUMN,
            api_key_stored.as_deref().unwrap_or(""),
        ).await,
        "publishable_key": r.try_get::<Option<&str>,_>("publishable_key").unwrap_or(None).unwrap_or(""),
        "webhook_secret": provider_secrets::open_for_use(
            db,
            provider_secrets::WEBHOOK_SECRET_COLUMN,
            webhook_secret_stored.as_deref().unwrap_or(""),
        ).await,
        "is_test_mode": r.try_get::<bool,_>("is_test_mode").unwrap_or(true),
    })))
}

/// Read a request header as a `&str`, `""` when it is absent or not valid UTF-8. Header names
/// are matched case-insensitively by `HeaderMap`.
fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

/// Clip an upstream response body for a single log line. Bodies come from a third party and can
/// be arbitrarily long (or echo our own request), so they never reach the log whole and newlines
/// never break the one-line-per-event format.
fn clip_for_log(raw: &str, max: usize) -> String {
    let flat = raw.replace(['\n', '\r'], " ");
    if flat.chars().count() <= max {
        flat
    } else {
        let head: String = flat.chars().take(max).collect();
        format!("{}…(truncated)", head)
    }
}

/// What PayPal's `verify-webhook-signature` call needs: the REST app credentials for Basic auth,
/// the webhook id the signature must verify against, and a log-only label for where the
/// credentials came from. No value in here is ever logged.
struct PaypalVerifyConfig {
    client_id: String,
    client_secret: String,
    webhook_id: String,
    source: &'static str,
}

/// Resolve the receiver's verification configuration, or `None` when PayPal is genuinely not
/// configured. Order:
///
/// 1. webhook id: `PAYPAL_WEBHOOK_ID` (config), then the `webhook_secret` of an ACTIVE `paypal`
///    row in `payment_providers` — the field the shipped admin console's *Payment providers*
///    panel writes, so enabling PayPal needs no redeploy;
/// 2. credentials: that same row's `api_key_encrypted` as `client_id:client_secret` (exactly the
///    value the checkout leg hands to PayPal as Basic auth), then the optional
///    `PAYPAL_CLIENT_ID` / `PAYPAL_CLIENT_SECRET` env pair.
///
/// A database error propagates — it is never collapsed into "not configured", because that would
/// turn a transient DB failure into a permanent 503 that looks like a config problem.
async fn paypal_verify_config(state: &AppState) -> Result<Option<PaypalVerifyConfig>, AppError> {
    let provider = get_active_provider(&state.db, "paypal").await?;
    let row_api_key = provider
        .as_ref()
        .map(|p| p["api_key"].as_str().unwrap_or("").to_string())
        .unwrap_or_default();
    let row_webhook_id = provider
        .as_ref()
        .map(|p| {
            p["webhook_secret"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default();

    let (client_id, client_secret, source) = match row_api_key.split_once(':') {
        Some((id, secret)) if !id.is_empty() && !secret.is_empty() => {
            (id.to_string(), secret.to_string(), "payment_providers")
        }
        _ => {
            let env_id = std::env::var("PAYPAL_CLIENT_ID").unwrap_or_default();
            let env_secret = std::env::var("PAYPAL_CLIENT_SECRET").unwrap_or_default();
            if env_id.is_empty() || env_secret.is_empty() {
                return Ok(None);
            }
            (env_id, env_secret, "env")
        }
    };

    let env_webhook_id = state.config.paypal_webhook_id.trim().to_string();
    let webhook_id = if env_webhook_id.is_empty() {
        row_webhook_id
    } else {
        env_webhook_id
    };
    if webhook_id.is_empty() {
        return Ok(None);
    }

    Ok(Some(PaypalVerifyConfig {
        client_id,
        client_secret,
        webhook_id,
        source,
    }))
}

/// The provider a plan checkout falls back to when neither the request nor `plan_tiers` names one.
/// See the resolution arm in `create_checkout_session` for the measurement behind this default
/// (kanban t_8a1afdd5): NULL on the tier must not be a refusal, because this install sells four
/// tiers and none of them carries a value.
const DEFAULT_PAYMENT_PROVIDER: &str = "stripe";

/// POST /api/v1/checkout/create
/// Create a Stripe/PayPal checkout session
pub async fn create_checkout_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let purchasable_type = req
        .get("purchasable_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("purchasable_type is required".into()))?;

    // Resolve payment provider: explicit > the plan tier's own column > this install's default.
    //
    // MEASURED 2026-10-15 (kanban t_46881521 leg 3, fixed under t_8a1afdd5): `plan_tiers` shipped
    // `payment_provider` NULL on ALL FOUR live rows, so a plan checkout that named no `provider_type`
    // was refused with a 400 BEFORE any credential was consulted — the arm could not succeed for any
    // tier this install sells, and the operator read that as the plan being misconfigured. The column
    // HAS a writer (`admin_create_plan` / `admin_update_plan_full` bind it, and the admin Plans panel
    // passes `payment_provider` as an add/edit input), so it stays the per-tier OVERRIDE it was meant
    // to be; NULL now means "use this install's provider", which is what the sibling ADASwift arm
    // does with the same idea (`plans.payment_provider` … `.unwrap_or("stripe")`). Stripe is the
    // default because it is the provider every install-wide arm here is written for, and an install
    // with no active row still answers the honest 400 below ("No active stripe provider configured")
    // rather than pretending to charge. A `purchasable_id` that names NO tier is still refused: it is
    // a bad purchase, not a provider question.
    let provider_type = if let Some(pt) = req.get("provider_type").and_then(|v| v.as_str()) {
        pt.to_string()
    } else if purchasable_type == "plan" {
        let pid = req
            .get("purchasable_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| {
                AppError::BadRequest("purchasable_id is required for plan checkout".into())
            })?;
        match sqlx::query_scalar::<_, Option<String>>(
            "SELECT payment_provider FROM plan_tiers WHERE id = $1",
        )
        .bind(pid)
        .fetch_optional(&state.db)
        .await?
        {
            // Some(row): the tier exists. Its column is an override; blank and NULL mean "default".
            Some(tier_provider) => tier_provider
                .filter(|p| !p.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_PAYMENT_PROVIDER.to_string()),
            None => {
                return Err(AppError::BadRequest(format!(
                    "purchasable_id {} names no plan tier",
                    pid
                )))
            }
        }
    } else {
        return Err(AppError::BadRequest(
            "provider_type is required (stripe, paypal)".into(),
        ));
    };

    // ── THE PRICE CHARGED IS OUR OWN CATALOGUE'S, NOT THE REQUEST'S ─────────────────────────────
    // MEASURED 2026-10-22 (cron `default`): this handler took `amount` from the REQUEST BODY and
    // handed it straight to the provider, so a workspace could POST `amount: 1` against the
    // Enterprise tier (price_monthly 199) and — the moment an operator configures a key — be
    // charged $1 for a $199 plan. The server had no opinion on the price it was selling at.
    //
    // The sibling arm has always read the plan row (ADASwift `create_upgrade_checkout`: `amount` is
    // `COALESCE(price_monthly, price)` from `plans`), and this app's own console already SENDS the
    // tier's stored `price_monthly` — so agreeing callers see no change. A request whose amount
    // disagrees is REFUSED before a credential is even read, rather than silently repriced: a
    // console sending the wrong number is a defect that must surface, not one the server papers
    // over. Non-plan purchases keep the caller's amount (this route sells nothing else here, and
    // inventing a price for them would be the same mistake in the other direction).
    let purchasable_id = req
        .get("purchasable_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());

    let requested_amount = req
        .get("amount")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| AppError::BadRequest("amount is required".into()))?;

    // The tier's SLUG travels out of this block with its price: a REFUSED plan checkout is recorded
    // as this workspace's plan request (see the refusal arm below), and the operator's queue keys
    // the ask by the slug and seats the workspace from it.
    let (amount, plan_slug) = if purchasable_type == "plan" {
        let pid = purchasable_id.ok_or_else(|| {
            AppError::BadRequest("purchasable_id is required for plan checkout".into())
        })?;
        let plan: Option<(String, Option<f64>, String)> = sqlx::query_as(
            "SELECT name, price_monthly::float8, slug FROM plan_tiers WHERE id = $1 AND is_active = true",
        )
        .bind(pid)
        .fetch_optional(&state.db)
        .await?;
        let Some((plan_name, plan_price, slug)) = plan else {
            return Err(AppError::NotFound(
                "That plan is not on sale any more. Nothing was charged.".into(),
            ));
        };
        let price = plan_price.unwrap_or(0.0);
        if !price.is_finite() || price <= 0.0 {
            return Err(AppError::BadRequest(format!(
                "The {} plan has no price set, so there is nothing to check out. \
                 Ask the operator to put a price on it.",
                plan_name
            )));
        }
        if (requested_amount - price).abs() > 0.005 {
            return Err(AppError::BadRequest(format!(
                "The {} plan costs {:.2} — the request asked for {:.2}. \
                 Nothing was charged; please reload the Plans screen and try again.",
                plan_name, price, requested_amount
            )));
        }
        (price, slug)
    } else {
        (requested_amount, String::new())
    };

    let currency = req
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("USD");

    // Resolve success_url: explicit, else the static /thank-you.html.
    // `plan_tiers` has no `thank_you_url` column and no surface writes one: `checkout_url` there is
    // the payment link the provider returns (checkout_url, this file:383), and neither the live plan
    // CRUD (`admin_settings_handler::admin_update_plan_full`, the only plan writer mounted —
    // routes.rs:701; the unmounted `plan_handler` duplicates were deleted in kanban t_647203c7) nor
    // the admin Plans UI sends a per-plan thank-you page. Selecting it made every create-session
    // without an explicit success_url a guaranteed ERROR 42703 (kanban t_b9c74751); the documented
    // default is the real fallback.
    let success_url = req
        .get("success_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "/thank-you.html".to_string());

    let cancel_url = req
        .get("cancel_url")
        .and_then(|v| v.as_str())
        .unwrap_or("/");

    let metadata = req.get("metadata").cloned().unwrap_or(json!({}));

    // ── AN UNCONFIGURED INSTALL IS NOT THE BUYER'S ERROR, AND THE ASK MUST NOT BE LOST ────────
    // MEASURED LIVE 2026-10-23 (cron `default`): this app's arm answered `400 {"error":"No active
    // stripe provider configured"}` — an operator instruction addressed to a customer, in a status
    // that blames the caller for the SERVER's missing merchant account. The console re-worded it
    // and filed a plan request, but only if that console JS ran: any other client, or a buyer who
    // closed the tab, left nothing behind. The refusal is now recorded HERE, in `tickets` with
    // `source = 'plan_request'` — the same row the console files and the operator's queue reads,
    // keyed on (aid, source, plan_slug, open) exactly as `tickets_handler::create` dedupes, so the
    // console's later press returns it as `already_open` instead of minting a twin. No receipt mail
    // is queued on this path: nothing was charged, and a refusal must not add mail volume.
    let provider = match get_active_provider(&state.db, &provider_type).await? {
        Some(p) if !p["api_key"].as_str().unwrap_or("").trim().is_empty() => p,
        other => {
            let reason = if other.is_some() {
                "payment_provider_key_missing"
            } else {
                "payment_provider_not_configured"
            };
            if !plan_slug.is_empty() {
                record_refused_upgrade(&state, account_id, user_id, &plan_slug, amount, reason)
                    .await;
            }
            tracing::error!(
                "checkout/create REFUSED: {reason} for account {account_id} asking for {plan_slug}"
            );
            return Ok((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "payment_provider_not_configured",
                    "message": "No charge was made — nothing was sent to a payment provider, so you cannot be billed. Upgrades are not switched on for this installation yet: your request has been recorded for the platform team, who can activate this plan for your workspace by hand.",
                    "configured": false,
                    "request_recorded": !plan_slug.is_empty()
                })),
            ));
        }
    };

    let api_key = provider["api_key"].as_str().unwrap_or("").to_string();

    // Create checkout session with the provider
    let provider_session = match provider_type.as_str() {
        "stripe" => {
            create_stripe_session(
                &api_key,
                amount,
                currency,
                purchasable_type,
                &success_url,
                cancel_url,
                &metadata,
            )
            .await?
        }
        "paypal" => {
            create_paypal_session(
                &api_key,
                amount,
                currency,
                purchasable_type,
                &success_url,
                cancel_url,
                &metadata,
            )
            .await?
        }
        _ => {
            return Err(AppError::BadRequest(format!(
                "Checkout not supported for provider type: {}",
                provider_type
            )))
        }
    };

    let provider_session_id = provider_session["id"].as_str().unwrap_or("");
    let checkout_url = provider_session["url"].as_str().unwrap_or("");

    // Store the checkout session in our database
    let session_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO checkout_sessions
           (id, account_id, user_id, provider_type, provider_session_id,
            purchasable_type, purchasable_id, amount, currency, status, metadata)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'pending', $10)"#,
    )
    .bind(session_id)
    .bind(account_id)
    .bind(user_id)
    .bind(&provider_type)
    .bind(provider_session_id)
    .bind(purchasable_type)
    .bind(purchasable_id)
    .bind(amount)
    .bind(currency)
    .bind(&metadata)
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::OK,
        Json(json!({
            "session_id": session_id.to_string(),
            "provider_session_id": provider_session_id,
            "checkout_url": checkout_url,
            "provider_type": provider_type,
        })),
    ))
}

/// Record a refused plan checkout as this workspace's plan request (2026-10-23).
///
/// The destination is `tickets` with `source = 'plan_request'`: the table the console's own
/// "request this plan" press writes and the operator's Plan requests queue reads, where resolving
/// the row moves the workspace onto the tier. The open-row key is the one
/// `tickets_handler::create` already dedupes on (account + source + tier, status not closed), so
/// the console's later press returns this row as `already_open` rather than a twin.
///
/// Deliberately queues no receipt mail. A failure to record is logged, never returned — the buyer
/// still gets the refusal sentence, and the operator still has the log line naming the account.
async fn record_refused_upgrade(
    state: &AppState,
    account_id: Uuid,
    user_id: Uuid,
    plan_slug: &str,
    amount: f64,
    reason: &str,
) {
    let plan_name: Option<String> =
        sqlx::query_scalar("SELECT name FROM plan_tiers WHERE slug = $1")
            .bind(plan_slug)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);
    let label = plan_name.unwrap_or_else(|| plan_slug.to_string());
    let inserted = sqlx::query(
        r#"INSERT INTO tickets (id, aid, subject, description, status, priority, source, plan_slug)
           SELECT $1, $2, $3, $4, 'open', 'medium', 'plan_request', $5
            WHERE NOT EXISTS (
                SELECT 1 FROM tickets
                 WHERE aid = $2 AND source = 'plan_request' AND plan_slug = $5
                   AND lower(status) NOT IN ('closed', 'resolved', 'done'))"#,
    )
    .bind(Uuid::new_v4())
    .bind(account_id)
    .bind(format!("Upgrade request: {label} plan"))
    .bind(format!(
        "This workspace pressed Upgrade on Plan & Billing and asked for the {label} plan \
         (${amount:.2}/month). Self-serve checkout is not open on this installation yet \
         ({reason}), so the team makes the switch by hand. Nothing was charged. Filed by \
         {user_id}."
    ))
    .bind(plan_slug)
    .execute(&state.db)
    .await;
    if let Err(e) = inserted {
        tracing::error!(
            "checkout/create: could not record the refused upgrade as a plan request: {e:?}"
        );
    }
}

/// Create a Stripe checkout session via Stripe API
async fn create_stripe_session(
    api_key: &str,
    amount: f64,
    currency: &str,
    purchasable_type: &str,
    success_url: &str,
    cancel_url: &str,
    metadata: &serde_json::Value,
) -> Result<serde_json::Value, AppError> {
    let client = reqwest::Client::new();

    // Stripe expects amount in cents
    let amount_cents = (amount * 100.0).round() as u64;

    // Build the line item
    let mut line_item = serde_json::json!({
        "price_data": {
            "currency": currency.to_lowercase(),
            "product_data": {
                "name": format!("{} purchase", purchasable_type.replace('_', " ")),
            },
            "unit_amount": amount_cents,
        },
        "quantity": 1,
    });

    // Add description from metadata if present
    if let Some(desc) = metadata.get("description").and_then(|v| v.as_str()) {
        line_item["price_data"]["product_data"]["description"] = json!(desc);
    }

    let mut body = serde_json::json!({
        "mode": "payment",
        "success_url": success_url,
        "cancel_url": cancel_url,
        "line_items": [line_item],
        "metadata": metadata.clone(),
    });

    // Map metadata to Stripe's flat format
    if let Some(obj) = body["metadata"].as_object_mut() {
        // Stripe metadata values must be strings
        for (_k, v) in obj.iter_mut() {
            if !v.is_string() {
                *v = json!(v.to_string());
            }
        }
    }

    // If it's a subscription-based purchase, set mode to subscription
    // (For now we use one-time payment mode — subscription support can be added later)

    // Endpoint override for acceptance runs, mirroring PAYPAL_API_BASE — the knob the PayPal arm
    // already reads (see /etc/swift/env/workflowswift.env), and the only way the plan arm can be
    // driven end-to-end with no third-party credential: Stripe refuses an invented key *before*
    // this handler reaches its `checkout_sessions` INSERT, so without a locally stubbed endpoint
    // the arm's own contract cannot be exercised at all. Unset in normal operation, i.e. the real
    // endpoint — this changes nothing until someone sets it.
    let api_base = std::env::var("STRIPE_API_BASE")
        .unwrap_or_else(|_| "https://api.stripe.com".to_string())
        .trim_end_matches('/')
        .to_string();

    let resp = client
        .post(format!("{}/v1/checkout/sessions", api_base))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&to_stripe_form_data(&body))
        .send()
        .await
        .map_err(|e| AppError::UpstreamRefused(format!("Stripe unreachable: {}", e)))?;

    let status = resp.status();
    let response_body: serde_json::Value = resp.json().await.map_err(|e| {
        AppError::UpstreamRefused(format!(
            "Stripe returned {} but its body was not JSON: {}",
            status.as_u16(),
            e
        ))
    })?;

    if !status.is_success() {
        let error_msg = response_body["error"]["message"]
            .as_str()
            .unwrap_or("Unknown Stripe error");
        return Err(AppError::UpstreamRefused(format!(
            "Stripe returned {}: {}",
            status.as_u16(),
            clip_for_log(error_msg, 300)
        )));
    }

    Ok(json!({
        "id": response_body["id"].as_str().unwrap_or(""),
        "url": response_body["url"].as_str().unwrap_or(""),
    }))
}

/// Create a PayPal order via PayPal REST API
async fn create_paypal_session(
    api_key: &str,
    amount: f64,
    currency: &str,
    _purchasable_type: &str,
    success_url: &str,
    cancel_url: &str,
    _metadata: &serde_json::Value,
) -> Result<serde_json::Value, AppError> {
    let client = reqwest::Client::new();

    // PayPal requires an access token first
    let token_resp = client
        .post("https://api-m.paypal.com/v1/oauth2/token")
        .header(
            "Authorization",
            format!("Basic {}", base64_encode_auth(api_key)),
        )
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("grant_type=client_credentials")
        .send()
        .await
        .map_err(|e| AppError::UpstreamRefused(format!("PayPal unreachable: {}", e)))?;

    let token_status = token_resp.status();
    let token_body: serde_json::Value = token_resp.json().await.map_err(|e| {
        AppError::UpstreamRefused(format!(
            "PayPal returned {} but its auth body was not JSON: {}",
            token_status.as_u16(),
            e
        ))
    })?;

    // The credential is checked HERE, before anything else can fail (kanban t_8a1afdd5): a wrong or
    // expired client id/secret answers 401 `invalid_client` and carries its reason in
    // `error_description`, and reading the body for `access_token` without looking at the status
    // turned that into an uninformative "Failed to get PayPal access token" behind a bare 500.
    if !token_status.is_success() {
        let reason = token_body["error_description"]
            .as_str()
            .or_else(|| token_body["error"].as_str())
            .unwrap_or("no reason given");
        return Err(AppError::UpstreamRefused(format!(
            "PayPal returned {}: {}",
            token_status.as_u16(),
            clip_for_log(reason, 300)
        )));
    }

    let access_token = token_body["access_token"].as_str().ok_or_else(|| {
        AppError::UpstreamRefused(
            "PayPal answered without an access_token, so no order could be created".into(),
        )
    })?;

    // Create the order
    let order_body = serde_json::json!({
        "intent": "CAPTURE",
        "purchase_units": [{
            "amount": {
                "currency_code": currency.to_uppercase(),
                "value": format!("{:.2}", amount),
            }
        }],
        "payment_source": {
            "paypal": {
                "experience_context": {
                    "payment_method_preference": "IMMEDIATE_PAYMENT_REQUIRED",
                    "landing_page": "LOGIN",
                    "user_action": "PAY_NOW",
                    "return_url": success_url,
                    "cancel_url": cancel_url,
                }
            }
        }
    });

    let order_resp = client
        .post("https://api-m.paypal.com/v2/checkout/orders")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .header("PayPal-Request-Id", format!("order-{}", Uuid::new_v4()))
        .json(&order_body)
        .send()
        .await
        .map_err(|e| AppError::UpstreamRefused(format!("PayPal unreachable: {}", e)))?;

    let order_status = order_resp.status();
    let order_body: serde_json::Value = order_resp.json().await.map_err(|e| {
        AppError::UpstreamRefused(format!(
            "PayPal returned {} but its order body was not JSON: {}",
            order_status.as_u16(),
            e
        ))
    })?;

    if !order_status.is_success() {
        let error_msg = order_body["message"]
            .as_str()
            .or_else(|| order_body["error_description"].as_str())
            .unwrap_or("Unknown PayPal error");
        return Err(AppError::UpstreamRefused(format!(
            "PayPal returned {}: {}",
            order_status.as_u16(),
            clip_for_log(error_msg, 300)
        )));
    }

    // Get the approval URL from the links
    let approval_url = order_body["links"]
        .as_array()
        .and_then(|links| {
            links
                .iter()
                .find(|l| l["rel"].as_str() == Some("approve"))
                .and_then(|l| l["href"].as_str())
        })
        .unwrap_or("");

    Ok(json!({
        "id": order_body["id"].as_str().unwrap_or(""),
        "url": approval_url,
    }))
}

// ──────────────────────────────────────────────
// Webhook Handlers
// ──────────────────────────────────────────────

/// Which arm of the Stripe receiver's failure contract fired. `None` means the delivery
/// verified and may be dispatched.
struct StripeRejection {
    /// What the caller is answered with.
    status: StatusCode,
    /// The `reason` in the response body.
    reason: &'static str,
    /// The `payment_webhook_events.status` this arm records. Two distinct values for two
    /// distinct situations: the column could not tell "nothing is configured to verify with"
    /// from "a signature was presented and did not verify" while both were `failed`.
    audit_status: &'static str,
}

/// Stripe's own documented default tolerance for a `t=` stamp, and this receiver's default: a
/// delivery whose stamp is further than this from THIS host's clock is refused even when its HMAC
/// verifies (see `stripe_webhook`, arm 4). Overridable with `STRIPE_WEBHOOK_TOLERANCE_SECS`.
pub const DEFAULT_STRIPE_SIGNATURE_TOLERANCE_SECS: i64 = 300;

/// The two fields of a `Stripe-Signature` header this receiver cares about — `t=` (when the
/// signature was generated) and `v1=` (the HMAC over `"{t}.{body}"`) — read in ONE place so the
/// verifier and the freshness arm can never disagree about which bytes were signed.
fn stripe_signature_parts(signature: &str) -> (Option<&str>, Option<&str>) {
    let mut timestamp = None;
    let mut v1 = None;
    for part in signature.split(',') {
        if let Some(t) = part.strip_prefix("t=") {
            timestamp = Some(t);
        } else if let Some(s) = part.strip_prefix("v1=") {
            v1 = Some(s);
        }
    }
    (timestamp, v1)
}

/// The `t=` stamp as epoch seconds, when the header carries one that is actually a number. `None`
/// means "this delivery gives no usable time" — the freshness arm refuses that too, because a stamp
/// that cannot be read cannot bound anything.
fn stripe_signature_timestamp(signature: &str) -> Option<i64> {
    stripe_signature_parts(signature)
        .0
        .and_then(|t| t.trim().parse::<i64>().ok())
}

/// The Stripe receiver's refusal contract as a pure function of what the deployment holds
/// (`secret`: the active `stripe` row's signing secret, `None` when there is nothing to verify
/// with), what the delivery carried (`signature`: the `Stripe-Signature` header), the verdict of
/// the HMAC check, and — for the freshness arm — the delivery's own `t=` stamp (`signed_at`),
/// this host's clock (`now`) and the tolerated distance between them (`tolerance_secs`).
/// Unit-tested below; see `stripe_webhook` for why each status was chosen.
///
/// The arms are ordered CONFIG → PRESENCE → AUTHENTICITY → FRESHNESS. Freshness is deliberately
/// last: an ancient stamp on a signature that does not verify says nothing (the signature is not
/// genuine, whenever it claims to be from), so the verification arm names it instead. Only a
/// delivery whose HMAC already verified can be called STALE.
fn stripe_rejection(
    secret: Option<&str>,
    signature: &str,
    signature_ok: bool,
    signed_at: Option<i64>,
    now: i64,
    tolerance_secs: i64,
) -> Option<StripeRejection> {
    // Nothing to verify with — the receiver cannot accept ANY event, and that is a state an
    // operator fixes from the admin panel, so asking for a retry is the right call.
    if secret.is_none() {
        return Some(StripeRejection {
            status: StatusCode::SERVICE_UNAVAILABLE,
            reason: "stripe_not_configured",
            audit_status: "not_configured",
        });
    }
    // A genuine Stripe delivery always carries `Stripe-Signature`; without it there is nothing
    // to verify against, and the secret itself is fine, so this gets its own reason.
    if signature.is_empty() {
        return Some(StripeRejection {
            status: StatusCode::SERVICE_UNAVAILABLE,
            reason: "stripe_signature_missing",
            audit_status: "signature_failed",
        });
    }
    if !signature_ok {
        return Some(StripeRejection {
            status: StatusCode::SERVICE_UNAVAILABLE,
            reason: "stripe_signature_verification_failed",
            audit_status: "signature_failed",
        });
    }
    // The signature is genuine, so the only thing left to decide is whether it is STILL GOOD.
    // `t=` is when Stripe generated the pair, and the pair is a bearer credential for exactly these
    // bytes: without this arm the same header+body verifies for ever (t_72a4bcdf). An absolute
    // difference, so a stamp far in the FUTURE — which would extend the window just as far as a
    // stamp in the past — is refused the same way. `signed_at == None` here means the `t=` field
    // verified as part of the payload but is not a number, so this delivery carries no clock to
    // bound: refused, because an unreadable stamp cannot bound anything.
    if signed_at.is_none_or(|t| (now - t).abs() > tolerance_secs) {
        return Some(StripeRejection {
            status: StatusCode::SERVICE_UNAVAILABLE,
            reason: "stripe_signature_timestamp_out_of_tolerance",
            audit_status: "signature_failed",
        });
    }
    None
}

/// POST /api/v1/webhooks/stripe
/// Handle incoming Stripe webhook events.
///
/// Fail CLOSED and LOUD (kanban t_40b77d6a). Closed: nothing that was not verified reaches
/// `handle_checkout_completed` — the previous shape accepted a delivery whenever the provider
/// row had no signing secret OR the request simply carried no `Stripe-Signature` header, so an
/// anonymous `POST {"type":"checkout.session.expired","data":{"object":{"id":…}}}` moved a
/// pending session's status. Loud: no refusal is answered 2xx. Stripe reads 2xx as "delivered"
/// and never retries it, so the old log-and-200 receiver lost the event for good with only a
/// `payment_webhook_events` row as evidence — a lost payment event, which is not a lost login.
///
/// Per-arm contract, with the reason each status was chosen:
///
/// - no active `stripe` row in `payment_providers`, or one with no signing secret stored ->
///   503 `stripe_not_configured`. The receiver is not able to accept events at all and Stripe
///   retries non-2xx with backoff for up to 3 days, so an event that arrives before the
///   endpoint's secret is pasted in Admin > Payment gateways is delivered again after it is.
/// - a signing secret IS stored and `Stripe-Signature` is absent -> 503
///   `stripe_signature_missing`.
/// - a signing secret IS stored and the signature does not verify -> 503
///   `stripe_signature_verification_failed`.
/// - the signature VERIFIES and its `t=` stamp is further from this host's clock than
///   `stripe_signature_tolerance_secs` -> 503 `stripe_signature_timestamp_out_of_tolerance`.
///
///   Why 503 here and the 401 that `paypal_webhook` answers for the same-sounding arm: PayPal's
///   verdict is computed by PayPal itself, so `verification_status != "SUCCESS"` is an
///   authoritative third-party statement that the delivery is not a genuine PayPal event and no
///   retry can change that. Stripe's verdict is computed HERE, as an HMAC over a secret this
///   deployment stores, so a mismatch is at least as likely to be OUR misconfiguration — a wrong
///   or stale secret, a rotation the panel has not applied yet — as a forged body, and the two
///   are indistinguishable from the bytes we are handed. Stripe re-signs every retry attempt, so
///   503 is the only answer that lets a repaired secret recover an already-lost receipt; a
///   permanently bad body costs a bounded number of retries (Stripe's own backoff, capped at 3
///   days) and is loud in three places: the ERROR line below, the audit row, and Stripe's
///   dashboard, which flags the endpoint as failing and tells the account owner.
/// - a body that is not JSON -> 400 `Invalid JSON`, kept deliberately AND parsed before
///   verification: a malformed body cannot be a Stripe event whatever the signature says, so 400
///   names the real problem (a proxy or a caller mangling the payload) instead of sending the
///   operator to the signing secret. This arm writes no audit row — there is no event id and no
///   structured body to store — so its record is the ERROR log line plus Stripe's dashboard.
/// - verified -> `200 processed`, and a failure that happens *after* verification (credential
///   delivery) still answers 5xx on purpose, from `handle_checkout_completed`.
///
/// Every refusal except the malformed body records its delivery in `payment_webhook_events` —
/// the only durable record — with a `status` that names WHICH arm fired (`not_configured` vs
/// `signature_failed`) and the reason in `error_message`.
///
/// WHY THIS RECEIVER BOUNDS THE AGE OF THE `t=` STAMP (the fourth arm; kanban t_72a4bcdf — DECIDED
/// on the sibling receiver as t_08628ca6, where the alternative "the replay is harmless" was
/// measured and rejected, so this card only ports the decided contract).
///
/// `{t}.{body}` is a bearer credential for exactly those bytes. The HMAC proves the bytes were
/// signed by someone holding the signing secret; it says nothing about WHEN, so before this arm a
/// `Stripe-Signature` header plus its body — captured from a proxy log, a misbehaving client, a
/// stale retry queue, or any endpoint that echoes headers — verified FOR EVER. That is not
/// theoretical on this receiver: MEASURED against the pre-fix binary (see
/// `/opt/swift/audits/t_72a4bcdf/`), a correctly-signed `checkout.session.expired` carrying a stamp
/// an hour old was accepted `200 {"status":"processed"}` and flipped a real `pending` row in
/// `checkout_sessions` to `expired`. `checkout.session.completed` no-ops once the session has left
/// `pending`, but the dispatch match is a growth point: every event type added to it inherits the
/// full replay. So the "harmless" answer is false today for `expired` and cannot be kept true for
/// tomorrow, and the bound is the smaller change.
///
/// The value is Stripe's own documented default — 300 s, `DEFAULT_STRIPE_SIGNATURE_TOLERANCE_SECS`,
/// overridable with `STRIPE_WEBHOOK_TOLERANCE_SECS` and clamped 30 s..24 h. It is Stripe's default
/// because that is the window their libraries use and any delivery Stripe itself makes lands inside
/// it; a deliberately smaller value would only add ways to refuse a genuine receipt. The comparison
/// is an ABSOLUTE difference, so a stamp far in the future — which would extend the window just as
/// far as a stamp in the past — is refused identically.
///
/// 503, like the other signature arms and for the same reason: this verdict is computed from THIS
/// host's clock, so a stale stamp is at least as likely to be our clock being wrong (or a box with
/// no NTP) as a replay, and the two are indistinguishable from the bytes. Stripe re-signs every
/// retry attempt — that is what makes the HMAC arm recoverable — so a genuine delivery, or one that
/// sat in a queue, verifies on the next attempt with a fresh stamp, while a captured header never
/// can. 401 would assert "not a genuine Stripe event", which we cannot support; 200 would silently
/// lose a receipt we had already refused. It cannot amplify: a forged request gets a 503 for itself
/// and nothing else.
///
/// Freshness is checked AFTER authenticity (`stripe_rejection` orders the arms), so an ancient stamp
/// on a signature that does not verify is reported as `stripe_signature_verification_failed` —
/// nothing is known to be genuine about it, whenever it claims to be from.
///
/// Distinguishable in the audit row without widening the existing status CHECK: the arm writes
/// `status = 'signature_failed'` (a signature that did not pass the receiver's contract for THIS
/// delivery) with `error_message = 'stripe_signature_timestamp_out_of_tolerance'`, names itself in
/// the response body, and logs its own ERROR line with the measured age and the tolerance in force —
/// which is also the line to read if EVERY delivery starts landing here: that means this host's
/// clock, not an attack.
pub async fn stripe_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<impl IntoResponse> {
    let event_body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(e) => {
            tracing::error!(
                "Stripe webhook rejected — the body is not JSON ({}), so it is not a Stripe \
                 event whatever the signature says. Answering 400; no audit row (no event id, \
                 no structured body to store).",
                e
            );
            return Err(AppError::BadRequest(format!("Invalid JSON: {}", e)));
        }
    };

    let event_type = event_body["type"].as_str().unwrap_or("unknown");
    let event_id = event_body["id"].as_str().unwrap_or("");

    // Read per event, never cached: pasting the endpoint's signing secret makes the very next
    // delivery verify, which is what makes the 503 arm recoverable instead of permanent.
    let provider = get_active_provider(&state.db, "stripe").await?;
    let signature = header_str(&headers, "stripe-signature");
    let secret = provider
        .as_ref()
        .and_then(|p| p["webhook_secret"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let signature_ok = match (secret, signature.is_empty()) {
        (Some(secret), false) => verify_stripe_signature(&body, signature, secret),
        _ => false,
    };
    // The `t=` stamp is only a clock once the HMAC verified (see `stripe_rejection`'s arm order):
    // an ancient stamp on a FORGED signature is just a forged signature. `now` is read once, here,
    // and the tolerance comes from `AppConfig` so a host with a wandering clock can be widened
    // without a rebuild (STRIPE_WEBHOOK_TOLERANCE_SECS).
    let signed_at = stripe_signature_timestamp(signature);
    let now = chrono::Utc::now().timestamp();
    let rejection = stripe_rejection(
        secret,
        signature,
        signature_ok,
        signed_at,
        now,
        state.config.stripe_signature_tolerance_secs,
    );

    // Log the delivery either way — a refusal is evidence and belongs in the audit table.
    let audit_status = match &rejection {
        None => "received",
        Some(r) => r.audit_status,
    };
    sqlx::query(
        r#"INSERT INTO payment_webhook_events
           (provider_type, event_type, event_id, raw_body, headers, status, error_message)
           VALUES ('stripe', $1, $2, $3, $4, $5, $6)"#,
    )
    .bind(event_type)
    .bind(event_id)
    .bind(&event_body)
    .bind(json!({"stripe-signature": signature}))
    .bind(audit_status)
    .bind(rejection.as_ref().map(|r| r.reason))
    .execute(&state.db)
    .await?;

    if let Some(rejection) = rejection {
        if rejection.audit_status == "not_configured" {
            tracing::error!(
                "Stripe webhook receiver is NOT CONFIGURED — no active 'stripe' row in \
                 payment_providers, or that row carries no signing secret, so no event can be \
                 verified. Answering 503 so Stripe retries (up to 3 days); paste the endpoint's \
                 whsec_… in Admin > Payment gateways and the retries verify. event_id={}",
                event_id
            );
        } else if rejection.reason == "stripe_signature_timestamp_out_of_tolerance" {
            // Its own line, because the operator action is different from "the secret is wrong":
            // the HMAC VERIFIED, so the secret is right and only the delivery's clock is off.
            tracing::error!(
                "Stripe webhook REJECTED — stripe_signature_timestamp_out_of_tolerance \
                 (event_id={}, signed_at={:?}, age={:?}s, tolerance={}s): the signature VERIFIED, \
                 so the signing secret is right, but this delivery's `t=` stamp is further from \
                 this host's clock than the tolerance — which is what stops a captured \
                 Stripe-Signature header from verifying for ever. Answering 503 so Stripe retries: \
                 Stripe re-signs every attempt, so a genuine delivery (or one that sat in a queue) \
                 verifies with a fresh stamp on the retry, while a captured header never can. If \
                 EVERY delivery starts landing here, this host's clock is the suspect — fix it, or \
                 widen STRIPE_WEBHOOK_TOLERANCE_SECS (default {}s).",
                event_id,
                signed_at,
                signed_at.map(|t| now - t),
                state.config.stripe_signature_tolerance_secs,
                DEFAULT_STRIPE_SIGNATURE_TOLERANCE_SECS
            );
        } else {
            tracing::error!(
                "Stripe webhook REJECTED — {} (event_id={}). Answering 503 so Stripe retries: if \
                 the stored signing secret is wrong, stale or mid-rotation, the retry is the only \
                 way this receipt is recovered; a forged body just gets retried and refused. \
                 Stripe marks the endpoint failing after repeated non-2xx.",
                rejection.reason,
                event_id
            );
        }
        return Ok((
            rejection.status,
            Json(json!({"status": "rejected", "reason": rejection.reason})),
        ));
    }

    // ── Verified from here on: only now may order state be touched ──
    match event_type {
        "checkout.session.completed" => {
            handle_checkout_completed(&state, &event_body, "stripe").await?;
        }
        "checkout.session.expired" => {
            if let Some(session) = event_body.get("data").and_then(|d| d.get("object")) {
                let provider_session_id = session["id"].as_str().unwrap_or("");
                mark_session_expired(&state.db, "stripe", provider_session_id).await?;
            }
        }
        _ => {
            // Ignore other event types
            sqlx::query("UPDATE payment_webhook_events SET status = 'ignored' WHERE event_id = $1")
                .bind(event_id)
                .execute(&state.db)
                .await?;
        }
    }

    // Mark webhook event as processed
    // (already handled inline above — the webhook was logged as 'received' and processed inline)

    Ok((StatusCode::OK, Json(json!({"status": "processed"}))))
}

/// POST /api/v1/webhooks/paypal
/// Handle incoming PayPal webhook events.
///
/// Fail closed, in this order, BEFORE anything is written or dispatched:
///   * any of the four `paypal-*` transmission headers missing -> 401
///     `missing_paypal_signature_headers`
///   * no webhook id, or no REST credentials to verify with -> 503
///     `paypal_not_configured` (PayPal is not called; no row is written)
///   * PayPal answered our verification call non-2xx -> 401 `paypal_verification_api_error`
///   * the verification call could not complete -> 401 `paypal_verification_unreachable`
///   * `verification_status != "SUCCESS"` -> 401 `signature_verification_failed`
/// Only a verified event reaches `payment_webhook_events` and `handle_checkout_completed`.
///
/// Why this arm exists at all (kanban t_5cf44e1b): the receiver used to read one header
/// for a log line and then INSERT the caller-supplied JSON and dispatch fulfilment on it,
/// so an anonymous `POST {"event_type":"PAYMENT.CAPTURE.COMPLETED","resource":{"id":…}}`
/// completed a checkout session and reached fulfilment with no signature whatsoever.
pub async fn paypal_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<impl IntoResponse> {
    let event_body: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON: {}", e)))?;

    // ── PayPal signature verification (fail closed) ──
    let trans_id = header_str(&headers, "paypal-transmission-id");
    let trans_time = header_str(&headers, "paypal-transmission-time");
    let trans_sig = header_str(&headers, "paypal-transmission-sig");
    let cert_url = header_str(&headers, "paypal-cert-url");

    if trans_id.is_empty() || trans_time.is_empty() || trans_sig.is_empty() || cert_url.is_empty() {
        tracing::error!("PayPal webhook rejected — missing signature headers");
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(json!({"status": "rejected", "reason": "missing_paypal_signature_headers"})),
        ));
    }

    // Resolve what the signature must be verified against. With no webhook id or no
    // credentials there is nothing to verify with, so the receiver says so (503) instead of
    // answering 200 as if it had processed the event.
    let verify_cfg = match paypal_verify_config(&state).await? {
        Some(cfg) => cfg,
        None => {
            tracing::error!(
                "PayPal webhook receiver is NOT CONFIGURED — no PAYPAL_WEBHOOK_ID and no \
                 webhook id on an active 'paypal' row in payment_providers, or no \
                 PAYPAL_CLIENT_ID/PAYPAL_CLIENT_SECRET and no usable api_key on that row. \
                 Rejecting without calling PayPal (fail closed)."
            );
            return Ok((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"status": "rejected", "reason": "paypal_not_configured"})),
            ));
        }
    };

    // PayPal verifies against the webhook id it was configured with, so which id is sent is
    // the difference between a real verdict and a blanket 401. PAYPAL_API_BASE is read per
    // request so the leg is sandbox- and test-addressable.
    let paypal_api_base =
        std::env::var("PAYPAL_API_BASE").unwrap_or_else(|_| "https://api-m.paypal.com".to_string());

    let verify_payload = json!({
        "auth_algo": header_str(&headers, "paypal-auth-algo"),
        "cert_url": cert_url,
        "transmission_id": trans_id,
        "transmission_sig": trans_sig,
        "transmission_time": trans_time,
        "webhook_id": verify_cfg.webhook_id,
        "webhook_event": &event_body,
    });

    // Bounded: a webhook task must not hang on a stalled third party. A timeout lands in the
    // transport arm below, which is a rejection, so the failure direction stays closed.
    let verify_resp = reqwest::Client::new()
        .post(format!(
            "{}/v1/notifications/verify-webhook-signature",
            paypal_api_base
        ))
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(10))
        .basic_auth(&verify_cfg.client_id, Some(&verify_cfg.client_secret))
        .json(&verify_payload)
        .send()
        .await;

    match verify_resp {
        Ok(resp) => {
            let status = resp.status();
            let raw = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                // PayPal answered our own call. That is NOT a signature verdict, so it gets
                // its own reason and its own log line (status + PayPal's own error body).
                tracing::error!(
                    "PayPal verify-webhook-signature answered non-2xx (status={}, body={}) — \
                     rejecting. Credentials source: {}",
                    status,
                    clip_for_log(&raw, 300),
                    verify_cfg.source
                );
                return Ok((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"status": "rejected", "reason": "paypal_verification_api_error"})),
                ));
            }
            let body: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
            if body["verification_status"] != "SUCCESS" {
                tracing::error!("PayPal webhook signature verification failed: {:?}", body);
                return Ok((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"status": "rejected", "reason": "signature_verification_failed"})),
                ));
            }
        }
        Err(e) => {
            // Transport leg — distinguishable from the non-2xx arm above, in BOTH the log and
            // the response reason.
            tracing::error!(
                "PayPal verify-webhook-signature call could not complete (transport error: {}) \
                 — rejecting. Credentials source: {}",
                e,
                verify_cfg.source
            );
            return Ok((
                StatusCode::UNAUTHORIZED,
                Json(json!({"status": "rejected", "reason": "paypal_verification_unreachable"})),
            ));
        }
    }

    // ── Verified: log the event and dispatch ──
    let event_type = event_body["event_type"].as_str().unwrap_or("unknown");
    let event_id = event_body["id"].as_str().unwrap_or("");

    // Log the webhook event
    let mut hdrs = json!({});
    if let Some(trans_id) = headers
        .get("paypal-transmission-id")
        .and_then(|v| v.to_str().ok())
    {
        hdrs["paypal-transmission-id"] = json!(trans_id);
    }

    sqlx::query(
        r#"INSERT INTO payment_webhook_events
           (provider_type, event_type, event_id, raw_body, headers, status)
           VALUES ('paypal', $1, $2, $3, $4, 'received')"#,
    )
    .bind(event_type)
    .bind(event_id)
    .bind(&event_body)
    .bind(&hdrs)
    .execute(&state.db)
    .await?;

    match event_type {
        "CHECKOUT.ORDER.APPROVED" | "PAYMENT.CAPTURE.COMPLETED" => {
            handle_checkout_completed(&state, &event_body, "paypal").await?;
        }
        _ => {
            sqlx::query("UPDATE payment_webhook_events SET status = 'ignored' WHERE event_id = $1")
                .bind(event_id)
                .execute(&state.db)
                .await?;
        }
    }

    Ok((StatusCode::OK, Json(json!({"status": "processed"}))))
}

// ──────────────────────────────────────────────
// Internal helpers
// ──────────────────────────────────────────────

/// Handle a completed checkout — update session status and trigger fulfillment
async fn handle_checkout_completed(
    state: &AppState,
    event_body: &serde_json::Value,
    provider_type: &str,
) -> Result<(), AppError> {
    let session = match provider_type {
        "stripe" => event_body["data"]["object"].clone(),
        "paypal" => event_body["resource"].clone(),
        _ => return Ok(()),
    };

    let provider_session_id = match provider_type {
        "stripe" => session["id"].as_str().map(|s| s.to_string()),
        "paypal" => session["id"].as_str().map(|s| s.to_string()),
        _ => None,
    };

    if provider_session_id.is_none() {
        tracing::warn!("Webhook received without provider session ID");
        return Ok(());
    }

    let provider_session_id = provider_session_id.unwrap();

    // Look up the checkout session before updating — grab customer_email from metadata
    let session_row = sqlx::query(
        r#"SELECT id, status, account_id, user_id, metadata, purchasable_type, purchasable_id
           FROM checkout_sessions
           WHERE provider_session_id = $1
             AND provider_type = $2"#,
    )
    .bind(&provider_session_id)
    .bind(provider_type)
    .fetch_optional(&state.db)
    .await?;

    // ── Credential delivery runs BEFORE the session is marked completed ──
    //
    // The mail IS the credential for a brand-new buyer: `deliver_credentials` generates the
    // password and the API never returns it, so recording the checkout as completed while the send
    // failed left a paying customer with nothing to log in with while every response said
    // "success" (log-only — the defect behind this card). The outcome is now persisted on the
    // session itself (`metadata.credential_delivery`), and a failure is surfaced as 5xx at the end
    // of this function so the payment provider retries the webhook; the retry re-generates the
    // password and re-sends, which is this buyer's only way back in.
    let mut delivery_error: Option<String> = None;
    if let Some(row) = session_row.as_ref() {
        let metadata: serde_json::Value = row.get("metadata");
        let customer_email = metadata
            .get("customer_email")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let customer_name = metadata
            .get("customer_name")
            .and_then(|v| v.as_str())
            .unwrap_or("Valued Customer");
        let session_status: String = row.get("status");
        let last_delivery = metadata
            .get("credential_delivery")
            .and_then(|d| d.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let session_account_id: Uuid = row.get("account_id");
        let session_ptype: String = row.get("purchasable_type");
        // The thing the buyer actually checked out, carried into the delivery so a NEW account this
        // session mints can be SEATED ON the plan it paid for (kanban t_9563b405) instead of
        // landing plan-less.
        let session_purchasable_id: Option<Uuid> = row.try_get("purchasable_id").ok().flatten();

        // Deliver on the first pass, or on a provider retry of a delivery that already failed.
        // A session completed before this change (no `credential_delivery` marker) is left alone:
        // re-sending credentials for an old purchase would be a surprise, not a repair.
        let needs_delivery =
            last_delivery != "sent" && (session_status == "pending" || last_delivery == "failed");

        if needs_delivery && !customer_email.is_empty() {
            let result = deliver_credentials(
                state,
                customer_email,
                customer_name,
                session_account_id,
                &session_ptype,
                session_purchasable_id,
            )
            .await;
            record_credential_delivery(&state.db, &provider_session_id, provider_type, &result)
                .await;
            match result {
                Ok(()) => tracing::info!(
                    "Credentials delivered for provider session {}",
                    provider_session_id
                ),
                Err(e) => {
                    tracing::error!("Failed to deliver credentials: {}", e);
                    delivery_error = Some(e);
                }
            }
        }
    }

    // Update the checkout session status
    let result = sqlx::query(
        r#"UPDATE checkout_sessions
           SET status = 'completed',
               webhook_received_at = NOW(),
               webhook_event_id = $1,
               updated_at = NOW()
           WHERE provider_session_id = $2
             AND provider_type = $3
             AND status = 'pending'"#,
    )
    .bind(event_body["id"].as_str().unwrap_or(""))
    .bind(&provider_session_id)
    .bind(provider_type)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        tracing::warn!(
            "No pending checkout session found for provider session: {} (already completed — \
             only credential delivery can still be outstanding)",
            provider_session_id
        );
        // The money-side effects below ran on the first pass, so skip them here. The delivery
        // outcome of *this* attempt is still authoritative: asking the provider to retry again is
        // the only way a buyer who never received credentials ever gets them.
        return match delivery_error {
            Some(e) => Err(delivery_failure(e)),
            None => Ok(()),
        };
    }

    // Mark the webhook event as processed
    sqlx::query("UPDATE payment_webhook_events SET status = 'processed' WHERE event_id = $1")
        .bind(event_body["id"].as_str().unwrap_or(""))
        .execute(&state.db)
        .await?;

    // ── Money-side effects ──
    //
    // Credential delivery already ran above, before the session flipped to 'completed'. This block
    // is only the purchase bookkeeping, and it must happen whether or not the mail went out: the
    // customer paid either way, so a mail outage must not also cost them the plan they bought.
    if let Some(row) = session_row {
        let _session_account_id: Uuid = row.get("account_id");
        let _ptype: String = row.get("purchasable_type");
        let purchasable_id: Option<Uuid> = row.try_get("purchasable_id").ok().flatten();

        // No cross-app conversion post here — deleted by kanban t_f6eb8834, do not re-add.
        //
        // What it did: `POST {FUNNELSWIFT_URL}/api/v1/webhooks/conversion` with
        // `{source_app, purchasable_type, provider_session_id}` — no `X-Internal-Key`, no `amount`,
        // no `lead_id`/`lead_email`/`affiliate_id` — and dropped the response (`let _ = …send()`).
        // Since kanban t_f408b7cc that receiver is a real, key-gated one, so the post answered
        // `401 Invalid internal key` (measured live, audits/t_f6eb8834/01-live-probes.txt) and
        // credited nothing — silently, because nothing read the response.
        //
        // Why deleted instead of repaired: WorkflowSwift holds no referral data at this point in
        // the flow. The crate names no affiliate/cookie/referral id, and the session's `metadata`
        // is whatever the caller posted (live rows carry `customer_name`/`customer_email` only).
        // The one attribution join key is the buyer's email, and the paid-plan path directly below
        // already sends it — key-authenticated — as the fleet's upgrade event. Measured against
        // the live receiver (audits/t_f6eb8834/02-arm-a-double-credit.json): that path credited the
        // probe commission (200 / 10.00 on a 100.00 sale); re-sending the SAME purchase through the
        // conversion receiver wrote a SECOND commission row (double pay) unless it carried the
        // upgrade event's own `event_id`, in which case the receiver answered `already-recorded`
        // and wrote nothing. So it could only double-pay or duplicate — never add a credit.
        //
        // Credit the referring affiliate for the paid-plan upgrade (if this is a plan purchase).
        if _ptype == "plan" {
            if let Some(plan_id) = purchasable_id {
                crate::handlers::plan_handler::attribute_plan_upgrade(
                    state,
                    _session_account_id,
                    plan_id,
                )
                .await;
            }
        }
    }

    tracing::info!(
        "Checkout completed: provider_session={}",
        provider_session_id
    );

    // The purchase is complete either way (the session is paid and the plan is attributed above).
    // The one thing that is *not* complete is the credential delivery: return 5xx so the payment
    // provider retries the webhook and the buyer gets a real second chance at their password.
    if let Some(e) = delivery_error {
        return Err(delivery_failure(e));
    }

    Ok(())
}

/// Turn a `deliver_credentials` failure into the response the payment provider gets.
///
/// A refusal that no retry can clear is a 4xx — asking the provider to retry forever would be a
/// defect of its own — and there are two of them, each named by its own prefix:
///   * `email: ` — the address in `checkout_sessions.metadata` is not an address (kanban
///     t_09e76b27);
///   * `permanent: ` — the address resolves to more than one user identity, so no single account
///     owns it and nothing may be minted or mailed for an arbitrary tenant (kanban t_8bcd0a8e);
///     only a data fix clears it.
/// Everything else is a transient delivery failure and stays 5xx so the retry can actually deliver
/// the credentials.
fn delivery_failure(e: String) -> AppError {
    if e.starts_with("email: ") || e.starts_with("permanent: ") {
        AppError::BadRequest(e)
    } else {
        AppError::Internal(format!("credential delivery failed: {}", e))
    }
}

/// The plan a checkout session's new account must be seated on, expressed as `plan_tiers.slug` —
/// the plan identity `auth::signup::create_account` takes (kanban t_9563b405, t_6d58f5be).
///
/// A plan purchase carries `purchasable_id` = the `plan_tiers` row the buyer checked out; it is
/// re-read here rather than trusted, so an id that is no longer an ACTIVE tier cannot seat an
/// entitlement this app does not sell. Anything else — a non-plan purchase, or a plan that no longer
/// resolves — seats the SAME default the public signup seats (`plan_tiers.slug = 'free'`, the value
/// `auth::handlers::register` defaults to), and only an app with NO active tier at all ends up
/// seating nothing: the writer writes no `account_plans` row for a slug that resolves to no active
/// tier, which is the one case where there is genuinely nothing to seat.
async fn checkout_plan_slug(
    db: &sqlx::PgPool,
    purchasable_type: &str,
    purchasable_id: Option<Uuid>,
) -> Result<String, String> {
    if purchasable_type == "plan" {
        if let Some(pid) = purchasable_id {
            let resolved: Option<String> = sqlx::query_scalar(
                "SELECT slug FROM plan_tiers WHERE id = $1 AND is_active = true",
            )
            .bind(pid)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("Failed to resolve the purchased plan: {}", e))?;
            if let Some(slug) = resolved {
                return Ok(slug);
            }
            tracing::warn!(
                plan_id = %pid,
                "checkout session's plan does not resolve to an ACTIVE plan tier — seating the \
                 signup default instead of leaving the new account plan-less"
            );
        }
    }

    let default: Option<String> = sqlx::query_scalar(
        "SELECT slug FROM plan_tiers WHERE slug = 'free' AND is_active = true LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .map_err(|e| format!("Failed to resolve the default plan: {}", e))?;
    if let Some(slug) = default {
        return Ok(slug);
    }

    let any: Option<String> = sqlx::query_scalar(
        "SELECT slug FROM plan_tiers WHERE is_active = true \
         ORDER BY sort_order ASC, created_at ASC LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .map_err(|e| format!("Failed to resolve the default plan: {}", e))?;
    // No active tier at all: `free` names nothing and the writer then seats no `account_plans` row.
    Ok(any.unwrap_or_else(|| "free".to_string()))
}

/// Deliver login credentials or purchase confirmation to the customer.
/// Flow:
/// 1. Look up user by email
/// 2. If user exists with password → send "purchase_confirmed"
/// 3. If user exists without password → generate password, hash, update, send "welcome"
/// 4. If user doesn't exist → the ONE account writer (`auth::signup::create_account`) mints the
///    WHOLE account on the plan the session paid for — its tags, industry dashboard,
///    `account_industries` row, n8n tenant config and API keys included — in ONE transaction
///    committed before the mail, and the welcome mail IS the credential
///    (`CredentialDelivery::AfterTheSend`; "get the mail out" is what the 5xx retry below
///    exists for — kanban t_9563b405, t_6d58f5be)
///
/// `Err` means the customer did **not** get their mail — for cases 3 and 4 the password it just
/// generated exists only in that message, so a caller must treat the error as a failed delivery
/// (not a log line). `handle_checkout_completed` does exactly that: it records the outcome on the
/// session and answers the payment provider with 5xx so the webhook is retried.
async fn deliver_credentials(
    state: &AppState,
    email: &str,
    name: &str,
    _session_account_id: Uuid,
    purchasable_type: &str,
    purchasable_id: Option<Uuid>,
) -> Result<(), String> {
    // ── Address boundary (kanban t_09e76b27) ────────────────────────────────────────────────
    // FIRST, before the lookup below and long before the `INSERT INTO users` that mints the
    // account. This value comes out of `checkout_sessions.metadata`, i.e. from whatever the payment
    // provider last posted, and every statement in this function — the dup lookup, the account/user
    // rows and the welcome mail that IS the credential — runs off it. Normalise + validate ONCE
    // here so a malformed address can never become a user row whose login nobody can reach.
    let email = email_addr::normalize(email)?;
    let email = email.as_str();

    // Look for the existing user. Case-insensitive, so a row written before this boundary existed
    // still matches the address the buyer actually typed, and resolved by IDENTITY rather than by
    // heap order (kanban t_8bcd0a8e).
    //
    // ── One address = ONE identity ──────────────────────────────────────────────────────────
    // This used to be `fetch_optional` on `lower(email)`, i.e. whichever row the heap returned —
    // and this function does not stop at reading: for a row it judges to have no password it MINTS
    // one and WRITES the hash, and for a row that has one it mails the buyer's receipt.
    // `users` is unique on `(aid, email)`, NOT on `email`, and this request carries no tenant and
    // no credential, so an address that maps to more than one identity must never resolve to an
    // arbitrary row: that is how a paid customer's receipt — or a freshly minted password — would
    // silently land on another tenant's user. Same rule as `auth::handlers::login`, through the
    // same resolver.
    let existing_user =
        match address_identity::resolve_address(&state.db, email, |user: &UserRow| {
            user.password_hash.as_str()
        })
        .await
        .map_err(|e| format!("DB lookup error: {}", e))?
        {
            AddressIdentity::Unique(user) => Some(user),
            AddressIdentity::Unknown => None,
            AddressIdentity::Ambiguous { rows, identities } => {
                // Nothing is mailed, minted or written. A retry cannot clear this (it is a data
                // condition, not a provider hiccup), so the caller answers it as a permanent refusal.
                tracing::error!(
                    rows,
                    identities,
                    "credential delivery refused: one address maps to more than one identity"
                );
                return Err(format!(
                "permanent: this address maps to {rows} user rows carrying {identities} usable \
                 credentials, so no single account owns it"
            ));
            }
        };

    let app_url = "https://app.workflowswift.com";

    if let Some(user) = existing_user {
        // User exists
        let has_password = !user.password_hash.is_empty()
            && !user.password_hash.is_empty()
            && user.password_hash != " ";

        if has_password {
            // Existing user — send purchase confirmation
            let vars = json!({
                "name": user.name,
                "email": user.email,
                "plan_name": purchasable_type,
                "app_url": app_url,
            });
            email::send_email(state, Some(user.aid), email, "purchase_confirmed", &vars).await
        } else {
            // User exists but has no usable password — mint one, send it, and only then store the
            // hash. Storing first would leave the row looking like "credentials delivered" if the
            // mail failed, and the retry would mail a receipt instead of the password.
            let password = generate_temp_password();
            let hash = hash_password(&password)
                .await
                .map_err(|e| format!("Password hashing failed: {}", e))?;

            let vars = json!({
                "name": user.name,
                "email": user.email,
                "password": password,
                "url": app_url,
                // The live `welcome` template (both halves) writes the login link as
                // `{{app_url}}`, so a message that carries only `url` renders the placeholder
                // VERBATIM — the buyer gets "Please log in at {{app_url}}" and an href that is not
                // a URL. Measured off the wire, kanban t_9563b405.
                "app_url": app_url,
            });
            let sent = email::send_email(state, Some(user.aid), email, "welcome", &vars).await;

            if sent.is_ok() {
                sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
                    .bind(&hash)
                    .bind(user.id)
                    .execute(&state.db)
                    .await
                    .map_err(|e| format!("Failed to update password: {}", e))?;
            }

            sent
        }
    } else {
        // ── No user for this address → the ONE account writer mints the WHOLE account ─────────
        //
        // DECISION (recorded here, as the card asks): this arm used to write its own
        // `INSERT INTO accounts` + `INSERT INTO users`, so a PAYING customer got an account with
        // none of the shape the ONE writer `auth::signup::create_account` mints — no default tags,
        // no industry dashboard (nothing to land on), no `account_industries` row, no n8n tenant
        // config and no API keys — plus two drifts of its own: `role = 'staff'`, a value no route
        // in this app recognises, and an `accounts.name` that was the raw customer name rather than
        // the writer's derivation. MEASURED live before this card (run pre1, leg L1: tags 0 /
        // dashboards 0 / account_industries 0 / n8n_account_config 0 / user_api_keys 0, role
        // 'staff', name 't6d58f5be Buyer'). The arm now calls the ONE writer with the plan the
        // session paid for, exactly as the tag/provision door does, so the account a paid buyer
        // gets IS the account a signup gets.
        //
        // The credential contract is the ONE thing that differs between the doors, and it is
        // EXPRESSED rather than bypassed: `CredentialDelivery::AfterTheSend` commits
        // `password_hash = ''` — the marker the `has_password == false` branch above reads — sends
        // the welcome mail (the only copy of the password), and stores the hash only once the send
        // is out. A refused send comes back as `Err` with the account, and its whole shape, already
        // committed, which is what the payment provider's retry then completes: a FRESH password to
        // the SAME account (kanban t_9563b405), landing on a workspace that is complete (this card).
        let password = generate_temp_password();
        let password_hash = hash_password(&password)
            .await
            .map_err(|e| format!("Password hashing failed: {}", e))?;
        let plan_slug = checkout_plan_slug(&state.db, purchasable_type, purchasable_id).await?;

        let ids = crate::auth::signup::create_account(
            state,
            crate::auth::signup::NewAccount {
                email,
                name,
                password_hash: &password_hash,
                password_plain: Some(&password),
                // `None` on both: the writer's own derivation (`"<name>'s Workspace"`, plus a
                // collision-retried slug) — the shape the public signup mints.
                account_name: None,
                account_slug: None,
                plan_slug: &plan_slug,
                // The public signup's own default industry, so the buyer lands on the dashboard a
                // signing-up customer gets.
                industry_slug: "site-flipping",
                // The writer's owner role. The literal this arm used to write, `staff`, is in no
                // role vocabulary this app accepts (`handlers::user_handler`: user / company_admin
                // / admin / super_admin / agency_admin / team_member).
                role: "user",
                credential_delivery: crate::auth::signup::CredentialDelivery::AfterTheSend,
                // DELIBERATELY `None` (kanban t_4a744754): this is the paid-checkout credential
                // path (a provider-signed payment webhook), not the served, unauthenticated signup —
                // the only door the `X-Swift-Harness` marker belongs to.
                probe_harness: None,
            },
        )
        .await
        .map_err(|e| format!("Failed to mint the account for the paid checkout: {}", e))?;

        tracing::info!(
            account = %ids.account_id,
            user = %ids.user_id,
            email = %email,
            plan = %plan_slug,
            "paid checkout minted an account on the plan the session was opened for, with the ONE \
             writer's whole shape"
        );

        // The credential outcome is `create_account`'s own return value — `Ok` here means the mail,
        // and therefore the buyer's password, is out.
        Ok(())
    }
}

/// Persist the latest credential-delivery outcome on the checkout session itself
/// (`metadata.credential_delivery` = `{status, at, error}`).
///
/// The session row is where a paid-but-undelivered purchase is visible to an admin
/// (`GET /api/v1/checkout-sessions` returns this metadata), and it is what a provider retry reads
/// to decide whether the delivery still needs to happen. Best-effort: bookkeeping must never turn a
/// successful send into a failure.
async fn record_credential_delivery(
    db: &sqlx::PgPool,
    provider_session_id: &str,
    provider_type: &str,
    result: &Result<(), String>,
) {
    let outcome = match result {
        Ok(()) => json!({
            "status": "sent",
            "at": chrono::Utc::now().to_rfc3339(),
            "error": null,
        }),
        Err(e) => json!({
            "status": "failed",
            "at": chrono::Utc::now().to_rfc3339(),
            "error": e,
        }),
    };

    let _ = sqlx::query(
        r#"UPDATE checkout_sessions
           SET metadata = jsonb_set(metadata, '{credential_delivery}', $1::jsonb, true),
               updated_at = NOW()
           WHERE provider_session_id = $2
             AND provider_type = $3"#,
    )
    .bind(outcome.to_string())
    .bind(provider_session_id)
    .bind(provider_type)
    .execute(db)
    .await;
}

/// Generate a cryptographically random temporary password (12 chars)
fn generate_temp_password() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789!@#";
    let mut rng = rand::thread_rng();
    let pass: String = (0..12)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect();
    pass
}

/// Hash a password using Argon2 — off the reactor and bounded by the process-wide Argon2
/// semaphore (`auth::api_key_auth::argon2_hash`), so a webhook-triggered credential delivery
/// cannot park a tokio worker thread while it computes 19 MiB of Argon2.
///
/// The `Result<_, String>` contract is unchanged (callers map the error to
/// `format!("Password hashing failed: {e}")`); only the inner message is now the hash
/// helper's own reason instead of the raw `argon2::password_hash::Error` display.
async fn hash_password(password: &str) -> Result<String, String> {
    crate::auth::api_key_auth::argon2_hash(password.to_string())
        .await
        .map_err(|e| e.to_string())
}

// ── Data types for credential delivery ──

#[derive(Debug, sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    aid: Uuid,
    password_hash: String,
    email: String,
    name: String,
}

/// Mark a checkout session as expired
async fn mark_session_expired(
    db: &sqlx::PgPool,
    provider_type: &str,
    provider_session_id: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE checkout_sessions SET status = 'expired', updated_at = NOW() \
         WHERE provider_session_id = $1 AND provider_type = $2 AND status = 'pending'",
    )
    .bind(provider_session_id)
    .bind(provider_type)
    .execute(db)
    .await?;

    Ok(())
}

// ──────────────────────────────────────────────
// GET /api/v1/checkout/sessions
// List checkout sessions for the authenticated account
// ──────────────────────────────────────────────

pub async fn list_checkout_sessions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        r#"SELECT id, provider_type, purchasable_type, purchasable_id::text,
                  amount::text, currency, status, provider_session_id,
                  metadata -> 'credential_delivery' AS credential_delivery,
                  created_at, updated_at
           FROM checkout_sessions
           WHERE account_id = $1
           ORDER BY created_at DESC
           LIMIT 50"#,
    )
    .bind(account_id)
    .fetch_all(&state.db)
    .await?;

    let sessions: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "provider_type": r.try_get::<&str,_>("provider_type").unwrap_or(""),
            "purchasable_type": r.try_get::<&str,_>("purchasable_type").unwrap_or(""),
            "purchasable_id": r.try_get::<Option<&str>,_>("purchasable_id").unwrap_or(None),
            "amount": r.try_get::<&str,_>("amount").unwrap_or("0"),
            "currency": r.try_get::<&str,_>("currency").unwrap_or(""),
            "status": r.try_get::<&str,_>("status").unwrap_or(""),
            "credential_delivery": r.try_get::<Option<serde_json::Value>,_>("credential_delivery").unwrap_or(None),
            "provider_session_id": r.try_get::<Option<&str>,_>("provider_session_id").unwrap_or(None),
            "created_at": r.try_get::<chrono::DateTime<chrono::Utc>,_>("created_at")
                .map(|t| t.to_rfc3339()).unwrap_or_default(),
            "updated_at": r.try_get::<chrono::DateTime<chrono::Utc>,_>("updated_at")
                .map(|t| t.to_rfc3339()).unwrap_or_default(),
        })
    }).collect();

    Ok(Json(json!({"sessions": sessions})))
}

/// Verify Stripe webhook signature using the signing secret
///
/// This answers ONE question — "do these bytes carry an HMAC made with our signing secret, over
/// exactly this `t=` and exactly this body?" — and nothing about WHEN. Freshness is a separate arm
/// (see `stripe_rejection`); the two read the header through `stripe_signature_parts` so they can
/// never disagree about the bytes that were signed.
fn verify_stripe_signature(body: &[u8], signature: &str, secret: &str) -> bool {
    // Stripe sends signatures in the format: t=timestamp,v1=signature
    // We need to extract the v1 signature and verify with HMAC-SHA256

    let (timestamp, expected_sig) = stripe_signature_parts(signature);
    let (Some(timestamp), Some(expected_sig)) = (timestamp, expected_sig) else {
        return false;
    };

    if timestamp.is_empty() || expected_sig.is_empty() {
        return false;
    }

    // Build the payload: timestamp + "." + body
    let payload = format!("{}.{}", timestamp, std::str::from_utf8(body).unwrap_or(""));

    // Compute HMAC-SHA256
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(payload.as_bytes());
    let computed = mac.finalize().into_bytes();

    // Compare in constant time
    let computed_hex = hex::encode(computed);
    // Use a simple comparison (constant-time would be ideal but for practical purposes this is fine)
    computed_hex == expected_sig
}

/// Convert a JSON value to URL-encoded form data for Stripe API
fn to_stripe_form_data(value: &serde_json::Value) -> Vec<(String, String)> {
    let mut pairs = Vec::new();

    fn flatten(prefix: &str, value: &serde_json::Value, pairs: &mut Vec<(String, String)>) {
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    let key = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{}[{}]", prefix, k)
                    };
                    flatten(&key, v, pairs);
                }
            }
            serde_json::Value::Array(arr) => {
                for (i, v) in arr.iter().enumerate() {
                    let key = format!("{}[{}]", prefix, i);
                    flatten(&key, v, pairs);
                }
            }
            serde_json::Value::String(s) => {
                pairs.push((prefix.to_string(), s.clone()));
            }
            serde_json::Value::Number(n) => {
                pairs.push((prefix.to_string(), n.to_string()));
            }
            serde_json::Value::Bool(b) => {
                pairs.push((prefix.to_string(), b.to_string()));
            }
            serde_json::Value::Null => {
                pairs.push((prefix.to_string(), String::new()));
            }
        }
    }

    flatten("", value, &mut pairs);
    pairs
}

/// Base64-encode a client_id:secret pair for PayPal Basic auth
fn base64_encode_auth(credentials: &str) -> String {
    // PayPal uses client_id:secret as the Basic auth token
    use base64::{engine::general_purpose, Engine as _};
    general_purpose::STANDARD.encode(credentials.as_bytes())
}

#[cfg(test)]
mod stripe_contract_tests {
    use super::*;

    /// The tolerance in force for the tests below: the shipped default.
    const TOL: i64 = DEFAULT_STRIPE_SIGNATURE_TOLERANCE_SECS;
    /// A FIXED "now". The contract is a function of the clock, so the tests pass one in rather than
    /// reading the wall clock — otherwise a test could flake on a slow machine.
    const NOW: i64 = 1_800_000_000;

    /// `stripe_rejection` as a one-line string, so the whole contract reads as a table. Assumes the
    /// delivery's stamp is current (the freshness arm has its own test below).
    fn arm(secret: Option<&str>, signature: &str, ok: bool) -> String {
        arm_at(secret, signature, ok, Some(NOW))
    }

    /// The same, with an explicit `t=` stamp — `None` meaning "no readable stamp".
    fn arm_at(secret: Option<&str>, signature: &str, ok: bool, signed_at: Option<i64>) -> String {
        match stripe_rejection(secret, signature, ok, signed_at, NOW, TOL) {
            None => "verified".to_string(),
            Some(r) => format!("{} {} {}", r.status.as_u16(), r.reason, r.audit_status),
        }
    }

    #[test]
    fn stripe_receiver_refuses_without_asking_stripe_to_retry_forever() {
        // Nothing to verify with (no active row, or a row with no secret): 503, and the audit
        // row says `not_configured` rather than the old catch-all `failed`.
        assert_eq!(
            arm(None, "", false),
            "503 stripe_not_configured not_configured"
        );
        // The same arm even when the caller claims verification: `None` cannot be overridden.
        assert_eq!(
            arm(None, "t=1,v1=aa", true),
            "503 stripe_not_configured not_configured"
        );
        // A secret is stored and the delivery carried no signature at all.
        assert_eq!(
            arm(Some("whsec_probe"), "", false),
            "503 stripe_signature_missing signature_failed"
        );
        // A secret is stored and the signature does not verify.
        assert_eq!(
            arm(Some("whsec_probe"), "t=1,v1=aa", false),
            "503 stripe_signature_verification_failed signature_failed"
        );
        // Only a positive verdict is admitted, and it is never a rejection.
        assert_eq!(arm(Some("whsec_probe"), "t=1,v1=aa", true), "verified");
    }

    /// t_72a4bcdf (the arm ADASwift decided as t_08628ca6): a signature that VERIFIES is not a
    /// licence to accept the body for ever. The `{t}.{body}` pair is a bearer credential, so a
    /// stamp outside the tolerance is refused even though the HMAC is genuine — in BOTH directions
    /// around this host's clock, and only for a delivery whose authenticity was already
    /// established.
    #[test]
    fn stripe_receiver_refuses_a_signature_whose_stamp_is_not_current() {
        // The ordinary case: a stamp at "now" verifies.
        assert_eq!(arm(Some("whsec_probe"), "t=1,v1=aa", true), "verified");
        // The boundary is INCLUSIVE (the comparison is `>`), and just inside it still verifies.
        assert_eq!(
            arm_at(Some("whsec_probe"), "t=1,v1=aa", true, Some(NOW - TOL)),
            "verified"
        );
        assert_eq!(
            arm_at(Some("whsec_probe"), "t=1,v1=aa", true, Some(NOW - TOL + 1)),
            "verified"
        );
        // One second past the bound in the PAST: this is the replay — a header captured earlier,
        // still carrying a genuine HMAC over a body we would act on.
        assert_eq!(
            arm_at(Some("whsec_probe"), "t=1,v1=aa", true, Some(NOW - TOL - 1)),
            "503 stripe_signature_timestamp_out_of_tolerance signature_failed"
        );
        // The shape the card describes: a delivery captured an hour ago, and one from weeks ago.
        assert_eq!(
            arm_at(Some("whsec_probe"), "t=1,v1=aa", true, Some(NOW - 3600)),
            "503 stripe_signature_timestamp_out_of_tolerance signature_failed"
        );
        assert_eq!(
            arm_at(
                Some("whsec_probe"),
                "t=1,v1=aa",
                true,
                Some(NOW - 86_400 * 30)
            ),
            "503 stripe_signature_timestamp_out_of_tolerance signature_failed"
        );
        // The FUTURE is bounded identically, or a stamp dated next month would hand the same
        // captured pair a month-long window.
        assert_eq!(
            arm_at(
                Some("whsec_probe"),
                "t=1,v1=aa",
                true,
                Some(NOW + 86_400 * 30)
            ),
            "503 stripe_signature_timestamp_out_of_tolerance signature_failed"
        );
        // ... while ordinary clock skew is NOT an outage: a few seconds either way still verifies.
        assert_eq!(
            arm_at(Some("whsec_probe"), "t=1,v1=aa", true, Some(NOW + 5)),
            "verified"
        );
        assert_eq!(
            arm_at(Some("whsec_probe"), "t=1,v1=aa", true, Some(NOW - 5)),
            "verified"
        );
        // A `t=` whose text verified as part of the payload but is not a number carries no clock,
        // so it cannot be bounded — refused rather than waved through.
        assert_eq!(
            arm_at(Some("whsec_probe"), "t=nope,v1=aa", true, None),
            "503 stripe_signature_timestamp_out_of_tolerance signature_failed"
        );
        // FRESHNESS IS DECIDED AFTER AUTHENTICITY: an ancient stamp on a FORGED signature is just a
        // forged signature, and the audit row must say that rather than blame this host's clock.
        assert_eq!(
            arm_at(Some("whsec_probe"), "t=1,v1=aa", false, Some(NOW - 86_400)),
            "503 stripe_signature_verification_failed signature_failed"
        );
        // ... and a deployment with nothing to verify with is named as not_configured, not as stale.
        assert_eq!(
            arm_at(None, "t=1,v1=aa", true, Some(NOW - 86_400)),
            "503 stripe_not_configured not_configured"
        );
        // A missing `Stripe-Signature` header is still `signature_missing`, even with no stamp.
        assert_eq!(
            arm_at(Some("whsec_probe"), "", false, None),
            "503 stripe_signature_missing signature_failed"
        );
        // The arm answers 503 like every other signature refusal (Stripe retries, so a genuine
        // delivery is re-signed; Stripe also flags the endpoint failing, so a wrong clock is loud),
        // and it lands in the audit table with the existing `signature_failed` vocabulary — the
        // reason in `error_message` is what distinguishes it.
        let r = stripe_rejection(
            Some("whsec_probe"),
            "t=1,v1=aa",
            true,
            Some(NOW - 3 * TOL),
            NOW,
            TOL,
        )
        .expect("a stale stamp must be a refusal");
        assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(r.reason, "stripe_signature_timestamp_out_of_tolerance");
        assert_eq!(r.audit_status, "signature_failed");
    }

    /// Both arms read `Stripe-Signature` through the same parser — the verifier hashes the `t=`
    /// text it is given, and the freshness arm has to agree on what that text is — and the
    /// tolerance is the deliberate 300 s Stripe ships, not whatever a variable happened to hold.
    #[test]
    fn the_header_is_read_once_and_the_tolerance_is_the_decided_value() {
        assert_eq!(stripe_signature_timestamp("t=1800000000,v1=aa"), Some(NOW));
        assert_eq!(stripe_signature_timestamp("v1=aa,t=1800000000"), Some(NOW));
        assert_eq!(stripe_signature_timestamp("t=1800000000"), Some(NOW));
        assert_eq!(stripe_signature_timestamp("v1=aa"), None);
        assert_eq!(stripe_signature_timestamp(""), None);
        assert_eq!(stripe_signature_timestamp("t=nope,v1=aa"), None);
        assert_eq!(stripe_signature_parts("t=1,v1=aa"), (Some("1"), Some("aa")));
        assert_eq!(stripe_signature_parts("nolabel"), (None, None));
        assert_eq!(DEFAULT_STRIPE_SIGNATURE_TOLERANCE_SECS, 300);
    }

    /// The arm that used to answer 200 must not answer 2xx any more: a 2xx tells Stripe the event
    /// was delivered, so it never retries and the receipt is lost silently. Covers every arm,
    /// including the two freshness directions.
    #[test]
    fn no_refusal_arm_is_answered_2xx() {
        for (secret, signature, ok, signed_at) in [
            (None, "", false, Some(NOW)),
            (None, "t=1,v1=aa", true, Some(NOW)),
            (Some("whsec_probe"), "", false, Some(NOW)),
            (Some("whsec_probe"), "t=1,v1=aa", false, Some(NOW)),
            (Some("whsec_probe"), "t=1,v1=aa", true, Some(NOW - 86_400)),
            (Some("whsec_probe"), "t=1,v1=aa", true, Some(NOW + 86_400)),
            (Some("whsec_probe"), "t=1,v1=aa", true, None),
        ] {
            let rejection = stripe_rejection(secret, signature, ok, signed_at, NOW, TOL)
                .expect("must be a refusal");
            assert!(
                !rejection.status.is_success(),
                "arm {secret:?}/{signature:?}/{ok}/{signed_at:?} must not be 2xx, got {}",
                rejection.status
            );
        }
    }
}

/// Pins the ONE role list for the whole payment-provider surface (kanban t_592cd45d). The defect
/// this guards was not a wrong predicate — there was no predicate on the read arm at all — so the
/// tests fix the decision in place: a future widening of the LIST has to fail here first and
/// justify itself, exactly as a future narrowing would.
#[cfg(test)]
mod payment_provider_role_tests {
    use super::*;

    fn claims(perm: Option<bool>) -> Claims {
        Claims {
            // Minted, not literals: a hardcoded UUID in src/ is a gate finding (the gate wants
            // lookups by slug, not by a baked-in id) and none of these tests care about the value.
            sub: Uuid::new_v4().to_string(),
            aid: Uuid::new_v4().to_string(),
            role: "user".into(),
            exp: 1_800_000_000,
            iat: 1_799_999_400,
            perm_is_super_admin: perm,
        }
    }

    /// The super-admin flag ON is the only thing that opens the surface — and it opens all three
    /// arms, because all three call this one predicate.
    #[test]
    fn the_super_admin_flag_opens_the_payment_provider_surface() {
        assert!(can_manage_payment_providers(&claims(Some(true))));
    }

    /// Everything else is refused: a tenant, a company admin, and a token minted without the flag
    /// at all (any non-super-admin login).
    #[test]
    fn tenant_and_flagless_sessions_are_refused() {
        for perm in [Some(false), None] {
            assert!(
                !can_manage_payment_providers(&claims(perm)),
                "perm={perm:?} must not manage payment providers"
            );
        }
    }

    /// The role WORD must not decide. A `super_admin` role string with the flag off is the shape
    /// the read arm used to wave through unchecked; it stays refused, so "authenticated" can never
    /// quietly become "authorised" here again.
    #[test]
    fn the_role_word_alone_is_not_enough() {
        let mut c = claims(Some(false));
        c.role = "super_admin".into();
        assert!(!can_manage_payment_providers(&c));
    }
}
#[cfg(test)]
mod payment_key_check_tests {
    use super::{provider_display, stripe_livemode_message};

    /// The three Stripe outcomes. The two `Some(..)` arms need a real credential to reach live
    /// (this install has none), so the statement the operator reads is pinned here instead.
    #[test]
    fn a_successful_stripe_check_reports_the_mode_it_actually_is() {
        assert!(
            stripe_livemode_message(Some(true)).contains("LIVE"),
            "a live key must say so"
        );
        assert!(
            stripe_livemode_message(Some(false)).contains("TEST-mode"),
            "a test key is a working key that charges nobody — it must say so"
        );
        // A body without the flag claims nothing about the mode.
        assert_eq!(stripe_livemode_message(None), "Stripe accepted the key.");
    }

    /// The refusal names the provider that answered, and an unknown id is passed through.
    #[test]
    fn the_prose_names_the_provider_that_answered() {
        assert_eq!(provider_display("stripe"), "Stripe");
        assert_eq!(provider_display("paypal"), "PayPal");
        assert_eq!(provider_display("adyen"), "adyen");
    }
}
