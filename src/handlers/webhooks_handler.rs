//! Webhooks handler — real per-account outbound webhook registry.
//!
//! Replaces the auto-generated stub (kanban t_01fa9bbc) which read a `webhooks` relation that no
//! migration ever created, was not auth/tenant-scoped, and swallowed the missing-relation error —
//! GET answered an empty 200 list while POST/PUT/DELETE answered 500 `relation "webhooks" does not
//! exist`. The admin shell ships the screen (Communications -> Webhooks) and posts
//! `name` + `url` + `is_active`.
//! Storage: migrations/055_create_webhooks.sql. Mirrors the tickets/leads handler pattern.
//!
//! NB: this is the tenant's own registry of endpoints. It is NOT the inbound payment receivers —
//! `POST /api/v1/webhooks/stripe` and `POST /api/v1/webhooks/paypal` stay in the public router,
//! and their event log is `payment_webhook_events`.
use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, FromRow)]
pub struct Webhook {
    pub id: Uuid,
    pub aid: Uuid,
    pub name: String,
    pub url: String,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

const COLS: &str = "SELECT id, aid, name, url, is_active, created_at, updated_at FROM webhooks";

/// The admin shell posts every form field as a plain string (an empty field is omitted), so accept
/// a string and ignore anything else rather than 422-ing the whole request.
fn text_field(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
}

/// `is_active` arrives as a real boolean from an API client but as the string "true"/"false" from
/// the admin form; accept both rather than rejecting the request on shape.
fn flag_field(body: &Value, key: &str) -> Option<bool> {
    match body.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        Value::String(s) => {
            let s = s.trim().to_ascii_lowercase();
            if s.is_empty() {
                return None;
            }
            Some(!matches!(s.as_str(), "false" | "0" | "no" | "off"))
        }
        _ => None,
    }
}

fn valid_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn account_id(claims: &Claims) -> ApiResult<Uuid> {
    Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)
}

pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let rows = sqlx::query_as::<_, Webhook>(&format!("{COLS} WHERE aid = $1 ORDER BY name ASC"))
        .bind(aid)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(json!({ "items": rows, "count": rows.len() })))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let name = text_field(&body, "name").unwrap_or_default();
    if name.is_empty() {
        return Err(AppError::Validation("Webhook name is required".to_string()));
    }
    let url = text_field(&body, "url").unwrap_or_default();
    if !valid_url(&url) {
        return Err(AppError::Validation(
            "Webhook url must start with http:// or https://".to_string(),
        ));
    }
    let is_active = flag_field(&body, "is_active").unwrap_or(true);
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO webhooks (id, aid, name, url, is_active) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(aid)
        .bind(&name)
        .bind(&url)
        .bind(is_active)
        .execute(&state.db)
        .await?;
    let row = sqlx::query_as::<_, Webhook>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_one(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({ "item": row }))))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let row = sqlx::query_as::<_, Webhook>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Webhook not found".to_string()))?;
    Ok(Json(json!({ "item": row })))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let existing = sqlx::query_as::<_, Webhook>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Webhook not found".to_string()))?;
    // A blank field is "unset", not "clear it" — the admin form omits blanks for the same reason.
    let name = text_field(&body, "name")
        .filter(|s| !s.is_empty())
        .unwrap_or(existing.name);
    let url = text_field(&body, "url")
        .filter(|s| !s.is_empty())
        .unwrap_or(existing.url);
    if !valid_url(&url) {
        return Err(AppError::Validation(
            "Webhook url must start with http:// or https://".to_string(),
        ));
    }
    let is_active = flag_field(&body, "is_active").unwrap_or(existing.is_active);
    sqlx::query(
        "UPDATE webhooks SET name = $1, url = $2, is_active = $3, updated_at = NOW() WHERE id = $4 AND aid = $5",
    )
    .bind(&name)
    .bind(&url)
    .bind(is_active)
    .bind(id)
    .bind(aid)
    .execute(&state.db)
    .await?;
    let row = sqlx::query_as::<_, Webhook>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(json!({ "item": row })))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let result = sqlx::query("DELETE FROM webhooks WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Webhook not found".to_string()));
    }
    Ok(Json(json!({ "deleted": true })))
}
