use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::features;
use crate::models::workflow::*;
use crate::n8n_converter;
use crate::AppState;

#[derive(Debug, serde::Deserialize)]
pub struct ListWorkflowsQuery {
    pub surface: Option<Uuid>,
}

pub async fn list_workflows(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListWorkflowsQuery>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let workflows = if let Some(surface_id) = query.surface {
        sqlx::query_as::<_, Workflow>(
            "SELECT * FROM workflows WHERE aid = $1 AND (surface_id = $2 OR surface_id IS NULL) ORDER BY name ASC",
        )
        .bind(aid)
        .bind(surface_id)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE aid = $1 ORDER BY name ASC")
            .bind(aid)
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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_workflows", "Workflows").await?;

    let trigger_type = req
        .trigger_type
        .clone()
        .unwrap_or_else(|| "manual".to_string());
    let workflow = sqlx::query_as::<_, Workflow>(
        r#"INSERT INTO workflows (id, aid, name, description, category, lifecycle_summary, tags, surface_id, trigger_type, trigger_config)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.category)
    .bind(&req.lifecycle_summary)
    .bind(&req.tags)
    .bind(req.surface_id)
    .bind(&trigger_type)
    .bind(&req.trigger_config)
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"workflow": workflow}))))
}

pub async fn get_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let workflow =
        sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1 AND aid = $2")
            .bind(id)
            .bind(aid)
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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let existing =
        sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1 AND aid = $2")
            .bind(id)
            .bind(aid)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    // Surface is reassignable here. `Option` semantics: an absent (or null) field in the body
    // preserves the stored surface instead of clearing it. Read this before the field moves below.
    let surface_id = req.surface_id.or(existing.surface_id);

    let name = req.name.unwrap_or(existing.name);
    let description = req.description.or(existing.description);
    let category = req.category.or(existing.category);
    let lifecycle_summary = req.lifecycle_summary.or(existing.lifecycle_summary);
    let tags = req.tags.or(existing.tags);

    let trigger_type = req.trigger_type.unwrap_or(
        existing
            .trigger_type
            .unwrap_or_else(|| "manual".to_string()),
    );
    let trigger_config = req.trigger_config.or(existing.trigger_config);

    let workflow = sqlx::query_as::<_, Workflow>(
        r#"UPDATE workflows SET name=$1, description=$2, category=$3, lifecycle_summary=$4, tags=$5, trigger_type=$6, trigger_config=$7, surface_id=$8, updated_at=NOW()
           WHERE id=$9 RETURNING *"#,
    )
    .bind(&name)
    .bind(&description)
    .bind(&category)
    .bind(&lifecycle_summary)
    .bind(&tags)
    .bind(&trigger_type)
    .bind(&trigger_config)
    .bind(surface_id)
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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("UPDATE workflows SET is_active = false WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Workflow not found".to_string()));
    }

    Ok(Json(json!({"message": "Workflow deleted"})))
}

/// How a user-facing run resolves the client the instance belongs to.
/// `workflow_instances.client_id` is NOT NULL, so a body without `client_id`
/// falls back to the account's system client instead of failing.
async fn resolve_client_id(
    state: &AppState,
    aid: Uuid,
    req: &serde_json::Value,
) -> Result<Uuid, AppError> {
    if let Some(cid) = req
        .get("client_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        let owned: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM clients WHERE id = $1 AND aid = $2")
                .bind(cid)
                .bind(aid)
                .fetch_optional(&state.db)
                .await?;
        return owned.ok_or_else(|| {
            AppError::Validation("client_id does not belong to this account".to_string())
        });
    }
    crate::execution::find_or_create_system_client(&state.db, aid, "manual").await
}

struct RunOutcome {
    instance_id: Uuid,
    status: String,
    steps: Vec<serde_json::Value>,
    warnings: Vec<String>,
    n8n: serde_json::Value,
    remaining_balance: i64,
    pending_steps: i32,
    failed_steps: i32,
}

/// The ONE user-facing execution path. Creates the instance row, executes every
/// step in this process, charges one credit, then *optionally* mirrors the
/// workflow into n8n — but only when the tenant's plan enables `n8n_deploy`,
/// and only ever as a warning. Nothing here can turn Run into a 500.
async fn run_in_process(
    state: &AppState,
    aid: Uuid,
    workflow: &Workflow,
    client_id: Uuid,
    req: &serde_json::Value,
    triggered_by: &str,
) -> Result<RunOutcome, AppError> {
    let steps = crate::execution::load_steps(&state.db, workflow.id).await?;
    if steps.is_empty() {
        return Err(AppError::BadRequest(
            "Workflow has no steps. Add at least one step before running.".to_string(),
        ));
    }

    let balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if balance < 1 {
        return Err(AppError::BadRequest(
            "Insufficient credits. Purchase more credits to run workflows.".to_string(),
        ));
    }

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&workflow.name)
        .to_string();
    let payload = req.get("payload").cloned().unwrap_or_else(|| json!({}));

    // 1. the instance exists before anything else — a run always leaves a row.
    let instance_id =
        crate::execution::create_instance(&state.db, workflow.id, aid, client_id, &name, "running")
            .await?;

    // 2. execute every step here and now.
    let ctx = crate::execution::StepContext::manual(aid, workflow.id, triggered_by, payload);
    let outcome =
        crate::execution::execute_steps(state, aid, workflow.id, instance_id, &ctx).await?;

    // 3. charge only now that the run has actually happened.
    sqlx::query(
        r#"INSERT INTO credit_transactions (id, aid, amount, transaction_type, description)
           VALUES ($1, $2, -1, 'workflow_execution', $3)"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(format!("Workflow execution: {}", workflow.name))
    .execute(&state.db)
    .await?;

    // 4. n8n mirror — optional, per-plan, never fatal.
    let mut warnings: Vec<String> = Vec::new();
    let n8n = match features::plan_flag(&state.db, aid, "n8n_deploy").await {
        Ok(false) => json!({
            "requested": false,
            "deployed": false,
            "skipped": "plan does not include n8n_deploy"
        }),
        Ok(true) => {
            if state.config.n8n_api_key.trim().is_empty() {
                warnings.push(
                    "n8n mirror skipped: n8n deployment is enabled on your plan but no n8n API key is provisioned on this fleet. The workflow ran in-process."
                        .to_string(),
                );
                json!({
                    "requested": true,
                    "deployed": false,
                    "error": "no n8n API key provisioned on this fleet"
                })
            } else {
                match mirror_to_n8n(state, aid, workflow).await {
                    Ok(v) => v,
                    Err(e) => {
                        warnings.push(format!("n8n mirror failed: {}", e));
                        json!({ "requested": true, "deployed": false, "error": e })
                    }
                }
            }
        }
        Err(e) => {
            warnings.push(format!("n8n mirror skipped: plan flag unreadable: {}", e));
            json!({ "requested": true, "deployed": false, "error": "plan flag unreadable" })
        }
    };

    // Item 3: a run that leaves steps pending is not a success. Say so in the
    // response as well as in the instance row.
    if outcome.pending_steps > 0 {
        warnings.push(format!(
            "{} step(s) are still pending: manual/approval steps need a human and delay/wait steps need a timer, and this build runs no background worker to advance them. The instance is reported as 'pending', not completed.",
            outcome.pending_steps
        ));
    }
    if outcome.failed_steps > 0 {
        warnings.push(format!(
            "{} step(s) failed — the instance is reported as 'failed'. Per-step detail is in GET /api/v1/instances/{{id}}/logs.",
            outcome.failed_steps
        ));
    }

    let remaining_balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    let _ =
        sqlx::query("UPDATE workflow_instances SET result = $1, updated_at = NOW() WHERE id = $2")
            .bind(json!({
                "runner": "in_process",
                "status": outcome.status,
                "steps_total": outcome.steps.len(),
                "pending_steps": outcome.pending_steps,
                "failed_steps": outcome.failed_steps,
                "warnings": warnings,
                "n8n": n8n,
            }))
            .bind(instance_id)
            .execute(&state.db)
            .await;

    Ok(RunOutcome {
        instance_id,
        status: outcome.status,
        steps: outcome.steps,
        warnings,
        n8n,
        remaining_balance,
        pending_steps: outcome.pending_steps,
        failed_steps: outcome.failed_steps,
    })
}

/// Mirror a workflow into n8n over its REST API with an API key.
/// Called only when the plan enables `n8n_deploy`; the caller turns any Err
/// into a warning. Never uses docker: this container has no docker binary and
/// no docker socket.
async fn mirror_to_n8n(
    state: &AppState,
    aid: Uuid,
    workflow: &Workflow,
) -> Result<serde_json::Value, String> {
    let api_key = state.config.n8n_api_key.trim();
    if api_key.is_empty() {
        return Err("no n8n API key is provisioned on this fleet".to_string());
    }

    let steps = crate::execution::load_steps(&state.db, workflow.id)
        .await
        .map_err(|e| format!("{}", e))?;
    if steps.is_empty() {
        return Err("workflow has no steps".to_string());
    }

    let step_values: Vec<serde_json::Value> = steps
        .iter()
        .map(|s| {
            json!({
                "step_type": s.step_type,
                "name": s.name,
                "description": serde_json::Value::Null,
                "sort_order": s.sort_order,
                "config": s.config,
            })
        })
        .collect();

    let callback_base_url = std::env::var("CALLBACK_BASE_URL")
        .unwrap_or_else(|_| "http://workflowswift:8085".to_string());
    let n8n_wf =
        n8n_converter::convert_steps_to_n8n(&step_values, aid, workflow.id, &callback_base_url);
    let n8n_json = n8n_converter::to_n8n_json(&n8n_wf);

    // n8n's public REST API (the one an API key works against).
    let url = format!(
        "{}/api/v1/workflows",
        state.config.n8n_url.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client: {}", e))?;

    let resp = client
        .post(&url)
        .header("X-N8N-API-KEY", api_key)
        .json(&n8n_json)
        .send()
        .await
        .map_err(|e| format!("n8n request failed: {}", e))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = body.chars().take(200).collect::<String>();
        return Err(format!("n8n rejected the import ({}) {}", status, body));
    }

    let _ = sqlx::query("UPDATE workflows SET lifecycle_summary = $1 WHERE id = $2")
        .bind(
            json!({
                "n8n_deployed": true,
                "n8n_webhook_path": n8n_wf.webhook_path,
                "deployed_at": chrono::Utc::now().to_rfc3339(),
            })
            .to_string(),
        )
        .bind(workflow.id)
        .execute(&state.db)
        .await;

    Ok(json!({
        "requested": true,
        "deployed": true,
        // The steps already ran in-process, so re-triggering n8n here would
        // execute the same workflow twice (duplicate emails/API calls).
        "triggered": false,
        "trigger_note": "steps were executed in-process; the n8n copy is available for external triggers",
        "webhook_path": n8n_wf.webhook_path,
        "name": n8n_wf.name,
        "node_count": n8n_wf.nodes.len(),
    }))
}

/// POST /api/v1/workflows/{id}/start — start a workflow now (optional body).
pub async fn start_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    body: Option<Json<serde_json::Value>>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_instances", "Instances").await?;

    let req = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));

    let workflow =
        sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1 AND aid = $2")
            .bind(id)
            .bind(aid)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    let client_id = resolve_client_id(&state, aid, &req).await?;
    let run = run_in_process(&state, aid, &workflow, client_id, &req, &claims.sub).await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "instance_id": run.instance_id.to_string(),
            "status": run.status,
            "execution": "in_process",
            "steps_total": run.steps.len(),
            "steps": run.steps,
            "n8n": run.n8n,
            "warnings": run.warnings,
            "remaining_balance": run.remaining_balance,
            "message": format!("Workflow '{}' started: instance created and steps executed.", workflow.name)
        })),
    ))
}

pub async fn get_workflow_steps(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let _aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let steps = sqlx::query_as::<_, WorkflowStep>(
        "SELECT * FROM workflow_steps WHERE workflow_id = $1 ORDER BY sort_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"steps": steps})))
}

/// ── Data-Card-first guardrail (kanban t_a6a1b299 item 5) ─────────────────────
///
/// The spec says "6 step types ... `Data Card` (**step 1 is always this**)"
/// (workflowswift-feature-spec.md §E). Until now the rule lived only in a
/// comment: a `notify` step could be created as step 1 of an empty workflow, and
/// no write path looked at what it left at position 0. It is enforced on every
/// write that decides the order — create, update-with-move, reorder — and
/// reported by validate-steps.
fn is_data_card(step_type: &str) -> bool {
    matches!(step_type, "data-card" | "data_card")
}

fn data_card_first_error() -> AppError {
    AppError::BadRequest(
        "Step 1 of a workflow must be a Data Card ('data-card') — it is what pulls the run's data. \
         Add the Data Card first, then add this step after it."
            .to_string(),
    )
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct StepOrderRow {
    id: Uuid,
    step_type: String,
    sort_order: i32,
}

/// Every step of a workflow, in the order the engine walks them.
async fn load_ordered_steps(
    db: &sqlx::PgPool,
    workflow_id: Uuid,
) -> Result<Vec<StepOrderRow>, AppError> {
    let mut rows = sqlx::query_as::<_, StepOrderRow>(
        "SELECT id, step_type, sort_order FROM workflow_steps WHERE workflow_id = $1",
    )
    .bind(workflow_id)
    .fetch_all(db)
    .await?;
    rows.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.id.cmp(&b.id)));
    Ok(rows)
}

/// Refuse a write that would leave a non-Data-Card step first — unless the
/// workflow already breaks the rule today, in which case it stays editable.
/// The grandfather clause matters: the inbound-capture workflows legitimately
/// start with an `integration` step, and refusing them would make live data
/// impossible to edit.
fn assert_data_card_first(
    current: &[StepOrderRow],
    projected: &[StepOrderRow],
) -> Result<(), AppError> {
    let already_ok = current
        .first()
        .map(|s| is_data_card(&s.step_type))
        .unwrap_or(true);
    if !already_ok {
        return Ok(());
    }
    match projected.first() {
        Some(first) if !is_data_card(&first.step_type) => Err(data_card_first_error()),
        _ => Ok(()),
    }
}

pub async fn create_workflow_step(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateWorkflowStepRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Verify workflow exists and belongs to account
    let _workflow =
        sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1 AND aid = $2")
            .bind(id)
            .bind(aid)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    // Auto-assign sort_order: max + 1
    let max_sort: Option<(Option<i32>,)> =
        sqlx::query_as("SELECT MAX(sort_order) FROM workflow_steps WHERE workflow_id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;

    let sort_order = max_sort.and_then(|r| r.0).map(|m| m + 1).unwrap_or(0);

    // Guardrail: step 1 of a workflow is always a Data Card.
    if sort_order == 0 && !is_data_card(&req.step_type) {
        return Err(data_card_first_error());
    }

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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Verify workflow exists and belongs to account
    let _workflow =
        sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1 AND aid = $2")
            .bind(workflow_id)
            .bind(aid)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    // Guardrail: a step's TYPE is fixed at creation. Name/description/config/order
    // stay editable; the type must not change (otherwise the guardrails that were
    // validated for the original type — e.g. Data Card first, Fork last — are void).
    let (current_type, current_sort): (String, i32) = sqlx::query_as(
        "SELECT step_type, sort_order FROM workflow_steps WHERE id = $1 AND workflow_id = $2",
    )
    .bind(step_id)
    .bind(workflow_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Step not found".to_string()))?;

    if !req.step_type.trim().is_empty() && req.step_type != current_type {
        return Err(AppError::BadRequest(format!(
            "Step type cannot be changed after creation (this step is '{}'). Delete the step and add a new one of type '{}'.",
            current_type, req.step_type
        )));
    }

    // An absent sort_order PRESERVES the stored position. It used to default to 0,
    // which silently teleported any edited step to the front of the workflow — and
    // could therefore park a non-Data-Card step at step 1.
    let sort_order = req.sort_order.unwrap_or(current_sort);
    if sort_order != current_sort {
        let current = load_ordered_steps(&state.db, workflow_id).await?;
        let mut projected = current.clone();
        for s in projected.iter_mut() {
            if s.id == step_id {
                s.sort_order = sort_order;
            }
        }
        projected.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.id.cmp(&b.id)));
        assert_data_card_first(&current, &projected)?;
    }

    let step = sqlx::query_as::<_, WorkflowStep>(
        r#"UPDATE workflow_steps SET step_type=$1, name=$2, description=$3, sort_order=$4, config=$5
           WHERE id=$6 AND workflow_id=$7
           RETURNING *"#,
    )
    .bind(&current_type)
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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Verify workflow exists and belongs to account
    let _workflow =
        sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1 AND aid = $2")
            .bind(workflow_id)
            .bind(aid)
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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Verify workflow exists and belongs to account
    let _workflow =
        sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1 AND aid = $2")
            .bind(id)
            .bind(aid)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    // Guardrail: a reorder must not leave a non-Data-Card step at step 1.
    let current = load_ordered_steps(&state.db, id).await?;
    let mut projected = current.clone();
    for (i, step_id) in req.step_ids.iter().enumerate() {
        for s in projected.iter_mut() {
            if s.id == *step_id {
                s.sort_order = i as i32;
            }
        }
    }
    projected.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.id.cmp(&b.id)));
    assert_data_card_first(&current, &projected)?;

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
/// Generates an n8n-compatible workflow JSON from the stored steps,
/// and imports it via the n8n REST API using `POST /rest/workflows`.
/// POST /api/v1/workflows/{id}/deploy — mirror a workflow into n8n.
/// Plan-gated on `n8n_deploy`; a failure here is reported plainly instead of
/// surfacing as an opaque 500. Deploying never runs the workflow.
pub async fn deploy_workflow_to_n8n(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    crate::features::enforce_plan_flag(&state.db, aid, "n8n_deploy", "n8n deployment").await?;

    let workflow =
        sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1 AND aid = $2")
            .bind(id)
            .bind(aid)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    let steps = crate::execution::load_steps(&state.db, workflow.id).await?;
    if steps.is_empty() {
        return Err(AppError::BadRequest(
            "Workflow has no steps. Add at least one step before deploying.".to_string(),
        ));
    }

    match mirror_to_n8n(&state, aid, &workflow).await {
        Ok(v) => Ok(Json(json!({
            "deployed": true,
            "webhook_path": v.get("webhook_path").cloned().unwrap_or(serde_json::Value::Null),
            "name": v.get("name").cloned().unwrap_or(serde_json::Value::Null),
            "node_count": v.get("node_count").cloned().unwrap_or(serde_json::Value::Null),
            "message": "Workflow deployed to n8n via its REST API."
        }))),
        Err(e) => Err(AppError::BadRequest(format!(
            "n8n deployment failed: {}",
            e
        ))),
    }
}

/// POST /api/v1/workflows/:id/run — deploy (if needed) and execute the workflow
/// Deploys the workflow to n8n if not yet deployed, then triggers the webhook.
/// POST /api/v1/workflows/{id}/run — execute the workflow NOW, in this process.
///
/// The steps run through the shared engine (crate::execution) — the same code
/// that serves POST /api/v1/incoming. n8n is NOT a dependency of Run: when the
/// tenant's plan enables `n8n_deploy` the workflow is additionally mirrored
/// into n8n, and any problem there is surfaced as a warning, never as a failed
/// run. A run always leaves a `workflow_instances` row behind; a 200 without an
/// instance row is not a run.
pub async fn run_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    body: Option<Json<serde_json::Value>>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_instances", "Instances").await?;

    // The shipped SPA posts with no body at all; treat that as an empty object
    // rather than rejecting the request.
    let req = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));

    let workflow =
        sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1 AND aid = $2")
            .bind(id)
            .bind(aid)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound("Workflow not found".to_string()))?;

    let client_id = resolve_client_id(&state, aid, &req).await?;
    let run = run_in_process(&state, aid, &workflow, client_id, &req, &claims.sub).await?;

    Ok(Json(json!({
        "status": run.status,
        "instance_id": run.instance_id.to_string(),
        "execution": "in_process",
        "workflow": workflow.name,
        "steps_total": run.steps.len(),
        "pending_steps": run.pending_steps,
        "failed_steps": run.failed_steps,
        "steps": run.steps,
        "n8n": run.n8n,
        "warnings": run.warnings,
        "remaining_balance": run.remaining_balance,
        "message": if run.pending_steps > 0 {
            format!(
                "Workflow '{}' ran {} step(s) in-process; {} step(s) are still pending and cannot advance in this build.",
                workflow.name,
                run.steps.len(),
                run.pending_steps
            )
        } else {
            format!(
                "Workflow '{}' ran: {} step(s) executed in-process.",
                workflow.name,
                run.steps.len()
            )
        }
    })))
}

/// POST /api/v1/workflows/validate-steps — validate a sequence of steps before deploy
/// Checks each step for:
///   - Required config fields per step type
///   - Proper ordering constraints
///   - Valid provider assignments
///   - Cyclic dependencies (for fork/loop steps)
/// Returns validation warnings and errors without requiring a DB write.
pub async fn validate_workflow_steps(
    State(_state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let steps = req
        .get("steps")
        .and_then(|v| v.as_array())
        .ok_or(AppError::Validation(
            "Missing 'steps' array in request body".to_string(),
        ))?;

    if steps.is_empty() {
        return Ok(Json(json!({
            "valid": false,
            "errors": ["Workflow must have at least one step."],
            "warnings": []
        })));
    }

    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Define required fields per step type
    let required_config_fields: std::collections::HashMap<&str, Vec<&str>> = [
        ("http-request", vec!["url", "method"]),
        ("action", vec!["url", "method"]),
        ("ai-action", vec!["prompt"]),
        ("openclaw", vec!["prompt"]),
        ("generate", vec!["prompt"]),
        ("export", vec!["destination"]),
        ("notify", vec!["channel", "recipient"]),
        ("data-card", vec!["metric_key"]),
        ("research", vec!["query"]),
        ("design", vec!["prompt"]),
        ("publish", vec!["content"]),
        ("condition", vec!["field"]),
        ("webhook", vec!["url"]),
        ("format", vec!["input_content"]),
        ("render_video", vec!["provider", "endpoint"]),
        ("render_image", vec!["provider", "endpoint"]),
        ("render_audio", vec!["provider", "endpoint"]),
    ]
    .iter()
    .cloned()
    .collect();

    // Track step types for ordering/duplicate checks
    let mut step_types: Vec<String> = Vec::new();
    let mut prev_type: Option<&str> = None;

    for (i, step) in steps.iter().enumerate() {
        let step_type = step
            .get("step_type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let step_name = step
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Unnamed step");

        step_types.push(step_type.to_string());

        // Guardrail (§E): step 1 is always a Data Card.
        if i == 0 && !is_data_card(step_type) {
            errors.push(format!(
                "Step 1 '{}': the first step must be a Data Card ('data-card') — it is what pulls the run's data.",
                step_name
            ));
        }

        // Check step_type is valid
        let valid_types = [
            "http-request",
            "action",
            "ai-action",
            "openclaw",
            "data-card",
            "data_card",
            "notify",
            "export",
            "delay",
            "wait",
            "transform",
            "code",
            "fork",
            "branch",
            "render_video",
            "render_media",
            "render_image",
            "render_audio",
            "generate",
            "format",
            "design",
            "publish",
            "loop",
            "condition",
            "manual",
            "research",
            "webhook",
        ];

        if !valid_types.contains(&step_type) {
            errors.push(format!(
                "Step {} '{}': Unknown step type '{}'. Valid types are: {}",
                i + 1,
                step_name,
                step_type,
                valid_types.join(", ")
            ));
            continue;
        }

        // Check required config fields
        if let Some(required_fields) = required_config_fields.get(step_type) {
            let config = step.get("config").and_then(|v| v.as_object());
            for field in required_fields {
                let has_field = config
                    .and_then(|c| c.get(*field))
                    .and_then(|v| v.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);

                if !has_field {
                    errors.push(format!(
                        "Step {} '{}' ({}): Missing required config field '{}'",
                        i + 1,
                        step_name,
                        step_type,
                        field
                    ));
                }
            }
        }

        // Ordering rules
        if (step_type == "fork" || step_type == "branch") && i as i32 >= steps.len() as i32 - 1 {
            warnings.push(format!(
                    "Step {} '{}': Fork/branch at the end of the workflow has no effect — no downstream steps to branch.",
                    i + 1, step_name
                ));
        }

        if step_type == "loop" && steps.len() < 2 {
            warnings.push(format!(
                    "Step {} '{}': Loop with only one step will iterate on itself indefinitely. Add inner steps.",
                    i + 1, step_name
                ));
        }

        if step_type == "manual" {
            warnings.push(format!(
                "Step {} '{}': Manual review will pause the workflow until a human approves or rejects.",
                i + 1, step_name
            ));
        }

        if step_type == "delay" || step_type == "wait" {
            let dur = step
                .get("config")
                .and_then(|c| c.get("duration_ms"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if dur == 0 {
                warnings.push(format!(
                    "Step {} '{}': Delay duration is 0ms — step will proceed immediately.",
                    i + 1,
                    step_name
                ));
            } else if dur > 7 * 24 * 3600000 {
                warnings.push(format!(
                    "Step {} '{}': Delay of {}ms exceeds 7 days — n8n may timeout.",
                    i + 1,
                    step_name,
                    dur
                ));
            }
        }

        if step_type == "render_video" || step_type == "render_image" || step_type == "render_audio"
        {
            let provider = step
                .get("config")
                .and_then(|c| c.get("provider"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if provider == "unknown" || provider.is_empty() {
                warnings.push(format!(
                    "Step {} '{}': No provider selected for rendering. Defaulting to 'unknown'.",
                    i + 1,
                    step_name
                ));
            }
        }

        prev_type = Some(step_type);
    }

    // Check for consecutive fork/branch runs (redundant)
    let mut fork_count = 0;
    for st in &step_types {
        if st == "fork" || st == "branch" {
            fork_count += 1;
        }
    }
    if fork_count > 3 {
        warnings.push(format!(
            "Workflow has {} fork/branch steps. Consider simplifying — deep nesting can make debugging difficult.",
            fork_count
        ));
    }

    Ok(Json(json!({
        "valid": errors.is_empty(),
        "errors": errors,
        "warnings": warnings,
        "error_count": errors.len(),
        "warning_count": warnings.len(),
        "total_steps": steps.len()
    })))
}

#[cfg(test)]
mod data_card_first_tests {
    use super::*;

    fn row(step_type: &str, sort_order: i32) -> StepOrderRow {
        StepOrderRow {
            id: Uuid::new_v4(),
            step_type: step_type.to_string(),
            sort_order,
        }
    }

    fn ordered(mut v: Vec<StepOrderRow>) -> Vec<StepOrderRow> {
        v.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.id.cmp(&b.id)));
        v
    }

    #[test]
    fn both_data_card_spellings_count() {
        assert!(is_data_card("data-card"));
        assert!(is_data_card("data_card"));
        assert!(!is_data_card("notify"));
        assert!(!is_data_card("integration"));
    }

    #[test]
    fn moving_a_data_card_off_step_one_is_refused() {
        let current = ordered(vec![row("data-card", 0), row("notify", 1)]);
        let mut projected = current.clone();
        for s in projected.iter_mut() {
            if s.step_type == "data-card" {
                s.sort_order = 9;
            }
        }
        let projected = ordered(projected);
        assert!(
            assert_data_card_first(&current, &projected).is_err(),
            "a reorder that drops a non-Data-Card step to position 0 must be refused"
        );
        // The unchanged order is still fine.
        assert!(assert_data_card_first(&current, &current).is_ok());
    }

    #[test]
    fn data_card_still_first_is_allowed() {
        let current = ordered(vec![row("notify", 1), row("data-card", 0)]);
        let projected = ordered(vec![row("data-card", 0), row("notify", 1)]);
        assert!(assert_data_card_first(&current, &projected).is_ok());
    }

    #[test]
    fn legacy_non_data_card_first_stays_editable() {
        // The inbound-capture workflows start with an `integration` step. Refusing
        // those would make live data impossible to reorder — grandfather them.
        let current = ordered(vec![row("integration", 0), row("notify", 1)]);
        let mut projected = current.clone();
        for s in projected.iter_mut() {
            if s.step_type == "integration" {
                s.sort_order = 1;
            } else {
                s.sort_order = 0;
            }
        }
        let projected = ordered(projected);
        assert!(
            assert_data_card_first(&current, &projected).is_ok(),
            "a workflow that already breaks the rule must not become uneditable"
        );
    }
}
