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
use crate::AppState;
use sqlx::Row;

// ──────────────────────────────────────────────
// Admin: Payment Provider CRUD
// ──────────────────────────────────────────────

/// GET /api/v1/payment-providers
/// List all configured payment providers (keys masked)
pub async fn list_payment_providers(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    // Only super admin can manage payment providers
    let rows = sqlx::query(
        r#"SELECT id, provider_type, label, is_active,
                  CASE WHEN api_key_encrypted IS NOT NULL AND api_key_encrypted != '' THEN 'configured' ELSE 'not_configured' END as key_status,
                  COALESCE(publishable_key, '') as publishable_key,
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
    // Super admin only
    if !claims.perm_is_super_admin.unwrap_or(false) {
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

    // Check if provider already exists
    let existing =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM payment_providers WHERE provider_type = $1")
            .bind(provider_type)
            .fetch_optional(&state.db)
            .await?;

    if let Some(provider_id) = existing {
        // Update — only overwrite api_key/webhook_secret if provided
        let mut query = String::from(
            "UPDATE payment_providers SET label = $1, is_active = $2, is_test_mode = $3, \
             publishable_key = $4, config = $5, updated_at = NOW()",
        );
        let mut param_idx = 6u8;

        if !api_key.is_empty() {
            query.push_str(&format!(", api_key_encrypted = ${}", param_idx));
            param_idx += 1;
        }
        if !webhook_secret.is_empty() {
            query.push_str(&format!(", webhook_secret_encrypted = ${}", param_idx));
            param_idx += 1;
        }
        query.push_str(&format!(" WHERE id = ${}", param_idx));

        let mut q = sqlx::query(&query)
            .bind(label)
            .bind(is_active)
            .bind(is_test_mode)
            .bind(publishable_key)
            .bind(&config);

        if !api_key.is_empty() {
            q = q.bind(api_key);
        }
        if !webhook_secret.is_empty() {
            q = q.bind(webhook_secret);
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
        .bind(api_key)
        .bind(webhook_secret)
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
    if !claims.perm_is_super_admin.unwrap_or(false) {
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

// ──────────────────────────────────────────────
// Checkout Session Creation
// ──────────────────────────────────────────────

/// Get active payment provider configuration
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

    Ok(row.map(|r| {
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "provider_type": r.try_get::<&str,_>("provider_type").unwrap_or(""),
            "api_key": r.try_get::<Option<&str>,_>("api_key_encrypted").unwrap_or(None).unwrap_or(""),
            "publishable_key": r.try_get::<Option<&str>,_>("publishable_key").unwrap_or(None).unwrap_or(""),
            "webhook_secret": r.try_get::<Option<&str>,_>("webhook_secret_encrypted").unwrap_or(None).unwrap_or(""),
            "is_test_mode": r.try_get::<bool,_>("is_test_mode").unwrap_or(true),
        })
    }))
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

    // Resolve payment provider: explicit > plan's payment_provider > error
    let provider_type = if let Some(pt) = req.get("provider_type").and_then(|v| v.as_str()) {
        pt.to_string()
    } else if purchasable_type == "plan" {
        if let Some(pid) = req
            .get("purchasable_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT payment_provider FROM plan_tiers WHERE id = $1",
            )
            .bind(pid)
            .fetch_optional(&state.db)
            .await?
            .flatten()
            .ok_or_else(|| {
                AppError::BadRequest(
                    "No provider_type specified and plan has no payment_provider set".into(),
                )
            })?
        } else {
            return Err(AppError::BadRequest(
                "purchasable_id is required for plan checkout".into(),
            ));
        }
    } else {
        return Err(AppError::BadRequest(
            "provider_type is required (stripe, paypal)".into(),
        ));
    };

    let amount = req
        .get("amount")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| AppError::BadRequest("amount is required".into()))?;

    let currency = req
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("USD");
    let purchasable_id = req
        .get("purchasable_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());

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

    // Get the active provider config
    let provider = get_active_provider(&state.db, &provider_type)
        .await?
        .ok_or_else(|| {
            AppError::BadRequest(format!("No active {} provider configured", provider_type))
        })?;

    let api_key = provider["api_key"].as_str().unwrap_or("").to_string();
    if api_key.is_empty() {
        return Err(AppError::BadRequest(format!(
            "{} API key not configured",
            provider_type
        )));
    }

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

    Ok(Json(json!({
        "session_id": session_id.to_string(),
        "provider_session_id": provider_session_id,
        "checkout_url": checkout_url,
        "provider_type": provider_type,
    })))
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
        .map_err(|e| AppError::Internal(format!("Stripe API error: {}", e)))?;

    let status = resp.status();
    let response_body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to parse Stripe response: {}", e)))?;

    if !status.is_success() {
        let error_msg = response_body["error"]["message"]
            .as_str()
            .unwrap_or("Unknown Stripe error");
        return Err(AppError::Internal(format!("Stripe error: {}", error_msg)));
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
        .map_err(|e| AppError::Internal(format!("PayPal auth error: {}", e)))?;

    let token_body: serde_json::Value = token_resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to parse PayPal auth response: {}", e)))?;

    let access_token = token_body["access_token"]
        .as_str()
        .ok_or_else(|| AppError::Internal("Failed to get PayPal access token".into()))?;

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
        .map_err(|e| AppError::Internal(format!("PayPal order error: {}", e)))?;

    let order_status = order_resp.status();
    let order_body: serde_json::Value = order_resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to parse PayPal order response: {}", e)))?;

    if !order_status.is_success() {
        let error_msg = order_body["message"]
            .as_str()
            .or_else(|| order_body["error_description"].as_str())
            .unwrap_or("Unknown PayPal error");
        return Err(AppError::Internal(format!("PayPal error: {}", error_msg)));
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
            Some(e) => Err(AppError::Internal(format!(
                "credential delivery failed: {}",
                e
            ))),
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

        // FunnelSwift affiliate conversion webhook (fire-and-forget)
        let psid = provider_session_id.clone();
        let ptype = _ptype.clone();
        tokio::spawn(async move {
            let funnelswift_url = std::env::var("FUNNELSWIFT_URL").unwrap_or_default();
            if !funnelswift_url.is_empty() {
                let _ = reqwest::Client::new()
                    .post(format!("{}/api/v1/webhooks/conversion", funnelswift_url))
                    .json(&serde_json::json!({
                        "source_app": "workflowswift",
                        "purchasable_type": ptype,
                        "provider_session_id": psid,
                    }))
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .await;
            }
        });

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
        return Err(AppError::Internal(format!(
            "credential delivery failed: {}",
            e
        )));
    }

    Ok(())
}

/// Deliver login credentials or purchase confirmation to the customer.
/// Flow:
/// 1. Look up user by email
/// 2. If user exists with password → send "purchase_confirmed"
/// 3. If user exists without password → generate password, hash, update, send "welcome"
/// 4. If user doesn't exist → create account + user, send "welcome"
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
    _purchasable_type: &str,
) -> Result<(), String> {
    // Look for existing user
    let existing_user = sqlx::query_as::<_, UserRow>(
        "SELECT id, aid, password_hash, email, name FROM users WHERE email = $1",
    )
    .bind(email)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| format!("DB lookup error: {}", e))?;

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
                "plan_name": _purchasable_type,
                "app_url": app_url,
            });
            email::send_email(state, email, "purchase_confirmed", &vars).await
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
            });
            let sent = email::send_email(state, email, "welcome", &vars).await;

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
        // New user — create account + user, send credentials
        let account_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let password = generate_temp_password();
        let hash = hash_password(&password)
            .await
            .map_err(|e| format!("Password hashing failed: {}", e))?;
        let slug = format!("cust-{}", &user_id.to_string()[..8]);

        // Create account
        sqlx::query(
            "INSERT INTO accounts (id, name, account_slug, is_active) VALUES ($1, $2, $3, true)",
        )
        .bind(account_id)
        .bind(name)
        .bind(&slug)
        .execute(&state.db)
        .await
        .map_err(|e| format!("Failed to create account: {}", e))?;

        // Create user linked to account — deliberately WITHOUT a usable password yet.
        //
        // The generated password exists only in the welcome mail, so it must not become the stored
        // credential before the mail is actually out. An empty `password_hash` is the marker for
        // "these credentials were never delivered": `login` answers 401 for it, and a retry of the
        // checkout webhook lands in the `has_password == false` branch above, mints a fresh
        // password and sends it. Committing the hash here would make the retry send a *receipt* to
        // a buyer who never received credentials.
        sqlx::query(
            r#"INSERT INTO users (id, aid, email, password_hash, name, role, is_active)
               VALUES ($1, $2, $3, '', $4, 'staff', true)"#,
        )
        .bind(user_id)
        .bind(account_id)
        .bind(email)
        .bind(name)
        .execute(&state.db)
        .await
        .map_err(|e| format!("Failed to create user: {}", e))?;

        // Send welcome email with credentials
        let vars = json!({
            "name": name,
            "email": email,
            "password": password,
            "url": app_url,
        });
        let sent = email::send_email(state, email, "welcome", &vars).await;

        if sent.is_ok() {
            // Only now is the password worth storing — the customer has it in their inbox.
            sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
                .bind(&hash)
                .bind(user_id)
                .execute(&state.db)
                .await
                .map_err(|e| format!("Failed to store password: {}", e))?;
        } else {
            tracing::error!(
                "Credentials for new account {} were NOT delivered — leaving password_hash empty \
                 so the webhook retry re-issues them",
                user_id
            );
        }

        sent
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
