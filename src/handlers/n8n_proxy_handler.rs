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

/// Shared n8n leg: POST the trigger body to the tenant's n8n webhook and charge
/// one credit **only after n8n has accepted the trigger**.
///
/// Before this, the credit was deducted up front and refunded only when the HTTP
/// call itself failed (a transport `Err`). An n8n 4xx/5xx — e.g. 404 "webhook is
/// not registered" — therefore returned HTTP 200 *and* kept the money, i.e. the
/// customer was billed for an execution that never happened (kanban
/// t_a6a1b299 item 1). Now: n8n answers first, the charge follows a 2xx, and any
/// other outcome comes back as a 502 carrying n8n's own words.
async fn fire_n8n_webhook(
    state: &AppState,
    aid: Uuid,
    triggered_by: &str,
    workflow_id: &str,
    payload: serde_json::Value,
    webhook_data: Option<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    // Balance is still read first: it decides whether the tenant may trigger at
    // all. It is a check, not a charge — nothing is written until n8n says yes.
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

    let resp = client
        .post(&n8n_url)
        .json(&n8n_body)
        .send()
        .await
        .map_err(|e| {
            AppError::Upstream(format!(
                "n8n is unreachable ({}). Nothing ran, so no credit was charged.",
                e
            ))
        })?;

    let status_code = resp.status();
    let raw = resp.text().await.unwrap_or_default();
    let body: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| {
        json!({
            "note": "n8n responded without a JSON body",
            "raw": raw.chars().take(300).collect::<String>(),
        })
    });

    if !status_code.is_success() {
        // n8n answered, and the answer was "no". Nothing executed -> nothing billed.
        tracing::warn!(
            aid = %aid,
            workflow_id = %workflow_id,
            n8n_status = status_code.as_u16(),
            n8n_body = %body,
            "n8n refused the trigger; no credit charged"
        );
        let message = format!(
            "n8n refused the trigger (HTTP {}): {}. Nothing ran, so no credit was charged.",
            status_code.as_u16(),
            n8n_reason(&body).trim_end_matches(|c: char| c == '.' || c.is_whitespace())
        );
        // A 4xx from n8n is a configuration answer the caller can act on (webhook
        // not registered, workflow not active, malformed trigger), and it has to
        // reach the client INTACT: Cloudflare replaces the body of a 5xx with its
        // own "error code: 502" page, so mapping this to 502 left the extension
        // able to say nothing more useful than "HTTP 502" (kanban t_dd19dc40).
        // n8n 5xx stays a 502 Upstream — that one really is an upstream failure.
        return Err(if status_code.is_client_error() {
            AppError::Validation(message)
        } else {
            AppError::Upstream(message)
        });
    }

    // n8n accepted the trigger — charge exactly one credit, now.
    sqlx::query(
        r#"INSERT INTO credit_transactions (id, aid, amount, transaction_type, description)
           VALUES ($1, $2, -1, 'n8n_execution', $3)"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(format!("n8n workflow execution: {}", workflow_id))
    .execute(&state.db)
    .await?;

    let new_balance = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "status": "triggered",
        "charged": true,
        "credits_charged": 1,
        "n8n_status": status_code.as_u16(),
        "n8n_response": body,
        "remaining_balance": new_balance,
    })))
}

/// n8n's reason for refusing, in a form a human can read.
///
/// An n8n 404 body is ~430 bytes of JSON, and truncating it at 300 cuts exactly the
/// sentence that tells the caller what to fix (`…"message":"The requested webhook
/// "POST x" is not registered.`). Prefer the machine-readable `message`, then `hint`
/// (which names the fix: activate the workflow), and only fall back to a blob.
fn n8n_reason(body: &serde_json::Value) -> String {
    for key in ["message", "hint"] {
        if let Some(text) = body.get(key).and_then(|v| v.as_str()) {
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    body.to_string().chars().take(300).collect()
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
