//! Reviews handler — auto-generated.

use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{ApiResult, AppError};
use crate::state::AppState;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Item {
    pub id: Uuid,
    pub aid: Uuid,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

#[derive(Deserialize)]
pub struct CreateInput {
    pub name: String,
}

#[derive(Deserialize)]
pub struct UpdateInput {
    pub name: Option<String>,
}

/// GET /api/v1/reviews
pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let limit = 50;
    let offset = 0;
    let items = sqlx::query_as::<_, Item>(
        "SELECT id, aid, name, created_at, updated_at FROM reviews ORDER BY name LIMIT $1 OFFSET $2"
    )
    .bind(limit).bind(offset)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    Ok(Json(json!({ "items": items, "count": items.len() })))
}

/// POST /api/v1/reviews
pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateInput>,
) -> ApiResult<Json<Value>> {
    let id = Uuid::new_v4();
    let aid = Uuid::nil();
    sqlx::query("INSERT INTO reviews (id, aid, name) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(aid)
        .bind(&body.name)
        .execute(&state.db)
        .await?;
    let item = sqlx::query_as::<_, Item>(
        "SELECT id, aid, name, created_at, updated_at FROM reviews WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({"item": item})))
}

/// GET /api/v1/reviews/{id}
pub async fn get(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let item = sqlx::query_as::<_, Item>(
        "SELECT id, aid, name, created_at, updated_at FROM reviews WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Reviews not found".to_string()))?;
    Ok(Json(json!({"item": item})))
}

/// PUT /api/v1/reviews/{id}
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateInput>,
) -> ApiResult<Json<Value>> {
    if let Some(name) = body.name {
        sqlx::query("UPDATE reviews SET name = $1, updated_at = now() WHERE id = $2")
            .bind(&name)
            .bind(id)
            .execute(&state.db)
            .await?;
    }
    let item = sqlx::query_as::<_, Item>(
        "SELECT id, aid, name, created_at, updated_at FROM reviews WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({"item": item})))
}

/// DELETE /api/v1/reviews/{id}
pub async fn delete(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    sqlx::query("DELETE FROM reviews WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({"status": "deleted"})))
}
