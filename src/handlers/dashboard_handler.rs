use axum::{
    extract::{State, Json},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;

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

    let total_workflows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflows WHERE aid = $1")
        .bind(aid)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    let active_instances: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_instances WHERE aid = $1 AND status NOT IN ('completed', 'cancelled')")
        .bind(aid)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    let total_clients: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM clients WHERE aid = $1 AND is_active = true")
        .bind(aid)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    let total_templates: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_templates WHERE aid = $1")
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
    let activities: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<&str, _>("id").unwrap_or(""),
            "user_id": r.try_get::<&str, _>("user_id").unwrap_or(""),
            "action": r.try_get::<&str, _>("action").unwrap_or(""),
            "entity_type": r.try_get::<&str, _>("entity_type").unwrap_or(""),
            "entity_id": r.try_get::<&str, _>("entity_id").unwrap_or(""),
            "created_at": r.try_get::<&str, _>("created_at").unwrap_or(""),
        })
    }).collect();

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
    let dashboard_type = req.get("dashboard_type")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let data = req.get("data")
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
    let dashboard_id: Uuid = match sqlx::query_scalar(
        "SELECT id FROM dashboards WHERE aid = $1 AND name = $2 LIMIT 1",
    )
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

    // Store the data
    let data_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO dashboard_data (id, dashboard_id, aid, metric_key, metric_value)
           VALUES ($1, $2, $3, $4, $5::jsonb)"#,
    )
    .bind(data_id)
    .bind(dashboard_id)
    .bind(aid)
    .bind(format!("n8n_{}", dashboard_type))
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
    .bind(format!("n8n_{}", dashboard_type))
    .execute(&state.db)
    .await
    .ok();

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
        "message": "Dashboard data stored successfully"
    })))
}

/// GET /api/v1/dashboard/industry-data — get dashboard data filtered by industry
pub async fn industry_dashboard_data(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    
    // Get account's industry
    let industry_slug: Option<String> = sqlx::query_scalar(
        "SELECT industry_slug FROM accounts WHERE id = $1"
    )
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .flatten();
    
    // Get widgets for this industry
    type WidgetRow = (Uuid, String, String, String, i32);
    let widgets = if let Some(ref slug) = industry_slug {
        sqlx::query_as::<_, WidgetRow>(
            "SELECT id, name, data_type, default_config, sort_order FROM industry_widgets WHERE industry_slug = $1 ORDER BY sort_order"
        )
        .bind(slug)
        .fetch_all(&state.db)
        .await?
    } else {
        vec![]
    };
    
    // Get dashboard data for each widget
    let mut widget_data = Vec::new();
    for (widget_id, name, data_type, default_config, sort_order) in &widgets {
        let data: Vec<serde_json::Value> = sqlx::query_as::<_, (serde_json::Value,)>(
            "SELECT data_value FROM dashboard_data WHERE aid = $1 AND metric_key = $2 ORDER BY recorded_at DESC LIMIT 1"
        )
        .bind(aid)
        .bind(name)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|r| r.0)
        .collect();
        
        widget_data.push(json!({
            "id": widget_id,
            "name": name,
            "type": data_type,
            "config": default_config,
            "sort_order": sort_order,
            "data": data.first().unwrap_or(&json!(null))
        }));
    }
    
    Ok(Json(json!({
        "industry": industry_slug,
        "widgets": widget_data
    })))
}

/// GET /api/v1/dashboard/metric-keys — return the authenticated user's widget metric keys
/// for the frontend data entry dropdown. Queries dashboard_widgets joined with dashboards
/// joined with account_industries for the user's account.
pub async fn get_widget_metric_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    type MetricKeyRow = (String, Option<String>, Option<String>);
    let rows = sqlx::query_as::<_, MetricKeyRow>(
        r#"SELECT DISTINCT dw.title, dw.config->>'metric_key', dw.config->>'subtitle'
           FROM dashboard_widgets dw
           JOIN dashboards d ON d.id = dw.dashboard_id
           JOIN account_industries ti ON ti.dashboard_id = d.id
           WHERE ti.aid = $1 AND ti.is_active = true
             AND dw.config->>'metric_key' IS NOT NULL
           ORDER BY dw.title"#
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let metric_keys: Vec<serde_json::Value> = rows
        .into_iter()
        .filter_map(|(title, metric_key, subtitle)| {
            let key = metric_key?;
            Some(json!({
                "key": key,
                "title": title,
                "subtitle": subtitle.unwrap_or_default(),
            }))
        })
        .collect();

    Ok(Json(json!({"metric_keys": metric_keys})))
}
