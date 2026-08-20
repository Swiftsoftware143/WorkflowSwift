//! Surfaces handler — David's specified feature. Admin CRUD; users filter workflows by surface.
//! Tenant-scoped real CRUD (name, slug, description, is_active).
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
pub struct Surface {
    pub id: Uuid,
    pub aid: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub is_active: Option<bool>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
#[derive(Deserialize)]
pub struct ListQuery {
    pub is_active: Option<bool>,
    #[serde(default)]
    pub page: i64,
    #[serde(default)]
    pub per_page: i64,
}
#[derive(Deserialize)]
pub struct CreateInput {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub is_active: Option<bool>,
}
#[derive(Deserialize)]
pub struct UpdateInput {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub is_active: Option<bool>,
}

const COLS: &str =
    "SELECT id, aid, name, slug, description, is_active, created_at, updated_at FROM surfaces";

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

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
    let rows = match &query.is_active {
        Some(a) => sqlx::query_as::<_, Surface>("SELECT * FROM surfaces WHERE aid=$1 AND is_active=$2 ORDER BY name ASC LIMIT $3 OFFSET $4").bind(aid).bind(a).bind(limit).bind(offset).fetch_all(&state.db).await?,
        None => sqlx::query_as::<_, Surface>("SELECT * FROM surfaces WHERE aid=$1 ORDER BY name ASC LIMIT $2 OFFSET $3").bind(aid).bind(limit).bind(offset).fetch_all(&state.db).await?,
    };
    Ok(Json(json!({"surfaces": rows, "count": rows.len()})))
}
pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(b): Json<CreateInput>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let id = Uuid::new_v4();
    let slug = b
        .slug
        .clone()
        .map(|s| slugify(&s))
        .unwrap_or_else(|| slugify(&b.name));
    // per-tenant unique slug
    let dup: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM surfaces WHERE aid=$1 AND slug=$2")
        .bind(aid)
        .bind(&slug)
        .fetch_one(&state.db)
        .await?;
    if dup > 0 {
        return Err(AppError::Duplicate(format!(
            "Surface slug '{}' already exists for this account",
            slug
        )));
    }
    sqlx::query("INSERT INTO surfaces (id, aid, name, slug, description, is_active) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(id).bind(aid).bind(&b.name).bind(&slug).bind(&b.description).bind(b.is_active.unwrap_or(true)).execute(&state.db).await?;
    let row = sqlx::query_as::<_, Surface>(&format!("{COLS} WHERE id = $1"))
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"surface": row}))))
}
pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let row = sqlx::query_as::<_, Surface>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Surface not found".into()))?;
    Ok(Json(json!({"surface": row})))
}
pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateInput>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let c = sqlx::query_as::<_, Surface>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Surface not found".into()))?;
    let new_slug = b
        .slug
        .as_deref()
        .map(slugify)
        .unwrap_or_else(|| c.slug.clone());
    sqlx::query("UPDATE surfaces SET name=COALESCE($3,name), slug=COALESCE($4,slug), description=COALESCE($5,description), is_active=COALESCE($6,is_active), updated_at=NOW() WHERE id=$1 AND aid=$2")
        .bind(id).bind(aid)
        .bind(b.name.as_ref().unwrap_or(&c.name))
        .bind(new_slug)
        .bind(b.description.as_ref().or(c.description.as_ref()))
        .bind(b.is_active.or(c.is_active)).execute(&state.db).await?;
    let row = sqlx::query_as::<_, Surface>(&format!("{COLS} WHERE id = $1"))
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(json!({"surface": row})))
}
pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM surfaces WHERE id=$1 AND aid=$2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("Surface not found".into()));
    }
    Ok(Json(json!({"deleted": true})))
}
