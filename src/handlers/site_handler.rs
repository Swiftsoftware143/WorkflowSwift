//! WorkflowSwift site settings — the DB row is the SOURCE OF TRUTH; the static pages are
//! materialized by a HOST-side applier, never by the request path.
//!
//! WHY (kanban t_0be214ec; same class as IncentiveSwift t_3fb0d3d2, ADASwift t_1f427190,
//! CoreSwift-CRM t_02986434 and t_9dede800; evidence /opt/swift/audits/t_0be214ec/)
//! --------------------------------------------------------------------------------------------
//! Two defects were measured here before anything was changed:
//!
//! 1. **No applier existed at all.** The marketing page + the three legal pages are plain files
//!    under `/opt/swift/nginx/www/workflowswift/` (served by nginx for workflowswift.com). The
//!    service runs in a container with ZERO mounts (`docker inspect workflowswift --format
//!    '{{json .Mounts}}'` -> `[]`), so `PUT /api/v1/admin/site` could never write them: it
//!    UPSERTed the `admin_settings` row FIRST, then died in `regenerate_html` reading a path that
//!    does not exist anywhere — `/opt/swift/www/workflowswift/index.html` — answering 500 *after*
//!    the write had landed (measured live: PUT `{}` -> 500 while the row's `updated_at` moved,
//!    log line "Failed to read /opt/swift/www/workflowswift/index.html: No such file or
//!    directory"). An admin saw a red toast for a save that had in fact committed.
//!
//! 2. **The three editors the card names were honoured by NOTHING.** `canonical_url`,
//!    `favicon_url` and the `homepage.headline`/`subheadline` pair were stored by the panel and
//!    read by no renderer, so an operator's input appeared nowhere on the served page. The card's
//!    arm (a) applies: the served page already carries the elements (`<link rel="canonical">`,
//!    one `rel="icon"`, one `<h1>`, the hero paragraph), so they are made REAL by in-place
//!    surgery, and both the code defaults and the row are reconciled to the SERVED bytes so the
//!    first apply is a byte-level no-op.
//!
//! The request-path write is retired the way the four sibling apps proved it: the row is the
//! source of truth, `update_site` writes ONLY the row and answers 2xx naming the applier, and
//! `/opt/swift/bin/ws-site-apply.sh` runs `workflowswift-api apply-site-settings` on the HOST
//! (where the files actually are) from cron. The alternative — mounting the served root into the
//! container — was rejected for the same reason the siblings rejected it: that root is also the
//! target of the repo->served publish gate (`/opt/swift/fleet/marketing-www-parity.py`), and a
//! container that writes it makes two writers for one tree with no reconciliation.
//!
//! INVARIANTS THIS MODULE KEEPS
//! --------------------------------------------------------------------------------------------
//! * No request path writes a file. `update_site` = one UPSERT + 2xx.
//! * A blank `legal_*` can never downgrade a published page: the applier guards on the VALUE
//!   (`trim().is_empty()` -> skip + reason), and `preserve_nonblank` refuses the same blank at the
//!   STORE, so the panel's GET-then-PUT round trip cannot blank the row either.
//! * A blank `canonical_url` / `favicon_url` / hero value leaves the SHIPPED element alone:
//!   clearing a field can never blank a live page.
//! * The applier is idempotent: a file is only rewritten when its bytes would change, so a
//!   scheduled run is free and the repo/served parity gate stays quiet.
use axum::{
    extract::{Json, State},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use std::fs;
use uuid::Uuid;

use sqlx::Row;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::AppState;

const SITE_KEY: &str = "workflowswift_site";

/// Where the static marketing page and the three legal pages live. These are HOST paths: the app
/// runs in a container with ZERO mounts, so the only process that can write them is the same binary
/// executed on the host (`workflowswift-api apply-site-settings`, driven by
/// /opt/swift/bin/ws-site-apply.sh).
pub(crate) const SITE_ROOT: &str = "/opt/swift/nginx/www/workflowswift/";
pub(crate) const SITE_INDEX: &str = "/opt/swift/nginx/www/workflowswift/index.html";

/// The legal pages this applier owns: (slug, settings key). The row carries the BODY that sits
/// between the page's `</h1>` and its `.back` footer — the same convention CoreSwift-CRM and
/// IncentiveSwift reconciled to — so the render is in-place surgery on the served bytes and a
/// reconciled row is byte-identical to what is served.
pub(crate) const LEGAL_PAGES: [(&str, &str); 3] = [
    ("terms", "legal_tos"),
    ("privacy", "legal_privacy"),
    ("refunds", "legal_refunds"),
];

/// The legal keys `preserve_nonblank` protects at the store.
const LEGAL_KEYS: [&str; 3] = ["legal_tos", "legal_privacy", "legal_refunds"];

/// GET /api/v1/admin/site — get site settings (SEO, tracking, homepage)
pub async fn get_site(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;
    let settings = load_settings(&state.db).await?;
    Ok(Json(settings))
}

/// The stored settings merged over the code defaults — the same value `get_site` serves, and the
/// input to the host-side applier.
pub(crate) async fn load_settings(db: &sqlx::PgPool) -> Result<serde_json::Value, AppError> {
    let defaults = default_site_settings();

    let row = sqlx::query("SELECT value FROM admin_settings WHERE key = $1")
        .bind(SITE_KEY)
        .fetch_optional(db)
        .await?;

    Ok(match row {
        Some(r) => {
            let val: serde_json::Value = r.try_get("value")?;
            merge_json(defaults, val)
        }
        None => defaults,
    })
}

/// PUT /api/v1/admin/site — save site settings.
///
/// Deliberately NO file writes on this path. The static pages are HOST paths and this service runs
/// in a container with no mount for them, so writing them here could only ever answer 500 — after
/// the row above had already committed, and an admin then saw a failure for a save that had in fact
/// landed (kanban t_0be214ec: measured PUT `{}` -> 500 with the row's `updated_at` already moved).
/// The row IS the source of truth (`get_site` reads it); the pages are materialized by the
/// host-side applier, which is their only writer.
pub async fn update_site(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let admin_id = Uuid::parse_str(&claims.sub).unwrap_or(Uuid::nil());

    // Merge with existing to preserve fields not sent
    let existing_row = sqlx::query("SELECT value FROM admin_settings WHERE key = $1")
        .bind(SITE_KEY)
        .fetch_optional(&state.db)
        .await?;

    let existing: Option<serde_json::Value> = match existing_row {
        Some(r) => Some(r.try_get("value")?),
        None => None,
    };

    let merged = match &existing {
        Some(existing_val) => merge_json(existing_val.clone(), req),
        None => req,
    };

    // The STORE-side value guard: a blank/absent incoming `legal_*` keeps the stored text.
    let (merged, preserved) = preserve_nonblank_legal(existing.as_ref(), merged);

    sqlx::query(
        r#"INSERT INTO admin_settings (key, value, description, updated_at, updated_by)
           VALUES ($1, $2::jsonb, 'WorkflowSwift site settings (SEO, tracking, homepage, legal)', NOW(), $3)
           ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, updated_at = NOW(), updated_by = $3"#,
    )
    .bind(SITE_KEY)
    .bind(merged.to_string())
    .bind(admin_id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "message": "Site settings saved",
        // The keys whose stored text a blank incoming value was refused for — the caller can see
        // exactly what was kept instead of having to diff the row.
        "preserved": preserved,
        "static_pages": {
            "writer": "/opt/swift/bin/ws-site-apply.sh (workflowswift-api apply-site-settings)",
            "within_minutes": 5
        }
    })))
}

/// A blank string (or null, or a missing key) is how "the operator cleared this field" and "this
/// form was rendered from a GET that merged the code defaults" look identical on the wire — and the
/// defaults carry `""` for all three legal keys. Refuse the blank at the STORE when the row already
/// holds text, so a GET-then-PUT round trip can never blank a live policy's source text. (The
/// applier guards the same value at the render, so the page is protected twice over.)
fn preserve_nonblank_legal(
    existing: Option<&serde_json::Value>,
    mut merged: serde_json::Value,
) -> (serde_json::Value, Vec<String>) {
    let mut preserved = Vec::new();
    for key in LEGAL_KEYS {
        let stored_has_text = existing
            .and_then(|e| e.get(key))
            .map(|v| !is_blank(Some(v)))
            .unwrap_or(false);
        if stored_has_text && is_blank(merged.get(key)) {
            if let (Some(dst), Some(src)) = (merged.get_mut(key), existing.and_then(|e| e.get(key)))
            {
                *dst = src.clone();
                preserved.push(key.to_string());
                tracing::warn!(
                    key,
                    "blank legal value refused: the stored legal text was preserved"
                );
            }
        }
    }
    (merged, preserved)
}

fn is_blank(v: Option<&serde_json::Value>) -> bool {
    match v {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(s)) => s.trim().is_empty(),
        Some(_) => false,
    }
}

/// `(path, rendered bytes)` for every file the applier owns and has a value for.
pub(crate) type PlanTargets = Vec<(String, String)>;
/// `(path, reason)` for every file deliberately left alone.
pub(crate) type PlanSkips = Vec<(String, String)>;

/// Render every file this applier owns, WITHOUT touching the disk.
///
/// Returns `(targets, skipped)`: `targets` is `(path, rendered bytes)` for every file whose source
/// value exists, `skipped` is `(path, reason)` for the ones deliberately left alone — a blank legal
/// value lands here, never in `targets`.
pub(crate) fn plan(settings: &serde_json::Value) -> (PlanTargets, PlanSkips) {
    let mut targets: PlanTargets = Vec::new();
    let mut skipped: PlanSkips = Vec::new();

    if !std::path::Path::new(SITE_ROOT).is_dir() {
        skipped.push((
            SITE_ROOT.to_string(),
            "directory is not present in this runtime (host-only path)".to_string(),
        ));
        return (targets, skipped);
    }

    match fs::read_to_string(SITE_INDEX) {
        Ok(before) => targets.push((
            SITE_INDEX.to_string(),
            inject_site_settings(&before, settings),
        )),
        Err(e) => skipped.push((SITE_INDEX.to_string(), format!("unreadable: {}", e))),
    }

    for (slug, key) in LEGAL_PAGES {
        let path = format!("{}{}.html", SITE_ROOT, slug);
        match settings.get(key).and_then(|v| v.as_str()) {
            // Blank or absent means "no policy text configured" -> leave the published page alone.
            Some(text) if !text.trim().is_empty() => match fs::read_to_string(&path) {
                Ok(before) => {
                    let mut rendered = before.clone();
                    replace_legal_body(&mut rendered, text);
                    targets.push((path, rendered));
                }
                Err(e) => skipped.push((path, format!("unreadable: {}", e))),
            },
            _ => skipped.push((
                path,
                format!(
                    "{} is blank or absent - the published page is left alone",
                    key
                ),
            )),
        }
    }

    (targets, skipped)
}

/// Materialize `settings` into the static marketing page + the three legal pages.
///
/// Returns `(written, skipped)`; every skipped entry is `(path, reason)`. Idempotent: a file is only
/// rewritten when its bytes would change, and nothing here is fatal — the caller reports the
/// outcome so no surface can claim a regeneration that did not happen.
pub(crate) fn apply_to_disk(settings: &serde_json::Value) -> (Vec<String>, PlanSkips) {
    let (targets, mut skipped) = plan(settings);
    let mut written: Vec<String> = Vec::new();

    for (path, rendered) in targets {
        match fs::read_to_string(&path) {
            Ok(before) if before == rendered => skipped.push((path, "unchanged".to_string())),
            _ => match fs::write(&path, rendered.as_bytes()) {
                Ok(_) => written.push(path),
                Err(e) => skipped.push((path, e.to_string())),
            },
        }
    }

    (written, skipped)
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
    upsert_meta_prop(
        &mut result,
        "og:title",
        s.get("og_title").and_then(|v| v.as_str()),
    );
    upsert_meta_prop(
        &mut result,
        "og:description",
        s.get("og_description").and_then(|v| v.as_str()),
    );
    // A blank `og_image_url` means "no share image configured". It must NOT inject an empty
    // `<meta property="og:image" content="">`: the served page carries no such tag, so the
    // injection made every apply rewrite the head (measured: it was the only reason `--check`
    // read `would-write` on an otherwise reconciled row). Blank -> the `None` arm, which is a
    // no-op when the tag is absent.
    let og_image = s
        .get("og_image_url")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty());
    upsert_meta_prop(&mut result, "og:image", og_image);
    // NOTE: `og:type` is deliberately NOT passed here with `None`. The shipped page carries
    // `<meta property="og:type" content="website">` and the settings row has no `og_type` key, so
    // the old `upsert_meta_prop(..., None)` call DELETED a served tag on every render.

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
            html_escape(ga_id),
            html_escape(ga_id)
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

    // ── The editors no reader honoured (kanban t_0be214ec) ──
    // `canonical_url`, `favicon_url` and the two `homepage` hero fields were written to the row by
    // the panel and read by NOTHING, so an operator's input landed in
    // `admin_settings.workflowswift_site` and appeared nowhere. Each is now IN-PLACE surgery on an
    // element the served page already carries, and each is a no-op when the value equals the
    // shipped one — the reconciled row is the shipped row, so the applier's `--check` stays
    // `unchanged` and the first apply cannot rewrite the live homepage. A blank value leaves the
    // shipped element alone: clearing a field can never blank a live page.
    if let Some(c) = s.get("canonical_url").and_then(|v| v.as_str()) {
        if !c.trim().is_empty() {
            upsert_link_href(&mut result, "canonical", c);
        }
    }
    if let Some(f) = s.get("favicon_url").and_then(|v| v.as_str()) {
        if !f.trim().is_empty() {
            // Only the `rel="icon"` tag is the operator's; the `alternate icon` href is a
            // content-hashed deploy asset and stays as shipped. The tag prefix carries the closing
            // quote so `rel="icon"` can never match `rel="alternate icon"`.
            upsert_link_href(&mut result, "icon", f);
        }
    }
    if let Some(hp) = s.get("homepage") {
        if let Some(h) = hp.get("headline").and_then(|v| v.as_str()) {
            if !h.trim().is_empty() {
                // VERBATIM, not escaped: the shipped headline is
                // `Build workflows<br>that <span class="accent">actually work</span>.` and escaping
                // it would publish the markup as text and drop the accent span. The panel already
                // advertises `Headline (HTML ok)`.
                replace_inner(&mut result, "<h1>", "</h1>", h);
            }
        }
        if let Some(sh) = hp.get("subheadline").and_then(|v| v.as_str()) {
            if !sh.trim().is_empty() {
                // The hero paragraph is the first `<p>` AFTER the `<h1>`. It is deliberately NOT
                // the first `<p class="subtitle">`: on this page that element is the FEATURES
                // section's subtitle further down, not the hero.
                replace_inner_after(&mut result, "</h1>", "<p", "</p>", sh);
            }
        }

        // ── The REST of the homepage editors (kanban t_e8bfd1f3) ──
        // One card after t_0be214ec: the Site Configuration panel renders these editors, the row
        // stores what the operator types, and no reader moved any of them — an operator's input
        // landed in `admin_settings.workflowswift_site.homepage` and appeared nowhere. Each is now
        // IN-PLACE surgery on an element the SERVED page already carries, located by the section
        // that owns it (never by position, never by the value it currently holds, so a field stays
        // repeatable after the operator sets it), and each is a no-op when the value equals the
        // shipped one — the reconciled row IS the shipped row, so the applier's `--check` reads
        // `unchanged` and the 5-minute run cannot rewrite the live homepage. A blank value leaves
        // the shipped element alone: clearing a field can never blank a live page.
        if let Some(v) = hp.get("logo_text").and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                // The logo's icon box (`<div class="logo-icon">WS</div>`) is shipped markup the
                // operator does not own; only the wordmark text node after it is the field's.
                replace_logo_text(&mut result, v);
            }
        }
        if let Some(v) = hp.get("sign_in_url").and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                // BOTH sign-in entry points move together (the nav's plain link and the hero's
                // outline button): "the sign-in URL" is one destination and the page offers it
                // twice, so honouring only one would leave the page contradicting itself.
                replace_plain_link_href_after(&mut result, "class=\"nav-links\"", v);
                replace_anchor_href_after(
                    &mut result,
                    "class=\"hero-actions\"",
                    "class=\"btn btn-outline\"",
                    v,
                );
            }
        }
        if let Some(v) = hp.get("nav_cta_text").and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                replace_anchor_inner_after(
                    &mut result,
                    "class=\"nav-links\"",
                    "class=\"btn btn-primary\"",
                    v,
                );
            }
        }
        if let Some(v) = hp.get("button_text").and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                replace_anchor_inner_after(
                    &mut result,
                    "class=\"hero-actions\"",
                    "class=\"btn btn-primary\"",
                    v,
                );
            }
        }
        if let Some(v) = hp.get("secondary_button_text").and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                replace_anchor_inner_after(
                    &mut result,
                    "class=\"hero-actions\"",
                    "class=\"btn btn-outline\"",
                    v,
                );
            }
        }
        if let Some(v) = hp.get("features_heading").and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                // The FIRST `<h2>` after the features section. The page carries three `<h2>`,
                // so the section anchor is what makes "the features heading" mean one element.
                replace_inner_after(
                    &mut result,
                    "<section class=\"features\" id=\"features\">",
                    "<h2",
                    "</h2>",
                    v,
                );
            }
        }
        if let Some(v) = hp.get("cta_heading").and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                replace_inner_after(&mut result, "<section class=\"cta\">", "<h2", "</h2>", v);
            }
        }
        if let Some(v) = hp.get("cta_text").and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                replace_inner_after(&mut result, "<section class=\"cta\">", "<p", "</p>", v);
            }
        }
        if let Some(v) = hp.get("footer_text").and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                // The footer's FIRST `<p>` — the one carrying the copyright and the SwiftSoftware
                // link. The legal-links `<p>` below it is shipped copy and stays.
                replace_inner_after(&mut result, "<footer>", "<p", "</p>", v);
            }
        }
        // NOTE: `homepage.features[]` is deliberately NOT read. Measured on the panel
        // (`www-admin/index.html`): there is no features editor at all — the payload posted
        // `features: []` on every save — and the six feature cards are a repeating structure no
        // single field can express, so the dead key was deleted from the payload, the defaults and
        // the row instead of being half-wired (kanban t_e8bfd1f3).
    }

    result
}

/// Point the `href` of the FIRST `<link rel="{rel}"` tag at `href`, inserting the attribute into a
/// tag that has none, or injecting a whole `<link>` into `<head>` when the page carries no such tag.
fn upsert_link_href(r: &mut String, rel: &str, href: &str) {
    let pat = format!("<link rel=\"{}\"", rel);
    match r.find(&pat) {
        None => inject_before_head_end(r, &format!("<link rel=\"{}\" href=\"{}\">", rel, href)),
        Some(p) => {
            // Bound the search to this one tag: a following tag's href must never be rewritten.
            let tag_end = match r[p..].find('>') {
                Some(e) => p + e,
                None => return,
            };
            match r[p..tag_end].find("href=\"") {
                Some(h) => {
                    let a = p + h + "href=\"".len();
                    match r[a..tag_end].find('"') {
                        Some(e) => r.replace_range(a..a + e, href),
                        None => r.insert_str(tag_end, &format!(" href=\"{}\"", href)),
                    }
                }
                None => r.insert_str(tag_end, &format!(" href=\"{}\"", href)),
            }
        }
    }
}

/// Replace the inner HTML of the FIRST `open`…`close` element with `value`, verbatim.
fn replace_inner(r: &mut String, open: &str, close: &str, value: &str) {
    let start = match r.find(open) {
        Some(p) => p + open.len(),
        None => return,
    };
    if let Some(e) = r[start..].find(close) {
        r.replace_range(start..start + e, value);
    }
}

/// Replace the inner HTML of the first `open_prefixed` element that appears AFTER `anchor`.
///
/// Used for the hero paragraph: the served page carries four `<p>` elements and only the one that
/// follows the `<h1>` is the hero, so an anchored lookup is what makes "the subheadline" mean the
/// same thing to the applier and to the operator.
fn replace_inner_after(r: &mut String, anchor: &str, open_prefix: &str, close: &str, value: &str) {
    let a = match r.find(anchor) {
        Some(p) => p + anchor.len(),
        None => return,
    };
    let op = match r[a..].find(open_prefix) {
        Some(p) => a + p,
        None => return,
    };
    let start = match r[op..].find('>') {
        Some(e) => op + e + 1,
        None => return,
    };
    if let Some(e) = r[start..].find(close) {
        r.replace_range(start..start + e, value);
    }
}

/// Replace the inner text of the FIRST `<a …>` element after `anchor` whose open tag contains
/// `needle` (kanban t_e8bfd1f3).
///
/// The nav CTA and the hero's two buttons are three of the page's six `class="btn …"` anchors, so
/// the element is located by the section that OWNS it (`class="nav-links"` / `class="hero-actions"`)
/// and its own button class — never by position, and never by the text it currently carries (which
/// is the value this field itself rewrites: a value-based locator would stop finding the element the
/// moment an operator set it).
fn replace_anchor_inner_after(r: &mut String, anchor: &str, needle: &str, value: &str) {
    let mut from = match r.find(anchor) {
        Some(p) => p + anchor.len(),
        None => return,
    };
    loop {
        let a = match r[from..].find("<a ") {
            Some(p) => from + p,
            None => return,
        };
        let gt = match r[a..].find('>') {
            Some(p) => a + p,
            None => return,
        };
        if r[a..gt].contains(needle) {
            let start = gt + 1;
            if let Some(e) = r[start..].find("</a>") {
                r.replace_range(start..start + e, value);
            }
            return;
        }
        from = gt + 1;
    }
}

/// Replace the `href` value of the FIRST `<a …>` element after `anchor` whose open tag contains
/// `needle` (kanban t_e8bfd1f3).
fn replace_anchor_href_after(r: &mut String, anchor: &str, needle: &str, href: &str) {
    let mut from = match r.find(anchor) {
        Some(p) => p + anchor.len(),
        None => return,
    };
    loop {
        let a = match r[from..].find("<a ") {
            Some(p) => from + p,
            None => return,
        };
        let gt = match r[a..].find('>') {
            Some(p) => a + p,
            None => return,
        };
        if r[a..gt].contains(needle) {
            if let Some(h) = r[a..gt].find("href=\"") {
                let start = a + h + "href=\"".len();
                if let Some(e) = r[start..].find('"') {
                    r.replace_range(start..start + e, href);
                }
            }
            return;
        }
        from = gt + 1;
    }
}

/// Replace the `href` of the FIRST *plain* link after `anchor`: an `<a>` that carries no `class`
/// attribute and does not point at a fragment (kanban t_e8bfd1f3).
///
/// The nav's sign-in link is the only nav anchor with no class — `#features` / `#extension` scroll
/// and the CTA is `class="btn btn-primary"` — so the sign-in element is located STRUCTURALLY. A
/// locator that matched the shipped `/login` URL instead would render the value once and then never
/// find the element again (the field would be one-shot), which is why the href's current value is
/// deliberately not consulted.
fn replace_plain_link_href_after(r: &mut String, anchor: &str, href: &str) {
    let mut from = match r.find(anchor) {
        Some(p) => p + anchor.len(),
        None => return,
    };
    loop {
        let a = match r[from..].find("<a ") {
            Some(p) => from + p,
            None => return,
        };
        let gt = match r[a..].find('>') {
            Some(p) => a + p,
            None => return,
        };
        let classless = !r[a..gt].contains("class=");
        let href_at = r[a..gt].find("href=\"");
        if classless {
            if let Some(h) = href_at {
                let start = a + h + "href=\"".len();
                if let Some(e) = r[start..].find('"') {
                    let is_fragment = r[start..start + e].starts_with('#');
                    if !is_fragment {
                        r.replace_range(start..start + e, href);
                        return;
                    }
                }
            }
        }
        from = gt + 1;
    }
}

/// Replace the logo anchor's TEXT NODE, keeping its icon box (kanban t_e8bfd1f3).
///
/// The served markup is
/// `<a href="/" class="logo"><div class="logo-icon">WS</div>WorkflowSwift</a>`: the icon box is
/// shipped markup the operator does not own — "Logo Text" is the wordmark — so the region this
/// field owns is the bytes between the icon's `</div>` and the anchor's `</a>`. Idempotent: after a
/// render the same `</div>` is still the icon's, so re-rendering substitutes the value with itself.
fn replace_logo_text(r: &mut String, value: &str) {
    let a = match r.find("class=\"logo\"") {
        Some(p) => p,
        None => return,
    };
    let gt = match r[a..].find('>') {
        Some(p) => a + p + 1,
        None => return,
    };
    // A logo anchor with no icon box owns everything up to its own close tag.
    let start = match r[gt..].find("</div>") {
        Some(p) => gt + p + "</div>".len(),
        None => gt,
    };
    if let Some(e) = r[start..].find("</a>") {
        r.replace_range(start..start + e, value);
    }
}

/// Replace the legal page's BODY: the bytes between its `</h1>` line and its `.back` footer.
///
/// In-place surgery (rather than a re-typed wrapper template) is what makes `render(row)`
/// byte-identical to the SERVED page for a reconciled row: everything outside the body — the
/// styles, the `<h1>`, the back-nav, the closing tags — is left exactly as published.
fn replace_legal_body(r: &mut String, body: &str) {
    let anchor = match r.find("<div class=\"container\">") {
        Some(p) => p,
        None => return,
    };
    let h1 = match r[anchor..].find("</h1>") {
        Some(e) => anchor + e + "</h1>".len(),
        None => return,
    };
    let start = if r[h1..].starts_with('\n') {
        h1 + 1
    } else {
        h1
    };
    let end = match r[start..].find("<div class=\"back\">") {
        Some(e) => start + e,
        None => return,
    };
    r.replace_range(start..end, body);
}

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
        inject_before_head_end(
            result,
            &format!(r#"  <meta name="{}" content="{}">"#, name, escaped),
        );
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
            inject_before_head_end(
                result,
                &format!(r#"  <meta property="{}" content="{}">"#, property, escaped),
            );
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
        inject_before_head_end(
            result,
            &format!(
                "  <script type=\"application/ld+json\">{}</script>",
                schema_json
            ),
        );
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

/// The code defaults, reconciled to the SERVED bytes of
/// `/opt/swift/nginx/www/workflowswift/index.html` (kanban t_0be214ec).
///
/// Every value the injector reads and WRITES into the page is here as the bytes the page already
/// carries, so a fresh install (no row) renders the published page byte-for-byte — the same
/// reconciliation the row received. The legal bodies stay `""`: a blank legal value is SKIPPED, so
/// an unconfigured install cannot blank the three published policy pages.
fn default_site_settings() -> serde_json::Value {
    json!({
        "title": "WorkflowSwift — Automate Everything. Visualize Everything.",
        "description": "WorkflowSwift is the all-in-one automation and data visualization platform. Build visual workflows, dynamic dashboards, and connect your tools without code.",
        "keywords": "workflow automation, data visualization, no-code, business process automation, dashboards, workflow builder",
        "og_title": "WorkflowSwift — Automate Everything. Visualize Everything.",
        "og_description": "Build visual workflows, dynamic dashboards, and connect your tools without code.",
        // The share image the SERVED page carries (`<meta property="og:image">`, added with the
        // other social-preview tags). It is here as the byte string the page already carries, like
        // every other value in this object, so an install with NO row renders the published homepage
        // byte-for-byte. It was `""` while the served page, the repo copy AND the live row all
        // carried the URL: the blank default DELETED the tag on every render, which is what made
        // `a_reconciled_row_leaves_the_live_homepage_byte_for_byte` red at HEAD (kanban t_79a4a151).
        "og_image_url": "https://workflowswift.com/assets/og-workflowswift.png",
        "favicon_url": "",
        "canonical_url": "https://workflowswift.com/",
        "ga_id": "",
        "gtm_id": "",
        "head_scripts": "",
        "body_scripts": "",
        // The served page's own JSON-LD block, verbatim — leading and trailing newline included,
        // because `upsert_schema` replaces the tag's inner text byte-for-byte.
        "schema_json": r#"
{
  "@context": "https://schema.org",
  "@type": "SoftwareApplication",
  "name": "WorkflowSwift",
  "description": "No-code automation and data visualization platform",
  "url": "https://workflowswift.com",
  "applicationCategory": "BusinessApplication",
  "operatingSystem": "Web",
  "offers": {
    "@type": "Offer",
    "price": "0",
    "priceCurrency": "USD"
  }
}
"#,
        "homepage": {
            // Every one of these is the SERVED byte string (kanban t_e8bfd1f3 derives them in
            // `audits/t_e8bfd1f3/10-populate-row.py` from the published page), so a fresh install —
            // no row at all — still renders the live homepage byte-for-byte. The three values that
            // carry markup are VERBATIM: escaping them would publish the accent spans as text.
            //
            // `homepage.features` is deliberately ABSENT: no panel editor ever set it (the console
            // posted `features: []` on every save) and nothing read it, so the key was deleted
            // rather than half-wired.
            "logo_text": "WorkflowSwift",
            // Blank is "leave the shipped /login hrefs alone" — the same arm `favicon_url` uses.
            "sign_in_url": "",
            "nav_cta_text": "Get Started",
            "headline": "Build workflows<br>that <span class=\"accent\">actually work</span>.",
            "subheadline": "No-code automation meets dynamic data dashboards. Connect your tools, build visual workflows, see everything in one place. WorkflowSwift turns your data into action.",
            "button_text": "Start Free →",
            "secondary_button_text": "Log In",
            "features_heading": "Everything you need to <span class=\"accent\">automate</span>",
            "cta_heading": "Ready to <span class=\"accent\">automate</span> your work?",
            "cta_text": "Get started in minutes. No credit card required.",
            "footer_text": "© 2026 WorkflowSwift — <a href=\"https://swiftsoftware.net\">A SwiftSoftware Company</a>"
        }
    })
}

fn require_admin(claims: &Claims) -> Result<(), AppError> {
    if !claims.perm_is_super_admin.unwrap_or(false) {
        return Err(AppError::Forbidden("Admin access required".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn served_index() -> Option<String> {
        fs::read_to_string("/opt/swift/nginx/www/workflowswift/index.html").ok()
    }

    #[test]
    fn a_reconciled_row_leaves_the_live_homepage_byte_for_byte() {
        // The reconciliation invariant: render(code defaults) == the SERVED page. Reads the real
        // served file when it is present (the applier host), skips otherwise so the container's
        // `cargo test` stays green.
        let Some(before) = served_index() else { return };
        let rendered = inject_site_settings(&before, &default_site_settings());
        if rendered != before {
            let a: Vec<&str> = before.lines().collect();
            let b: Vec<&str> = rendered.lines().collect();
            // Surface WHICH side moved instead of only that they differ: up to eight divergent
            // lines, both sides (kanban t_79a4a151). A line present on one side only is called out,
            // because that is the signature of a blank row value DELETING a served tag.
            let mut detail = String::new();
            let mut shown = 0usize;
            for i in 0..a.len().max(b.len()) {
                if a.get(i) == b.get(i) {
                    continue;
                }
                let (s, r) = (a.get(i).copied(), b.get(i).copied());
                let one_sided = matches!((s, r), (Some(x), _) if x.trim().is_empty())
                    || matches!((s, r), (_, Some(x)) if x.trim().is_empty());
                let note = if one_sided {
                    " (a tag is present on ONE side only — the other side is empty)"
                } else {
                    ""
                };
                detail.push_str(&format!(
                    "\n  line {}: served={:?} rendered={:?}{}",
                    i + 1,
                    s,
                    r,
                    note
                ));
                shown += 1;
                if shown >= 8 {
                    detail.push_str("\n  … (further divergent lines not shown)");
                    break;
                }
            }
            panic!(
                "the code defaults no longer render the served homepage byte-for-byte\n  \
                 served:   /opt/swift/nginx/www/workflowswift/index.html ({} line(s))\n  \
                 rendered: inject_site_settings(served, default_site_settings()) ({} line(s))\n  \
                 {} divergent line(s):{}",
                a.len(),
                b.len(),
                shown,
                detail
            );
        }
    }

    #[test]
    fn the_three_editors_now_change_the_page() {
        let html = "<head><link rel=\"icon\" href=\"/shipped.svg\">\
                    <link rel=\"canonical\" href=\"https://workflowswift.com/\"></head>\
                    <body><h1>old</h1><p>hero</p><p class=\"subtitle\">features</p></body>";
        let s = json!({
            "canonical_url": "https://example.com/probe",
            "favicon_url": "/probe.ico",
            "homepage": { "headline": "PROBE-H1", "subheadline": "PROBE-SUB" }
        });
        let out = inject_site_settings(html, &s);
        assert!(out.contains("<link rel=\"canonical\" href=\"https://example.com/probe\">"));
        assert!(out.contains("<link rel=\"icon\" href=\"/probe.ico\">"));
        assert!(out.contains("<h1>PROBE-H1</h1>"));
        assert!(out.contains("<p>PROBE-SUB</p>"));
        // The FEATURES subtitle must not be the element the subheadline editor moves.
        assert!(out.contains("<p class=\"subtitle\">features</p>"));
    }

    #[test]
    fn a_blank_value_leaves_the_shipped_element_alone() {
        let html = "<head><link rel=\"icon\" href=\"/shipped.svg\"></head>\
                    <body><h1>kept</h1><p>kept-hero</p></body>";
        let s = json!({
            "canonical_url": "   ",
            "favicon_url": "",
            "homepage": { "headline": "", "subheadline": "  " }
        });
        let out = inject_site_settings(html, &s);
        assert_eq!(out, html);
    }

    #[test]
    fn a_blank_og_image_does_not_inject_a_tag_and_og_type_is_never_deleted() {
        let html = "<head><meta property=\"og:type\" content=\"website\"></head><body></body>";
        // The reconciled default carries the served share image, so this test asks for a BLANK one
        // explicitly — the blank arm is what must not inject an empty `<meta … content="">`.
        let mut s = default_site_settings();
        s["og_image_url"] = json!("");
        let out = inject_site_settings(html, &s);
        assert!(
            !out.contains("og:image"),
            "a blank og_image_url must not inject an empty tag"
        );
        assert!(
            out.contains("<meta property=\"og:type\" content=\"website\">"),
            "og:type is served copy the settings row has no key for"
        );
        // …and the reconciled default (the URL the served page carries) DOES emit the tag, so a
        // fresh install reproduces the published homepage's share image (kanban t_79a4a151).
        let out = inject_site_settings(html, &default_site_settings());
        assert!(
            out.contains("og:image"),
            "the reconciled og_image_url must emit the served share-image tag: {out}"
        );
    }

    #[test]
    fn a_blank_legal_value_is_skipped_never_rendered() {
        let s = default_site_settings();
        let (targets, skipped) = plan(&s);
        // On the host (served root present) the index is the only target; the three legal bodies
        // are blank, so they are skipped with a reason and can never be blanked.
        let legal_targets = targets
            .iter()
            .filter(|(p, _)| p.ends_with(".html") && !p.ends_with("index.html"))
            .count();
        assert_eq!(legal_targets, 0);
        assert_eq!(
            skipped
                .iter()
                .filter(|(_, r)| r.contains("blank or absent"))
                .count(),
            3
        );
    }

    #[test]
    fn the_legal_body_replacement_is_in_place_and_idempotent() {
        let page = "<div class=\"container\">\n<h1>Terms of Service</h1>\nOLD BODY\n<div class=\"back\">x</div>\n</div>\n</body>\n</html>\n";
        // The region the applier owns is exactly the bytes between `</h1>\n` and
        // `<div class="back">` — trailing newline INCLUDED, which is the same slice the
        // reconciliation stores in the row. Replacing it with its own bytes is a no-op.
        let mut same = page.to_string();
        replace_legal_body(&mut same, "OLD BODY\n");
        assert_eq!(same, page);

        let mut r = page.to_string();
        replace_legal_body(&mut r, "NEW BODY\n");
        assert_eq!(
            r,
            "<div class=\"container\">\n<h1>Terms of Service</h1>\nNEW BODY\n<div class=\"back\">x</div>\n</div>\n</body>\n</html>\n",
            "everything outside the body must be untouched"
        );

        // Idempotent.
        let mut again = r.clone();
        replace_legal_body(&mut again, "NEW BODY\n");
        assert_eq!(again, r);

        // A page with no marker pair is returned unchanged rather than corrupted.
        let mut odd = "<html><body>no markers</body></html>".to_string();
        replace_legal_body(&mut odd, "X");
        assert_eq!(odd, "<html><body>no markers</body></html>");
    }

    // ── kanban t_e8bfd1f3: the REST of the homepage editors ──

    /// The 1-based served lines that differ from a render, with both sides.
    fn moved_lines<'a>(served: &'a str, out: &'a str) -> Vec<(usize, &'a str, &'a str)> {
        served
            .lines()
            .zip(out.lines())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| (i + 1, a, b))
            .collect()
    }

    /// The value of the first `href="…"` on a line.
    fn href_of(line: &str) -> Option<&str> {
        let at = line.find("href=\"")? + "href=\"".len();
        let len = line[at..].find('"')?;
        Some(&line[at..at + len])
    }

    /// Assert that injecting `probe` for `field` moved EXACTLY its own region: `owned` served
    /// line(s), each of them a line that carried this field's SHIPPED value (and, where two lines
    /// could carry it, `anchor`), and the page restored byte-for-byte by swapping the probe back.
    /// Every failure names the field and prints both sides of the offending line(s).
    fn assert_own_region(
        served: &str,
        out: &str,
        field: &str,
        probe: &str,
        shipped_value: &str,
        owned: usize,
        anchor: Option<&str>,
    ) {
        assert_eq!(
            served.lines().count(),
            out.lines().count(),
            "homepage.{field} changed the page's line count — an editor may only swap bytes in place"
        );
        let moved = moved_lines(served, out);
        assert_eq!(
            moved.len(),
            owned,
            "homepage.{field} moved {} served line(s), not {owned}: moved {:?}",
            moved.len(),
            moved.iter().map(|(n, _, _)| *n).collect::<Vec<usize>>()
        );
        for (n, before, after) in &moved {
            assert!(
                after.contains(probe),
                "homepage.{field} never reached served line {n}: {after:?}"
            );
            assert!(
                before.contains(shipped_value),
                "homepage.{field} moved served line {n}, which does not carry this editor's \
                 shipped value ({shipped_value:?}) — that is NOT this field's region: {before:?}"
            );
            if let Some(a) = anchor {
                assert!(
                    before.contains(a),
                    "homepage.{field} moved served line {n}, which carries no {a} — that is NOT \
                     this field's region: {before:?}"
                );
            }
        }
        let restored = out.replace(probe, shipped_value);
        if restored != served {
            let back = moved_lines(served, &restored);
            panic!(
                "homepage.{field} changed more than its own value — swapping {probe:?} back for \
                 {shipped_value:?} does not reproduce the served page; still divergent: {:?}",
                back.iter()
                    .map(|(n, b, a)| format!("line {n}: served={b:?} restored={a:?}"))
                    .collect::<Vec<String>>()
            );
        }
    }

    /// Every homepage editor the Site Configuration panel renders moves EXACTLY the served line(s)
    /// it owns. Measured on the REAL published page, so a reader that reached a sibling element — a
    /// second `<h2>`, the features subtitle, the legal-links paragraph — fails here.
    ///
    /// NO absolute line numbers (kanban t_79a4a151): the page legitimately GREW — the four
    /// social-preview tags (`<meta property="og:image">`, `og:image:width`, `og:image:height`,
    /// `<meta name="twitter:image">`) landed at lines 18-21 — and the coordinates this test used to
    /// carry (logo_text at 80, …) went stale, turning the suite red with no code change at all. The
    /// region is now pinned by what it IS: the moved served line must be one that carried THIS
    /// field's shipped value, and swapping the probe back must reproduce the served page
    /// byte-for-byte. The whole-page coordinate check is
    /// `a_reconciled_row_leaves_the_live_homepage_byte_for_byte`.
    #[test]
    fn every_homepage_editor_moves_only_its_own_region() {
        let Some(served) = served_index() else { return };
        let shipped = default_site_settings();
        // (field, probe, how many served lines this editor owns, an optional structural anchor the
        // moved line must carry). The anchor is only needed where the value leg cannot tell two
        // lines apart on its own: `logo_text`'s shipped value is the bare wordmark `WorkflowSwift`,
        // which the FOOTER's line carries too.
        let cases: Vec<(&str, &str, usize, Option<&str>)> = vec![
            ("logo_text", "ProbeBrand", 1, Some("class=\"logo\"")),
            ("nav_cta_text", "PROBE-NAV-CTA", 1, None),
            ("button_text", "PROBE-PRIMARY", 1, None),
            ("secondary_button_text", "PROBE-SECONDARY", 1, None),
            ("features_heading", "PROBE-FEATURES-H2", 1, None),
            ("cta_heading", "PROBE-CTA-H2", 1, None),
            ("cta_text", "PROBE-CTA-P", 1, None),
            ("footer_text", "PROBE-FOOTER", 1, None),
        ];
        for (field, probe, owned, anchor) in cases {
            let mut s = shipped.clone();
            s["homepage"][field] = json!(probe);
            let out = inject_site_settings(&served, &s);
            assert!(
                out.contains(probe),
                "homepage.{field} never reached the page"
            );
            let value = shipped["homepage"][field].as_str().unwrap();
            assert_own_region(&served, &out, field, probe, value, owned, anchor);
        }
        // The sign-in URL owns TWO served lines: the nav's plain link and the hero's outline button
        // — the page offers one destination twice. Its shipped row value is BLANK ("leave the
        // shipped /login hrefs alone"), so the hrefs it moves are read out of the moved lines
        // themselves: each line must be its own served bytes with ONLY the href swapped, and both
        // must be the SAME destination.
        let mut s = shipped.clone();
        s["homepage"]["sign_in_url"] = json!("https://example.com/probe-signin");
        let out = inject_site_settings(&served, &s);
        assert_eq!(
            served.lines().count(),
            out.lines().count(),
            "homepage.sign_in_url changed the page's line count"
        );
        let moved = moved_lines(&served, &out);
        assert_eq!(
            moved.len(),
            2,
            "homepage.sign_in_url owns two served lines (the nav link and the hero button); it \
             moved {:?}",
            moved.iter().map(|(n, _, _)| *n).collect::<Vec<usize>>()
        );
        let mut hrefs: Vec<String> = Vec::new();
        for (n, before, after) in &moved {
            let href = href_of(before)
                .unwrap_or_else(|| panic!("served line {n} carries no href: {before:?}"));
            assert_eq!(
                *after,
                before.replace(href, "https://example.com/probe-signin"),
                "homepage.sign_in_url must swap ONLY the href on served line {n}"
            );
            hrefs.push(href.to_string());
        }
        assert_eq!(
            hrefs[0], hrefs[1],
            "the two sign-in entry points must point at ONE destination"
        );
    }

    /// A cleared field can never blank a live page: every blank editor is a no-op on the served file.
    /// The settings start from the reconciled defaults — a settings object missing the SEO keys
    /// would legitimately move the head (`og_title` absent DELETES the tag), which is not what this
    /// card is about.
    #[test]
    fn a_blank_homepage_field_never_moves_the_shipped_page() {
        let Some(served) = served_index() else { return };
        let mut s = default_site_settings();
        for k in [
            "logo_text",
            "sign_in_url",
            "nav_cta_text",
            "button_text",
            "secondary_button_text",
            "features_heading",
            "cta_heading",
            "cta_text",
            "footer_text",
        ] {
            s["homepage"][k] = json!(if k == "sign_in_url" { "   " } else { "" });
        }
        assert_eq!(inject_site_settings(&served, &s), served);
    }

    /// The two locators that could have been written against the *value* they render — the sign-in
    /// href and the logo wordmark — are structural, so the field keeps working after the operator
    /// changes it (a value-based locator would make each field one-shot).
    #[test]
    fn the_sign_in_and_logo_locators_are_structural_not_value_based() {
        let html = "<nav><div class=\"nav-links\"><a href=\"#features\">Features</a>\
                    <a href=\"#extension\">Ext</a>\
                    <a href=\"https://app.example.com/login\">Log In</a>\
                    <a href=\"/register\" class=\"btn btn-primary\">Get Started</a></div></nav>\
                    <div class=\"hero-actions\">\
                    <a href=\"/register\" class=\"btn btn-primary\">Start</a>\
                    <a href=\"https://app.example.com/login\" class=\"btn btn-outline\">Log In</a></div>";
        let one = json!({"homepage": {"sign_in_url": "https://one.example.com/signin"}});
        let a = inject_site_settings(html, &one);
        assert!(
            a.contains("<a href=\"https://one.example.com/signin\">Log In</a>"),
            "the nav's plain link was not the element the field moved: {a}"
        );
        assert!(
            a.contains("<a href=\"https://one.example.com/signin\" class=\"btn btn-outline\">"),
            "the hero's outline button was not moved with it: {a}"
        );
        assert!(
            !a.contains("app.example.com"),
            "a shipped href survived: {a}"
        );
        assert!(
            a.contains("href=\"#features\"") && a.contains("href=\"#extension\""),
            "the scroll links are not sign-in URLs and must not be rewritten: {a}"
        );
        // A SECOND value still lands: the locator never consults the href it already wrote.
        let two = json!({"homepage": {"sign_in_url": "https://two.example.com/signin"}});
        let b = inject_site_settings(&a, &two);
        assert!(
            b.contains("<a href=\"https://two.example.com/signin\" class=\"btn btn-outline\">"),
            "sign_in_url stopped working after the first render: {b}"
        );
        assert!(
            !b.contains("one.example.com"),
            "the first value survived: {b}"
        );

        // The logo field owns the wordmark TEXT NODE only: the icon box stays, and re-rendering is
        // a byte-level no-op.
        let logo_html = "<a href=\"/\" class=\"logo\"><div class=\"logo-icon\">WS</div>Brand</a>";
        let logo = json!({"homepage": {"logo_text": "ProbeBrand"}});
        let l1 = inject_site_settings(logo_html, &logo);
        assert_eq!(
            l1,
            "<a href=\"/\" class=\"logo\"><div class=\"logo-icon\">WS</div>ProbeBrand</a>"
        );
        let l2 = inject_site_settings(&l1, &logo);
        assert_eq!(l1, l2, "the logo render must be idempotent");
    }

    /// `homepage.features[]` had no editor and no reader; the key is gone from the defaults, and the
    /// eleven fields the panel DOES render are all present.
    #[test]
    fn the_dead_features_key_is_gone_from_the_defaults() {
        let hp = &default_site_settings()["homepage"];
        assert!(
            hp.get("features").is_none(),
            "homepage.features has no editor and no reader; it must not be in the defaults"
        );
        for k in [
            "logo_text",
            "sign_in_url",
            "nav_cta_text",
            "button_text",
            "secondary_button_text",
            "features_heading",
            "cta_heading",
            "cta_text",
            "footer_text",
            "headline",
            "subheadline",
        ] {
            assert!(hp.get(k).is_some(), "missing homepage.{k}");
        }
    }

    /// A page with none of the markers is returned unchanged rather than corrupted.
    #[test]
    fn the_new_surgery_helpers_leave_a_foreign_page_untouched() {
        let html = "<html><body><h1>x</h1></body></html>";
        let mut r = html.to_string();
        replace_logo_text(&mut r, "X");
        replace_plain_link_href_after(&mut r, "class=\"nav-links\"", "/x");
        replace_anchor_href_after(
            &mut r,
            "class=\"hero-actions\"",
            "class=\"btn btn-outline\"",
            "/x",
        );
        replace_anchor_inner_after(
            &mut r,
            "class=\"nav-links\"",
            "class=\"btn btn-primary\"",
            "X",
        );
        assert_eq!(r, html);
    }
}
