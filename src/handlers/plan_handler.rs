use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::features;
use crate::models::plan::*;
use crate::AppState;

/// Fire-and-forget sync of a plan to FunnelSwift's affiliate_products.
async fn sync_plan_to_affiliate(
    config: &crate::config::AppConfig,
    action: &str,
    plan_name: &str,
    plan_price: f64,
    is_active: bool,
) {
    let url = format!(
        "{}/api/v1/internal/sync-affiliate-plan",
        config.funnelswift_url.trim_end_matches('/')
    );
    let api_key = config.internal_sync_key.clone();

    let action_owned = action.to_string();
    let plan_name_owned = plan_name.to_string();

    let payload = serde_json::json!({
        "action": &action_owned,
        "plan_name": &plan_name_owned,
        "plan_price": plan_price,
        "source_app": "workflowswift",
        "is_active": is_active,
        "owner_name": "SwiftSoftware",
        "product_type": "software",
        "api_key": &api_key,
    });

    tokio::spawn(async move {
        match reqwest::Client::new()
            .post(&url)
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    tracing::info!(
                        "sync-affiliate-plan {} {}: {}",
                        action_owned,
                        plan_name_owned,
                        status
                    );
                } else {
                    let body = resp.text().await.unwrap_or_default();
                    tracing::warn!(
                        "sync-affiliate-plan {} {} failed: {} - {}",
                        action_owned,
                        plan_name_owned,
                        status,
                        body
                    );
                }
            }
            Err(e) => tracing::warn!(
                "sync-affiliate-plan {} {} error: {}",
                action_owned,
                plan_name_owned,
                e
            ),
        }
    });
}

pub async fn list_plans(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let plans = sqlx::query_as::<_, PlanTier>(
        "SELECT id, name, slug, description, price_monthly::text as price_monthly, price_yearly::text as price_yearly, features, checkout_url, is_active, sort_order, payment_provider, created_at FROM plan_tiers WHERE is_active = true ORDER BY sort_order ASC NULLS LAST",
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"plans": plans})))
}

pub async fn create_plan(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_plans", "Plans").await?;
    if !claims.perm_is_super_admin.unwrap_or(false) {
        return Err(AppError::Forbidden(
            "Only the super admin can create plans".to_string(),
        ));
    }

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let slug = req
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or(&name.to_lowercase().replace(' ', "-"))
        .to_string();
    let description = req.get("description").and_then(|v| v.as_str());
    let price_monthly: Option<String> = req.get("price_monthly").and_then(|v| {
        v.as_str()
            .map(|s| s.to_string())
            .or_else(|| v.as_f64().map(|n| n.to_string()))
    });
    let price_yearly: Option<String> = req.get("price_yearly").and_then(|v| {
        v.as_str()
            .map(|s| s.to_string())
            .or_else(|| v.as_f64().map(|n| n.to_string()))
    });
    let features = req.get("features");

    if name.is_empty() {
        return Err(AppError::Validation("Plan name is required".to_string()));
    }

    let payment_provider = req
        .get("payment_provider")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Capture price before it's moved into bind
    let plan_price_for_sync = price_monthly
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    // NOTE: `plan_tiers.price_monthly/price_yearly` are NUMERIC(10,2) and sqlx has no
    // String/f64 decode for numeric at all, so EVERY read of this struct must cast the
    // column (`price_monthly::text`) — an INSERT/UPDATE ... RETURNING list must therefore
    // name its columns instead of using `RETURNING *` (kanban t_3d0c5623).
    let plan = sqlx::query_as::<_, PlanTier>(
        r#"INSERT INTO plan_tiers (id, name, slug, description, price_monthly, price_yearly, features, payment_provider)
           VALUES ($1, $2, $3, $4, $5::numeric, $6::numeric, $7::jsonb, $8)
           RETURNING id, name, slug, description, price_monthly::text as price_monthly, price_yearly::text as price_yearly, features, checkout_url, is_active, sort_order, payment_provider, created_at"#,
    )
    .bind(Uuid::new_v4())
    .bind(&name)
    .bind(&slug)
    .bind(description)
    .bind(price_monthly)
    .bind(price_yearly)
    .bind(features)
    .bind(&payment_provider)
    .fetch_one(&state.db)
    .await?;

    // Sync to FunnelSwift affiliate products
    let plan_name2 = name.clone();
    let config2 = state.config.clone();
    tokio::spawn(async move {
        sync_plan_to_affiliate(&config2, "create", &plan_name2, plan_price_for_sync, true).await;
    });

    Ok((StatusCode::CREATED, Json(json!({"plan": plan}))))
}

// ── Removed: the six unmounted legacy plan handlers (kanban t_647203c7) ──
//
// `delete_plan`, `admin_list_all_plans`, `admin_update_plan`, `admin_get_plan`,
// `admin_update_plan_features` and the equally unreachable `update_plan` used to live here.
// Not one of them was reachable: `grep -rn 'plan_handler::' src/` resolves only `list_plans`,
// `create_plan` and `get_plan_capabilities` (routes.rs:157,161); all six had 0 references
// anywhere in the crate, and the linker had already dropped their string literals from the
// shipped binary (`grep -a -o 'Only admins can view plans' target/release/workflowswift-api`
// -> 0, while every reachable marker is present).
//
// They were the ONLY statements in the crate that authorized on the role LITERAL
// `role == "admin"` / `"agency_admin"`, and production has no `role='admin'` row (t_2255fdea
// measured that and decided to leave it un-normalised). Dead source is a latch: mounting one
// later — the obvious temptation, since `admin_list_all_plans` reads like the console's plan
// list — would have made that literal load-bearing again and 403'd the owner, because the
// platform admin is `perm_is_super_admin`, not `role='admin'`. Deleting them makes the
// t_2255fdea decision self-enforcing: `role='admin'` now has zero readers in the crate.
//
// The live admin plan CRUD the served console calls is `admin_settings_handler::*`
// (routes.rs:696-702, gated on `perm_is_super_admin`). The mounted trio stays below.

/// GET /api/v1/plans/capabilities — get what industries the user's plan supports
pub async fn get_plan_capabilities(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Resolve the account's plan through the app's ONE resolver, so this display surface and the
    // entitlement path (`features::enforce_plan_flag`) can never disagree about which tier an
    // account is on. They did disagree: this function used to hand-roll the active-row lookup and,
    // when an account had NO `account_plans` row, it hard-coded the `free` tier — while
    // `features::resolve_plan_id` consults `accounts.plan_id` first and only then falls back to
    // the first active tier. Measured live on a probe account (t_3914ee20): no row,
    // `accounts.plan_id = professional`, this endpoint answered `free` while the api_access gate
    // answered `professional`.
    let resolved = features::resolve_plan_id(&state.db, aid).await?;
    let plan_id: Uuid = resolved.unwrap_or(Uuid::nil());

    // `plan_tiers.features` is NULLABLE with DEFAULT '{}'::jsonb, so the missing value is the
    // table's OWN default (kanban t_4a499b90); a bare `features::text` would fail the row decode
    // (`decoding column 2: unexpected null`) for a row that lost its default.
    let plan_info: Option<(String, String, String)> = sqlx::query_as(
        "SELECT slug, name, COALESCE(features, '{}'::jsonb)::text FROM plan_tiers WHERE id = $1",
    )
    .bind(plan_id)
    .fetch_optional(&state.db)
    .await?;

    let (plan_slug, plan_name, _plan_features) =
        plan_info.unwrap_or_else(|| ("free".to_string(), "Free".to_string(), "{}".to_string()));

    // Get max industries from feature_limits
    let max_industries: i32 = sqlx::query_scalar(
        "SELECT limit_value FROM feature_limits WHERE plan_id = $1 AND feature_key = 'max_industries'"
    )
    .bind(plan_id)
    .fetch_optional(&state.db)
    .await?
    .flatten()
    .unwrap_or(1);

    // Get which industries this plan supports
    let industry_rows = sqlx::query(
        r#"SELECT tc.slug, tc.name, tc.icon, tc.description
           FROM plan_capabilities pc
           JOIN template_categories tc ON tc.slug = pc.industry_slug
           WHERE pc.plan_id = $1 AND pc.is_active = true AND tc.is_active = true
           ORDER BY tc.sort_order"#,
    )
    .bind(plan_id)
    .fetch_all(&state.db)
    .await?;

    let mut supported_industries = Vec::new();
    for row in industry_rows {
        let slug: String = row.try_get("slug").unwrap_or_default();
        let name: String = row.try_get("name").unwrap_or_default();
        let icon: Option<String> = row.try_get("icon").ok();
        let desc: Option<String> = row.try_get("description").ok();
        supported_industries.push(json!({
            "slug": slug,
            "name": name,
            "icon": icon,
            "description": desc
        }));
    }

    // Get current usage count
    let current_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM account_industries WHERE aid = $1 AND is_active = true",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "plan_id": plan_id.to_string(),
        "plan_slug": plan_slug,
        "plan_name": plan_name,
        "max_industries": max_industries,
        "current_industries": current_count,
        "industries_remaining": if max_industries == -1 { -1 } else { (max_industries as i64) - current_count },
        "supported_industries": supported_industries
    })))
}

/// Parse a plan price stored as a string (e.g. "29.00" or "$29") into f64.
fn parse_plan_price(price: &Option<String>) -> f64 {
    price
        .as_deref()
        .map(|v| {
            v.chars()
                .filter(|c| c.is_ascii_digit() || *c == '.')
                .collect::<String>()
        })
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// Fire-and-forget notification to FunnelSwift that a user upgraded to a paid plan,
/// so the referring affiliate is credited (permanent, no expiry).
pub(crate) async fn notify_funnelswift_upgrade(
    config: &crate::config::AppConfig,
    email: &str,
    plan_name: &str,
    plan_price: f64,
    event_id: &str,
) {
    if email.is_empty() || config.funnelswift_url.is_empty() {
        return;
    }
    let url = format!(
        "{}/api/v1/internal/affiliate/upgrade-event",
        config.funnelswift_url.trim_end_matches('/')
    );
    let key = config.internal_sync_key.clone();
    let payload = serde_json::json!({
        "source_app": "workflowswift",
        "email": email,
        "plan_name": plan_name,
        "plan_price": plan_price,
        "event_id": event_id,
    });
    tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(&url)
            .header("x-internal-key", key)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;
    });
}

/// Resolve the account's email + plan, and notify FunnelSwift if it's a PAID upgrade.
/// Free-plan assignment is the initial attribution (handled at tag time), not an upgrade.
pub(crate) async fn attribute_plan_upgrade(state: &AppState, aid: Uuid, plan_id: Uuid) {
    let plan: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT name, price_monthly::text FROM plan_tiers WHERE id = $1")
            .bind(plan_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let Some((plan_name, price)) = plan else {
        return;
    };
    let plan_price = parse_plan_price(&price);
    if plan_price <= 0.0 {
        return;
    }
    let email: Option<String> =
        sqlx::query_scalar("SELECT email FROM users WHERE aid = $1 LIMIT 1")
            .bind(aid)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let Some(email) = email else {
        return;
    };
    notify_funnelswift_upgrade(
        &state.config,
        &email,
        &plan_name,
        plan_price,
        &Uuid::new_v4().to_string(),
    )
    .await;
}
