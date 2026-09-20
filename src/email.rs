//! Email module — sends transactional emails (welcome, team_invite, password_reset).
//!
//! Templates are stored in the `email_templates` table and can be configured
//! via the admin panel. HTML + text versions with toggle support.
//!
//! SMTP/API config comes from `admin_settings` (key: "email") — DB ONLY.
//! There is no env-var credential fallback: an unconfigured provider logs and
//! skips the send instead of silently using a server-wide env var.

use serde_json::json;
use uuid::Uuid;

use crate::state::AppState;

/// Default From address — used only when the admin has not set one in
/// Admin > Settings > Email. Not a credential, so it is safe as a constant.
const DEFAULT_EMAIL_FROM: &str = "swiftsoftware143@yahoo.com";

/// Render a template string by replacing {{key}} placeholders with values from `vars`.
fn render_template(template: &str, vars: &serde_json::Value) -> String {
    let mut result = template.to_string();

    // Replace {{key}} with JSON string values
    if let Some(obj) = vars.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{{{}}}}}", key);
            let replacement = value.as_str().unwrap_or("");
            result = result.replace(&placeholder, replacement);
        }
    }

    result
}

/// Send a templated email using database-stored templates.
/// Falls back to hardcoded inline templates if DB lookup fails.
/// This is the preferred method — pass `AppState` to get access to DB and config.
pub async fn send_email(
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
            );

            let html_body = t
                .html_body
                .as_ref()
                .map(|h| render_template(h, vars))
                .unwrap_or_default();

            let text_body = render_template(&t.body.unwrap_or_default(), vars);

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
    let mut params = std::collections::HashMap::new();
    params.insert("from", from_header(cfg));
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
