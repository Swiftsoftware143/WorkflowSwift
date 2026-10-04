//! Notify dispatch — the app-side half of a tenant workflow's Notify step
//! (kanban t_d3ff37ef).
//!
//! A generated graph cannot hold a provider credential: an `emailSend` node declares `smtp` as a
//! REQUIRED credential, n8n holds none, and ONE such node makes the tenant's ENTIRE workflow
//! un-activatable (`400 … Missing required credential: smtp` — measured on n8n 2.34.6,
//! kanban t_70baf9b0). So the mirror emits a plain `n8n-nodes-base.httpRequest` POSTing to
//! `POST /api/v1/notify/dispatch` (the same shape as the sibling `dashboard/push-widget-data` and
//! `n8n/run-outcome` arms) and the app does the sending, from its own panel-configured provider.
//!
//! AUTH: `notify_dispatch` is a machine caller only — `X-Internal-Key: <INTERNAL_SYNC_KEY>`,
//! exactly like `POST /api/v1/n8n/run-outcome` (`instance_handler.rs`). No JWT: the run was
//! triggered externally, so there is no user token to present. An unset key refuses every caller
//! (fail closed) rather than accepting an empty header.
//!
//! The two remaining handlers are for the tenant console and are JWT-scoped to the caller's own
//! account: which channels this install can actually deliver, and which of the account's own
//! people a Notify step may reach.

use axum::{
    extract::{Json, State},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::AppState;

/// The step index out of a dispatch body.
///
/// n8n's `httpRequest` node sends every `bodyParameters` value as a STRING, so the graph this route
/// exists for posts `{"step_index":"1"}` — measured live (kanban t_d3ff37ef: the mirrored node ran,
/// the route answered 400 `step_index must be an integer`, and the graph's Error Trigger arm
/// reported it). Accepting the decimal string as well as the JSON number keeps the route's contract
/// honest without making the generated graph carry a type n8n cannot express here.
fn step_index_from(body: &serde_json::Value) -> Option<i64> {
    body.get("step_index").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
    })
}

/// `POST /api/v1/notify/dispatch`
///
/// Body, exactly the sibling arms' shape:
/// ```json
/// { "workflow_id": "<uuid>", "step_index": 2 }
/// ```
///
/// The route loads the step from `workflow_steps`, resolves its recipient to the account's OWN
/// people (`crate::notify`), sends through the panel-configured provider, and records the outcome
/// in `notify_send_attempts`. Every failure mode answers the caller with a status it can act on:
/// a refusal is 400, the account's hourly cap is 429, a provider failure is 502. Nothing here ever
/// answers 200 for a send that did not happen.
pub async fn notify_dispatch(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let presented = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if state.config.internal_sync_key.is_empty() || presented != state.config.internal_sync_key {
        return Err(AppError::Unauthorized);
    }

    let workflow_id = body
        .get("workflow_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
        .ok_or_else(|| {
            AppError::BadRequest("workflow_id must be the workflow's uuid".to_string())
        })?;
    let step_index = step_index_from(&body)
        .ok_or_else(|| AppError::BadRequest("step_index must be an integer".to_string()))?;
    if step_index < 0 || step_index > i32::MAX as i64 {
        return Err(AppError::BadRequest(
            "step_index is out of range".to_string(),
        ));
    }

    // The workflow is the tenant boundary: everything the step may reach is resolved from THIS
    // row's `aid`, never from anything in the request body.
    let aid: Uuid = sqlx::query_scalar("SELECT aid FROM workflows WHERE id = $1")
        .bind(workflow_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("workflow {workflow_id} does not exist")))?;

    // The step the mirror named. The mirror walks the steps in `sort_order` and names each node by
    // its index in that walk, so the same ordering resolves it here.
    let row: Option<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT step_type, COALESCE(config, '{}'::jsonb) FROM workflow_steps \
         WHERE workflow_id = $1 ORDER BY sort_order, created_at OFFSET $2 LIMIT 1",
    )
    .bind(workflow_id)
    .bind(step_index as i32)
    .fetch_optional(&state.db)
    .await?;
    let (step_type, config) = row.ok_or_else(|| {
        AppError::NotFound(format!(
            "workflow {workflow_id} has no step at index {step_index}"
        ))
    })?;
    if step_type != "notify" {
        return Err(AppError::BadRequest(format!(
            "step {step_index} of workflow {workflow_id} is a '{step_type}' step, not a Notify \
             step"
        )));
    }

    let channel = config
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    // `webhook` is handled where it always was — a direct outbound call to the tenant's own URL,
    // which needs no provider and no app route. This route exists for the sender-backed channels.
    if channel == "webhook" {
        return Err(AppError::BadRequest(
            "a webhook Notify step posts straight to its own URL — it has nothing to dispatch \
             through this route"
                .to_string(),
        ));
    }

    let outcome = crate::notify::deliver(
        &state,
        aid,
        Some(workflow_id),
        Some(step_index as i32),
        &channel,
        &config,
    )
    .await;

    if let Some(err) = crate::notify::outcome_error(&outcome) {
        return Err(err);
    }

    Ok(Json(json!({
        "status": outcome.status,
        "channel": outcome.channel,
        "workflow_id": workflow_id.to_string(),
        "step_index": step_index,
        "detail": outcome.detail,
        "recipients": outcome.recipients,
    })))
}

/// `GET /api/v1/notify/channels`
///
/// The channels the tenant console may OFFER — exactly the ones this install can deliver on.
/// `webhook` is always there (an outbound call to a URL the tenant owns); `email` / `sms` appear
/// only once their sender is configured in the admin panel, which is what makes "nothing is
/// offered that cannot be delivered" true in the UI as well as at the write path.
pub async fn notify_channels(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let senders = crate::notify::load_senders(&state).await;
    Ok(Json(json!({
        "channels": senders.offered(),
        "senders": { "email": senders.email, "sms": senders.sms },
        "scopes": crate::notify::NOTIFY_SCOPES,
    })))
}

/// `GET /api/v1/notify/recipients`
///
/// The account's OWN people, which is the only set a Notify step may reach. The console's picker
/// is built from this: every account user, the billing contact, or one named user. A person with
/// no phone on file is reported with `has_phone: false` so the SMS picker can say why they are
/// not selectable rather than silently dropping them.
pub async fn notify_recipients(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let rows: Vec<(Uuid, String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT id, name, email, phone, role FROM users \
         WHERE aid = $1 AND is_active = true ORDER BY created_at",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let people: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, name, email, phone, role)| {
            json!({
                "user_id": id.to_string(),
                "name": name,
                "email": email,
                "has_phone": phone.as_deref().map(|p| !p.trim().is_empty()).unwrap_or(false),
                "billing_contact": role == "company_admin",
            })
        })
        .collect();

    Ok(Json(json!({ "recipients": people })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dispatch body a generated graph really posts. n8n's `httpRequest` node sends every
    /// `bodyParameters` value as a STRING — measured live on the mirrored node (kanban t_d3ff37ef:
    /// `{"step_index":"1"}` answered 400 `step_index must be an integer`), so the route has to read
    /// the decimal string as well as the JSON number, and refuse anything else.
    #[test]
    fn step_index_is_read_from_the_number_or_its_decimal_string() {
        assert_eq!(step_index_from(&json!({"step_index": 1})), Some(1));
        assert_eq!(step_index_from(&json!({"step_index": "1"})), Some(1));
        assert_eq!(step_index_from(&json!({"step_index": " 12 "})), Some(12));
        assert_eq!(step_index_from(&json!({"step_index": "0"})), Some(0));
        // Not an index: refused, never defaulted to 0 (which would dispatch the FIRST step).
        assert_eq!(step_index_from(&json!({"step_index": "abc"})), None);
        assert_eq!(step_index_from(&json!({"step_index": ""})), None);
        assert_eq!(step_index_from(&json!({"step_index": null})), None);
        assert_eq!(step_index_from(&json!({})), None);
        assert_eq!(step_index_from(&json!({"step_index": 1.5})), None);
    }
}
