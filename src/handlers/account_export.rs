//! `account_export` — `GET /api/v1/account/export`: the customer's OWN copy of their account data.
//!
//! Parity with FunnelSwift's `/api/v1/account/export`. Before this route the app had no
//! account-level export at all: the only downloads were per-screen CSVs of one list, so a customer
//! could not obtain what the workspace holds about them in one file — and a customer leaving could
//! not take their data with them.
//!
//! # Scope
//!
//! The account id comes from the TOKEN (`claims.aid`), never from a path segment, a query string
//! or a body, so there is nothing to spoof: one account can only ever receive its own document.
//! Every collection below is filtered on the same `aid` column the app's own list handlers use.
//!
//! # What is deliberately NOT in the file
//!
//! Nothing that could be replayed to sign in as somebody, and no provider secret:
//! `users.password_hash`, `api_keys.key_hash` / `key_encrypted`, `accounts.hexomatic_key` and
//! `accounts.settings` (which holds provider/branding internals), payment-provider credentials,
//! and every email body. The document states that in its own `excluded` field, so the customer is
//! not left guessing what was left out.
//!
//! # Shape
//!
//! One pretty-printed JSON object, served as an attachment:
//! `{ format, exported_at, account:[…], counts:{…}, excluded, <collection>:[…] }`.
//! `counts` carries the row count per collection so the customer (and a support engineer) can see
//! at a glance that the file is complete rather than silently truncated.

use axum::{
    extract::{Extension, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::state::AppState;

/// Every collection in the export: the key it appears under, and the query that reads it.
///
/// All of them are `aid`-scoped (the account id from the token). `json_agg` means one round trip
/// per collection and no row-by-row marshalling in Rust; `coalesce(…, '[]')` is what makes an
/// empty account export `[]` instead of `null`.
///
/// Column lists are explicit, never `SELECT *`: a new secret column added to a table later must
/// not silently start leaving the building through this route.
const COLLECTIONS: &[(&str, &str)] = &[
    (
        "account",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, account_slug, industry_slug, is_active, created_at, updated_at
               FROM accounts WHERE id = $1) x",
    ),
    (
        "plan",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT ap.id, ap.plan_id, pt.slug AS plan_slug, pt.name AS plan_name,
                    ap.status, ap.started_at, ap.expires_at, ap.is_active, ap.created_at
               FROM account_plans ap
               LEFT JOIN plan_tiers pt ON pt.id = ap.plan_id
              WHERE ap.aid = $1) x",
    ),
    (
        "workspaces",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, is_default, created_at, updated_at
               FROM workspaces WHERE tenant_id = $1) x",
    ),
    (
        "users",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, email, name, role, is_active, last_login_at, created_at, updated_at,
                    closed_at
               FROM users WHERE aid = $1) x",
    ),
    (
        "workflows",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, description, category, tags, is_active, trigger_type, created_at,
                    updated_at
               FROM workflows WHERE aid = $1) x",
    ),
    (
        "leads",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, email, phone, company, status, source, notes, created_at, updated_at
               FROM leads WHERE aid = $1) x",
    ),
    (
        "clients",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, email, phone, website, industry, notes, is_active, created_at,
                    updated_at
               FROM clients WHERE aid = $1) x",
    ),
    (
        "tags",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, color, is_active, created_at FROM tags WHERE aid = $1) x",
    ),
    (
        "automations",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, description, trigger_type, trigger_config, action_type,
                    action_config, is_active, created_at
               FROM automations WHERE aid = $1) x",
    ),
    (
        "invoices",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, amount, status, due_date, paid_at, is_active, created_at
               FROM invoices WHERE aid = $1) x",
    ),
    (
        "brand_monitors",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, brand_name, keywords, platforms, is_active, created_at, updated_at,
                    last_scanned_at
               FROM brand_monitors WHERE aid = $1) x",
    ),
    (
        "prospecting_leads",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, business_name, website, industry, location, estimated_size, contact_email,
                    contact_phone, enriched, created_at
               FROM prospecting_leads WHERE aid = $1) x",
    ),
    (
        "api_keys",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, prefix, permissions, last_used_at, expires_at, is_active, created_at
               FROM api_keys WHERE aid = $1) x",
    ),
];

/// What a customer must be told about their own copy: what is NOT in it, and why.
const EXCLUDED: &str = "Nothing that could be replayed to sign in as somebody or to act as this \
     workspace is included: stored passwords and password hashes, API-key secrets (each key's \
     PREFIX and its dates are here), provider credentials, payment-provider secrets and the bodies \
     of individual emails. Ask support if you need one of those in a different form.";

/// `GET /api/v1/account/export`
pub async fn export_account(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> ApiResult<Response> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let mut out = Map::new();
    let mut counts = Map::new();

    for (key, sql) in COLLECTIONS {
        let raw: String = sqlx::query_scalar(sql)
            .bind(account_id)
            .fetch_one(&state.pool)
            .await?;
        let rows: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!([]));
        counts.insert(
            (*key).to_string(),
            json!(rows.as_array().map(|a| a.len()).unwrap_or(0)),
        );
        out.insert((*key).to_string(), rows);
    }

    out.insert("counts".to_string(), Value::Object(counts));
    out.insert("excluded".to_string(), json!(EXCLUDED));
    out.insert(
        "exported_at".to_string(),
        json!(chrono::Utc::now().to_rfc3339()),
    );
    out.insert(
        "format".to_string(),
        json!("workflowswift-account-export/1"),
    );

    let body = serde_json::to_string_pretty(&Value::Object(out))
        .map_err(|e| AppError::Internal(format!("export encode error: {e}")))?;

    let filename = format!(
        "workflowswift-account-{}.json",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
        .map_err(|e| AppError::Internal(format!("export header error: {e}")))?;

    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        body,
    )
        .into_response())
}
