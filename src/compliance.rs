//! Email compliance — one gate in front of every message this app sends, and one way out for the person
//! who receives it.
//!
//! The rules are the ones CoreSwift and IncentiveSwift already enforce, because a customer's inbox does
//! not care which of our products mailed them:
//!
//!  * `transactional` (receipts, password resets, security notices) is exempt from opt-out by law and
//!    must keep working for somebody who unsubscribed from everything. NOT exempt from harm control: an
//!    address that hard-bounced or complained is never mailed again, in any class.
//!  * `notification` and `bulk` are stoppable. `workflow_notify` — the mail a workflow sends to whoever
//!    it names — is the class that matters here: before this module the only way out was to reply and
//!    hope, and the mail carried no link, no header and no list.
//!
//! SCOPE, and why it is fixed rather than per workspace: this app mails as ITSELF — one brand, one
//! sending identity (`no-reply@mail.workflowswift.com`) — so the do-not-mail list is the app's, not one
//! workspace's. The tables carry a tenant column so the day an account sends under its own identity the
//! same code keys per account with no migration; until then every row is written against
//! [`PLATFORM_TENANT`].
//!
//! The token in the link is an HMAC over `address|class` keyed off this app's own `jwt_secret` with a
//! domain separator, so it cannot be forged, cannot be replayed into a session, and needs no row: a link
//! in a year-old message still works.

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    Extension, Json,
};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::handlers::admin_settings_handler::require_admin;
use crate::state::AppState;

pub const BULK: &str = "bulk";
pub const NOTIFICATION: &str = "notification";
pub const TRANSACTIONAL: &str = "transactional";

/// The scope this app's own mail belongs to. See the module note: one brand, one sending identity.
pub const PLATFORM_TENANT: Uuid = Uuid::from_u128(1);

/// The domain separator that keeps an opt-out token from ever being a session token.
const TOKEN_DOMAIN: &str = "workflowswift.unsubscribe.v1";

/// How a template type maps to a compliance class.
///
/// Written as an ALLOW-LIST of stoppable types rather than "everything not transactional": a template
/// type added later must be considered on purpose, and an unlisted one is treated as transactional so a
/// new receipt can never be silenced by accident. The suffix arms cover the marketing spellings so a
/// campaign template does not need a release to be stoppable.
pub fn class_for(template_type: &str) -> &'static str {
    match template_type {
        // The mail a workflow sends to whoever it names — the one class here that reaches someone who
        // never signed up and has no console to visit. Everything else this app sends is an account
        // notice (welcome, reset, receipt, plan move, a run that failed on the owner's own account),
        // and silencing those would hide exactly what the customer asked to be told.
        "workflow_notify" => NOTIFICATION,
        t if t.ends_with("_bulk") || t.ends_with("_marketing") || t.ends_with("_campaign") => BULK,
        _ => TRANSACTIONAL,
    }
}

pub fn normalise(address: &str) -> String {
    address.trim().to_lowercase()
}

fn key(secret: &str) -> Vec<u8> {
    format!("{secret}:{TOKEN_DOMAIN}").into_bytes()
}

fn sign(secret: &str, payload: &str) -> String {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(&key(secret)).expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// A tamper-proof opt-out token: the address and the class it covers. No expiry (a link in a year-old
/// message must still work) and no stored state.
pub fn token_for(secret: &str, address: &str, mail_class: &str) -> String {
    let addr = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(normalise(address));
    let payload = format!("{addr}.{mail_class}");
    let sig = sign(secret, &payload);
    format!("{payload}.{sig}")
}

pub fn verify_token(secret: &str, token: &str) -> Option<(String, String)> {
    let (payload, sig) = token.rsplit_once('.')?;
    if !constant_time_eq(sign(secret, payload).as_bytes(), sig.as_bytes()) {
        return None;
    }
    let (addr, mail_class) = payload.split_once('.')?;
    let address = String::from_utf8(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(addr)
            .ok()?,
    )
    .ok()?;
    Some((address, mail_class.to_string()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The public page a mail links to. Absolute, on the host the customer's own browser will load.
pub fn unsubscribe_url(secret: &str, address: &str, mail_class: &str) -> String {
    format!(
        "{}/api/v1/public/unsubscribe/{}",
        crate::branding::APP_URL,
        token_for(secret, address, mail_class)
    )
}

/// Which stored classes block a message about to be sent in `mail_class`.
fn blocking_classes(mail_class: &str) -> &'static [&'static str] {
    match mail_class {
        TRANSACTIONAL => &["transactional"],
        NOTIFICATION => &["all", "notification"],
        _ => &["all", "bulk"],
    }
}

/// Somebody asked this app to mail this address less. Returns the reason when that covers the class we
/// were about to send.
///
/// A database that cannot answer must never read as "nobody opted out": the caller gets `Err` and the
/// gate then REFUSES the two stoppable classes (fail-closed) while transactional mail still goes out.
pub async fn suppression(
    db: &PgPool,
    address: &str,
    mail_class: &str,
) -> Result<Option<String>, sqlx::Error> {
    let classes: Vec<String> = blocking_classes(mail_class)
        .iter()
        .map(|c| c.to_string())
        .collect();
    sqlx::query_scalar::<_, String>(
        "SELECT reason FROM email_suppressions
          WHERE tenant_id = $1 AND address = $2 AND mail_class = ANY($3)
          ORDER BY created_at LIMIT 1",
    )
    .bind(PLATFORM_TENANT)
    .bind(normalise(address))
    .bind(&classes)
    .fetch_optional(db)
    .await
}

/// The gate's verdict for one message about to be sent.
pub enum Verdict {
    /// Nothing recorded for this address in a blocking class.
    Send,
    /// The address asked not to be mailed (or the list could not be read). Carries the reason.
    Refuse(String),
}

pub async fn gate(db: &PgPool, address: &str, mail_class: &str) -> Verdict {
    match suppression(db, address, mail_class).await {
        Ok(None) => Verdict::Send,
        Ok(Some(reason)) => Verdict::Refuse(reason),
        Err(e) => {
            tracing::error!("suppression read failed for {address}: {e}");
            if mail_class == TRANSACTIONAL {
                Verdict::Send
            } else {
                Verdict::Refuse("suppression list unavailable".to_string())
            }
        }
    }
}

pub async fn suppress(
    db: &PgPool,
    address: &str,
    reason: &str,
    mail_class: &str,
    source: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO email_suppressions (tenant_id, address, mail_class, reason, source)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (tenant_id, address, mail_class)
         DO UPDATE SET reason = EXCLUDED.reason, source = EXCLUDED.source, updated_at = now()",
    )
    .bind(PLATFORM_TENANT)
    .bind(normalise(address))
    .bind(mail_class)
    .bind(reason)
    .bind(source)
    .execute(db)
    .await?;

    if mail_class != TRANSACTIONAL {
        let _ = sqlx::query(
            "INSERT INTO email_consent (tenant_id, address, mail_class, state, how)
             VALUES ($1, $2, $3, 'revoked', $4)",
        )
        .bind(PLATFORM_TENANT)
        .bind(normalise(address))
        .bind(if mail_class == "all" {
            BULK
        } else {
            mail_class
        })
        .bind(source)
        .execute(db)
        .await;
    }
    Ok(())
}

/// One row per refusal, so "the gate did its job" is a record and not a claim.
pub async fn log_refused(
    db: &PgPool,
    mail_class: &str,
    address: &str,
    subject: Option<&str>,
    detail: &str,
) {
    let _ = sqlx::query(
        "INSERT INTO email_message_log
            (tenant_id, mail_class, address, subject, provider, status, detail)
         VALUES ($1, $2, $3, $4, NULL, 'refused_suppressed', $5)",
    )
    .bind(PLATFORM_TENANT)
    .bind(if mail_class == "all" {
        BULK
    } else {
        mail_class
    })
    .bind(normalise(address))
    .bind(subject)
    .bind(detail)
    .execute(db)
    .await;
}

/// The footer appended to a stoppable message: who is writing, and the one link that stops it.
pub fn footer(secret: &str, address: &str, mail_class: &str, sender: &str) -> (String, String) {
    let url = unsubscribe_url(secret, address, mail_class);
    let text = format!("\n\n--\n{sender}\nStop these emails: {url}\n");
    let html = format!(
        "<hr style=\"border:0;border-top:1px solid #e5e7eb;margin:28px 0 12px\">\
         <p style=\"font:13px/1.5 system-ui,sans-serif;color:#6b7280;margin:0\">{sender}</p>\
         <p style=\"font:13px/1.5 system-ui,sans-serif;color:#6b7280;margin:6px 0 0\">\
         <a href=\"{url}\" style=\"color:#4f46e5\">Stop these emails</a></p>"
    );
    (text, html)
}

// ── public surface: the opt-out itself ────────────────────────────────────────────────────────────

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn page_shell(title: &str, inner: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\"><title>{}</title></head>\
         <body style=\"margin:0;background:#fff\"><div style=\"font:16px/1.6 system-ui,sans-serif;\
         max-width:560px;margin:10vh auto;padding:0 20px;color:#111\">{}</div></body></html>",
        html_escape(title),
        inner
    )
}

fn invalid_link() -> Response {
    (
        StatusCode::GONE,
        Html(page_shell(
            "Link not valid",
            "<h1 style=\"font-size:1.2rem\">This link is not valid</h1>\
             <p>Reply to any message from this sender and ask them to stop writing to you.</p>",
        )),
    )
        .into_response()
}

pub(crate) async fn show_unsubscribe(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Response {
    let Some((address, class)) = verify_token(&state.config.jwt_secret, &token) else {
        return invalid_link();
    };
    let addr = normalise(&address);
    let body = format!(
        "<h1 style=\"font-size:1.25rem;margin:0 0 8px\">Mail from WorkflowSwift is switched off for \
         {addr}</h1>\
         <p style=\"color:#555;margin:0 0 20px\">Press the button and this address will not be written to \
         again. Messages you need — receipts, password resets, security notices — keep working.</p>\
         <form method=post action=\"/api/v1/public/unsubscribe/{token}\">\
         <button style=\"font:inherit;padding:12px 22px;border:0;border-radius:8px;background:#4f46e5;\
         color:#fff;cursor:pointer\">Stop these emails</button></form>",
        addr = html_escape(&addr),
        token = token
    );
    let _ = class;
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Html(page_shell("Unsubscribe", &body)),
    )
        .into_response()
}

async fn do_unsubscribe(state: &AppState, token: &str) -> Result<String, Response> {
    let Some((address, class)) = verify_token(&state.config.jwt_secret, token) else {
        return Err(invalid_link());
    };
    let stored_class = if class == TRANSACTIONAL {
        "all"
    } else {
        class.as_str()
    };
    match suppress(
        &state.db,
        &address,
        "unsubscribed",
        stored_class,
        "one_click",
    )
    .await
    {
        Ok(()) => Ok(normalise(&address)),
        Err(e) => {
            tracing::error!("unsubscribe failed for {address}: {e}");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "could not record the opt-out" })),
            )
                .into_response())
        }
    }
}

pub(crate) async fn one_click(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Response {
    match do_unsubscribe(&state, &token).await {
        Ok(address) => (
            StatusCode::OK,
            Html(page_shell(
                "Unsubscribed",
                &format!(
                    "<h1 style=\"font-size:1.25rem\">Done — {}</h1>\
                     <p>WorkflowSwift will not write to this address again.</p>",
                    html_escape(&address)
                ),
            )),
        )
            .into_response(),
        Err(resp) => resp,
    }
}

/// RFC 8058 one-click clients POST `List-Unsubscribe=One-Click` to the URL; some post JSON. Accept both
/// by taking the token from the query string when there is no path segment.
pub(crate) async fn one_click_body(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    match q.get("token") {
        Some(t) => match do_unsubscribe(&state, t).await {
            Ok(address) => {
                Json(json!({ "unsubscribed": true, "address": address })).into_response()
            }
            Err(resp) => resp,
        },
        None => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no token" })),
        )
            .into_response(),
    }
}

// ── operator surface ──────────────────────────────────────────────────────────────────────────────

fn unavailable() -> Response {
    // A FAILED read must never paint as "nobody has opted out": that sentence is a statement about the
    // customer's list, and the screen has an error arm of its own.
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "the do-not-mail list could not be read", "status": 500 })),
    )
        .into_response()
}

pub(crate) async fn list_suppressions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Response {
    if let Err(e) = require_admin(&claims) {
        return e.into_response();
    }
    let rows = match sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT address, mail_class, reason, source, created_at FROM email_suppressions
          WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT 1000",
    )
    .bind(PLATFORM_TENANT)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("do-not-mail list read failed: {e}");
            return unavailable();
        }
    };
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(address, mail_class, reason, source, created_at)| {
            json!({ "address": address, "mail_class": mail_class, "reason": reason,
                    "source": source, "created_at": created_at })
        })
        .collect();
    Json(json!({
        "suppressions": items,
        "count": items.len(),
        "message": if items.is_empty() {
            "Nobody has asked this app to stop mailing them."
        } else {
            "These addresses are never written to again (receipts and password resets excepted)."
        }
    }))
    .into_response()
}

pub(crate) async fn add_suppression(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(e) = require_admin(&claims) {
        return e.into_response();
    }
    let Some(address) = body.get("address").and_then(|v| v.as_str()) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "address is required" })),
        )
            .into_response();
    };
    let address = normalise(address);
    if !address.contains('@') || address.len() < 5 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "that is not an email address" })),
        )
            .into_response();
    }
    match suppress(&state.db, &address, "manual", "all", "operator").await {
        Ok(()) => Json(json!({ "suppressed": true, "address": address })).into_response(),
        Err(e) => {
            tracing::error!("manual suppression failed for {address}: {e}");
            unavailable()
        }
    }
}

pub(crate) async fn remove_suppression(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(address): Path<String>,
) -> Response {
    if let Err(e) = require_admin(&claims) {
        return e.into_response();
    }
    let addr = normalise(&address);
    // Every class for this address goes: an operator lifting a block means the address is mail-able
    // again, and leaving a 'bulk' row behind would keep refusing the very mail they meant to restore.
    match sqlx::query("DELETE FROM email_suppressions WHERE tenant_id = $1 AND address = $2")
        .bind(PLATFORM_TENANT)
        .bind(&addr)
        .execute(&state.db)
        .await
    {
        Ok(r) => Json(json!({ "removed": r.rows_affected(), "address": addr })).into_response(),
        Err(e) => {
            tracing::error!("suppression removal failed for {addr}: {e}");
            unavailable()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_map_treats_receipts_as_transactional() {
        assert_eq!(class_for("workflow_notify"), NOTIFICATION);
        assert_eq!(class_for("welcome"), TRANSACTIONAL);
        assert_eq!(class_for("password_reset"), TRANSACTIONAL);
        assert_eq!(class_for("spring_campaign"), BULK);
        assert_eq!(class_for("unknown_future_mail"), TRANSACTIONAL);
    }

    #[test]
    fn token_round_trips_and_refuses_a_forgery() {
        let t = token_for("secret-a", "Someone@Example.com", "all");
        assert_eq!(
            verify_token("secret-a", &t),
            Some(("someone@example.com".to_string(), "all".to_string()))
        );
        assert_eq!(verify_token("secret-b", &t), None);
        let forged = format!("{}x", &t[..t.len() - 1]);
        assert_eq!(verify_token("secret-a", &forged), None);
    }

    #[test]
    fn blocking_classes_exempt_transactional_from_a_full_opt_out() {
        assert_eq!(blocking_classes(TRANSACTIONAL), &["transactional"]);
        assert!(blocking_classes(NOTIFICATION).contains(&"all"));
        assert!(!blocking_classes(NOTIFICATION).contains(&"bulk"));
    }

    #[test]
    fn footer_carries_a_working_url() {
        let (text, html) = footer("s", "a@b.com", NOTIFICATION, "WorkflowSwift");
        assert!(text.contains("/api/v1/public/unsubscribe/"));
        assert!(text.contains("Stop these emails"));
        assert!(html.contains("Stop these emails"));
    }
}
