use axum::{
    extract::{State, Json, Path, Query},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use std::collections::HashMap;
use sqlx::Row;
use uuid::Uuid;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;

/// List integrations bound to a workflow step
pub async fn list_step_integrations(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let step_id_str = params.get("step_id").ok_or_else(|| AppError::BadRequest("step_id required".into()))?;
    let step_id = Uuid::parse_str(step_id_str).map_err(|_| AppError::BadRequest("Invalid step_id".into()))?;

    let rows = sqlx::query(
        "SELECT wsi.id::text, wsi.integration_target_id::text, wsi.payload_template::text,
                it.name as target_name, it.provider as provider, it.provider_preset
         FROM workflow_step_integrations wsi
         JOIN integration_targets it ON it.id = wsi.integration_target_id
         WHERE wsi.step_id = $1
         ORDER BY wsi.sort_order"
    )
    .bind(step_id)
    .fetch_all(&state.db)
    .await?;

    let bindings: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<&str,_>("id").unwrap_or(""),
            "integration_target_id": r.try_get::<&str,_>("integration_target_id").unwrap_or(""),
            "target_name": r.try_get::<&str,_>("target_name").unwrap_or(""),
            "provider": r.try_get::<&str,_>("provider").unwrap_or(""),
            "provider_preset": r.try_get::<Option<String>,_>("provider_preset").unwrap_or(None),
            "payload_template": r.try_get::<&str,_>("payload_template").unwrap_or("{}")
        })
    }).collect();

    Ok(Json(json!({"step_integrations": bindings})))
}

/// Bind an integration target to a workflow step
pub async fn create_step_integration(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let step_id_str = req.get("step_id").and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("step_id required".into()))?;
    let step_id = Uuid::parse_str(step_id_str).map_err(|_| AppError::BadRequest("Invalid step_id".into()))?;

    let target_id_str = req.get("integration_target_id").and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("integration_target_id required".into()))?;
    let target_id = Uuid::parse_str(target_id_str).map_err(|_| AppError::BadRequest("Invalid integration_target_id".into()))?;

    let payload_template = req.get("payload_template").cloned().unwrap_or(json!({}));
    let sort_order: i32 = req.get("sort_order").and_then(|v| v.as_i64()).map(|n| n as i32).unwrap_or(0);

    // Verify the integration target belongs to this account
    let _target = sqlx::query("SELECT id FROM integration_targets WHERE id = $1 AND aid = $2")
        .bind(target_id).bind(aid)
        .fetch_optional(&state.db).await?
        .ok_or(AppError::NotFound("Integration target not found".into()))?;

    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workflow_step_integrations (id, step_id, integration_target_id, payload_template, sort_order)
         VALUES ($1, $2, $3, $4, $5)"
    )
    .bind(id).bind(step_id).bind(target_id).bind(&payload_template).bind(sort_order)
    .execute(&state.db).await?;

    Ok((StatusCode::CREATED, Json(json!({
        "id": id.to_string(),
        "step_id": step_id_str,
        "integration_target_id": target_id_str,
        "status": "bound"
    }))))
}

/// Remove an integration binding from a step
pub async fn delete_step_integration(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let binding_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid ID".into()))?;
    sqlx::query("DELETE FROM workflow_step_integrations WHERE id = $1")
        .bind(binding_id)
        .execute(&state.db).await?;
    Ok(Json(json!({"status": "unbound"})))
}

/// Get available integration targets for an account (for dropdown selection)
pub async fn list_available_integrations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        "SELECT id::text, name, provider, provider_preset, is_active
         FROM integration_targets WHERE aid = $1 AND is_active = true
         ORDER BY name"
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let targets: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<&str,_>("id").unwrap_or(""),
            "name": r.try_get::<&str,_>("name").unwrap_or(""),
            "provider": r.try_get::<&str,_>("provider").unwrap_or(""),
            "provider_preset": r.try_get::<Option<String>,_>("provider_preset").unwrap_or(None),
            "is_active": r.try_get::<bool,_>("is_active").unwrap_or(false)
        })
    }).collect();

    Ok(Json(json!({"available_integrations": targets})))
}
