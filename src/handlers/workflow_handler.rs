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
            "SELECT * FROM workflows WHERE aid = $1 AND is_active = true AND (surface_id = $2 OR surface_id IS NULL) ORDER BY name ASC",
        )
        .bind(aid)
        .bind(surface_id)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, Workflow>(
            "SELECT * FROM workflows WHERE aid = $1 AND is_active = true ORDER BY name ASC",
        )
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

    let workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND aid = $2 AND is_active = true",
    )
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

    let existing = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND aid = $2 AND is_active = true",
    )
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

    // A waiting step is normal progress, not a warning. The response already
    // carries `status: "in_progress"` plus `pending_steps`, and each waiting
    // step carries its own `due_date` / `status`, so a yellow "1 step(s) are
    // still waiting" banner on every workflow that contains a Wait step is
    // pure noise — it fires on the healthy path and trains the user to ignore
    // the warnings array, which is where the REAL problems (n8n unreachable,
    // failed steps) have to be visible.
    if outcome.failed_steps > 0 {
        warnings.push(format!(
            "{} step(s) failed — the instance is reported as 'failed'. Per-step detail is in GET /api/v1/instances/{{id}}/logs.",
            outcome.failed_steps
        ));
    }

    // A step the engine could not execute is not a clean run. `execution.rs`'s `_` arm marks such
    // a step `unexecutable` (reachable only for a row written straight into the database — the
    // steps API refuses these types now), and it is surfaced here because a run that reports
    // success around a step that never happened is exactly the defect this card is about.
    let mut unexecutable: Vec<String> = outcome
        .steps
        .iter()
        .filter_map(|s| {
            s.get("unexecutable")
                .and_then(|v| v.as_str())
                .map(|t| t.to_string())
        })
        .collect();
    unexecutable.sort();
    unexecutable.dedup();
    if !unexecutable.is_empty() {
        warnings.push(format!(
            "{} step type(s) have no executor in this app and did nothing: {}. They cannot be added from the console — delete the step and re-add it as a supported type.",
            unexecutable.len(),
            unexecutable.join(", ")
        ));
    }

    // A Notify step that could not send anything (a channel with no sender in this app, a blank
    // Webhook URL — kanban t_e0e6a42e) is `skipped` with an `undeliverable` reason. It is not a
    // failed step, but the run must not read as a clean success around a notification that went
    // nowhere, so the reason is surfaced here.
    let mut undeliverable: Vec<String> = outcome
        .steps
        .iter()
        .filter_map(|s| {
            s.get("undeliverable")
                .and_then(|v| v.as_str())
                .map(|t| t.to_string())
        })
        .collect();
    undeliverable.sort();
    undeliverable.dedup();
    if !undeliverable.is_empty() {
        warnings.push(format!(
            "{} step(s) sent nothing: {}. Fix the step's configuration and re-run.",
            undeliverable.len(),
            undeliverable.join("; ")
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

/// Refuse to hand n8n a destination this app would refuse itself.
///
/// The mirrored graph is executed by n8n — a separate container — so the destination gate the
/// in-process arms call (`security::webhook_security::gate_step_destination`) does NOT run on it
/// (kanban t_2741ac13; both legs measured in `/opt/swift/audits/t_2741ac13/`). Every URL the graph
/// would call that is not one of this app's own callbacks is the tenant's to choose, so it goes
/// through the SAME gate, on the SAME vocabulary, before the graph is handed to n8n. There is no
/// second blocklist here on purpose.
///
/// A refusal fails the mirror: nothing is written to n8n and the run reports why (the caller turns
/// the Err into the `n8n.error` block plus a warning). Writing the graph anyway would put a node
/// whose destination this app refuses into a second executor — the exact read primitive the engine
/// gate exists to close — and writing it with that node REMOVED would make the external run
/// silently do less than the tenant's workflow.
///
/// Fail closed on a destination the app cannot READ as a literal: a URL carrying an n8n expression
/// (`={{ … }}`) is resolved by n8n at run time, so the app cannot gate what it cannot see.
async fn gate_mirror_destinations(
    n8n_wf: &n8n_converter::N8nWorkflow,
    callback_base_url: &str,
) -> Result<(), String> {
    for (node, url) in n8n_converter::tenant_destinations(n8n_wf, callback_base_url) {
        let shown: String = url.chars().take(120).collect();
        if url.trim().is_empty() {
            return Err(format!(
                "step '{}' has no destination URL, so there is nothing to gate — set the step's URL and deploy again",
                node
            ));
        }
        if url.contains("{{") {
            return Err(format!(
                "step '{}' has a destination this app cannot read ('{}'): n8n resolves it at run time, so the app cannot check it — use a literal URL",
                node, shown
            ));
        }
        crate::security::webhook_security::gate_step_destination(&url)
            .await
            .map_err(|e| format!("step '{}' destination refused: {}", node, e))?;
    }
    Ok(())
}

/// Mirror a workflow into n8n over its REST API with an API key.
/// Called only when the plan enables `n8n_deploy`; the caller turns any Err
/// into a warning. Never uses docker: this container has no docker binary and
/// no docker socket.
///
/// IDEMPOTENT per WorkflowSwift workflow: n8n has no natural key for an import,
/// so the app keys its copy there by name (`WFS <workflow_id>`) and a later
/// mirror UPDATES that copy (PUT) instead of importing another one — a workflow
/// that is run and started keeps exactly ONE n8n copy. The id n8n assigned is
/// recorded in `workflows.lifecycle_summary.n8n_workflow_id`, so the common
/// path needs no lookup at all.
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
    let n8n_wf = n8n_converter::convert_steps_to_n8n(
        &step_values,
        aid,
        workflow.id,
        &callback_base_url,
        state.config.internal_sync_key.as_str(),
    );
    // The graph n8n will run is NOT the graph this process runs: gate its destinations before it
    // leaves the box (kanban t_2741ac13). A refusal here fails the mirror and is reported.
    gate_mirror_destinations(&n8n_wf, &callback_base_url).await?;
    let n8n_json = n8n_converter::to_n8n_json(&n8n_wf);

    // n8n's public REST API (the one an API key works against). The mirror is an
    // UPSERT: n8n has no natural key here, so without this every call imported a
    // NEW copy of the same workflow (kanban t_2cdf44c1).
    let base = state.config.n8n_url.trim_end_matches('/').to_string();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client: {}", e))?;

    // Which copy in n8n belongs to this WorkflowSwift workflow? An id recorded by
    // a previous mirror costs no call; only a first mirror (or one n8n has since
    // forgotten) pays for a lookup by name. A failed lookup never fails the
    // mirror — it falls back to importing, which is what the old code always did.
    let mut n8n_id = recorded_n8n_workflow_id(workflow.lifecycle_summary.as_deref());
    let mut duplicates: Vec<String> = Vec::new();
    let mut lookup_warning: Option<String> = None;
    if n8n_id.is_none() {
        match find_n8n_mirror(&client, &base, api_key, &n8n_wf.name).await {
            Ok((found, older)) => {
                n8n_id = found;
                duplicates = older;
            }
            Err(e) => lookup_warning = Some(e),
        }
    }

    let (mut status, mut body) =
        send_mirror(&client, &base, api_key, &n8n_json, n8n_id.as_deref()).await?;
    if status == reqwest::StatusCode::NOT_FOUND && n8n_id.is_some() {
        // The recorded id is stale — the copy was deleted in n8n. Import it again
        // rather than reporting a mirror that was never written.
        n8n_id = None;
        duplicates.clear();
        let retry = send_mirror(&client, &base, api_key, &n8n_json, None).await?;
        status = retry.0;
        body = retry.1;
    }
    if !status.is_success() {
        let body: String = body.chars().take(200).collect();
        return Err(format!("n8n rejected the import ({}) {}", status, body));
    }

    let created = n8n_id.is_none();
    let returned: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(|_| json!({}));
    let mirror_id = returned
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| n8n_id.clone())
        .ok_or_else(|| "n8n accepted the import but returned no workflow id".to_string())?;

    // Collapse older copies of THIS workflow, left behind by mirrors from before
    // the upsert. Exact-name matches only — no other workflow is ever touched.
    let mut collapsed = 0usize;
    for dupe in duplicates
        .iter()
        .filter(|d| d.as_str() != mirror_id.as_str())
    {
        if let Ok(r) = client
            .delete(format!("{}/api/v1/workflows/{}", base, dupe))
            .header("X-N8N-API-KEY", api_key)
            .send()
            .await
        {
            if r.status().is_success() {
                collapsed += 1;
            }
        }
    }

    // Merge, never clobber: `lifecycle_summary` is also a user-writable column.
    let mut summary = lifecycle_summary_object(workflow.lifecycle_summary.as_deref());
    summary.insert("n8n_deployed".to_string(), json!(true));
    summary.insert("n8n_webhook_path".to_string(), json!(n8n_wf.webhook_path));
    summary.insert("n8n_workflow_id".to_string(), json!(mirror_id));
    summary.insert(
        "deployed_at".to_string(),
        json!(chrono::Utc::now().to_rfc3339()),
    );
    let _ = sqlx::query("UPDATE workflows SET lifecycle_summary = $1 WHERE id = $2")
        .bind(serde_json::Value::Object(summary).to_string())
        .bind(workflow.id)
        .execute(&state.db)
        .await;

    let mut n8n = json!({
        "requested": true,
        "deployed": true,
        // The steps already ran in-process, so re-triggering n8n here would
        // execute the same workflow twice (duplicate emails/API calls).
        "triggered": false,
        "trigger_note": "steps were executed in-process; the n8n copy is available for external triggers (POST)",
        "n8n_workflow_id": mirror_id,
        "action": if created { "created" } else { "updated" },
        "webhook_path": n8n_wf.webhook_path,
        // The method is part of the contract the tenant is handed (kanban t_d4dd6e42): the
        // Webhook node's own default was GET, and a path with no verb named is what let a
        // GET-only registration ship unnoticed.
        "webhook_method": crate::n8n_converter::MIRROR_TRIGGER_METHOD,
        "name": n8n_wf.name,
        "node_count": n8n_wf.nodes.len(),
    });
    if let Some(o) = n8n.as_object_mut() {
        if collapsed > 0 {
            o.insert("duplicates_collapsed".to_string(), json!(collapsed));
        }
        if let Some(e) = lookup_warning {
            o.insert("lookup_warning".to_string(), json!(e));
        }
    }
    Ok(n8n)
}

/// The n8n workflow id recorded by a previous mirror, if the summary holds one.
fn recorded_n8n_workflow_id(lifecycle_summary: Option<&str>) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(lifecycle_summary?).ok()?;
    parsed
        .get("n8n_workflow_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Start from what the column already holds so the mirror only ADDS keys. A
/// non-JSON summary used to be destroyed outright by the mirror; keep it.
fn lifecycle_summary_object(existing: Option<&str>) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    match existing {
        None => {}
        Some(s) => match serde_json::from_str::<serde_json::Value>(s) {
            Ok(serde_json::Value::Object(m)) => return m,
            Ok(other) => {
                out.insert("legacy_lifecycle_summary".to_string(), other);
            }
            Err(_) if s.trim().is_empty() => {}
            Err(_) => {
                out.insert("legacy_lifecycle_summary".to_string(), json!(s));
            }
        },
    }
    out
}

/// Send the converted workflow to n8n: PUT onto the copy the app owns, POST when
/// there is nothing to update yet. Returns (status, raw body).
async fn send_mirror(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    payload: &serde_json::Value,
    target: Option<&str>,
) -> Result<(reqwest::StatusCode, String), String> {
    let (method, url) = match target {
        Some(id) => (
            reqwest::Method::PUT,
            format!("{}/api/v1/workflows/{}", base, id),
        ),
        None => (reqwest::Method::POST, format!("{}/api/v1/workflows", base)),
    };
    let resp = client
        .request(method, &url)
        .header("X-N8N-API-KEY", api_key)
        .json(payload)
        .send()
        .await
        .map_err(|e| format!("n8n request failed: {}", e))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Ok((status, body))
}

/// Every n8n workflow carrying `name` (the app's key there is `WFS <workflow_id>`).
/// Returns the NEWEST copy plus any older duplicates, so the caller can update one
/// copy and collapse the rest. Names are compared exactly: the filter is a
/// convenience, not the decision.
async fn find_n8n_mirror(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    name: &str,
) -> Result<(Option<String>, Vec<String>), String> {
    let resp = client
        .get(format!("{}/api/v1/workflows", base))
        .query(&[("limit", "250"), ("name", name)])
        .header("X-N8N-API-KEY", api_key)
        .send()
        .await
        .map_err(|e| format!("n8n lookup failed: {}", e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("n8n lookup rejected ({})", status));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("n8n lookup body: {}", e))?;
    let mut rows: Vec<(String, String)> = body
        .get("data")
        .and_then(|d| d.as_array())
        .map(|rows| {
            rows.iter()
                .filter(|r| r.get("name").and_then(|n| n.as_str()) == Some(name))
                .filter_map(|r| {
                    let id = r.get("id")?.as_str()?.to_string();
                    let created = r
                        .get("createdAt")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some((id, created))
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| b.1.cmp(&a.1)); // newest first
    if rows.is_empty() {
        return Ok((None, Vec::new()));
    }
    let (newest, _) = rows.remove(0);
    Ok((Some(newest), rows.into_iter().map(|(id, _)| id).collect()))
}

/// The name WorkflowSwift gives every n8n copy of `workflow_id`.
///
/// `src/n8n_converter.rs` builds the same string (`name: format!("WFS {}", workflow_id)`), so a
/// mirror carrying this name is THIS row's copy and nobody else's.
pub fn mirror_name(workflow_id: Uuid) -> String {
    format!("WFS {}", workflow_id)
}

/// Retire (delete) the n8n mirror of every id in `ids`, using an already-built client.
///
/// DB-free on purpose: this is the one piece of the "a hard delete must not leave the mirror
/// behind" rule that can be tested against a stub n8n.
///
/// * `Ok(n)` — n mirrors were retired. A mirror that is already absent in n8n counts, because the
///   caller's question is "does a `WFS <id>` copy still exist?", and the answer is no either way.
/// * `Err(_)` — at least one mirror could NOT be retired (n8n unreachable, refused the delete, or
///   the lookup itself failed). The caller MUST NOT delete the rows then: doing exactly that is
///   what leaves a `WFS <uuid>` orphan behind (kanban t_a965cf32).
pub async fn retire_mirrors_with(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    ids: &[Uuid],
) -> Result<usize, String> {
    let mut retired = 0usize;
    for id in ids {
        let name = mirror_name(*id);
        let (newest, older) = find_n8n_mirror(client, base, api_key, &name).await?;
        for mirror in newest.into_iter().chain(older) {
            let resp = client
                .delete(format!("{}/api/v1/workflows/{}", base, mirror))
                .header("X-N8N-API-KEY", api_key)
                .send()
                .await
                .map_err(|e| format!("n8n delete of mirror {} failed: {}", mirror, e))?;
            let status = resp.status();
            if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
                retired += 1;
            } else {
                return Err(format!(
                    "n8n refused deleting mirror {} ({})",
                    mirror, status
                ));
            }
        }
    }
    Ok(retired)
}

/// Retire every `WFS <uuid>` mirror belonging to `aid`'s workflows, BEFORE those rows are
/// hard-deleted by their owner (`ON DELETE CASCADE` from `accounts`).
///
/// The app's own delete path soft-deletes (`delete_workflow` sets `is_active = false`), so a
/// `workflows` row normally keeps its source and its mirror is never orphaned. The admin account
/// wipe was the one PRODUCT path that hard-deleted the rows: `workflows.aid -> accounts(id)` is
/// `ON DELETE CASCADE`, so `DELETE FROM accounts` took every `workflows` row with it and left each
/// mirror in n8n with nothing to point at (kanban t_a965cf32).
///
/// Fails closed: if the account still has workflow rows and their mirrors cannot be retired, the
/// caller must refuse the delete rather than perform it.
pub async fn retire_account_n8n_mirrors(state: &AppState, aid: Uuid) -> Result<usize, String> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM workflows WHERE aid = $1")
        .bind(aid)
        .fetch_all(&state.db)
        .await
        .map_err(|e| format!("reading the account's workflows failed: {}", e))?;
    if ids.is_empty() {
        return Ok(0);
    }
    let api_key = state.config.n8n_api_key.trim().to_string();
    if api_key.is_empty() {
        return Err(format!(
            "{} workflow row(s) would be deleted but no n8n API key is provisioned, so their `WFS <id>` mirrors cannot be retired",
            ids.len()
        ));
    }
    let base = state.config.n8n_url.trim_end_matches('/').to_string();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client: {}", e))?;
    retire_mirrors_with(&client, &base, &api_key, &ids).await
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

    let workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND aid = $2 AND is_active = true",
    )
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
/// The one Data-Card vocabulary predicate. `pub(crate)` because the template path asks the SAME
/// question of a template's own first step — a template is installed as a workflow, so the two
/// doors must not disagree about which types are a Data Card (kanban t_96e77263).
pub(crate) fn is_data_card(step_type: &str) -> bool {
    matches!(step_type, "data-card" | "data_card")
}

fn data_card_first_error() -> AppError {
    AppError::BadRequest(
        "Step 1 of a workflow must be a Data Card ('data-card') — it is what pulls the run's data. \
         Add the Data Card first, then add this step after it."
            .to_string(),
    )
}

/// A Notify step's `channel` is vocabulary, not a free string — `execution::NOTIFY_CHANNELS` is the
/// one list, the console's Channel select offers exactly it, and the API refuses anything else.
///
/// `email` and `sms` were channels a tenant could pick that deliver nothing (kanban t_08be842f):
/// no tenant-triggered mail sender exists in this app (its mail path is template-based and not
/// reachable from n8n) and no SMS provider exists at all. A channel the step cannot deliver on is
/// the same defect class as a step type the engine cannot execute (kanban t_fe60cdf5), one level
/// down, so it is refused on the way in and reported by validate-steps.
fn notify_channel_error(channel: &str) -> AppError {
    AppError::Validation(format!(
        "Notify channel '{}' has no sender in this app. Valid channels are: {}",
        channel,
        crate::execution::notify_channel_list()
    ))
}

/// Refuse a notify step whose `config.channel` this product cannot deliver on. Every other step
/// type passes through untouched.
///
/// `pub(crate)` because the template write paths enforce the SAME rule: a template step is copied
/// verbatim into `workflow_steps` by `POST /templates/{id}/install`, so a channel the steps API
/// refuses cannot be allowed to arrive through a template (kanban t_27a15474).
pub(crate) fn assert_notify_channel_ok(
    step_type: &str,
    config: &Option<serde_json::Value>,
) -> Result<(), AppError> {
    if step_type != "notify" {
        return Ok(());
    }
    let channel = config
        .as_ref()
        .and_then(|c| c.get("channel"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if crate::execution::is_notify_channel(channel) {
        return Ok(());
    }
    Err(notify_channel_error(if channel.is_empty() {
        "(missing)"
    } else {
        channel
    }))
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

/// The Data-Card-first rule as ONE predicate: given whatever ends up at a workflow's position 0,
/// answer yes or the refusal. Every write path that decides the order asks this — the Builder's
/// create/reorder (below) and the template path's create/import/install (kanban t_96e77263), which
/// is the only way the two doors cannot drift apart again.
pub(crate) fn assert_first_step_is_data_card(
    first_step_type: Option<&str>,
) -> Result<(), AppError> {
    match first_step_type {
        Some(first) if !is_data_card(first) => Err(data_card_first_error()),
        _ => Ok(()),
    }
}

/// Refuse a write that would leave a non-Data-Card step first — unless the
/// workflow already breaks the rule today, in which case it stays editable.
/// The grandfather clause matters: the inbound-capture workflows legitimately
/// start with an `integration` step, and refusing them would make live data
/// impossible to edit.
///
/// The grandfather reads the CURRENT rows only. A NEW workflow (nothing at position 0 yet) has
/// nothing to grandfather, so its step 1 is refused outright — which is exactly the template path's
/// semantics: `create_template`/`import_template`/`install` all build a workflow that does not
/// exist yet (kanban t_96e77263).
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
    assert_first_step_is_data_card(projected.first().map(|s| s.step_type.as_str()))
}

pub async fn create_workflow_step(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateWorkflowStepRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Verify workflow exists and belongs to account
    let _workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND aid = $2 AND is_active = true",
    )
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

    // Guardrail: step 1 of a workflow is always a Data Card. This is the SAME predicate the
    // template path runs on a template's first step (kanban t_96e77263).
    if sort_order == 0 {
        assert_first_step_is_data_card(Some(&req.step_type))?;
    }

    // The whole vocabulary is `crate::execution::EXECUTABLE_STEP_TYPES`, so a step the engine has
    // no arm for cannot be created at all. Before kanban t_fe60cdf5 this handler validated NOTHING
    // and the API happily accepted types that fell through the engine's `_` arm and did nothing.
    //
    // A RETIRED type says so by name (kanban t_02519738): `export` was the console-offered one and
    // used to be accepted, POSTed to a platform webhook that is not registered, and reported
    // `completed` for the 404 AND for a transport failure.
    if crate::execution::is_retired_step_type(&req.step_type) {
        return Err(AppError::Validation(format!(
            "Step type '{}' is retired: nothing in this app performs it, so the step would do \
             nothing. Valid step types are: {}",
            req.step_type,
            crate::execution::executable_step_type_list()
        )));
    }
    if !crate::execution::is_executable_step_type(&req.step_type) {
        return Err(AppError::Validation(format!(
            "Step type '{}' has no executor in this app. Valid step types are: {}",
            req.step_type,
            crate::execution::executable_step_type_list()
        )));
    }

    // A notify step's CHANNEL is vocabulary too (kanban t_08be842f).
    assert_notify_channel_ok(&req.step_type, &req.config)?;

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
    let _workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND aid = $2 AND is_active = true",
    )
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

    // An edit may not write a notify channel the product cannot deliver on either (kanban
    // t_08be842f): this is also the path a legacy `email` step takes to become deliverable.
    assert_notify_channel_ok(&current_type, &req.config)?;

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
    let _workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND aid = $2 AND is_active = true",
    )
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
    let _workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND aid = $2 AND is_active = true",
    )
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

    let workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND aid = $2 AND is_active = true",
    )
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
            "webhook_method": crate::n8n_converter::MIRROR_TRIGGER_METHOD,
            "name": v.get("name").cloned().unwrap_or(serde_json::Value::Null),
            "node_count": v.get("node_count").cloned().unwrap_or(serde_json::Value::Null),
            // Idempotence is visible here too: the id is stable, and a later
            // mirror reports "updated" instead of importing another copy.
            "n8n_workflow_id": v.get("n8n_workflow_id").cloned().unwrap_or(serde_json::Value::Null),
            "action": v.get("action").cloned().unwrap_or(serde_json::Value::Null),
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

    let workflow = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE id = $1 AND aid = $2 AND is_active = true",
    )
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
                "Workflow '{}' ran {} step(s) in-process; {} step(s) are waiting (a delay advances on its due time, a manual step needs a decision).",
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
        ("notify", vec!["channel", "recipient"]),
        ("data-card", vec!["metric_key"]),
        ("design", vec!["prompt"]),
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
        // ONE vocabulary: the types the engine can execute (src/execution.rs). This list used to be
        // a second, hand-kept copy that had drifted — eleven of its names had no engine arm, so the
        // validator blessed a workflow whose steps did nothing (kanban t_fe60cdf5). `research` and
        // `openclaw` are retired: nothing in this app can run either (see the decision record).
        let valid_types = crate::execution::EXECUTABLE_STEP_TYPES;

        // A RETIRED type says so by name (kanban t_02519738): `export` was the console-offered one.
        if crate::execution::is_retired_step_type(step_type) {
            errors.push(format!(
                "Step {} '{}': step type '{}' is retired — nothing in this app performs it, so the \
                 step would do nothing. Valid types are: {}",
                i + 1,
                step_name,
                step_type,
                valid_types.join(", ")
            ));
            continue;
        }

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

        // A notify step's CHANNEL is vocabulary too (kanban t_08be842f): `email` and `sms` were
        // channels the product could not deliver on, so a workflow that carries one is reported
        // here exactly as an unknown step type is.
        if step_type == "notify" {
            let channel = step
                .get("config")
                .and_then(|c| c.get("channel"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !channel.is_empty() && !crate::execution::is_notify_channel(channel) {
                errors.push(format!(
                    "Step {} '{}' (notify): Unknown channel '{}'. Valid channels are: {}",
                    i + 1,
                    step_name,
                    channel,
                    crate::execution::notify_channel_list()
                ));
            }
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

/// The mirror's nodes run in n8n, not in this process, so the engine's destination gate does not
/// cover them (kanban t_2741ac13). These legs pin both halves of the fix: a tenant destination the
/// gate allows still mirrors, and a destination the gate refuses (or one it cannot read) fails the
/// mirror with a reason that names the step — instead of writing a graph n8n would happily fetch.
#[cfg(test)]
mod mirror_destination_gate_tests {
    use super::*;
    use crate::n8n_converter::{convert_steps_to_n8n, tenant_destinations, N8nWorkflow};

    const BASE: &str = "https://app.example.com";

    fn graph(url: &str) -> N8nWorkflow {
        let steps = vec![
            json!({"step_type": "data-card", "name": "Card", "config": {"metric_key": "m"}}),
            json!({"step_type": "http-request", "name": "Tenant API",
                   "config": {"method": "GET", "url": url}}),
        ];
        convert_steps_to_n8n(&steps, Uuid::new_v4(), Uuid::new_v4(), BASE, "sync-key")
    }

    #[tokio::test]
    async fn a_public_destination_still_mirrors() {
        let g = graph("http://209.222.97.179:18099/hit/x");
        assert!(gate_mirror_destinations(&g, BASE).await.is_ok());
    }

    #[tokio::test]
    async fn a_loopback_destination_fails_the_mirror_and_names_the_step() {
        let g = graph("http://127.0.0.1:18098/hit/loopback");
        let err = gate_mirror_destinations(&g, BASE)
            .await
            .expect_err("a loopback destination must fail the mirror");
        assert!(err.contains("Tenant API"), "{err}");
        assert!(err.contains("127.0.0.1"), "{err}");
        assert!(err.contains("not a valid destination"), "{err}");
    }

    #[tokio::test]
    async fn a_run_time_expression_is_refused_rather_than_shipped_ungated() {
        let g = graph("={{ $json.target }}");
        let err = gate_mirror_destinations(&g, BASE)
            .await
            .expect_err("a destination the app cannot read must not be shipped");
        assert!(err.contains("Tenant API"), "{err}");
        assert!(err.contains("run time"), "{err}");
    }

    #[tokio::test]
    async fn an_empty_destination_is_refused() {
        let g = graph("");
        let err = gate_mirror_destinations(&g, BASE)
            .await
            .expect_err("an empty destination must not be shipped");
        assert!(err.contains("has no destination URL"), "{err}");
    }

    /// This app's own callbacks are generated here, not tenant input: a graph of one Data Card step
    /// emits callbacks to this app's origin only, and the gate is offered nothing.
    #[tokio::test]
    async fn this_apps_own_callbacks_are_not_gated() {
        let steps = vec![json!({"step_type": "data-card", "name": "Card", "config": {}})];
        let g = convert_steps_to_n8n(&steps, Uuid::new_v4(), Uuid::new_v4(), BASE, "sync-key");
        assert!(tenant_destinations(&g, BASE).is_empty());
        assert!(gate_mirror_destinations(&g, BASE).await.is_ok());
    }
}

/// Regression tests for kanban t_a965cf32 — no hard delete of a `workflows` row may leave its
/// `WFS <uuid>` n8n mirror behind.
///
/// These drive the real `retire_mirrors_with` against a stub n8n on an ephemeral port, so the
/// "the operation retires the mirror" and "the operation REFUSES when it cannot" halves are both
/// asserted without a database or a live n8n.
#[cfg(test)]
mod mirror_retirement_tests {
    use super::*;
    use axum::routing::{delete as axum_delete, get as axum_get};
    use axum::Router;
    use std::sync::{Arc, Mutex};

    /// The stub answers n8n's `GET /api/v1/workflows` (the mirror lookup, list-shaped, filtered
    /// client-side by exact name exactly as the real one is) and accepts
    /// `DELETE /api/v1/workflows/{id}`, recording every id it was asked to delete.
    async fn stub_n8n(
        mirrors: Vec<(&'static str, String)>,
        lookup_status: u16,
        delete_status: u16,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let deleted: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let rows: Vec<serde_json::Value> = mirrors
            .iter()
            .map(|(id, name)| {
                json!({"id": id, "name": name, "createdAt": "2026-01-01T00:00:00.000Z"})
            })
            .collect();

        let app = Router::new()
            .route(
                "/api/v1/workflows",
                axum_get(move || {
                    let rows = rows.clone();
                    async move {
                        (
                            StatusCode::from_u16(lookup_status).unwrap(),
                            Json(json!({"data": rows})),
                        )
                    }
                }),
            )
            .route(
                "/api/v1/workflows/{id}",
                axum_delete({
                    let deleted = deleted.clone();
                    move |Path(id): Path<String>| {
                        let deleted = deleted.clone();
                        async move {
                            deleted.lock().unwrap().push(id);
                            StatusCode::from_u16(delete_status).unwrap()
                        }
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("stub listener");
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (base, deleted)
    }

    /// The name must be the converter's (`src/n8n_converter.rs`): a mirror under any other name is
    /// not this row's copy, and the census/guard reads the same string.
    #[test]
    fn mirror_name_is_the_converter_name() {
        let id = Uuid::new_v4();
        let name = mirror_name(id);
        let suffix = name.strip_prefix("WFS ").expect("`WFS ` prefix");
        assert_eq!(Uuid::parse_str(suffix).expect("uuid suffix"), id);
        assert_eq!(name, format!("WFS {}", id));
    }

    /// The whole point of the card: the operation deletes the mirror — and only that workflow's.
    #[tokio::test]
    async fn retires_the_mirror_and_leaves_another_workflows_mirror_alone() {
        let target = Uuid::new_v4();
        let unrelated = Uuid::new_v4();
        let (base, deleted) = stub_n8n(
            vec![
                ("n8n-target", mirror_name(target)),
                ("n8n-other", mirror_name(unrelated)),
            ],
            200,
            204,
        )
        .await;

        let n = retire_mirrors_with(&reqwest::Client::new(), &base, "k", &[target])
            .await
            .expect("the mirror must be retirable");

        assert_eq!(n, 1, "one mirror retired");
        assert_eq!(*deleted.lock().unwrap(), vec!["n8n-target".to_string()]);
    }

    /// A hard delete must be REFUSED while a mirror may survive, so a lookup that fails is an
    /// error — never a silent pass that orphans the mirror.
    #[tokio::test]
    async fn refuses_when_the_lookup_fails() {
        let id = Uuid::new_v4();
        let (base, deleted) = stub_n8n(vec![("n8n-target", mirror_name(id))], 500, 204).await;

        let err = retire_mirrors_with(&reqwest::Client::new(), &base, "k", &[id])
            .await
            .expect_err("a lookup failure must refuse the retire");

        assert!(err.contains("lookup"), "unexpected error: {}", err);
        assert!(deleted.lock().unwrap().is_empty(), "nothing may be deleted");
    }

    /// n8n answering the lookup but refusing the delete is a refusal too.
    #[tokio::test]
    async fn refuses_when_n8n_refuses_the_delete() {
        let id = Uuid::new_v4();
        let (base, deleted) = stub_n8n(vec![("n8n-target", mirror_name(id))], 200, 500).await;

        let err = retire_mirrors_with(&reqwest::Client::new(), &base, "k", &[id])
            .await
            .expect_err("a refused delete must refuse the retire");

        assert!(
            err.contains("refused deleting"),
            "unexpected error: {}",
            err
        );
        assert_eq!(*deleted.lock().unwrap(), vec!["n8n-target".to_string()]);
    }

    /// A workflow with no mirror is already safe: zero retired, zero deletes, no error.
    #[tokio::test]
    async fn no_mirror_is_not_a_failure() {
        let id = Uuid::new_v4();
        let (base, deleted) = stub_n8n(vec![], 200, 204).await;

        let n = retire_mirrors_with(&reqwest::Client::new(), &base, "k", &[id])
            .await
            .expect("an unmapped workflow is already safe");
        assert_eq!(n, 0);
        assert!(deleted.lock().unwrap().is_empty());
    }
}
