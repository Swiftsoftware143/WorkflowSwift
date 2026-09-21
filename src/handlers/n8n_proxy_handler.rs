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

pub async fn trigger_n8n_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let workflow_id = req
        .get("workflow_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if workflow_id.is_empty() {
        return Err(AppError::Validation("workflow_id is required".to_string()));
    }

    let payload = req
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    let webhook_data = req.get("webhook_data").cloned();

    fire_n8n_webhook(
        &state,
        aid,
        &claims.sub,
        &workflow_id,
        payload,
        webhook_data,
    )
    .await
}

/// `POST /workflows/trigger` — the endpoint the shipped Swift Market Intel
/// extension calls (background.js sendToWorkflow / popup.js "send to
/// WorkflowSwift"), with `{workflow_id, input_data, source, timestamp}`.
///
/// The card that added this path asked for two things beyond the generic
/// `/n8n/trigger`: the payload key the extension actually sends (`input_data`),
/// and a tenant-scoped target — a workflow owned by another account must be
/// refused here, never forwarded to n8n.
///
/// Resolution order:
///
/// - a UUID `workflow_id` must be a `workflows` row of THIS account; another
///   tenant's workflow is refused (403) and an unknown one is 404;
/// - a non-UUID is treated as a workflow name: a row matching it must belong to
///   this account (another tenant's -> 403), while a name that matches no row
///   keeps the historical bare-n8n-webhook-alias behaviour.
pub async fn trigger_extension_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let workflow_id = req
        .get("workflow_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    if workflow_id.is_empty() {
        return Err(AppError::Validation("workflow_id is required".to_string()));
    }

    if let Ok(wid) = Uuid::parse_str(&workflow_id) {
        let owner: Option<Uuid> = sqlx::query_scalar("SELECT aid FROM workflows WHERE id = $1")
            .bind(wid)
            .fetch_optional(&state.db)
            .await?;
        match owner {
            Some(other) if other != aid => {
                return Err(AppError::Forbidden(
                    "Workflow belongs to another account".to_string(),
                ))
            }
            None => return Err(AppError::NotFound("Workflow not found".to_string())),
            _ => {}
        }
    } else {
        let owner: Option<Uuid> =
            sqlx::query_scalar("SELECT aid FROM workflows WHERE lower(name) = lower($1) LIMIT 1")
                .bind(&workflow_id)
                .fetch_optional(&state.db)
                .await?;
        if let Some(other) = owner {
            if other != aid {
                return Err(AppError::Forbidden(
                    "Workflow belongs to another account".to_string(),
                ));
            }
        }
    }

    // The extension sends the scraped page as `input_data`; keep `payload` as a
    // fallback so both spellings work. `source`/`timestamp` travel alongside so
    // the n8n node can tell where the run came from.
    let payload = req
        .get("input_data")
        .or_else(|| req.get("payload"))
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    let webhook_data = json!({
        "source": req.get("source").cloned().unwrap_or(serde_json::Value::Null),
        "timestamp": req.get("timestamp").cloned().unwrap_or(serde_json::Value::Null),
    });

    fire_n8n_webhook(
        &state,
        aid,
        &claims.sub,
        &workflow_id,
        payload,
        Some(webhook_data),
    )
    .await
}

/// Shared n8n leg: charge one credit, POST the trigger body to the tenant's n8n
/// webhook, refund the credit if n8n is unreachable.
async fn fire_n8n_webhook(
    state: &AppState,
    aid: Uuid,
    triggered_by: &str,
    workflow_id: &str,
    payload: serde_json::Value,
    webhook_data: Option<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    // Check credit balance
    let balance = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1",
    )
    .bind(aid)
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
        r#"INSERT INTO credit_transactions (id, aid, amount, transaction_type, description)
           VALUES ($1, $2, -1, 'n8n_execution', $3)"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(format!("n8n workflow execution: {}", workflow_id))
    .execute(&state.db)
    .await?;

    // Call n8n webhook on configured n8n instance
    let n8n_url = format!(
        "{}/webhook/{}",
        state.config.n8n_webhook_url.trim_end_matches('/'),
        workflow_id
    );
    let client = reqwest::Client::new();

    let n8n_body = json!({
        "aid": aid,
        "triggered_by": triggered_by,
        "payload": payload,
        "webhook_data": webhook_data,
    });

    let n8n_response = client.post(&n8n_url).json(&n8n_body).send().await;

    match n8n_response {
        Ok(resp) => {
            let status_code = resp.status();
            let body: serde_json::Value = resp
                .json()
                .await
                .unwrap_or(json!({"note": "n8n responded without body"}));

            let new_balance = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1",
            )
            .bind(aid)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

            Ok(Json(json!({
                "status": "triggered",
                "n8n_status": status_code.as_u16(),
                "n8n_response": body,
                "remaining_balance": new_balance,
            })))
        }
        Err(e) => {
            // Refund credit on failure
            sqlx::query(
                r#"INSERT INTO credit_transactions (id, aid, amount, transaction_type, description)
                   VALUES ($1, $2, 1, 'refund', $3)"#,
            )
            .bind(Uuid::new_v4())
            .bind(aid)
            .bind(format!("Refund for failed n8n execution: {}", workflow_id))
            .execute(&state.db)
            .await?;

            Err(AppError::Internal(format!(
                "n8n webhook call failed: {}",
                e
            )))
        }
    }
}

pub async fn check_n8n_health(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let client = reqwest::Client::new();
    let health_url = format!(
        "{}/health",
        state.config.n8n_webhook_url.trim_end_matches('/')
    );

    match client.get(&health_url).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let body: serde_json::Value = resp
                .json()
                .await
                .unwrap_or(json!({"note": "health endpoint returned non-JSON"}));

            Ok(Json(json!({
                "status": if status == 200 { "healthy" } else { "degraded" },
                "n8n_status_code": status,
                "n8n_response": body,
            })))
        }
        Err(e) => Ok(Json(json!({
            "status": "unhealthy",
            "error": format!("Cannot reach n8n: {}", e),
        }))),
    }
}
