//! Incoming webhook handler — the single endpoint all Swift tools push to.
//!
//! POST /api/v1/incoming
//!
//! Any internal Swift tool (IncentiveSwift, FunnelSwift, MissedCallRespondr,
//! ADASwift, etc.) pushes its lead data here. WorkflowSwift matches the
//! incoming data to an active workflow (by configured trigger), creates a
//! workflow instance, and steps through each step — dispatching to integration
//! targets using stored API keys, triggering n8n workflows, etc.
//!
//! Users configure everything in WorkflowSwift — this is the hands-off layer.

use axum::{
    extract::State,
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use chrono::Utc;

use crate::state::AppState;
use crate::error::{AppError, ApiResult};

/// Payload that any Swift tool sends to WorkflowSwift.
#[derive(Debug, Deserialize)]
pub struct IncomingPayload {
    /// Source tool: "incentiveswift", "funnelswift", "missedcallrespondr", "adaswift", etc.
    pub source: String,
    /// Campaign or workflow slug to match against workflow config.
    pub campaign_slug: Option<String>,
    /// Contact information.
    pub contact: IncomingContact,
    /// Arbitrary data / form answers / metadata from the source.
    pub data: Option<Value>,
    /// Entry ID from the source system (for traceability).
    pub source_entry_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IncomingContact {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub business_name: Option<String>,
}

/// POST /api/v1/incoming
///
/// Receives lead data from any Swift tool, matches to an active workflow,
/// creates an instance, and steps through the workflow steps.
pub async fn receive_incoming(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<IncomingPayload>,
) -> ApiResult<Json<Value>> {
    // Internal auth: if server has an internal_sync_key configured,
    // the request MUST provide the matching X-Internal-Key header.
    // If no key is configured on the server, the endpoint is open.
    if !state.config.internal_sync_key.is_empty() {
        let internal_key = headers.get("x-internal-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if internal_key != state.config.internal_sync_key {
            return Err(AppError::Unauthorized);
        }
    }
    let source = &payload.source;
    let slug = payload.campaign_slug.as_deref().unwrap_or("default");

    tracing::info!(
        "Incoming lead from {} (slug: {}) - {} {} <{}>",
        source,
        slug,
        payload.contact.first_name.as_deref().unwrap_or("?"),
        payload.contact.last_name.as_deref().unwrap_or("?"),
        payload.contact.email.as_deref().unwrap_or("no-email"),
    );

    // Find the first active workflow whose name or description matches the source
    // Workflows are linked to integration targets via workflow_steps.integration_target_id
    // The trigger mapping is: workflow name contains source slug, or a step config maps it
    let workflow = sqlx::query_as::<_, WorkflowRow>(
        r#"SELECT id, aid, name, description, category
           FROM workflows
           WHERE is_active = true
           AND (
               name ILIKE $1
               OR description ILIKE $1
               OR id IN (
                   SELECT ws.workflow_id FROM workflow_steps ws
                   WHERE ws.config->>'source' = $2
                      OR ws.config->>'campaign_slug' = $3
               )
           )
           ORDER BY created_at DESC
           LIMIT 1"#,
    )
    .bind(format!("%{}%", source))
    .bind(source)
    .bind(slug)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

    let workflow = match workflow {
        Some(w) => w,
        None => {
            tracing::info!("No active workflow matched for source={}, slug={}", source, slug);
            return Ok(Json(json!({
                "status": "accepted",
                "matched": false,
                "message": format!("No active workflow matches source '{}' / slug '{}'", source, slug),
                "note": "Create a workflow in WorkflowSwift with 'incoming' as trigger source"
            })));
        }
    };

    // Use the workflow's account
    let aid = workflow.aid;

    // Fetch the workflow steps (ordered by sort_order)
    let steps = sqlx::query_as::<_, WorkflowStepRow>(
        r#"SELECT id, step_type, name, config, sort_order, integration_target_id
           FROM workflow_steps
           WHERE workflow_id = $1
           ORDER BY sort_order ASC"#,
    )
    .bind(workflow.id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

    // Build the context that gets stored with the instance
    let context = json!({
        "source": source,
        "campaign_slug": slug,
        "contact": {
            "first_name": payload.contact.first_name,
            "last_name": payload.contact.last_name,
            "email": payload.contact.email,
            "phone": payload.contact.phone,
            "business_name": payload.contact.business_name,
        },
        "data": payload.data,
        "source_entry_id": payload.source_entry_id,
        "captured_at": Utc::now().to_rfc3339(),
    });

    // Create a workflow instance. We need a client_id — use a system/placeholder one
    let placeholder_client_id = find_or_create_system_client(&state.db, aid, source).await?;
    let instance_id = Uuid::new_v4();

    sqlx::query(
        r#"INSERT INTO workflow_instances (id, workflow_id, client_id, aid, name, status, current_step_order)
           VALUES ($1, $2, $3, $4, $5, 'active', 0)"#,
    )
    .bind(instance_id)
    .bind(workflow.id)
    .bind(placeholder_client_id)
    .bind(aid)
    .bind(format!("Incoming: {} from {}", 
        payload.contact.email.as_deref().unwrap_or("lead"), source))
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to create workflow instance: {}", e)))?;

    // Execute each workflow step
    let mut step_results: Vec<Value> = vec![];

    for (i, step) in steps.iter().enumerate() {
        let step_instance_id = Uuid::new_v4();
        let step_type = &step.step_type;
        let step_config = step.config.clone().unwrap_or(json!({}));

        // Create the instance step record
        sqlx::query(
            r#"INSERT INTO workflow_instance_steps (id, instance_id, step_type, name, sort_order, status)
               VALUES ($1, $2, $3, $4, $5, 'in_progress')"#,
        )
        .bind(step_instance_id)
        .bind(instance_id)
        .bind(step_type)
        .bind(&step.name)
        .bind(i as i32)
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
                        "contact": payload.contact,
                        "data": payload.data,
                        "source": source,
                        "campaign_slug": slug,
                        "source_entry_id": payload.source_entry_id,
                        "context": context,
                    });

                    match super::integration_dispatch_handler::forward_dispatch(
                        &state.db,
                        target_id,
                        aid,
                        &dispatch_payload,
                    ).await {
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
                let url = step_config.get("url").and_then(|v| v.as_str()).unwrap_or("");
                if url.is_empty() {
                    json!({"step": i, "type": "webhook", "status": "skipped", "reason": "No URL configured"})
                } else {
                    let wh_payload = json!({
                        "contact": payload.contact,
                        "data": payload.data,
                        "source": source,
                        "campaign_slug": slug,
                        "source_entry_id": payload.source_entry_id,
                    });
                    let client = reqwest::Client::new();
                    match client.post(url).json(&wh_payload).timeout(std::time::Duration::from_secs(15)).send().await {
                        Ok(resp) => {
                            let status_code = resp.status().as_u16();
                            let body = resp.text().await.unwrap_or_default();
                            json!({"step": i, "type": "webhook", "status": status_code, "url": url, "response": body})
                        }
                        Err(e) => json!({"step": i, "type": "webhook", "status": "error", "error": e.to_string()})
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
                            "contact": payload.contact,
                            "campaign_slug": slug,
                            "source": source,
                            "source_entry_id": payload.source_entry_id,
                        }
                    });

                    let webhook_url = format!("{}/webhook/incoming/{}", n8n_url.trim_end_matches('/'), wf_id);
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
                        Err(e) => json!({"step": i, "type": "n8n", "status": "error", "error": e.to_string()})
                    }
                } else {
                    json!({"step": i, "type": "n8n", "status": "skipped", "reason": "No n8n_workflow_id in step config"})
                }
            }
            "manual" | "approval" => {
                // Manual step — mark as pending; human reviews in WorkflowSwift UI
                json!({"step": i, "type": "manual", "status": "pending"})
            }
            "delay" | "wait" => {
                let duration = step_config.get("duration").and_then(|v| v.as_str()).unwrap_or("1h");
                json!({"step": i, "type": "delay", "duration": duration, "status": "pending", "note": "Will be processed by background worker"})
            }
            _ => {
                json!({"step": i, "type": step_type, "status": "unknown_type", "message": format!("WorkflowSwift doesn't know how to execute step type '{}'", step_type)})
            }
        };

        // Mark step instance as completed (or error)
        let step_status = result.get("status").and_then(|v| v.as_str()).unwrap_or("completed");
        let completed_at = if step_status == "completed" {
            Some(Utc::now())
        } else {
            None
        };

        sqlx::query(
            r#"UPDATE workflow_instance_steps SET status = $1, completed_at = $2, notes = $3 WHERE id = $4"#,
        )
        .bind(step_status)
        .bind(completed_at)
        .bind(result.to_string())
        .bind(step_instance_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

        step_results.push(result);
    }

    // Mark instance as completed if all steps succeeded
    let all_ok = step_results.iter().all(|r| {
        let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("");
        status == "completed" || status == "skipped" || status == "pending"
    });

    let instance_status = if all_ok { "completed" } else { "in_progress" };

    sqlx::query(
        "UPDATE workflow_instances SET status = $1, completed_at = $2, updated_at = NOW() WHERE id = $3"
    )
    .bind(instance_status)
    .bind(if all_ok { Some(Utc::now()) } else { None })
    .bind(instance_id)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

    Ok(Json(json!({
        "status": instance_status,
        "instance_id": instance_id.to_string(),
        "workflow": workflow.name,
        "matched": true,
        "steps_total": steps.len(),
        "steps": step_results,
    })))
}

// ── Internal helper types ──

#[derive(Debug, sqlx::FromRow)]
struct WorkflowRow {
    id: Uuid,
    aid: Uuid,
    name: String,
    description: Option<String>,
    category: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct WorkflowStepRow {
    id: Uuid,
    step_type: String,
    name: String,
    config: Option<Value>,
    sort_order: i32,
    integration_target_id: Option<Uuid>,
}

/// Find or create a system client for incoming/webhook-based leads.
/// This avoids requiring a real client_id for automated workflow instances.
/// Uses a v4 UUID each time (we upsert by email so duplicates are harmless).
async fn find_or_create_system_client(
    db: &sqlx::PgPool,
    aid: Uuid,
    source: &str,
) -> Result<Uuid, AppError> {
    let client_id = Uuid::new_v4();
    let sys_email = format!("incoming+{}@workflowswift.local", source);

    // Check if a system client by email already exists
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM clients WHERE email = $1 AND aid = $2"
    )
    .bind(&sys_email)
    .bind(aid)
    .fetch_optional(db)
    .await
    .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

    if let Some(existing_id) = existing {
        return Ok(existing_id);
    }

    // Create new system client (clients has is_active, not status)
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
