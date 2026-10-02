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

/// Default From address — used only when the admin has not set one in
/// Admin > Settings > Email. Not a credential, so it is safe as a constant.
///
/// It must live on the domain the provider actually sends through
/// (`mail.workflowswift.com`, see `api_url`): Mailgun signs with that domain's DKIM, so a From on
/// any other domain fails DMARC alignment at a strict receiver. Measured on the live domain — a
/// message From `swiftsoftware143@yahoo.com` sent through `mail.workflowswift.com` was accepted
/// and then failed with remote code `554 reason=espblock` (yahoo.com publishes `p=reject`), while
/// the same message From `noreply@mail.workflowswift.com` was delivered (`250 ok dirdel`).
const DEFAULT_EMAIL_FROM: &str = "noreply@mail.workflowswift.com";

/// The organisation-level domain of an address or From header:
/// `WorkflowSwift <noreply@mail.workflowswift.com>` -> `workflowswift.com`.
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
                send_email_request(&cfg, to, &subject, &text_body, &html_body).await
            } else {
                send_email_request(&cfg, to, &subject, &text_body, "").await
            }
        }
        None => {
            // Fallback to hardcoded template
            send_email_fallback(&cfg, to, template_type, vars).await
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
    to: &str,
    template_type: &str,
    vars: &serde_json::Value,
) -> Result<(), String> {
    let result = send_email_inner(state, to, template_type, vars).await;
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
    fn is_configured(&self) -> bool {
        match self.provider.as_str() {
            "smtp" | "mail" => !self.smtp_host.trim().is_empty(),
            "sendgrid" => !self.api_key.trim().is_empty(),
            _ => !self.api_url.trim().is_empty() && !self.api_key.trim().is_empty(),
        }
    }
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

/// From header — `Name <addr>` when a name is set, else the bare address.
fn from_header(cfg: &EmailConfig) -> String {
    let addr = if cfg.from_address.is_empty() {
        DEFAULT_EMAIL_FROM.to_string()
    } else {
        cfg.from_address.clone()
    };
    if cfg.from_name.is_empty() {
        addr
    } else {
        format!("{} <{}>", cfg.from_name, addr)
    }
}

/// Fallback hardcoded templates (used when DB template not found)
async fn send_email_fallback(
    cfg: &EmailConfig,
    to: &str,
    template_type: &str,
    vars: &serde_json::Value,
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

            send_email_request(cfg, to, &subject, &text_body, &html_body).await
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
            send_email_request(cfg, to, &subject, &text_body, &html_body).await
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

            send_email_request(cfg, to, "Password Reset Request", &text_body, &html_body).await
        }
        _ => {
            let text_body = format!("WorkflowSwift Notification:\n\n{}", vars);
            send_email_request(cfg, to, "WorkflowSwift Notification", &text_body, "").await
        }
    }
}

/// Compatibility wrapper — used by password reset flow which has no AppState
/// Attempts DB template first, falls back to inline.
pub async fn send_reset_email(state: &AppState, to: &str, token: &str) -> Result<(), String> {
    let vars = json!({
        "token": token,
        "name": "there",
        "app_url": "https://app.workflowswift.com",
    });
    send_email(state, to, "password_reset", &vars).await
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

    match cfg.provider.as_str() {
        "smtp" | "mail" => send_via_smtp(cfg, to, subject, text_body, html_body).await,
        "sendgrid" => send_via_sendgrid(cfg, to, subject, text_body, html_body).await,
        "sendiio" => send_via_sendiio(cfg, to, subject, text_body, html_body).await,
        _ => send_via_mailgun(cfg, to, subject, text_body, html_body).await,
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
    // that hole (the same check is why `DEFAULT_EMAIL_FROM` lives on mail.workflowswift.com).
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

    let sender = if cfg.from_address.is_empty() {
        DEFAULT_EMAIL_FROM
    } else {
        cfg.from_address.as_str()
    };

    let body = json!({
        "personalizations": [{"to": [{"email": to}]}],
        "from": {"email": sender, "name": cfg.from_name},
        "subject": subject,
        "content": [content],
    });

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

    #[test]
    fn org_domain_reads_the_domain_of_a_from_header() {
        assert_eq!(
            org_domain("WorkflowSwift <noreply@mail.workflowswift.com>"),
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
            &org_domain("WorkflowSwift <noreply@mail.workflowswift.com>")
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
    fn default_from_is_aligned_with_the_mailgun_sending_domain() {
        let url = "https://api.mailgun.net/v3/mail.workflowswift.com/messages";
        assert!(mailgun_url_sends_as(url, &org_domain(DEFAULT_EMAIL_FROM)));
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
