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

use axum::{extract::State, http::HeaderMap, Json};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{ApiResult, AppError};
use crate::handlers::coreswift_external::{push_lead_to_coreswift, CapturedLead};
use crate::state::AppState;

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
        let internal_key = headers
            .get("x-internal-key")
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
            tracing::info!(
                "No active workflow matched for source={}, slug={}",
                source,
                slug
            );
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
    let placeholder_client_id =
        crate::execution::find_or_create_system_client(&state.db, aid, source).await?;
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

    // ── INBOUND CORE SWIFT PUSH (fleet standard R2) ──
    // A real capture just happened: the lead is recorded locally as a workflow instance.
    // Deliver it to the hub too, with the account's own BYOK key. Not connected (or hub
    // down) => this quietly does nothing and the run continues exactly as before.
    {
        let lead = CapturedLead::from_parts(
            payload.contact.first_name.clone(),
            payload.contact.last_name.clone(),
            payload.contact.email.clone(),
            payload.contact.phone.clone(),
            payload.contact.business_name.clone(),
            Some(source.to_string()),
        );
        let fields = payload.data.clone().unwrap_or_else(|| json!({}));
        let workflow_name = workflow.name.clone();
        let pushed = push_lead_to_coreswift(
            &state,
            aid,
            &lead,
            None,
            &[],
            fields,
            &format!("incoming run on workflow '{workflow_name}'"),
        )
        .await;
        tracing::info!(
            "Incoming capture for aid {aid} (source={source}) — CoreSwift push delivered={pushed}"
        );
    }

    // Execute every step IN THIS PROCESS via the shared engine (src/execution.rs).
    // Same engine the user-facing Run button uses — one implementation, two entrances.
    let ctx = crate::execution::StepContext {
        source: source.to_string(),
        campaign_slug: slug.to_string(),
        contact: json!(payload.contact),
        data: payload.data.clone(),
        source_entry_id: payload.source_entry_id.clone(),
        context: context.clone(),
    };

    let outcome =
        crate::execution::execute_steps(&state, aid, workflow.id, instance_id, &ctx).await?;

    Ok(Json(json!({
        "status": outcome.status,
        "instance_id": instance_id.to_string(),
        "workflow": workflow.name,
        "matched": true,
        "steps_total": outcome.steps.len(),
        "steps": outcome.steps,
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
