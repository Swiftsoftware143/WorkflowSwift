use axum::{
    extract::{Json, Path, State},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::models::instance::*;
use crate::AppState;

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
    .ok_or(AppError::NotFound(
        "Workflow instance not found".to_string(),
    ))?;

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
        sqlx::query("UPDATE workflow_instances SET status = $1, updated_at = NOW() WHERE id = $2")
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
///
/// AUTH: this handler sits on the public router and authenticates itself, because
/// the caller it exists for — n8n — has no user JWT. Two credentials are accepted:
///
///   * `X-Internal-Key: <INTERNAL_SYNC_KEY>` — machine callers (n8n's HTTP node,
///     the same header `POST /api/v1/incoming` uses);
///   * `Authorization: Bearer <user JWT>` — the app itself, scoped to the caller's
///     own account (the instance must belong to `claims.aid`).
///
/// Before this, the handler's own comment claimed "no auth required — this is
/// called by n8n internally" while the route was mounted behind the JWT
/// middleware: n8n got a 401 on every callback, so no execution result could ever
/// be written back (kanban t_a6a1b299 item 4).
pub async fn instance_callback(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let internal_key = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let internal_ok = !state.config.internal_sync_key.is_empty()
        && internal_key == state.config.internal_sync_key;

    if !internal_ok {
        // Not a machine caller — require a user token and tenant scope.
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.strip_prefix("Bearer ").unwrap_or(v))
            .unwrap_or("");
        let claims = crate::auth::middleware::verify_token(token, &state.config.jwt_secret)
            .map_err(|_| AppError::Unauthorized)?;
        let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

        let owned: bool = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM workflow_instances WHERE id = $1 AND aid = $2)",
        )
        .bind(id)
        .bind(aid)
        .fetch_one(&state.db)
        .await
        .unwrap_or(false);

        if !owned {
            return Err(AppError::NotFound("Instance not found".to_string()));
        }
    }

    let status = req
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("completed");

    let result = req.get("result");
    let error_msg = req.get("error").and_then(|v| v.as_str());

    // Verify instance exists
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_instances WHERE id = $1)")
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
        let _ = sqlx::query("UPDATE workflow_instances SET error_text = $1 WHERE id = $2")
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

// NOTE (kanban t_afccb1a8): `POST /api/v1/instances/{id}/advance` and its handler `advance_instance`
// used to live here. The route and the handler were REMOVED, not repaired. Three measured reasons:
//
//   1. The only caller in the shipped product was the tenant shell's Instances row button, and that
//      call was malformed: the shell's `API.post(p, b)` omits the body when `b` is undefined but
//      still sends `Content-Type: application/json`, while this handler's extractor was a REQUIRED
//      `Json` — so every click answered 400 "Failed to parse the request body as JSON: EOF while
//      parsing a value at line 1 column 0" (reproduced in real Chromium against
//      app.workflowswift.com). The control was dead from the day it shipped.
//   2. Even a well-formed body advanced nothing. The handler read `current_step_order` from the
//      REQUEST and wrote that same value straight back, so the served body-less call carried 0 ->
//      no UPDATE; `completed` was absent -> no status change. The gesture was a no-op by
//      construction; it also could not settle a step, because settling needs the step's own id.
//   3. Progress is owned by the engine, not by a client-supplied index: `execution::walk` moves
//      `current_step_order` as it executes and a parked run is settled per step by
//      `decide_instance_step` (POST /instances/{id}/steps/{step_id}/decision) or by the background
//      worker for a due delay. Incrementing a pointer would have skipped a step without running it.
//
// Removing it also closes a write path: any tenant could have written an arbitrary
// `current_step_order` (and `completed: true`) onto its own instance, both of which only the engine
// should ever set. The former fan-out to `workflow_step_integrations` was retired in t_fa169e94;
// this removes the route that hosted it.

/// GET /api/v1/instances/{id}/logs — the run history of one instance.
///
/// `workflow_execution_logs` existed since migration 039 and was never written
/// nor read by anything: the spec asks for "execution logs / full run history"
/// and there was none (kanban t_a6a1b299 item 2). The engine now writes one row
/// per step attempt; this is the surface that reads them back, tenant-scoped.
pub async fn list_instance_logs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let owned: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM workflow_instances WHERE id = $1 AND aid = $2")
            .bind(id)
            .bind(aid)
            .fetch_optional(&state.db)
            .await?;

    if owned.is_none() {
        return Err(AppError::NotFound("Instance not found".to_string()));
    }

    // `instance_step_id` is what the decision endpoint takes (the log row's own
    // `step_id` is the workflow_steps definition, not the instance step), and
    // `due_date`/`instance_step_status` are what the UI needs to show a wait or a
    // gate as actionable instead of a dead end (kanban t_1ff4b916).
    let rows = sqlx::query(
        r#"SELECT l.id, l.step_id, l.step_type, l.step_name, l.sort_order, l.status, l.provider,
                  l.input_data, l.output_data, l.error_message, l.duration_ms, l.started_at, l.completed_at,
                  s.id AS instance_step_id, s.status AS instance_step_status, s.due_date
           FROM workflow_execution_logs l
           LEFT JOIN LATERAL (
               SELECT id, status, due_date
               FROM workflow_instance_steps
               WHERE instance_id = l.instance_id AND sort_order = l.sort_order
               ORDER BY created_at DESC
               LIMIT 1
           ) s ON TRUE
           WHERE l.instance_id = $1
           ORDER BY l.sort_order ASC, l.started_at ASC"#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let logs: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let step_id: Option<Uuid> = r.get("step_id");
            let provider: Option<String> = r.get("provider");
            let input_data: Option<serde_json::Value> = r.get("input_data");
            let output_data: Option<serde_json::Value> = r.get("output_data");
            let error_message: Option<String> = r.get("error_message");
            let duration_ms: Option<i32> = r.get("duration_ms");
            let started_at: Option<chrono::DateTime<chrono::Utc>> = r.get("started_at");
            let completed_at: Option<chrono::DateTime<chrono::Utc>> = r.get("completed_at");
            let instance_step_id: Option<Uuid> = r.get("instance_step_id");
            let instance_step_status: Option<String> = r.get("instance_step_status");
            let due_date: Option<chrono::DateTime<chrono::Utc>> = r.get("due_date");
            json!({
                "id": r.get::<Uuid, _>("id"),
                "step_id": step_id,
                "instance_step_id": instance_step_id,
                "instance_step_status": instance_step_status,
                "due_date": due_date,
                "step_type": r.get::<String, _>("step_type"),
                "step_name": r.get::<String, _>("step_name"),
                "sort_order": r.get::<i32, _>("sort_order"),
                "status": r.get::<String, _>("status"),
                "provider": provider,
                "input_data": input_data,
                "output_data": output_data,
                "error_message": error_message,
                "duration_ms": duration_ms,
                "started_at": started_at,
                "completed_at": completed_at,
            })
        })
        .collect();

    Ok(Json(json!({
        "instance_id": id.to_string(),
        "count": logs.len(),
        "logs": logs,
    })))
}

/// POST /api/v1/instances/{id}/steps/{step_id}/decision
///
/// The explicit advance path for the steps the engine cannot run by itself
/// (kanban t_1ff4b916 item 2). Before this, a `manual`/`approval` gate was a dead
/// end: the run stopped, said `pending`, and nothing could ever move it.
///
///   {"decision": "approve"}  -> the gate is settled as completed and the run
///                               continues with the steps behind it
///   {"decision": "reject"}   -> the gate is settled as failed and the run stops
///
/// Tenant-scoped: the step must belong to an instance of the caller's account;
/// anything else answers 404 (never "exists but forbidden").
///
/// It does NOT bill. The run was charged once at trigger time — settling a step
/// advances the existing instance and never creates a second billed run (item 3).
pub async fn decide_instance_step(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((id, step_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let raw = req
        .get("decision")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    let decision = match raw.as_str() {
        "approve" | "approved" => crate::execution::Decision::Approve,
        "reject" | "rejected" | "deny" | "denied" => crate::execution::Decision::Reject,
        _ => {
            return Err(AppError::Validation(
                "decision must be 'approve' or 'reject'".to_string(),
            ))
        }
    };

    let step = sqlx::query_as::<_, (Uuid, String, String)>(
        r#"SELECT s.id, s.step_type, s.status
           FROM workflow_instance_steps s
           JOIN workflow_instances i ON i.id = s.instance_id
           WHERE s.id = $1 AND s.instance_id = $2 AND i.aid = $3"#,
    )
    .bind(step_id)
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?;

    let (_, step_type, status) =
        step.ok_or_else(|| AppError::NotFound("Instance step not found".to_string()))?;

    if status != "pending" && status != "in_progress" {
        return Err(AppError::Validation(format!(
            "step is already '{}' — only a waiting step can be decided",
            status
        )));
    }

    match step_type.as_str() {
        "manual" | "approval" | "delay" | "wait" => {}
        other => {
            return Err(AppError::Validation(format!(
                "step type '{}' is not a human gate or a wait — it is settled by the engine",
                other
            )))
        }
    }

    // A wait can be fast-forwarded (that is an approve) but "rejecting" a timer
    // means nothing: refuse it rather than fail a run on a meaningless call.
    if decision == crate::execution::Decision::Reject
        && (step_type == "delay" || step_type == "wait")
    {
        return Err(AppError::Validation(
            "a delay/wait step can only be approved (run now) — reject applies to manual/approval steps"
                .to_string(),
        ));
    }

    // Atomic claim: a concurrent worker or a second click cannot double-settle.
    let claimed = sqlx::query(
        "UPDATE workflow_instance_steps SET status = 'in_progress' WHERE id = $1 AND status IN ('pending', 'in_progress')",
    )
    .bind(step_id)
    .execute(&state.db)
    .await?
    .rows_affected();

    if claimed == 0 {
        return Err(AppError::Validation(
            "step was settled by another caller — reload the run history".to_string(),
        ));
    }

    let actor = format!("user:{}", claims.sub);
    let advance = crate::execution::AdvanceRequest {
        step_instance_id: step_id,
        decision,
    };

    let outcome = match crate::execution::resume_instance(&state, id, Some(advance), &actor).await {
        Ok(o) => o,
        Err(e) => {
            // Hand the step back so the caller can retry; do not leave it stranded
            // in 'in_progress' on a transient failure.
            let _ = sqlx::query(
                "UPDATE workflow_instance_steps SET status = 'pending' WHERE id = $1 AND status = 'in_progress'",
            )
            .bind(step_id)
            .execute(&state.db)
            .await;
            return Err(e);
        }
    };

    // The decision settled this step; any OTHER step still waiting is normal
    // progress (its own timer or its own decision) and is reported through
    // `status: "in_progress"` + `pending_steps`, not as a warning.
    let warnings: Vec<String> = Vec::new();

    tracing::info!(
        instance_id = %id, step_id = %step_id, decision = %raw,
        instance_status = %outcome.status, actor = %actor,
        "instance step decided"
    );

    Ok(Json(json!({
        "instance_id": id.to_string(),
        "step_id": step_id.to_string(),
        "decision": raw,
        "status": outcome.status,
        "pending_steps": outcome.pending_steps,
        "failed_steps": outcome.failed_steps,
        "steps": outcome.steps,
        "warnings": warnings,
    })))
}
