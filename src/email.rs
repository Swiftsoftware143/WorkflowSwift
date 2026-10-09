//! Email module — sends transactional emails (welcome, team_invite, password_reset).
//!
//! Templates are stored in the `email_templates` table and can be configured
//! via the admin panel. HTML + text versions with toggle support.
//!
//! SMTP/API config comes from `admin_settings` (key: "email") — DB ONLY.
//! There is no env-var credential fallback: an unconfigured provider logs and
//! skips the send instead of silently using a server-wide env var.

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::state::AppState;

use crate::branding::{self, Branding};

/// Prepend an account's branding header to a rendered `(text, html)` pair (kanban t_c3cfe7ba).
///
/// Additive by construction: `branding` is `None` for an account with nothing configured, and the
/// pair is returned untouched — so every unbranded account's mail is byte-identical to before this
/// module existed. A text part cannot carry an image, so it opens with the brand NAME; the HTML part
/// opens with the header block (the logo, or the name when there is no logo). When there is no HTML
/// part at all, one is built from the ESCAPED text so the header has somewhere to live.
fn apply_branding(branding: Option<&Branding>, text: String, html: String) -> (String, String) {
    let Some(b) = branding else {
        return (text, html);
    };
    let text_out = format!("{}{}", b.text_header(), text);
    let header = b.header_html(b.resolve_logo_url(branding::APP_URL).as_deref());
    let html_out = if !html.is_empty() {
        format!("{header}{html}")
    } else {
        format!(
            "{header}<div style=\"white-space:pre-wrap;font-family:-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#111827\">{}</div>",
            branding::escape_html(&text)
        )
    };
    (text_out, html_out)
}

/// The transport call every send funnels through, with branding applied.
async fn send_branded(
    cfg: &EmailConfig,
    to: &str,
    subject: &str,
    text_body: &str,
    html_body: &str,
    branding: Option<&Branding>,
) -> Result<(), String> {
    let (t, h) = apply_branding(branding, text_body.to_string(), html_body.to_string());
    send_email_request(cfg, to, subject, &t, &h).await
}

/// Default From ADDRESS — used only when the admin has not set one in
/// Admin > Settings > Email. Not a credential, so it is safe as a constant.
///
/// It must live on the domain the provider actually sends through
/// (`mail.workflowswift.com`, see `api_url`): Mailgun signs with that domain's DKIM, so a From on
/// any other domain fails DMARC alignment at a strict receiver. Measured on the live domain — a
/// message From `swiftsoftware143@yahoo.com` sent through `mail.workflowswift.com` was accepted
/// and then failed with remote code `554 reason=espblock` (yahoo.com publishes `p=reject`), while
/// the same message from this domain was delivered (`250 ok dirdel`).
///
/// The address is **hyphenated** (`no-reply@`): that is the fleet's sender-identity convention
/// (references/mail-identity-conventions-2026-10-08.md, kanban t_68d95177). It used to be the
/// unhyphenated `noreply@`, which only ever reached the wire on a config with an empty
/// `from_address` (the `smtp` / `sendgrid` arms are "configured" without one).
const DEFAULT_EMAIL_FROM_ADDRESS: &str = "no-reply@mail.workflowswift.com";

/// Default From DISPLAY NAME — the other half of the same fallback identity, matching the
/// `from_name` half of the `admin_settings.email` row. System mail is never a bare address: an
/// install with no `from_name` sends as `"WorkflowSwift Help Desk" <no-reply@mail.workflowswift.com>`
/// (owner's rule, 2026-10-08). Never a shared literal across apps — each app names its own product.
const DEFAULT_EMAIL_FROM_NAME: &str = "WorkflowSwift Help Desk";

/// The organisation-level domain of an address or From header:
/// `WorkflowSwift Help Desk <no-reply@mail.workflowswift.com>` -> `workflowswift.com`.
/// Used only to warn about DMARC-alignment-breaking From addresses.
fn org_domain(value: &str) -> String {
    let host = match value.rfind('@') {
        Some(at) => &value[at + 1..],
        None => value,
    };
    let host = host
        .split('/')
        .next()
        .unwrap_or(host)
        .trim_matches(|c: char| c == '>' || c == ' ' || c == '.')
        .to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() <= 2 {
        labels.join(".")
    } else {
        labels[labels.len() - 2..].join(".")
    }
}

/// Does this Mailgun endpoint send through `domain`? A Mailgun API URL embeds the sending domain
/// in its path (`https://api.mailgun.net/v3/<domain>/messages`), which is the domain Mailgun signs
/// DKIM with — so this is the check that decides whether a From is DMARC-aligned.
fn mailgun_url_sends_as(api_url: &str, domain: &str) -> bool {
    !domain.is_empty() && api_url.to_ascii_lowercase().contains(domain)
}

/// Placeholder-shaped tokens that survived rendering — i.e. names no caller bound.
///
/// This module substitutes ONE vocabulary: `{{key}}` (double braces). A stored row written in the
/// other dialect the fleet has shipped (`Hi {name}!`, single braces) is NOT substituted, so before
/// this it reached the recipient as literal braces with **no log line at all** — the blind spot
/// kanban t_c8df11e6 closed in IncentiveSwift's `template_render` (kanban t_f70ef1bb).
///
/// `{name}` and a surviving `{{name}}` are both reported as `name`, deduplicated, first-seen
/// order. Nothing is evaluated or stripped: the copy is the admin's, only the REPORT is ours.
fn unsubstituted(rendered: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = rendered;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { break };
        let name = after[..close].trim_matches('{').trim_matches('}').trim();
        if !name.is_empty()
            && name.len() < 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            && !out.iter().any(|n| n.as_str() == name)
        {
            out.push(name.to_string());
        }
        rest = &after[close + 1..];
    }
    out
}

/// Every distinct mustache BLOCK MARKER still present in `rendered` — `{{#if x}}`, `{{/if}}`,
/// `{{else}}`, `{{^x}}`, `{{!c}}`, `{{>p}}` — named IN FULL, deduplicated, first-seen order.
///
/// A marker holds no identifier, so `unsubstituted` cannot see it; without this arm a stored row
/// that drifted into the handlebars vocabulary (`templates/*.html` really uses it; these email
/// renderers do not) would mail raw markers and log nothing (kanban t_c8df11e6 / t_f70ef1bb).
fn unprocessed_scaffolding(rendered: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(rel) = rendered[i..].find("{{") {
        let start = i + rel;
        let Some(endrel) = rendered[start + 2..].find("}}") else {
            break;
        };
        let end = start + 2 + endrel + 2;
        let inner = &rendered[start + 2..end - 2];
        let is_marker = inner == "else"
            || matches!(
                inner.chars().next(),
                Some('#') | Some('/') | Some('^') | Some('!') | Some('>')
            );
        if is_marker {
            let token = rendered[start..end].to_string();
            if !out.contains(&token) {
                out.push(token);
            }
        }
        i = end;
    }
    out
}

/// Report — at `warn!`, naming every token — whatever a render left unprocessed, then return the
/// placeholder NAMES so a caller (or a probe) can assert on them.
///
/// This is the loud half of the fix: a token neither substituted nor reported is a defect the
/// recipient receives as literal braces, indistinguishable from deliberate copy. The context names
/// which part of the mail (subject / html_body / text_body) carried it.
fn warn_unsubstituted(rendered: &str, context: &str) -> Vec<String> {
    let left = unsubstituted(rendered);
    let scaffolding = unprocessed_scaffolding(rendered);
    if !left.is_empty() || !scaffolding.is_empty() {
        tracing::warn!(
            context = %context,
            placeholders = ?left,
            scaffolding = ?scaffolding,
            "email template markup was NOT processed - the recipient receives it verbatim; use the \
             double-brace vocabulary ({{key}}) and no mustache conditionals (this renderer \
             implements none)"
        );
    }
    left
}

/// Render a template string by replacing {{key}} placeholders with values from `vars`.
/// Anything left over is reported by [`warn_unsubstituted`] instead of being mailed silently.
fn render_template(template: &str, vars: &serde_json::Value, context: &str) -> String {
    let mut result = template.to_string();

    // Replace {{key}} with JSON string values
    if let Some(obj) = vars.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{{{}}}}}", key);
            let replacement = value.as_str().unwrap_or("");
            result = result.replace(&placeholder, replacement);
        }
    }

    warn_unsubstituted(&result, context);

    result
}

/// Send a templated email using database-stored templates.
/// Falls back to hardcoded inline templates if DB lookup fails.
/// This is the preferred method — pass `AppState` to get access to DB and config.
async fn send_email_inner(
    state: &AppState,
    aid: Option<Uuid>,
    to: &str,
    template_type: &str,
    vars: &serde_json::Value,
) -> Result<(), String> {
    // Get email config from DB admin_settings (DB only — no env fallback)
    let cfg = match get_email_config(state).await {
        Some(c) => c,
        None => {
            eprintln!(
                "[email] skipping '{}' to {} — email provider not configured \
                 (Admin > Settings > Email)",
                template_type, to
            );
            return Err(
                "Email not configured: set the provider in Admin > Settings > Email".to_string(),
            );
        }
    };

    // Try to load template from DB
    let template = sqlx::query_as::<_, EmailTemplateRow>(
        r#"SELECT id, name, subject, body, html_body, is_html, is_default
           FROM email_templates
           WHERE template_type = $1 AND (is_default = true OR is_default IS NULL)
           ORDER BY is_default DESC, created_at DESC
           LIMIT 1"#,
    )
    .bind(template_type)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    // Load the account's branding ONCE (kanban t_c3cfe7ba). `None` for no account / nothing set,
    // which is what keeps every unbranded account's mail byte-identical to before this feature.
    let branding = match aid {
        Some(a) => crate::branding::load(&state.db, a).await,
        None => None,
    };

    match template {
        Some(t) => {
            // Use DB template
            let subject = render_template(
                &t.subject.unwrap_or_else(|| match template_type {
                    "welcome" => "Welcome to WorkflowSwift!".into(),
                    "team_invite" => "Team Invitation".into(),
                    "password_reset" => "Password Reset".into(),
                    _ => "WorkflowSwift Notification".into(),
                }),
                vars,
                "subject",
            );

            let html_body = t
                .html_body
                .as_ref()
                .map(|h| render_template(h, vars, "html_body"))
                .unwrap_or_default();

            let text_body = render_template(&t.body.unwrap_or_default(), vars, "text_body");

            let use_html = t.is_html.unwrap_or(true);

            if use_html && !html_body.is_empty() {
                send_branded(
                    &cfg,
                    to,
                    &subject,
                    &text_body,
                    &html_body,
                    branding.as_ref(),
                )
                .await
            } else {
                send_branded(&cfg, to, &subject, &text_body, "", branding.as_ref()).await
            }
        }
        None => {
            // Fallback to hardcoded template
            send_email_fallback(&cfg, to, template_type, vars, branding.as_ref()).await
        }
    }
}

/// Send a templated email, recording the outcome in the transport's own
/// `admin_settings` row.
///
/// Why the wrapper exists: a broken transport used to be **log-only**. Every caller
/// (`register`, `forgot-password`, `invite_user`, `deliver_credentials`) logged the
/// error and returned success, so a locked-out user was told to check an inbox that
/// would never receive anything, and a buyer whose password exists *only* in the
/// credential email could pay and get nothing while the API said "completed".
/// `record_send_outcome` puts the failure where an admin actually looks.
pub async fn send_email(
    state: &AppState,
    aid: Option<Uuid>,
    to: &str,
    template_type: &str,
    vars: &serde_json::Value,
) -> Result<(), String> {
    // Fleet harness addresses never reach a real relay (parity with FunnelSwift, kanban t_36b55ed2).
    // A probe that signs up with a fleet-dev domain (`swiftsoftware.dev/.net`) is created normally but
    // its mail is withheld: the address is routable, so a send can only land in a fleet mailbox or
    // bounce (measured 2026-10-09 on mail.workflowswift.com: `accepted` then `bounced` 552), and every
    // such send burns a delivery on the domain's sending reputation. The RFC-2606 class
    // (.local/.test/.invalid/example.*) is deliberately NOT suppressed — content harnesses point the
    // provider at a local sink and read the message off the wire, so silencing it would delete proof.
    if let Some(domain) = crate::security::probe_addr::harness_domain(to) {
        tracing::info!(
            to = %to,
            domain = %domain,
            template = %template_type,
            "send suppressed: recipient is a fleet harness address (fleet-dev domain)"
        );
        return Ok(());
    }
    let result = send_email_inner(state, aid, to, template_type, vars).await;
    record_send_outcome(state, template_type, &result).await;
    result
}

/// Persist the result of the latest send attempt into the `email` config row
/// (`last_send_ok` / `last_send_error` / `last_send_at` / `last_send_template`).
///
/// Storing it beside the provider config is deliberate: the admin SPA already GETs
/// `/admin/settings/email` to render the Email Provider panel, so the failure
/// surfaces without a new endpoint or a new table. The recipient is NOT stored.
///
/// Best-effort by design — a bookkeeping failure must never fail a send that
/// actually succeeded, so every error path here is swallowed.
async fn record_send_outcome(state: &AppState, template_type: &str, result: &Result<(), String>) {
    let row = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT value FROM admin_settings WHERE key = 'email'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let Some(mut cfg) = row else { return };
    let Some(obj) = cfg.as_object_mut() else {
        return;
    };

    match result {
        Ok(()) => {
            obj.insert("last_send_ok".to_string(), json!(true));
            obj.insert("last_send_error".to_string(), json!(null));
        }
        Err(e) => {
            obj.insert("last_send_ok".to_string(), json!(false));
            obj.insert("last_send_error".to_string(), json!(e.clone()));
        }
    }
    obj.insert(
        "last_send_at".to_string(),
        json!(chrono::Utc::now().to_rfc3339()),
    );
    obj.insert(
        "last_send_template".to_string(),
        json!(template_type.to_string()),
    );

    // This is a THIRD writer of the `email` row (kanban t_a794cb09): it reads the whole value,
    // adds the last-send fields and writes it back, so it must keep the credential sealed —
    // otherwise a send would re-publish a credential that arrived plaintext.
    if let Err(e) = seal_config_secrets(&state.db, &mut cfg).await {
        eprintln!(
            "[email] could not seal admin_settings.email before recording the send outcome: {e}"
        );
    }

    let _ =
        sqlx::query("UPDATE admin_settings SET value = $1, updated_at = NOW() WHERE key = 'email'")
            .bind(&cfg)
            .execute(&state.db)
            .await;
}

/// Email provider configuration, read from the `admin_settings` row keyed
/// `email` (Admin > Settings > Email). The DB is the **only** source of
/// credentials — no env-var fallback anywhere, so the admin UI is authoritative.
#[derive(Debug, Clone)]
struct EmailConfig {
    /// `smtp` | `mailgun` | `sendgrid` | `sendiio`
    provider: String,
    /// API endpoint. mailgun: `https://api.mailgun.net/v3/<domain>/messages`,
    /// sendgrid: blank → `https://api.sendgrid.com/v3/mail/send`, sendiio: the
    /// account's send endpoint.
    api_url: String,
    api_key: String,
    from_address: String,
    from_name: String,
    smtp_host: String,
    smtp_port: u16,
    smtp_username: String,
    smtp_password: String,
    /// `none` | `tls` (STARTTLS) | `ssl` (implicit TLS)
    smtp_encryption: String,
}

impl EmailConfig {
    /// Are the fields the selected provider needs present?
    pub fn is_configured(&self) -> bool {
        match self.provider.as_str() {
            "smtp" | "mail" => !self.smtp_host.trim().is_empty(),
            "sendgrid" => !self.api_key.trim().is_empty(),
            _ => !self.api_url.trim().is_empty() && !self.api_key.trim().is_empty(),
        }
    }
}

/// Is there a mail sender on this install? The SAME predicate the Notify gate gates on
/// (kanban t_d3ff37ef) — an `email` Notify step is only storable when this is true, so the
/// console can never offer a mail channel that would deliver nothing.
pub async fn is_configured(state: &AppState) -> bool {
    get_email_config(state)
        .await
        .map(|c| c.is_configured())
        .unwrap_or(false)
}

/// The credential fields carried inside the `admin_settings.email` object. They are sealed with
/// the SAME `enc:v1:` envelope this app already uses for `provider_keys`/`integration_targets` —
/// this config was the one credential path that stored its value in the clear (kanban
/// t_a794cb09), so a dump or a backup yielded a usable Mailgun private key for every fleet domain.
pub const CONFIG_SECRET_FIELDS: [&str; 2] = ["api_key", "smtp_password"];

/// Seal the credential fields of the email-config object IN PLACE, before it is stored.
///
/// * empty stays empty — a blank field is "no credential", never a ciphertext of nothing;
/// * an already-sealed value is left exactly as it is: that is what the panel's masked
///   round-trip carries back, and re-encrypting it would destroy the stored credential;
/// * a missing master key makes this FAIL — a plaintext credential is never a fallback.
pub async fn seal_config_secrets(
    pool: &PgPool,
    cfg: &mut serde_json::Value,
) -> Result<(), crate::security::provider_key_crypto::CryptoError> {
    let Some(obj) = cfg.as_object_mut() else {
        return Ok(());
    };
    for field in CONFIG_SECRET_FIELDS {
        let current = obj
            .get(field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if current.is_empty() || crate::security::provider_key_crypto::is_encrypted(&current) {
            continue;
        }
        let sealed =
            crate::security::provider_key_crypto::encrypt_for_storage(pool, &current).await?;
        obj.insert(field.to_string(), serde_json::Value::String(sealed));
    }
    Ok(())
}

/// Open the credential fields of the email-config object IN PLACE after a DB read, so what
/// reaches a provider (or the admin panel) is the credential and never the envelope. A value
/// without the envelope is a legacy plaintext row and is passed through unchanged.
pub async fn open_config_secrets(
    pool: &PgPool,
    cfg: &mut serde_json::Value,
) -> Result<(), crate::security::provider_key_crypto::CryptoError> {
    let Some(obj) = cfg.as_object_mut() else {
        return Ok(());
    };
    for field in CONFIG_SECRET_FIELDS {
        let current = obj
            .get(field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if current.is_empty() || !crate::security::provider_key_crypto::is_encrypted(&current) {
            continue;
        }
        let opened =
            crate::security::provider_key_crypto::decrypt_from_storage(pool, &current).await?;
        obj.insert(field.to_string(), serde_json::Value::String(opened));
    }
    Ok(())
}

/// Seal every credential still sitting in the clear in the `admin_settings.email` row.
///
/// Three writers touch this row (the admin settings route, the generic
/// `PUT /api/v1/admin/settings/{key}` route and `record_send_outcome`); all of them seal, and
/// this is what converges a row that arrives plaintext from a database restored out of a dump
/// taken before the change. Idempotent; returns the number of rows it had to rewrite.
pub async fn seal_legacy_config_secrets(
    pool: &PgPool,
) -> Result<u64, crate::security::provider_key_crypto::CryptoError> {
    let mut value: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM admin_settings WHERE key = 'email'")
            .fetch_optional(pool)
            .await?;
    let Some(mut value) = value.take() else {
        return Ok(0);
    };
    if !value.is_object() {
        return Ok(0);
    }
    let before = value.clone();
    seal_config_secrets(pool, &mut value).await?;
    if value == before {
        return Ok(0);
    }
    sqlx::query("UPDATE admin_settings SET value = $1, updated_at = NOW() WHERE key = 'email'")
        .bind(&value)
        .execute(pool)
        .await?;
    Ok(1)
}

fn cfg_str(cfg: &serde_json::Value, key: &str) -> String {
    cfg.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Read the email provider config from `admin_settings`.
///
/// Returns `None` (after logging) when the admin has not configured a provider —
/// the caller then refuses to send. There is deliberately **no** env-var
/// fallback: a server-wide EMAIL_API_URL/EMAIL_API_KEY would silently shadow this.
async fn get_email_config(state: &AppState) -> Option<EmailConfig> {
    let cfg = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT value FROM admin_settings WHERE key = 'email'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let cfg = match cfg {
        Some(c) => c,
        None => {
            eprintln!(
                "[email] not configured: admin_settings key 'email' is missing \
                 (Admin > Settings > Email) — skipping send"
            );
            return None;
        }
    };

    // The credential is ciphertext at rest (kanban t_a794cb09): open it before the provider sees
    // it — an `enc:v1:` envelope sent as a Basic-auth password is a guaranteed 401, not a send.
    let mut cfg = cfg;
    if let Err(e) = open_config_secrets(&state.db, &mut cfg).await {
        eprintln!(
            "[email] admin_settings.email credential cannot be opened ({e}) — \
             skipping send (PROVIDER_KEY_ENC_SECRET mismatch?)"
        );
        return None;
    }

    let provider = {
        let p = cfg_str(&cfg, "provider").to_ascii_lowercase();
        if p.is_empty() {
            // Row predates the provider field: the Mailgun-compatible API was the
            // only transport, so keep it as the stored default.
            "mailgun".to_string()
        } else {
            p
        }
    };

    let config = EmailConfig {
        provider,
        api_url: cfg_str(&cfg, "api_url"),
        api_key: cfg_str(&cfg, "api_key"),
        from_address: cfg_str(&cfg, "from_address"),
        from_name: cfg_str(&cfg, "from_name"),
        smtp_host: cfg_str(&cfg, "smtp_host"),
        smtp_port: cfg.get("smtp_port").and_then(|v| v.as_u64()).unwrap_or(587) as u16,
        smtp_username: cfg_str(&cfg, "smtp_username"),
        smtp_password: cfg_str(&cfg, "smtp_password"),
        smtp_encryption: {
            let e = cfg_str(&cfg, "smtp_encryption").to_ascii_lowercase();
            if e.is_empty() {
                "tls".to_string()
            } else {
                e
            }
        },
    };

    if !config.is_configured() {
        eprintln!(
            "[email] not configured: provider '{}' is missing its credentials \
             (Admin > Settings > Email) — skipping send",
            config.provider
        );
        return None;
    }

    Some(config)
}

/// The `(address, display name)` pair this transport sends as.
///
/// Each half falls back INDEPENDENTLY to the app's own default when the admin left it blank,
/// because the two halves are two row fields (`from_address` / `from_name`): an install that set
/// only one of them keeps the half it set and gets the app identity for the other. An install
/// with both blank therefore sends the full default identity rather than a bare address.
fn from_identity(cfg: &EmailConfig) -> (String, String) {
    let addr = if cfg.from_address.trim().is_empty() {
        DEFAULT_EMAIL_FROM_ADDRESS.to_string()
    } else {
        cfg.from_address.clone()
    };
    let name = if cfg.from_name.trim().is_empty() {
        DEFAULT_EMAIL_FROM_NAME.to_string()
    } else {
        cfg.from_name.clone()
    };
    (addr, name)
}

/// From header — always `Name <addr>`: both halves fall back to the app identity, so the header is
/// a parseable mailbox and never ships a bare default address.
fn from_header(cfg: &EmailConfig) -> String {
    let (addr, name) = from_identity(cfg);
    format!("{} <{}>", name, addr)
}

/// Fallback hardcoded templates (used when DB template not found)
async fn send_email_fallback(
    cfg: &EmailConfig,
    to: &str,
    template_type: &str,
    vars: &serde_json::Value,
    branding: Option<&Branding>,
) -> Result<(), String> {
    let app_url = vars
        .get("app_url")
        .and_then(|v| v.as_str())
        .unwrap_or("https://app.workflowswift.com");

    match template_type {
        "welcome" | "team_invite" => {
            let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
            let email = vars.get("email").and_then(|v| v.as_str()).unwrap_or("");
            let password = vars.get("password").and_then(|v| v.as_str()).unwrap_or("");

            let account_name = if template_type == "team_invite" {
                vars.get("account_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Your Team")
            } else {
                ""
            };

            let subject = if template_type == "welcome" {
                "Welcome to WorkflowSwift!".to_string()
            } else {
                format!("You've been invited to {}", account_name)
            };

            let html_body = if template_type == "welcome" {
                format!(
                    r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"></head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333; margin:0;padding:0;background:#f4f4f4;">
<table style="width:100%;max-width:600px;margin:20px auto;background:#fff;border-radius:8px;overflow:hidden;">
<tr><td style="padding:30px 40px;background:linear-gradient(135deg,#2563eb,#1d4ed8);text-align:center;">
<h1 style="color:#fff;font-size:28px;margin:0;">Welcome to WorkflowSwift!</h1></td></tr>
<tr><td style="padding:40px;">
<p style="font-size:16px;color:#374151;">Hello <strong>{name}</strong>,</p>
<p style="font-size:16px;color:#374151;">Welcome! Your account has been created.</p>
<div style="background:#f3f4f6;padding:20px;border-radius:8px;margin:25px 0;border-left:4px solid #2563eb;">
<p style="margin:8px 0;font-size:14px;color:#6b7280;"><strong>Email:</strong> <span style="color:#111827;">{email}</span></p>
<p style="margin:8px 0;font-size:14px;color:#6b7280;"><strong>Temp Password:</strong> <span style="color:#111827;font-family:monospace;">{password}</span></p></div>
<p style="font-size:14px;color:#6b7280;">Please log in and change your password.</p>
<table style="margin:30px auto;"><tr><td style="background:#2563eb;border-radius:6px;text-align:center;">
<a href="{url}" style="display:inline-block;padding:14px 40px;color:#fff;text-decoration:none;font-size:16px;font-weight:bold;">Log In Now</a>
</td></tr></table>
<p style="font-size:14px;color:#9ca3af;text-align:center;">Best regards,<br>The WorkflowSwift Team</p>
</td></tr></table></body></html>"#,
                    name = name,
                    email = email,
                    password = password,
                    url = app_url
                )
            } else {
                format!(
                    r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"></head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333; margin:0;padding:0;background:#f4f4f4;">
<table style="width:100%;max-width:600px;margin:20px auto;background:#fff;border-radius:8px;overflow:hidden;">
<tr><td style="padding:30px 40px;background:linear-gradient(135deg,#059669,#047857);text-align:center;">
<h1 style="color:#fff;font-size:28px;margin:0;">Team Invitation</h1></td></tr>
<tr><td style="padding:40px;">
<p style="font-size:16px;color:#374151;">Hello <strong>{name}</strong>,</p>
<p style="font-size:16px;color:#374151;">You have been invited to join <strong>{account}</strong>!</p>
<div style="background:#f3f4f6;padding:20px;border-radius:8px;margin:25px 0;border-left:4px solid #059669;">
<p style="margin:8px 0;font-size:14px;color:#6b7280;"><strong>Email:</strong> <span style="color:#111827;">{email}</span></p>
<p style="margin:8px 0;font-size:14px;color:#6b7280;"><strong>Temp Password:</strong> <span style="color:#111827;font-family:monospace;">{password}</span></p></div>
<p style="font-size:14px;color:#6b7280;">Please log in and change your password.</p>
<table style="margin:30px auto;"><tr><td style="background:#059669;border-radius:6px;text-align:center;">
<a href="{url}" style="display:inline-block;padding:14px 40px;color:#fff;text-decoration:none;font-size:16px;font-weight:bold;">Log In Now</a>
</td></tr></table>
<p style="font-size:14px;color:#9ca3af;text-align:center;">Best regards,<br>The WorkflowSwift Team</p>
</td></tr></table></body></html>"#,
                    name = name,
                    account = account_name,
                    email = email,
                    password = password,
                    url = app_url
                )
            };

            let text_body = if template_type == "welcome" {
                format!(
                    "Hello {},\n\nWelcome to WorkflowSwift! Your account has been created.\n\nHere are your login credentials:\n  Email: {}\n  Temporary Password: {}\n\nPlease log in at {} and change your password.\n\nBest regards,\nThe WorkflowSwift Team",
                    name, email, password, app_url
                )
            } else {
                format!(
                    "Hello {},\n\nYou have been invited to join {} on WorkflowSwift!\n\nHere are your login credentials:\n  Email: {}\n  Temporary Password: {}\n\nPlease log in at {} and change your password.\n\nBest regards,\nThe WorkflowSwift Team",
                    name, account_name, email, password, app_url
                )
            };

            send_branded(cfg, to, &subject, &text_body, &html_body, branding).await
        }
        "purchase_confirmed" => {
            let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
            let plan_name = vars
                .get("plan_name")
                .and_then(|v| v.as_str())
                .unwrap_or("a plan");
            let app_url = vars
                .get("app_url")
                .and_then(|v| v.as_str())
                .unwrap_or("https://app.workflowswift.com");

            let subject = "Payment Received — Thank You!".to_string();
            let html_body = format!(
                r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"></head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333; margin:0;padding:0;background:#f4f4f4;">
<table style="width:100%;max-width:600px;margin:20px auto;background:#fff;border-radius:8px;overflow:hidden;">
<tr><td style="padding:30px 40px;background:linear-gradient(135deg,#059669,#047857);text-align:center;">
<h1 style="color:#fff;font-size:28px;margin:0;">Payment Received!</h1></td></tr>
<tr><td style="padding:40px;">
<p style="font-size:16px;color:#374151;">Hello <strong>{name}</strong>,</p>
<p style="font-size:16px;color:#374151;">Your payment for <strong>{plan_name}</strong> has been confirmed. Thank you!</p>
<p style="font-size:14px;color:#6b7280;">You can access your account and manage your subscription from the dashboard.</p>
<table style="margin:30px auto;"><tr><td style="background:#059669;border-radius:6px;text-align:center;">
<a href="{url}" style="display:inline-block;padding:14px 40px;color:#fff;text-decoration:none;font-size:16px;font-weight:bold;">Go to Dashboard</a>
</td></tr></table>
<p style="font-size:14px;color:#9ca3af;text-align:center;">Best regards,<br>The WorkflowSwift Team</p>
</td></tr></table></body></html>"#,
                name = name,
                plan_name = plan_name,
                url = app_url
            );
            let text_body = format!(
                "Hello {},\n\nYour payment for {} has been confirmed. Thank you!\n\nYou can access your account at {}.\n\nBest regards,\nThe WorkflowSwift Team",
                name, plan_name, app_url
            );
            send_branded(cfg, to, &subject, &text_body, &html_body, branding).await
        }
        "password_reset" => {
            let token = vars.get("token").and_then(|v| v.as_str()).unwrap_or("");

            let html_body = format!(
                r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"></head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333; margin:0;padding:0;background:#f4f4f4;">
<table style="width:100%;max-width:600px;margin:20px auto;background:#fff;border-radius:8px;overflow:hidden;">
<tr><td style="padding:30px 40px;background:linear-gradient(135deg,#dc2626,#b91c1c);text-align:center;">
<h1 style="color:#fff;font-size:28px;margin:0;">Password Reset</h1></td></tr>
<tr><td style="padding:40px;">
<p style="font-size:16px;color:#374151;">You have requested a password reset.</p>
<div style="background:#fef2f2;padding:20px;border-radius:8px;margin:25px 0;text-align:center;border:2px dashed #dc2626;">
<p style="font-size:32px;font-weight:bold;letter-spacing:6px;color:#dc2626;margin:0;font-family:monospace;">{token}</p></div>
<p style="font-size:14px;color:#6b7280;">Code expires in <strong>1 hour</strong>.</p>
<p style="font-size:14px;color:#9ca3af;margin-top:25px;">If you did not request this, ignore this email.</p>
<p style="font-size:14px;color:#9ca3af;text-align:center;margin-top:30px;">- The WorkflowSwift Team</p>
</td></tr></table></body></html>"#,
                token = token
            );

            let text_body = format!(
                "Your password reset code is: {}\n\nThis code expires in 1 hour.\n\nIf you did not request this password reset, please ignore this email.\n\n- WorkflowSwift",
                token
            );

            send_branded(
                cfg,
                to,
                "Password Reset Request",
                &text_body,
                &html_body,
                branding,
            )
            .await
        }
        // A Notify step's mail (kanban t_d3ff37ef). The recipient is ALWAYS one of the account's
        // own people — `crate::notify` resolves it and refuses anything else BEFORE this is
        // reached, so the address here is never a free-text one the step named.
        "workflow_notify" => {
            let message = vars.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
            let subject = vars
                .get("subject")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("WorkflowSwift notification");
            let safe = message.replace('<', "&lt;").replace('>', "&gt;");
            let html_body = format!(
                "<html><body style=\"font-family:sans-serif\"><p>Hi {name},</p>\
                 <p>A workflow you own sent you this notification:</p>\
                 <blockquote style=\"border-left:4px solid #6366f1;padding:8px 16px;color:#374151\">\
                 {safe}</blockquote>\
                 <p style=\"font-size:13px;color:#9ca3af\">- WorkflowSwift</p></body></html>"
            );
            let text_body = format!("Hi {name},\n\nA workflow you own sent you this notification:\n\n{message}\n\n- WorkflowSwift");
            send_branded(cfg, to, subject, &text_body, &html_body, branding).await
        }
        _ => {
            let text_body = format!("WorkflowSwift Notification:\n\n{}", vars);
            send_branded(
                cfg,
                to,
                "WorkflowSwift Notification",
                &text_body,
                "",
                branding,
            )
            .await
        }
    }
}

/// Compatibility wrapper — used by password reset flow which has no AppState
/// Attempts DB template first, falls back to inline.
pub async fn send_reset_email(
    state: &AppState,
    aid: Option<Uuid>,
    to: &str,
    token: &str,
) -> Result<(), String> {
    let vars = json!({
        "token": token,
        "name": "there",
        "app_url": "https://app.workflowswift.com",
    });
    // `aid` is the resetting user's account, passed in by `forgot_password` so the reset mail can
    // carry that account's branding (kanban t_c3cfe7ba). It used to be dropped, which is the same
    // shape as the fleet's `Uuid::nil()` trap: a reset mail could never be branded.
    send_email(state, aid, to, "password_reset", &vars).await
}

// ---- Data types ----

#[derive(Debug, sqlx::FromRow)]
struct EmailTemplateRow {
    id: Uuid,
    name: String,
    subject: Option<String>,
    body: Option<String>,
    html_body: Option<String>,
    is_html: Option<bool>,
    is_default: Option<bool>,
}

// ---- Core sender ----

/// The provider vocabulary of this app's mail sender — ONE list, read by the Admin > Settings >
/// Email picker (www-admin/index.html) and by `GET /api/v1/integrations/resolve` for an `email`
/// step. Each entry has an arm in `send_email_request` below; `smtp` and `mailgun` are the two
/// that are only reachable through their own field set.
///
/// The resolver used to answer for the `email` STEP TYPE with a hand-kept
/// `["sendgrid","smtp","mailgun"]` — which both omitted `sendiio` (a transport the admin UI offers)
/// and carried the RETIRED `export` step type in the same arm (kanban t_88082a4c).
pub const EMAIL_PROVIDERS: &[&str] = &["smtp", "mailgun", "sendgrid", "sendiio"];

/// The app's OWN support address. David (2026-10-08): the reply-to/support address in a
/// transactional mail is ALWAYS `support@` the app's own main domain — never the platform's
/// or a sibling company's. WorkflowSwift mail carried none at all, so [`with_support_footer`]
/// adds this to every message that leaves the app.
pub const SUPPORT_EMAIL: &str = "support@workflowswift.com";

/// Append the app's support line to one body.
///
/// Applied at the single funnel every transport shares ([`send_email_request`]), so a body that
/// came from the authoritative `email_templates` row carries the address exactly like a
/// hardcoded fallback body does, and a template added later inherits it. An EMPTY body stays
/// empty (the `html_body` "" means "no HTML part" and must not become a footer-only part), and
/// a body that already carries the address is returned untouched.
fn with_support_footer(body: &str, html: bool) -> String {
    if body.is_empty() || body.contains(SUPPORT_EMAIL) {
        return body.to_string();
    }
    if html {
        format!(
            "{}\n<p style=\"font-size:13px;color:#6b7280;text-align:center;\">Need help? Contact <a href=\"mailto:{}\">{}</a></p>",
            body, SUPPORT_EMAIL, SUPPORT_EMAIL
        )
    } else {
        format!("{}\n\nNeed help? Contact {}\n", body, SUPPORT_EMAIL)
    }
}

/// Send one message through whichever provider the admin selected in
/// Admin > Settings > Email. Providers: `smtp` | `mailgun` | `sendgrid` | `sendiio`.
///
/// The provider is a stored choice, not a compiled-in one: no branch here can be
/// reached without a DB configuration, and an unknown value falls back to the
/// Mailgun-compatible HTTP shape (which was the only transport before the field
/// existed) rather than to an env var.
async fn send_email_request(
    cfg: &EmailConfig,
    to: &str,
    subject: &str,
    text_body: &str,
    html_body: &str,
) -> Result<(), String> {
    if !cfg.is_configured() {
        return Err(
            "Email not configured: set the provider in Admin > Settings > Email".to_string(),
        );
    }

    // Every transactional body leaves with the app's own support address on it.
    let text_body = with_support_footer(text_body, false);
    let html_body = with_support_footer(html_body, true);

    match cfg.provider.as_str() {
        "smtp" | "mail" => send_via_smtp(cfg, to, subject, &text_body, &html_body).await,
        "sendgrid" => send_via_sendgrid(cfg, to, subject, &text_body, &html_body).await,
        "sendiio" => send_via_sendiio(cfg, to, subject, &text_body, &html_body).await,
        _ => send_via_mailgun(cfg, to, subject, &text_body, &html_body).await,
    }
}

/// Plain JSON POST helper shared by the API transports.
async fn post_json(
    url: &str,
    headers: Vec<(&'static str, String)>,
    body: serde_json::Value,
) -> Result<(), String> {
    let mut req = reqwest::Client::new().post(url).json(&body);
    for (name, value) in headers {
        req = req.header(name, value);
    }
    let resp = req
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("Failed to reach email provider: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Email provider returned {}: {}", status, text));
    }

    Ok(())
}

/// Mailgun-compatible HTTP API: Basic Auth (`api:<key>`) + form-encoded body.
async fn send_via_mailgun(
    cfg: &EmailConfig,
    to: &str,
    subject: &str,
    text_body: &str,
    html_body: &str,
) -> Result<(), String> {
    let from = from_header(cfg);

    // A From on a domain that is not the sending domain's organisation fails DMARC alignment at a
    // strict receiver: the provider accepts the message and the *recipient* rejects it, so the
    // only symptom is a 554 in the provider's event log. Warn loudly instead of shipping mail into
    // that hole (the same check is why `DEFAULT_EMAIL_FROM_ADDRESS` lives on mail.workflowswift.com).
    let from_domain = org_domain(&from);
    if !from_domain.is_empty() && !mailgun_url_sends_as(&cfg.api_url, &from_domain) {
        eprintln!(
            "[email] From domain '{}' is not the domain this Mailgun endpoint sends as ('{}') — \
             strict receivers reject unaligned mail (DMARC); set the From address on that domain \
             in Admin > Settings > Email",
            from_domain, cfg.api_url
        );
    }

    let mut params = std::collections::HashMap::new();
    params.insert("from", from);
    params.insert("to", to.to_string());
    params.insert("subject", subject.to_string());
    params.insert("text", text_body.to_string());

    if !html_body.is_empty() {
        params.insert("html", html_body.to_string());
    }

    let resp = reqwest::Client::new()
        .post(&cfg.api_url)
        .basic_auth("api", Some(&cfg.api_key))
        .form(&params)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("Failed to reach mailgun: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Email API returned {}: {}", status, text));
    }

    Ok(())
}

/// The SendGrid v3 request body. Carries the SAME two-half identity the mailgun/SMTP arms build
/// (`from_identity`), so a config with no `from_address` posts the hyphenated default address with
/// the app's display name — it used to post the bare, unhyphenated address and an empty name.
fn sendgrid_body(
    cfg: &EmailConfig,
    to: &str,
    subject: &str,
    content: &serde_json::Value,
) -> serde_json::Value {
    let (sender, sender_name) = from_identity(cfg);
    json!({
        "personalizations": [{"to": [{"email": to}]}],
        "from": {"email": sender, "name": sender_name},
        "subject": subject,
        "content": [content],
    })
}

/// SendGrid v3 `/mail/send` — Bearer key, JSON body.
/// `api_url` may be left blank; the provider's endpoint is then used.
async fn send_via_sendgrid(
    cfg: &EmailConfig,
    to: &str,
    subject: &str,
    text_body: &str,
    html_body: &str,
) -> Result<(), String> {
    let url = if cfg.api_url.trim().is_empty() {
        "https://api.sendgrid.com/v3/mail/send".to_string()
    } else {
        cfg.api_url.clone()
    };

    let content = if html_body.is_empty() {
        json!({"type": "text/plain", "value": text_body})
    } else {
        json!({"type": "text/html", "value": html_body})
    };

    let body = sendgrid_body(cfg, to, subject, &content);

    post_json(
        &url,
        vec![("Authorization", format!("Bearer {}", cfg.api_key))],
        body,
    )
    .await
}

/// Sendiio JSON endpoint: `{email, subject, message, api_key}`.
async fn send_via_sendiio(
    cfg: &EmailConfig,
    to: &str,
    subject: &str,
    text_body: &str,
    html_body: &str,
) -> Result<(), String> {
    let message = if html_body.is_empty() {
        text_body
    } else {
        html_body
    };

    let body = json!({
        "email": to,
        "subject": subject,
        "message": message,
        "api_key": cfg.api_key,
    });

    post_json(&cfg.api_url, Vec::new(), body).await
}

/// Plain SMTP via lettre. `smtp_encryption`: `ssl` = implicit TLS,
/// `none` = plain, anything else = STARTTLS.
async fn send_via_smtp(
    cfg: &EmailConfig,
    to: &str,
    subject: &str,
    text_body: &str,
    html_body: &str,
) -> Result<(), String> {
    use lettre::message::MultiPart;
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

    let from = from_header(cfg);
    let builder = Message::builder()
        .from(
            from.parse()
                .map_err(|e| format!("Invalid from address '{}': {}", from, e))?,
        )
        .to(to
            .parse()
            .map_err(|e| format!("Invalid to address '{}': {}", to, e))?)
        .subject(subject);

    let email = if html_body.is_empty() {
        builder.body(text_body.to_string())
    } else {
        builder.multipart(MultiPart::alternative_plain_html(
            text_body.to_string(),
            html_body.to_string(),
        ))
    }
    .map_err(|e| format!("Failed to build email: {}", e))?;

    let creds = Credentials::new(cfg.smtp_username.clone(), cfg.smtp_password.clone());

    let mailer = match cfg.smtp_encryption.as_str() {
        "ssl" | "implicit" => AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.smtp_host)
            .map_err(|e| format!("SMTP relay error: {}", e))?
            .port(cfg.smtp_port)
            .credentials(creds)
            .build(),
        "none" | "plain" => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&cfg.smtp_host)
            .port(cfg.smtp_port)
            .credentials(creds)
            .build(),
        _ => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.smtp_host)
            .map_err(|e| format!("SMTP STARTTLS error: {}", e))?
            .port(cfg.smtp_port)
            .credentials(creds)
            .build(),
    };

    mailer
        .send(email)
        .await
        .map(|_| ())
        .map_err(|e| format!("SMTP send failed: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `EmailConfig` carrying only the two identity halves — every other field is irrelevant to
    /// the From the transport builds. `provider` / `smtp_host` are set by the one test that needs a
    /// SEND to actually be attemptable.
    fn cfg_with(from_address: &str, from_name: &str) -> EmailConfig {
        EmailConfig {
            provider: "mailgun".to_string(),
            api_url: "https://api.mailgun.net/v3/mail.workflowswift.com/messages".to_string(),
            api_key: "key".to_string(),
            from_address: from_address.to_string(),
            from_name: from_name.to_string(),
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_username: String::new(),
            smtp_password: String::new(),
            smtp_encryption: "none".to_string(),
        }
    }

    #[test]
    fn org_domain_reads_the_domain_of_a_from_header() {
        assert_eq!(
            org_domain("WorkflowSwift Help Desk <no-reply@mail.workflowswift.com>"),
            "workflowswift.com"
        );
        // The From that shipped before this fix: same organisation as the sending domain is NOT
        // the same thing — yahoo.com is a different organisation and a strict receiver rejects it.
        assert_eq!(
            org_domain("WorkflowSwift <swiftsoftware143@yahoo.com>"),
            "yahoo.com"
        );
    }

    #[test]
    fn mailgun_url_sends_as_the_domain_in_its_path() {
        let url = "https://api.mailgun.net/v3/mail.workflowswift.com/messages";
        // What the app stores today: aligned, so no warning.
        assert!(mailgun_url_sends_as(
            url,
            &org_domain("WorkflowSwift Help Desk <no-reply@mail.workflowswift.com>")
        ));
        // What it stored while this card was open: Mailgun accepted it and Yahoo answered
        // `554 espblock`, because yahoo.com is not a domain this endpoint signs for.
        assert!(!mailgun_url_sends_as(
            url,
            &org_domain("WorkflowSwift <swiftsoftware143@yahoo.com>")
        ));
        // Never warn on an unconfigured endpoint.
        assert!(!mailgun_url_sends_as(url, ""));
    }

    #[test]
    fn the_email_provider_vocabulary_matches_the_admin_picker() {
        // Every provider the sender declares must be one the admin UI actually offers, and vice
        // versa: the resolver serves this list for an `email` step (kanban t_88082a4c), so a
        // transport the admin can select but the list omits (or one it names but the picker does
        // not) is the drift this const exists to stop.
        let admin = include_str!("../www-admin/index.html");
        for provider in EMAIL_PROVIDERS {
            assert!(
                admin.contains(&format!("'{provider}'")),
                "email provider '{provider}' is missing from the Admin > Settings > Email picker"
            );
        }
        assert_eq!(
            EMAIL_PROVIDERS.len(),
            4,
            "the admin picker offers four providers"
        );
    }

    #[test]
    fn default_from_is_aligned_with_the_mailgun_sending_domain() {
        let url = "https://api.mailgun.net/v3/mail.workflowswift.com/messages";
        assert!(mailgun_url_sends_as(
            url,
            &org_domain(DEFAULT_EMAIL_FROM_ADDRESS)
        ));
        // The default identity as a whole — name included — must ALSO be aligned, because a From
        // whose domain drifted (the card's whole point) would mail into the DMARC hole silently.
        assert!(mailgun_url_sends_as(
            url,
            &org_domain(&from_header(&cfg_with("", "")))
        ));
    }

    /// A config with no `from_address` is reachable on the `smtp` and `sendgrid` arms (they need a
    /// host / a key, not a From), and it must send the owner's identity — never the retired bare,
    /// unhyphenated default (kanban t_98ffd5fa).
    #[test]
    fn the_default_identity_is_the_app_help_desk_with_a_hyphenated_address() {
        let cfg = cfg_with("", "");
        assert_eq!(
            from_header(&cfg),
            "WorkflowSwift Help Desk <no-reply@mail.workflowswift.com>"
        );
        // The SMTP arm feeds exactly this string into `Message::builder().from(..)`, which has to
        // PARSE it: a malformed default would fail every SMTP send at runtime, not merely look
        // wrong in a log.
        let mailbox: lettre::message::Mailbox = from_header(&cfg)
            .parse()
            .expect("the default From must parse as a mailbox");
        assert_eq!(mailbox.email.to_string(), "no-reply@mail.workflowswift.com");
        assert_eq!(mailbox.name.as_deref(), Some("WorkflowSwift Help Desk"));
        // The unhyphenated form David asked to retire must be gone, both as the whole address and
        // as the local part.
        assert!(!from_header(&cfg).contains("noreply@"));
    }

    /// The identity is TWO row fields, so each half falls back on its own: setting only one keeps
    /// the half that was set and supplies the app identity for the other. The live config sets
    /// both, and that case must be untouched.
    #[test]
    fn each_half_of_the_from_identity_falls_back_on_its_own() {
        assert_eq!(
            from_header(&cfg_with("billing@acme.example", "Acme Billing")),
            "Acme Billing <billing@acme.example>",
            "both halves set: nothing is invented"
        );
        assert_eq!(
            from_header(&cfg_with("", "Acme Billing")),
            "Acme Billing <no-reply@mail.workflowswift.com>",
            "address blank: the hyphenated default, the admin's own display name kept"
        );
        assert_eq!(
            from_header(&cfg_with("billing@acme.example", "")),
            "WorkflowSwift Help Desk <billing@acme.example>",
            "name blank: the app's own display name supplies it"
        );
        // Whitespace is not a value: a row saved with spaces must not produce a broken header.
        assert_eq!(
            from_header(&cfg_with("   ", "  ")),
            "WorkflowSwift Help Desk <no-reply@mail.workflowswift.com>"
        );
    }

    /// The premise that makes the default reachable at all — neither arm requires a `from_address`.
    #[test]
    fn smtp_and_sendgrid_are_configured_without_a_from_address() {
        let mut smtp = cfg_with("", "");
        smtp.provider = "smtp".to_string();
        smtp.smtp_host = "smtp.example.com".to_string();
        assert!(
            smtp.is_configured(),
            "provider=smtp needs a host, not a From"
        );

        let mut sendgrid = cfg_with("", "");
        sendgrid.provider = "sendgrid".to_string();
        sendgrid.api_key = "SG.test".to_string();
        assert!(
            sendgrid.is_configured(),
            "provider=sendgrid needs a key, not a From"
        );
    }

    /// The SendGrid arm used to post `{"email": noreply@…, "name": ""}`. It must carry the SAME
    /// two-half identity the mailgun/SMTP arms build.
    #[test]
    fn the_sendgrid_arm_carries_the_default_identity_too() {
        let body = sendgrid_body(
            &cfg_with("", ""),
            "dana@example.com",
            "Hello",
            &json!({"type": "text/plain", "value": "hi"}),
        );
        assert_eq!(body["from"]["email"], "no-reply@mail.workflowswift.com");
        assert_eq!(body["from"]["name"], "WorkflowSwift Help Desk");
        assert_eq!(
            body["personalizations"][0]["to"][0]["email"],
            "dana@example.com"
        );
        // And with the config the live row carries, the row's own two halves still win.
        let row = sendgrid_body(
            &cfg_with("no-reply@mail.workflowswift.com", "WorkflowSwift Help Desk"),
            "dana@example.com",
            "Hello",
            &json!({"type": "text/plain", "value": "hi"}),
        );
        assert_eq!(row["from"]["name"], "WorkflowSwift Help Desk");
    }

    /// The SMTP arm end to end, over a real socket: the default From must arrive as the `From:`
    /// header on the wire. This is the only instrument that runs `from_header` -> `parse` ->
    /// lettre's serialiser -> a socket, so it is the one that can fail if the default stops being
    /// a valid mailbox. Plain sink, no STARTTLS — the same shape an `smtp_encryption = none`
    /// transport (`builder_dangerous`) speaks. AUTH is advertised because this arm always sets
    /// credentials, and lettre refuses to send when the server offers no mechanism it can use.
    #[tokio::test]
    async fn the_smtp_arm_puts_the_default_identity_on_the_wire() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("smtp sink listener");
        let port = listener.local_addr().unwrap().port();

        let sink = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("one connection");
            let (rd, mut wr) = stream.into_split();
            let mut rd = BufReader::new(rd);
            let mut message: Vec<u8> = Vec::new();
            let mut in_data = false;
            let mut line: Vec<u8> = Vec::new();
            wr.write_all(b"220 workflowswift-smtp-sink ESMTP\r\n")
                .await
                .unwrap();
            wr.flush().await.unwrap();
            loop {
                line.clear();
                if rd.read_until(b'\n', &mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                if in_data {
                    if line == b".\r\n" || line == b".\n" {
                        in_data = false;
                        wr.write_all(b"250 OK queued as <probe>\r\n").await.unwrap();
                        wr.flush().await.unwrap();
                    } else {
                        message.extend_from_slice(&line);
                    }
                    continue;
                }
                let up = String::from_utf8_lossy(&line).to_ascii_uppercase();
                // TWO capability lines, the LAST unhyphenated: a lone "250-..." is a continuation
                // and a real client waits for the final line forever.
                let reply: &[u8] = if up.starts_with("EHLO") || up.starts_with("HELO") {
                    b"250-workflowswift-smtp-sink\r\n250-AUTH PLAIN LOGIN\r\n250 SIZE 10485760\r\n"
                } else if up.starts_with("DATA") {
                    in_data = true;
                    b"354 End data with <CR><LF>.<CR><LF>\r\n"
                } else if up.starts_with("QUIT") {
                    b"221 Bye\r\n"
                } else {
                    b"250 OK\r\n"
                };
                wr.write_all(reply).await.unwrap();
                wr.flush().await.unwrap();
                if up.starts_with("QUIT") {
                    break;
                }
            }
            String::from_utf8_lossy(&message).to_string()
        });

        let mut cfg = cfg_with("", "");
        cfg.provider = "smtp".to_string();
        cfg.smtp_host = "127.0.0.1".to_string();
        cfg.smtp_port = port;

        send_via_smtp(&cfg, "dana@example.com", "Identity probe", "body text", "")
            .await
            .expect("the smtp arm must accept the default identity");

        let wire = tokio::time::timeout(std::time::Duration::from_secs(10), sink)
            .await
            .expect("the sink must see the send before the deadline")
            .expect("sink task");
        // Read the header back and PARSE it rather than string-matching the raw bytes: lettre
        // quotes a display name that contains spaces (`From: "WorkflowSwift Help Desk" <...>`),
        // which is the same mailbox the unquoted form denotes. What must hold is the identity the
        // recipient sees, and that the wire value is a valid mailbox.
        let from_line = wire
            .lines()
            .find(|l| l.starts_with("From: "))
            .expect("a From header on the wire");
        let wired: lettre::message::Mailbox = from_line["From: ".len()..]
            .trim()
            .parse()
            .expect("the wire From must re-parse as a mailbox");
        assert_eq!(wired.email.to_string(), "no-reply@mail.workflowswift.com");
        assert_eq!(wired.name.as_deref(), Some("WorkflowSwift Help Desk"));
        assert!(
            !wire.contains("noreply@mail.workflowswift.com"),
            "the retired unhyphenated default reached the wire:\n{}",
            wire
        );
    }

    #[test]
    fn names_a_single_brace_placeholder_the_renderer_cannot_substitute() {
        // The dialect that used to mail silently: `{{key}}` is the ONLY vocabulary
        // `render_template` substitutes, so a stored row written `Hi {name}!` went out as literal
        // braces. First half of the proof — the detector must NAME `name` (kanban t_f70ef1bb).
        assert_eq!(
            unsubstituted("Hi {name}!"),
            vec!["name"],
            "a single-brace token is a leftover even though the renderer never bound it"
        );
        assert_eq!(
            unsubstituted("Welcome to {app_name}, {email} ({name})"),
            vec!["app_name", "email", "name"]
        );
        // A surviving double-brace token (no caller bound `password`) is just as unsubstituted.
        assert_eq!(
            unsubstituted("{{name}} / {{password}}"),
            vec!["name", "password"]
        );
        // Deduplicated, in first-seen order.
        assert_eq!(unsubstituted("{x} {x} {y}"), vec!["x", "y"]);
    }

    #[test]
    fn stays_quiet_on_a_clean_double_brace_body() {
        // Second half of the proof: when every `{{key}}` is bound, `render_template` reports
        // nothing — no false positive on the vocabulary this app speaks, nor on braces CSS/HTML
        // legitimately carries.
        let vars = json!({
            "name": "Dana",
            "email": "dana@example.com",
            "app_name": "WorkflowSwift",
        });
        let out = render_template(
            "Hi {{name}}, welcome to {{app_name}} ({{email}})!",
            &vars,
            "test",
        );
        assert_eq!(out, "Hi Dana, welcome to WorkflowSwift (dana@example.com)!");
        assert!(
            unsubstituted(&out).is_empty(),
            "clean render reports nothing"
        );
        assert!(unprocessed_scaffolding(&out).is_empty());
        for clean in [
            "a { color: red; }",
            r#"{"a": {"b": 1}}"#,
            "<p style='font-size:14px'>Hi</p>",
            "",
        ] {
            assert!(
                unsubstituted(clean).is_empty(),
                "must stay silent on {clean:?}"
            );
        }
    }

    #[test]
    fn names_mustache_markers_the_renderer_cannot_process() {
        // The other half of the same blind spot: a block marker holds no identifier, so
        // `unsubstituted` cannot see it — it is named IN FULL by its own arm.
        assert_eq!(
            unprocessed_scaffolding("{{#if prize_name}}x{{/if}}"),
            vec!["{{#if prize_name}}", "{{/if}}"]
        );
        assert_eq!(
            unprocessed_scaffolding("{{#each xs}}{{x}}{{else}}none{{/each}}"),
            vec!["{{#each xs}}", "{{else}}", "{{/each}}"]
        );
        assert!(
            unprocessed_scaffolding("Welcome, {{name}}!").is_empty(),
            "the vocabulary this app speaks is never reported as markup"
        );
    }
}
