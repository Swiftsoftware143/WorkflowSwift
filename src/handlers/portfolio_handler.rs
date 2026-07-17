use axum::{extract::{State, Json, Path}, http::{HeaderMap, StatusCode}, response::IntoResponse, Extension};
use serde_json::json;
use uuid::Uuid;
use sqlx::Row;
use crate::features;
use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;

pub async fn list_portfolio_companies(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let rows = sqlx::query("SELECT id::text, name, slug, settings::text, created_at::text FROM portfolio_companies WHERE aid = $1 ORDER BY name")
        .bind(aid)
        .fetch_all(&state.db).await?;
    let companies: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({"id": r.try_get::<&str,_>("id").unwrap_or(""), "name": r.try_get::<&str,_>("name").unwrap_or(""), "slug": r.try_get::<&str,_>("slug").unwrap_or("")})
    }).collect();
    Ok(Json(json!({"portfolio_companies": companies})))
}

pub async fn create_portfolio_company(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_portfolio_companys", "Portfolio Companys").await?;
    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("Company").to_string();
    let slug = req.get("slug").and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| name.to_lowercase().replace(' ', "-"));
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO portfolio_companies (id, aid, name, slug) VALUES ($1, $2, $3, $4)")
        .bind(id).bind(aid).bind(&name).bind(&slug)
        .execute(&state.db).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id.to_string(), "name": name, "slug": slug}))))
}

pub async fn get_portfolio_company(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let company_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid ID".into()))?;
    let row = sqlx::query("SELECT id::text, name, slug, settings::text, created_at::text FROM portfolio_companies WHERE id = $1 AND aid = $2")
        .bind(company_id).bind(aid)
        .fetch_optional(&state.db).await?
        .ok_or(AppError::NotFound("Company not found".into()))?;
    Ok(Json(json!({"id": row.try_get::<&str,_>("id").unwrap_or(""), "name": row.try_get::<&str,_>("name").unwrap_or(""), "slug": row.try_get::<&str,_>("slug").unwrap_or("")})))
}

pub async fn update_portfolio_company(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let company_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid ID".into()))?;
    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE portfolio_companies SET name = $1, updated_at = NOW() WHERE id = $2 AND aid = $3")
            .bind(name).bind(company_id).bind(aid).execute(&state.db).await?;
    }
    if let Some(slug) = req.get("slug").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE portfolio_companies SET slug = $1, updated_at = NOW() WHERE id = $2 AND aid = $3")
            .bind(slug).bind(company_id).bind(aid).execute(&state.db).await?;
    }
    Ok(Json(json!({"status": "updated"})))
}

/// POST /api/v1/internal/portfolio-companies — internal sync, no JWT
pub async fn internal_create_portfolio_company(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let key = headers.get("x-internal-key").and_then(|v| v.to_str().ok()).unwrap_or("");
    if key != state.config.internal_sync_key {
        return Err(AppError::Forbidden("Invalid internal key".into()));
    }

    let aid = body.get("aid")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("aid required".into()))?;

    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("Company").to_string();
    let slug = body.get("slug").and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| name.to_lowercase().replace(' ', "-"));
    let email = body.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let description = body.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let id = Uuid::new_v4();

    // Ensure account exists (FK constraint)
    sqlx::query(
        "INSERT INTO accounts (id, name, account_slug) VALUES ($1, $2, CONCAT($3, '-', LEFT(CAST($1 AS TEXT), 8))) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, account_slug = EXCLUDED.account_slug"
    )
    .bind(aid)
    .bind(&name)
    .bind(&slug)
    .execute(&state.db)
    .await.ok();

    sqlx::query(
        "INSERT INTO portfolio_companies (id, aid, name, slug, email, description) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, account_slug = EXCLUDED.account_slug"
    )
    .bind(id)
    .bind(aid)
    .bind(&name)
    .bind(&slug)
    .bind(&email)
    .bind(&description)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({"status": "synced", "id": id.to_string()})))
}

pub async fn delete_portfolio_company(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let company_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid ID".into()))?;
    sqlx::query("DELETE FROM portfolio_companies WHERE id = $1 AND aid = $2")
        .bind(company_id).bind(aid).execute(&state.db).await?;
    Ok(Json(json!({"status": "deleted"})))
}