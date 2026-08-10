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

    let payload = req
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    let webhook_data = req.get("webhook_data").cloned();

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
        "aid": claims.aid,
        "triggered_by": claims.sub,
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
