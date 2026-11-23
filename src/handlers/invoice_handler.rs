//! `invoice_handler` — the operator's invoice ledger (2026-11-23).
//!
//! Card payments are OFF on this installation (`payment_providers` holds zero rows), so a plan
//! REQUEST is the buying path and the operator's own `billing` words tell a customer how to send
//! money. The `invoices` table and the customer's read door have existed since migration 010, but
//! nothing in the console could RAISE a row: the operator's book was his own memory and an inbox,
//! and a customer who asked to buy could not be billed at all. ADASwift (migration 036) and
//! FunnelSwift (migration 140) already carry this rail; this is the WorkflowSwift side of it.
//!
//! Four doors:
//!   * `GET  /api/v1/invoices`              — the caller's OWN invoices (account from the token).
//!   * `GET  /api/v1/admin/invoices`        — the operator's whole book, optionally `?account_id=`.
//!   * `POST /api/v1/admin/invoices`        — raise one.
//!   * `PUT  /api/v1/admin/invoices/{id}`   — mark paid / void / reopen.
//!
//! The admin doors sit under `/api/v1/admin`, which the boundary already gates to platform admins;
//! each handler also states that predicate itself (`require_admin`), like every other admin writer
//! in this app. The customer door is scoped by `claims.aid`, so one workspace can never read
//! another's rows.
//!
//! The amount is deliberately not editable once raised: an invoice that has been sent can be
//! settled or cancelled, never silently re-priced.

use axum::{
    extract::{Json, Path, Query, State},
    response::IntoResponse,
    Extension,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::FromRow;
use std::collections::HashMap;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::handlers::admin_settings_handler::require_admin;
use crate::AppState;

/// One invoice with the two names a reader needs. `invoices.amount` is NUMERIC(10,2) and sqlx
/// cannot decode NUMERIC into `serde_json::Value`, so every read casts it to text — the same
/// convention the plan catalogue uses for its price.
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct InvoiceRow {
    pub id: Uuid,
    pub aid: Uuid,
    pub plan_id: Uuid,
    /// Joined in on every read. A catalogue may stop listing the plan an old invoice names, and a
    /// row that falls back to "Plan" tells the customer nothing.
    pub plan_name: Option<String>,
    /// The workspace's own name, joined the same way, so a bill nobody can attribute is not a
    /// record.
    pub account_name: Option<String>,
    pub amount: String,
    pub status: String,
    pub due_date: Option<chrono::DateTime<chrono::Utc>>,
    pub paid_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

fn me_account(claims: &Claims) -> Result<Uuid, AppError> {
    Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)
}

#[derive(Debug, Deserialize)]
pub struct InvoiceBody {
    /// The workspace this invoice is for (admin create only).
    #[serde(default, alias = "aid")]
    pub account_id: Option<String>,
    #[serde(default)]
    pub plan_id: Option<String>,
    /// "49", 49, "49.00" or "$49.00" — a price a person typed.
    #[serde(default)]
    pub amount: Option<serde_json::Value>,
    /// "YYYY-MM-DD" from the console's date input, or empty for no due date.
    #[serde(default)]
    pub due_date: Option<serde_json::Value>,
    /// pending | paid | void. Absent means pending.
    #[serde(default)]
    pub status: Option<String>,
}

/// A price as a person types it, normalised to two decimals. `None` is a refusal, never a zero.
fn money_text(v: &serde_json::Value) -> Option<String> {
    let raw = match v {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let cleaned = raw.trim_start_matches('$').replace(',', "");
    let parsed: f64 = cleaned.parse().ok()?;
    if !parsed.is_finite() || parsed <= 0.0 || parsed > 1_000_000.0 {
        return None;
    }
    Some(format!("{parsed:.2}"))
}

/// A due date as the console sends it; anything that is not a calendar day is refused, so a typo
/// can never land in the column that decides when a customer is overdue.
fn due_text(v: &serde_json::Value) -> Option<Option<String>> {
    let raw = match v {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Null => return Some(None),
        _ => return None,
    };
    if raw.is_empty() {
        return Some(None);
    }
    let day = raw.get(..10).unwrap_or(&raw);
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    Some(Some(day.to_string()))
}

fn status_word(v: Option<&String>) -> Option<&'static str> {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("pending") => Some("pending"),
        Some("paid") => Some("paid"),
        Some("void") => Some("void"),
        _ => None,
    }
}

const CUST_BY_AID: &str = "SELECT i.id, i.aid, i.plan_id, pt.name AS plan_name, a.name AS account_name, \
     i.amount::text AS amount, i.status, i.due_date, i.paid_at, i.created_at \
     FROM invoices i LEFT JOIN plan_tiers pt ON pt.id = i.plan_id LEFT JOIN accounts a ON a.id = i.aid \
     WHERE i.aid = $1 ORDER BY i.created_at DESC LIMIT 200";

const CUST_BY_ID: &str = "SELECT i.id, i.aid, i.plan_id, pt.name AS plan_name, a.name AS account_name, \
     i.amount::text AS amount, i.status, i.due_date, i.paid_at, i.created_at \
     FROM invoices i LEFT JOIN plan_tiers pt ON pt.id = i.plan_id LEFT JOIN accounts a ON a.id = i.aid \
     WHERE i.id = $1 AND i.aid = $2";

const ADMIN_ALL: &str = "SELECT i.id, i.aid, i.plan_id, pt.name AS plan_name, a.name AS account_name, \
     i.amount::text AS amount, i.status, i.due_date, i.paid_at, i.created_at \
     FROM invoices i LEFT JOIN plan_tiers pt ON pt.id = i.plan_id LEFT JOIN accounts a ON a.id = i.aid \
     ORDER BY i.created_at DESC LIMIT 500";

const ADMIN_BY_ACCOUNT: &str = "SELECT i.id, i.aid, i.plan_id, pt.name AS plan_name, a.name AS account_name, \
     i.amount::text AS amount, i.status, i.due_date, i.paid_at, i.created_at \
     FROM invoices i LEFT JOIN plan_tiers pt ON pt.id = i.plan_id LEFT JOIN accounts a ON a.id = i.aid \
     WHERE i.aid = $1 ORDER BY i.created_at DESC LIMIT 500";

/// GET /api/v1/invoices — the caller's own invoices, newest first.
pub async fn list_invoices(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = me_account(&claims)?;

    let invoices = sqlx::query_as::<_, InvoiceRow>(CUST_BY_AID)
        .bind(aid)
        .fetch_all(&state.db)
        .await?;

    // `items` is what the console's generic list renderer reads, `invoices` is the original key
    // kept for any existing caller — both point at the same rows.
    Ok(Json(
        json!({"invoices": &invoices, "items": &invoices, "count": invoices.len()}),
    ))
}

/// GET /api/v1/invoices/{id} — one of the caller's own invoices.
pub async fn get_invoice(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = me_account(&claims)?;

    let invoice = sqlx::query_as::<_, InvoiceRow>(CUST_BY_ID)
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Invoice not found".to_string()))?;

    Ok(Json(json!({"item": &invoice, "invoice": invoice})))
}

/// GET /api/v1/admin/invoices[?account_id=…] — the operator's whole book.
pub async fn admin_list_invoices(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;
    let filter = q
        .get("account_id")
        .or_else(|| q.get("aid"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let rows = match filter {
        Some(a) => {
            let account = Uuid::parse_str(&a)
                .map_err(|_| AppError::BadRequest("That workspace id is not valid".to_string()))?;
            sqlx::query_as::<_, InvoiceRow>(ADMIN_BY_ACCOUNT)
                .bind(account)
                .fetch_all(&state.db)
                .await?
        }
        None => {
            sqlx::query_as::<_, InvoiceRow>(ADMIN_ALL)
                .fetch_all(&state.db)
                .await?
        }
    };
    Ok(Json(
        json!({"invoices": &rows, "items": &rows, "count": rows.len()}),
    ))
}

/// POST /api/v1/admin/invoices — raise an invoice for one workspace on one of this install's plans.
pub async fn admin_create_invoice(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<InvoiceBody>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;
    let account = body
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest("Choose the workspace this invoice is for".to_string())
        })?;
    let account = Uuid::parse_str(account)
        .map_err(|_| AppError::BadRequest("That workspace id is not valid".to_string()))?;
    let plan = body
        .plan_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::BadRequest("Choose the plan this invoice covers".to_string()))?;
    let plan = Uuid::parse_str(plan)
        .map_err(|_| AppError::BadRequest("That plan id is not valid".to_string()))?;
    let amount = body.amount.as_ref().and_then(money_text).ok_or_else(|| {
        AppError::BadRequest("Enter an amount above zero, for example 49.00".to_string())
    })?;
    let due = match body.due_date.as_ref() {
        None => None,
        Some(v) => due_text(v).ok_or_else(|| {
            AppError::BadRequest("Enter the due date as a calendar day".to_string())
        })?,
    };
    let status = status_word(body.status.as_ref())
        .ok_or_else(|| AppError::BadRequest("A status is pending, paid or void".to_string()))?;

    let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM accounts WHERE id = $1")
        .bind(account)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(AppError::NotFound(
            "That workspace no longer exists".to_string(),
        ));
    }
    let plan_exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM plan_tiers WHERE id = $1")
        .bind(plan)
        .fetch_optional(&state.db)
        .await?;
    if plan_exists.is_none() {
        return Err(AppError::NotFound("That plan no longer exists".to_string()));
    }

    let row = sqlx::query_as::<_, InvoiceRow>(
        "WITH ins AS ( \
            INSERT INTO invoices (aid, plan_id, amount, status, due_date, paid_at) \
            VALUES ($1, $2, $3::numeric, $4, CAST($5 AS timestamptz), \
                    CASE WHEN $4 = 'paid' THEN now() ELSE NULL END) \
            RETURNING id, aid, plan_id, amount, status, due_date, paid_at, created_at ) \
         SELECT i.id, i.aid, i.plan_id, pt.name AS plan_name, a.name AS account_name, \
                i.amount::text AS amount, i.status, i.due_date, i.paid_at, i.created_at \
           FROM ins i LEFT JOIN plan_tiers pt ON pt.id = i.plan_id LEFT JOIN accounts a ON a.id = i.aid",
    )
    .bind(account)
    .bind(plan)
    .bind(&amount)
    .bind(status)
    .bind(due.as_deref())
    .fetch_one(&state.db)
    .await?;

    Ok(Json(json!({"item": &row, "invoice": row})))
}

/// PUT /api/v1/admin/invoices/{id} — mark paid (stamps `paid_at`), void it (clears the stamp), or
/// put it back to pending.
pub async fn admin_update_invoice(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(body): Json<InvoiceBody>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;
    let id = Uuid::parse_str(&id)
        .map_err(|_| AppError::NotFound("That invoice no longer exists".to_string()))?;
    let status = status_word(body.status.as_ref())
        .ok_or_else(|| AppError::BadRequest("A status is pending, paid or void".to_string()))?;
    let row = sqlx::query_as::<_, InvoiceRow>(
        "WITH upd AS ( \
            UPDATE invoices SET status = $2, \
                   paid_at = CASE WHEN $2 = 'paid' THEN COALESCE(paid_at, now()) ELSE NULL END \
             WHERE id = $1 \
            RETURNING id, aid, plan_id, amount, status, due_date, paid_at, created_at ) \
         SELECT i.id, i.aid, i.plan_id, pt.name AS plan_name, a.name AS account_name, \
                i.amount::text AS amount, i.status, i.due_date, i.paid_at, i.created_at \
           FROM upd i LEFT JOIN plan_tiers pt ON pt.id = i.plan_id LEFT JOIN accounts a ON a.id = i.aid",
    )
    .bind(id)
    .bind(status)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("That invoice no longer exists".to_string()))?;

    Ok(Json(json!({"item": &row, "invoice": row})))
}
