use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::time::Duration;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::models::competitor_watch::*;
use crate::AppState;

/// How many redirect hops a check will follow. Each hop is re-gated through the app's SSRF guard, so
/// a public page that bounces to `127.0.0.1` is refused rather than fetched.
const MAX_REDIRECTS: usize = 3;
/// The most of a page a check will read. A snapshot is a fingerprint, not an archive.
const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
const FETCH_TIMEOUT_SECS: u64 = 20;
const FETCH_UA: &str = "WorkflowSwift-CompetitorWatch/1.0 (+https://workflowswift.com/guide.html)";
/// The most characters `competitors.name` accepts from the form.
///
/// `competitors.name` is `varchar(255)` and this writer had no bound at all, so a 300-character
/// name reached the INSERT, Postgres refused it and the customer got
/// `500 {"message":"Database error"}` while the console's Add-competitor modal had accepted every
/// character they typed (measured live 2026-10-30). The cap stays LOOSER than the column on
/// purpose — the application must never refuse a value the database would have stored.
const MAX_NAME_CHARS: usize = 200;

/// Trim a competitor name and bound it to what the column can hold.
fn competitor_name(raw: &str) -> ApiResult<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(AppError::Validation(
            "Enter a name for this competitor.".to_string(),
        ));
    }
    let len = name.chars().count();
    if len > MAX_NAME_CHARS {
        return Err(AppError::Validation(format!(
            "A competitor name can be at most {MAX_NAME_CHARS} characters — this one is {len}."
        )));
    }
    Ok(name.to_string())
}

pub async fn list_competitors(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let competitors = sqlx::query_as::<_, Competitor>(
        "SELECT * FROM competitors WHERE aid = $1 ORDER BY name ASC",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"competitors": competitors})))
}

pub async fn create_competitor(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Refused BEFORE the first write: an over-long name used to be a 500 from Postgres.
    let name = competitor_name(req.get("name").and_then(|v| v.as_str()).unwrap_or(""))?;

    let website = req
        .get("website")
        .and_then(|v| v.as_str())
        .map(String::from);
    let description = req
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);

    let strengths: Option<Vec<String>> =
        req.get("strengths").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        });

    let weaknesses: Option<Vec<String>> =
        req.get("weaknesses").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        });

    let market_share = req.get("market_share").and_then(|v| v.as_f64());

    let competitor = sqlx::query_as::<_, Competitor>(
        r#"INSERT INTO competitors (id, aid, name, website, description, strengths, weaknesses, market_share, is_active)
           VALUES ($1, $2, $3, $4, $5, $6::text[], $7::text[], $8, true)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(&name)
    .bind(&website)
    .bind(&description)
    .bind(&strengths)
    .bind(&weaknesses)
    .bind(market_share)
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"competitor": competitor}))))
}

pub async fn update_competitor(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let _existing =
        sqlx::query_as::<_, Competitor>("SELECT * FROM competitors WHERE id = $1 AND aid = $2")
            .bind(id)
            .bind(aid)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound("Competitor not found".to_string()))?;

    if let Some(raw) = req.get("name").and_then(|v| v.as_str()) {
        // Same guard as the create door: a rename must not become a 500 either.
        let name = competitor_name(raw)?;
        sqlx::query("UPDATE competitors SET name = $1 WHERE id = $2")
            .bind(&name)
            .bind(id)
            .execute(&state.db)
            .await?;
    }

    if let Some(website) = req.get("website") {
        let val = website.as_str().map(String::from);
        sqlx::query("UPDATE competitors SET website = $1 WHERE id = $2")
            .bind(val)
            .bind(id)
            .execute(&state.db)
            .await?;
    }

    if let Some(description) = req.get("description") {
        let val = description.as_str().map(String::from);
        sqlx::query("UPDATE competitors SET description = $1 WHERE id = $2")
            .bind(val)
            .bind(id)
            .execute(&state.db)
            .await?;
    }

    if let Some(strengths) = req.get("strengths").and_then(|v| v.as_array()) {
        let val: Vec<String> = strengths
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        sqlx::query("UPDATE competitors SET strengths = $1::text[] WHERE id = $2")
            .bind(&val)
            .bind(id)
            .execute(&state.db)
            .await?;
    }

    if let Some(weaknesses) = req.get("weaknesses").and_then(|v| v.as_array()) {
        let val: Vec<String> = weaknesses
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        sqlx::query("UPDATE competitors SET weaknesses = $1::text[] WHERE id = $2")
            .bind(&val)
            .bind(id)
            .execute(&state.db)
            .await?;
    }

    if let Some(market_share) = req.get("market_share") {
        sqlx::query("UPDATE competitors SET market_share = $1 WHERE id = $2")
            .bind(market_share.as_f64())
            .bind(id)
            .execute(&state.db)
            .await?;
    }

    if let Some(is_active) = req.get("is_active").and_then(|v| v.as_bool()) {
        sqlx::query("UPDATE competitors SET is_active = $1 WHERE id = $2")
            .bind(is_active)
            .bind(id)
            .execute(&state.db)
            .await?;
    }

    sqlx::query("UPDATE competitors SET updated_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    let competitor = sqlx::query_as::<_, Competitor>("SELECT * FROM competitors WHERE id = $1")
        .bind(id)
        .fetch_one(&state.db)
        .await?;

    Ok(Json(json!({"competitor": competitor})))
}

pub async fn delete_competitor(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM competitors WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Competitor not found".to_string()));
    }

    Ok(Json(json!({"message": "Competitor deleted successfully"})))
}

pub async fn check_competitor(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let competitor = sqlx::query_as::<_, Competitor>(
        "SELECT * FROM competitors WHERE id = $1 AND aid = $2 AND is_active = true",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(
        "Active competitor not found".to_string(),
    ))?;

    // A check has to FETCH something. Without a website there is nothing to compare, and pretending
    // otherwise is what this endpoint used to do: it answered "No significant changes detected."
    // without a single request.
    let website = competitor
        .website
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    if website.is_empty() {
        return Err(AppError::BadRequest(
            "Add the competitor's website first — a check has to fetch something real.".to_string(),
        ));
    }

    let (final_url, body) = fetch_page(&website).await?;
    let digest = sha256_hex(&body);
    let size = body.len() as i64;

    // What the LAST check saw. NULL means never checked, and that is answered as a baseline rather
    // than as "unchanged": a comparison that could not be made is not a comparison that passed.
    let prev = sqlx::query("SELECT snapshot_hash, snapshot_bytes, snapshot_at FROM competitors WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .fetch_one(&state.db)
        .await?;
    let prev_hash: Option<String> = prev.try_get("snapshot_hash").ok().flatten();
    let prev_bytes: Option<i64> = prev.try_get("snapshot_bytes").ok().flatten();
    let prev_at: Option<chrono::DateTime<Utc>> = prev.try_get("snapshot_at").ok().flatten();

    let (status, changes_detected, summary) = match prev_hash {
        None => (
            "baseline".to_string(),
            false,
            format!(
                "First check of {final_url} — saved a baseline of {size} bytes. Run the check again to detect changes."
            ),
        ),
        Some(h) if h == digest => (
            "completed".to_string(),
            false,
            format!(
                "No change at {final_url} since the last check{} ({size} bytes).",
                prev_at
                    .map(|t| format!(" on {}", t.format("%Y-%m-%d %H:%M UTC")))
                    .unwrap_or_default()
            ),
        ),
        Some(_) => (
            "completed".to_string(),
            true,
            format!(
                "{final_url} changed since the last check{}: {size} bytes now, {} before.",
                prev_at
                    .map(|t| format!(" on {}", t.format("%Y-%m-%d %H:%M UTC")))
                    .unwrap_or_default(),
                prev_bytes
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            ),
        ),
    };

    sqlx::query(
        "UPDATE competitors SET snapshot_hash = $2, snapshot_bytes = $3, snapshot_at = NOW(), \
         last_checked_at = NOW(), updated_at = NOW() WHERE id = $1",
    )
    .bind(id)
    .bind(&digest)
    .bind(size)
    .execute(&state.db)
    .await?;

    let result = CompetitorCheckResult {
        competitor_id: competitor.id,
        name: competitor.name.clone(),
        status,
        changes_detected,
        summary,
        checked_at: Utc::now(),
    };

    Ok(Json(json!({
        "check_result": result,
        "url_checked": final_url,
        "sample_data": false,
    })))
}

/// Fetch one page, refusing anything the app's SSRF gate rejects and re-gating every redirect hop.
///
/// `security::webhook_security::gate_step_destination` is the fleet's own guard for a
/// customer-supplied URL (it refuses loopback, private and link-local targets), and it is the reason
/// this cannot be used to read the box's own services. The client follows NO redirects itself: a
/// 302 to `127.0.0.1` would otherwise bypass that gate, so each hop is resolved and re-gated here.
async fn fetch_page(start: &str) -> ApiResult<(String, Vec<u8>)> {
    use crate::security::webhook_security;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(FETCH_UA)
        .build()
        .map_err(|e| AppError::Internal(format!("competitor fetch client: {e}")))?;

    let mut current = reqwest::Url::parse(start).map_err(|_| {
        AppError::BadRequest(
            "That is not a full web address — it must start with http:// or https://".to_string(),
        )
    })?;

    for hop in 0..=MAX_REDIRECTS {
        webhook_security::gate_step_destination(current.as_str())
            .await
            .map_err(|e| AppError::BadRequest(format!("That address cannot be checked: {e}")))?;

        let resp = client.get(current.as_str()).send().await.map_err(|e| {
            AppError::UpstreamRefused(format!(
                "{} could not be reached ({e}). Nothing was compared.",
                current
            ))
        })?;

        let status = resp.status();

        if status.is_redirection() {
            let next = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string());
            let Some(next) = next else {
                return Err(AppError::UpstreamRefused(format!(
                    "{} redirected (HTTP {}) without saying where. Nothing was compared.",
                    current,
                    status.as_u16()
                )));
            };
            if hop == MAX_REDIRECTS {
                return Err(AppError::UpstreamRefused(format!(
                    "{} redirected more than {MAX_REDIRECTS} times. Nothing was compared.",
                    start
                )));
            }
            current = current.join(&next).map_err(|_| {
                AppError::UpstreamRefused(format!(
                    "{} redirected to an address that could not be read ({next}).",
                    current
                ))
            })?;
            continue;
        }

        if !status.is_success() {
            return Err(AppError::UpstreamRefused(format!(
                "{} answered HTTP {}. Nothing was compared.",
                current,
                status.as_u16()
            )));
        }

        let bytes = resp.bytes().await.map_err(|e| {
            AppError::UpstreamRefused(format!("{} stopped sending its page ({e}).", current))
        })?;
        let mut body = bytes.to_vec();
        body.truncate(MAX_SNAPSHOT_BYTES);
        return Ok((current.to_string(), body));
    }

    Err(AppError::UpstreamRefused(format!(
        "{} could not be read. Nothing was compared.",
        start
    )))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
