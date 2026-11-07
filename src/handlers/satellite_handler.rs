//! The five data feeds the Industry Sources panel has always advertised.
//!
//! `industry_sources_handler::seed_industry_sources` seeds every industry with five sources whose
//! `endpoint` points back at this app:
//!
//!   /api/v1/satellite/market-research · /news · /competitors · /leads · /trends
//!
//! Measured live 2026-11-06: all five answered 404 while a real route (`/api/v1/industries`)
//! answered 200, and the app carried **115** `industry_data_sources` rows across 23 industries —
//! every one of them naming a refresh cadence, a credit cost and `is_active = true` for an endpoint
//! that resolved nowhere. The console showed an operator a catalogue of live feeds; nothing was
//! behind any of them.
//!
//! This module is the missing half. Each feed reads the store the app ALREADY keeps for that
//! subject, scoped to the caller's own account, so the catalogue's declared endpoint is true:
//!
//!   market-research -> `prospecting_results`  (businesses found for a search)
//!   news            -> `brand_monitor_results` (brand mentions with source + snippet)
//!   competitors     -> `competitors`           (the competitor roster the console edits)
//!   leads           -> `leads`                 (the lead store)
//!   trends          -> `dashboard_data`        (the metric time series)
//!
//! Nothing here invents a business rule: every field returned is a column already stored. The
//! endpoints are authenticated and take the account from the token — there is no account id in any
//! path, so one tenant cannot read another's rows.
//!
//! Timestamps and numerics are selected `::text` on purpose: the response is a feed payload, not a
//! typed model, and a cast keeps a future column-type change from breaking the read.

use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::{postgres::PgRow, Row};
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::AppState;

/// Rows one feed returns when the caller does not ask. Bounded so a large account cannot make a
/// refresh pull the whole table.
const DEFAULT_LIMIT: i64 = 50;
/// The hard ceiling, looser than the default on purpose.
const MAX_LIMIT: i64 = 200;

#[derive(Debug, Deserialize)]
pub struct FeedQuery {
    /// Optional industry filter. Kept in the contract because every seeded source is per-industry;
    /// the stores below are not partitioned by industry, so it is echoed back rather than applied.
    pub industry: Option<String>,
    pub limit: Option<i64>,
}

impl FeedQuery {
    fn limit(&self) -> i64 {
        self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }
}

fn account_id(claims: &Claims) -> ApiResult<Uuid> {
    Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)
}

/// A nullable text column, widened to "absent" rather than "empty string" so a caller can tell a
/// blank field from a missing one in the same way the rest of this app's reads do.
fn text(row: &PgRow, col: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(col).ok().flatten()
}

/// `GET /api/v1/satellite/competitors` — the competitor roster this account has recorded.
pub async fn competitors(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<FeedQuery>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let rows = sqlx::query(
        "SELECT id::text AS id, name, website, description,
                created_at::text AS created_at, last_checked_at::text AS last_checked_at,
                is_active
           FROM competitors
          WHERE aid = $1
          ORDER BY name
          LIMIT $2",
    )
    .bind(aid)
    .bind(q.limit())
    .fetch_all(&s.db)
    .await?;

    let items: Vec<_> = rows
        .iter()
        .map(|r| {
            json!({
                "id": text(r, "id"),
                "name": text(r, "name"),
                "website": text(r, "website"),
                "description": text(r, "description"),
                "created_at": text(r, "created_at"),
                "last_checked_at": text(r, "last_checked_at"),
                "is_active": r.try_get::<Option<bool>, _>("is_active").ok().flatten(),
            })
        })
        .collect();

    Ok(Json(feed("competitors", &q, items)))
}

/// `GET /api/v1/satellite/leads` — the lead store.
pub async fn leads(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<FeedQuery>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let rows = sqlx::query(
        "SELECT id::text AS id, name, email, phone, company, status, source,
                created_at::text AS created_at
           FROM leads
          WHERE aid = $1
          ORDER BY created_at DESC
          LIMIT $2",
    )
    .bind(aid)
    .bind(q.limit())
    .fetch_all(&s.db)
    .await?;

    let items: Vec<_> = rows
        .iter()
        .map(|r| {
            json!({
                "id": text(r, "id"),
                "name": text(r, "name"),
                "email": text(r, "email"),
                "phone": text(r, "phone"),
                "company": text(r, "company"),
                "status": text(r, "status"),
                "source": text(r, "source"),
                "created_at": text(r, "created_at"),
            })
        })
        .collect();

    Ok(Json(feed("leads", &q, items)))
}

/// `GET /api/v1/satellite/news` — brand mentions, newest first. This is the feed behind the
/// `news_feed` / `rss` source: each stored mention already carries the publisher and a snippet.
pub async fn news(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<FeedQuery>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let rows = sqlx::query(
        "SELECT r.id::text AS id, r.source, r.source_url, r.title, r.snippet, r.sentiment,
                r.published_at::text AS published_at, r.fetched_at::text AS fetched_at
           FROM brand_monitor_results r
          WHERE r.tenant_id = $1
          ORDER BY COALESCE(r.published_at, r.fetched_at) DESC
          LIMIT $2",
    )
    .bind(aid)
    .bind(q.limit())
    .fetch_all(&s.db)
    .await?;

    let items: Vec<_> = rows
        .iter()
        .map(|r| {
            json!({
                "id": text(r, "id"),
                "source": text(r, "source"),
                "source_url": text(r, "source_url"),
                "title": text(r, "title"),
                "snippet": text(r, "snippet"),
                "sentiment": text(r, "sentiment"),
                "published_at": text(r, "published_at"),
                "fetched_at": text(r, "fetched_at"),
            })
        })
        .collect();

    Ok(Json(feed("news", &q, items)))
}

/// `GET /api/v1/satellite/trends` — the account's metric time series, newest first.
///
/// `metric_value` is cast to text so the read does not depend on whether the column is numeric,
/// double precision or text today.
pub async fn trends(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<FeedQuery>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let rows = sqlx::query(
        "SELECT metric_key, metric_value::text AS metric_value,
                recorded_at::text AS recorded_at, trigger_source
           FROM dashboard_data
          WHERE aid = $1 AND is_active IS DISTINCT FROM false
          ORDER BY recorded_at DESC
          LIMIT $2",
    )
    .bind(aid)
    .bind(q.limit())
    .fetch_all(&s.db)
    .await?;

    let items: Vec<_> = rows
        .iter()
        .map(|r| {
            json!({
                "metric_key": text(r, "metric_key"),
                "metric_value": text(r, "metric_value"),
                "recorded_at": text(r, "recorded_at"),
                "trigger_source": text(r, "trigger_source"),
            })
        })
        .collect();

    Ok(Json(feed("trends", &q, items)))
}

/// `GET /api/v1/satellite/market-research` — businesses found for this account's searches.
pub async fn market_research(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<FeedQuery>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let rows = sqlx::query(
        "SELECT id::text AS id, business_name, business_website, business_phone, business_email,
                business_address, source, source_url, rating::text AS rating,
                review_count, fetched_at::text AS fetched_at
           FROM prospecting_results
          WHERE tenant_id = $1
          ORDER BY fetched_at DESC
          LIMIT $2",
    )
    .bind(aid)
    .bind(q.limit())
    .fetch_all(&s.db)
    .await?;

    let items: Vec<_> = rows
        .iter()
        .map(|r| {
            json!({
                "id": text(r, "id"),
                "business_name": text(r, "business_name"),
                "business_website": text(r, "business_website"),
                "business_phone": text(r, "business_phone"),
                "business_email": text(r, "business_email"),
                "business_address": text(r, "business_address"),
                "source": text(r, "source"),
                "source_url": text(r, "source_url"),
                "rating": text(r, "rating"),
                "review_count": r.try_get::<Option<i32>, _>("review_count").ok().flatten(),
                "fetched_at": text(r, "fetched_at"),
            })
        })
        .collect();

    Ok(Json(feed("market-research", &q, items)))
}

/// One response shape for all five, so a refresh runner can poll any source the catalogue names
/// with the same parser. `count` is the number of items returned, not a table total — the store may
/// hold more than the page this call asked for.
fn feed(source: &str, q: &FeedQuery, items: Vec<serde_json::Value>) -> serde_json::Value {
    json!({
        "source": source,
        "industry": q.industry,
        "limit": q.limit(),
        "count": items.len(),
        "items": items,
    })
}
