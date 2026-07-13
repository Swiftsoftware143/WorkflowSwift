use axum::{
    extract::{State, Json},
    http::HeaderMap,
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;
use sqlx::Row;

use crate::AppState;
use crate::error::{AppError, ApiResult};
/// POST /api/v1/internal/dashboard-data-seed
/// Called by n8n on a schedule to generate dashboard data for all accounts.
/// Requires X-Internal-Key header matching the configured internal_sync_key.
pub async fn seed_dashboard_data(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let key = headers.get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if key != state.config.internal_sync_key {
        return Err(AppError::Forbidden("Invalid internal key".into()));
    }

    // Get all active accounts with their primary industry
    let accounts = sqlx::query(
        r#"SELECT t.id, t.industry_slug
           FROM accounts t
           WHERE t.is_active = true"#
    )
    .fetch_all(&state.db)
    .await?;

    let mut results = Vec::new();

    for row in &accounts {
        let aid: Uuid = row.try_get("id").unwrap_or_default();
        let industry_slug: String = row.try_get("industry_slug").unwrap_or_else(|_| "site-flipping".to_string());

        // Get the metric keys for this account's dashboard widgets
        let metric_keys: Vec<String> = sqlx::query_scalar(
            r#"SELECT DISTINCT config->>'metric_key'
               FROM dashboard_widgets dw
               JOIN dashboards d ON d.id = dw.dashboard_id
               JOIN account_industries ti ON ti.dashboard_id = d.id
               WHERE ti.aid = $1 AND ti.industry_slug = $2
               AND config->>'metric_key' IS NOT NULL"#
        )
        .bind(aid)
        .bind(&industry_slug)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        for metric_key in &metric_keys {
            // Generate sample data based on metric key patterns
            let value = generate_sample_data(metric_key);

            sqlx::query(
                r#"INSERT INTO dashboard_data (id, dashboard_id, aid, metric_key, metric_value)
                   SELECT $1, d.id, $2, $3, $4::jsonb
                   FROM dashboards d
                   JOIN account_industries ti ON ti.dashboard_id = d.id
                   WHERE ti.aid = $2 AND ti.industry_slug = $5
                   ORDER BY d.created_at DESC
                   LIMIT 1"#
            )
            .bind(Uuid::new_v4())
            .bind(aid)
            .bind(format!("n8n_{}", metric_key))
            .bind(&value)
            .bind(&industry_slug)
            .execute(&state.db)
            .await
            .ok();

            results.push(json!({
                "aid": aid.to_string(),
                "metric_key": metric_key,
                "industry": &industry_slug
            }));
        }
    }

    Ok(Json(json!({
        "seeded": true,
        "accounts_processed": accounts.len(),
        "metrics_seeded": results.len(),
        "details": results
    })))
}

fn generate_sample_data(metric_key: &str) -> serde_json::Value {
    use rand::Rng;
    let mut rng = rand::thread_rng();

    // Generate realistic data based on metric key patterns
    if metric_key.contains("revenue") || metric_key.contains("sales")
        || metric_key.contains("funding") || metric_key.contains("billings")
        || metric_key.contains("awarded") || metric_key.contains("portfolio") {
        json!({"value": rng.gen_range(50000..500000), "currency": "USD"})
    } else if metric_key.contains("rate") || metric_key.contains("success") {
        json!({"value": rng.gen_range(15..85), "suffix": "%"})
    } else if metric_key.contains("trends") || metric_key.contains("chart") {
        json!({
            "points": (0..7).map(|i| json!({
                "label": format!("Day {}", i+1),
                "value": rng.gen_range(20..100)
            })).collect::<Vec<_>>()
        })
    } else if metric_key.contains("activity") || metric_key.contains("recent") {
        let actions = ["New deal closed", "Lead converted", "Email sent", "Task completed",
            "Milestone reached", "Contract signed", "Proposal submitted", "Follow-up scheduled"];
        json!({
            "activities": (0..5).map(|_| json!({
                "action": actions[rng.gen_range(0..actions.len())],
                "timestamp": chrono::Utc::now().to_rfc3339()
            })).collect::<Vec<_>>()
        })
    } else if metric_key.contains("abandoned") {
        json!({"value": rng.gen_range(3..25)})
    } else if metric_key.contains("avg_order") || metric_key.contains("avg") {
        json!({"value": rng.gen_range(100..2000), "currency": "USD"})
    } else {
        // Default: simple count
        json!({"value": rng.gen_range(5..200)})
    }
}
