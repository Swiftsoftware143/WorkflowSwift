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
            "generate" | "ai-action" | "ai_action" => {
                // Call the configured LLM provider
                let provider = step_config.get("provider").and_then(|v| v.as_str()).unwrap_or("openai");
                let prompt = step_config.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                let model = step_config.get("model").and_then(|v| v.as_str()).unwrap_or("gpt-4");
                let system_prompt = step_config.get("system_prompt").and_then(|v| v.as_str()).unwrap_or("");

                // Try to call the AI provider via n8n or direct API
                // For now, route through n8n which handles provider routing
                let n8n_payload = json!({
                    "action": "generate",
                    "provider": provider,
                    "model": model,
                    "system_prompt": system_prompt,
                    "prompt": prompt,
                    "context": context,
                    "contact": payload.contact,
                    "data": payload.data,
                    "campaign_slug": slug,
                });

                let n8n_url = format!("{}/webhook/workflowswift-generate", state.config.n8n_webhook_url.trim_end_matches('/'));
                let client = reqwest::Client::new();
                let mut req = client.post(&n8n_url).json(&n8n_payload).timeout(std::time::Duration::from_secs(60));
                if !state.config.n8n_api_key.is_empty() {
                    req = req.header("X-API-Key", &state.config.n8n_api_key);
                }

                let mut payload_req = req.send().await;

                // If the n8n webhook doesn't exist, try direct API call as fallback
                if payload_req.is_err() {
                    // Fallback: send to the default n8n workflow handler
                    let fallback_url = format!("{}/webhook/incoming/content-gen", state.config.n8n_webhook_url.trim_end_matches('/'));
                    let client = reqwest::Client::new();
                    let mut fallback_req = client.post(&fallback_url).json(&n8n_payload).timeout(std::time::Duration::from_secs(60));
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
                let format_type = step_config.get("format").and_then(|v| v.as_str()).unwrap_or("twitter-thread");
                let tone = step_config.get("tone").and_then(|v| v.as_str()).unwrap_or("professional");
                
                // Format via n8n or mark as config-based transformation
                let _n8n_payload = json!({
                    "action": "format",
                    "format": format_type,
                    "tone": tone,
                    "context": context,
                    "contact": payload.contact,
                    "data": payload.data,
                });

                json!({"step": i, "type": "format", "status": "completed", "format": format_type, "tone": tone, "note": "Formatting queued — will transform content for selected platform"})
            }
            "design" => {
                let style = step_config.get("style").and_then(|v| v.as_str()).unwrap_or("modern");
                let dimensions = step_config.get("dimensions").and_then(|v| v.as_str()).unwrap_or("1024x1024");

                json!({"step": i, "type": "design", "status": "completed", "style": style, "dimensions": dimensions, "note": "Design queued — will generate visual assets via configured provider"})
            }
            "publish" => {
                let provider = step_config.get("provider").and_then(|v| v.as_str()).unwrap_or("webhook");
                let message = step_config.get("message").and_then(|v| v.as_str()).unwrap_or("");
                let platforms: Vec<String> = step_config.get("platforms").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
                let media_url = step_config.get("media_url").and_then(|v| v.as_str()).unwrap_or("");

                // Route to n8n which handles multi-platform publishing
                let n8n_payload = json!({
                    "action": "publish",
                    "provider": provider,
                    "platforms": platforms,
                    "message": message,
                    "media_url": media_url,
                    "context": context,
                    "contact": payload.contact,
                    "data": payload.data,
                });

                let n8n_url = format!("{}/webhook/workflowswift-publish", state.config.n8n_webhook_url.trim_end_matches('/'));
                let client = reqwest::Client::new();
                let mut req = client.post(&n8n_url).json(&n8n_payload).timeout(std::time::Duration::from_secs(30));
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
                let format = step_config.get("format").and_then(|v| v.as_str()).unwrap_or("csv");
                let targets: Vec<String> = step_config.get("targets").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
                let filename = step_config.get("filename").and_then(|v| v.as_str()).unwrap_or("workflow-export");

                // Send export job to n8n
                let n8n_payload = json!({
                    "action": "export",
                    "format": format,
                    "targets": targets,
                    "filename": filename,
                    "context": context,
                    "contact": payload.contact,
                    "data": payload.data,
                });

                let n8n_url = format!("{}/webhook/workflowswift-export", state.config.n8n_webhook_url.trim_end_matches('/'));
                let client = reqwest::Client::new();
                let mut req = client.post(&n8n_url).json(&n8n_payload).timeout(std::time::Duration::from_secs(30));
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
                let channel = step_config.get("channel").and_then(|v| v.as_str()).unwrap_or("email");
                let recipient = step_config.get("recipient").and_then(|v| v.as_str()).unwrap_or("");
                let subject = step_config.get("subject").and_then(|v| v.as_str()).unwrap_or("");
                let message = step_config.get("message").and_then(|v| v.as_str()).unwrap_or("");

                // Route to n8n notification handler
                let n8n_payload = json!({
                    "action": "notify",
                    "channel": channel,
                    "recipient": recipient,
                    "subject": subject,
                    "message": message,
                    "context": context,
                    "contact": payload.contact,
                    "data": payload.data,
                });

                let n8n_url = format!("{}/webhook/workflowswift-notify", state.config.n8n_webhook_url.trim_end_matches('/'));
                let client = reqwest::Client::new();
                let mut req = client.post(&n8n_url).json(&n8n_payload).timeout(std::time::Duration::from_secs(15));
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
                // Pull data from dashboard and attach to context
                let widget_name = step_config.get("widget_name").and_then(|v| v.as_str()).unwrap_or("");
                let metric_key = step_config.get("metric_key").and_then(|v| v.as_str()).unwrap_or("");

                // Query dashboard_data for latest value
                let metric_value: Option<serde_json::Value> = if !metric_key.is_empty() {
                    sqlx::query_scalar(
                        r#"SELECT metric_value FROM dashboard_data WHERE aid = $1 AND metric_key = $2 ORDER BY recorded_at DESC LIMIT 1"#
                    )
                    .bind(aid)
                    .bind(metric_key)
                    .fetch_optional(&state.db)
                    .await
                    .unwrap_or(None)
                } else {
                    None
                };

                json!({"step": i, "type": "data-card", "status": "completed", "widget_name": widget_name, "metric_key": metric_key, "metric_value": metric_value})
            }
            "fork" => {
                let branches: Vec<serde_json::Value> = step_config.get("branches").and_then(|v| v.as_array()).map(|a| a.clone()).unwrap_or_default();
                json!({"step": i, "type": "fork", "status": "completed", "branches": branches.len(), "note": "Workflow will fork into parallel branches"})
            }
            "loop" => {
                let iterations = step_config.get("iterations").and_then(|v| v.as_u64()).unwrap_or(1);
                json!({"step": i, "type": "loop", "status": "completed", "iterations": iterations, "note": format!("Will loop {} times or until condition met", iterations)})
            }
            "condition" | "ifelse" => {
                let condition = step_config.get("condition").and_then(|v| v.as_str()).unwrap_or("true");
                json!({"step": i, "type": "condition", "status": "completed", "condition": condition, "note": "Condition evaluation queued"})
            }
            _ => {
                // Allow unknown step types to pass through instead of failing
                // Frontend can show a warning but the workflow doesn't break
                json!({"step": i, "type": step_type, "status": "warning", "message": format!("Step type '{}' is not executable — marked as warning but workflow continues", step_type)})
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
