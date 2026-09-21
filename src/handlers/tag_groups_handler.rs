//! Tag groups handler — real per-account tag groups.
//!
//! Replaces the auto-generated stub (kanban t_01fa9bbc) which read a `tag_groups` relation that
//! no migration ever created, was not auth/tenant-scoped, and swallowed the missing-relation
//! error with `unwrap_or_default()` — so GET answered an empty 200 list while POST/PUT/DELETE
//! answered 500 `relation "tag_groups" does not exist`. The admin shell ships the screen
//! (Tags & Labels -> Tag Groups) and posts `name` + `description`.
//! Storage: migrations/054_create_tag_groups.sql. Mirrors the tickets/leads handler pattern.
use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, FromRow)]
pub struct TagGroup {
    pub id: Uuid,
    pub aid: Uuid,
    pub name: String,
    pub description: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

const COLS: &str = "SELECT id, aid, name, description, created_at, updated_at FROM tag_groups";

/// The admin shell posts every form field as a plain string (an empty field is omitted), so
/// accept a string and ignore anything else rather than 422-ing the whole request.
fn text_field(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
}

fn account_id(claims: &Claims) -> ApiResult<Uuid> {
    Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)
}

pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let rows = sqlx::query_as::<_, TagGroup>(&format!("{COLS} WHERE aid = $1 ORDER BY name ASC"))
        .bind(aid)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(json!({ "items": rows, "count": rows.len() })))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let name = text_field(&body, "name").unwrap_or_default();
    if name.is_empty() {
        return Err(AppError::Validation(
            "Tag group name is required".to_string(),
        ));
    }
    let description = text_field(&body, "description").unwrap_or_default();
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tag_groups (id, aid, name, description) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(aid)
        .bind(&name)
        .bind(&description)
        .execute(&state.db)
        .await?;
    let row = sqlx::query_as::<_, TagGroup>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_one(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({ "item": row }))))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let row = sqlx::query_as::<_, TagGroup>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Tag group not found".to_string()))?;
    Ok(Json(json!({ "item": row })))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let existing = sqlx::query_as::<_, TagGroup>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Tag group not found".to_string()))?;
    // A blank field is "unset", not "clear it" — the admin form omits blanks for the same reason.
    let name = text_field(&body, "name")
        .filter(|s| !s.is_empty())
        .unwrap_or(existing.name);
    let description = text_field(&body, "description").unwrap_or(existing.description);
    sqlx::query(
        "UPDATE tag_groups SET name = $1, description = $2, updated_at = NOW() WHERE id = $3 AND aid = $4",
    )
    .bind(&name)
    .bind(&description)
    .bind(id)
    .bind(aid)
    .execute(&state.db)
    .await?;
    let row = sqlx::query_as::<_, TagGroup>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(json!({ "item": row })))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let result = sqlx::query("DELETE FROM tag_groups WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Tag group not found".to_string()));
    }
    Ok(Json(json!({ "deleted": true })))
}
