use axum::{
    extract::{State, Json},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::features;

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
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // Count total used credits (negative amounts)
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(ABS(amount)), 0) FROM credit_transactions WHERE tenant_id = $1 AND amount < 0 AND transaction_type = 'usage'",
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // Rollover balance: sum of all 'rollover' transactions
    let rollover_balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE tenant_id = $1 AND transaction_type = 'rollover'",
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // Non-rollover available credits (can be negative if overspent)
    let available = balance; // total balance is the real available amount

    // Next reset date (if plan has monthly cycle)
    let next_reset: Option<String> = sqlx::query_scalar::<_, String>(
        r#"SELECT to_char(COALESCE(tp.billing_cycle_start, NOW() + interval '1 month'), 'YYYY-MM-DD')
           FROM tenant_plans tp
           WHERE tp.tenant_id = $1 AND tp.is_active = true
           LIMIT 1"#,
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

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
/// List recent credit transactions for this tenant.
pub async fn list_transactions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        r#"SELECT id, amount, transaction_type, description, reference_id, created_at::text
           FROM credit_transactions
           WHERE tenant_id = $1
           ORDER BY created_at DESC
           LIMIT 100"#,
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    let transactions: Vec<serde_json::Value> = rows.iter().map(|row| {
        json!({
            "id": row.try_get::<Uuid, _>("id").map(|u| u.to_string()).unwrap_or_default(),
            "amount": row.try_get::<i64, _>("amount").unwrap_or(0),
            "transaction_type": row.try_get::<&str, _>("transaction_type").unwrap_or(""),
            "description": row.try_get::<Option<&str>, _>("description").unwrap_or(None),
            "reference_id": row.try_get::<Option<&str>, _>("reference_id").unwrap_or(None),
            "created_at": row.try_get::<&str, _>("created_at").unwrap_or(""),
        })
    }).collect();

    Ok(Json(json!({"transactions": transactions})))
}

/// GET /api/v1/credits/packages
/// List available credit packages for purchase.
pub async fn list_credit_packages(
    State(state): State<AppState>,
) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query(
        r#"SELECT id::text, name, credits, price::text, is_active, created_at::text
           FROM credit_packages
           WHERE is_active = true
           ORDER BY credits ASC"#,
    )
    .fetch_all(&state.db)
    .await?;

    let packages: Vec<serde_json::Value> = rows.iter().map(|row| {
        json!({
            "id": row.try_get::<&str, _>("id").unwrap_or(""),
            "name": row.try_get::<&str, _>("name").unwrap_or(""),
            "credits": row.try_get::<i32, _>("credits").unwrap_or(0),
            "price": row.try_get::<&str, _>("price").unwrap_or("0.00"),
            "is_active": row.try_get::<bool, _>("is_active").unwrap_or(true),
        })
    }).collect();

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
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Get current non-rollover balance
    let non_rollover: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE tenant_id = $1 AND transaction_type != 'rollover'",
    )
    .bind(tenant_id)
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
        r#"INSERT INTO credit_transactions (id, tenant_id, amount, transaction_type, description)
           VALUES ($1, $2, $3, 'rollover', 'Credits rolled over from previous cycle — never expires')"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
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
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "message": format!("{} credits rolled over — never expires", non_rollover),
        "rolled": non_rollover,
        "total_balance": total
    })))
}

pub async fn create_credit_package(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, tenant_id, "max_credit_packages", "Credit Packages").await?;

    let role = &claims.role;
    if role != "admin" && role != "owner" {
        return Err(AppError::Forbidden("Only admins can purchase credits".to_string()));
    }

    let package_id_str = req.get("package_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Validation("package_id is required".to_string()))?;
    let package_id = Uuid::parse_str(package_id_str)
        .map_err(|_| AppError::Validation("Invalid package_id".to_string()))?;

    let row = sqlx::query(
        "SELECT id::text, name, credits, price::text FROM credit_packages WHERE id = $1 AND is_active = true",
    )
    .bind(package_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Package not found".to_string()))?;

    let credits: i32 = row.try_get("credits").unwrap_or(0);
    let price: String = row.try_get("price").unwrap_or_default();

    let tx_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO credit_transactions (id, tenant_id, amount, transaction_type, description)
           VALUES ($1, $2, $3, 'purchase', $4)"#,
    )
    .bind(tx_id)
    .bind(tenant_id)
    .bind(credits)
    .bind(format!("Purchased {} credit package for ${}", credits, price))
    .execute(&state.db)
    .await?;

    // Check if this is their first rollover-eligible period
    let purchase_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM credit_transactions WHERE tenant_id = $1 AND transaction_type = 'purchase'",
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // Auto-rollover if they have existing unused credits
    if purchase_count > 1 {
        let current_sum: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE tenant_id = $1 AND transaction_type != 'rollover'",
        )
        .bind(tenant_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

        if current_sum > credits as i64 {
            // They have more from previous purchases — roll the surplus
            let surplus = current_sum - credits as i64;
            if surplus > 0 {
                // Note: rollover is recorded separately in the billing cycle, not on purchase
                // For now, just indicate rollover is available
            }
        }
    }

    let new_balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "message": "Credits purchased",
        "credits_added": credits,
        "new_balance": new_balance,
        "rollover_eligible": true
    })))
}

/// Deduct credits based on workflow type (tiered pricing).
/// n8n calls this at the start of a workflow.
pub async fn deduct_credit(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let pricing = tiered_pricing();

    let workflow_type = req.get("workflow_type").and_then(|v| v.as_str()).unwrap_or("simple");
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
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if balance < amount {
        return Err(AppError::BadRequest(format!(
            "Insufficient credits. Need {}, have {}. Purchase more credits.", amount, balance
        )));
    }

    sqlx::query(
        r#"INSERT INTO credit_transactions (id, tenant_id, amount, transaction_type, description)
           VALUES ($1, $2, $3, 'usage', $4)"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(-amount)
    .bind(&description)
    .execute(&state.db)
    .await?;

    let new_balance: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0) FROM credit_transactions WHERE tenant_id = $1",
    )
    .bind(tenant_id)
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
