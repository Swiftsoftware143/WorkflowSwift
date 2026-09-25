//! Agent Handler — Paperclip agent profiles (`list_agents` / `create_agent` / `delete_agent`).
//! The dashboard pages are served by `paperclip_handler` — see the note at the end of this
//! file for the two uncalled iterations that lived here and were deleted (kanban t_82bb3fc2).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::{error::AppError, AppState};

#[derive(Deserialize)]
pub struct AgentQuery {
    pub workspace_id: Option<String>,
}

// ── Agent Profiles ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    pub role: Option<String>,
    pub model: Option<String>,
    pub budget_credits: Option<i32>,
    pub workspace_id: Option<String>,
}

pub async fn list_agents(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<AgentQuery>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let agents = if let Some(ref ws) = q.workspace_id {
        let ws_id =
            Uuid::parse_str(ws).map_err(|_| AppError::BadRequest("Invalid workspace_id".into()))?;
        sqlx::query_as::<_, (Uuid, String, String, Option<String>, Option<i32>, Option<i32>, String)>(
            "SELECT id, name, role, model, budget_credits, credits_spent, status FROM agent_profiles WHERE aid = $1 AND portfolio_company_id = $2 ORDER BY created_at"
        ).bind(aid).bind(ws_id).fetch_all(&s.db).await?
    } else {
        sqlx::query_as::<_, (Uuid, String, String, Option<String>, Option<i32>, Option<i32>, String)>(
            "SELECT id, name, role, model, budget_credits, credits_spent, status FROM agent_profiles WHERE aid = $1 ORDER BY created_at"
        ).bind(aid).fetch_all(&s.db).await?
    };

    let result: Vec<serde_json::Value> = agents
        .into_iter()
        .map(|a| {
            json!({
                "id": a.0, "name": a.1, "role": a.2, "model": a.3,
                "budget": a.4, "spent": a.5, "status": a.6
            })
        })
        .collect();

    Ok(Json(json!({"agents": result})))
}

pub async fn create_agent(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateAgentRequest>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let id = Uuid::new_v4();

    let ws_id = req.workspace_id.and_then(|w| Uuid::parse_str(&w).ok());

    if let Some(ref ws) = ws_id {
        let owned: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM portfolio_companies WHERE id = $1 AND aid = $2)",
        )
        .bind(ws)
        .bind(aid)
        .fetch_one(&s.db)
        .await?;
        if !owned {
            return Err(AppError::NotFound("Workspace not found".into()));
        }
    }

    sqlx::query(
        "INSERT INTO agent_profiles (id, aid, portfolio_company_id, name, role, model, budget_credits) VALUES ($1,$2,$3,$4,$5,$6,$7)"
    ).bind(id).bind(aid).bind(ws_id).bind(&req.name)
    .bind(req.role.unwrap_or_else(|| "worker".into()))
    .bind(req.model).bind(req.budget_credits.unwrap_or(0))
    .execute(&s.db).await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({"id": id, "status": "created"})),
    ))
}

pub async fn delete_agent(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("DELETE FROM agent_profiles WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .execute(&s.db)
        .await?;
    Ok(Json(json!({"status": "deleted"})))
}

// NOTE (kanban t_82bb3fc2): two earlier iterations of the dashboard pages used to live here —
// `workspace_dashboard` and `activity_timeline` (the section header in this file called them
// "Updated Paperclip Dashboard — fix timeline SQL"). They are DELETED, not wired:
//   * NO route ever mounted them. `grep -rn 'agent_handler::' src/routes.rs` mounts only
//     list_agents / create_agent / delete_agent, and this file was the only place the two names
//     appeared anywhere under src/.
//   * No served surface called them: the two paths they would need, GET /api/v1/dashboard/workspace
//     and GET /api/v1/dashboard/timeline, are owned by the `paperclip_handler` twins that routes.rs
//     mounts, and the served SPA's own timeline panel calls /dashboard/timeline (paperclip).
//   * Those twins are a strict superset: they verify workspace ownership (404 for a workspace the
//     account does not own, instead of silently ignoring the filter), take `days`/`limit`, return
//     `total_automations`, and propagate query errors with `?` instead of swallowing them through
//     `unwrap_or_default()` into an empty 200.
// Wiring the pair would therefore have put a second, strictly worse implementation behind one page,
// which is why the disposition is DELETE (same class as t_fee9dc11 / get_decrypted_key). The live
// pages are paperclip_handler::{workspace_dashboard, activity_timeline}.
