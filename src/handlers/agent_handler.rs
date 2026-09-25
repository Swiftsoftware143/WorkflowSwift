//! Agent Handler — Paperclip agent profiles, and the per-workspace dashboard counts.

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

// ── Updated Paperclip Dashboard — fix timeline SQL ────────────────

/// Updated workspace dashboard with agent + ticket counts
pub async fn workspace_dashboard(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<AgentQuery>,
) -> Result<impl IntoResponse, AppError> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let (ws_clause, ws_bind): (String, Option<Uuid>) = if let Some(ref ws) = q.workspace_id {
        let ws_id =
            Uuid::parse_str(ws).map_err(|_| AppError::BadRequest("Invalid workspace_id".into()))?;
        ("AND portfolio_company_id = $2".into(), Some(ws_id))
    } else {
        (String::new(), None)
    };

    let active_instances: Vec<serde_json::Value> = if let Some(ref ws_id) = ws_bind {
        sqlx::query_as::<_, (Uuid, String, String, String)>(
            &format!("SELECT wi.id, COALESCE(w.name,'unnamed') as name, wi.status, wi.started_at::text FROM workflow_instances wi LEFT JOIN workflows w ON w.id = wi.workflow_id WHERE wi.aid = $1 {} AND wi.status IN ('running','in_progress','pending') ORDER BY wi.started_at DESC LIMIT 20", ws_clause)
        ).bind(aid).bind(ws_id).fetch_all(&s.db).await.map_err(|e| {
            eprintln!("SQL error in active_instances: {}", e);
            AppError::Internal("Query error".into())
        }).unwrap_or_default().into_iter().map(|r| json!({"id": r.0, "name": r.1, "status": r.2, "started_at": r.3})).collect()
    } else {
        sqlx::query_as::<_, (Uuid, String, String, String)>(
            "SELECT wi.id, COALESCE(w.name,'unnamed') as name, wi.status, wi.started_at::text FROM workflow_instances wi LEFT JOIN workflows w ON w.id = wi.workflow_id WHERE wi.aid = $1 AND wi.status IN ('running','in_progress','pending') ORDER BY wi.started_at DESC LIMIT 20"
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
        let ws_id =
            Uuid::parse_str(ws).map_err(|_| AppError::BadRequest("Invalid workspace_id".into()))?;
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

/// Fetch and decrypt a provider API key for an account (optionally scoped to a
/// portfolio company).
///
/// The column stores ciphertext at rest; decryption goes through the shared at-rest helper
/// so the app has exactly one stored format (`enc:v1:` + base64, AES-256).
pub async fn get_decrypted_key(
    pool: &sqlx::PgPool,
    aid: Uuid,
    provider: &str,
    ws_id: Option<Uuid>,
) -> Result<Option<String>, AppError> {
    let stored: Option<String> = if let Some(ws) = ws_id {
        sqlx::query_scalar(
            "SELECT api_key FROM provider_keys
             WHERE aid = $1 AND provider = $2 AND portfolio_company_id = $3 AND is_active = true",
        )
        .bind(aid)
        .bind(provider)
        .bind(ws)
        .fetch_optional(pool)
        .await?
    } else {
        sqlx::query_scalar(
            "SELECT api_key FROM provider_keys
             WHERE aid = $1 AND provider = $2 AND portfolio_company_id IS NULL AND is_active = true",
        )
        .bind(aid)
        .bind(provider)
        .fetch_optional(pool)
        .await?
    };

    match stored {
        Some(s) => Ok(Some(
            crate::security::provider_key_crypto::decrypt_from_storage(pool, &s).await?,
        )),
        None => Ok(None),
    }
}
