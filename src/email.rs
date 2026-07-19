//! Email module — sends transactional emails (welcome, team_invite, password_reset).
//!
//! Templates are stored in the `email_templates` table and can be configured
//! via the admin panel. HTML + text versions with toggle support.
//!
//! SMTP/API config comes from `admin_settings` (key: "email").
//! Fallback: EMAIL_API_URL / EMAIL_API_KEY env vars.

use std::env;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::state::AppState;

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
///
/// This is the preferred method — pass `AppState` to get access to DB and config.
pub async fn send_email(
    state: &AppState,
    to: &str,
    template_type: &str,
    vars: &serde_json::Value,
) -> Result<(), String> {
    // Get email config from DB admin_settings, fallback to env vars
    let (api_url, api_key, from_address) = get_email_config(state).await;

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
            let subject = render_template(&t.subject.unwrap_or_else(|| match template_type {
                "welcome" => "Welcome to WorkflowSwift!".into(),
                "team_invite" => "Team Invitation".into(),
                "password_reset" => "Password Reset".into(),
                _ => "WorkflowSwift Notification".into(),
            }), vars);

            let html_body = t.html_body.as_ref()
                .map(|h| render_template(h, vars))
                .unwrap_or_default();

            let text_body = render_template(&t.body.unwrap_or_default(), vars);

            let use_html = t.is_html.unwrap_or(true);

            if use_html && !html_body.is_empty() {
                send_email_request(&api_url, &api_key, &from_address, to, &subject, &text_body, &html_body).await
            } else {
                send_email_request(&api_url, &api_key, &from_address, to, &subject, &text_body, "").await
            }
        }
        None => {
            // Fallback to hardcoded template
            send_email_fallback(api_url, api_key, from_address, to, template_type, vars).await
        }
    }
}

/// Get email config: first from admin_settings DB, fallback to env vars.
async fn get_email_config(state: &AppState) -> (String, String, String) {
    // Try DB config
    let db_config = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT value FROM admin_settings WHERE key = 'email'"
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    if let Some(cfg) = db_config {
        let api_url = cfg.get("api_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| env::var("EMAIL_API_URL").unwrap_or_default());

        let api_key = cfg.get("api_key")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| env::var("EMAIL_API_KEY").unwrap_or_default());

        let from = cfg.get("from_address")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| env::var("EMAIL_FROM").unwrap_or_else(|_| "swiftsoftware143@yahoo.com".to_string()));

        (api_url, api_key, from)
    } else {
        let api_url = env::var("EMAIL_API_URL").unwrap_or_default();
        let api_key = env::var("EMAIL_API_KEY").unwrap_or_default();
        let from = env::var("EMAIL_FROM").unwrap_or_else(|_| "swiftsoftware143@yahoo.com".to_string());
        (api_url, api_key, from)
    }
}

/// Fallback hardcoded templates (used when DB template not found)
async fn send_email_fallback(
    api_url: String,
    api_key: String,
    from: String,
    to: &str,
    template_type: &str,
    vars: &serde_json::Value,
) -> Result<(), String> {
    let app_url = vars.get("app_url")
        .and_then(|v| v.as_str())
        .unwrap_or("https://app.workflowswift.com");

    match template_type {
        "welcome" | "team_invite" => {
            let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
            let email = vars.get("email").and_then(|v| v.as_str()).unwrap_or("");
            let password = vars.get("password").and_then(|v| v.as_str()).unwrap_or("");

            let account_name = if template_type == "team_invite" {
                vars.get("account_name").and_then(|v| v.as_str()).unwrap_or("Your Team")
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
                    name=name, email=email, password=password, url=app_url
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
                    name=name, account=account_name, email=email, password=password, url=app_url
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

            send_email_request(&api_url, &api_key, &from, to, &subject, &text_body, &html_body).await
        }
        "purchase_confirmed" => {
            let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
            let plan_name = vars.get("plan_name").and_then(|v| v.as_str()).unwrap_or("a plan");
            let app_url = vars.get("app_url")
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
                name=name, plan_name=plan_name, url=app_url
            );
            let text_body = format!(
                "Hello {},\n\nYour payment for {} has been confirmed. Thank you!\n\nYou can access your account at {}.\n\nBest regards,\nThe WorkflowSwift Team",
                name, plan_name, app_url
            );
            send_email_request(&api_url, &api_key, &from, to, &subject, &text_body, &html_body).await
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
                token=token
            );

            let text_body = format!(
                "Your password reset code is: {}\n\nThis code expires in 1 hour.\n\nIf you did not request this password reset, please ignore this email.\n\n- WorkflowSwift",
                token
            );

            send_email_request(&api_url, &api_key, &from, to, "Password Reset Request", &text_body, &html_body).await
        }
        _ => {
            let text_body = format!("WorkflowSwift Notification:\n\n{}", vars.to_string());
            send_email_request(&api_url, &api_key, &from, to, "WorkflowSwift Notification", &text_body, "").await
        }
    }
}

/// Compatibility wrapper — used by password reset flow which has no AppState
pub async fn send_reset_email(to: &str, token: &str) -> Result<(), String> {
    let api_url = env::var("EMAIL_API_URL")
        .map_err(|_| "EMAIL_API_URL not set".to_string())?;
    let api_key = env::var("EMAIL_API_KEY")
        .map_err(|_| "EMAIL_API_KEY not set".to_string())?;
    let from = env::var("EMAIL_FROM")
        .unwrap_or_else(|_| "swiftsoftware143@yahoo.com".to_string());

    let vars = json!({
        "token": token,
        "app_url": "https://app.workflowswift.com",
    });

    let text_body = format!(
        "Your password reset code is: {}\n\nThis code expires in 1 hour.\n\nIf you did not request this password reset, please ignore this email.\n\n- WorkflowSwift",
        token
    );

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
        token=token
    );

    send_email_request(&api_url, &api_key, &from, to, "Password Reset Request", &text_body, &html_body).await
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

/// Core HTTP request to the email API provider (Mailgun-compatible).
/// Uses Basic Auth (api:<key>) and form-encoded body as required by Mailgun REST API.
async fn send_email_request(
    api_url: &str,
    api_key: &str,
    from: &str,
    to: &str,
    subject: &str,
    text_body: &str,
    html_body: &str,
) -> Result<(), String> {
    if api_url.is_empty() || api_key.is_empty() {
        return Err("Email not configured: set EMAIL_API_URL and EMAIL_API_KEY or configure in Admin > Settings > Email".to_string());
    }

    let mut params = std::collections::HashMap::new();
    params.insert("from", from);
    params.insert("to", to);
    params.insert("subject", subject);
    params.insert("text", text_body);

    if !html_body.is_empty() {
        params.insert("html", html_body);
    }

    let client = reqwest::Client::new();
    let resp = client
        .post(api_url)
        .basic_auth("api", Some(api_key))
        .form(&params)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Failed to send email request: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Email API returned {}: {}", status, text));
    }

    Ok(())
}
