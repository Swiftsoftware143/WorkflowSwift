//! Leads handler — workflow inputs. Tenant-scoped real CRUD (lead captured -> workflow runs on it).
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
pub struct Lead {
    pub id: Uuid,
    pub aid: Uuid,
    pub name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub company: Option<String>,
    pub status: Option<String>,
    pub source: Option<String>,
    pub surface_id: Option<Uuid>,
    pub workflow_id: Option<Uuid>,
    pub notes: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
#[derive(Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
    pub surface_id: Option<Uuid>,
    #[serde(default)]
    pub page: i64,
    #[serde(default)]
    pub per_page: i64,
}
#[derive(Deserialize)]
pub struct CreateInput {
    pub name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub company: Option<String>,
    pub status: Option<String>,
    pub source: Option<String>,
    pub surface_id: Option<Uuid>,
    pub workflow_id: Option<Uuid>,
    pub notes: Option<String>,
}
#[derive(Deserialize)]
pub struct UpdateInput {
    pub name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub company: Option<String>,
    pub status: Option<String>,
    pub source: Option<String>,
    pub surface_id: Option<Uuid>,
    pub workflow_id: Option<Uuid>,
    pub notes: Option<String>,
}

const COLS: &str = "SELECT id, aid, name, email, phone, company, status, source, surface_id, workflow_id, notes, created_at, updated_at FROM leads";
const UPD: &str = "UPDATE leads SET name=COALESCE($2,name), email=COALESCE($3,email), phone=COALESCE($4,phone), company=COALESCE($5,company), status=COALESCE($6,status), source=COALESCE($7,source), surface_id=COALESCE($8,surface_id), workflow_id=COALESCE($9,workflow_id), notes=COALESCE($10,notes), updated_at=NOW() WHERE id=$1 AND aid=$11";

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
    let rows = if query.status.is_some() && query.surface_id.is_some() {
        sqlx::query_as::<_, Lead>("SELECT * FROM leads WHERE aid=$1 AND status=$2 AND surface_id=$3 ORDER BY created_at DESC LIMIT $4 OFFSET $5").bind(aid).bind(&query.status).bind(query.surface_id).bind(limit).bind(offset).fetch_all(&state.db).await?
    } else if query.status.is_some() {
        sqlx::query_as::<_, Lead>("SELECT * FROM leads WHERE aid=$1 AND status=$2 ORDER BY created_at DESC LIMIT $3 OFFSET $4").bind(aid).bind(&query.status).bind(limit).bind(offset).fetch_all(&state.db).await?
    } else if query.surface_id.is_some() {
        sqlx::query_as::<_, Lead>("SELECT * FROM leads WHERE aid=$1 AND surface_id=$2 ORDER BY created_at DESC LIMIT $3 OFFSET $4").bind(aid).bind(query.surface_id).bind(limit).bind(offset).fetch_all(&state.db).await?
    } else {
        sqlx::query_as::<_, Lead>(
            "SELECT * FROM leads WHERE aid=$1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(aid)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    };
    Ok(Json(json!({"leads": rows, "count": rows.len()})))
}
pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(b): Json<CreateInput>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO leads (id, aid, name, email, phone, company, status, source, surface_id, workflow_id, notes) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
        .bind(id).bind(aid).bind(&b.name).bind(&b.email).bind(&b.phone).bind(&b.company)
        .bind(&b.status).bind(&b.source).bind(b.surface_id).bind(b.workflow_id).bind(&b.notes)
        .execute(&state.db).await?;
    let row = sqlx::query_as::<_, Lead>(&format!("{COLS} WHERE id = $1"))
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"lead": row}))))
}
pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let row = sqlx::query_as::<_, Lead>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Lead not found".into()))?;
    Ok(Json(json!({"lead": row})))
}
pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateInput>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let c = sqlx::query_as::<_, Lead>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Lead not found".into()))?;
    sqlx::query(UPD)
        .bind(id)
        .bind(b.name.as_ref().unwrap_or(&c.name))
        .bind(b.email.as_ref().or(c.email.as_ref()))
        .bind(b.phone.as_ref().or(c.phone.as_ref()))
        .bind(b.company.as_ref().or(c.company.as_ref()))
        .bind(b.status.as_ref().or(c.status.as_ref()))
        .bind(b.source.as_ref().or(c.source.as_ref()))
        .bind(b.surface_id.or(c.surface_id))
        .bind(b.workflow_id.or(c.workflow_id))
        .bind(b.notes.as_ref().or(c.notes.as_ref()))
        .bind(aid)
        .execute(&state.db)
        .await?;
    let row = sqlx::query_as::<_, Lead>(&format!("{COLS} WHERE id = $1"))
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(json!({"lead": row})))
}
pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM leads WHERE id=$1 AND aid=$2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("Lead not found".into()));
    }
    Ok(Json(json!({"deleted": true})))
}
