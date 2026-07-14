use axum::{
    extract::{State, Json},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;
use std::fs;

use sqlx::Row;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;

const SITE_KEY: &str = "workflowswift_site";

/// GET /api/v1/admin/site — get site settings (SEO, tracking, homepage)
pub async fn get_site(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let defaults = default_site_settings();

    let row = sqlx::query(
        "SELECT value FROM admin_settings WHERE key = $1"
    )
    .bind(SITE_KEY)
    .fetch_optional(&state.db)
    .await?;

    let settings = match row {
        Some(r) => {
            let val: serde_json::Value = r.try_get("value")?;
            merge_json(defaults, val)
        }
        None => defaults,
    };

    Ok(Json(settings))
}

/// PUT /api/v1/admin/site — update site settings & regenerate HTML
pub async fn update_site(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let admin_id = Uuid::parse_str(&claims.sub).unwrap_or(Uuid::nil());

    // Merge with existing to preserve fields not sent
    let existing_row = sqlx::query(
        "SELECT value FROM admin_settings WHERE key = $1"
    )
    .bind(SITE_KEY)
    .fetch_optional(&state.db)
    .await?;

    let merged = match existing_row {
        Some(r) => {
            let existing_val: serde_json::Value = r.try_get("value")?;
            merge_json(existing_val, req)
        }
        None => req,
    };

    sqlx::query(
        r#"INSERT INTO admin_settings (key, value, description, updated_at, updated_by)
           VALUES ($1, $2::jsonb, 'WorkflowSwift site settings (SEO, tracking, homepage)', NOW(), $3)
           ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, updated_at = NOW(), updated_by = $3"#
    )
    .bind(SITE_KEY)
    .bind(merged.to_string())
    .bind(admin_id)
    .execute(&state.db)
    .await?;

    // Regenerate HTML
    regenerate_html(&merged)?;

    Ok(Json(json!({"message": "Site settings updated", "slug": "workflowswift"})))
}

/// ── HTML Injection (no regex) ──

fn regenerate_html(settings: &serde_json::Value) -> Result<(), AppError> {
    let html_path = "/opt/swift/www/workflowswift/index.html";
    let html = fs::read_to_string(html_path)
        .map_err(|e| AppError::Internal(format!("Failed to read {}: {}", html_path, e)))?;

    let html = inject_site_settings(&html, settings);

    fs::write(html_path, &html)
        .map_err(|e| AppError::Internal(format!("Failed to write {}: {}", html_path, e)))?;

    Ok(())
}

fn inject_site_settings(html: &str, s: &serde_json::Value) -> String {
    let mut result = html.to_string();

    // ── Title ──
    if let Some(title) = s.get("title").and_then(|v| v.as_str()) {
        replace_title(&mut result, title);
    }

    // ── Meta tags ──
    if let Some(desc) = s.get("description").and_then(|v| v.as_str()) {
        upsert_meta(&mut result, "description", desc);
    }
    if let Some(kw) = s.get("keywords").and_then(|v| v.as_str()) {
        upsert_meta(&mut result, "keywords", kw);
    }

    // ── OG tags ──
    upsert_meta_prop(&mut result, "og:title", s.get("og_title").and_then(|v| v.as_str()));
    upsert_meta_prop(&mut result, "og:description", s.get("og_description").and_then(|v| v.as_str()));
    upsert_meta_prop(&mut result, "og:image", s.get("og_image_url").and_then(|v| v.as_str()));
    upsert_meta_prop(&mut result, "og:type", None);

    // ── Schema.org ──
    if let Some(schema_json) = s.get("schema_json").and_then(|v| v.as_str()) {
        if !schema_json.is_empty() {
            upsert_schema(&mut result, schema_json);
        }
    }

    // ── GA / GTM ──
    let ga_id = s.get("ga_id").and_then(|v| v.as_str()).unwrap_or("");
    let gtm_id = s.get("gtm_id").and_then(|v| v.as_str()).unwrap_or("");

    remove_ga_gtm(&mut result);

    if !ga_id.is_empty() {
        let ga_script = format!(
            r#"<script async src="https://www.googletagmanager.com/gtag/js?id={}"></script>
<script>window.dataLayer=window.dataLayer||[];function gtag(){{dataLayer.push(arguments);}}gtag('js',new Date());gtag('config','{}');</script>"#,
            html_escape(ga_id), html_escape(ga_id)
        );
        inject_before_head_end(&mut result, &ga_script);
    }

    if !gtm_id.is_empty() {
        let gtm_head = format!(
            r#"<script>(function(w,d,s,l,i){{w[l]=w[l]||[];w[l].push({{'gtm.start':new Date().getTime(),event:'gtm.js'}});var f=d.getElementsByTagName(s)[0],j=d.createElement(s),dl=l!='dataLayer'?'&l='+l:'';j.async=true;j.src='https://www.googletagmanager.com/gtm.js?id='+i+dl;f.parentNode.insertBefore(j,f);}})(window,document,'script','dataLayer','{}');</script>"#,
            html_escape(gtm_id)
        );
        inject_before_head_end(&mut result, &gtm_head);

        let gtm_body = format!(
            r#"<noscript><iframe src="https://www.googletagmanager.com/ns.html?id={}" height="0" width="0" style="display:none;visibility:hidden"></iframe></noscript>"#,
            html_escape(gtm_id)
        );
        inject_after_body_start(&mut result, &gtm_body);
    }

    // ── Custom head scripts (ADA widget, chatbot, etc) ──
    if let Some(head_scripts) = s.get("head_scripts").and_then(|v| v.as_str()) {
        if !head_scripts.is_empty() {
            inject_before_head_end(&mut result, head_scripts);
        }
    }

    // ── Custom body end scripts ──
    if let Some(body_scripts) = s.get("body_scripts").and_then(|v| v.as_str()) {
        if !body_scripts.is_empty() {
            inject_before_body_end(&mut result, body_scripts);
        }
    }

    result
}

/// ── String helpers (no regex) ──

fn replace_title(result: &mut String, new_title: &str) {
    let open = "<title>";
    let close = "</title>";
    if let Some(start) = result.find(open) {
        let after_open = start + open.len();
        if let Some(end) = result[after_open..].find(close) {
            result.replace_range(after_open..after_open + end, new_title);
        }
    } else {
        inject_before_head_end(result, &format!("  <title>{}</title>", new_title));
    }
}

fn upsert_meta(result: &mut String, name: &str, content: &str) {
    let escaped = html_escape(content);
    let pattern = format!(r#"<meta name="{}""#, name);
    if let Some(pos) = result.find(&pattern) {
        // Replace the full tag
        let after = &result[pos..];
        // Find closing >
        if let Some(end) = after.find('>') {
            let full_tag_end = pos + end + 1;
            let new_tag = format!(r#"<meta name="{}" content="{}">"#, name, escaped);
            result.replace_range(pos..full_tag_end, &new_tag);
        }
    } else {
        inject_before_head_end(result, &format!(r#"  <meta name="{}" content="{}">"#, name, escaped));
    }
}

fn upsert_meta_prop(result: &mut String, property: &str, content: Option<&str>) {
    if let Some(c) = content {
        let escaped = html_escape(c);
        let pattern = format!(r#"<meta property="{}""#, property);
        if let Some(pos) = result.find(&pattern) {
            let after = &result[pos..];
            if let Some(end) = after.find('>') {
                let full_tag_end = pos + end + 1;
                let new_tag = format!(r#"<meta property="{}" content="{}">"#, property, escaped);
                result.replace_range(pos..full_tag_end, &new_tag);
            }
        } else {
            inject_before_head_end(result, &format!(r#"  <meta property="{}" content="{}">"#, property, escaped));
        }
    } else {
        // If content is None, just ensure the tag doesn't exist (remove it)
        let _pattern = format!(r#"<meta property="{}"[^>]*>"#, property);
        // Simple remove: find the tag and delete it
        let search = format!(r#"<meta property="{}""#, property);
        if let Some(pos) = result.find(&search) {
            let after = &result[pos..];
            if let Some(end) = after.find('>') {
                result.replace_range(pos..pos + end + 1, "");
            }
        }
    }
}

fn upsert_schema(result: &mut String, schema_json: &str) {
    let open = r#"<script type="application/ld+json">"#;
    let close = r#"</script>"#;
    if let Some(start) = result.find(open) {
        let after_open = start + open.len();
        if let Some(end) = result[after_open..].find(close) {
            result.replace_range(after_open..after_open + end, schema_json);
        }
    } else {
        inject_before_head_end(result, &format!("  <script type=\"application/ld+json\">{}</script>", schema_json));
    }
}

fn remove_ga_gtm(result: &mut String) {
    // Remove GA gtag script
    let ga_async = r#"<script async src="https://www.googletagmanager.com/gtag/js"#;
    loop {
        if let Some(pos) = result.find(ga_async) {
            if let Some(end) = result[pos..].find("</script>") {
                result.replace_range(pos..pos + end + 9, "");
                continue;
            }
        }
        break;
    }

    // Remove GA config script block
    let ga_config = r#"<script>window.dataLayer=window.dataLayer"#;
    loop {
        if let Some(pos) = result.find(ga_config) {
            if let Some(end) = result[pos..].find("</script>") {
                result.replace_range(pos..pos + end + 9, "");
                continue;
            }
        }
        break;
    }

    // Remove GTM head script
    let gtm_head = r#"<script>(function(w,d,s,l,i){w[l]=w[l]||[];w[l].push"#;
    loop {
        if let Some(pos) = result.find(gtm_head) {
            if let Some(end) = result[pos..].find("</script>") {
                result.replace_range(pos..pos + end + 9, "");
                continue;
            }
        }
        break;
    }

    // Remove GTM noscript iframe
    let gtm_ns = r#"<noscript><iframe src="https://www.googletagmanager.com/ns.html"#;
    loop {
        if let Some(pos) = result.find(gtm_ns) {
            if let Some(end) = result[pos..].find("</noscript>") {
                result.replace_range(pos..pos + end + 11, "");
                continue;
            }
        }
        break;
    }

    // Clean up triple newlines
    while result.contains("\n\n\n") {
        *result = result.replace("\n\n\n", "\n\n");
    }
}

fn inject_before_head_end(result: &mut String, content: &str) {
    let close_head = "</head>";
    if let Some(pos) = result.rfind(close_head) {
        result.insert_str(pos, &format!("\n  {}", content));
    }
}

fn inject_after_body_start(result: &mut String, content: &str) {
    let _close_body_tag = ">";
    // Find the first <body...> tag and insert after its >
    if let Some(body_pos) = result.find("<body") {
        let after = &result[body_pos..];
        if let Some(end) = after.find('>') {
            let insert_at = body_pos + end + 1;
            result.insert_str(insert_at, &format!("\n  {}", content));
        }
    }
}

fn inject_before_body_end(result: &mut String, content: &str) {
    let close_body = "</body>";
    if let Some(pos) = result.rfind(close_body) {
        result.insert_str(pos, &format!("\n  {}", content));
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn merge_json(a: serde_json::Value, b: serde_json::Value) -> serde_json::Value {
    match (a, b) {
        (serde_json::Value::Object(mut a_map), serde_json::Value::Object(b_map)) => {
            for (k, v) in b_map {
                a_map.insert(k, v);
            }
            serde_json::Value::Object(a_map)
        }
        (_a, b) => b,
    }
}

fn default_site_settings() -> serde_json::Value {
    json!({
        "title": "WorkflowSwift | No-Code Visual Workflow Automation Platform",
        "description": "Connect your data, your tools, and your teams with WorkflowSwift. Build powerful, automated sequences using our drag-and-drop visual builder—no coding required.",
        "keywords": "workflow automation platform, no-code automation, visual workflow builder, business process automation, API integration, webhook automation, WorkflowSwift",
        "og_title": "Automate Everything. Code Nothing. | WorkflowSwift",
        "og_description": "Build visual automation pipelines that connect your favorite apps, trigger automatically, and display real-time data on industry-specific dashboards.",
        "og_image_url": "",
        "favicon_url": "",
        "canonical_url": "https://workflowswift.com",
        "ga_id": "",
        "gtm_id": "",
        "head_scripts": "",
        "body_scripts": "",
        "schema_json": "{\"@context\":\"https://schema.org\",\"@type\":\"SoftwareApplication\",\"name\":\"WorkflowSwift\",\"operatingSystem\":\"All\",\"applicationCategory\":\"BusinessApplication\",\"offers\":{\"@type\":\"Offer\",\"price\":\"0.00\",\"priceCurrency\":\"USD\"},\"description\":\"No-code visual workflow automation platform. Connect data, tools, and teams with drag-and-drop pipelines.\",\"aggregateRating\":{\"@type\":\"AggregateRating\",\"ratingValue\":\"4.9\",\"reviewCount\":\"142\"},\"featureList\":\"Webhook triggers, scheduled runs, HTTP requests, conditional logic, email actions, dashboard widgets, browser automation, AI prompts, manual review steps\"}",
        "homepage": {
            "logo_text": "WorkflowSwift",
            "sign_in_url": "",
            "nav_cta_text": "Sign In",
            "headline": "Automate Everything.<br>Code Nothing.",
            "subheadline": "Connect your data, your tools, and your teams with powerful, visual automation pipelines.",
            "button_text": "Start Building Free",
            "secondary_button_text": "Sign In",
            "features_heading": "Everything You Need to Automate",
            "features": [],
            "cta_heading": "Ready to Automate Your Workflow?",
            "cta_text": "Join thousands of teams using WorkflowSwift to save time and reduce errors.",
            "footer_text": "© 2026 WorkflowSwift. All rights reserved."
        }
    })
}

fn require_admin(claims: &Claims) -> Result<(), AppError> {
    if !claims.perm_is_super_admin.unwrap_or(false) {
        return Err(AppError::Forbidden("Admin access required".to_string()));
    }
    Ok(())
}
