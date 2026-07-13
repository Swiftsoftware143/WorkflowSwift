use axum::{
    extract::{State, Json},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::features;
use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::models::tag::*;

pub async fn list_tags(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let tags = sqlx::query_as::<_, Tag>(
        "SELECT * FROM tags WHERE aid = $1 ORDER BY name ASC",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"tags": tags})))
}

pub async fn create_tag(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_tags", "Tags").await?;

    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let color = req.get("color").and_then(|v| v.as_str()).map(|s| s.to_string());

    if name.is_empty() {
        return Err(AppError::Validation("Tag name is required".to_string()));
    }

    // Check duplicate
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM tags WHERE aid = $1 AND name = $2",
    )
    .bind(aid)
    .bind(&name)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if existing > 0 {
        return Err(AppError::Duplicate("Tag already exists".to_string()));
    }

    let tag = sqlx::query_as::<_, Tag>(
        r#"INSERT INTO tags (id, aid, name, color) VALUES ($1, $2, $3, $4) RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(&name)
    .bind(&color)
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"tag": tag}))))
}

pub async fn assign_tag(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AssignTagRequest>,
) -> ApiResult<impl IntoResponse> {
    let _aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Check tag exists and belongs to account
    let _tag = sqlx::query_as::<_, Tag>(
        "SELECT * FROM tags WHERE id = $1",
    )
    .bind(req.tag_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Tag not found".to_string()))?;

    // Check if already assigned
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM tag_assignments WHERE tag_id = $1 AND entity_type = $2 AND entity_id = $3",
    )
    .bind(req.tag_id)
    .bind(&req.entity_type)
    .bind(req.entity_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if existing > 0 {
        return Ok((StatusCode::OK, Json(json!({"message": "Already assigned"}))));
    }

    sqlx::query(
        "INSERT INTO tag_assignments (id, tag_id, entity_type, entity_id) VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(req.tag_id)
    .bind(&req.entity_type)
    .bind(req.entity_id)
    .execute(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"message": "Tag assigned"}))))
}

pub async fn unassign_tag(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<UnassignTagRequest>,
) -> ApiResult<impl IntoResponse> {
    let _aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    sqlx::query(
        "DELETE FROM tag_assignments WHERE tag_id = $1 AND entity_type = $2 AND entity_id = $3",
    )
    .bind(req.tag_id)
    .bind(&req.entity_type)
    .bind(req.entity_id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({"message": "Tag unassigned"})))
}
