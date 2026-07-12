use axum::{
    extract::{State, Json, Path, Query},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;



use crate::features;
use crate::n8n_converter;
use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::models::workflow::*;

#[derive(Debug, serde::Deserialize)]
pub struct ListWorkflowsQuery {
    pub surface: Option<Uuid>,
}

pub async fn list_workflows(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListWorkflowsQuery>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let workflows = if let Some(surface_id) = query.surface {
        sqlx::query_as::<_, Workflow>(
            "SELECT * FROM workflows WHERE tenant_id = $1 AND (surface_id = $2 OR surface_id IS NULL) ORDER BY name ASC",
        )
        .bind(tenant_id)
        .bind(surface_id)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, Workflow>(
            "SELECT * FROM workflows WHERE tenant_id = $1 ORDER BY name ASC",
        )
        .bind(tenant_id)
        .fetch_all(&state.db)
        .await?
    };

    Ok(Json(json!({"workflows": workflows})))
}

pub async fn create_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateWorkflowRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, tenant_id, "max_workflows", "Workflows").await?;

    let workflow = sqlx::query_as::<_, Workflow>(
        r#"INSERT INTO workflows (id, tenant_id, name, description, category, lifecycle_summary, tags, surface_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.category)
    .bind(&req.lifecycle_summary)
    .bind(&req.tags)
    .bind(&req.surface_id)
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"workflow": workflow}))))
}

pub async fn get_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    let steps = sqlx::query_as::<_, WorkflowStep>(
        "SELECT * FROM workflow_steps WHERE workflow_id = $1 ORDER BY sort_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"workflow": workflow, "steps": steps})))
}

pub async fn update_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateWorkflowRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let existing = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    let name = req.name.unwrap_or(existing.name);
    let description = req.description.or(existing.description);
    let category = req.category.or(existing.category);
    let lifecycle_summary = req.lifecycle_summary.or(existing.lifecycle_summary);
    let tags = req.tags.or(existing.tags);

    let workflow = sqlx::query_as::<_, Workflow>(
        r#"UPDATE workflows SET name=$1, description=$2, category=$3, lifecycle_summary=$4, tags=$5, updated_at=NOW()
           WHERE id=$6 RETURNING *"#,
    )
    .bind(&name)
    .bind(&description)
    .bind(&category)
    .bind(&lifecycle_summary)
    .bind(&tags)
    .bind(id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(json!({"workflow": workflow})))
}

pub async fn delete_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("UPDATE workflows SET is_active = false WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Workflow not found".to_string()));
    }

    Ok(Json(json!({"message": "Workflow deleted"})))
}

pub async fn start_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, tenant_id, "max_instances", "Instances").await?;

    let client_id = req.get("client_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(AppError::Validation("client_id is required".to_string()))?;

    let workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    // Check credits
    let balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if balance < 1 {
        return Err(AppError::BadRequest(
            "Insufficient credits. Purchase more credits to run workflows.".to_string(),
        ));
    }

    let instance_id = Uuid::new_v4();
    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or(&workflow.name).to_string();

    // Define callback base URL for n8n node callbacks
    let callback_base_url = std::env::var("CALLBACK_BASE_URL")
        .unwrap_or_else(|_| "http://workflowswift:8085".to_string());

    // Check if already deployed; if not, deploy now
    let needs_deploy = match &workflow.lifecycle_summary {
        Some(summary) => !summary.contains("n8n_deployed"),
        None => true,
    };

    let webhook_path = if needs_deploy {
        let steps = sqlx::query_as::<_, WorkflowStep>(
            "SELECT * FROM workflow_steps WHERE workflow_id = $1 ORDER BY sort_order ASC",
        )
        .bind(id)
        .fetch_all(&state.db)
        .await?;

        if steps.is_empty() {
            return Err(AppError::BadRequest(
                "Workflow has no steps. Add at least one step before running.".to_string(),
            ));
        }

        let step_values: Vec<serde_json::Value> = steps
            .iter()
            .map(|s| {
                json!({
                    "step_type": s.step_type,
                    "name": s.name,
                    "description": s.description,
                    "sort_order": s.sort_order,
                    "config": s.config,
                })
            })
            .collect();

        let n8n_wf = n8n_converter::convert_steps_to_n8n(&step_values, tenant_id, id, &callback_base_url);
        let n8n_json = n8n_converter::to_n8n_json(&n8n_wf);
        let n8n_json_str = serde_json::to_string_pretty(&n8n_json)
            .map_err(|e| AppError::Internal(format!("JSON serialization: {}", e)))?;

        // Import via n8n CLI on user-n8n-main container
        // n8n CE doesn't expose REST API without owner auth, so use docker exec
        let temp_path = format!("/tmp/wfs_run_{}.json", id);
        tokio::fs::write(&temp_path, &n8n_json_str)
            .await
            .map_err(|e| AppError::Internal(format!("Write temp file: {}", e)))?;

        use std::process::Command as StdCommand;
        let _ = StdCommand::new("docker")
            .args(["cp", &temp_path, "user-n8n-main:/tmp/"])
            .output();

        let import_filename = format!("wfs_run_{}.json", id);
        let import_output = StdCommand::new("docker")
            .args(["exec", "user-n8n-main", "n8n", "import:workflow",
                   &format!("--input=/tmp/{}", import_filename),
                   "--activeState=fromJson"])
            .output()
            .map_err(|e| AppError::Internal(format!("n8n import exec failed: {}", e)))?;

        if !import_output.status.success() {
            let stderr = String::from_utf8_lossy(&import_output.stderr);
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(AppError::Internal(format!("n8n import failed: {}", stderr)));
        }

        let _ = tokio::fs::remove_file(&temp_path).await;
        let _ = StdCommand::new("docker")
            .args(["exec", "user-n8n-main", "rm", &format!("/tmp/{}", import_filename)])
            .output();

        let _ = sqlx::query(
            "UPDATE workflows SET lifecycle_summary = $1 WHERE id = $2",
        )
        .bind(json!({
            "n8n_deployed": true,
            "n8n_webhook_path": n8n_wf.webhook_path,
            "deployed_at": chrono::Utc::now().to_rfc3339(),
        }).to_string())
        .bind(id)
        .execute(&state.db)
        .await;

        n8n_wf.webhook_path
    } else {
        // Extract from lifecycle summary
        let summary: serde_json::Value = workflow.lifecycle_summary
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(json!({}));
        summary.get("n8n_webhook_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    if webhook_path.is_empty() {
        return Err(AppError::Internal(
            "Failed to determine webhook path for this workflow.".to_string(),
        ));
    }

    // Create instance record
    sqlx::query(
        r#"INSERT INTO workflow_instances (id, workflow_id, client_id, tenant_id, name, status, started_at)
           VALUES ($1, $2, $3, $4, $5, 'running', NOW())"#,
    )
    .bind(instance_id)
    .bind(id)
    .bind(client_id)
    .bind(tenant_id)
    .bind(&name)
    .execute(&state.db)
    .await?;

    // Deduct 1 execution credit
    sqlx::query(
        r#"INSERT INTO credit_transactions (id, tenant_id, amount, transaction_type, description)
           VALUES ($1, $2, -1, 'workflow_execution', $3)"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(format!("Workflow execution: {}", workflow.name))
    .execute(&state.db)
    .await?;

    // Trigger n8n webhook with instance context
    let n8n_url = format!("{}/webhook/{}", state.config.n8n_webhook_url.trim_end_matches('/'), webhook_path);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| AppError::Internal(format!("HTTP client: {}", e)))?;

    let trigger_payload = json!({
        "instance_id": instance_id.to_string(),
        "workflow_id": id.to_string(),
        "tenant_id": claims.aid,
        "triggered_by": claims.sub,
        "client_id": client_id.to_string(),
        "payload": req.get("payload").cloned().unwrap_or(json!({})),
        "callback_url": format!("{}/api/v1/instances/{}/callback", callback_base_url, instance_id),
        "dashboard_url": format!("{}/api/v1/dashboard/push-widget-data", callback_base_url),
        "headers": {
            "authorization": req.get("authorization").and_then(|v| v.as_str()).unwrap_or(""),
        }
    });

    // Fire-and-forget the n8n trigger in background
    let db = state.db.clone();
    let inst_id = instance_id;
    let wf_name = workflow.name.clone();
    let tid = tenant_id;

    tokio::spawn(async move {
        match client.post(&n8n_url).json(&trigger_payload).send().await {
            Ok(resp) => {
                let status_code = resp.status().as_u16();
                if status_code >= 200 && status_code < 300 {
                    tracing::info!(instance_id = %inst_id, "n8n accepted workflow");
                } else {
                    let _ = sqlx::query(
                        "UPDATE workflow_instances SET status = 'failed', completed_at = NOW() WHERE id = $1"
                    )
                    .bind(inst_id)
                    .execute(&db)
                    .await;
                    let _ = sqlx::query(
                        r#"INSERT INTO credit_transactions (id, tenant_id, amount, transaction_type, description)
                           VALUES ($1, $2, 1, 'refund', $3)"#
                    )
                    .bind(Uuid::new_v4())
                    .bind(tid)
                    .bind(format!("Refund for failed execution: {}", wf_name))
                    .execute(&db)
                    .await;
                    tracing::warn!(instance_id = %inst_id, status = status_code, "n8n rejected workflow");
                }
            }
            Err(e) => {
                let _ = sqlx::query(
                    "UPDATE workflow_instances SET status = 'failed', completed_at = NOW() WHERE id = $1"
                )
                .bind(inst_id)
                .execute(&db)
                .await;
                let _ = sqlx::query(
                    r#"INSERT INTO credit_transactions (id, tenant_id, amount, transaction_type, description)
                       VALUES ($1, $2, 1, 'refund', $3)"#
                )
                .bind(Uuid::new_v4())
                .bind(tid)
                .bind(format!("Refund for n8n error: {}", wf_name))
                .execute(&db)
                .await;
                tracing::error!(instance_id = %inst_id, error = %e, "n8n webhook call failed");
            }
        }
    });

    Ok((StatusCode::CREATED, Json(json!({
        "instance_id": instance_id.to_string(),
        "status": "running",
        "webhook_path": webhook_path,
        "message": format!("Workflow '{}' started. Check instance for results.", workflow.name)
    }))))
}

pub async fn get_workflow_steps(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let _tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let steps = sqlx::query_as::<_, WorkflowStep>(
        "SELECT * FROM workflow_steps WHERE workflow_id = $1 ORDER BY sort_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"steps": steps})))
}

pub async fn create_workflow_step(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateWorkflowStepRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Verify workflow exists and belongs to tenant
    let _workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    // Auto-assign sort_order: max + 1
    let max_sort: Option<(Option<i32>,)> = sqlx::query_as(
        "SELECT MAX(sort_order) FROM workflow_steps WHERE workflow_id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let sort_order = max_sort
        .and_then(|r| r.0)
        .map(|m| m + 1)
        .unwrap_or(0);

    let step = sqlx::query_as::<_, WorkflowStep>(
        r#"INSERT INTO workflow_steps (id, workflow_id, step_type, name, description, sort_order, config)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind(&req.step_type)
    .bind(&req.name)
    .bind(&req.description)
    .bind(sort_order)
    .bind(&req.config)
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"step": step}))))
}

pub async fn update_workflow_step(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((workflow_id, step_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateWorkflowStepRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Verify workflow exists and belongs to tenant
    let _workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND tenant_id = $2",
    )
    .bind(workflow_id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    let sort_order = req.sort_order.unwrap_or(0);
    let step = sqlx::query_as::<_, WorkflowStep>(
        r#"UPDATE workflow_steps SET step_type=$1, name=$2, description=$3, sort_order=$4, config=$5
           WHERE id=$6 AND workflow_id=$7
           RETURNING *"#,
    )
    .bind(&req.step_type)
    .bind(&req.name)
    .bind(&req.description)
    .bind(sort_order)
    .bind(&req.config)
    .bind(step_id)
    .bind(workflow_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Step not found".to_string()))?;

    Ok(Json(json!({"step": step})))
}

pub async fn delete_workflow_step(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((workflow_id, step_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Verify workflow exists and belongs to tenant
    let _workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND tenant_id = $2",
    )
    .bind(workflow_id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    let result = sqlx::query("DELETE FROM workflow_steps WHERE id = $1 AND workflow_id = $2")
        .bind(step_id)
        .bind(workflow_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Step not found".to_string()));
    }

    Ok(Json(json!({"message": "Step deleted"})))
}

pub async fn reorder_workflow_steps(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<ReorderStepsRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Verify workflow exists and belongs to tenant
    let _workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    for (i, step_id) in req.step_ids.iter().enumerate() {
        sqlx::query("UPDATE workflow_steps SET sort_order = $1 WHERE id = $2 AND workflow_id = $3")
            .bind(i as i32)
            .bind(step_id)
            .bind(id)
            .execute(&state.db)
            .await?;
    }

    let steps = sqlx::query_as::<_, WorkflowStep>(
        "SELECT * FROM workflow_steps WHERE workflow_id = $1 ORDER BY sort_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"steps": steps})))
}

// ─── New: Deploy Workflow to n8n via REST API ───

/// POST /api/v1/workflows/:id/deploy — convert WorkflowSwift steps to n8n and import
///
/// Generates an n8n-compatible workflow JSON from the stored steps,
/// and imports it via the n8n REST API using `POST /rest/workflows`.
pub async fn deploy_workflow_to_n8n(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    let steps = sqlx::query_as::<_, WorkflowStep>(
        "SELECT * FROM workflow_steps WHERE workflow_id = $1 ORDER BY sort_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    if steps.is_empty() {
        return Err(AppError::BadRequest(
            "Workflow has no steps. Add at least one step before deploying.".to_string(),
        ));
    }

    // Convert to n8n JSON via the converter
    let step_values: Vec<serde_json::Value> = steps
        .iter()
        .map(|s| {
            json!({
                "step_type": s.step_type,
                "name": s.name,
                "description": s.description,
                "sort_order": s.sort_order,
                "config": s.config,
            })
        })
        .collect();

    let callback_base_url = std::env::var("CALLBACK_BASE_URL")
        .unwrap_or_else(|_| "http://workflowswift:8085".to_string());
    let n8n_wf = n8n_converter::convert_steps_to_n8n(&step_values, tenant_id, id, &callback_base_url);
    let n8n_json = n8n_converter::to_n8n_json(&n8n_wf);

    // Deploy via n8n REST API
    let n8n_api_url = format!("{}/rest/workflows", state.config.n8n_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| AppError::Internal(format!("HTTP client: {}", e)))?;

    let mut req_builder = client.post(&n8n_api_url).json(&n8n_json);
    if !state.config.n8n_api_key.is_empty() {
        req_builder = req_builder.header("X-N8N-API-KEY", &state.config.n8n_api_key);
    }

    let api_resp = req_builder
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("n8n API request failed: {}", e)))?;

    let api_status = api_resp.status();
    if !api_status.is_success() {
        let error_text = api_resp.text().await.unwrap_or_else(|_| "unknown error".to_string());
        return Err(AppError::Internal(format!(
            "n8n workflow import failed ({}): {}",
            api_status, error_text
        )));
    }

    // Store the webhook path so the frontend can trigger it
    sqlx::query(
        "UPDATE workflows SET lifecycle_summary = $1 WHERE id = $2",
    )
    .bind(json!({
        "n8n_deployed": true,
        "n8n_webhook_path": n8n_wf.webhook_path,
        "deployed_at": chrono::Utc::now().to_rfc3339(),
    }).to_string())
    .bind(id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "deployed": true,
        "webhook_path": n8n_wf.webhook_path,
        "name": n8n_wf.name,
        "node_count": n8n_wf.nodes.len(),
        "message": "Workflow deployed to n8n via REST API. Trigger it from the workflow runner."
    })))
}

/// POST /api/v1/workflows/:id/run — deploy (if needed) and execute the workflow
///
/// Deploys the workflow to n8n if not yet deployed, then triggers the webhook.
pub async fn run_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    // Check if already deployed; if not, deploy first
    let needs_deploy = match &workflow.lifecycle_summary {
        Some(summary) => !summary.contains("n8n_deployed"),
        None => true,
    };

    if needs_deploy {
        let steps = sqlx::query_as::<_, WorkflowStep>(
            "SELECT * FROM workflow_steps WHERE workflow_id = $1 ORDER BY sort_order ASC",
        )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

        if steps.is_empty() {
            return Err(AppError::BadRequest(
                "Workflow has no steps. Add at least one step before running.".to_string(),
            ));
        }

        let step_values: Vec<serde_json::Value> = steps
            .iter()
            .map(|s| {
                json!({
                    "step_type": s.step_type,
                    "name": s.name,
                    "description": s.description,
                    "sort_order": s.sort_order,
                    "config": s.config,
                })
            })
            .collect();

        let callback_base_url = std::env::var("CALLBACK_BASE_URL")
            .unwrap_or_else(|_| "http://workflowswift:8085".to_string());
        let n8n_wf = n8n_converter::convert_steps_to_n8n(&step_values, tenant_id, id, &callback_base_url);
        let n8n_json = n8n_converter::to_n8n_json(&n8n_wf);

        // Deploy via n8n REST API
        let n8n_api_url = format!("{}/rest/workflows", state.config.n8n_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| AppError::Internal(format!("HTTP client: {}", e)))?;

        let mut req_builder = client.post(&n8n_api_url).json(&n8n_json);
        if !state.config.n8n_api_key.is_empty() {
            req_builder = req_builder.header("X-N8N-API-KEY", &state.config.n8n_api_key);
        }

        let api_resp = req_builder
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("n8n API request failed: {}", e)))?;

        let api_status = api_resp.status();
        if !api_status.is_success() {
            let error_text = api_resp.text().await.unwrap_or_else(|_| "unknown error".to_string());
            return Err(AppError::Internal(format!(
                "n8n workflow import failed ({}): {}",
                api_status, error_text
            )));
        }

        sqlx::query(
            "UPDATE workflows SET lifecycle_summary = $1 WHERE id = $2",
        )
        .bind(json!({
            "n8n_deployed": true,
            "n8n_webhook_path": n8n_wf.webhook_path,
            "deployed_at": chrono::Utc::now().to_rfc3339(),
        }).to_string())
        .bind(id)
        .execute(&state.db)
        .await?;
    }

    // Get the webhook path from lifecycle_summary
    let summary: serde_json::Value = workflow.lifecycle_summary
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({}));

    let webhook_path = summary.get("n8n_webhook_path")
        .and_then(|v| v.as_str())
        .ok_or(AppError::Internal(
            "Workflow deployed but missing webhook path. Try deploying again.".to_string(),
        ))?;

    // Check credits before triggering
    let balance = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if balance < 1 {
        return Err(AppError::BadRequest(
            "Insufficient credits. Please purchase more credits.".to_string(),
        ));
    }

    // Deduct 1 credit
    sqlx::query(
        r#"INSERT INTO credit_transactions (id, tenant_id, amount, transaction_type, description)
           VALUES ($1, $2, -1, 'workflow_execution', $3)"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(format!("Workflow execution: {}", workflow.name))
    .execute(&state.db)
    .await?;

    // Trigger n8n webhook
    let n8n_url = format!("{}/webhook/{}", state.config.n8n_webhook_url.trim_end_matches('/'), webhook_path);
    let client = reqwest::Client::new();
    let trigger_payload = json!({
        "tenant_id": claims.aid,
        "triggered_by": claims.sub,
        "payload": req.get("payload").cloned().unwrap_or(json!({})),
        "headers": {
            "authorization": req.get("authorization").and_then(|v| v.as_str()).unwrap_or(""),
        }
    });

    match client
        .post(&n8n_url)
        .json(&trigger_payload)
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let body: serde_json::Value = resp
                .json()
                .await
                .unwrap_or(json!({"note": "n8n responded without body"}));

            let new_balance = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE tenant_id = $1",
            )
            .bind(tenant_id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

            Ok(Json(json!({
                "status": "triggered",
                "n8n_status": status,
                "n8n_response": body,
                "remaining_balance": new_balance,
            })))
        }
        Err(e) => {
            // Refund on failure
            let _ = sqlx::query(
                r#"INSERT INTO credit_transactions (id, tenant_id, amount, transaction_type, description)
                   VALUES ($1, $2, 1, 'refund', $3)"#,
            )
            .bind(Uuid::new_v4())
            .bind(tenant_id)
            .bind(format!("Refund for failed execution: {}", workflow.name))
            .execute(&state.db)
            .await;

            Err(AppError::Internal(format!("n8n webhook call failed: {}", e)))
        }
    }
}
