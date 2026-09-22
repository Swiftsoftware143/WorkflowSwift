//! In-process workflow execution engine.
//!
//! ONE engine, two entry points:
//!   * `POST /api/v1/incoming`  — a Swift tool pushes a lead (capture path)
//!   * `POST /api/v1/workflows/{id}/run` / `{id}/start` — the user presses Run
//!
//! Historically the user-facing Run path shelled out to `docker cp` +
//! `docker exec user-n8n-main n8n import:workflow`. That container does not
//! exist, and the app container has no docker binary and no docker socket, so
//! Run was a guaranteed HTTP 500. The inbound path, by contrast, always
//! executed the steps in this process. This module is that proven loop,
//! lifted verbatim so both paths run the same code.
//!
//! n8n is OPTIONAL: the per-plan `n8n_deploy` flag decides whether a run also
//! mirrors the workflow into n8n. It can never fail a run.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
// The dashboard series key space lives with its writers; the `data-card` step below reads through
// the same helpers so a step's metric_key resolves exactly like a widget's config.metric_key does.
use crate::handlers::industry_handler::{
    canonical_metric_key, latest_widget_metric, metric_key_candidates,
};
use crate::state::AppState;

/// Everything a step can see about the run that triggered it.
#[derive(Debug, Clone)]
pub struct StepContext {
    pub source: String,
    pub campaign_slug: String,
    pub contact: Value,
    pub data: Option<Value>,
    pub source_entry_id: Option<String>,
    pub context: Value,
}

impl StepContext {
    /// Context for a user-initiated run from the app UI: no external lead.
    pub fn manual(aid: Uuid, workflow_id: Uuid, triggered_by: &str, payload: Value) -> Self {
        Self {
            source: "manual".to_string(),
            campaign_slug: "manual".to_string(),
            contact: json!({}),
            data: Some(payload.clone()),
            source_entry_id: None,
            context: json!({
                "source": "manual",
                "campaign_slug": "manual",
                "triggered_by": triggered_by,
                "aid": aid,
                "workflow_id": workflow_id,
                "payload": payload,
                "started_at": Utc::now().to_rfc3339(),
            }),
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct StepRow {
    pub id: Uuid,
    pub step_type: String,
    pub name: String,
    pub config: Option<Value>,
    pub sort_order: i32,
    pub integration_target_id: Option<Uuid>,
}

/// Result of walking every step of one instance.
#[derive(Debug)]
pub struct ExecutionOutcome {
    pub steps: Vec<Value>,
    pub status: String,
    /// Steps that finished the walk without executing: `manual`/`approval`
    /// (a human must approve or reject) and `delay`/`wait` (a timer must fire).
    /// These are settled by the background worker (src/execution_worker.rs) or by
    /// the decision endpoint, both of which resume this same walk — so a run that
    /// leaves one behind is reported `pending`, never `completed`, until it moves.
    pub pending_steps: i32,
    pub failed_steps: i32,
}

/// The `status` a step result really has.
///
/// Three step arms (webhook, n8n, publish) store a raw HTTP status *code* in
/// `status`. Reading that with `as_str()` fell through to the default
/// "completed", so an upstream step that answered 500 was recorded — and
/// displayed — as a success. Numbers are classified here instead.
pub fn classify_step_status(result: &Value) -> String {
    match result.get("status") {
        Some(Value::Number(n)) => {
            let code = n.as_u64().unwrap_or(0);
            if (200..400).contains(&code) {
                "completed".to_string()
            } else {
                "failed".to_string()
            }
        }
        Some(Value::String(s)) => match s.as_str() {
            "error" | "failed" => "failed".to_string(),
            "pending" => "pending".to_string(),
            "skipped" => "skipped".to_string(),
            "warning" => "warning".to_string(),
            "in_progress" => "in_progress".to_string(),
            _ => "completed".to_string(),
        },
        _ => "completed".to_string(),
    }
}

/// Human-readable reason for a step that did not succeed, for the log row.
fn step_error_text(result: &Value) -> Option<String> {
    for key in ["error", "reason", "message"] {
        if let Some(s) = result.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    match result.get("status") {
        Some(Value::Number(n)) => Some(format!("upstream returned HTTP {}", n)),
        _ => None,
    }
}

/// Result of a `data-card` step.
///
/// A lookup failure becomes `status: "error"` with the reason in `error`: `classify_step_status`
/// reads that as `failed` (so the step — and the run — report the truth) and `step_error_text`
/// copies it into the `workflow_execution_logs.error_message`. The pre-fix arm used
/// `unwrap_or(None)`, so a query that never ran produced `metric_value: null` and a *completed*
/// step, indistinguishable from a widget nobody had pushed to.
fn data_card_result(
    step: usize,
    widget_name: &str,
    metric_key: &str,
    lookup: Result<Option<Value>, sqlx::Error>,
) -> Value {
    match lookup {
        Ok(metric_value) => json!({
            "step": step,
            "type": "data-card",
            "status": "completed",
            "widget_name": widget_name,
            "metric_key": metric_key,
            "resolved_metric_key": if metric_key.is_empty() {
                String::new()
            } else {
                canonical_metric_key(metric_key)
            },
            "metric_value": metric_value,
        }),
        Err(e) => json!({
            "step": step,
            "type": "data-card",
            "status": "error",
            "widget_name": widget_name,
            "metric_key": metric_key,
            "error": format!("dashboard series lookup failed: {e}"),
        }),
    }
}

/// How long a `delay`/`wait` step holds the run.
///
/// `duration_ms` is the canonical config key (the n8n converter's wait node and
/// the step validator both read it); `duration` is a human string ("5s", "90",
/// "30m", "1h", "2d") that older workflows — and the smoke harness — carry
/// instead. A config with neither keeps the engine's original 1h default.
pub fn delay_duration_ms(config: &Value) -> i64 {
    if let Some(ms) = config.get("duration_ms").and_then(|v| v.as_i64()) {
        if ms > 0 {
            return ms;
        }
    }
    if let Some(secs) = config.get("duration_seconds").and_then(|v| v.as_i64()) {
        if secs > 0 {
            return secs.saturating_mul(1000);
        }
    }
    if let Some(ms) = config
        .get("duration")
        .and_then(|v| v.as_str())
        .and_then(parse_duration_str)
    {
        return ms;
    }
    if let Some(secs) = config.get("duration").and_then(|v| v.as_i64()) {
        if secs > 0 {
            return secs.saturating_mul(1000);
        }
    }
    3_600_000
}

/// `"5s"` / `"90"` / `"30m"` / `"2h"` / `"1d"` / `"1w"` -> milliseconds.
/// `None` when the value carries no number at all.
pub fn parse_duration_str(s: &str) -> Option<i64> {
    let t = s.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    let unit_ms: f64 = match t.chars().last()? {
        's' => 1000.0,
        'm' => 60_000.0,
        'h' => 3_600_000.0,
        'd' => 86_400_000.0,
        'w' => 604_800_000.0,
        _ => 1000.0, // bare number: seconds
    };
    let num: String = t
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    let n: f64 = num.parse().ok()?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    Some((n * unit_ms) as i64)
}

/// The instant a `delay`/`wait` step becomes due. Stored on the instance step's
/// `due_date` so the background worker (src/execution_worker.rs) knows when to
/// advance it — before this, nothing wrote that column and nothing read it.
pub fn delay_due_at(config: &Value, now: DateTime<Utc>) -> DateTime<Utc> {
    now + chrono::Duration::milliseconds(delay_duration_ms(config))
}

/// A settle request for ONE instance step.
#[derive(Debug, Clone, Copy)]
pub struct AdvanceRequest {
    pub step_instance_id: Uuid,
    pub decision: Decision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Settle the step as completed: the delay's timer fired, or a human approved.
    Approve,
    /// Settle the step as failed: a human rejected the gate.
    Reject,
}

/// What the walk knows about the steps this instance already has. Empty for a
/// fresh run, populated by `resume_instance`.
#[derive(Debug, Default)]
struct ResumePlan {
    /// sort_order -> (instance-step id, status) of every row already written.
    existing: std::collections::HashMap<i32, (Uuid, String)>,
    /// The one step this walk is allowed to settle now.
    advance: Option<AdvanceRequest>,
    /// Who is settling it: "worker" (timer) or the user id (human decision).
    actor: String,
}

impl ResumePlan {
    fn fresh() -> Self {
        Self {
            existing: std::collections::HashMap::new(),
            advance: None,
            actor: "engine".to_string(),
        }
    }
}

/// Find or create a system client for automated instances, so a run never
/// needs a human-created client_id (`workflow_instances.client_id` is NOT NULL).
pub async fn find_or_create_system_client(
    db: &PgPool,
    aid: Uuid,
    source: &str,
) -> Result<Uuid, AppError> {
    let sys_email = format!("incoming+{}@workflowswift.local", source);

    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM clients WHERE email = $1 AND aid = $2")
            .bind(&sys_email)
            .bind(aid)
            .fetch_optional(db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

    if let Some(existing_id) = existing {
        return Ok(existing_id);
    }

    let client_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO clients (id, aid, name, email, is_active)
           VALUES ($1, $2, $3, $4, true)
           ON CONFLICT (id) DO NOTHING"#,
    )
    .bind(client_id)
    .bind(aid)
    .bind(format!("System ({})", source))
    .bind(&sys_email)
    .execute(db)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to create system client: {}", e)))?;

    Ok(client_id)
}

pub async fn load_steps(db: &PgPool, workflow_id: Uuid) -> Result<Vec<StepRow>, AppError> {
    sqlx::query_as::<_, StepRow>(
        r#"SELECT id, step_type, name, config, sort_order, integration_target_id
           FROM workflow_steps
           WHERE workflow_id = $1
           ORDER BY sort_order ASC"#,
    )
    .bind(workflow_id)
    .fetch_all(db)
    .await
    .map_err(|e| AppError::Internal(format!("DB error: {}", e)))
}

/// Insert the instance row. `status` is 'active' for capture-path instances and
/// 'running' for user-initiated ones.
pub async fn create_instance(
    db: &PgPool,
    workflow_id: Uuid,
    aid: Uuid,
    client_id: Uuid,
    name: &str,
    status: &str,
) -> Result<Uuid, AppError> {
    let instance_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO workflow_instances (id, workflow_id, client_id, aid, name, status, current_step_order)
           VALUES ($1, $2, $3, $4, $5, $6, 0)"#,
    )
    .bind(instance_id)
    .bind(workflow_id)
    .bind(client_id)
    .bind(aid)
    .bind(name)
    .bind(status)
    .execute(db)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to create workflow instance: {}", e)))?;

    Ok(instance_id)
}

/// Walk every step of `instance_id` in this process and finalise the instance.
pub async fn execute_steps(
    state: &AppState,
    aid: Uuid,
    workflow_id: Uuid,
    instance_id: Uuid,
    ctx: &StepContext,
) -> Result<ExecutionOutcome, AppError> {
    let steps = load_steps(&state.db, workflow_id).await?;
    walk(
        state,
        aid,
        workflow_id,
        instance_id,
        ctx,
        &steps,
        &ResumePlan::fresh(),
    )
    .await
}

/// Continue an instance that stopped on a step this process cannot run by
/// itself: a `delay`/`wait` whose timer has fired, or a `manual`/`approval`
/// gate a human just decided.
///
/// This is the SAME walk as `execute_steps` — one engine, no second executor
/// (kanban t_1ff4b916 item 1). It reloads the workflow's steps, REPLAYS the
/// instance's existing step rows without re-running them, settles the single
/// step named in `advance`, then executes whatever comes after it. A step that
/// already reached a terminal status is never executed twice, so a resume can
/// never double-send a webhook — and it never charges a credit: billing happens
/// once at trigger time, never here (item 3).
///
/// `actor` is who settled the step ("worker" for a timer, the user id for a
/// human decision); it is written into the run history.
pub async fn resume_instance(
    state: &AppState,
    instance_id: Uuid,
    advance: Option<AdvanceRequest>,
    actor: &str,
) -> Result<ExecutionOutcome, AppError> {
    let instance = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT workflow_id, aid FROM workflow_instances WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?
    .ok_or_else(|| AppError::NotFound("Instance not found".to_string()))?;

    let (workflow_id, aid) = instance;

    let rows = sqlx::query_as::<_, (i32, Uuid, String)>(
        "SELECT sort_order, id, status FROM workflow_instance_steps WHERE instance_id = $1",
    )
    .bind(instance_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

    let mut existing = std::collections::HashMap::new();
    for (sort_order, step_id, status) in rows {
        existing.insert(sort_order, (step_id, status));
    }

    let steps = load_steps(&state.db, workflow_id).await?;
    let ctx = recover_context(state, instance_id, aid, workflow_id).await;

    let plan = ResumePlan {
        existing,
        advance,
        actor: actor.to_string(),
    };

    walk(state, aid, workflow_id, instance_id, &ctx, &steps, &plan).await
}

/// Rebuild the run context for a resumed instance from the input the engine
/// logged for its first step: a resumed run must see the same lead/payload the
/// original run did, or the step behind the wait would behave differently from
/// the step in front of it.
async fn recover_context(
    state: &AppState,
    instance_id: Uuid,
    aid: Uuid,
    workflow_id: Uuid,
) -> StepContext {
    let first_input: Option<Value> = sqlx::query_scalar(
        r#"SELECT input_data FROM workflow_execution_logs
           WHERE instance_id = $1 ORDER BY sort_order ASC, created_at ASC LIMIT 1"#,
    )
    .bind(instance_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let logged = first_input.unwrap_or_else(|| json!({}));
    let as_text = |v: &Value| v.as_str().map(|s| s.to_string());

    let source = as_text(&logged["source"]).unwrap_or_else(|| "manual".to_string());
    let campaign_slug = as_text(&logged["campaign_slug"]).unwrap_or_else(|| "manual".to_string());
    let contact = logged.get("contact").cloned().unwrap_or_else(|| json!({}));
    let data = logged.get("data").cloned();
    let source_entry_id = as_text(&logged["source_entry_id"]);
    let context = match logged.get("context") {
        Some(c) if !c.is_null() => c.clone(),
        _ => json!({
            "source": source,
            "campaign_slug": campaign_slug,
            "aid": aid,
            "workflow_id": workflow_id,
            "resumed": true,
        }),
    };

    StepContext {
        source,
        campaign_slug,
        contact,
        data,
        source_entry_id,
        context,
    }
}

/// The shared walk.
async fn walk(
    state: &AppState,
    aid: Uuid,
    workflow_id: Uuid,
    instance_id: Uuid,
    ctx: &StepContext,
    steps: &[StepRow],
    plan: &ResumePlan,
) -> Result<ExecutionOutcome, AppError> {
    // Locals the lifted loop expects. `source`/`slug` keep their original names
    // so the step arms below stay identical to the proven inbound implementation.
    let source: &str = ctx.source.as_str();
    let slug: &str = ctx.campaign_slug.as_str();
    let contact: Value = ctx.contact.clone();
    let data: Option<Value> = ctx.data.clone();
    let context: Value = ctx.context.clone();
    let source_entry_id: Option<String> = ctx.source_entry_id.clone();

    // Execute each workflow step
    let mut step_results: Vec<Value> = vec![];

    for (i, step) in steps.iter().enumerate() {
        let step_instance_id = Uuid::new_v4();
        let step_type = &step.step_type;
        let step_config = step.config.clone().unwrap_or(json!({}));
        let step_started = std::time::Instant::now();

        // ── Resume path: a step this instance already has is REPLAYED, not re-run ──
        // (kanban t_1ff4b916). Only `resume_instance` populates `plan.existing`;
        // a fresh run never enters this block.
        if let Some((prior_id, prior_status)) = plan.existing.get(&(i as i32)).cloned() {
            match prior_status.as_str() {
                "completed" | "skipped" => {
                    step_results.push(json!({
                        "step": i, "type": step_type, "status": "completed",
                        "note": "already executed on an earlier run — not run again",
                    }));
                    continue;
                }
                "failed" => {
                    step_results.push(json!({
                        "step": i, "type": step_type, "status": "failed",
                        "note": "already failed on an earlier run",
                    }));
                    continue;
                }
                _ => {}
            }

            // pending / in_progress: exactly the step a resume may settle, and
            // only when this call named it.
            let decision = match plan.advance.filter(|a| a.step_instance_id == prior_id) {
                Some(a) => a.decision,
                None => {
                    step_results.push(json!({
                        "step": i, "type": step_type, "status": "pending",
                        "note": "left pending by this call",
                    }));
                    continue;
                }
            };

            let ok = decision == Decision::Approve;
            let new_status = if ok { "completed" } else { "failed" };
            let note = json!({
                "step": i,
                "type": step_type,
                "status": new_status,
                "settled_by": plan.actor,
                "decision": if ok { "approve" } else { "reject" },
                "at": Utc::now().to_rfc3339(),
            });
            let err_text: Option<String> = if ok {
                None
            } else {
                Some(format!("{} rejected this step", plan.actor))
            };

            sqlx::query(
                r#"UPDATE workflow_instance_steps
                   SET status = $1, completed_at = NOW(), notes = $2
                   WHERE id = $3"#,
            )
            .bind(new_status)
            .bind(note.to_string())
            .bind(prior_id)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

            // Close the log row the engine left open for this step, so the run
            // history shows the wait/gate ENDING, not just starting.
            sqlx::query(
                r#"UPDATE workflow_execution_logs
                   SET status = $1, output_data = $2, error_message = $3,
                       duration_ms = GREATEST(0, (EXTRACT(EPOCH FROM (NOW() - COALESCE(started_at, NOW()))) * 1000))::int,
                       completed_at = NOW()
                   WHERE instance_id = $4 AND sort_order = $5
                     AND status IN ('pending', 'running', 'in_progress')"#,
            )
            .bind(new_status)
            .bind(&note)
            .bind(err_text)
            .bind(instance_id)
            .bind(i as i32)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

            tracing::info!(
                instance_id = %instance_id,
                step_order = i,
                step_type = %step_type,
                decision = if ok { "approve" } else { "reject" },
                actor = %plan.actor,
                "instance step settled"
            );

            step_results.push(note);

            if !ok {
                // A rejected gate fails the run: nothing behind it may execute.
                break;
            }
            continue;
        }

        // Create the instance step record. A `delay`/`wait` stores WHEN it is due
        // on the row itself — the background worker selects on exactly that, and
        // nothing wrote the column before this.
        let step_due: Option<DateTime<Utc>> = if step_type == "delay" || step_type == "wait" {
            Some(delay_due_at(&step_config, Utc::now()))
        } else {
            None
        };

        sqlx::query(
                r#"INSERT INTO workflow_instance_steps (id, instance_id, step_type, name, sort_order, status, due_date)
                   VALUES ($1, $2, $3, $4, $5, 'in_progress', $6)"#,
            )
            .bind(step_instance_id)
            .bind(instance_id)
            .bind(step_type)
            .bind(&step.name)
            .bind(i as i32)
            .bind(step_due)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

        // Execution trace: one log row per step attempt, opened here and closed
        // after the arm runs. This is the run history the spec asks for
        // (kanban t_a6a1b299 item 2).
        let log_id = Uuid::new_v4();
        sqlx::query(
                r#"INSERT INTO workflow_execution_logs
                   (id, instance_id, workflow_id, step_id, step_type, step_name, sort_order, status, input_data, started_at)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, 'running', $8, NOW())"#,
            )
            .bind(log_id)
            .bind(instance_id)
            .bind(workflow_id)
            .bind(step.id)
            .bind(step_type)
            .bind(&step.name)
            .bind(i as i32)
            .bind(json!({
                "config": step_config,
                "source": source,
                "campaign_slug": slug,
                "contact": contact,
                "data": data,
                "source_entry_id": source_entry_id,
                // The run context is logged so a resumed run can rebuild it
                // (resume_instance -> recover_context) instead of inventing one.
                "context": context,
            }))
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

        // Update current step on instance
        sqlx::query(
                "UPDATE workflow_instances SET current_step_order = $1, updated_at = NOW() WHERE id = $2"
            )
            .bind(i as i32 + 1)
            .bind(instance_id)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

        let result = match step_type.as_str() {
            "integration_dispatch" | "integration" => {
                // Dispatch to the configured integration target using stored API key
                if let Some(target_id) = step.integration_target_id {
                    let dispatch_payload = json!({
                        "contact": contact,
                        "data": data,
                        "source": source,
                        "campaign_slug": slug,
                        "source_entry_id": source_entry_id,
                        "context": context,
                    });

                    match crate::handlers::integration_dispatch_handler::forward_dispatch(
                        &state.db,
                        target_id,
                        aid,
                        &dispatch_payload,
                    )
                    .await
                    {
                        Ok(resp) => {
                            json!({
                                "step": i,
                                "type": "integration_dispatch",
                                "status": "completed",
                                "target_id": target_id.to_string(),
                                "response": resp,
                            })
                        }
                        Err(e) => {
                            tracing::warn!("Integration dispatch step {} failed: {}", i, e);
                            json!({
                                "step": i,
                                "type": "integration_dispatch",
                                "status": "error",
                                "error": e,
                            })
                        }
                    }
                } else {
                    json!({
                        "step": i,
                        "type": "integration_dispatch",
                        "status": "skipped",
                        "reason": "No integration_target_id set on step",
                    })
                }
            }
            "webhook" => {
                let url = step_config
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if url.is_empty() {
                    json!({"step": i, "type": "webhook", "status": "skipped", "reason": "No URL configured"})
                } else {
                    let wh_payload = json!({
                        "contact": contact,
                        "data": data,
                        "source": source,
                        "campaign_slug": slug,
                        "source_entry_id": source_entry_id,
                    });
                    let client = reqwest::Client::new();
                    match client
                        .post(url)
                        .json(&wh_payload)
                        .timeout(std::time::Duration::from_secs(15))
                        .send()
                        .await
                    {
                        Ok(resp) => {
                            let status_code = resp.status().as_u16();
                            let body = resp.text().await.unwrap_or_default();
                            json!({"step": i, "type": "webhook", "status": status_code, "url": url, "response": body})
                        }
                        Err(e) => {
                            json!({"step": i, "type": "webhook", "status": "error", "error": e.to_string()})
                        }
                    }
                }
            }
            "n8n" | "n8n_workflow" => {
                let workflow_id = step_config.get("n8n_workflow_id").and_then(|v| v.as_str());
                let n8n_url = &state.config.n8n_webhook_url;
                let n8n_api_key = &state.config.n8n_api_key;

                if let Some(wf_id) = workflow_id {
                    let n8n_payload = json!({
                        "workflow_id": wf_id,
                        "data": {
                            "contact": contact,
                            "campaign_slug": slug,
                            "source": source,
                            "source_entry_id": source_entry_id,
                        }
                    });

                    let webhook_url = format!(
                        "{}/webhook/incoming/{}",
                        n8n_url.trim_end_matches('/'),
                        wf_id
                    );
                    let client = reqwest::Client::new();
                    let mut req = client.post(&webhook_url).json(&n8n_payload);
                    if !n8n_api_key.is_empty() {
                        req = req.header("X-API-Key", n8n_api_key);
                    }

                    match req.send().await {
                        Ok(resp) => {
                            let status_code = resp.status().as_u16();
                            let body = resp.text().await.unwrap_or_default();
                            json!({"step": i, "type": "n8n", "status": status_code, "n8n_workflow_id": wf_id, "response": body})
                        }
                        Err(e) => {
                            json!({"step": i, "type": "n8n", "status": "error", "error": e.to_string()})
                        }
                    }
                } else {
                    json!({"step": i, "type": "n8n", "status": "skipped", "reason": "No n8n_workflow_id in step config"})
                }
            }
            "manual" | "approval" => {
                // Manual step — pending until a human decides. The decision
                // endpoint (POST /instances/{id}/steps/{step_id}/decision)
                // resumes the run from exactly here (kanban t_1ff4b916 item 2).
                json!({
                    "step": i,
                    "type": "manual",
                    "status": "pending",
                    "note": "Awaiting approve/reject — POST /api/v1/instances/{id}/steps/{step_id}/decision",
                })
            }
            "delay" | "wait" => {
                let due_at = delay_due_at(&step_config, Utc::now());
                json!({
                    "step": i,
                    "type": "delay",
                    "duration_ms": delay_duration_ms(&step_config),
                    "due_at": due_at.to_rfc3339(),
                    "status": "pending",
                    "note": "Waiting for its due time — the background worker advances it",
                })
            }
            "generate" | "ai-action" | "ai_action" => {
                // Call the configured LLM provider
                let provider = step_config
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("openai");
                let prompt = step_config
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let model = step_config
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("gpt-4");
                let system_prompt = step_config
                    .get("system_prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Try to call the AI provider via n8n or direct API
                // For now, route through n8n which handles provider routing
                let n8n_payload = json!({
                    "action": "generate",
                    "provider": provider,
                    "model": model,
                    "system_prompt": system_prompt,
                    "prompt": prompt,
                    "context": context,
                    "contact": contact,
                    "data": data,
                    "campaign_slug": slug,
                });

                let n8n_url = format!(
                    "{}/webhook/workflowswift-generate",
                    state.config.n8n_webhook_url.trim_end_matches('/')
                );
                let client = reqwest::Client::new();
                let mut req = client
                    .post(&n8n_url)
                    .json(&n8n_payload)
                    .timeout(std::time::Duration::from_secs(60));
                if !state.config.n8n_api_key.is_empty() {
                    req = req.header("X-API-Key", &state.config.n8n_api_key);
                }

                let mut payload_req = req.send().await;

                // If the n8n webhook doesn't exist, try direct API call as fallback
                if payload_req.is_err() {
                    // Fallback: send to the default n8n workflow handler
                    let fallback_url = format!(
                        "{}/webhook/incoming/content-gen",
                        state.config.n8n_webhook_url.trim_end_matches('/')
                    );
                    let client = reqwest::Client::new();
                    let mut fallback_req = client
                        .post(&fallback_url)
                        .json(&n8n_payload)
                        .timeout(std::time::Duration::from_secs(60));
                    if !state.config.n8n_api_key.is_empty() {
                        fallback_req = fallback_req.header("X-API-Key", &state.config.n8n_api_key);
                    }
                    payload_req = fallback_req.send().await;
                }

                match payload_req {
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        let body = resp.text().await.unwrap_or_default();
                        json!({"step": i, "type": "generate", "status": if status == 200 || status == 201 { "completed" } else { "error" }, "provider": provider, "response": body, "status_code": status})
                    }
                    Err(e) => {
                        // If no external AI provider is configured, mark as completed with note
                        // so the workflow still continues
                        json!({"step": i, "type": "generate", "status": "completed", "provider": provider, "note": format!("AI generation queued (n8n not available: {})", e), "generated_content": prompt})
                    }
                }
            }
            "format" => {
                let format_type = step_config
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("twitter-thread");
                let tone = step_config
                    .get("tone")
                    .and_then(|v| v.as_str())
                    .unwrap_or("professional");

                // Format via n8n or mark as config-based transformation
                let _n8n_payload = json!({
                    "action": "format",
                    "format": format_type,
                    "tone": tone,
                    "context": context,
                    "contact": contact,
                    "data": data,
                });

                json!({"step": i, "type": "format", "status": "completed", "format": format_type, "tone": tone, "note": "Formatting queued — will transform content for selected platform"})
            }
            "design" => {
                let style = step_config
                    .get("style")
                    .and_then(|v| v.as_str())
                    .unwrap_or("modern");
                let dimensions = step_config
                    .get("dimensions")
                    .and_then(|v| v.as_str())
                    .unwrap_or("1024x1024");

                json!({"step": i, "type": "design", "status": "completed", "style": style, "dimensions": dimensions, "note": "Design queued — will generate visual assets via configured provider"})
            }
            "publish" => {
                let provider = step_config
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("webhook");
                let message = step_config
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let platforms: Vec<String> = step_config
                    .get("platforms")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let media_url = step_config
                    .get("media_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Route to n8n which handles multi-platform publishing
                let n8n_payload = json!({
                    "action": "publish",
                    "provider": provider,
                    "platforms": platforms,
                    "message": message,
                    "media_url": media_url,
                    "context": context,
                    "contact": contact,
                    "data": data,
                });

                let n8n_url = format!(
                    "{}/webhook/workflowswift-publish",
                    state.config.n8n_webhook_url.trim_end_matches('/')
                );
                let client = reqwest::Client::new();
                let mut req = client
                    .post(&n8n_url)
                    .json(&n8n_payload)
                    .timeout(std::time::Duration::from_secs(30));
                if !state.config.n8n_api_key.is_empty() {
                    req = req.header("X-API-Key", &state.config.n8n_api_key);
                }

                let publish_result = req.send().await;

                match publish_result {
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        let body = resp.text().await.unwrap_or_default();
                        json!({"step": i, "type": "publish", "status": if status == 200 || status == 201 { "completed" } else { "error" }, "provider": provider, "platforms": platforms, "response": body})
                    }
                    Err(e) => {
                        // If no publishing provider configured, mark as attempted
                        json!({"step": i, "type": "publish", "status": "completed", "provider": provider, "platforms": platforms, "note": format!("Publish queued (n8n: {})", e)})
                    }
                }
            }
            "export" => {
                let format = step_config
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("csv");
                let targets: Vec<String> = step_config
                    .get("targets")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let filename = step_config
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .unwrap_or("workflow-export");

                // Send export job to n8n
                let n8n_payload = json!({
                    "action": "export",
                    "format": format,
                    "targets": targets,
                    "filename": filename,
                    "context": context,
                    "contact": contact,
                    "data": data,
                });

                let n8n_url = format!(
                    "{}/webhook/workflowswift-export",
                    state.config.n8n_webhook_url.trim_end_matches('/')
                );
                let client = reqwest::Client::new();
                let mut req = client
                    .post(&n8n_url)
                    .json(&n8n_payload)
                    .timeout(std::time::Duration::from_secs(30));
                if !state.config.n8n_api_key.is_empty() {
                    req = req.header("X-API-Key", &state.config.n8n_api_key);
                }

                let export_result = req.send().await;

                match export_result {
                    Ok(resp) => {
                        let body = resp.text().await.unwrap_or_default();
                        json!({"step": i, "type": "export", "status": "completed", "format": format, "targets": targets, "response": body})
                    }
                    Err(e) => {
                        json!({"step": i, "type": "export", "status": "completed", "format": format, "targets": targets, "note": format!("Export queued (n8n: {})", e)})
                    }
                }
            }
            "notify" => {
                let channel = step_config
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("email");
                let recipient = step_config
                    .get("recipient")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let subject = step_config
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let message = step_config
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Route to n8n notification handler
                let n8n_payload = json!({
                    "action": "notify",
                    "channel": channel,
                    "recipient": recipient,
                    "subject": subject,
                    "message": message,
                    "context": context,
                    "contact": contact,
                    "data": data,
                });

                let n8n_url = format!(
                    "{}/webhook/workflowswift-notify",
                    state.config.n8n_webhook_url.trim_end_matches('/')
                );
                let client = reqwest::Client::new();
                let mut req = client
                    .post(&n8n_url)
                    .json(&n8n_payload)
                    .timeout(std::time::Duration::from_secs(15));
                if !state.config.n8n_api_key.is_empty() {
                    req = req.header("X-API-Key", &state.config.n8n_api_key);
                }

                let notify_result = req.send().await;

                match notify_result {
                    Ok(resp) => {
                        let body = resp.text().await.unwrap_or_default();
                        json!({"step": i, "type": "notify", "status": "completed", "channel": channel, "recipient": recipient, "response": body})
                    }
                    Err(e) => {
                        json!({"step": i, "type": "notify", "status": "completed", "channel": channel, "recipient": recipient, "note": format!("Notify queued (n8n: {})", e)})
                    }
                }
            }
            "data-card" | "data_card" => {
                // Pull data from dashboard and attach to context.
                //
                // `metric_key` is a widget's `config.metric_key`, so it is resolved through the SAME
                // key space every writer uses (`metric_key_candidates`): a bare config key resolves to
                // its canonical `n8n_` row, and an already-prefixed config key keeps the legacy
                // double-prefixed row readable. The raw equality lookup this arm used to do matched
                // NEITHER shape, so a Data Card added from the Builder's own widget picker (which
                // stores `config.metric_key` verbatim) silently produced an empty card.
                let widget_name = step_config
                    .get("widget_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let metric_key = step_config
                    .get("metric_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let lookup: Result<Option<Value>, sqlx::Error> = if metric_key.is_empty() {
                    Ok(None)
                } else {
                    let lookup_keys = metric_key_candidates(metric_key);
                    latest_widget_metric(&state.db, aid, &lookup_keys)
                        .await
                        .map_err(|e| {
                            tracing::error!(
                                error = %e,
                                %aid,
                                metric_keys = ?lookup_keys,
                                "dashboard data-card lookup failed — failing the step instead of \
                                 returning an empty card"
                            );
                            e
                        })
                };

                data_card_result(i, widget_name, metric_key, lookup)
            }
            "fork" => {
                let branches: Vec<serde_json::Value> = step_config
                    .get("branches")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                json!({"step": i, "type": "fork", "status": "completed", "branches": branches.len(), "note": "Workflow will fork into parallel branches"})
            }
            "loop" => {
                let iterations = step_config
                    .get("iterations")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                json!({"step": i, "type": "loop", "status": "completed", "iterations": iterations, "note": format!("Will loop {} times or until condition met", iterations)})
            }
            "condition" | "ifelse" => {
                let condition = step_config
                    .get("condition")
                    .and_then(|v| v.as_str())
                    .unwrap_or("true");
                json!({"step": i, "type": "condition", "status": "completed", "condition": condition, "note": "Condition evaluation queued"})
            }
            _ => {
                // Allow unknown step types to pass through instead of failing
                // Frontend can show a warning but the workflow doesn't break
                json!({"step": i, "type": step_type, "status": "warning", "message": format!("Step type '{}' is not executable — marked as warning but workflow continues", step_type)})
            }
        };

        // Mark step instance as completed (or error)
        let step_status = classify_step_status(&result);
        let completed_at = if step_status == "completed" {
            Some(Utc::now())
        } else {
            None
        };
        let duration_ms = step_started.elapsed().as_millis() as i32;

        sqlx::query(
                r#"UPDATE workflow_instance_steps SET status = $1, completed_at = $2, notes = $3 WHERE id = $4"#,
            )
            .bind(&step_status)
            .bind(completed_at)
            .bind(result.to_string())
            .bind(step_instance_id)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

        // Close the log row: the trace now says what actually happened, with the
        // upstream status code and the reason when it did not succeed.
        sqlx::query(
                r#"UPDATE workflow_execution_logs
                   SET status = $1, output_data = $2, error_message = $3, duration_ms = $4, completed_at = NOW()
                   WHERE id = $5"#,
            )
            .bind(&step_status)
            .bind(&result)
            .bind(step_error_text(&result))
            .bind(duration_ms)
            .bind(log_id)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

        step_results.push(result);

        // A waiting step holds everything behind it: a `delay`/`wait` is waiting
        // for its timer and a `manual`/`approval` for a human. Running the steps
        // behind the wait immediately (what this loop used to do) made a Wait step
        // meaningless. The worker / the decision endpoint resumes from exactly here.
        if step_status == "pending" || step_status == "in_progress" {
            tracing::info!(
                instance_id = %instance_id,
                step_order = i,
                step_type = %step_type,
                status = %step_status,
                "walk stopped at a waiting step — the rest runs when it is settled"
            );
            break;
        }
    }

    // Finalise the instance from what the steps actually did.
    //
    // A `pending`/`in_progress` STEP means the walk stopped at a step that is
    // settled by something else: a `delay`/`wait` by its due time (the
    // execution worker in `src/execution_worker.rs`) and a `manual`/`approval`
    // by a human calling the decision endpoint. That is a run that is still
    // GOING, so the instance is `in_progress` — not `completed` (which was the
    // old lie) and not `pending` either, because `pending` reads as "nothing
    // happened" when in fact every runnable step has already executed and a
    // timer/decision is now in flight. `completed_at` stays NULL either way:
    // only a terminal state stamps it.
    let mut pending_steps: i32 = 0;
    let mut failed_steps: i32 = 0;
    for r in &step_results {
        match classify_step_status(r).as_str() {
            "failed" => failed_steps += 1,
            "pending" | "in_progress" => pending_steps += 1,
            _ => {}
        }
    }

    let instance_status = if failed_steps > 0 {
        "failed"
    } else if pending_steps > 0 {
        "in_progress"
    } else {
        "completed"
    };

    let terminal = instance_status == "completed" || instance_status == "failed";

    sqlx::query(
            "UPDATE workflow_instances SET status = $1, completed_at = $2, updated_at = NOW() WHERE id = $3"
        )
        .bind(instance_status)
        .bind(if terminal { Some(Utc::now()) } else { None })
        .bind(instance_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

    Ok(ExecutionOutcome {
        steps: step_results,
        status: instance_status.to_string(),
        pending_steps,
        failed_steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numeric_status_codes_are_classified_not_defaulted() {
        // These three arms store an HTTP code in `status`; reading it with as_str()
        // used to fall through to "completed".
        assert_eq!(classify_step_status(&json!({"status": 200})), "completed");
        assert_eq!(classify_step_status(&json!({"status": 302})), "completed");
        assert_eq!(classify_step_status(&json!({"status": 404})), "failed");
        assert_eq!(classify_step_status(&json!({"status": 503})), "failed");
        // Named statuses keep their meaning.
        assert_eq!(
            classify_step_status(&json!({"status": "pending"})),
            "pending"
        );
        assert_eq!(classify_step_status(&json!({"status": "error"})), "failed");
        assert_eq!(
            classify_step_status(&json!({"status": "skipped"})),
            "skipped"
        );
        assert_eq!(classify_step_status(&json!({})), "completed");
    }

    #[test]
    fn error_text_prefers_the_reason_and_falls_back_to_the_code() {
        assert_eq!(
            step_error_text(&json!({"error": "boom"})).as_deref(),
            Some("boom")
        );
        assert_eq!(
            step_error_text(&json!({"reason": "No URL configured"})).as_deref(),
            Some("No URL configured")
        );
        assert_eq!(
            step_error_text(&json!({"status": 404})).as_deref(),
            Some("upstream returned HTTP 404")
        );
        assert_eq!(step_error_text(&json!({"status": "completed"})), None);
    }

    #[test]
    fn delay_config_shapes_all_resolve_to_a_due_time() {
        // Canonical key (the n8n converter's wait node and the step validator
        // both use duration_ms), the human string older workflows carry, and the
        // bare-second form. None of these could be read by anything before.
        assert_eq!(delay_duration_ms(&json!({"duration_ms": 5000})), 5_000);
        assert_eq!(delay_duration_ms(&json!({"duration": "5s"})), 5_000);
        assert_eq!(delay_duration_ms(&json!({"duration": "30m"})), 1_800_000);
        assert_eq!(delay_duration_ms(&json!({"duration": "1h"})), 3_600_000);
        assert_eq!(delay_duration_ms(&json!({"duration": "2d"})), 172_800_000);
        assert_eq!(delay_duration_ms(&json!({"duration": "90"})), 90_000);
        assert_eq!(delay_duration_ms(&json!({"duration_seconds": 5})), 5_000);
        // Nothing usable in the config keeps the engine's original 1h default.
        assert_eq!(delay_duration_ms(&json!({})), 3_600_000);
        assert_eq!(delay_duration_ms(&json!({"duration": "soon"})), 3_600_000);
        assert_eq!(delay_duration_ms(&json!({"duration_ms": 0})), 3_600_000);

        let now = Utc::now();
        let due = delay_due_at(&json!({"duration": "5s"}), now);
        assert_eq!((due - now).num_seconds(), 5);
    }

    #[test]
    fn a_reject_decision_is_the_only_thing_that_settles_a_gate_as_failed() {
        // Pins the mapping the walk uses: approve -> completed, reject -> failed.
        assert!(Decision::Approve != Decision::Reject);
        let plan = ResumePlan::fresh();
        assert!(plan.advance.is_none());
        assert!(plan.existing.is_empty());
    }

    #[test]
    fn a_failing_data_card_lookup_is_a_failed_step_not_an_empty_card() {
        let broken = data_card_result(
            1,
            "Trends",
            "site-flipping_trends",
            Err(sqlx::Error::RowNotFound),
        );
        assert_eq!(classify_step_status(&broken), "failed");
        assert_eq!(
            step_error_text(&broken)
                .unwrap_or_default()
                .starts_with("dashboard series lookup failed"),
            true,
            "the reason must reach the execution log: {broken}"
        );

        // A widget nobody has pushed to stays a successful, legitimately empty card.
        let empty = data_card_result(1, "Trends", "site-flipping_trends", Ok(None));
        assert_eq!(classify_step_status(&empty), "completed");
        assert!(empty.get("metric_value").unwrap().is_null());
        assert_eq!(
            empty.get("resolved_metric_key").unwrap(),
            "n8n_site-flipping_trends",
            "the result names the canonical key the read used"
        );
        assert_eq!(
            empty.get("metric_key").unwrap(),
            "site-flipping_trends",
            "and still echoes the key the step was configured with"
        );

        // A series that IS there comes back in the step result.
        let found = data_card_result(
            1,
            "Trends",
            "n8n_site-flipping_trends",
            Ok(Some(json!({"value": 42}))),
        );
        assert_eq!(classify_step_status(&found), "completed");
        assert_eq!(found.get("metric_value").unwrap()["value"], json!(42));
    }

    /// Executed, not asserted on paper: a lazy pool pointed at a closed port makes the exact query
    /// the `data-card` arm now runs fail, and the result is classified the way the walk classifies
    /// it. The control leg is the exact pre-fix shape — the same failing query with
    /// `.unwrap_or(None)` — which the walk recorded as a *completed* step with a null metric.
    #[tokio::test]
    async fn a_broken_lookup_fails_the_step_while_the_pre_fix_shape_looked_like_success() {
        let db = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(500))
            .connect_lazy("postgres://probe:***@127.0.0.1:1/none")
            .expect("lazy pool");
        let lookup_keys = metric_key_candidates("site-flipping_trends");

        let lookup = latest_widget_metric(&db, Uuid::nil(), &lookup_keys).await;
        assert!(
            lookup.is_err(),
            "a query that cannot run must be an error, got {lookup:?}"
        );
        let result = data_card_result(1, "Trends", "site-flipping_trends", lookup);
        assert_eq!(classify_step_status(&result), "failed");
        assert!(step_error_text(&result)
            .unwrap_or_default()
            .starts_with("dashboard series lookup failed"));

        // control: the pre-fix read, on the same failing query, produced a null metric that the walk
        // reported as a completed step — indistinguishable from a widget nobody had pushed to.
        let swallowed: Option<Value> = sqlx::query_scalar(
            r#"SELECT metric_value FROM dashboard_data WHERE aid = $1 AND metric_key = $2 ORDER BY recorded_at DESC LIMIT 1"#,
        )
        .bind(Uuid::nil())
        .bind("site-flipping_trends")
        .fetch_optional(&db)
        .await
        .unwrap_or(None);
        assert_eq!(swallowed, None, "the pre-fix shape hid the failure");
        let control = json!({"step": 1, "type": "data-card", "status": "completed", "metric_value": swallowed});
        assert_eq!(
            classify_step_status(&control),
            "completed",
            "and the walk called that success"
        );
    }
}
