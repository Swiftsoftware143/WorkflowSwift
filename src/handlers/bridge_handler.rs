use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::AppState;

pub async fn ingest_data(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let source = req
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("extension")
        .to_string();

    let payload = req
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    let log_entry = sqlx::query_as::<_, IngestLogEntry>(
        r#"INSERT INTO extension_ingest_log (id, aid, source, payload, status)
           VALUES ($1, $2, $3, $4::jsonb, 'received')
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(&source)
    .bind(&payload)
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"ingest_log": log_entry}))))
}

pub async fn get_commands(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let commands = sqlx::query_as::<_, ExtensionCommand>(
        r#"SELECT id, tenant_id AS aid, command, payload, status, delivered_at, created_at
           FROM extension_commands
           WHERE tenant_id = $1 AND status = 'pending'
           ORDER BY created_at ASC"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    // Mark commands as delivered
    for cmd in &commands {
        sqlx::query("UPDATE extension_commands SET status = 'delivered', delivered_at = NOW() WHERE id = $1")
            .bind(cmd.id)
            .execute(&state.db)
            .await?;
    }

    Ok(Json(json!({"commands": commands})))
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
struct IngestLogEntry {
    id: Uuid,
    aid: Uuid,
    source: String,
    payload: serde_json::Value,
    status: String,
    error: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
struct ExtensionCommand {
    id: Uuid,
    aid: Uuid,
    command: String,
    payload: serde_json::Value,
    status: String,
    delivered_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

/// GET /api/v1/bridge/inbound — list inbound task files
pub async fn list_inbound_tasks(State(_s): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let inbound_dir = std::path::PathBuf::from("/opt/ai-bridge/inbound");
    let mut tasks = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&inbound_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                        tasks.push(data);
                    }
                }
            }
        }
    }
    Ok(Json(json!({"tasks": tasks})))
}

/// GET /api/v1/bridge/outbound — list outbound result files
pub async fn list_outbound_results(
    State(_s): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let outbound_dir = std::path::PathBuf::from("/opt/ai-bridge/outbound");
    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&outbound_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                        results.push(data);
                    }
                }
            }
        }
    }
    Ok(Json(json!({"results": results})))
}

/// Minimal test — returns 200 with empty results
pub async fn ping_bridge(State(_s): State<AppState>) -> Result<impl IntoResponse, AppError> {
    Ok(Json(json!({"status": "bridge-ok"})))
}

/// `GET /bridge/status` — the endpoint the shipped Chrome extension's
/// "Test Connection" button calls (options.js). Requires a valid credential and
/// echoes back which account the credential resolved to, so the caller can see
/// the key was accepted *as* a specific tenant.
pub async fn bridge_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let account: Option<String> =
        sqlx::query_scalar("SELECT account_slug FROM accounts WHERE id = $1")
            .bind(aid)
            .fetch_optional(&state.db)
            .await?
            .flatten();

    Ok(Json(json!({
        "server": "WorkflowSwift",
        "status": "ok",
        "account_id": claims.aid,
        "account_slug": account,
        "user_id": claims.sub,
        "auth": if claims.role == "api_key" { "api_key" } else { "jwt" },
    })))
}

/// `POST /bridge/commands/ack` — the endpoint the shipped Chrome extension's
/// `acknowledgeCommand()` posts to after it has executed a command it polled from
/// `GET /bridge/commands`.
///
/// Shaped exactly like its siblings: the account comes from the authenticated claims
/// (`aid`), never from the body, and the UPDATE carries `AND tenant_id = $4`, so a key
/// belonging to tenant A cannot acknowledge — or even learn the existence of — a
/// command owned by tenant B. An unknown / foreign command id is a 404 and leaves the
/// other tenant's row untouched.
///
/// Body: `{"command_id": "<uuid>", "status": "completed"|"failed", "result": {...}}`.
/// The shipped client always sends `status`; when it is omitted we default to
/// `completed`, and anything other than the two known values is a 400 rather than a
/// junk status silently stored in the table.
pub async fn acknowledge_command(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AckRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let requested = req
        .status
        .as_deref()
        .unwrap_or("completed")
        .trim()
        .to_ascii_lowercase();
    let status = match requested.as_str() {
        "completed" => "completed",
        "failed" => "failed",
        other => {
            return Err(AppError::BadRequest(format!(
                "invalid status '{other}': expected 'completed' or 'failed'"
            )))
        }
    };

    let updated = sqlx::query_as::<_, AcknowledgedCommand>(
        r#"UPDATE extension_commands
              SET status = $1,
                  result = COALESCE($2::jsonb, result),
                  acknowledged_at = NOW()
            WHERE id = $3 AND tenant_id = $4
        RETURNING id, tenant_id AS aid, command, payload, status,
                  delivered_at, acknowledged_at, result, created_at"#,
    )
    .bind(status)
    .bind(req.result.clone())
    .bind(req.command_id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?;

    // No row matched: either the id does not exist at all or it belongs to another
    // tenant. Both are the same 404 — we never confirm a foreign id exists.
    let Some(row) = updated else {
        return Err(AppError::NotFound(format!(
            "command {} is not known to this account",
            req.command_id
        )));
    };

    let mut body = serde_json::to_value(&row)
        .map_err(|e| AppError::Internal(format!("serialize acknowledged command: {}", e)))?;
    if let Some(map) = body.as_object_mut() {
        map.insert("acknowledged".to_string(), json!(true));
    }

    Ok(Json(body))
}

/// Request body of `POST /bridge/commands/ack`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckRequest {
    pub command_id: Uuid,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
}

/// The acknowledged row as this endpoint returns it. Deliberately a separate struct
/// from `ExtensionCommand` so the poll path's `SELECT` list is untouched by this
/// change.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AcknowledgedCommand {
    pub id: Uuid,
    pub aid: Uuid,
    pub command: String,
    pub payload: serde_json::Value,
    pub status: String,
    pub delivered_at: Option<DateTime<Utc>>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub result: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}
