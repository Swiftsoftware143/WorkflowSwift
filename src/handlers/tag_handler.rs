use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::features;
use crate::models::tag::*;
use crate::AppState;

/// `tags.name` is `varchar(100)` in this database — NOT the 255 the fleet's other apps use — so the
/// cap is read from the column, never copied from a sibling.
const TAG_NAME_CHARS: usize = 100;
/// `tags.color` is `varchar(7)` (a `#RRGGBB` hex string is exactly 7).
const TAG_COLOR_CHARS: usize = 7;

pub async fn list_tags(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let tags = sqlx::query_as::<_, Tag>("SELECT * FROM tags WHERE aid = $1 ORDER BY name ASC")
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

    let raw_name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if raw_name.is_empty() {
        return Err(AppError::Validation("Tag name is required".to_string()));
    }
    // `tags.name` is varchar(100) and `tags.color` varchar(7) (see \d tags) and neither length was
    // checked, so an over-long value reached the INSERT and came back as the raw
    // `500 {"code":500,"error":true,"message":"Database error"}` — measured live 2026-10-17 on this
    // route: a 101-character name, and separately an 8-character colour. Refuse BEFORE the write, as
    // the 422 the console can read, and bind the trimmed value (names are rendered into mail
    // subjects and HTML). Same class the display-name module closed for people's names.
    let name = crate::security::display_name::normalize_capped(raw_name, TAG_NAME_CHARS, "name")
        .map_err(AppError::Validation)?;
    let color = req
        .get("color")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if let Some(c) = color.as_deref() {
        if c.chars().count() > TAG_COLOR_CHARS {
            return Err(AppError::Validation(format!(
                "color: is longer than {TAG_COLOR_CHARS} characters"
            )));
        }
    }

    // Check duplicate
    let existing =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tags WHERE aid = $1 AND name = $2")
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
    let _tag = sqlx::query_as::<_, Tag>("SELECT * FROM tags WHERE id = $1")
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

    Ok((
        StatusCode::CREATED,
        Json(json!({"message": "Tag assigned"})),
    ))
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
