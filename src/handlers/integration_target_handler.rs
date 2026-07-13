use axum::{extract::{State, Json, Path, Query}, http::StatusCode, response::IntoResponse, Extension};
use serde_json::json;
use std::collections::HashMap;
use uuid::Uuid;
use sqlx::Row;
use crate::features;
use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;

pub async fn list_integration_targets(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let portfolio_filter = params.get("portfolio_company_id").and_then(|v| Uuid::parse_str(v).ok());

    if let Some(pc_id) = portfolio_filter {
        let rows = sqlx::query("SELECT id::text, aid::text, portfolio_company_id::text, user_id::text, name, provider, webhook_url, events::text, is_active, created_at::text FROM integration_targets WHERE aid = $1 AND portfolio_company_id = $2 ORDER BY name")
            .bind(aid).bind(pc_id)
            .fetch_all(&state.db).await?;
        let targets: Vec<serde_json::Value> = rows.iter().map(|r| {
            json!({"id": r.try_get::<&str,_>("id").unwrap_or(""), "name": r.try_get::<&str,_>("name").unwrap_or(""), "provider": r.try_get::<&str,_>("provider").unwrap_or(""), "webhook_url": r.try_get::<&str,_>("webhook_url").unwrap_or(""), "is_active": r.try_get::<bool,_>("is_active").unwrap_or(false)})
        }).collect();
        return Ok(Json(json!({"integration_targets": targets})));
    }

    let rows = sqlx::query("SELECT id::text, aid::text, portfolio_company_id::text, user_id::text, name, provider, webhook_url, events::text, is_active, created_at::text FROM integration_targets WHERE aid = $1 ORDER BY name")
        .bind(aid)
        .fetch_all(&state.db).await?;
    let targets: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({"id": r.try_get::<&str,_>("id").unwrap_or(""), "name": r.try_get::<&str,_>("name").unwrap_or(""), "provider": r.try_get::<&str,_>("provider").unwrap_or(""), "webhook_url": r.try_get::<&str,_>("webhook_url").unwrap_or(""), "is_active": r.try_get::<bool,_>("is_active").unwrap_or(false)})
    }).collect();
    Ok(Json(json!({"integration_targets": targets})))
}

pub async fn create_integration_target(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_integration_targets", "Integration Targets").await?;
    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("Target").to_string();
    let provider = req.get("provider").and_then(|v| v.as_str()).unwrap_or("webhook").to_string();
    let webhook_url = req.get("webhook_url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let pc_id = req.get("portfolio_company_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
    let user_id = req.get("user_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
    let api_key = req.get("api_key").and_then(|v| v.as_str());
    let id = Uuid::new_v4();

    sqlx::query("INSERT INTO integration_targets (id, aid, portfolio_company_id, user_id, name, provider, webhook_url, api_key) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
        .bind(id).bind(aid).bind(pc_id).bind(user_id).bind(&name).bind(&provider).bind(&webhook_url).bind(api_key)
        .execute(&state.db).await?;

    Ok((StatusCode::CREATED, Json(json!({"id": id.to_string(), "name": name, "provider": provider}))))
}

pub async fn update_integration_target(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let target_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid ID".into()))?;

    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE integration_targets SET name = $1, updated_at = NOW() WHERE id = $2 AND aid = $3")
            .bind(name).bind(target_id).bind(aid).execute(&state.db).await?;
    }
    if let Some(url) = req.get("webhook_url").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE integration_targets SET webhook_url = $1, updated_at = NOW() WHERE id = $2 AND aid = $3")
            .bind(url).bind(target_id).bind(aid).execute(&state.db).await?;
    }
    if let Some(active) = req.get("is_active").and_then(|v| v.as_bool()) {
        sqlx::query("UPDATE integration_targets SET is_active = $1, updated_at = NOW() WHERE id = $2 AND aid = $3")
            .bind(active).bind(target_id).bind(aid).execute(&state.db).await?;
    }

    Ok(Json(json!({"status": "updated"})))
}

pub async fn delete_integration_target(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let target_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid ID".into()))?;
    sqlx::query("DELETE FROM integration_targets WHERE id = $1 AND aid = $2")
        .bind(target_id).bind(aid).execute(&state.db).await?;
    Ok(Json(json!({"status": "deleted"})))
}