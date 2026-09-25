use axum::{
    extract::{Json, State},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::handlers::industry_handler::canonical_metric_key;
use crate::AppState;

/// Cost for dashboard data push — this is data users paid a workflow credit
/// to generate, so the dashboard read is free. The ingestion costs 0.5 credits
/// to prevent abuse (can't DDOS the dashboard with garbage data).
const DASHBOARD_DATA_COST: i64 = 1; // same as 1 workflow trigger
const DASHBOARD_VIEW_COST: i64 = 0; // viewing stats is free

/// Dashboard overview stats — free to view (they paid to generate the data already)
pub async fn dashboard_stats(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // `is_active` is this app's only soft-delete flag (see `delete_workflow`), so a deleted
    // workflow must not keep inflating the dashboard counter — the same rule `features.rs`
    // applies to the plan seat and `list_workflows` to the table (kanban t_217d0e5f).
    let total_workflows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workflows WHERE aid = $1 AND is_active = true")
            .bind(aid)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);
    let active_instances: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_instances WHERE aid = $1 AND status NOT IN ('completed', 'cancelled')")
        .bind(aid)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    let total_clients: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM clients WHERE aid = $1 AND is_active = true")
            .bind(aid)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);
    let total_templates: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workflow_templates WHERE aid = $1")
            .bind(aid)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

    Ok(Json(json!({
        "stats": {
            "total_workflows": total_workflows,
            "active_instances": active_instances,
            "total_clients": total_clients,
            "total_templates": total_templates,
        }
    })))
}

/// Dashboard activity log — free to view
pub async fn dashboard_activity(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        r#"SELECT id::text, user_id::text, action, entity_type, entity_id::text, created_at::text
           FROM audit_logs WHERE aid = $1 ORDER BY created_at DESC LIMIT 20"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    use sqlx::Row;
    let activities: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<&str, _>("id").unwrap_or(""),
                "user_id": r.try_get::<&str, _>("user_id").unwrap_or(""),
                "action": r.try_get::<&str, _>("action").unwrap_or(""),
                "entity_type": r.try_get::<&str, _>("entity_type").unwrap_or(""),
                "entity_id": r.try_get::<&str, _>("entity_id").unwrap_or(""),
                "created_at": r.try_get::<&str, _>("created_at").unwrap_or(""),
            })
        })
        .collect();

    Ok(Json(json!({"activities": activities})))
}

/// Ingest dashboard data from n8n workflow results.
/// This costs DASHBOARD_DATA_COST credits — it's a premium storage/display
/// feature on top of the workflow execution itself.
///
/// n8n calls this after running a workflow. The data is stored keyed by
/// (aid, dashboard_type) and displayed in the user's dashboard.
pub async fn push_dashboard_data(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Extract dashboard type and data
    let dashboard_type = req
        .get("dashboard_type")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let data = req
        .get("data")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    if data.is_empty() {
        return Err(AppError::Validation("No data payload provided".to_string()));
    }

    // Check credits for dashboard data storage
    let balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if balance < DASHBOARD_DATA_COST {
        return Err(AppError::BadRequest(format!(
            "Insufficient credits for dashboard data push. Need {}, have {}. Purchase more credits.",
            DASHBOARD_DATA_COST, balance
        )));
    }

    // Deduct credit for dashboard data storage
    let tx_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO credit_transactions (id, aid, amount, transaction_type, description)
           VALUES ($1, $2, $3, 'usage', 'Dashboard data: ') "#,
    )
    .bind(tx_id)
    .bind(aid)
    .bind(-DASHBOARD_DATA_COST)
    .bind(format!("Dashboard data push: {}", dashboard_type))
    .execute(&state.db)
    .await?;

    // Get or create default dashboard for this account (async, not closure)
    let dashboard_id: Uuid =
        match sqlx::query_scalar("SELECT id FROM dashboards WHERE aid = $1 AND name = $2 LIMIT 1")
            .bind(aid)
            .bind("Default Dashboard")
            .fetch_optional(&state.db)
            .await?
        {
            Some(id) => id,
            None => {
                let id = Uuid::new_v4();
                sqlx::query(
                    r#"INSERT INTO dashboards (id, aid, name, description, layout)
                   VALUES ($1, $2, 'Default Dashboard', 'Auto-created dashboard', '{}'::jsonb)"#,
                )
                .bind(id)
                .bind(aid)
                .execute(&state.db)
                .await?;
                id
            }
        };

    // Store the data. The key is normalised to the canonical n8n_ form here as well: this write
    // path used to prefix unconditionally, which is how a dashboard_type that already carried the
    // prefix ended up stored as n8n_n8n_<x> (migration 056 collapses those legacy rows).
    let metric_key = canonical_metric_key(dashboard_type);
    let data_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO dashboard_data (id, dashboard_id, aid, metric_key, metric_value)
           VALUES ($1, $2, $3, $4, $5::jsonb)"#,
    )
    .bind(data_id)
    .bind(dashboard_id)
    .bind(aid)
    .bind(&metric_key)
    .bind(serde_json::to_value(&data).unwrap_or_default())
    .execute(&state.db)
    .await?;

    // Clean old data — keep latest 100 entries per key per account
    sqlx::query(
        r#"DELETE FROM dashboard_data
           WHERE aid = $1 AND metric_key = $2
           AND id NOT IN (
               SELECT id FROM dashboard_data
               WHERE aid = $1 AND metric_key = $2
               ORDER BY recorded_at DESC
               LIMIT 100
           )"#,
    )
    .bind(aid)
    .bind(&metric_key)
    .execute(&state.db)
    .await
    .ok();

    // ── Check for dashboard-triggered workflows ──
    // If any active workflow has trigger_type='dashboard_data' and
    // trigger_config->>'metric_key' matches this dashboard_type,
    // automatically start a new workflow instance.
    tracing::info!(
        "Checking dashboard triggers for aid={} metric_key={}",
        aid.to_string(),
        metric_key
    );
    let matching_workflows: Vec<Uuid> = match sqlx::query_as::<_, (Uuid,)>(
        r#"SELECT id FROM workflows
           WHERE aid = $1
             AND is_active = true
             AND trigger_type = 'dashboard_data'
             AND trigger_config->>'metric_key' = $2"#,
    )
    .bind(aid)
    .bind(&metric_key)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => {
            let ids: Vec<Uuid> = rows.into_iter().map(|r| r.0).collect();
            tracing::info!(
                "Found {} matching workflows for trigger metric_key={}",
                ids.len(),
                metric_key
            );
            ids
        }
        Err(e) => {
            tracing::error!(
                "Dashboard trigger query for aid={} metric_key={} failed: {}",
                aid.to_string(),
                metric_key,
                e
            );
            Vec::new()
        }
    };

    let mut triggered_ids: Vec<String> = Vec::new();
    for wf_id in matching_workflows {
        // Create a simple trigger payload
        let trigger_payload = serde_json::json!({
            "data": data,
            "source": "dashboard_trigger",
            "metric_key": metric_key,
        });

        // Create a workflow instance
        let instance_id = Uuid::new_v4();
        let placeholder_client_id = Uuid::new_v4();
        let instance_result = sqlx::query(
            r#"INSERT INTO workflow_instances (id, workflow_id, client_id, aid, name, status, current_step_order, context)
               VALUES ($1, $2, $3, $4, $5, 'active', 0, $6::jsonb)"#
        )
        .bind(instance_id)
        .bind(wf_id)
        .bind(placeholder_client_id)
        .bind(aid)
        .bind(format!("Auto-triggered from dashboard: {}", dashboard_type))
        .bind(&trigger_payload)
        .execute(&state.db)
        .await;

        if instance_result.is_ok() {
            triggered_ids.push(instance_id.to_string());
        }
    }

    let new_balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "stored": true,
        "dashboard_id": dashboard_id.to_string(),
        "data_id": data_id.to_string(),
        "dashboard_type": dashboard_type,
        "credits_used": DASHBOARD_DATA_COST,
        "balance": new_balance,
        "triggered_workflows": triggered_ids,
        "message": "Dashboard data stored successfully"
    })))
}
