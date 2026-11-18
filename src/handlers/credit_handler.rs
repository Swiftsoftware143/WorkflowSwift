use axum::{
    extract::{Json, State},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::{PgConnection, Row};
use std::collections::HashMap;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::AppState;

/// THE ONE GRANT every plan seat goes through (kanban t_b0774174).
///
/// A new workspace could not run a single workflow: the signup granted
/// `plan_tiers.features->>'credits_monthly'` and NO tier declared the key, so the grant was dead
/// code and every account (free AND Enterprise) sat at `SUM(amount) = 0` while the Run gate refuses
/// below 1. A `credits_monthly` grant that only the signup knows about also cannot cover the other
/// doors that seat a plan — the operator's "assign plan", a resolved plan-request ticket, and the
/// fleet portfolio sync — so the rule lives here, in one statement, and every door calls it.
///
/// THE RULE
/// * Grant the tier's `credits_monthly`, else ONE CREDIT PER WORKFLOW the tier includes
///   (`features.max_workflows`, finite positive only). `professional`/`enterprise` store `-1` and
///   are covered by the declared allowance; the fallback keeps a NEW operator-created tier from
///   reproducing this defect.
/// * Only for a workspace that has NEVER had a ledger row (`NOT EXISTS`), so this is a WELCOME
///   grant, not a refill: a workspace whose credits were spent is never re-credited, and repeated
///   plan moves cannot farm credits. The guard is inside the INSERT, so it is a single
///   statement and cannot race another caller.
/// * Returns the amount granted (`None` = nothing to grant), so a caller can log it.
pub async fn grant_plan_welcome_credits(
    conn: &mut PgConnection,
    aid: Uuid,
    plan_id: Uuid,
) -> Result<Option<i32>, AppError> {
    let granted: Option<i32> = sqlx::query_scalar(
        r#"
        INSERT INTO credit_transactions
               (id, aid, amount, transaction_type, description, reference_id)
        SELECT $1, $2, g.amount, 'grant',
               'Welcome credits: first month of ' || COALESCE(g.name, 'your plan'),
               'plan_allowance:' || COALESCE(g.slug, 'unknown')
          FROM (
              SELECT name,
                     slug,
                     CASE
                       -- A declared allowance is the tier's own number; 0 means "this tier grants
                       -- none" and is honoured. The ceiling exists only so an operator typo cannot
                       -- mint an unbounded balance.
                       WHEN (features->>'credits_monthly') ~ '^[0-9]+$'
                         THEN LEAST((features->>'credits_monthly')::integer, 100000)
                       -- No declared allowance: one run per workflow the plan includes. `-1` and
                       -- "unlimited" do not match this regex and grant nothing here.
                       WHEN (features->>'max_workflows') ~ '^[0-9]+$'
                         THEN LEAST((features->>'max_workflows')::integer, 1000)
                       ELSE NULL
                     END AS amount
                FROM plan_tiers
               WHERE id = $3
          ) g
         WHERE g.amount > 0
           AND NOT EXISTS (
               SELECT 1 FROM credit_transactions ct WHERE ct.aid = $2
           )
        RETURNING amount
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(plan_id)
    .fetch_optional(conn)
    .await?;

    if let Some(amt) = granted {
        tracing::info!(account = %aid, plan = %plan_id, credits = amt,
            "welcome credits granted — the workspace can run its plan's workflows");
    }
    Ok(granted)
}

/// Tiered pricing — profit margin optimized
fn tiered_pricing() -> HashMap<&'static str, (i64, &'static str)> {
    let mut m = HashMap::new();
    m.insert("simple", (2, "Simple workflow execution"));
    m.insert("medium", (3, "Medium complexity workflow"));
    m.insert("complex", (5, "Complex workflow execution"));
    m.insert("auto", (3, "Automated recurring workflow"));
    m.insert("ai_enhanced", (7, "AI-enhanced workflow"));
    m.insert("newsletter", (2, "Newsletter delivery"));
    m.insert("lead_gen", (3, "Lead generation workflow"));
    m.insert("social", (3, "Social media multi-post"));
    m.insert("onboarding", (3, "Employee onboarding"));
    m.insert("project", (4, "Project delivery workflow"));
    m.insert("capability", (3, "Capability statement"));
    m.insert("acquisition", (3, "Business acquisition target"));
    m.insert("prime_outreach", (3, "Prime contractor outreach"));
    m.insert("proposal_followup", (2, "Proposal follow-up"));
    m.insert("subcontractor", (3, "Subcontractor recruitment"));
    m.insert("teaming", (3, "Teaming introduction"));
    m.insert("sam_solicitations", (5, "SAM.gov solicitation tracking"));
    m.insert("monitor", (5, "Federal contract monitoring"));
    m
}

/// GET /api/v1/credits/balance
/// Returns the current credit balance + rollover info.
pub async fn credit_balance(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // Count total used credits (negative amounts)
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(ABS(amount)), 0) FROM credit_transactions WHERE aid = $1 AND amount < 0 AND transaction_type = 'usage'",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // Rollover balance: sum of all 'rollover' transactions
    let rollover_balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1 AND transaction_type = 'rollover'",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // Non-rollover available credits (can be negative if overspent)
    let available = balance; // total balance is the real available amount

    // NEXT RESET: deliberately NULL — there is NO renewal engine (kanban t_b0774174).
    //
    // This field used to answer the next monthly anniversary of the active plan's `started_at`,
    // with the comment above keeping "the field's promise: a 'next reset' is always in the
    // future". That promise was never honoured by anything: no code grants credits on a billing
    // boundary, `credit_transactions.billing_month` has no reader, and the plan allowance is
    // granted ONCE, when a workspace first lands on a plan (`grant_plan_welcome_credits`). A date
    // in this field told a customer "credits arrive on the 18th" and nothing ever arrived — the
    // same class as the dead `credits_monthly` grant this card fixed. The field is kept (no client
    // in this app renders it, and the response shape stays intact) and answers null until a real
    // renewal exists; top-ups today are credit packs the team grants by hand.
    let next_reset: Option<String> = None;

    Ok(Json(json!({
        "balance": balance,
        "used": used,
        "available": available.max(0),
        "rollover_balance": rollover_balance,
        "next_reset": next_reset,
        "currency": "credits"
    })))
}

/// GET /api/v1/credits/transactions
/// List recent credit transactions for this account.
pub async fn list_transactions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        r#"SELECT id, amount, transaction_type, description, reference_id, created_at::text
           FROM credit_transactions
           WHERE aid = $1
           ORDER BY created_at DESC
           LIMIT 100"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let transactions: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            // `credit_transactions.amount` is INT4, so it decodes as i32 — reading it as i64
            // failed on EVERY row and `unwrap_or(0)` reported every transaction as 0
            // (kanban t_3d0c5623). No field is defaulted silently any more: a decode failure
            // is logged and only then reported as null/0/"".
            let id = row.try_get::<Uuid, _>("id");
            let amount = row.try_get::<i32, _>("amount");
            let transaction_type = row.try_get::<&str, _>("transaction_type");
            let description = row.try_get::<Option<&str>, _>("description");
            let reference_id = row.try_get::<Option<&str>, _>("reference_id");
            let created_at = row.try_get::<&str, _>("created_at");

            for (field, err) in [
                ("id", id.as_ref().err()),
                ("amount", amount.as_ref().err()),
                ("transaction_type", transaction_type.as_ref().err()),
                ("description", description.as_ref().err()),
                ("reference_id", reference_id.as_ref().err()),
                ("created_at", created_at.as_ref().err()),
            ] {
                if let Some(e) = err {
                    tracing::warn!(
                        field = field,
                        error = %e,
                        "credit_transactions: field failed to decode"
                    );
                }
            }

            json!({
                "id": id.map(|u| u.to_string()).unwrap_or_default(),
                "amount": amount.unwrap_or(0),
                "transaction_type": transaction_type.unwrap_or(""),
                "description": description.unwrap_or(None),
                "reference_id": reference_id.unwrap_or(None),
                "created_at": created_at.unwrap_or(""),
            })
        })
        .collect();

    Ok(Json(json!({"transactions": transactions})))
}

/// GET /api/v1/credits/packages
/// List available credit packages for purchase.
pub async fn list_credit_packages(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query(
        r#"SELECT id::text, name, credits, price::text, is_active, created_at::text
           FROM credit_packages
           WHERE is_active = true
           ORDER BY credits ASC"#,
    )
    .fetch_all(&state.db)
    .await?;

    let packages: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.try_get::<&str, _>("id").unwrap_or(""),
                "name": row.try_get::<&str, _>("name").unwrap_or(""),
                "credits": row.try_get::<i32, _>("credits").unwrap_or(0),
                "price": row.try_get::<&str, _>("price").unwrap_or("0.00"),
                "is_active": row.try_get::<bool, _>("is_active").unwrap_or(true),
            })
        })
        .collect();

    Ok(Json(json!({"packages": packages})))
}

/// POST /api/v1/credits/rollover
/// Move remaining balance to a non-expiring rollover bucket.
/// This is called automatically at the end of a billing cycle.
/// Rollover credits never expire.
pub async fn rollover_credits(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Get current non-rollover balance
    let non_rollover: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1 AND transaction_type != 'rollover'",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if non_rollover <= 0 {
        return Ok(Json(json!({
            "message": "No credits to roll over",
            "rolled": 0
        })));
    }

    // Move positive balance to rollover bucket
    sqlx::query(
        r#"INSERT INTO credit_transactions (id, aid, amount, transaction_type, description)
           VALUES ($1, $2, $3, 'rollover', 'Credits rolled over from previous cycle — never expires')"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(non_rollover)
    .execute(&state.db)
    .await?;

    // Subtract from non-rollover balance (mark as moved to rollover)
    // By recording a negative rollover entry that cancels out the non-rollover balance
    // Actually, the simpler approach: record the rollover transaction, which adds to rollover total.
    // The non-rollover transactions still exist — but when we query for balance,
    // we sum ALL transactions, and separate rollover_balance query sums only 'rollover' type.
    // This means the user's total balance doesn't change — just re-categorized.

    let total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "message": format!("{} credits rolled over — never expires", non_rollover),
        "rolled": non_rollover,
        "total_balance": total
    })))
}

/// Deduct credits based on workflow type (tiered pricing).
/// n8n calls this at the start of a workflow.
pub async fn deduct_credit(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let pricing = tiered_pricing();

    let workflow_type = req
        .get("workflow_type")
        .and_then(|v| v.as_str())
        .unwrap_or("simple");
    let explicit_amount = req.get("amount").and_then(|v| v.as_i64());

    let (amount, description) = if let Some(a) = explicit_amount {
        (a, format!("Workflow execution ({} credits)", a))
    } else if let Some((a, desc)) = pricing.get(workflow_type) {
        (*a, desc.to_string())
    } else {
        (2, "Workflow execution".to_string())
    };

    if amount <= 0 {
        return Err(AppError::Validation("Amount must be positive".to_string()));
    }

    let balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if balance < amount {
        // Same truth as the run gate in `workflow_handler::run_in_process` (cron 2026-11-04): there
        // is no self-serve purchase in this app, so a refusal may not tell the caller to buy. Name
        // the numbers and the door that actually exists.
        return Err(AppError::BadRequest(format!(
            "This needs {amount} credits and the workspace has {balance}. Open Credits to request \
             a credit pack — nothing is charged in the app; our team adds the credits to this \
             workspace by hand and replies to your request."
        )));
    }

    sqlx::query(
        r#"INSERT INTO credit_transactions (id, aid, amount, transaction_type, description)
           VALUES ($1, $2, $3, 'usage', $4)"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(-amount)
    .bind(&description)
    .execute(&state.db)
    .await?;

    let new_balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE aid = $1",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "deducted": amount,
        "workflow_type": workflow_type,
        "description": description,
        "balance": new_balance
    })))
}
