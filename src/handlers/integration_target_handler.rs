//! Integration target handlers — CRUD with webhook security (domain allowlisting + daily limits).

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::features;
use crate::security::webhook_security;
use crate::AppState;
use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

pub async fn list_integration_targets(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let portfolio_filter = params
        .get("portfolio_company_id")
        .and_then(|v| Uuid::parse_str(v).ok());

    if let Some(pc_id) = portfolio_filter {
        let rows = sqlx::query(
            "SELECT id::text, aid::text, portfolio_company_id::text, user_id::text, \
             name, provider, webhook_url, events::text, is_active, \
             COALESCE(allowed_domains, ARRAY[]::TEXT[])::text[]::text as allowed_domains, \
             COALESCE(daily_limit, 1000)::int as daily_limit, \
             created_at::text \
             FROM integration_targets WHERE aid = $1 AND portfolio_company_id = $2 ORDER BY name",
        )
        .bind(aid)
        .bind(pc_id)
        .fetch_all(&state.db)
        .await?;
        let targets: Vec<serde_json::Value> = rows.iter().map(|r| {
            json!({
                "id": r.try_get::<&str,_>("id").unwrap_or(""),
                "name": r.try_get::<&str,_>("name").unwrap_or(""),
                "provider": r.try_get::<&str,_>("provider").unwrap_or(""),
                "webhook_url": r.try_get::<&str,_>("webhook_url").unwrap_or(""),
                "is_active": r.try_get::<bool,_>("is_active").unwrap_or(false),
                "allowed_domains": r.try_get::<Vec<String>,_>("allowed_domains").unwrap_or_default(),
                "daily_limit": r.try_get::<i32,_>("daily_limit").unwrap_or(1000),
            })
        }).collect();
        return Ok(Json(json!({"integration_targets": targets})));
    }

    let rows = sqlx::query(
        "SELECT id::text, aid::text, portfolio_company_id::text, user_id::text, \
         name, provider, webhook_url, events::text, is_active, \
         COALESCE(allowed_domains, ARRAY[]::TEXT[])::text[]::text as allowed_domains, \
         COALESCE(daily_limit, 1000)::int as daily_limit, \
         created_at::text \
         FROM integration_targets WHERE aid = $1 ORDER BY name",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;
    let targets: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<&str,_>("id").unwrap_or(""),
            "name": r.try_get::<&str,_>("name").unwrap_or(""),
            "provider": r.try_get::<&str,_>("provider").unwrap_or(""),
            "webhook_url": r.try_get::<&str,_>("webhook_url").unwrap_or(""),
            "is_active": r.try_get::<bool,_>("is_active").unwrap_or(false),
            "allowed_domains": r.try_get::<Vec<String>,_>("allowed_domains").unwrap_or_default(),
            "daily_limit": r.try_get::<i32,_>("daily_limit").unwrap_or(1000),
        })
    }).collect();
    Ok(Json(json!({"integration_targets": targets})))
}

pub async fn create_integration_target(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(
        &state.db,
        aid,
        "max_integration_targets",
        "Integration Targets",
    )
    .await?;

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Target")
        .to_string();
    let provider = req
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("webhook")
        .to_string();
    let webhook_url = req
        .get("webhook_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let pc_id = req
        .get("portfolio_company_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let user_id = req
        .get("user_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let api_key = req.get("api_key").and_then(|v| v.as_str());
    let allowed_domains: Vec<String> = req
        .get("allowed_domains")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let daily_limit: i32 = req
        .get("daily_limit")
        .and_then(|v| v.as_i64())
        .map(|n| n as i32)
        .unwrap_or(1000);

    // Validate webhook URL against allowed domains
    webhook_security::validate_webhook_url(&webhook_url, &allowed_domains)
        .map_err(|msg| AppError::Validation(format!("Webhook URL rejected: {}", msg)))?;

    let id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO integration_targets (id, aid, portfolio_company_id, user_id, name, provider, webhook_url, api_key, allowed_domains, daily_limit) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
    )
    .bind(id).bind(aid).bind(pc_id).bind(user_id)
    .bind(&name).bind(&provider).bind(&webhook_url).bind(api_key)
    .bind(&allowed_domains).bind(daily_limit)
    .execute(&state.db).await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id.to_string(),
            "name": name,
            "provider": provider,
            "webhook_url": webhook_url,
            "allowed_domains": allowed_domains,
            "daily_limit": daily_limit,
        })),
    ))
}

pub async fn update_integration_target(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let target_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid ID".into()))?;

    // Fetch existing to validate webhook URL if it's being changed
    let existing = sqlx::query(
        "SELECT webhook_url, COALESCE(allowed_domains, ARRAY[]::TEXT[])::text[] as allowed_domains \
         FROM integration_targets WHERE id = $1 AND aid = $2"
    )
    .bind(target_id).bind(aid)
    .fetch_optional(&state.db).await?
    .ok_or_else(|| AppError::NotFound("Integration target not found".into()))?;

    let existing_webhook: String = existing.try_get("webhook_url").unwrap_or_default();
    let existing_domains: Vec<String> = existing.try_get("allowed_domains").unwrap_or_default();

    let webhook_url = req
        .get("webhook_url")
        .and_then(|v| v.as_str())
        .unwrap_or(&existing_webhook)
        .to_string();
    let allowed_domains: Vec<String> = req
        .get("allowed_domains")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or(existing_domains);

    // Validate webhook URL against allowed domains if either changed
    webhook_security::validate_webhook_url(&webhook_url, &allowed_domains)
        .map_err(|msg| AppError::Validation(format!("Webhook URL rejected: {}", msg)))?;

    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE integration_targets SET name = $1, updated_at = NOW() WHERE id = $2 AND aid = $3")
            .bind(name).bind(target_id).bind(aid).execute(&state.db).await?;
    }
    if req.get("webhook_url").is_some() {
        sqlx::query("UPDATE integration_targets SET webhook_url = $1, updated_at = NOW() WHERE id = $2 AND aid = $3")
            .bind(&webhook_url).bind(target_id).bind(aid).execute(&state.db).await?;
    }
    if let Some(active) = req.get("is_active").and_then(|v| v.as_bool()) {
        sqlx::query("UPDATE integration_targets SET is_active = $1, updated_at = NOW() WHERE id = $2 AND aid = $3")
            .bind(active).bind(target_id).bind(aid).execute(&state.db).await?;
    }
    if req.get("allowed_domains").is_some() || req.get("daily_limit").is_some() {
        sqlx::query(
            "UPDATE integration_targets SET allowed_domains = $1, daily_limit = $2, updated_at = NOW() WHERE id = $3 AND aid = $4"
        )
        .bind(&allowed_domains)
        .bind(req.get("daily_limit").and_then(|v| v.as_i64()).map(|n| n as i32).unwrap_or(1000))
        .bind(target_id).bind(aid)
        .execute(&state.db).await?;
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
        .bind(target_id)
        .bind(aid)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({"status": "deleted"})))
}
