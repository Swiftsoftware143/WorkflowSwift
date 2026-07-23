use axum::{
    extract::Path,
    extract::{State, Json},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::features;
use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::models::plan::*;

/// Fire-and-forget sync of a plan to FunnelSwift's affiliate_products.
async fn sync_plan_to_affiliate(
    config: &crate::config::AppConfig,
    action: &str,
    plan_name: &str,
    plan_price: f64,
    is_active: bool,
) {
    let url = format!("{}/api/v1/internal/sync-affiliate-plan", config.funnelswift_url.trim_end_matches('/'));
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
                    tracing::info!("sync-affiliate-plan {} {}: {}", action_owned, plan_name_owned, status);
                } else {
                    let body = resp.text().await.unwrap_or_default();
                    tracing::warn!("sync-affiliate-plan {} {} failed: {} - {}", action_owned, plan_name_owned, status, body);
                }
            }
            Err(e) => tracing::warn!("sync-affiliate-plan {} {} error: {}", action_owned, plan_name_owned, e),
        }
    });
}

pub async fn list_plans(
    State(state): State<AppState>,
) -> ApiResult<impl IntoResponse> {
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
        return Err(AppError::Forbidden("Only the super admin can create plans".to_string()));
    }

    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let slug = req.get("slug").and_then(|v| v.as_str()).unwrap_or(&name.to_lowercase().replace(' ', "-")).to_string();
    let description = req.get("description").and_then(|v| v.as_str());
    let price_monthly: Option<String> = req.get("price_monthly").and_then(|v| {
        v.as_str().map(|s| s.to_string())
            .or_else(|| v.as_f64().map(|n| n.to_string()))
    });
    let price_yearly: Option<String> = req.get("price_yearly").and_then(|v| {
        v.as_str().map(|s| s.to_string())
            .or_else(|| v.as_f64().map(|n| n.to_string()))
    });
    let features = req.get("features");

    if name.is_empty() {
        return Err(AppError::Validation("Plan name is required".to_string()));
    }

    let payment_provider = req.get("payment_provider").and_then(|v| v.as_str()).map(|s| s.to_string());

    // Capture price before it's moved into bind
    let plan_price_for_sync = price_monthly
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    let plan = sqlx::query_as::<_, PlanTier>(
        r#"INSERT INTO plan_tiers (id, name, slug, description, price_monthly, price_yearly, features, payment_provider)
           VALUES ($1, $2, $3, $4, $5::numeric, $6::numeric, $7::jsonb, $8)
           RETURNING *"#,
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

pub async fn update_plan(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    if !claims.perm_is_super_admin.unwrap_or(false) {
        return Err(AppError::Forbidden("Only the super admin can update plans".to_string()));
    }

    let existing = sqlx::query_as::<_, PlanTier>(
        "SELECT * FROM plan_tiers WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Plan not found".to_string()))?;

    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or(&existing.name).to_string();
    let slug = req.get("slug").and_then(|v| v.as_str()).unwrap_or(&existing.slug).to_string();
    let description = req.get("description").and_then(|v| v.as_str()).map(|s| s.to_string()).or(existing.description);
    let price_monthly = req.get("price_monthly").and_then(|v| v.as_str()).map(|s| s.to_string()).or(existing.price_monthly);
    let price_yearly = req.get("price_yearly").and_then(|v| v.as_str()).map(|s| s.to_string()).or(existing.price_yearly);
    let features = req.get("features").or(existing.features.as_ref());
    let checkout_url = req.get("checkout_url").and_then(|v| v.as_str()).map(|s| s.to_string()).or(existing.checkout_url);
    let is_active = req.get("is_active").and_then(|v| v.as_bool()).unwrap_or(existing.is_active);
    let sort_order = req.get("sort_order").and_then(|v| v.as_i64()).map(|v| v as i32).or(existing.sort_order);

    let payment_provider = req.get("payment_provider").and_then(|v| v.as_str()).map(|s| s.to_string()).or(existing.payment_provider.clone());

    // Capture price before it's moved into bind
    let plan_price_for_sync = price_monthly
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    sqlx::query(
        r#"UPDATE plan_tiers SET name=$1, slug=$2, description=$3, price_monthly=$4, price_yearly=$5,
           features=$6::jsonb, checkout_url=$7, is_active=$8, sort_order=$9, payment_provider=$10
           WHERE id=$11"#,
    )
    .bind(&name)
    .bind(&slug)
    .bind(&description)
    .bind(&price_monthly)
    .bind(&price_yearly)
    .bind(features)
    .bind(&checkout_url)
    .bind(is_active)
    .bind(sort_order)
    .bind(&payment_provider)
    .bind(id)
    .execute(&state.db)
    .await?;

    // Sync to FunnelSwift affiliate products
    let plan_name2 = name.clone();
    let config2 = state.config.clone();
    tokio::spawn(async move {
        sync_plan_to_affiliate(&config2, "update", &plan_name2, plan_price_for_sync, is_active).await;
    });

    Ok(Json(json!({"message": "Plan updated"})))
}

pub async fn delete_plan(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    if claims.role != "admin" && claims.role != "agency_admin" {
        return Err(AppError::Forbidden("Only admins can delete plans".to_string()));
    }

    // Get plan name before deleting for affiliate sync
    let plan_name = sqlx::query_scalar::<_, String>("SELECT name FROM plan_tiers WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_default();

    let result = sqlx::query("DELETE FROM plan_tiers WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Plan not found".to_string()));
    }

    // Sync deactivation to FunnelSwift affiliate products
    if !plan_name.is_empty() {
        let plan_name2 = plan_name.clone();
        let config2 = state.config.clone();
        tokio::spawn(async move {
            sync_plan_to_affiliate(&config2, "deactivate", &plan_name2, 0.0, false).await;
        });
    }

    Ok(Json(json!({"message": "Plan deleted"})))
}


// ── Admin plan management ──

pub async fn admin_list_all_plans(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    if claims.role != "agency_admin" && claims.role != "admin" {
        return Err(AppError::Forbidden("Only admins can list all plans".to_string()));
    }
    let plans = sqlx::query_as::<_, PlanTier>(
        "SELECT id, name, slug, description, price_monthly::text as price_monthly, price_yearly::text as price_yearly, features, checkout_url, is_active, sort_order, payment_provider, created_at FROM plan_tiers ORDER BY sort_order ASC NULLS LAST"
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({"plans": plans, "total": plans.len()})))
}

pub async fn admin_update_plan(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    if claims.role != "agency_admin" && claims.role != "admin" {
        return Err(AppError::Forbidden("Only admins can update plans".to_string()));
    }
    // Check plan exists
    let existing = sqlx::query_as::<_, PlanTier>(
        "SELECT id, name, slug, description, price_monthly::text as price_monthly, price_yearly::text as price_yearly, features, checkout_url, is_active, sort_order, payment_provider, created_at FROM plan_tiers WHERE id = $1::uuid"
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Plan not found".to_string()))?;

    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or(&existing.name).to_string();
    let slug = req.get("slug").and_then(|v| v.as_str()).unwrap_or(&existing.slug).to_string();
    let description = req.get("description").and_then(|v| v.as_str());
    let price_monthly = req.get("price_monthly").and_then(|v| v.as_str()).or(existing.price_monthly.as_deref()).unwrap_or("0.00");
    let price_yearly = req.get("price_yearly").and_then(|v| v.as_str()).or(existing.price_yearly.as_deref()).unwrap_or("0.00");
    let features = req.get("features").cloned().or_else(|| existing.features.clone());

    let payment_provider = req.get("payment_provider").and_then(|v| v.as_str()).map(|s| s.to_string()).or(existing.payment_provider.clone());

    sqlx::query(
        r#"UPDATE plan_tiers SET name=$1, slug=$2, description=$3, price_monthly=$4::numeric, price_yearly=$5::numeric, features=$6::jsonb, payment_provider=$7 WHERE id=$8::uuid"#,
    )
    .bind(&name)
    .bind(&slug)
    .bind(description)
    .bind(price_monthly)
    .bind(price_yearly)
    .bind(&features)
    .bind(&payment_provider)
    .bind(&id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({"message": "Plan updated"})))
}

pub async fn admin_get_plan(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    if claims.role != "agency_admin" && claims.role != "admin" {
        return Err(AppError::Forbidden("Only admins can view plans".to_string()));
    }
    let plan = sqlx::query_as::<_, PlanTier>(
        "SELECT id, name, slug, description, price_monthly::text as price_monthly, price_yearly::text as price_yearly, features, checkout_url, is_active, sort_order, payment_provider, created_at FROM plan_tiers WHERE id = $1::uuid"
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Plan not found".to_string()))?;

    Ok(Json(json!({"plan": plan})))
}

pub async fn admin_update_plan_features(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    if claims.role != "agency_admin" && claims.role != "admin" {
        return Err(AppError::Forbidden("Admin access required".to_string()));
    }
    let features = req.get("features").ok_or_else(|| AppError::Validation("features object is required".to_string()))?;
    let features_json = features.to_string();

    sqlx::query(
        r#"UPDATE plan_tiers SET features = COALESCE(features, '{}'::jsonb) || $1::jsonb WHERE id = $2::uuid"#,
    )
    .bind(&features_json)
    .bind(&id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({"message": "Features updated"})))
}

/// GET /api/v1/plans/capabilities — get what industries the user's plan supports
pub async fn get_plan_capabilities(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Find user's active plan
    let plan_info = sqlx::query(
        r#"SELECT p.id, p.slug, p.name, p.features::text
           FROM account_plans tp
           JOIN plan_tiers p ON p.id = tp.plan_id
           WHERE tp.aid = $1 AND tp.status = 'active'
           ORDER BY tp.created_at DESC LIMIT 1"#
    )
    .bind(aid)
    .fetch_optional(&state.db)
    .await?;

    let (plan_id, plan_slug, plan_name, _plan_features) = match plan_info {
        Some(row) => {
            let pid: Uuid = row.try_get("id").unwrap_or_default();
            let slug: String = row.try_get("slug").unwrap_or_else(|_| "free".to_string());
            let name: String = row.try_get("name").unwrap_or_else(|_| "Free".to_string());
            let feats: String = row.try_get("features").unwrap_or_else(|_| "{}".to_string());
            (pid, slug, name, feats)
        },
        None => {
            // Default to Free plan capabilities
            let pid: Uuid = sqlx::query_scalar(
                "SELECT id FROM plan_tiers WHERE slug = 'free' AND is_active = true LIMIT 1"
            )
            .fetch_optional(&state.db)
            .await?
            .flatten()
            .unwrap_or(Uuid::nil());
            (pid, "free".to_string(), "Free".to_string(), "{}".to_string())
        }
    };

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
           ORDER BY tc.sort_order"#
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
        "SELECT COUNT(*) FROM account_industries WHERE aid = $1 AND is_active = true"
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

pub async fn admin_assign_plan(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    if claims.role != "agency_admin" && claims.role != "admin" {
        return Err(AppError::Forbidden("Admin access required".to_string()));
    }
    let target_aid = req.get("aid").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::Validation("Valid aid is required".to_string()))?;
    let plan_id = req.get("plan_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::Validation("Valid plan_id is required".to_string()))?;
    let _billing_cycle = req.get("billing_cycle").and_then(|v| v.as_str()).unwrap_or("monthly");

    let _result = sqlx::query(
        r#"INSERT INTO account_plans (id, aid, plan_id, status, started_at)
           VALUES ($1, $2, $3, 'active', NOW())
           ON CONFLICT (aid) DO UPDATE SET plan_id = $3, status = 'active', started_at = NOW()
           RETURNING id, aid, plan_id, status"#,
    )
    .bind(Uuid::new_v4())
    .bind(target_aid)
    .bind(plan_id)
    .fetch_optional(&state.db)
    .await?;

    Ok(Json(json!({"message": "Plan assigned", "assignment": json!({"status": "active"})})))
}
