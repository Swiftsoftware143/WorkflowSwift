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

/// The app's OWN wordmark, used for the header of every message sent for an account that has
/// configured no branding of its own (kanban t_081edaac).
///
/// A mail client fetches it over the network with NO session, so it is an absolute URL on a host
/// that answers publicly — and it is a PNG, because Gmail and Outlook do not render the SVG the
/// pages use. `alt` carries the name, so a client that blocks images still says whose mail this is.
const APP_LOGO_PATH: &str = "/logo.png";

/// The public origin the private-inbox module (ported from CoreSwift, kanban t_473a6895) prints in
/// the records a customer pastes at their DNS provider, and in the webhook URL it hands the mail
/// provider. One value, so a second copy of the address can never appear in a mail.
pub const APP_URL: &str = crate::branding::APP_URL;

/// The mail client's own front door (the private-inbox host, ported with the module). Every
/// message a customer sends about an inbox, and every link a notification points at, resolves to
/// this ONE host.
pub const APP_URL_MAIL: &str = "https://inbox.workflowswift.com";

/// The identity every message falls back to when its account has configured none: this app's own
/// name and wordmark, in the fleet's default accent (kanban t_081edaac).
///
/// Before this the header block only appeared once an account had uploaded a logo of its own, so a
/// brand-new account's mail — and the Email Branding card's own preview — arrived with no header at
/// all, while the panel said "it carries the WorkflowSwift header". An account's own branding still
/// wins field by field.
fn app_default_branding() -> Branding {
    Branding {
        brand_name: branding::APP_NAME.to_string(),
        brand_color: String::new(),
        logo_url: APP_LOGO_PATH.to_string(),
    }
}

/// Bind the branding merge fields into a render's variable map (kanban t_081edaac).
///
/// They are PER-ACCOUNT, so they cannot live in a `&'static` default map: the account's own values
/// have to be substituted into the template it is rendered from. `stored` is what the account
/// actually saved — NOT the effective identity — so a preview can say truthfully whose logo the
/// reader is looking at: `brand_name` / `logo_url` fall back to the app's own, and `logo_is_own`
/// answers whether the logo is the account's own upload or this app's wordmark standing in for one.
/// `logo_url` is therefore never bound as an empty string.
fn bind_branding(vars: &mut serde_json::Value, stored: Option<&Branding>) {
    let app_default = app_default_branding();
    let own_logo = stored.and_then(|b| b.resolve_logo_url(branding::APP_URL));
    let name = match stored {
        Some(b) if !b.brand_name.is_empty() => b.brand_name.clone(),
        _ => branding::APP_NAME.to_string(),
    };
    let logo = own_logo
        .clone()
        .or_else(|| app_default.resolve_logo_url(branding::APP_URL))
        .unwrap_or_default();
    if let Some(obj) = vars.as_object_mut() {
        obj.insert("brand_name".to_string(), json!(name));
        obj.insert("logo_url".to_string(), json!(logo));
        obj.insert(
            "logo_is_own".to_string(),
            json!(if own_logo.is_some() { "yes" } else { "no" }),
        );
    }
}

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
        // Inside the document, never in front of it: the welcome/invite mails are whole documents,
        // and a header before `<!DOCTYPE html>` sits outside `<body>` (see `branding::place_header`).
        crate::branding::place_header(&html, &header)
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

/// Render a template string by replacing the placeholder keys in `vars` — in BOTH brace
/// conventions — with their values. Anything left over is reported by [`warn_unsubstituted`]
/// instead of being mailed silently.
///
/// BOTH CONVENTIONS, ONE RENDERER (measured live 2026-10-12). `{{key}}` is what the shipped rows
/// speak, but the admin console's template editor accepts a single-brace body with no placeholder
/// help and no validation, so a row an operator saved as `Welcome to {app_name}!` mailed the
/// recipient those literal bytes — including `Password: {password}`, i.e. no credentials at all.
/// Measured end to end off a local SMTP sink: the ACTIVE single-brace welcome row
/// (`404dbfc7-c786-4c2a-bf2c-7bd988efafec`) sent the subject `Welcome to {app_name}!` and a body
/// whose password line was literally `{password}`. The two apps already normalised this way are
/// IncentiveSwift and MissedCall Respondr.
///
/// The substitution is KEY-DRIVEN, never a regex sweep: only `{k}` / `{{k}}` for a `k` present in
/// the merged map is replaced, so a brace that is NOT a placeholder (JSON in a body, a CSS block)
/// survives byte-for-byte. App-level facts (`app_name`, `app_url`, `login_url`) are merged in as
/// defaults so the same template renders the same whichever door sent it; a caller-supplied key
/// wins.
fn render_template(template: &str, vars: &serde_json::Value, context: &str) -> String {
    let mut result = template.to_string();

    // The app's own URL, unless the caller knows a better one (the paid door passes the same value
    // under `app_url`). `app_url` and `login_url` are the two spellings the shipped rows use for
    // it, so both are bound to the one value.
    let app_url = vars
        .get("app_url")
        .and_then(|v| v.as_str())
        .unwrap_or(crate::branding::APP_URL);

    let mut merged: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    merged.insert(
        "app_name".to_string(),
        serde_json::json!(crate::branding::APP_NAME),
    );
    merged.insert("app_url".to_string(), serde_json::json!(app_url));
    merged.insert("login_url".to_string(), serde_json::json!(app_url));
    if let Some(obj) = vars.as_object() {
        // Caller-supplied keys WIN, so a door that knows the real account name still overrides.
        for (key, value) in obj {
            merged.insert(key.clone(), value.clone());
        }
    }

    for (key, value) in &merged {
        let replacement = value.as_str().unwrap_or("");
        // `{{key}}` FIRST: replacing the single form first would eat the inner braces of a
        // `{{key}}` placeholder and leave a stray `{}`-wrapped value behind.
        result = result.replace(&format!("{{{{{}}}}}", key), replacement);
        result = result.replace(&format!("{{{}}}", key), replacement);
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

    // Load the account's branding ONCE (kanban t_c3cfe7ba). `None` for no account / nothing set —
    // and an account with NOTHING set now gets THIS APP'S own header instead of none at all
    // (kanban t_081edaac), so `branding` below is never `None` on a send.
    let stored_branding = match aid {
        Some(a) => crate::branding::load(&state.db, a).await,
        None => None,
    };
    let branding = stored_branding.clone().unwrap_or_else(app_default_branding);
    // The MERGE fields follow the STORED document (an account with a name but no logo keeps their
    // own name and `logo_is_own = no`); the HEADER below follows `branding`, which carries the app's
    // own identity when there is no stored document at all.
    let mut vars = vars.clone();
    bind_branding(&mut vars, stored_branding.as_ref());
    let vars = &vars;

    match template {
        Some(t) => {
            // Use DB template
            let subject = render_template(
                &t.subject.unwrap_or_else(|| match template_type {
                    "welcome" => "Welcome to WorkflowSwift!".into(),
                    "team_invite" => "Team Invitation".into(),
                    "password_reset" => "Password Reset".into(),
                    "ticket_update" => "Update on your WorkflowSwift request".into(),
                    // The receipt is not the answer: it says the request arrived and nothing was
                    // charged (cron 2026-10-20).
                    "plan_request_receipt" => "We have your plan request".into(),
                    // The operator's diagnostic is not a notification: the subject has to say it
                    // was a test, or the mail is indistinguishable from real traffic in an inbox.
                    "email_test" => "WorkflowSwift test email".into(),
                    // A hand-move (kanban t_e12d2e73). No install carries this template row, so the
                    // INLINE arm below is what a customer actually reads; this default keeps a
                    // stored row's blank subject from falling back to a generic notification.
                    "plan_changed" => plan_changed_subject(
                        vars.get("plan_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("your new plan"),
                    ),
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
                send_branded(&cfg, to, &subject, &text_body, &html_body, Some(&branding)).await
            } else {
                send_branded(&cfg, to, &subject, &text_body, "", Some(&branding)).await
            }
        }
        None => {
            // Fallback to hardcoded template
            send_email_fallback(&cfg, to, template_type, vars, Some(&branding)).await
        }
    }
}

/// THE SUBJECT THE CUSTOMER READS when their workspace is moved onto a plan (kanban t_e12d2e73).
///
/// One definition, used twice: by the inline `plan_changed` arm (which IS the mail a real customer
/// receives — no install carries that template row) and by the DELIVERY HISTORY. The funnel records
/// a reserved/harness recipient as `withheld` BEFORE anything is rendered, so without this the
/// operator's history could only name the template key, never which sentence was suppressed.
pub(crate) fn plan_changed_subject(plan_name: &str) -> String {
    format!("Your WorkflowSwift plan is now {plan_name}")
}

/// The subject the inline arms send, for the types where the funnel must name the mail before it
/// renders (the withheld arm). `None` for every type whose history row is already unambiguous from
/// its template key — this must never grow into a second copy of the arms' wording.
pub(crate) fn inline_subject(template_type: &str, vars: &serde_json::Value) -> Option<String> {
    match template_type {
        "plan_changed" => Some(plan_changed_subject(
            vars.get("plan_name")
                .and_then(|v| v.as_str())
                .unwrap_or("your new plan"),
        )),
        _ => None,
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
    // Fleet-harness AND reserved addresses never reach a real relay (parity with FunnelSwift,
    // kanban t_36b55ed2 + t_e4d94bd9). A probe that signs up with a fleet-dev domain
    // (`swiftsoftware.dev/.net`) OR a reserved RFC 2606 / 6761 name is created normally but its mail
    // is withheld: neither can reach a customer, so a send can only bounce (measured 2026-10-09 on
    // mail.workflowswift.com: `accepted` then `failed` 552), and every such send burns a delivery on
    // the domain's sending reputation. `.local` / `localhost` stay OPEN so the local-sink content
    // harnesses can still read a message off the wire.
    if let Some(domain) = crate::security::probe_addr::harness_domain(to) {
        tracing::info!(
            to = %to,
            domain = %domain,
            template = %template_type,
            "send suppressed: recipient is a fleet harness address (fleet-dev domain)"
        );
        // Record the NON-SEND (2026-10-14). The return below is honest about the transport — nothing
        // was attempted — but recording NOTHING left an older `last_send_ok: true` standing in
        // `admin_settings.email`, so the admin panel read "Last email send succeeded" for a message
        // that never left the box (the class fixed on FunnelSwift/ADASwift/MissedCall the same day).
        // Name the mail when the subject is knowable BEFORE rendering (a plan move, 2026-11-12):
        // otherwise the history can only show the template key, and the operator cannot tell WHICH
        // message a reserved address swallowed.
        let subject_hint = inline_subject(template_type, vars);
        record_withheld_send(state, to, template_type, domain, subject_hint.as_deref()).await;
        return Ok(());
    }
    let result = send_email_inner(state, aid, to, template_type, vars).await;
    record_send_outcome(state, to, template_type, &result).await;
    result
}

/// Append one row to the delivery history (`email_send_log`, migration 091).
///
/// The panel slot below answers "what happened to the most recent message?"; this table answers
/// "did that customer's credentials mail ever go out?", which the slot cannot — any later mail
/// overwrites it. Best-effort by design: a bookkeeping failure must never fail a send that
/// actually went out, so the error is logged and dropped.
async fn append_send_log(
    state: &AppState,
    to: &str,
    template_type: &str,
    status: &str,
    detail: &str,
) {
    if let Err(e) = sqlx::query(
        "INSERT INTO email_send_log (to_email, template, status, detail) VALUES ($1, $2, $3, $4)",
    )
    .bind(to)
    .bind(template_type)
    .bind(status)
    .bind(detail.chars().take(240).collect::<String>())
    .execute(&state.db)
    .await
    {
        tracing::warn!("could not append to email_send_log: {e}");
    }
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
async fn record_send_outcome(
    state: &AppState,
    to: &str,
    template_type: &str,
    result: &Result<(), String>,
) {
    // History FIRST and independently of the slot written below: the slot keeps only the LATEST
    // outcome, so a refused credentials mail vanished the moment any later send succeeded.
    append_send_log(
        state,
        to,
        template_type,
        if result.is_ok() { "sent" } else { "failed" },
        result.as_ref().err().map(|e| e.as_str()).unwrap_or(""),
    )
    .await;
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
            obj.remove("last_send_withheld");
        }
        Err(e) => {
            obj.insert("last_send_ok".to_string(), json!(false));
            obj.insert("last_send_error".to_string(), json!(e.clone()));
            obj.remove("last_send_withheld");
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

/// Record a message the transport REFUSED to attempt — the recipient is a fleet-harness / reserved
/// address, so `send_email` returns before any provider request (2026-10-14).
///
/// Distinct from a FAILURE: nothing is broken, no customer mail is affected, and the operator must
/// not be told to go fix the transport. So it writes `last_send_ok=false` **plus**
/// `last_send_withheld=true`, and the admin panel renders that as "was NOT SENT" rather than
/// "FAILED". Same discipline as `record_send_outcome`: best-effort (a bookkeeping error must never
/// fail a send) and it re-seals the credentials before writing the row back.
pub(crate) async fn record_withheld_send(
    state: &AppState,
    to: &str,
    template_type: &str,
    domain: &str,
    subject: Option<&str>,
) {
    // Its own status, not a failure: a reserved/harness recipient is a NON-SEND — nothing was
    // attempted, no provider request was made — so it must not read as a refusal. The SUBJECT is
    // appended when the funnel knows it (2026-11-12): a history that says only "some mail" cannot
    // answer the operator's one question about a suppression — WHICH message was it.
    let detail = match subject {
        Some(s) => format!(
            "withheld: '{domain}' is a fleet-harness / reserved address, so nothing was sent (no provider request) | subject: {s}"
        ),
        None => format!(
            "withheld: '{domain}' is a fleet-harness / reserved address, so nothing was sent (no provider request)"
        ),
    };
    append_send_log(state, to, template_type, "withheld", &detail).await;
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

    obj.insert("last_send_ok".to_string(), json!(false));
    obj.insert("last_send_withheld".to_string(), json!(true));
    obj.insert("last_send_error".to_string(), json!(detail));
    obj.insert(
        "last_send_at".to_string(),
        json!(chrono::Utc::now().to_rfc3339()),
    );
    obj.insert(
        "last_send_template".to_string(),
        json!(template_type.to_string()),
    );

    if let Err(e) = seal_config_secrets(&state.db, &mut cfg).await {
        eprintln!(
            "[email] could not seal admin_settings.email before recording the withheld send: {e}"
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

/// Does the `admin_settings.email` row carry a credential at all? An empty secret field is not a
/// credential, so the panel can say "nothing saved" instead of guessing from the row's existence (a
/// row can hold only a provider/URL and no key).
pub fn stored_credential_present(cfg: &serde_json::Value) -> bool {
    CONFIG_SECRET_FIELDS.iter().any(|f| {
        !cfg.get(*f)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty()
    })
}

/// Is the credential in this row ENCRYPTED AT REST?
///
/// `true` only when a credential field is non-empty AND every non-empty one carries the storage
/// envelope. A value without the envelope is a legacy plaintext row — a restored dump, a manual SQL
/// write, or a key-rotation tool that updates the row directly — which this app's boot backfill
/// (`seal_legacy_config_secrets`) has not converged yet. The panel PRINTS the answer, so "saved" is
/// never read as "sealed": until 2026-10-16 the SMS panel asserted "sealed at rest" and the Provider
/// Keys panel asserted "encrypted at rest" whether or not that was true (the rule CoreSwift's panel
/// set, kanban t_d1fcda7e). The predicate is the same `provider_key_crypto::is_encrypted` the
/// seal/open halves use, so the panel cannot disagree with the storage layer.
pub fn stored_credential_sealed(cfg: &serde_json::Value) -> bool {
    let mut any = false;
    for f in CONFIG_SECRET_FIELDS {
        let v = cfg.get(f).and_then(|v| v.as_str()).unwrap_or("");
        if v.is_empty() {
            continue;
        }
        any = true;
        if !crate::security::provider_key_crypto::is_encrypted(v) {
            return false;
        }
    }
    any
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
/// The two sentences the ticket-update notice is built from.
///
/// Pure so the wording is testable without a mail transport: a resolve that MOVED the workspace
/// onto a plan says so ("your workspace is now on the Professional plan") instead of the old
/// hedge, and every other status keeps exactly the wording it had.
pub(crate) fn ticket_update_wording(status: &str, applied_plan: Option<&str>) -> (String, String) {
    let said = match (status, applied_plan) {
        (_, Some(plan)) => format!("has been resolved — your workspace is now on the {plan} plan"),
        ("pending", None) => "is being worked on by our team".to_string(),
        ("resolved", None) => "has been resolved".to_string(),
        ("closed", None) => "has been handled".to_string(),
        (_, None) => "is open again".to_string(),
    };
    let next = match (status, applied_plan) {
        ("open", None) => "We will pick it up from here.".to_string(),
        (_, Some(_)) => "You can use the new plan right away. Nothing further is needed from you, and this change charged you nothing.".to_string(),
        _ => "If that was a plan request, nothing has been charged — a plan change is made on your workspace and any upgrade is agreed with you first.".to_string(),
    };
    (said, next)
}

/// The exact subject / plain text / HTML of the failed-run notice.
///
/// Split out from the `"workflow_failed"` arm so the WORDING can be asserted without a provider:
/// a withheld (reserved-address) send never reaches the renderer, so a `format!` slip inside the arm
/// would otherwise stay invisible until a real customer's run failed.
fn workflow_failed_bodies(app_url: &str, vars: &serde_json::Value) -> (String, String, String) {
    let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
    let workflow_name = vars
        .get("workflow_name")
        .and_then(|v| v.as_str())
        .unwrap_or("your workflow");
    let run_name = vars
        .get("run_name")
        .and_then(|v| v.as_str())
        .unwrap_or("a run");
    let failure = vars.get("failure").and_then(|v| v.as_str()).unwrap_or("");
    let failed_steps = vars
        .get("failed_steps")
        .and_then(|v| v.as_i64())
        .unwrap_or(1)
        .max(1);
    let console_url = vars
        .get("console_url")
        .and_then(|v| v.as_str())
        .unwrap_or(app_url);
    let steps_word = if failed_steps == 1 { "step" } else { "steps" };
    // The subject and the plain-text part carry the raw values; only the HTML part escapes them,
    // because a workflow NAME and a run NAME are customer-supplied strings and a step's message is
    // whatever the upstream service sent back.
    let esc = |v: &str| {
        v.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let safe_name = esc(name);
    let safe_workflow = esc(workflow_name);
    let safe_run = esc(run_name);
    let safe_failure = esc(failure);
    let subject = format!("WorkflowSwift: {} failed", workflow_name);
    let html_body = format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"></head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333; margin:0;padding:0;background:#f4f4f4;">
<table style="width:100%;max-width:600px;margin:20px auto;background:#fff;border-radius:8px;overflow:hidden;">
<tr><td style="padding:30px 40px;background:linear-gradient(135deg,#dc2626,#b91c1c);text-align:center;">
<h1 style="color:#fff;font-size:26px;margin:0;">A workflow run failed</h1></td></tr>
<tr><td style="padding:40px;">
<p style="font-size:16px;color:#374151;">Hello <strong>{safe_name}</strong>,</p>
<p style="font-size:16px;color:#374151;">Your workflow <strong>{safe_workflow}</strong> failed on its latest run (<strong>{safe_run}</strong>) &mdash; {failed_steps} {steps_word} did not complete.</p>
<div style="background:#fef2f2;padding:18px;border-radius:8px;margin:24px 0;border-left:4px solid #dc2626;">
<p style="margin:0;font-size:14px;color:#7f1d1d;"><strong>What the run reported:</strong></p>
<p style="margin:8px 0 0;font-size:14px;color:#111827;font-family:monospace;word-break:break-word;">{safe_failure}</p></div>
<table style="margin:26px auto;"><tr><td style="background:#dc2626;border-radius:6px;text-align:center;">
<a href="{console_url}" style="display:inline-block;padding:14px 36px;color:#fff;text-decoration:none;font-size:16px;font-weight:bold;">Open my run list</a>
</td></tr></table>
<p style="font-size:14px;color:#374151;">Your other workflows keep running &mdash; this notice exists so a stopped automation does not go unnoticed.</p>
<p style="font-size:14px;color:#9ca3af;text-align:center;margin-top:30px;">- The WorkflowSwift Team</p>
</td></tr></table></body></html>"#,
        safe_name = safe_name,
        safe_workflow = safe_workflow,
        safe_run = safe_run,
        failed_steps = failed_steps,
        steps_word = steps_word,
        safe_failure = safe_failure,
        console_url = console_url
    );
    let text_body = format!(
        "Hello {name},\n\nYour workflow '{workflow_name}' failed on its latest run ('{run_name}') - {failed_steps} {steps_word} did not complete.\n\nWhat the run reported:\n{failure}\n\nOpen your run list: {console_url}\n\nYour other workflows keep running - this notice exists so a stopped automation does not go unnoticed.\n\n- WorkflowSwift"
    );
    (subject, text_body, html_body)
}

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
            // The customer must be TOLD where to enter the code (fixed 2026-10-13, cron `default`):
            // this mail used to hand over a 36-character code with no destination at all, so someone
            // who had forgotten their password had to guess where the reset form lives.
            // CORRECTED 2026-10-16 (cron `default`): the destination it was given was `/login`, which
            // has NO code field -- its "Forgot password?" link only mails ANOTHER code. The page that
            // accepts the code is `/reset-password` (input id="t", prefilled from `?token=`), so that
            // is what the mail links to now, and the expiry it states is the 24h the server mints
            // (`auth::handlers::forgot_password`) rather than a 1-hour promise it never kept.
            let app_url = vars
                .get("app_url")
                .and_then(|v| v.as_str())
                .unwrap_or("https://app.workflowswift.com");

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
<p style="font-size:14px;color:#374151;">Open <a href="{app_url}/reset-password?token={token}" style="color:#dc2626;">{app_url}/reset-password</a> and choose a new password &mdash; the reset code is filled in for you.</p>
<p style="font-size:14px;color:#374151;">If that link does not open the form, go to <strong>{app_url}/reset-password</strong> and paste the code above.</p>
<p style="font-size:14px;color:#6b7280;">The code expires in <strong>24 hours</strong>.</p>
<p style="font-size:14px;color:#9ca3af;margin-top:25px;">If you did not request this, ignore this email.</p>
<p style="font-size:14px;color:#9ca3af;text-align:center;margin-top:30px;">- The WorkflowSwift Team</p>
</td></tr></table></body></html>"#,
                token = token,
                app_url = app_url
            );

            let text_body = format!(
                "Your password reset code is: {}\n\nOpen {}/reset-password?token={} to choose a new password - the code is filled in for you. If the form does not open, go to {}/reset-password and paste the code above.\n\nThis code expires in 24 hours.\n\nIf you did not request this password reset, please ignore this email.\n\n- WorkflowSwift",
                token, app_url, token, app_url
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
        // The sign-in ADDRESS CHANGE's confirmation (cron 2026-10-18). The link it carries is the
        // whole point of the mail — `confirm-email-change?token=…` — and the token is single-use
        // and lives 2 hours (auth::handlers::request_email_change). Without this arm an
        // `email_change` send would fall to the generic `_` arm below, which prints the raw vars
        // and no link: the customer would be told to check a mailbox that has nothing actionable.
        "email_change" => {
            let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
            let new_email = vars.get("new_email").and_then(|v| v.as_str()).unwrap_or("");
            let link = vars.get("link").and_then(|v| v.as_str()).unwrap_or(app_url);
            let html_body = format!(
                r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"></head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333; margin:0;padding:0;background:#f4f4f4;">
<table style="width:100%;max-width:600px;margin:20px auto;background:#fff;border-radius:8px;overflow:hidden;">
<tr><td style="padding:30px 40px;background:linear-gradient(135deg,#6366f1,#4f46e5);text-align:center;">
<h1 style="color:#fff;font-size:26px;margin:0;">Confirm your new sign-in address</h1></td></tr>
<tr><td style="padding:40px;">
<p style="font-size:16px;color:#374151;">Hello <strong>{name}</strong>,</p>
<p style="font-size:16px;color:#374151;">We were asked to make <strong>{new_email}</strong> the sign-in address for your WorkflowSwift account.</p>
<table style="margin:28px auto;"><tr><td style="background:#6366f1;border-radius:6px;text-align:center;">
<a href="{link}" style="display:inline-block;padding:14px 36px;color:#fff;text-decoration:none;font-size:16px;font-weight:bold;">Confirm this address</a>
</td></tr></table>
<p style="font-size:14px;color:#374151;">Your current address keeps working until you press that button. The link expires in <strong>2 hours</strong> and can be used once.</p>
<p style="font-size:14px;color:#9ca3af;margin-top:25px;">If you did not ask for this, ignore this email &mdash; nothing has changed.</p>
<p style="font-size:14px;color:#9ca3af;text-align:center;margin-top:30px;">- The WorkflowSwift Team</p>
</td></tr></table></body></html>"#,
                name = name,
                new_email = new_email,
                link = link
            );
            let text_body = format!(
                "Hello {},\n\nWe were asked to make {} the sign-in address for your WorkflowSwift account.\n\nOpen this link to confirm it:\n{}\n\nYour current address keeps working until you confirm. The link expires in 2 hours and can be used once.\n\nIf you did not ask for this, ignore this email - nothing has changed.\n\n- WorkflowSwift",
                name, new_email, link
            );
            send_branded(
                cfg,
                to,
                "Confirm your new WorkflowSwift sign-in address",
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
        // The OWNER's notice that a run FAILED (cron 2026-11-10). Before this arm existed a failed
        // run was a row in the console's run list and nothing else, so an automation that stopped
        // delivering was indistinguishable from one that had simply not been needed.
        "workflow_failed" => {
            let (subject, text_body, html_body) = workflow_failed_bodies(app_url, vars);
            send_branded(cfg, to, &subject, &text_body, &html_body, branding).await
        }
        // The CUSTOMER's notice that their own request moved (2026-10-19). A plan request is filed
        // as a real ticket from the dashboard's Plan & Billing screen (`source='plan_request'`) and
        // reaches the operator's cross-tenant queue — and until this arm existed that was the end of
        // it: the operator pressed "Mark handled" and the person who asked was never told. Wording is
        // per status, and the plan-request sentence never promises a charge (nothing is charged when
        // a request is filed; a plan change is agreed with the customer first).
        "ticket_update" => {
            let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
            let ticket_subject = vars
                .get("ticket_subject")
                .and_then(|v| v.as_str())
                .unwrap_or("your request");
            let status = vars
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("updated");
            let app_url = vars
                .get("app_url")
                .and_then(|v| v.as_str())
                .unwrap_or("https://app.workflowswift.com");
            // A resolve that MOVED the workspace onto the plan states the outcome. Until this the
            // notice always hedged ("If that was a plan request, nothing has been charged — a plan
            // change is made on your workspace…") even when the plan had just been applied, which
            // is the same silent-outcome shape the operator side had (measured live 2026-10-21).
            // `plan_name` is present ONLY when this resolve applied one, so a plain ticket keeps
            // its old wording.
            let applied_plan = vars.get("plan_name").and_then(|v| v.as_str());
            let (said, next) = ticket_update_wording(status, applied_plan);
            let safe_subject = ticket_subject.replace('<', "&lt;").replace('>', "&gt;");
            // The operator's WRITTEN ANSWER (2026-11-06). A status word told the asker *that*
            // something happened, never *what* was decided — and a visitor who filed from the
            // contact page has no dashboard to sign in to, so this body is the only place they can
            // read it. A ticket moved without an answer keeps its old wording exactly.
            let reply = vars
                .get("reply")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let reply_text = match reply {
                Some(r) => format!("\n\nOur answer:\n\n{r}\n"),
                None => String::new(),
            };
            let reply_html = match reply {
                Some(r) => format!(
                    "<div style=\"background:#f0fdf4;border-left:3px solid #22c55e;padding:12px 14px;border-radius:6px;margin:18px 0\"><div style=\"font-size:13px;color:#166534;margin-bottom:4px\">Our answer</div><div style=\"white-space:pre-wrap\">{}</div></div>",
                    r.replace('<', "&lt;").replace('>', "&gt;")
                ),
                None => String::new(),
            };
            // A visitor with no workspace cannot open /dashboard, so the footer tells them where
            // their answer actually lives.
            let foot = if vars
                .get("public_visitor")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                format!("You can read this answer again, and file another message, at {app_url}/contact.html")
            } else {
                format!("You can see it any time at {app_url}/dashboard (Support).")
            };
            let text_body = format!(
                "Hi {name},\n\nYour WorkflowSwift request \"{ticket_subject}\" {said}.\n\n{next}\n{reply_text}\n{foot}\n\n- WorkflowSwift"
            );
            let html_body = format!(
                "<html><body style=\"font-family:Arial,sans-serif;line-height:1.6;color:#333\">\
                 <p>Hi {name},</p>\
                 <p>Your WorkflowSwift request <strong>{safe_subject}</strong> {said}.</p>\
                 <p style=\"color:#374151\">{next}</p>{reply_html}\
                 <p><a href=\"{app_url}/dashboard\" style=\"color:#4f46e5\">Open your dashboard</a></p>\
                 <p style=\"font-size:13px;color:#9ca3af\">- WorkflowSwift</p></body></html>"
            );
            send_branded(
                cfg,
                to,
                "Update on your WorkflowSwift request",
                &text_body,
                &html_body,
                branding,
            )
            .await
        }
        // A PERSON MOVED THE CUSTOMER ONTO A PLAN (kanban t_e12d2e73). Self-serve checkout needs a
        // payment provider and no install in this fleet has one yet, so nearly every paid seat is
        // given BY HAND from the operator console. That switch used to be silent: the tier changed
        // and the customer's only clue was a screen whose allowances had moved. Nothing is charged,
        // so the body says so, and it points at the plan screen rather than the marketing site.
        "plan_changed" => {
            let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
            let plan_name = vars
                .get("plan_name")
                .and_then(|v| v.as_str())
                .unwrap_or("your new plan");
            let subject = plan_changed_subject(plan_name);
            let text_body = format!(
                "Hi {name},\n\nYour workspace has been moved onto the {plan_name} plan. Our team made this change for you, and nothing was charged to your card for it.\n\nOpen {app_url}/dashboard and choose Plan & Billing to see exactly what the plan includes.\n\nIf you expected a different plan, reply to this message and we will put it right.\n\n- WorkflowSwift"
            );
            let html_body = format!(
                "<html><body style=\"font-family:Arial,sans-serif;line-height:1.6;color:#333\">\
                 <p>Hi {name},</p>\
                 <p>Your workspace has been moved onto the <strong>{plan_name}</strong> plan. Our team made this change for you, and <strong>nothing was charged to your card for it</strong>.</p>\
                 <p>Open Plan &amp; Billing on your dashboard to see exactly what the plan includes — and when it renews.</p>\
                 <p><a href=\"{app_url}/dashboard\" style=\"color:#4f46e5\">Open your dashboard</a></p>\
                 <p style=\"font-size:13px;color:#9ca3af\">If you expected a different plan, reply to this message and we will put it right.<br>- WorkflowSwift</p></body></html>"
            );
            send_branded(cfg, to, &subject, &text_body, &html_body, branding).await
        }
        // The receipt for a filed plan request (cron 2026-10-20). `POST /api/v1/tickets` with
        // source='plan_request' is what the console's Plans section files while checkout is closed,
        // so it is the only way a customer can ask to buy — and until this arm existed the row was
        // written while the customer's inbox stayed empty. Reached whenever the account has no
        // `plan_request_receipt` template row, which is the normal state on a fresh install.
        "plan_request_receipt" => {
            let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
            let request_subject = vars
                .get("ticket_subject")
                .and_then(|v| v.as_str())
                .unwrap_or("your request");
            let app_url = vars
                .get("app_url")
                .and_then(|v| v.as_str())
                .unwrap_or("https://app.workflowswift.com");
            let safe_subject = request_subject.replace('<', "&lt;").replace('>', "&gt;");
            let text_body = format!(
                "Hi {name},\n\nThanks — we have your request:\n\n    {request_subject}\n\nNothing has been charged. A person (not a robot) reads it and answers you by email, and you can see where it stands under Support on your dashboard: {app_url}/dashboard\n\nIf you would rather not go ahead, reply to this message and we will close it.\n\n- WorkflowSwift"
            );
            let html_body = format!(
                "<html><body style=\"font-family:Arial,sans-serif;line-height:1.6;color:#333\">\
                 <p>Hi {name},</p>\
                 <p>Thanks — we have your request:</p>\
                 <p style=\"background:#f3f4f6;padding:12px 16px;border-radius:8px;border-left:4px solid #4f46e5;color:#111827\"><strong>{safe_subject}</strong></p>\
                 <p style=\"color:#374151\"><strong>Nothing has been charged.</strong> A person (not a robot) reads it and answers you by email, and you can see where it stands under Support on your dashboard.</p>\
                 <p><a href=\"{app_url}/dashboard\" style=\"color:#4f46e5\">Open your dashboard</a></p>\
                 <p style=\"font-size:13px;color:#9ca3af\">If you would rather not go ahead, reply to this message and we will close it.<br>- WorkflowSwift</p></body></html>"
            );
            send_branded(
                cfg,
                to,
                "We have your plan request",
                &text_body,
                &html_body,
                branding,
            )
            .await
        }
        // The OPERATOR's copy of the same event (2026-11-09). `POST /api/v1/tickets` with
        // source='plan_request' (and the Upgrade button's refused checkout) already mails the
        // customer a receipt; nothing told the person who can actually move the row. On an
        // installation with no payment provider the request IS the buying path, so an ask nobody is
        // told about is a sale nobody makes. Destination is the operator's own address, stored in
        // `admin_settings.billing.operator_alert_email` (Admin -> Platform Settings); unset sends
        // nothing and this code never invents a mailbox.
        "plan_request_operator_alert" => {
            let workspace = vars
                .get("workspace")
                .and_then(|v| v.as_str())
                .unwrap_or("A workspace");
            let plan = vars
                .get("plan")
                .and_then(|v| v.as_str())
                .unwrap_or("a plan");
            let requester = vars
                .get("requester")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            let admin_url = vars
                .get("admin_url")
                .and_then(|v| v.as_str())
                .unwrap_or("https://admin.workflowswift.com");
            let text_body = format!(
                "{workspace} asked to be moved onto {plan}.\n\nRequested by: {requester}\nPlan: {plan}\n\nThe request is waiting in the admin console (Plans -> Plan requests). Moving it to done puts that workspace on the plan: {admin_url}\n\n- WorkflowSwift"
            );
            let html_body = format!(
                "<html><body style=\"font-family:Arial,sans-serif;line-height:1.6;color:#333\">\
                 <p><strong>{workspace}</strong> asked to be moved onto <strong>{plan}</strong>.</p>\
                 <p>Requested by: <strong>{requester}</strong></p>\
                 <p>The request is waiting in the admin console (Plans &rarr; Plan requests). Moving it to <em>done</em> puts that workspace on the plan.</p>\
                 <p><a href=\"{admin_url}\" style=\"color:#4f46e5\">Open the admin console</a></p>\
                 <p style=\"font-size:13px;color:#9ca3af\">Nothing has been charged. This message is sent once per request.<br>- WorkflowSwift</p></body></html>"
            );
            let subject = format!("A customer asked to buy {plan}");
            send_branded(cfg, to, &subject, &text_body, &html_body, branding).await
        }
        // The OPERATOR's copy of an ORDINARY help request (2026-11-09) — reached when no stored
        // template row exists, which is true of every install today. Measured live on the sibling
        // app: the contact-page door answered 201 "Your message is with our team" and queued ZERO
        // mail, and the signed-in door alerted only for a PLAN request — so "I cannot sign in"
        // waited in a console nobody had been told to open.
        "support_request_operator_alert" => {
            let who = vars
                .get("who")
                .and_then(|v| v.as_str())
                .unwrap_or("A customer");
            let reply_to = vars
                .get("reply_to")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            let ask = vars
                .get("subject_line")
                .and_then(|v| v.as_str())
                .unwrap_or("a support request");
            let message = vars.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let admin_url = vars
                .get("admin_url")
                .and_then(|v| v.as_str())
                .unwrap_or("https://admin.workflowswift.com");
            let esc = |s: &str| {
                s.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;")
            };
            let notice = format!(
                "{who} wrote to support.\n\nSubject: {ask}\nReply to: {reply_to}\n\n{message}\n\n                 Answer it in the admin console (Support -> Tickets) and the asker is mailed your                  reply: {admin_url}\n\n- WorkflowSwift"
            );
            let html_body = format!(
                "<html><body style=\"font-family:Arial,sans-serif;line-height:1.6;color:#333\">\
                 <p><strong>{}</strong> wrote to support.</p>\
                 <p>Subject: <strong>{}</strong></p>\
                 <p>Reply to: <strong>{}</strong></p>\
                 <pre style=\"white-space:pre-wrap;font-family:inherit\">{}</pre>\
                 <p>Answer it in the admin console (Support &rarr; Tickets) and the asker is mailed                  your reply.</p>\
                 <p><a href=\"{}\" style=\"color:#4f46e5\">Open the admin console</a></p>\
                 <p style=\"font-size:13px;color:#9ca3af\">- WorkflowSwift</p></body></html>",
                esc(who),
                esc(ask),
                esc(reply_to),
                esc(message),
                admin_url,
            );
            let subject = format!("A support request needs an answer: {ask}");
            send_branded(cfg, to, &subject, &notice, &html_body, branding).await
        }
        // The OPERATOR's copy of a NEW SIGNUP (2026-11-09, parity with FunnelSwift) — reached when
        // no stored template row exists, which is true of every install today. Until this arm
        // existed a new customer arrived in complete silence: the account, the plan and the
        // credentials mail were all written, and the only place the arrival appeared was the
        // console the operator had not been told to open.
        "signup_operator_alert" => {
            let workspace = vars
                .get("workspace")
                .and_then(|v| v.as_str())
                .unwrap_or("A workspace");
            let who = vars
                .get("who")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            let plan = vars
                .get("plan")
                .and_then(|v| v.as_str())
                .unwrap_or("no plan attached");
            let source = vars
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let admin_url = vars
                .get("admin_url")
                .and_then(|v| v.as_str())
                .unwrap_or("https://admin.workflowswift.com");
            let esc = |s: &str| {
                s.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;")
            };
            let text_body = format!(
                "A new workspace signed up.\n\nWorkspace: {workspace}\nOwner: {who}\nPlan: {plan}\nSource: {source}\n\nTheir login credentials were emailed to the owner. The workspace is in the admin console (Accounts) and on the dashboard: {admin_url}\n\n- WorkflowSwift"
            );
            let html_body = format!(
                "<html><body style=\"font-family:Arial,sans-serif;line-height:1.6;color:#333\">\
                 <p>A new workspace signed up.</p>\
                 <p><strong>{}</strong><br>\
                 Owner: {}<br>\
                 Plan: {}<br>\
                 Source: {}</p>\
                 <p>Their login credentials were emailed to the owner. The workspace is in the admin console (Accounts) and on the dashboard.</p>\
                 <p><a href=\"{}\" style=\"color:#4f46e5\">Open the admin console</a></p>\
                 <p style=\"font-size:13px;color:#9ca3af\">- WorkflowSwift</p></body></html>",
                esc(workspace),
                esc(who),
                esc(plan),
                esc(source),
                admin_url,
            );
            let subject = format!("New workspace: {workspace}");
            send_branded(cfg, to, &subject, &text_body, &html_body, branding).await
        }
        // The operator's diagnostic from Admin > Settings > Email (cron 2026-11-10, kanban card
        // raised by the live log line below). `email_test` is DECLARED in the template catalog as
        // "The test message from Admin > Settings > Email", but had no branch here — so pressing
        // "Send test email" fell through to the generic arm and the operator received
        // "You have a new notification from WorkflowSwift", which reads like real traffic and
        // proves nothing about the delivery it exists to prove. Measured in the running container:
        //   WARN ... send_email_fallback: no inline branch for this template_type
        //        template_type="email_test"
        // The message now says what it is, that the saved provider accepted it, and that no action
        // follows. `name` is bound by the caller (the handler always sends "Admin").
        "email_test" => {
            let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
            let name = name
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            let sent_at = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
            let subject = format!("WorkflowSwift test email — delivery confirmed ({sent_at})");
            let text_body = format!(
                "Hi {name},\n\nThis is a TEST email from WorkflowSwift, sent at {sent_at}.\n\n\
                 If you are reading it, the mail settings saved in Admin > Settings > Email are \
                 working: WorkflowSwift handed this message to your mail provider and the provider \
                 accepted it for delivery to this address.\n\n\
                 Nothing else was sent, and there is nothing for you to do.\n\n- WorkflowSwift"
            );
            let html_body = format!(
                "<html><body style=\"font-family:Arial,sans-serif;line-height:1.6;color:#333\">\
                 <p>Hi {name},</p>\
                 <p>This is a <strong>TEST email</strong> from WorkflowSwift, sent at {sent_at}.</p>\
                 <p>If you are reading it, the mail settings saved in <strong>Admin &gt; Settings \
                 &gt; Email</strong> are working: WorkflowSwift handed this message to your mail \
                 provider, and the provider accepted it for delivery to this address.</p>\
                 <p>Nothing else was sent, and there is nothing for you to do.</p>\
                 <p style=\"font-size:13px;color:#9ca3af\">- WorkflowSwift</p></body></html>"
            );
            send_branded(cfg, to, &subject, &text_body, &html_body, branding).await
        }
        // The operator's own branding PREVIEW (cron 2026-10-24, parity with FunnelSwift,
        // IncentiveSwift and ADASwift). Reached whenever the account has no `branding_preview`
        // template row, which is the normal state on a fresh install — the sample must render
        // anyway. The header above this body is the account's OWN branding when they have one, and
        // this app's own wordmark when they do not (kanban t_081edaac) — so the body says which,
        // instead of describing a header that is not there.
        "branding_preview" => {
            let (subject, text_body, html_body) = branding_preview_inline(vars);
            send_branded(cfg, to, &subject, &text_body, &html_body, branding).await
        }
        // THE STORAGE RUNG REMINDER (kanban t_fb46239b; parity with CoreSwift's B206 arm). This app
        // carries NO `storage_warning` row in `email_templates`, so without this branch the 50/75/99
        // mail that `evaluate_and_notify` claims each month reached the customer as the generic
        // "You have a new notification" — for the one message whose entire value is its numbers.
        // Plain sentences on purpose: the subject names the real percentage because a customer reads
        // that first, and the body NEVER says mail will be deleted to make room, because keeping
        // that promise is what the whole storage rule exists for.
        "storage_warning" => {
            let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
            let used = vars.get("used").and_then(|v| v.as_str()).unwrap_or("some");
            let quota = vars
                .get("quota")
                .and_then(|v| v.as_str())
                .unwrap_or("your plan allowance");
            let percent = vars.get("percent").and_then(|v| v.as_i64()).unwrap_or(0);
            let threshold = vars.get("threshold").and_then(|v| v.as_i64()).unwrap_or(50);
            let inbox_url = vars
                .get("inbox_url")
                .and_then(|v| v.as_str())
                .unwrap_or(APP_URL_MAIL);
            let plans_url = vars
                .get("plans_url")
                .and_then(|v| v.as_str())
                .unwrap_or(app_url);
            let retention = vars.get("retention").and_then(|v| v.as_str()).unwrap_or("");
            let subject = format!("Your mailbox is {percent}% full — {used} of {quota} used");
            let action = if percent >= 100 {
                "Your mailbox is full. Mail is still arriving and nothing has been deleted, but new \
                 attachments and files are paused until you clear space or upgrade."
            } else if threshold >= 99 {
                "Your mailbox is almost full. Plain mail still arrives and nothing is deleted, but \
                 once the allowance is reached new attachments and files are paused."
            } else {
                "You have room for now. Clearing out mail you no longer need (Trash and Spam count \
                 too) keeps the meter down, and a bigger plan raises the allowance."
            };
            let text_body = format!(
                "Hi {name},\n\nThis is a storage update for your mailbox: you are using {used} of \
                 {quota} ({percent}%).\n\n{action}\n\nTwo things that are ALWAYS true here: mail sent \
                 to you keeps arriving, and nothing is deleted to make room for new mail. The only \
                 thing a full mailbox pauses is adding new attachments or files.\n\nOpen your mail and \
                 clear some space:\n    {inbox_url}\n\nSee the plans with more storage:\n    \
                 {plans_url}\n\n{retention}\n\nYou will get at most one of these messages per storage \
                 level per month.\n\n- The WorkflowSwift Team"
            );
            let esc = |s: &str| {
                s.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
            };
            let safe_name = esc(name);
            let safe_action = esc(action);
            let safe_retention = esc(retention);
            let html_body = format!(
                "<html><body style=\"font-family:Arial,sans-serif;line-height:1.6;color:#333\">\
                 <p>Hi {safe_name},</p>\
                 <p>This is a storage update for your mailbox: you are using <strong>{used} of {quota} ({percent}%)</strong>.</p>\
                 <p>{safe_action}</p>\
                 <p>Two things that are <strong>always</strong> true here: mail sent to you keeps arriving, and <strong>nothing is deleted</strong> to make room for new mail. The only thing a full mailbox pauses is adding new attachments or files.</p>\
                 <p><a href=\"{inbox_url}\" style=\"color:#4f46e5\">Open your mail and clear some space</a></p>\
                 <p><a href=\"{plans_url}\" style=\"color:#4f46e5\">See the plans with more storage</a></p>\
                 <p style=\"font-size:13px;color:#9ca3af\">{safe_retention}<br>You will get at most one of these messages per storage level per month.<br>- The WorkflowSwift Team</p></body></html>"
            );
            send_branded(cfg, to, &subject, &text_body, &html_body, branding).await
        }
        _ => {
            // An unknown template_type must never mail the serialised variables: `vars` holds
            // whatever the caller passed — passwords, reset tokens, ticket bodies — and the old arm
            // shipped exactly that as the body. Keep the subject (it is the marker ops greps for),
            // send a readable generic notice, and WARN with the type so a missing branch is visible
            // instead of silently mailing a variable dump (the same class fixed on ADASwift,
            // t_3217c548).
            tracing::warn!(
                template_type = template_type,
                "send_email_fallback: no inline branch for this template_type — sending the generic \
                 notification instead of a variable dump"
            );
            let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
            let text_body = format!(
                "Hi {name},\n\nYou have a new notification from WorkflowSwift.\n\n\
                 Sign in to see it: {}\n\n- WorkflowSwift",
                crate::branding::APP_URL
            );
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

/// The `branding_preview` sample built inline, used when the account has no template row of
/// its own (the normal state on a fresh install). Returns `(subject, text, html)` so the SEND
/// path ([`send_email_fallback`]) and the RENDER path ([`render_branding_preview`]) share ONE
/// copy of the wording: a preview that drew its own sample could describe a mail the sender
/// never emits, which is exactly the defect the exact-mail arm exists to close.
fn branding_preview_inline(vars: &serde_json::Value) -> (String, String, String) {
    let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
    let app_url = vars
        .get("app_url")
        .and_then(|v| v.as_str())
        .unwrap_or(crate::branding::APP_URL);
    // `logo_url` is ALWAYS bound now (this app's wordmark is the fallback), so the copy
    // reads the owner flag rather than testing the URL: an upload the account never made
    // must not be described as theirs, and a header this app supplied must not be called
    // "YOUR branding".
    let (header_line, html_line) = if vars.get("logo_is_own").and_then(|v| v.as_str())
        == Some("yes")
    {
        (
            "The header above this line is YOUR branding: your logo, your display name and your colour. If it is right here, it is right on every notification and account email your team receives.",
            "The header above this line is <strong>your</strong> branding: your logo, your display name and your colour. If it is right here, it is right on every notification and account email your team receives.",
        )
    } else {
        (
            "No logo of your own is set yet, so this message carries the WorkflowSwift wordmark and name. Upload a logo on your console's Email Branding card, save it, and send yourself another preview to see your own.",
            "No logo of your own is set yet, so this message carries the <strong>WorkflowSwift</strong> wordmark and name. Upload a logo on your console's Email Branding card, save it, and send yourself another preview to see your own.",
        )
    };
    let text_body = format!(
        "Hi {name},\n\nThis is a sample of the mail WorkflowSwift sends on your behalf.\n\n{header_line}\n\nChange it any time in your console, under Profile & Account > Email Branding: {app_url}\n\n- WorkflowSwift"
    );
    let html_body = format!(
        "<html><body style=\"font-family:Arial,sans-serif;line-height:1.6;color:#333\">\
         <p>Hi {name},</p>\
         <p>This is a sample of the mail WorkflowSwift sends on your behalf.</p>\
         <p style=\"color:#374151\">{html_line}</p>\
         <p style=\"color:#374151\">Change it any time in your console, under <strong>Profile &amp; Account &gt; Email Branding</strong>: <a href=\"{app_url}\" style=\"color:#4f46e5\">{app_url}</a></p>\
         <p style=\"font-size:13px;color:#9ca3af\">- WorkflowSwift</p></body></html>"
    );
    (
        "A preview of your WorkflowSwift email branding".to_string(),
        text_body,
        html_body,
    )
}

/// The mail this account's branding produces for the `branding_preview` sample, RENDERED and
/// returned instead of sent (cron 2026-10-28, parity with FunnelSwift, IncentiveSwift and ADASwift).
///
/// WHY: the console's Email Branding card draws its sample in the browser, so it can describe a
/// header the mailer never emits. This answers with the bytes the REAL path produces — the same
/// template lookup, the same [`render_template`] merge and the same [`apply_branding`] header — so
/// what an operator sees is what a recipient receives. Nothing is mailed and no transport config is
/// read: an install with no provider set can still see what its mail will look like.
pub struct BrandingPreview {
    pub subject: String,
    pub text: String,
    pub html: String,
    /// The account has a stored branding document of its own (vs this app's identity standing in).
    pub branded: bool,
    /// The logo above the message is the account's OWN upload, not this app's wordmark.
    pub own_logo: bool,
}

pub async fn render_branding_preview(
    state: &AppState,
    aid: Uuid,
    to: &str,
    name: &str,
) -> Result<BrandingPreview, String> {
    // The SAME two steps send_email_inner takes before it sends, in the same order.
    let stored_branding = crate::branding::load(&state.db, aid).await;
    let branding = stored_branding.clone().unwrap_or_else(app_default_branding);
    let mut vars = json!({
        "name": name,
        "email": to,
        "app_url": crate::branding::APP_URL,
    });
    bind_branding(&mut vars, stored_branding.as_ref());

    let template = sqlx::query_as::<_, EmailTemplateRow>(
        r#"SELECT id, name, subject, body, html_body, is_html, is_default
           FROM email_templates
           WHERE template_type = $1 AND (is_default = true OR is_default IS NULL)
           ORDER BY is_default DESC, created_at DESC
           LIMIT 1"#,
    )
    .bind("branding_preview")
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let (subject, text_body, html_body) = match template {
        Some(t) => {
            let subject = render_template(
                &t.subject
                    .unwrap_or_else(|| "A preview of your WorkflowSwift email branding".into()),
                &vars,
                "subject",
            );
            let html_body = t
                .html_body
                .as_ref()
                .map(|h| render_template(h, &vars, "html_body"))
                .unwrap_or_default();
            let text_body = render_template(&t.body.unwrap_or_default(), &vars, "text_body");
            let use_html = t.is_html.unwrap_or(true);
            (
                subject,
                text_body,
                if use_html { html_body } else { String::new() },
            )
        }
        None => branding_preview_inline(&vars),
    };

    let (text, html) = apply_branding(Some(&branding), text_body, html_body);
    let own_logo = stored_branding
        .as_ref()
        .and_then(|b| b.resolve_logo_url(crate::branding::APP_URL))
        .is_some();
    Ok(BrandingPreview {
        subject,
        text,
        html,
        branded: stored_branding.is_some(),
        own_logo,
    })
}

/// The operator's own in-inbox proof that their branding is applied (cron 2026-10-24, parity with
/// FunnelSwift, IncentiveSwift and ADASwift). Recipient is ALWAYS the caller's own address — this is
/// a preview, never a relay — and a reserved/harness address is withheld by the one suppression arm
/// every send already passes through, then recorded as a non-send for the operator to see.
///
/// It goes through [`send_email`], the SAME door a customer's mail takes, precisely because that is
/// what makes the sample trustworthy: one template lookup, one branding header, one transport.
pub async fn send_branding_preview_email(
    state: &AppState,
    aid: Uuid,
    to: &str,
    name: &str,
) -> Result<(), String> {
    let vars = json!({
        "name": name,
        "email": to,
        "app_url": crate::branding::APP_URL,
    });
    send_email(state, Some(aid), to, "branding_preview", &vars).await
}

/// Tell the ACCOUNT OWNER their workflow run FAILED (cron 2026-11-10).
///
/// The run row already said `failed` and the console's run list showed it, but nothing reached a
/// human: a customer whose automation had stopped delivering had to go looking. The recipient is
/// the account's own owner (`users`, owner/company_admin first), never a free-text address, and the
/// message goes through [`send_email`] — the one door every customer mail takes — so a
/// reserved/harness address is withheld at source and recorded as a non-send instead of bouncing
/// against the sending domain's reputation.
///
/// Best effort: the run is already recorded, so a mail failure is logged and dropped.
pub async fn alert_run_failed(
    state: &AppState,
    aid: Uuid,
    workflow_id: Uuid,
    instance_id: Uuid,
    failed_steps: i32,
    first_failure: &str,
) {
    let owner: Option<(String, String)> = sqlx::query_as(
        "SELECT name, email FROM users WHERE aid = $1 AND is_active \
         ORDER BY (role IN ('owner','company_admin')) DESC, sort_order NULLS LAST, created_at ASC \
         LIMIT 1",
    )
    .bind(aid)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let Some((name, email)) = owner else { return };

    let workflow_name: String = sqlx::query_scalar(
        "SELECT COALESCE(NULLIF(name, ''), 'your workflow') FROM workflows WHERE id = $1",
    )
    .bind(workflow_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "your workflow".to_string());

    let run_name: String = sqlx::query_scalar(
        "SELECT COALESCE(NULLIF(name, ''), 'a run') FROM workflow_instances WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "a run".to_string());

    let vars = json!({
        "name": name,
        "email": email,
        "workflow_name": workflow_name,
        "run_name": run_name,
        "failed_steps": failed_steps,
        "failure": first_failure.chars().take(400).collect::<String>(),
        "app_url": crate::branding::APP_URL,
        "console_url": format!("{}/#instances", crate::branding::APP_URL),
    });
    if let Err(e) = send_email(state, Some(aid), &email, "workflow_failed", &vars).await {
        tracing::warn!(%instance_id, to = %email, "failed-run notice could not be sent: {e}");
    }
}

/// Tell the OPERATOR that somebody wrote to support (2026-11-09).
///
/// WHY THIS EXISTS (measured live on the sibling app the same day): `POST /api/v1/public/support-request`
/// — the contact page's form, the door a LOCKED-OUT customer has — answers 201 "Your message is with
/// our team" and queued ZERO mail, while the signed-in door alerted the operator only for a PLAN
/// request. Any ordinary "I cannot sign in" ticket therefore waited in a console nobody had been
/// told to open. FunnelSwift closed the identical gap at 10:59 that morning.
///
/// Destination = the operator's own `operator_alert_email`, stored beside the system mail settings
/// (`admin_settings.billing.operator_alert_email`, Admin -> Platform Settings). An unset address
/// sends nothing: this code never invents a mailbox. Sent through the SYSTEM configuration (`None`),
/// never the asking workspace's, so it arrives even when that workspace's own mail is the broken
/// thing. Best effort — the row is the record, so a failure never turns a filed request into a 500.
pub async fn send_support_request_operator_alert(
    state: &AppState,
    who: &str,
    reply_to: &str,
    subject_line: &str,
    message: &str,
) -> bool {
    let billing: Option<serde_json::Value> =
        match sqlx::query_scalar("SELECT value FROM admin_settings WHERE key = 'billing'")
            .fetch_optional(&state.db)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("support operator alert: could not read the billing settings: {e}");
                return false;
            }
        };
    let Some(to) = billing
        .as_ref()
        .and_then(|v| v.get("operator_alert_email"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        // Nothing configured is a normal state, not a failure: no mailbox is invented.
        return false;
    };
    // A FLEET PROBE MUST NEVER RING THE OPERATOR (kanban t_f17b5802). Requester, not route.
    if let Some(reason) = crate::security::probe_addr::synthetic_requester(reply_to, who) {
        tracing::info!(
            reason = %reason,
            "support operator alert WITHHELD — the filer is a synthetic identity"
        );
        return false;
    }
    let vars = json!({
        "who": if who.trim().is_empty() { "A customer" } else { who.trim() },
        "reply_to": reply_to,
        "subject_line": subject_line,
        "message": message,
        "admin_url": "https://admin.workflowswift.com",
    });
    match send_email(state, None, &to, "support_request_operator_alert", &vars).await {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("support operator alert to {to} failed: {e}");
            false
        }
    }
}

/// Tell the OPERATOR that a workspace asked to buy a plan (2026-11-09).
///
/// The customer already gets a receipt (see `notify_plan_request_receipt` in the tickets handler);
/// this is the other half of the same event — until it existed, nothing told the person who can
/// actually move the request row. On an installation with no payment provider configured the
/// request IS the buying path, so an ask nobody is told about is a sale nobody makes (the same gap
/// FunnelSwift closed on 2026-11-09).
///
/// The destination is the operator's own address, stored beside the billing settings
/// (`admin_settings.billing.operator_alert_email`, set from Admin -> Platform Settings). An unset
/// address sends nothing — this code never invents a mailbox. Sent through the SYSTEM mail
/// configuration (`None`), not the asking workspace's, so it arrives even when that workspace's own
/// mail server is the thing that is broken.
///
/// Best effort by design: the request row is already written and the console's queue reads it, so a
/// mail failure must never turn a recorded request into an error. Returns whether a send was
/// attempted and queued, which is what the caller logs.
pub async fn send_plan_request_operator_alert(
    state: &AppState,
    aid: Uuid,
    subject: &str,
    requester_email: &str,
) -> bool {
    let billing: Option<serde_json::Value> =
        match sqlx::query_scalar("SELECT value FROM admin_settings WHERE key = 'billing'")
            .fetch_optional(&state.db)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("operator alert: could not read the billing settings: {e}");
                return false;
            }
        };
    let Some(to) = billing
        .as_ref()
        .and_then(|v| v.get("operator_alert_email"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        // Nothing configured is a normal state, not a failure: no mailbox is invented.
        return false;
    };
    let workspace: String = sqlx::query_scalar("SELECT name FROM accounts WHERE id = $1")
        .bind(aid)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "A workspace".to_string());
    // A FLEET PROBE MUST NEVER RING THE OPERATOR (kanban t_f17b5802). Requester, not route.
    if let Some(reason) =
        crate::security::probe_addr::synthetic_requester(requester_email, &workspace)
    {
        tracing::info!(
            reason = %reason,
            "plan-request operator alert WITHHELD — the requester is a synthetic identity"
        );
        return false;
    }
    let vars = json!({
        "workspace": workspace,
        "plan": subject,
        "requester": requester_email,
        "admin_url": "https://admin.workflowswift.com",
    });
    match send_email(state, None, &to, "plan_request_operator_alert", &vars).await {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(%aid, "plan-request operator alert to {to} failed: {e}");
            false
        }
    }
}

/// Tell the OPERATOR that a new workspace just signed up (2026-11-09, parity with FunnelSwift).
///
/// WHY THIS EXISTS (measured 2026-11-09): `POST /api/v1/auth/register` mints an account, a user,
/// a plan subscription and mails the owner their first password — and NOTHING told the person who
/// runs the install. The plan-request and support-request arms next door already mail
/// `operator_alert_email`; a new customer is the same kind of news and the one event the operator
/// most wants to see, yet it only appeared if somebody happened to open the console. FunnelSwift
/// closed the identical gap the same day.
///
/// Destination = the operator's own address (`admin_settings.billing.operator_alert_email`, set
/// from Admin -> Platform Settings). An unset address sends nothing: this code never invents a
/// mailbox. Sent through the SYSTEM configuration (`None`), never the new workspace's, because that
/// workspace has no sender identity yet and this is platform mail.
///
/// Never called for a harness/probe signup — the caller gates on the same two suppression reasons
/// the credentials mail uses, so machinery cannot mail the operator.
///
/// Best effort by design: the account is already committed, so a mail failure is logged and the
/// caller keeps its 201. Returns whether a send was attempted and queued.
pub async fn send_signup_operator_alert(
    state: &AppState,
    aid: Uuid,
    owner_name: &str,
    owner_email: &str,
    plan_slug: &str,
    source: &str,
) -> bool {
    // A FLEET PROBE MUST NEVER RING THE OPERATOR (kanban t_f17b5802, extended 2026-11-13).
    // The caller gates on the probe header and `harness_domain` — the MAIL-suppression class.
    // The fleet's disposable inbox (`maxxspace.com`) is deliberately NOT in that class (the
    // credential-delivery proof reads its password back out of it), so an audit signup through it
    // still pages a human. Measured live 2026-11-13: audit walks delivered operator alerts to the
    // owner's real mailbox. Requester, not route: the same door serves real customers.
    if let Some(reason) = crate::security::probe_addr::synthetic_requester(owner_email, owner_name)
    {
        tracing::info!(
            reason = %reason,
            "signup operator alert WITHHELD — the new workspace is a synthetic identity"
        );
        return false;
    }
    let billing: Option<serde_json::Value> =
        match sqlx::query_scalar("SELECT value FROM admin_settings WHERE key = 'billing'")
            .fetch_optional(&state.db)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("signup operator alert: could not read the billing settings: {e}");
                return false;
            }
        };
    let Some(to) = billing
        .as_ref()
        .and_then(|v| v.get("operator_alert_email"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        // Nothing configured is a normal state, not a failure: no mailbox is invented.
        return false;
    };
    // The workspace name is what the operator recognises; a blank one falls back to the owner's
    // address, never to an invented label.
    let workspace: String = sqlx::query_scalar::<_, Option<String>>(
        "SELECT NULLIF(btrim(name), '') FROM accounts WHERE id = $1",
    )
    .bind(aid)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .flatten()
    .unwrap_or_else(|| owner_email.to_string());
    // THE WORKSPACE NAME IS PART OF THE IDENTITY (2026-11-14). The gate above judges the OWNER;
    // a harness mints a clean-looking owner on a workspace whose own name still says
    // "Probe Co 1794597085". Measured on the two sibling apps that already carry this arm:
    // that exact shape delivered "New workspace: <probe name>" to the operator's real inbox while
    // an owner-only gate passed. Judged here, after the name is read and BEFORE any send, so no
    // `email_send_log` row is written either. Over-protection is the safe direction the residue
    // policy names: a customer who calls their workspace "Probe …" simply does not page us.
    if let Some(reason) = crate::security::probe_addr::synthetic_requester("", &workspace) {
        tracing::info!(
            reason = %reason,
            "signup operator alert WITHHELD — the workspace name is a synthetic identity"
        );
        return false;
    }
    // The tier's display name, not its slug: "Free", not "free". A catalogue that no longer holds
    // the slug still gets a readable answer rather than an empty line.
    let plan_name: String = sqlx::query_scalar::<_, Option<String>>(
        "SELECT NULLIF(btrim(name), '') FROM plan_tiers WHERE slug = $1",
    )
    .bind(plan_slug)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .flatten()
    .unwrap_or_else(|| {
        if plan_slug.trim().is_empty() {
            "no plan attached".to_string()
        } else {
            plan_slug.trim().to_string()
        }
    });
    let who = if owner_name.trim().is_empty() {
        owner_email.to_string()
    } else {
        format!("{} ({})", owner_name.trim(), owner_email)
    };
    let source = if source.trim().is_empty() {
        "unknown"
    } else {
        source.trim()
    };
    let vars = json!({
        "workspace": workspace,
        "who": who,
        "email": owner_email,
        "plan": plan_name,
        "source": source,
        "admin_url": "https://admin.workflowswift.com",
    });
    match send_email(state, None, &to, "signup_operator_alert", &vars).await {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(%aid, "signup operator alert to {to} failed: {e}");
            false
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

/// The app's OWN support channel — a PAGE, not a mailbox (measured 2026-11-06 on dns.google and
/// cloudflare-dns.com): every address this product printed lived on a domain publishing MX = 0,
/// with nothing on this host listening on port 25, so a customer's reply BOUNCED. The app serves a
/// contact form on its own host that files the message into the operator's support queue
/// (`POST /api/v1/public/support-request`), so [`with_support_footer`] names that page.
pub const SUPPORT_CONTACT: &str = "https://app.workflowswift.com/contact.html";

/// Append the app's support line to one body.
///
/// Applied at the single funnel every transport shares ([`send_email_request`]), so a body that
/// came from the authoritative `email_templates` row carries the address exactly like a
/// hardcoded fallback body does, and a template added later inherits it. An EMPTY body stays
/// empty (the `html_body` "" means "no HTML part" and must not become a footer-only part), and
/// a body that already carries the address is returned untouched.
fn with_support_footer(body: &str, html: bool) -> String {
    if body.is_empty() || body.contains(SUPPORT_CONTACT) {
        return body.to_string();
    }
    if html {
        format!(
            "{}\n<p style=\"font-size:13px;color:#6b7280;text-align:center;\">Need help? <a href=\"{}\">Send us a message</a> — no sign-in needed.</p>",
            body, SUPPORT_CONTACT
        )
    } else {
        format!(
            "{}\n\nNeed help? Send us a message at {}\n",
            body, SUPPORT_CONTACT
        )
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

    #[test]
    fn the_failed_run_notice_names_the_workflow_and_the_run_list() {
        let vars = json!({
            "name": "Dana",
            "workflow_name": "Invoice chaser",
            "run_name": "Nightly run",
            "failed_steps": 2,
            "failure": "upstream returned HTTP 503",
            "console_url": "https://app.workflowswift.com/#instances",
        });
        let (subject, text, html) = workflow_failed_bodies("https://app.workflowswift.com", &vars);
        assert!(subject.contains("Invoice chaser"), "subject={subject}");
        assert!(
            text.contains("Invoice chaser") && text.contains("Nightly run"),
            "text={text}"
        );
        assert!(
            text.contains("2 steps"),
            "the count is pluralised: text={text}"
        );
        assert!(
            text.contains("https://app.workflowswift.com/#instances"),
            "text={text}"
        );
        assert!(
            html.contains("https://app.workflowswift.com/#instances"),
            "html lacks the link"
        );
        assert!(
            html.contains("upstream returned HTTP 503"),
            "html lacks the reason"
        );

        // A step message is customer-supplied text, so it must not be able to inject markup.
        let hostile = json!({
            "name": "Dana",
            "workflow_name": "<script>x</script>",
            "run_name": "run",
            "failed_steps": 1,
            "failure": "<img src=x onerror=alert(1)>",
            "console_url": "https://app.workflowswift.com/#instances",
        });
        let (subject, text, html) =
            workflow_failed_bodies("https://app.workflowswift.com", &hostile);
        assert!(
            subject.contains("<script>x</script>"),
            "subject is an escaped field: {subject}"
        );
        assert!(
            !html.contains("<script>x</script>"),
            "the html arm must escape the name"
        );
        assert!(
            !html.contains("<img src=x"),
            "the html arm must escape the reason"
        );
        assert!(
            text.contains("<img src=x onerror=alert(1)>"),
            "plain text keeps the raw reason"
        );
        assert!(text.contains("1 step "), "singular wording: text={text}");
    }

    #[test]
    fn a_resolve_that_moved_the_plan_says_which_plan() {
        let (said, next) = ticket_update_wording("resolved", Some("Professional"));
        assert!(said.contains("now on the Professional plan"), "said={said}");
        assert!(
            !said.contains("has been resolved\u{2014}"),
            "the plan sentence replaces, not appends"
        );
        assert!(next.contains("charged you nothing"), "next={next}");
    }

    #[test]
    fn a_plain_ticket_keeps_its_old_wording() {
        let (said, next) = ticket_update_wording("resolved", None);
        assert_eq!(said, "has been resolved");
        assert!(next.contains("If that was a plan request"), "next={next}");
        let (pending, _) = ticket_update_wording("pending", None);
        assert_eq!(pending, "is being worked on by our team");
        let (reopened, _) = ticket_update_wording("open", None);
        assert_eq!(reopened, "is open again");
    }

    #[test]
    fn the_plan_sentence_is_used_for_any_status_that_applied_one() {
        // `closed` also applies a plan in the resolve arm, so the wording cannot be keyed on
        // "resolved" alone.
        let (said, _) = ticket_update_wording("closed", Some("Enterprise"));
        assert!(said.contains("now on the Enterprise plan"), "said={said}");
    }

    /// The at-rest posture the panels now PRINT, instead of asserting it. Until 2026-10-16 the SMS
    /// panel said "sealed at rest" and the Provider Keys panel said "encrypted at rest"
    /// unconditionally, so an operator could not tell a sealed row from a legacy plaintext one
    /// (the rule CoreSwift's panel set, kanban t_d1fcda7e).
    #[test]
    fn at_rest_posture_reads_the_storage_envelope() {
        // Nothing saved: no credential, so not sealed either.
        assert!(!stored_credential_present(&json!({})));
        assert!(!stored_credential_sealed(&json!({})));
        // Routing fields only: still no credential.
        assert!(!stored_credential_present(&json!({
            "provider": "mailgun",
            "api_url": "https://api.mailgun.net/v3/mail.workflowswift.com/messages",
            "api_key": ""
        })));
        // A legacy plaintext credential is present and NOT sealed.
        let plain = json!({"api_key": "key-0123456789abcdef"});
        assert!(stored_credential_present(&plain));
        assert!(!stored_credential_sealed(&plain));
        // The envelope is the whole answer, not the field name.
        let sealed = json!({
            "api_key": format!("{}{}", crate::security::provider_key_crypto::ENC_PREFIX, "Y2lwaGVy")
        });
        assert!(stored_credential_present(&sealed));
        assert!(stored_credential_sealed(&sealed));
        // Every non-empty credential field must be enveloped: one plaintext field beside a sealed
        // one is "stored in the clear", which is the state the operator has to see.
        let mixed = json!({
            "api_key": format!("{}{}", crate::security::provider_key_crypto::ENC_PREFIX, "Y2lwaGVy"),
            "smtp_password": "hunter2"
        });
        assert!(!stored_credential_sealed(&mixed));
    }

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

    /// 2026-10-24 (kanban t_081edaac). The header block used to appear only once an ACCOUNT had
    /// uploaded a logo, so a brand-new account's mail — and the Email Branding card's own preview —
    /// arrived with no wordmark at all while the panel said "it carries the WorkflowSwift header".
    /// The app's OWN identity is now the fallback: a PNG on a public host (mail clients do not
    /// render the SVG the pages use) whose `alt` carries the name, so a blocked image still says
    /// whose mail this is.
    #[test]
    fn an_account_with_no_branding_still_gets_the_apps_own_wordmark() {
        let b = app_default_branding();
        assert_eq!(b.brand_name, "WorkflowSwift");
        let url = b
            .resolve_logo_url(branding::APP_URL)
            .expect("the app wordmark resolves");
        assert_eq!(url, "https://app.workflowswift.com/logo.png");

        let header = b.header_html(Some(&url));
        assert!(header.contains("<img"), "{header}");
        assert!(header.contains(&url), "{header}");
        assert!(header.contains("alt=\"WorkflowSwift\""), "{header}");
        assert!(
            header.contains(branding::DEFAULT_BRAND_COLOR),
            "the fleet accent paints the rule: {header}"
        );

        // BOTH parts open with it: the text part is headed with the name (a text part cannot carry
        // an image), the HTML part with the wordmark.
        let (text, html) = apply_branding(
            Some(&b),
            "Hello".to_string(),
            "<html><body><p>Hello</p></body></html>".to_string(),
        );
        assert_eq!(text, "WorkflowSwift\n\nHello");
        assert!(html.contains(&url), "{html}");
        assert!(html.contains("<p>Hello</p>"), "{html}");

        // The additive leg the feature was shipped with is unchanged: `None` still returns the pair
        // untouched. Every SEND passes `Some` now, because the funnel substitutes the app identity.
        let (t2, h2) = apply_branding(None, "Hello".to_string(), "<p>Hello</p>".to_string());
        assert_eq!((t2.as_str(), h2.as_str()), ("Hello", "<p>Hello</p>"));
    }

    /// The merge fields are bound from what the ACCOUNT stored, not from the effective identity, so
    /// the preview can say truthfully whose logo the reader is looking at — and `logo_url` is never
    /// bound as an empty string.
    #[test]
    fn the_branding_merge_fields_say_whose_logo_it_is() {
        // Nothing stored: the app's own identity, and the reader is told it is not theirs.
        let mut v = json!({"name": "Dana"});
        bind_branding(&mut v, None);
        assert_eq!(
            v.get("brand_name").and_then(|x| x.as_str()),
            Some("WorkflowSwift")
        );
        assert_eq!(
            v.get("logo_url").and_then(|x| x.as_str()),
            Some("https://app.workflowswift.com/logo.png")
        );
        assert_eq!(v.get("logo_is_own").and_then(|x| x.as_str()), Some("no"));
        assert_eq!(
            render_template("Hi {{name}} — {{brand_name}}", &v, "test"),
            "Hi Dana — WorkflowSwift"
        );
        assert!(unsubstituted(&render_template("{{logo_url}}", &v, "t")).is_empty());

        // A name but no logo: their own name wins, and the logo is still the app's.
        let name_only = Branding {
            brand_name: "Acme".to_string(),
            brand_color: String::new(),
            logo_url: String::new(),
        };
        let mut v = json!({});
        bind_branding(&mut v, Some(&name_only));
        assert_eq!(v.get("brand_name").and_then(|x| x.as_str()), Some("Acme"));
        assert_eq!(v.get("logo_is_own").and_then(|x| x.as_str()), Some("no"));
        assert_eq!(
            v.get("logo_url").and_then(|x| x.as_str()),
            Some("https://app.workflowswift.com/logo.png")
        );

        // Their own upload: the stored pointer, version stamp and all.
        let owned = Branding {
            brand_name: "Acme".to_string(),
            brand_color: "#0ea5e9".to_string(),
            logo_url: "/api/v1/branding/logo/abc?v=7".to_string(),
        };
        let mut v = json!({});
        bind_branding(&mut v, Some(&owned));
        assert_eq!(
            v.get("logo_url").and_then(|x| x.as_str()),
            Some("https://app.workflowswift.com/api/v1/branding/logo/abc?v=7")
        );
        assert_eq!(v.get("logo_is_own").and_then(|x| x.as_str()), Some("yes"));
    }
}
