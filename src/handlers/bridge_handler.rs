use axum::{
    extract::{State, Json},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::{json, Value};
use uuid::Uuid;

use std::path::PathBuf;
use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;

pub async fn ingest_data(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let source = req.get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("extension")
        .to_string();

    let payload = req.get("payload")
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

    Ok((
        StatusCode::CREATED,
        Json(json!({"ingest_log": log_entry})),
    ))
}

pub async fn get_commands(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let commands = sqlx::query_as::<_, ExtensionCommand>(
        r#"SELECT * FROM extension_commands
           WHERE aid = $1 AND status = 'pending'
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
pub async fn list_inbound_tasks(
    State(_s): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
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
pub async fn ping_bridge(
    State(_s): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    Ok(Json(json!({"status": "bridge-ok"})))
}
