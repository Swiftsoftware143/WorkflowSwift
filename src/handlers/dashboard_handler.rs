//! Dashboard handler — the account's own totals.
//!
//! Two siblings were DELETED here by kanban t_3a8ccd2a, both measured dead on the live binary:
//!
//! * `dashboard_activity` (`GET /api/v1/dashboard/activity`) read `audit_logs` — a table written by
//!   NOTHING in this app (0 rows, 0 INSERT sites, no DB trigger), so the route could only ever
//!   answer `{"activities":[]}`, and `/dashboard/timeline` already serves the account's activity.
//! * `push_dashboard_data` (`POST /api/v1/dashboard/data`) had no reachable caller: this app's own
//!   n8n generator (`n8n_converter`'s data-card node) posts `{metric_key, value}` to
//!   `/dashboard/push-widget-data`, the served SPA posts to the same sibling, the 55 live n8n
//!   workflows contain no reference to `dashboard/data`, the 12 legacy `n8n-templates/*.json` that
//!   do reference it POST to `/api/dashboard/data` — a path this router does not serve (measured
//!   404, direct and through the public edge) — and no workflow in the live DB uses
//!   `trigger_type='dashboard_data'`.
//!
//! Evidence and the live A/B: /opt/swift/audits/t_3a8ccd2a/REPORT.md.

use axum::{extract::State, response::IntoResponse, Extension, Json};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::AppState;

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
