//! Tickets handler — real tenant-scoped support tickets.
//! Replaces the auto-generated stub (which targeted a non-existent table and
//! was not auth/tenant-scoped). Mirrors the leads handler pattern.
use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, FromRow)]
pub struct Ticket {
    pub id: Uuid,
    pub aid: Uuid,
    pub subject: String,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub source: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
    #[serde(default)]
    pub page: i64,
    #[serde(default)]
    pub per_page: i64,
}

#[derive(Deserialize)]
pub struct CreateInput {
    pub subject: String,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub source: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateInput {
    pub subject: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
}

// Gate rule 5d / class 14 (kanban t_15a04e9d): the statement must be a COMPLETE compile-time
// literal, visible at the site. A `const` ident cannot be joined by `concat!`, but a `macro_rules!`
// that expands to a literal can — so the select list stays written ONCE here and every statement is
// still one literal at compile time.
macro_rules! cols {
    () => {
        "SELECT id, aid, subject, description, status, priority, source, created_at, updated_at FROM tickets"
    };
}

const BY_ID: &str = concat!(cols!(), " WHERE id = $1");
const BY_ID_AID: &str = concat!(cols!(), " WHERE id = $1 AND aid = $2");

pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListQuery>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let limit = if query.per_page > 0 {
        query.per_page
    } else {
        50
    };
    let offset = if query.page > 0 {
        (query.page - 1) * limit
    } else {
        0
    };
    let rows = if query.status.is_some() {
        sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE aid=$1 AND status=$2 ORDER BY created_at DESC LIMIT $3 OFFSET $4")
            .bind(aid).bind(&query.status).bind(limit).bind(offset)
            .fetch_all(&state.db).await?
    } else {
        sqlx::query_as::<_, Ticket>(
            "SELECT * FROM tickets WHERE aid=$1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(aid)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    };
    Ok(Json(json!({ "items": rows, "count": rows.len() })))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(b): Json<CreateInput>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    if b.subject.trim().is_empty() {
        return Err(AppError::Validation("subject is required".into()));
    }
    let id = Uuid::new_v4();
    let status = b.status.clone().unwrap_or_else(|| "open".to_string());
    let priority = b.priority.clone().unwrap_or_else(|| "medium".to_string());
    let source = b.source.clone().unwrap_or_else(|| "manual".to_string());
    // kanban t_zz-tickets — `tickets.description` is `text NOT NULL DEFAULT ''`, and this INSERT
    // names the column explicitly, so binding the Option straight through sent SQL NULL and the
    // column DEFAULT never applied: a ticket created WITHOUT a description — the console's own form
    // skips blank inputs, and `subject` is the only required field — answered
    // `500 {"code":500,"error":true,"message":"Database error"}` (measured live 2026-10-14; the same
    // body WITH a description answered 201). Bind the column's own default instead of NULL.
    let description = b.description.clone().unwrap_or_default();
    sqlx::query("INSERT INTO tickets (id, aid, subject, description, status, priority, source) VALUES ($1,$2,$3,$4,$5,$6,$7)")
        .bind(id).bind(aid).bind(&b.subject).bind(&description).bind(&status).bind(&priority).bind(&source)
        .execute(&state.db).await?;
    let row = sqlx::query_as::<_, Ticket>(BY_ID)
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({ "item": row }))))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let row = sqlx::query_as::<_, Ticket>(BY_ID_AID)
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Ticket not found".into()))?;
    Ok(Json(json!({ "item": row })))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateInput>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let cur = sqlx::query_as::<_, Ticket>(BY_ID_AID)
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Ticket not found".into()))?;
    sqlx::query("UPDATE tickets SET subject=COALESCE($2,subject), description=COALESCE($3,description), status=COALESCE($4,status), priority=COALESCE($5,priority), updated_at=NOW() WHERE id=$1 AND aid=$6")
        .bind(id)
        .bind(b.subject.as_ref().unwrap_or(&cur.subject))
        .bind(b.description.as_ref().or(cur.description.as_ref()))
        .bind(b.status.as_ref().or(cur.status.as_ref()))
        .bind(b.priority.as_ref().or(cur.priority.as_ref()))
        .bind(aid)
        .execute(&state.db).await?;
    let row = sqlx::query_as::<_, Ticket>(BY_ID)
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(json!({ "item": row })))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM tickets WHERE id=$1 AND aid=$2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("Ticket not found".into()));
    }
    Ok(Json(json!({ "deleted": true })))
}

// ── The OPERATOR's cross-tenant queue (2026-10-19) ────────────────────────────────────────────────
// The customer console files a plan request as a real ticket (`source='plan_request'`, the
// `requestPlan` button on the served console's Plans section). `list` above is TENANT-SCOPED
// (`WHERE aid = $1`), and the operator's own Support > Tickets panel calls exactly that route — so a
// request filed by a customer landed in that customer's workspace and was invisible to the operator
// unless he impersonated the workspace first. Measured live 2026-10-19: a customer filed "Plan
// request — Starter ($29/mo)" (201), and a super-admin session answered `GET /api/v1/tickets` 200
// without that row while `GET /api/v1/admin/tickets` was 404.
//
// These two handlers are the missing surface: they read across tenants and are gated on the SAME
// flag the admin middleware enforces (`users.perm_is_super_admin`), checked in the handler as well so
// the rule does not depend on where the router happens to mount them. Read is one page of every
// workspace's tickets newest-first, with the workspace NAME joined in so a row is attributable
// without a second lookup; the write sets the STATUS only (that is the whole operator decision here —
// "picked this up" / "done"), and never edits a customer's own words.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminTicket {
    pub id: Uuid,
    pub aid: Uuid,
    pub account_name: Option<String>,
    pub subject: String,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub source: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct AdminListQuery {
    pub status: Option<String>,
    pub source: Option<String>,
    #[serde(default)]
    pub page: i64,
    #[serde(default)]
    pub per_page: i64,
}

#[derive(Debug, Deserialize)]
pub struct AdminStatusInput {
    pub status: String,
}

fn require_super_admin(claims: &Claims) -> Result<(), AppError> {
    if claims.perm_is_super_admin.unwrap_or(false) {
        Ok(())
    } else {
        Err(AppError::Forbidden(
            "Only the platform operator can read every workspace's tickets".to_string(),
        ))
    }
}

/// `GET /api/v1/admin/tickets` — every workspace's support tickets, newest first.
///
/// Optional `status` / `source` filters (`source=plan_request` is the upgrade queue), and the
/// console's own paging (`page`, `per_page`, capped at 200). Returns `{items, count}` — the same
/// envelope `list` answers, so one console renderer serves both.
pub async fn list_all(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<AdminListQuery>,
) -> ApiResult<impl IntoResponse> {
    require_super_admin(&claims)?;
    let limit = if q.per_page > 0 {
        q.per_page.min(200)
    } else {
        50
    };
    let offset = if q.page > 1 { (q.page - 1) * limit } else { 0 };
    let rows = sqlx::query_as::<_, AdminTicket>(
        "SELECT t.id, t.aid, a.name AS account_name, t.subject, t.description, t.status, t.priority, t.source, t.created_at, t.updated_at FROM tickets t LEFT JOIN accounts a ON a.id = t.aid WHERE ($1::text IS NULL OR t.status = $1) AND ($2::text IS NULL OR t.source = $2) ORDER BY t.created_at DESC LIMIT $3 OFFSET $4",
    )
    .bind(q.status.as_deref())
    .bind(q.source.as_deref())
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({ "items": rows, "count": rows.len() })))
}

/// `PUT /api/v1/admin/tickets/{id}` — move ANY workspace's ticket to a new status.
///
/// The customer's own route (`update` above) is pinned to `aid`, which is why an operator could not
/// even mark a plan request as handled without impersonating the tenant. Status only: the subject and
/// description belong to the person who wrote them.
pub async fn admin_update_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(b): Json<AdminStatusInput>,
) -> ApiResult<impl IntoResponse> {
    require_super_admin(&claims)?;
    let status = b.status.trim().to_lowercase();
    if !matches!(status.as_str(), "open" | "pending" | "closed" | "resolved") {
        return Err(AppError::Validation(
            "status must be one of: open, pending, closed, resolved".to_string(),
        ));
    }
    let updated = sqlx::query("UPDATE tickets SET status = $2, updated_at = NOW() WHERE id = $1")
        .bind(id)
        .bind(&status)
        .execute(&state.db)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound("Ticket not found".into()));
    }
    // Re-read through the SAME joined select the list uses, so the console repaints an attributable
    // row (account name included) rather than a bare ticket.
    let row = sqlx::query_as::<_, AdminTicket>(
        "SELECT t.id, t.aid, a.name AS account_name, t.subject, t.description, t.status, t.priority, t.source, t.created_at, t.updated_at FROM tickets t LEFT JOIN accounts a ON a.id = t.aid WHERE t.id = $1",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({ "item": row })))
}
