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
    /// The tier a plan request asks for (cron 2026-10-20). `None` on an ordinary ticket.
    pub plan_slug: Option<String>,
    /// When the operator's resolve moved the workspace onto `plan_slug`. `None` = not applied yet.
    pub plan_applied_at: Option<chrono::DateTime<chrono::Utc>>,
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
    /// The catalogue slug this request asks for. Optional: an ordinary ticket names no plan.
    pub plan_slug: Option<String>,
    /// The credit pack a `credit_pack_request` asks for (cron 2026-10-24). Optional everywhere else.
    pub credit_pack_id: Option<String>,
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
        "SELECT id, aid, subject, description, status, priority, source, created_at, updated_at, plan_slug, plan_applied_at FROM tickets"
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

/// File the paid tier a VISITOR PICKED on the public pricing page, at the moment they sign up.
///
/// The pricing cards carry `?plan=professional` into `/signup`, nginx carries that query through to
/// `/login?register=1&plan=professional`, and the register form posted `{name, email}` — so the pick
/// died at the door: the workspace started on Free and NOTHING in the operator's Plan requests queue
/// knew a paid tier had been asked for. A tier a visitor picked is a lead, not a preference.
///
/// The row is the SAME machine-readable `source='plan_request'` ticket the console's Plan & Billing
/// request button files (`POST /tickets` with `plan_slug`), which is what the operator's Plan
/// requests screen queries (`?source=plan_request`) and what its Resolve action switches the
/// workspace with. So a signup pick is workable, not a line in a log.
///
/// Best-effort by design: a free, unknown or retired slug files nothing and returns `None`; the
/// signup itself must never fail because of a preference. Called AFTER the account exists, so the
/// ticket always has an owner.
pub async fn file_signup_plan_request(
    db: &sqlx::PgPool,
    aid: Uuid,
    picked: &str,
) -> Result<Option<(String, String)>, AppError> {
    let slug = picked.trim().to_ascii_lowercase();
    if slug.is_empty() || slug == "free" {
        return Ok(None);
    }
    let row: Option<(String, String, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT slug, name, price_monthly::float8, price_yearly::float8 FROM plan_tiers \
         WHERE slug = $1 AND is_active = true LIMIT 1",
    )
    .bind(&slug)
    .fetch_optional(db)
    .await?;
    // An unknown / retired slug is not a request an operator could ever honour.
    let Some((slug, name, monthly, yearly)) = row else {
        return Ok(None);
    };
    let monthly = monthly.unwrap_or(0.0);
    let yearly = yearly.unwrap_or(0.0);
    // A tier that costs NOTHING is not a request to buy — the signup already starts on it.
    if monthly <= 0.0 && yearly <= 0.0 {
        return Ok(None);
    }
    // One press is one request, keyed on the TIER: a workspace with a Professional ask open can
    // still ask for Enterprise, and no visitor's pick may become two rows in the operator's queue.
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tickets WHERE aid = $1 AND source = 'plan_request' AND plan_slug = $2 \
         AND lower(status) NOT IN ('closed','resolved','done') ORDER BY created_at DESC LIMIT 1",
    )
    .bind(aid)
    .bind(&slug)
    .fetch_optional(db)
    .await?;
    if existing.is_some() {
        return Ok(Some((slug, name)));
    }
    let price = if monthly > 0.0 {
        format!("${monthly:.0}/month")
    } else {
        format!("${yearly:.0}/year")
    };
    sqlx::query(
        "INSERT INTO tickets (id, aid, subject, description, status, priority, source, plan_slug) \
         VALUES ($1,$2,$3,$4,'open','medium','plan_request',$5)",
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(format!("Upgrade request: {name} plan"))
    .bind(format!(
        "Picked the {name} plan ({price}) on the pricing page at signup. Self-serve checkout is not \
         open yet, so the team has to switch this workspace onto the paid plan by hand."
    ))
    .bind(&slug)
    .execute(db)
    .await?;
    Ok(Some((slug, name)))
}

/// Both spellings a console may use for "the customer is asking to change plan": the operator
/// queue labels it `plan_request`, the served console files it as `plan_request` too, and older
/// builds sent `billing` for the same intent.
fn is_plan_request(source: &str) -> bool {
    matches!(source, "plan_request" | "billing")
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
    // The tier a plan request ASKS FOR travels with the row (cron 2026-10-20). Before this it lived
    // only as words inside `subject` ("Upgrade request: Professional plan"), so the operator's
    // resolve had nothing machine-readable to move the workspace onto and the customer stayed on the
    // free plan while being told the request was handled (measured live). Validated here against the
    // ACTIVE catalogue, so a typo or a retired tier is a 400 at filing time rather than a request no
    // operator can match to a plan.
    let plan_slug = b
        .plan_slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    if let Some(slug) = plan_slug.as_deref() {
        let known: Option<String> =
            sqlx::query_scalar("SELECT name FROM plan_tiers WHERE slug = $1 AND is_active = true")
                .bind(slug)
                .fetch_optional(&state.db)
                .await?;
        if known.is_none() {
            return Err(AppError::Validation(format!(
                "No active plan with slug '{slug}'"
            )));
        }
    }
    // A plan request must carry the tier it asks FOR, machine-readable: the operator's resolve
    // moves the workspace onto `plan_slug` and nothing else can. The console sent only the words
    // ("Plan request — Professional ($49/mo)") until 2026-10-21, so every request filed from it
    // resolved to no plan at all — measured live on the shipped binary: filed twice, resolved as
    // the operator, `GET /auth/me` still answered "Free" and the only active `account_plans` row
    // was `free`, while the customer had been mailed that the plan was set up. Derive the tier
    // from the subject against the ACTIVE catalogue when the caller omits it, so a cached console
    // cannot file a request that can never be honoured; and refuse when nothing can be named,
    // because a plan request with no plan is a dead row in the operator's queue.
    let plan_slug = if plan_slug.is_none() && is_plan_request(&source) {
        sqlx::query_scalar::<_, String>(
            "SELECT slug FROM plan_tiers WHERE is_active = true AND $1 ILIKE '%' || name || '%' \
             ORDER BY length(name) DESC LIMIT 1",
        )
        .bind(&b.subject)
        .fetch_optional(&state.db)
        .await?
    } else {
        plan_slug
    };
    if is_plan_request(&source) && plan_slug.is_none() {
        return Err(AppError::Validation(
            "A plan request must name the plan it asks for".into(),
        ));
    }
    // A second press on the same plan is not a second request (fleet-wide, 2026-10-21 — the same
    // rule IncentiveSwift's `plan_requests` and MissedCall's upgrade ask already carry). Measured
    // live on the shipped binary: two POSTs one after the other wrote TWO open `plan_request`
    // tickets and queued two receipts, so the operator's queue read as two customers asking once.
    // Keyed on the TIER, so a workspace with a Professional ask open can still ask for Enterprise,
    // and ordinary support tickets are never deduped (a question may legitimately repeat).
    if is_plan_request(&source) {
        if let Some(slug) = plan_slug.as_deref() {
            let existing: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM tickets WHERE aid = $1 AND source = $2 AND plan_slug = $3 \
                 AND lower(status) NOT IN ('closed','resolved','done') \
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(aid)
            .bind(&source)
            .bind(slug)
            .fetch_optional(&state.db)
            .await?;
            if let Some(existing_id) = existing {
                let row = sqlx::query_as::<_, Ticket>(BY_ID)
                    .bind(existing_id)
                    .fetch_one(&state.db)
                    .await?;
                // `acknowledged: false` because nothing new was mailed: the receipt went out with
                // the FIRST press, and a second mail for the same ask is exactly the noise this
                // guard exists to stop.
                return Ok((
                    StatusCode::OK,
                    Json(json!({ "item": row, "acknowledged": false, "already_open": true })),
                ));
            }
        }
    }
    // ── Credit-pack request ───────────────────────────────────────────────────────────────
    // The Credits screen offers four packs with real prices and, until this, no way to obtain one:
    // the only purchase route is super-admin-only AND scoped to the caller's own account, while a
    // deployed workflow's balance gate refuses at zero credits — measured live on a fresh Free
    // workspace: `POST /credits/deduct` answered 400 "Insufficient credits. Need 2, have 0."
    // A pack request is a TICKET, the same shape a plan upgrade already uses: nothing is charged
    // here, the operator sees it in the Plan requests queue, and the same resolve that applies a
    // plan applies the pack's credits (`admin_update_status`).
    let credit_pack_id = if source == "credit_pack_request" {
        let raw = b
            .credit_pack_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::Validation("A credit pack request must name the pack it asks for".into())
            })?;
        let pack_id = Uuid::parse_str(raw)
            .map_err(|_| AppError::Validation("Invalid credit_pack_id".into()))?;
        // Validated against the ACTIVE catalogue, so a retired pack is refused at filing time
        // instead of sitting in the queue as a row no operator can fulfil.
        let known: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM credit_packages WHERE id = $1 AND is_active = true")
                .bind(pack_id)
                .fetch_optional(&state.db)
                .await?;
        if known.is_none() {
            return Err(AppError::Validation(
                "No active credit pack with that id".into(),
            ));
        }
        // One open ask per pack — the same rule plan requests carry, and keyed on the PACK so a
        // workspace asking for the Growth Pack is not swallowed by an open Starter request.
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM tickets WHERE aid = $1 AND source = 'credit_pack_request' \
             AND credit_pack_id = $2 AND lower(status) NOT IN ('closed','resolved','done') \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(aid)
        .bind(pack_id)
        .fetch_optional(&state.db)
        .await?;
        if let Some(existing_id) = existing {
            let row = sqlx::query_as::<_, Ticket>(BY_ID)
                .bind(existing_id)
                .fetch_one(&state.db)
                .await?;
            return Ok((
                StatusCode::OK,
                Json(json!({ "item": row, "acknowledged": false, "already_open": true })),
            ));
        }
        Some(pack_id)
    } else {
        None
    };
    sqlx::query("INSERT INTO tickets (id, aid, subject, description, status, priority, source, plan_slug, credit_pack_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(id).bind(aid).bind(&b.subject).bind(&description).bind(&status).bind(&priority).bind(&source)
        .bind(plan_slug.as_deref())
        .bind(credit_pack_id)
        .execute(&state.db).await?;
    let row = sqlx::query_as::<_, Ticket>(BY_ID)
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    // A PLAN REQUEST is the console's way to buy while checkout is closed (cron 2026-10-20): the row
    // was written and the customer's inbox stayed empty until an operator happened to answer. The
    // receipt goes out here, best-effort, and `acknowledged` reports whether it was actually queued
    // so the console can only promise a mail that exists. Both spellings are accepted because the
    // served console files `source:'billing'` while the operator queue labels the same intent
    // `plan_request` — the customer's intent is identical and neither may be silent.
    let acknowledged = if is_plan_request(&source) {
        notify_customer_of_plan_request(&state, &row).await
    } else {
        false
    };
    Ok((
        StatusCode::CREATED,
        Json(json!({ "item": row, "acknowledged": acknowledged })),
    ))
}

/// The receipt a customer gets the moment their plan request is filed (cron 2026-10-20).
///
/// Filed from the console's Plans section as a real ticket (`source='plan_request'`). While checkout
/// is closed that request IS the buying path, so an unanswered one is a lost sale. Sends to the
/// workspace's OWN owner through `email::send_email` — the funnel that carries per-account branding
/// and withholds reserved/harness domains, so a probe records the attempt without a message leaving
/// the box. Best-effort: the row is the record the operator works from.
async fn notify_customer_of_plan_request(state: &AppState, ticket: &Ticket) -> bool {
    let owner: Option<(String, String)> = sqlx::query_as(
        "SELECT name, email FROM users WHERE aid = $1 AND is_active ORDER BY (role IN ('owner','company_admin')) DESC, sort_order NULLS LAST, created_at ASC LIMIT 1",
    )
    .bind(ticket.aid)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let Some((name, email)) = owner else {
        tracing::warn!(
            ticket = %ticket.id,
            aid = %ticket.aid,
            "plan request: the workspace has no active user to acknowledge"
        );
        return false;
    };
    let vars = json!({
        "name": name,
        "ticket_subject": ticket.subject,
        "app_url": "https://app.workflowswift.com",
    });
    match crate::email::send_email(
        state,
        Some(ticket.aid),
        &email,
        "plan_request_receipt",
        &vars,
    )
    .await
    {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(ticket = %ticket.id, "plan-request receipt to {email} failed: {e}");
            false
        }
    }
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
    /// The tier this request asks for (cron 2026-10-20); `None` on an ordinary ticket.
    pub plan_slug: Option<String>,
    /// The tier's display name (joined from `plan_tiers`), so the operator row names the plan rather
    /// than printing a slug.
    pub plan_name: Option<String>,
    /// When the resolve moved the workspace onto that tier; `None` = still waiting.
    pub plan_applied_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The harness marker of the workspace that filed this (`accounts.probe_harness`), e.g.
    /// `auto-example.invalid`. Empty for a real customer. Without it the operator's queue cannot
    /// tell a revenue-shaped "Upgrade request" from a test run, and the probe rows read as
    /// customers waiting on an answer (measured 2026-10-20: both open requests in the queue were
    /// harness rows). Exposed, never filtered server-side: hiding a row here would make the queue
    /// lie in the other direction.
    pub account_probe: Option<String>,
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
        "SELECT t.id, t.aid, a.name AS account_name, t.subject, t.description, t.status, t.priority, t.source, t.created_at, t.updated_at, t.plan_slug, COALESCE(pt.name, t.plan_slug) AS plan_name, t.plan_applied_at, COALESCE(a.probe_harness, '') AS account_probe FROM tickets t LEFT JOIN accounts a ON a.id = t.aid LEFT JOIN plan_tiers pt ON pt.slug = t.plan_slug WHERE ($1::text IS NULL OR t.status = $1) AND ($2::text IS NULL OR t.source = $2) ORDER BY t.created_at DESC LIMIT $3 OFFSET $4",
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
    // The status BEFORE this call (2026-10-19). The customer is told only when something actually
    // moved: a console that re-sends the value it already shows, or an operator double-clicking
    // "Mark handled", must not mail the same person twice about the same non-event.
    let prior: Option<(String, Uuid, Option<String>, bool, Option<Uuid>, bool)> = sqlx::query_as(
        "SELECT status, aid, plan_slug, (plan_applied_at IS NOT NULL), credit_pack_id, \
         (credits_applied_at IS NOT NULL) FROM tickets WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let Some((
        prior_status,
        ticket_aid,
        requested_plan,
        already_applied,
        ticket_credit_pack,
        credits_already_applied,
    )) = prior
    else {
        return Err(AppError::NotFound("Ticket not found".into()));
    };
    let changed = prior_status != status;
    let updated = sqlx::query("UPDATE tickets SET status = $2, updated_at = NOW() WHERE id = $1")
        .bind(id)
        .bind(&status)
        .execute(&state.db)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound("Ticket not found".into()));
    }
    // MOVE THE WORKSPACE ONTO THE PLAN IT ASKED FOR (cron 2026-10-20). Before this the operator's
    // "Resolved" changed one word on the row and the customer was mailed "handled" while the
    // workspace stayed on the free plan — measured live on the shipped binary: filed a Professional
    // request, resolved it as a super-admin, `GET /auth/me` still answered plan_name "Free" and the
    // only active `account_plans` row was `free`. The switch is deliberately the SAME shape the
    // platform's "assign plan" route uses (`admin_assign_plan`): supersede every active row, insert
    // the new one, one transaction — `features::resolve_plan_id` reads the newest ACTIVE row first,
    // so two live plans can never be ambiguous. Idempotent through `plan_applied_at`: a second
    // resolve, or a following "closed", never supersedes the same tier twice.
    let mut plan_applied = false;
    let mut applied_plan_name: Option<String> = None;
    if changed && matches!(status.as_str(), "resolved" | "closed") && !already_applied {
        if let Some(slug) = requested_plan.as_deref() {
            let plan: Option<(Uuid, Option<String>)> = sqlx::query_as(
                "SELECT id, name FROM plan_tiers WHERE slug = $1 AND is_active = true",
            )
            .bind(slug)
            .fetch_optional(&state.db)
            .await?;
            match plan {
                Some((plan_id, plan_name)) => {
                    let mut tx = state.db.begin().await?;
                    sqlx::query(
                        "UPDATE account_plans SET status = 'superseded', is_active = false, expires_at = NOW() \
                         WHERE aid = $1 AND status = 'active'",
                    )
                    .bind(ticket_aid)
                    .execute(&mut *tx)
                    .await?;
                    sqlx::query(
                        "INSERT INTO account_plans (aid, plan_id, status, started_at) \
                         VALUES ($1, $2, 'active', NOW())",
                    )
                    .bind(ticket_aid)
                    .bind(plan_id)
                    .execute(&mut *tx)
                    .await?;
                    sqlx::query("UPDATE tickets SET plan_applied_at = NOW() WHERE id = $1")
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                    tx.commit().await?;
                    plan_applied = true;
                    applied_plan_name = plan_name;
                    tracing::info!(
                        ticket = %id,
                        account = %ticket_aid,
                        plan = %slug,
                        "Plan request resolved — the workspace was moved onto the requested plan"
                    );
                }
                None => {
                    tracing::warn!(
                        ticket = %id,
                        plan = %slug,
                        "Plan request names a tier that is no longer active — status only, nothing switched"
                    );
                }
            }
        }
    }
    // ── Apply the pack a credit request names ──────────────────────────────────────────────
    // Same transactional shape as the plan switch above and idempotent the same way: the row
    // records `credits_applied_at`, so a second resolve (or a later "closed") never grants twice.
    // A `grant` transaction, not `purchase`: no money moved through the app — the operator made
    // the arrangement by hand, exactly like a plan switch. The ticket id is the reference, which
    // makes the ledger line traceable back to the ask.
    let mut credits_applied: Option<i32> = None;
    let mut credits_balance: Option<i64> = None;
    if changed && matches!(status.as_str(), "resolved" | "closed") && !credits_already_applied {
        if let Some(pack_id) = ticket_credit_pack {
            let pack: Option<(String, i32)> = sqlx::query_as(
                "SELECT name, credits FROM credit_packages WHERE id = $1 AND is_active = true",
            )
            .bind(pack_id)
            .fetch_optional(&state.db)
            .await?;
            match pack {
                Some((name, credits)) if credits > 0 => {
                    let mut tx = state.db.begin().await?;
                    sqlx::query(
                        "INSERT INTO credit_transactions (id, aid, amount, transaction_type, description, reference_id) \
                         VALUES ($1, $2, $3, 'grant', $4, $5)",
                    )
                    .bind(Uuid::new_v4())
                    .bind(ticket_aid)
                    .bind(credits)
                    .bind(format!(
                        "Credit pack applied: {name} ({credits} credits) — request {id}"
                    ))
                    .bind(id.to_string())
                    .execute(&mut *tx)
                    .await?;
                    sqlx::query("UPDATE tickets SET credits_applied_at = NOW() WHERE id = $1")
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                    tx.commit().await?;
                    credits_applied = Some(credits);
                    credits_balance = sqlx::query_scalar(
                        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1",
                    )
                    .bind(ticket_aid)
                    .fetch_one(&state.db)
                    .await
                    .ok();
                    tracing::info!(
                        ticket = %id,
                        account = %ticket_aid,
                        credits,
                        "Credit pack request resolved — the credits were added to the workspace"
                    );
                }
                Some(_) => tracing::warn!(
                    ticket = %id,
                    "Credit pack declares no credits — status only, nothing granted"
                ),
                None => tracing::warn!(
                    ticket = %id,
                    "Credit request names a pack that is no longer active — status only, nothing granted"
                ),
            }
        }
    }
    // Re-read through the SAME joined select the list uses, so the console repaints an attributable
    // row (account name included) rather than a bare ticket.
    let row = sqlx::query_as::<_, AdminTicket>(
        "SELECT t.id, t.aid, a.name AS account_name, t.subject, t.description, t.status, t.priority, t.source, t.created_at, t.updated_at, t.plan_slug, COALESCE(pt.name, t.plan_slug) AS plan_name, t.plan_applied_at, COALESCE(a.probe_harness, '') AS account_probe FROM tickets t LEFT JOIN accounts a ON a.id = t.aid LEFT JOIN plan_tiers pt ON pt.slug = t.plan_slug WHERE t.id = $1",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    if changed {
        // The plan the workspace was ACTUALLY moved onto, when this resolve applied one: the
        // notice then states it instead of hedging about what a plan request might mean.
        notify_customer(&state, &row, &status, applied_plan_name.as_deref()).await;
    }
    Ok(Json(json!({
        "item": row,
        "plan_applied": plan_applied,
        "plan_name": applied_plan_name,
        // `None` when this ticket was not a credit request: the console then says nothing about credits.
        "credits_applied": credits_applied,
        "credits_balance": credits_balance,
    })))
}

/// Tell the person who filed the request that it moved (2026-10-19).
///
/// The operator's action is already committed when this runs, so it is best-effort by construction:
/// a mail that cannot go out is logged and never turns a successful status change into a 500. The
/// recipient is the workspace's OWN owner (the same pick `auth` uses for lifecycle mail), never a
/// free-text address — and `email::send_email` withholds reserved/harness domains, so a probe
/// workspace (`...@example.invalid`) records the attempt in `email_send_log` without any message
/// leaving the box.
async fn notify_customer(
    state: &AppState,
    ticket: &AdminTicket,
    status: &str,
    applied_plan: Option<&str>,
) {
    let owner: Option<(String, String)> = sqlx::query_as(
        "SELECT name, email FROM users WHERE aid = $1 AND is_active ORDER BY (role IN ('owner','company_admin')) DESC, sort_order NULLS LAST, created_at ASC LIMIT 1",
    )
    .bind(ticket.aid)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let Some((name, email)) = owner else {
        tracing::warn!(
            ticket = %ticket.id,
            aid = %ticket.aid,
            "ticket status change: the workspace has no active user to notify"
        );
        return;
    };
    let vars = json!({
        "name": name,
        "ticket_subject": ticket.subject,
        "status": status,
        // `None` renders as JSON null, which the template arm reads as "no plan moved".
        "plan_name": applied_plan,
        "app_url": "https://app.workflowswift.com",
    });
    if let Err(e) =
        crate::email::send_email(state, Some(ticket.aid), &email, "ticket_update", &vars).await
    {
        tracing::warn!(
            ticket = %ticket.id,
            "ticket status notice to {email} failed: {e}"
        );
    }
}
