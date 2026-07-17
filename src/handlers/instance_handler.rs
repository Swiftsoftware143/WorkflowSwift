use axum::{
    extract::{State, Json, Path},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;
use sqlx::Row;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::models::instance::*;

pub async fn list_instances(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let instances = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE aid = $1 ORDER BY created_at DESC",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"instances": instances})))
}

pub async fn get_instance(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let instance = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE id = $1 AND aid = $2",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Workflow instance not found".to_string()))?;

    let steps = sqlx::query_as::<_, WorkflowInstanceStep>(
        "SELECT * FROM workflow_instance_steps WHERE instance_id = $1 ORDER BY sort_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"instance": instance, "steps": steps})))
}

pub async fn update_instance(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let _existing = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE id = $1 AND aid = $2",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Instance not found".to_string()))?;

    if let Some(status) = req.get("status").and_then(|v| v.as_str()) {
        sqlx::query(
            "UPDATE workflow_instances SET status = $1, updated_at = NOW() WHERE id = $2",
        )
        .bind(status)
        .bind(id)
        .execute(&state.db)
        .await?;
    }

    Ok(Json(json!({"message": "Instance updated"})))
}

/// Advance a workflow instance to the next step.
/// Optionally dispatches the current step through its bound integration target.
/// POST /api/v1/instances/{id}/callback
/// Called by n8n after workflow execution completes.
/// Updates instance status and stores n8n results.
/// Accepts: { status: "completed"|"failed", result: {...}, error?: string }
pub async fn instance_callback(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    // No auth required — this is called by n8n internally
    // The instance_id acts as bearer token

    let status = req.get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("completed");

    let result = req.get("result");
    let error_msg = req.get("error").and_then(|v| v.as_str());

    // Verify instance exists
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workflow_instances WHERE id = $1)"
    )
    .bind(id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(false);

    if !exists {
        return Err(AppError::NotFound("Instance not found".to_string()));
    }

    // Update instance — store result and error directly on the row
    if let Some(result_val) = result {
        sqlx::query(
            r#"UPDATE workflow_instances SET status = $1, completed_at = NOW(), updated_at = NOW(), result = $2::jsonb WHERE id = $3"#
        )
        .bind(status)
        .bind(result_val)
        .bind(id)
        .execute(&state.db)
        .await?;
    } else {
        sqlx::query(
            r#"UPDATE workflow_instances SET status = $1, completed_at = NOW(), updated_at = NOW() WHERE id = $2"#
        )
        .bind(status)
        .bind(id)
        .execute(&state.db)
        .await?;
    }

    if let Some(err) = error_msg {
        let _ = sqlx::query(
            "UPDATE workflow_instances SET error_text = $1 WHERE id = $2"
        )
        .bind(err)
        .bind(id)
        .execute(&state.db)
        .await;
    }

    if let Some(err) = error_msg {
        tracing::warn!(instance_id = %id, error = %err, "Workflow instance completed with error");
    }

    Ok(Json(json!({
        "received": true,
        "instance_id": id.to_string(),
        "status": status
    })))
}

pub async fn advance_instance(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let instance = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE id = $1 AND aid = $2",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Instance not found".to_string()))?;

    let current_order: i32 = req.get("current_step_order").and_then(|v| v.as_i64()).map(|n| n as i32).unwrap_or(0);

    // Check if there's an integration target bound to this step
    // Also fetch security fields: allowed_domains and daily_limit
    let integration_rows = sqlx::query(
        "SELECT wsi.integration_target_id::text, it.provider_preset, it.webhook_url, it.api_key,
                COALESCE(it.allowed_domains, ARRAY[]::TEXT[])::text[] as allowed_domains,
                COALESCE(it.daily_limit, 1000)::int as daily_limit,
                it.id as raw_id
         FROM workflow_step_integrations wsi
         JOIN integration_targets it ON it.id = wsi.integration_target_id AND it.is_active = true
         WHERE wsi.step_id IN (
             SELECT ws.id FROM workflow_steps ws
             WHERE ws.workflow_id = $1 AND ws.sort_order = $2
         )
         AND it.aid = $3
         ORDER BY wsi.sort_order"
    )
    .bind(instance.workflow_id)
    .bind(current_order)
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    // If there are bound integrations, dispatch through each
    let mut dispatch_results: Vec<serde_json::Value> = Vec::new();

    for row in integration_rows {
        let target_id: String = row.try_get("integration_target_id").unwrap_or_default();
        let provider_preset: Option<String> = row.try_get("provider_preset").unwrap_or(None);
        let webhook_url: Option<String> = row.try_get("webhook_url").unwrap_or(None);
        let api_key: Option<String> = row.try_get("api_key").unwrap_or(None);
        let allowed_domains: Vec<String> = row.try_get("allowed_domains").unwrap_or_default();
        let daily_limit: i32 = row.try_get("daily_limit").unwrap_or(1000);
        let raw_target_id: uuid::Uuid = row.try_get("raw_id").unwrap_or(uuid::Uuid::nil());

        // Security check: domain allowlist + daily rate limit before firing
        if let Some(ref url) = webhook_url {
            if !url.is_empty() {
                if let Err(e) = crate::security::webhook_security::check_webhook_security(
                    &state.db,
                    &raw_target_id,
                    url,
                    &allowed_domains,
                    daily_limit,
                ).await {
                    dispatch_results.push(json!({
                        "target_id": target_id,
                        "status": "blocked",
                        "error": format!("Blocked by security policy: {}", e),
                    }));
                    continue;
                }
            } else { continue; }
        } else { continue; }

        // Build the target URL from webhook_url or provider preset base_url
        let target_url = if let Some(ref url) = webhook_url {
            url.clone()
        } else {
            // Try to look up the preset base URL
            let base: Option<String> = if let Some(ref preset) = provider_preset {
                sqlx::query_scalar("SELECT base_url FROM integration_provider_presets WHERE key = $1")
                    .bind(preset)
                    .fetch_optional(&state.db).await?
                    .unwrap_or_default()
            } else { None };
            if let Some(b) = base { b } else { continue; }
        };

        // Dispatch
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| AppError::Internal(format!("HTTP client error: {}", e)))?;

        let mut dispatch_req = client.post(&target_url)
            .json(&json!({
                "workflow_instance_id": id.to_string(),
                "step_order": current_order,
                "payload": req.get("payload")
            }));

        if let Some(ref key) = api_key {
            dispatch_req = dispatch_req.header("Authorization", format!("Bearer {}", key));
            dispatch_req = dispatch_req.header("x-api-key", key);
        }

        match dispatch_req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body: serde_json::Value = resp.json().await.unwrap_or(json!({"status": "ok"}));
                dispatch_results.push(json!({
                    "target_id": target_id,
                    "status": status,
                    "response": body
                }));
            }
            Err(e) => {
                dispatch_results.push(json!({
                    "target_id": target_id,
                    "status": "error",
                    "error": e.to_string()
                }));
            }
        }
    }

    // Update the instance's current step
    if current_order > 0 {
        sqlx::query(
            "UPDATE workflow_instances SET current_step_order = $1, updated_at = NOW() WHERE id = $2",
        )
        .bind(current_order)
        .bind(id)
        .execute(&state.db)
        .await?;
    }

    // Optionally mark completed
    let completed = req.get("completed").and_then(|v| v.as_bool()).unwrap_or(false);
    if completed {
        sqlx::query(
            "UPDATE workflow_instances SET status = 'completed', completed_at = NOW(), updated_at = NOW() WHERE id = $1",
        )
        .bind(id)
        .execute(&state.db)
        .await?;
    }

    Ok(Json(json!({
        "message": "Instance advanced",
        "dispatch_results": dispatch_results
    })))
}
