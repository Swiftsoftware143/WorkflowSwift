use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::models::competitor_watch::*;
use crate::AppState;

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

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if name.is_empty() {
        return Err(AppError::Validation("name is required".to_string()));
    }

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

    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE competitors SET name = $1 WHERE id = $2")
            .bind(name)
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

    // Update last_checked_at
    sqlx::query("UPDATE competitors SET last_checked_at = NOW(), updated_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    // Mock check result
    let result = CompetitorCheckResult {
        competitor_id: competitor.id,
        name: competitor.name.clone(),
        status: "completed".to_string(),
        changes_detected: false,
        summary: format!(
            "Checked competitor '{}'. No significant changes detected.",
            competitor.name
        ),
        checked_at: Utc::now(),
    };

    Ok(Json(json!({"check_result": result})))
}
