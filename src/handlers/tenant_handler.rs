use axum::{
    extract::{State, Json},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::models::tenant::Tenant;

pub async fn get_tenant(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let tenant = sqlx::query_as::<_, Tenant>(
        "SELECT * FROM tenants WHERE id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Tenant not found".to_string()))?;

    Ok(Json(json!({"tenant": tenant})))
}

pub async fn update_tenant(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE tenants SET name = $1 WHERE id = $2")
            .bind(name)
            .bind(tenant_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(slug) = req.get("slug").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE tenants SET slug = $1 WHERE id = $2")
            .bind(slug)
            .bind(tenant_id)
            .execute(&state.db)
            .await?;
    }

    if let Some(hex_key) = req.get("hexomatic_key").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE tenants SET hexomatic_key = $1 WHERE id = $2")
            .bind(if hex_key.is_empty() { None } else { Some(hex_key) })
            .bind(tenant_id)
            .execute(&state.db)
            .await?;
    }

    if let Some(year) = req.get("footer_year").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE tenants SET footer_year = $1 WHERE id = $2")
            .bind(year)
            .bind(tenant_id)
            .execute(&state.db)
            .await?;
    }

    if let Some(company) = req.get("footer_company").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE tenants SET footer_company = $1 WHERE id = $2")
            .bind(company)
            .bind(tenant_id)
            .execute(&state.db)
            .await?;
    }

    Ok(Json(json!({"message": "Tenant updated"})))
}

pub async fn get_hexomatic_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let row: (Option<String>,) = sqlx::query_as(
        "SELECT hexomatic_key FROM tenants WHERE id = $1"
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await?;

    let masked = match row.0 {
        Some(ref k) if k.len() > 6 => format!("{}...{}", &k[..3], &k[k.len()-3..]),
        Some(k) => k,
        None => String::new(),
    };

    Ok(Json(json!({"key": masked})))
}

pub async fn set_tenant_industry(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let industry_slug = req.get("industry_slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("industry_slug is required".into()))?;
    
    sqlx::query("UPDATE tenants SET industry_slug = $1 WHERE id = $2")
        .bind(industry_slug)
        .bind(tenant_id)
        .execute(&state.db)
        .await?;
    
    Ok(Json(json!({"status": "ok", "industry_slug": industry_slug})))
}

pub async fn set_hexomatic_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let key = req.get("key").and_then(|v| v.as_str()).unwrap_or("");

    sqlx::query("UPDATE tenants SET hexomatic_key = $1 WHERE id = $2")
        .bind(if key.is_empty() { None } else { Some(key) })
        .bind(tenant_id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({"message": "Hexomatic API key stored"})))
}
