//! Tickets handler — real tenant-scoped support tickets.
//! Replaces the auto-generated stub (which targeted a non-existent table and
//! was not auth/tenant-scoped). Mirrors the leads handler pattern.
use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, FromRow)]
pub struct Ticket {
    pub id: Uuid,
    pub aid: Uuid,
    pub subject: String,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub source: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
    #[serde(default)]
    pub page: i64,
    #[serde(default)]
    pub per_page: i64,
}

#[derive(Deserialize)]
pub struct CreateInput {
    pub subject: String,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub source: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateInput {
    pub subject: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
}

const COLS: &str = "SELECT id, aid, subject, description, status, priority, source, created_at, updated_at FROM tickets";

pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListQuery>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let limit = if query.per_page > 0 {
        query.per_page
    } else {
        50
    };
    let offset = if query.page > 0 {
        (query.page - 1) * limit
    } else {
        0
    };
    let rows = if query.status.is_some() {
        sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE aid=$1 AND status=$2 ORDER BY created_at DESC LIMIT $3 OFFSET $4")
            .bind(aid).bind(&query.status).bind(limit).bind(offset)
            .fetch_all(&state.db).await?
    } else {
        sqlx::query_as::<_, Ticket>(
            "SELECT * FROM tickets WHERE aid=$1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(aid)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    };
    Ok(Json(json!({ "items": rows, "count": rows.len() })))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(b): Json<CreateInput>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    if b.subject.trim().is_empty() {
        return Err(AppError::Validation("subject is required".into()));
    }
    let id = Uuid::new_v4();
    let status = b.status.clone().unwrap_or_else(|| "open".to_string());
    let priority = b.priority.clone().unwrap_or_else(|| "medium".to_string());
    let source = b.source.clone().unwrap_or_else(|| "manual".to_string());
    sqlx::query("INSERT INTO tickets (id, aid, subject, description, status, priority, source) VALUES ($1,$2,$3,$4,$5,$6,$7)")
        .bind(id).bind(aid).bind(&b.subject).bind(&b.description).bind(&status).bind(&priority).bind(&source)
        .execute(&state.db).await?;
    let row = sqlx::query_as::<_, Ticket>(&format!("{COLS} WHERE id = $1"))
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({ "item": row }))))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let row = sqlx::query_as::<_, Ticket>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Ticket not found".into()))?;
    Ok(Json(json!({ "item": row })))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateInput>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let cur = sqlx::query_as::<_, Ticket>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Ticket not found".into()))?;
    sqlx::query("UPDATE tickets SET subject=COALESCE($2,subject), description=COALESCE($3,description), status=COALESCE($4,status), priority=COALESCE($5,priority), updated_at=NOW() WHERE id=$1 AND aid=$6")
        .bind(id)
        .bind(b.subject.as_ref().unwrap_or(&cur.subject))
        .bind(b.description.as_ref().or(cur.description.as_ref()))
        .bind(b.status.as_ref().or(cur.status.as_ref()))
        .bind(b.priority.as_ref().or(cur.priority.as_ref()))
        .bind(aid)
        .execute(&state.db).await?;
    let row = sqlx::query_as::<_, Ticket>(&format!("{COLS} WHERE id = $1"))
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(json!({ "item": row })))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM tickets WHERE id=$1 AND aid=$2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("Ticket not found".into()));
    }
    Ok(Json(json!({ "deleted": true })))
}
