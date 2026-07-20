//! Agent Handler — Paperclip agent profiles, tickets, kanban, budgets, and BYOK integrations

use axum::{
    extract::{Path, State, Query},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{AppState, error::AppError};
use crate::auth::models::Claims;

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
        let ws_id = Uuid::parse_str(ws).map_err(|_| AppError::BadRequest("Invalid workspace_id".into()))?;
        sqlx::query_as::<_, (Uuid, String, String, Option<String>, i32, i32, String)>(
            "SELECT id, name, role, model, budget_credits, credits_spent, status FROM agent_profiles WHERE aid = $1 AND portfolio_company_id = $2 ORDER BY created_at"
        ).bind(aid).bind(ws_id).fetch_all(&s.db).await?
    } else {
        sqlx::query_as::<_, (Uuid, String, String, Option<String>, i32, i32, String)>(
            "SELECT id, name, role, model, budget_credits, credits_spent, status FROM agent_profiles WHERE aid = $1 ORDER BY created_at"
        ).bind(aid).fetch_all(&s.db).await?
    };

    let result: Vec<serde_json::Value> = agents.into_iter().map(|a| json!({
        "id": a.0, "name": a.1, "role": a.2, "model": a.3,
        "budget": a.4, "spent": a.5, "status": a.6
    })).collect();

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
        let owned: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM portfolio_companies WHERE id = $1 AND aid = $2)")
            .bind(ws).bind(aid).fetch_one(&s.db).await?;
        if !owned { return Err(AppError::NotFound("Workspace not found".into())); }
    }

    sqlx::query(
        "INSERT INTO agent_profiles (id, aid, portfolio_company_id, name, role, model, budget_credits) VALUES ($1,$2,$3,$4,$5,$6,$7)"
    ).bind(id).bind(aid).bind(ws_id).bind(&req.name)
    .bind(req.role.unwrap_or_else(|| "worker".into()))
    .bind(req.model).bind(req.budget_credits.unwrap_or(0))
    .execute(&s.db).await?;

    Ok((StatusCode::CREATED, Json(json!({"id": id, "status": "created"}))))
}

// ── Agent Tickets (Kanban) ─────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateTicketRequest {
    pub agent_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub workspace_id: Option<String>,
}

pub async fn list_tickets(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<AgentQuery>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let tickets = if let Some(ref ws) = q.workspace_id {
        let ws_id = Uuid::parse_str(ws).map_err(|_| AppError::BadRequest("Invalid workspace_id".into()))?;
        sqlx::query_as::<_, (Uuid, Option<Uuid>, String, Option<String>, String, String, Option<String>, i32, chrono::DateTime<chrono::Utc>)>(
            "SELECT id, agent_id, title, description, status, priority, assigned_to, budget_credits, created_at FROM agent_tickets WHERE aid = $1 AND portfolio_company_id = $2 ORDER BY created_at DESC"
        ).bind(aid).bind(ws_id).fetch_all(&s.db).await?
    } else {
        sqlx::query_as::<_, (Uuid, Option<Uuid>, String, Option<String>, String, String, Option<String>, i32, chrono::DateTime<chrono::Utc>)>(
            "SELECT id, agent_id, title, description, status, priority, assigned_to, budget_credits, created_at FROM agent_tickets WHERE aid = $1 ORDER BY created_at DESC"
        ).bind(aid).fetch_all(&s.db).await?
    };

    let result: Vec<serde_json::Value> = tickets.into_iter().map(|t| json!({
        "id": t.0, "agent_id": t.1, "title": t.2, "description": t.3,
        "status": t.4, "priority": t.5, "assigned_to": t.6,
        "budget": t.7, "created_at": t.8
    })).collect();

    Ok(Json(json!({"tickets": result})))
}

pub async fn update_ticket_status(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let new_status = body.get("status").and_then(|v| v.as_str()).ok_or_else(|| AppError::BadRequest("status required".into()))?;

    sqlx::query("UPDATE agent_tickets SET status = $1, updated_at = NOW() WHERE id = $2 AND aid = $3")
        .bind(new_status).bind(id).bind(aid).execute(&s.db).await?;

    Ok(Json(json!({"status": "updated"})))
}

// ── Updated Paperclip Dashboard — fix timeline SQL ────────────────

/// Updated workspace dashboard with agent + ticket counts
pub async fn workspace_dashboard(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<AgentQuery>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let (ws_clause, ws_bind): (String, Option<Uuid>) = if let Some(ref ws) = q.workspace_id {
        let ws_id = Uuid::parse_str(ws).map_err(|_| AppError::BadRequest("Invalid workspace_id".into()))?;
        ("AND portfolio_company_id = $2".into(), Some(ws_id))
    } else {
        (String::new(), None)
    };

    let active_instances: Vec<serde_json::Value> = if let Some(ref ws_id) = ws_bind {
        sqlx::query_as::<_, (Uuid, String, String, String)>(
            &format!("SELECT wi.id, COALESCE(w.name,'unnamed') as name, wi.status, wi.started_at::text FROM workflow_instances wi LEFT JOIN workflows w ON w.id = wi.workflow_id WHERE wi.aid = $1 {} AND wi.status IN ('running','pending') ORDER BY wi.started_at DESC LIMIT 20", ws_clause)
        ).bind(aid).bind(ws_id).fetch_all(&s.db).await.map_err(|e| {
            eprintln!("SQL error in active_instances: {}", e);
            AppError::Internal("Query error".into())
        }).unwrap_or_default().into_iter().map(|r| json!({"id": r.0, "name": r.1, "status": r.2, "started_at": r.3})).collect()
    } else {
        sqlx::query_as::<_, (Uuid, String, String, String)>(
            "SELECT wi.id, COALESCE(w.name,'unnamed') as name, wi.status, wi.started_at::text FROM workflow_instances wi LEFT JOIN workflows w ON w.id = wi.workflow_id WHERE wi.aid = $1 AND wi.status IN ('running','pending') ORDER BY wi.started_at DESC LIMIT 20"
        ).bind(aid).fetch_all(&s.db).await.map_err(|e| {
            eprintln!("SQL error in active_instances: {}", e);
            AppError::Internal("Query error".into())
        }).unwrap_or_default().into_iter().map(|r| json!({"id": r.0, "name": r.1, "status": r.2, "started_at": r.3})).collect()
    };

    Ok(Json(json!({
        "dashboard": {
            "active_instances": active_instances,
            "active_count": active_instances.len(),
        }
    })))
}

/// Fixed timeline — no SQL bug
pub async fn activity_timeline(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<AgentQuery>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let (ws_clause, ws_bind): (String, Option<Uuid>) = if let Some(ref ws) = q.workspace_id {
        let ws_id = Uuid::parse_str(ws).map_err(|_| AppError::BadRequest("Invalid workspace_id".into()))?;
        ("AND wi.portfolio_company_id = $2".into(), Some(ws_id))
    } else {
        (String::new(), None)
    };

    let events: Vec<serde_json::Value> = if let Some(ref ws_id) = ws_bind {
        sqlx::query_as::<_, (Uuid, String, String, String)>(
            &format!("SELECT wi.id, COALESCE(w.name,'workflow') as title, wi.status as description, wi.started_at::text as ts FROM workflow_instances wi LEFT JOIN workflows w ON w.id = wi.workflow_id WHERE wi.aid = $1 {} ORDER BY wi.started_at DESC LIMIT 30", ws_clause)
        ).bind(aid).bind(ws_id).fetch_all(&s.db).await.unwrap_or_default().into_iter().map(|r| json!({"id": r.0, "title": r.1, "description": r.2, "timestamp": r.3, "type": "instance"})).collect()
    } else {
        sqlx::query_as::<_, (Uuid, String, String, String)>(
            "SELECT wi.id, COALESCE(w.name,'workflow') as title, wi.status as description, wi.started_at::text as ts FROM workflow_instances wi LEFT JOIN workflows w ON w.id = wi.workflow_id WHERE wi.aid = $1 ORDER BY wi.started_at DESC LIMIT 30"
        ).bind(aid).fetch_all(&s.db).await.unwrap_or_default().into_iter().map(|r| json!({"id": r.0, "title": r.1, "description": r.2, "timestamp": r.3, "type": "instance"})).collect()
    };

    Ok(Json(json!({"timeline": events})))
}

// ── BYOK Integrations (Provider Keys per Workspace) ───────────────

#[derive(Deserialize)]
pub struct UpsertProviderKeyRequest {
    pub provider: String,
    pub api_key: String, // stored in metadata
    pub workspace_id: Option<String>,
}

pub async fn list_provider_keys(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<AgentQuery>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let keys = if let Some(ref ws) = q.workspace_id {
        let ws_id = Uuid::parse_str(ws).map_err(|_| AppError::BadRequest("Invalid workspace_id".into()))?;
        sqlx::query_as::<_, (String, String, bool)>(
            "SELECT provider, COALESCE(label, provider), is_active FROM provider_keys WHERE aid = $1 AND portfolio_company_id = $2 ORDER BY provider"
        ).bind(aid).bind(ws_id).fetch_all(&s.db).await?
    } else {
        sqlx::query_as::<_, (String, String, bool)>(
            "SELECT provider, COALESCE(label, provider), is_active FROM provider_keys WHERE aid = $1 ORDER BY provider"
        ).bind(aid).fetch_all(&s.db).await?
    };

    let result: Vec<serde_json::Value> = keys.into_iter().map(|k| json!({
        "provider": k.0, "label": k.1, "configured": true, "active": k.2
    })).collect();

    Ok(Json(json!({"provider_keys": result})))
}

pub async fn delete_agent(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("DELETE FROM agent_profiles WHERE id = $1 AND aid = $2").bind(id).bind(aid).execute(&s.db).await?;
    Ok(Json(json!({"status": "deleted"})))
}

pub async fn upsert_provider_key(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<UpsertProviderKeyRequest>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let ws_id = req.workspace_id.and_then(|w| Uuid::parse_str(&w).ok());

    // Delete and re-insert for clean upsert
    let ws_uuid = ws_id;
    sqlx::query(
        "DELETE FROM provider_keys WHERE aid = $1 AND provider = $2 AND (portfolio_company_id = $3 OR ($3 IS NULL AND portfolio_company_id IS NULL))"
    )
    .bind(aid).bind(&req.provider).bind(ws_uuid)
    .execute(&s.db).await?;
    sqlx::query(
        "INSERT INTO provider_keys (id, aid, portfolio_company_id, provider, api_key, is_active)
         VALUES ($1,$2,$3,$4,$5,true)"
    )
    .bind(Uuid::new_v4()).bind(aid).bind(ws_uuid)
    .bind(&req.provider).bind(&req.api_key)
    .execute(&s.db).await?;

    Ok(Json(json!({"status": "saved", "provider": req.provider, "configured": true})))
}

pub async fn delete_provider_key(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(provider): Path<String>,
    Query(q): Query<AgentQuery>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    if let Some(ref ws) = q.workspace_id {
        let ws_id = Uuid::parse_str(ws).map_err(|_| AppError::BadRequest("Invalid workspace_id".into()))?;
        sqlx::query("DELETE FROM provider_keys WHERE aid = $1 AND provider = $2 AND portfolio_company_id = $3")
            .bind(aid).bind(&provider).bind(ws_id).execute(&s.db).await?;
    } else {
        sqlx::query("DELETE FROM provider_keys WHERE aid = $1 AND provider = $2 AND portfolio_company_id IS NULL")
            .bind(aid).bind(&provider).execute(&s.db).await?;
    }

    Ok(Json(json!({"status": "deleted"})))
}
