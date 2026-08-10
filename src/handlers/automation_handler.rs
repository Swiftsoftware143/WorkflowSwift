use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::features;
use crate::models::automation::*;
use crate::AppState;

pub async fn list_automations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let automations = sqlx::query_as::<_, Automation>(
        "SELECT * FROM automations WHERE aid = $1 ORDER BY name ASC",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"automations": automations})))
}

pub async fn create_automation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_automations", "Automations").await?;

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = req
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let trigger_type = req
        .get("trigger_type")
        .and_then(|v| v.as_str())
        .unwrap_or("manual")
        .to_string();
    let trigger_config = req.get("trigger_config").cloned();
    let action_type = req
        .get("action_type")
        .and_then(|v| v.as_str())
        .unwrap_or("webhook")
        .to_string();
    let action_config = req.get("action_config").cloned();

    if name.is_empty() {
        return Err(AppError::Validation(
            "Automation name is required".to_string(),
        ));
    }

    let automation = sqlx::query_as::<_, Automation>(
        r#"INSERT INTO automations (id, aid, name, description, trigger_type, trigger_config, action_type, action_config)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(&name)
    .bind(&description)
    .bind(&trigger_type)
    .bind(&trigger_config)
    .bind(&action_type)
    .bind(&action_config)
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"automation": automation}))))
}

pub async fn run_automation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let _aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let _automation = sqlx::query_as::<_, Automation>("SELECT * FROM automations WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Automation not found".to_string()))?;

    let run_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO automation_runs (id, automation_id, status, trigger_data)
           VALUES ($1, $2, 'running', $3)"#,
    )
    .bind(run_id)
    .bind(id)
    .bind(&req)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "run_id": run_id.to_string(),
        "status": "running",
        "message": "Automation triggered"
    })))
}
