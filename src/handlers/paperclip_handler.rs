//! Paperclip Dashboard Handler
//! Visual automation tracking: active instances, tickets, automation stats, activity timeline

use axum::{
    extract::{Path, State, Query},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{AppState, error::AppError};
use crate::auth::models::Claims;

#[derive(Deserialize)]
pub struct DashboardQuery {
    pub workspace_id: Option<String>,
    pub days: Option<i32>,
    pub limit: Option<i64>,
}

/// GET /api/v1/dashboard/workspace
/// Returns active instances, recent tickets, automation stats, and counts.
pub async fn workspace_dashboard(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<DashboardQuery>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let days = q.days.unwrap_or(7);
    let limit = q.limit.unwrap_or(10);

    // Resolve workspace scope
    let ws_filter = if let Some(ref ws_id) = q.workspace_id {
        let ws_uuid = Uuid::parse_str(ws_id).map_err(|_| AppError::BadRequest("Invalid workspace_id".into()))?;
        // Verify ownership
        let owned: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM portfolio_companies WHERE id = $1 AND aid = $2)"
        )
        .bind(ws_uuid).bind(aid)
        .fetch_one(&s.db).await?;
        if !owned { return Err(AppError::NotFound("Workspace not found".into())); }
        Some(ws_uuid)
    } else {
        None
    };

    // Collect stats
    let active_instances: Vec<serde_json::Value> = if let Some(ws_id) = ws_filter {
        sqlx::query_as::<_, (Uuid, String, String, String, String)>(
            r#"SELECT wi.id, w.name, wi.status, wi.started_at::text, COALESCE(wi.updated_at, wi.started_at)::text
               FROM workflow_instances wi
               JOIN workflows w ON w.id = wi.workflow_id
               WHERE wi.aid = $1 AND wi.portfolio_company_id = $2
                 AND wi.status IN ('running', 'pending') AND wi.started_at > NOW() - make_interval(days => $3)
               ORDER BY wi.started_at DESC LIMIT $4"#
        )
        .bind(aid).bind(ws_id).bind(days).bind(limit)
        .fetch_all(&s.db).await?
        .into_iter().map(|r| json!({
            "id": r.0, "workflow_name": r.1, "status": r.2,
            "started_at": r.3, "updated_at": r.4
        })).collect()
    } else {
        sqlx::query_as::<_, (Uuid, String, String, String, String)>(
            r#"SELECT wi.id, w.name, wi.status, wi.started_at::text, COALESCE(wi.updated_at, wi.started_at)::text
               FROM workflow_instances wi
               JOIN workflows w ON w.id = wi.workflow_id
               WHERE wi.aid = $1
                 AND wi.status IN ('running', 'pending') AND wi.started_at > NOW() - make_interval(days => $2)
               ORDER BY wi.started_at DESC LIMIT $3"#
        )
        .bind(aid).bind(days).bind(limit)
        .fetch_all(&s.db).await?
        .into_iter().map(|r| json!({
            "id": r.0, "workflow_name": r.1, "status": r.2,
            "started_at": r.3, "updated_at": r.4
        })).collect()
    };

    // Automation stats
    let total_automations: i64 = if let Some(ws_id) = ws_filter {
        sqlx::query_scalar("SELECT COUNT(*) FROM automations WHERE aid = $1 AND portfolio_company_id = $2")
            .bind(aid).bind(ws_id).fetch_one(&s.db).await.unwrap_or(0)
    } else {
        sqlx::query_scalar("SELECT COUNT(*) FROM automations WHERE aid = $1")
            .bind(aid).fetch_one(&s.db).await.unwrap_or(0)
    };

    Ok(Json(json!({
        "dashboard": {
            "active_instances": active_instances,
            "active_count": active_instances.len(),
            "total_automations": total_automations,
        }
    })))
}

/// GET /api/v1/dashboard/timeline
/// Returns a flat activity timeline across workflows, automation runs, and tickets.
pub async fn activity_timeline(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<DashboardQuery>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let days = q.days.unwrap_or(30);
    let limit = q.limit.unwrap_or(20);

    let ws_filter = q.workspace_id
        .and_then(|id| Uuid::parse_str(&id).ok());

    let events = if let Some(ws_id) = ws_filter {
        sqlx::query_as::<_, (Uuid, String, String, String, String)>(
            r#"SELECT wi.id, 'instance'::text as event_type, w.name as title,
                      wi.started_at::text, wi.status as description
               FROM workflow_instances wi
               JOIN workflows w ON w.id = wi.workflow_id
               WHERE wi.aid = $1 AND wi.portfolio_company_id = $2
                 AND wi.started_at > NOW() - make_interval(days => $3)
               ORDER BY wi.started_at DESC LIMIT $4"#
        )
        .bind(aid).bind(ws_id).bind(days).bind(limit)
        .fetch_all(&s.db).await?
    } else {
        sqlx::query_as::<_, (Uuid, String, String, String, String)>(
            r#"SELECT wi.id, 'instance'::text as event_type, w.name as title,
                      wi.started_at::text, wi.status as description
               FROM workflow_instances wi
               JOIN workflows w ON w.id = wi.workflow_id
               WHERE wi.aid = $1
                 AND wi.started_at > NOW() - make_interval(days => $2)
               ORDER BY wi.started_at DESC LIMIT $3"#
        )
        .bind(aid).bind(days).bind(limit)
        .fetch_all(&s.db).await?
    };

    let timeline: Vec<serde_json::Value> = events.into_iter().map(|e| json!({
        "id": e.0, "type": e.1, "title": e.2,
        "timestamp": e.3, "status": e.4
    })).collect();

    Ok(Json(json!({ "timeline": timeline })))
}
