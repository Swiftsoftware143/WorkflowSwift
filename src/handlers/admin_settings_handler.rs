use axum::{
    extract::{State, Json, Path},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;

/// GET /api/v1/admin/settings — list all admin settings
pub async fn list_settings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let rows = sqlx::query(
        "SELECT key, value, description, updated_at FROM admin_settings ORDER BY key"
    )
    .fetch_all(&state.db)
    .await?;

    let mut settings = serde_json::Map::new();
    for row in &rows {
        let key: String = row.try_get("key")?;
        let value: serde_json::Value = row.try_get("value")?;
        let description: Option<String> = row.try_get("description").ok();
        let updated_at: chrono::DateTime<chrono::Utc> = row.try_get("updated_at")?;
        settings.insert(key, json!({
            "value": value,
            "description": description,
            "updated_at": updated_at.to_rfc3339()
        }));
    }

    Ok(Json(json!({"settings": settings})))
}

/// GET /api/v1/admin/settings/:key — get a specific setting
pub async fn get_setting(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(key): Path<String>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let row = sqlx::query(
        "SELECT key, value, description, updated_at FROM admin_settings WHERE key = $1"
    )
    .bind(&key)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Setting '{}' not found", key)))?;

    let key_str: String = row.try_get("key")?;
    let value: serde_json::Value = row.try_get("value")?;
    let description: Option<String> = row.try_get("description").ok();
    let updated_at: chrono::DateTime<chrono::Utc> = row.try_get("updated_at")?;

    Ok(Json(json!({
        "key": key_str,
        "value": value,
        "description": description,
        "updated_at": updated_at.to_rfc3339()
    })))
}

/// PUT /api/v1/admin/settings/:key — update a setting
pub async fn update_setting(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(key): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let value = req.get("value").ok_or_else(|| AppError::Validation("value is required".to_string()))?;
    let description = req.get("description").and_then(|v| v.as_str());

    let admin_id = Uuid::parse_str(&claims.sub).unwrap_or(Uuid::nil());

    sqlx::query(
        r#"INSERT INTO admin_settings (key, value, description, updated_at, updated_by)
           VALUES ($1, $2::jsonb, $3, NOW(), $4)
           ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, description = COALESCE($3, admin_settings.description), updated_at = NOW(), updated_by = $4"#
    )
    .bind(&key)
    .bind(value.to_string())
    .bind(description)
    .bind(admin_id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({"message": "Setting updated", "key": key})))
}

// ── Retention Settings (convenience wrapper) ──

/// GET /api/v1/admin/retention — get retention policy
pub async fn get_retention_policy(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let row = sqlx::query(
        "SELECT value, updated_at FROM admin_settings WHERE key = 'retention'"
    )
    .fetch_optional(&state.db)
    .await?;

    let policy = match row {
        Some(r) => {
            let val: serde_json::Value = r.try_get("value")?;
            let updated: chrono::DateTime<chrono::Utc> = r.try_get("updated_at")?;
            json!({
                "policy": val,
                "updated_at": updated.to_rfc3339()
            })
        }
        None => json!({
            "policy": {"default_days": 90, "max_days": 365, "min_days": 1},
            "updated_at": null
        })
    };

    Ok(Json(policy))
}

/// PUT /api/v1/admin/retention — update retention policy
pub async fn update_retention_policy(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let default_days = req.get("default_days").and_then(|v| v.as_i64()).unwrap_or(90);
    let max_days = req.get("max_days").and_then(|v| v.as_i64()).unwrap_or(365);
    let min_days = req.get("min_days").and_then(|v| v.as_i64()).unwrap_or(1);

    let admin_id = Uuid::parse_str(&claims.sub).unwrap_or(Uuid::nil());

    sqlx::query(
        r#"INSERT INTO admin_settings (key, value, description, updated_at, updated_by)
           VALUES ('retention', $1::jsonb, 'Global data retention policy', NOW(), $2)
           ON CONFLICT (key) DO UPDATE SET value = $1::jsonb, updated_at = NOW(), updated_by = $2"#
    )
    .bind(json!({
        "default_days": default_days,
        "max_days": max_days,
        "min_days": min_days
    }).to_string())
    .bind(admin_id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "message": "Retention policy updated",
        "policy": {
            "default_days": default_days,
            "max_days": max_days,
            "min_days": min_days
        }
    })))
}

// ── Plan Feature Definitions ──

/// GET /api/v1/admin/feature-definitions — list all definable features
pub async fn list_feature_definitions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let rows = sqlx::query(
        "SELECT key, label, description, value_type, default_value, unit, category, sort_order, is_visible
         FROM plan_feature_definitions ORDER BY category, sort_order"
    )
    .fetch_all(&state.db)
    .await?;

    let mut features = Vec::new();
    for row in &rows {
        features.push(json!({
            "key": row.try_get::<String, _>("key")?,
            "label": row.try_get::<String, _>("label")?,
            "description": row.try_get::<Option<String>, _>("description")?,
            "value_type": row.try_get::<String, _>("value_type")?,
            "default_value": row.try_get::<serde_json::Value, _>("default_value")?,
            "unit": row.try_get::<Option<String>, _>("unit")?,
            "category": row.try_get::<String, _>("category")?,
            "sort_order": row.try_get::<i32, _>("sort_order")?,
            "is_visible": row.try_get::<bool, _>("is_visible")?
        }));
    }

    Ok(Json(json!({"features": features})))
}

// ── Plan Management (admin, full control) ──

/// GET /api/v1/admin/plans — list all plans with full details
pub async fn admin_list_plans(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let plans = sqlx::query(
        "SELECT id, name, slug, description, price_monthly::text as price_monthly,
                price_yearly::text as price_yearly, features, checkout_url,
                is_active, sort_order, created_at,
                max_workflows, max_users, retention_days, can_export, can_deploy_n8n, has_api_access
         FROM plan_tiers ORDER BY sort_order ASC NULLS LAST"
    )
    .fetch_all(&state.db)
    .await?;

    let mut result = Vec::new();
    for row in &plans {
        let id: Uuid = row.try_get("id")?;
        let name: String = row.try_get("name")?;
        let slug: String = row.try_get("slug")?;
        let description: Option<String> = row.try_get("description").ok();
        let price_monthly: Option<String> = row.try_get("price_monthly").ok();
        let price_yearly: Option<String> = row.try_get("price_yearly").ok();
        let features: Option<serde_json::Value> = row.try_get("features").ok();
        let checkout_url: Option<String> = row.try_get("checkout_url").ok();
        let is_active: bool = row.try_get("is_active")?;
        let sort_order: Option<i32> = row.try_get("sort_order").ok();
        let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
        let max_workflows: Option<i32> = row.try_get("max_workflows").ok();
        let max_users: Option<i32> = row.try_get("max_users").ok();
        let retention_days: Option<i32> = row.try_get("retention_days").ok();
        let can_export: Option<bool> = row.try_get("can_export").ok();
        let can_deploy_n8n: Option<bool> = row.try_get("can_deploy_n8n").ok();
        let has_api_access: Option<bool> = row.try_get("has_api_access").ok();

        // Get feature_limits for this plan
        let limits = sqlx::query(
            "SELECT feature_key, limit_value FROM feature_limits WHERE plan_id = $1"
        )
        .bind(id)
        .fetch_all(&state.db)
        .await?;

        let mut limits_map = serde_json::Map::new();
        for lr in &limits {
            let fk: String = lr.try_get("feature_key")?;
            let lv: i32 = lr.try_get("limit_value")?;
            limits_map.insert(fk, json!(lv));
        }

        result.push(json!({
            "id": id.to_string(),
            "name": name,
            "slug": slug,
            "description": description,
            "price_monthly": price_monthly,
            "price_yearly": price_yearly,
            "features": features,
            "checkout_url": checkout_url,
            "is_active": is_active,
            "sort_order": sort_order,
            "created_at": created_at.to_rfc3339(),
            "max_workflows": max_workflows,
            "max_users": max_users,
            "retention_days": retention_days,
            "can_export": can_export,
            "can_deploy_n8n": can_deploy_n8n,
            "has_api_access": has_api_access,
            "feature_limits": limits_map
        }));
    }

    Ok(Json(json!({"plans": result})))
}

/// POST /api/v1/admin/plans — create a new plan
pub async fn admin_create_plan(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let name = req.get("name").and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Validation("Plan name is required".to_string()))?;
    let slug_default = name.to_lowercase().replace(' ', "-");
    let slug = req.get("slug").and_then(|v| v.as_str()).unwrap_or(&slug_default);
    let description = req.get("description").and_then(|v| v.as_str());
    let price_monthly = req.get("price_monthly").and_then(|v| {
        v.as_str().map(|s| s.to_string())
            .or_else(|| v.as_f64().map(|n| format!("{:.2}", n)))
    });
    let price_yearly = req.get("price_yearly").and_then(|v| {
        v.as_str().map(|s| s.to_string())
            .or_else(|| v.as_f64().map(|n| format!("{:.2}", n)))
    });
    let features = req.get("features").cloned();
    let is_active = req.get("is_active").and_then(|v| v.as_bool()).unwrap_or(true);
    let sort_order = req.get("sort_order").and_then(|v| v.as_i64()).map(|v| v as i32);

    let features_json = features.unwrap_or(json!({}));

    let plan = sqlx::query(
        r#"INSERT INTO plan_tiers (id, name, slug, description, price_monthly, price_yearly, features, is_active, sort_order)
           VALUES ($1, $2, $3, $4, $5::numeric, $6::numeric, $7::jsonb, $8, $9)
           RETURNING id, name, slug"#,
    )
    .bind(Uuid::new_v4())
    .bind(name)
    .bind(slug)
    .bind(description)
    .bind(price_monthly)
    .bind(price_yearly)
    .bind(features_json.to_string())
    .bind(is_active)
    .bind(sort_order)
    .fetch_one(&state.db)
    .await?;

    let plan_id: Uuid = plan.try_get("id")?;
    let plan_name: String = plan.try_get("name")?;

    Ok((StatusCode::CREATED, Json(json!({
        "message": "Plan created",
        "plan": { "id": plan_id.to_string(), "name": plan_name, "slug": slug }
    }))))
}

/// PUT /api/v1/admin/plans/:id — full plan update
pub async fn admin_update_plan_full(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    // Check exists
    let _existing = sqlx::query("SELECT id, name, slug FROM plan_tiers WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Plan not found".to_string()))?;

    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let slug = req.get("slug").and_then(|v| v.as_str()).unwrap_or("");
    let description = req.get("description").and_then(|v| v.as_str());
    let price_monthly = req.get("price_monthly").and_then(|v| {
        v.as_str().map(|s| s.to_string())
            .or_else(|| v.as_f64().map(|n| format!("{:.2}", n)))
    });
    let price_yearly = req.get("price_yearly").and_then(|v| {
        v.as_str().map(|s| s.to_string())
            .or_else(|| v.as_f64().map(|n| format!("{:.2}", n)))
    });
    let features = req.get("features").and_then(|v| v.as_object());
    let is_active = req.get("is_active").and_then(|v| v.as_bool());
    let sort_order = req.get("sort_order").and_then(|v| v.as_i64()).map(|v| v as i32);
    let max_workflows = req.get("max_workflows").and_then(|v| v.as_i64()).map(|v| v as i32);
    let max_users = req.get("max_users").and_then(|v| v.as_i64()).map(|v| v as i32);
    let retention_days = req.get("retention_days").and_then(|v| v.as_i64()).map(|v| v as i32);
    let can_export = req.get("can_export").and_then(|v| v.as_bool());
    let can_deploy_n8n = req.get("can_deploy_n8n").and_then(|v| v.as_bool());
    let has_api_access = req.get("has_api_access").and_then(|v| v.as_bool());

    let name_use = if name.is_empty() {
        let n: String = sqlx::query_scalar("SELECT name FROM plan_tiers WHERE id = $1")
            .bind(id).fetch_one(&state.db).await?;
        n
    } else { name.to_string() };

    let slug_use = if slug.is_empty() {
        let s: String = sqlx::query_scalar("SELECT slug FROM plan_tiers WHERE id = $1")
            .bind(id).fetch_one(&state.db).await?;
        s
    } else { slug.to_string() };

    sqlx::query(
        r#"UPDATE plan_tiers SET
            name = COALESCE($1, name),
            slug = COALESCE($2, slug),
            description = COALESCE($3, description),
            price_monthly = COALESCE($4::numeric, price_monthly),
            price_yearly = COALESCE($5::numeric, price_yearly),
            is_active = COALESCE($6, is_active),
            sort_order = COALESCE($7, sort_order),
            max_workflows = COALESCE($8, max_workflows),
            max_users = COALESCE($9, max_users),
            retention_days = COALESCE($10, retention_days),
            can_export = COALESCE($11, can_export),
            can_deploy_n8n = COALESCE($12, can_deploy_n8n),
            has_api_access = COALESCE($13, has_api_access)
         WHERE id = $14"#
    )
    .bind(if name.is_empty() { None } else { Some(&name_use) })
    .bind(if slug.is_empty() { None } else { Some(&slug_use) })
    .bind(description)
    .bind(&price_monthly)
    .bind(&price_yearly)
    .bind(is_active)
    .bind(sort_order)
    .bind(max_workflows)
    .bind(max_users)
    .bind(retention_days)
    .bind(can_export)
    .bind(can_deploy_n8n)
    .bind(has_api_access)
    .bind(id)
    .execute(&state.db)
    .await?;

    // Update feature limits if provided
    if let Some(feature_overrides) = req.get("feature_limits").and_then(|v| v.as_object()) {
        for (fkey, fval) in feature_overrides {
            if let Some(limit) = fval.as_i64() {
                sqlx::query(
                    r#"INSERT INTO feature_limits (plan_id, feature_key, limit_value)
                       VALUES ($1, $2, $3)
                       ON CONFLICT (plan_id, feature_key) DO UPDATE SET limit_value = $3"#
                )
                .bind(id)
                .bind(fkey)
                .bind(limit as i32)
                .execute(&state.db)
                .await?;
            }
        }
    }

    // Update features JSON if provided
    if let Some(feats) = features {
        sqlx::query(
            "UPDATE plan_tiers SET features = $1::jsonb WHERE id = $2"
        )
        .bind(serde_json::Value::Object(feats.clone()).to_string())
        .bind(id)
        .execute(&state.db)
        .await?;
    }

    Ok(Json(json!({"message": "Plan updated", "plan_id": id.to_string()})))
}

/// DELETE /api/v1/admin/plans/:id — delete a plan
pub async fn admin_delete_plan(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let result = sqlx::query("DELETE FROM plan_tiers WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Plan not found".to_string()));
    }

    Ok(Json(json!({"message": "Plan deleted"})))
}

// ── Account-level retention override ──

/// GET /api/v1/admin/accounts — list accounts with retention info
pub async fn admin_list_accounts(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let accounts = sqlx::query(
        r#"SELECT a.id, a.name, a.account_slug, a.is_active, a.retention_days, a.retention_purge_at,
                  a.created_at,
                  p.name as plan_name, p.slug as plan_slug
           FROM accounts a
           LEFT JOIN account_plans ap ON ap.aid = a.id AND ap.status = 'active'
           LEFT JOIN plan_tiers p ON p.id = ap.plan_id
           ORDER BY a.created_at DESC"#
    )
    .fetch_all(&state.db)
    .await?;

    let mut result = Vec::new();
    for row in &accounts {
        let id: Uuid = row.try_get("id")?;
        let name: Option<String> = row.try_get("name").ok();
        let email: Option<String> = None;
        let slug: Option<String> = row.try_get("account_slug").ok();
        let is_active: bool = row.try_get("is_active")?;
        let retention_days: Option<i32> = row.try_get("retention_days").ok();
        let retention_purge_at: Option<chrono::DateTime<chrono::Utc>> = row.try_get("retention_purge_at").ok();
        let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
        let plan_name: Option<String> = row.try_get("plan_name").ok();
        let plan_slug: Option<String> = row.try_get("plan_slug").ok();

        result.push(json!({
            "id": id.to_string(),
            "name": name,
            "email": email,
            "slug": slug,
            "is_active": is_active,
            "retention_days": retention_days,
            "retention_purge_at": retention_purge_at.map(|t| t.to_rfc3339()),
            "created_at": created_at.to_rfc3339(),
            "plan": plan_name.map(|n| json!({"name": n, "slug": plan_slug}))
        }));
    }

    Ok(Json(json!({"accounts": result})))
}

/// PUT /api/v1/admin/accounts/:id/retention — override account retention
pub async fn admin_set_account_retention(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let retention_days = req.get("retention_days").and_then(|v| v.as_i64())
        .ok_or_else(|| AppError::Validation("retention_days is required".to_string()))? as i32;

    let purge_at = req.get("purge_at")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    if retention_days < 1 {
        return Err(AppError::Validation("retention_days must be at least 1".to_string()));
    }

    sqlx::query(
        "UPDATE accounts SET retention_days = $1, retention_purge_at = $2 WHERE id = $3"
    )
    .bind(retention_days)
    .bind(purge_at)
    .bind(id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "message": "Account retention updated",
        "retention_days": retention_days
    })))
}

// ── Admin Account Creation ──

/// POST /api/v1/admin/accounts/create
/// Only super_admin can call this. Creates a new account + user with role="user",
/// assigns a plan, and sends a welcome email with login credentials.
pub async fn admin_create_account(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let email = req.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let account_name = req.get("account_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let plan_slug = req.get("plan_slug").and_then(|v| v.as_str()).unwrap_or("starter").to_string();
    let industry_slug = req.get("industry_slug").and_then(|v| v.as_str()).unwrap_or("").to_string();

    if name.is_empty() || email.is_empty() || account_name.is_empty() {
        return Err(AppError::Validation("name, email, and account_name are required".to_string()));
    }

    // Check for existing user by email
    let existing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE email = $1")
        .bind(&email)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

    if existing > 0 {
        return Err(AppError::Duplicate("A user with this email already exists".to_string()));
    }

    // Create the account
    let account_id = Uuid::new_v4();
    let slug = req.get("account_slug")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            account_name.to_lowercase()
                .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
                .trim_matches('-')
                .to_string()
        });

    sqlx::query(
        r#"INSERT INTO accounts (id, name, slug)
           VALUES ($1, $2, $3)"#
    )
    .bind(account_id)
    .bind(&account_name)
    .bind(&slug)
    .execute(&state.db)
    .await?;

    // Resolve plan_id from slug or use default
    let plan_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM plan_tiers WHERE slug = $1 AND is_active = true"
    )
    .bind(&plan_slug)
    .fetch_optional(&state.db)
    .await?;

    // Assign plan to account (via account_plans if found)
    if let Some(pid) = plan_id {
        sqlx::query(
            r#"INSERT INTO account_plans (aid, plan_id, status, started_at)
               VALUES ($1, $2, 'active', NOW())
               ON CONFLICT (aid, plan_id) DO UPDATE SET status = 'active'"#
        )
        .bind(account_id)
        .bind(pid)
        .execute(&state.db)
        .await?;
    }

    // Set industry if provided
    if !industry_slug.is_empty() {
        let industry_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM industries WHERE slug = $1"
        )
        .bind(&industry_slug)
        .fetch_optional(&state.db)
        .await?
        .flatten();

        if let Some(ind_id) = industry_id {
            let existing_industry = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM account_industries WHERE account_id = $1 AND industry_id = $2"
            )
            .bind(account_id)
            .bind(ind_id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

            if existing_industry == 0 {
                sqlx::query(
                    "INSERT INTO account_industries (account_id, industry_id) VALUES ($1, $2)"
                )
                .bind(account_id)
                .bind(ind_id)
                .execute(&state.db)
                .await?;
            }
        }
    }

    // Generate temp password and create user
    use argon2::{Argon2, PasswordHasher};
    use argon2::password_hash::SaltString;
    use rand::rngs::OsRng;

    let temp_password = Uuid::new_v4().to_string();
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(temp_password.as_bytes(), &salt)
        .map_err(|e| AppError::Hash(e.to_string()))?
        .to_string();

    let user_id = Uuid::new_v4();
    let now = chrono::Utc::now();

    sqlx::query(
        r#"INSERT INTO users (id, aid, email, password_hash, name, role, is_active, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, 'user', true, $6, $6)"#,
    )
    .bind(user_id)
    .bind(account_id)
    .bind(&email)
    .bind(&hash)
    .bind(&name)
    .bind(now)
    .execute(&state.db)
    .await?;

    // Send welcome email
    let email_result = crate::email::send_email(
        &state,
        &email,
        "welcome",
        &json!({
            "name": name,
            "email": email,
            "password": temp_password,
            "app_url": "https://app.workflowswift.com",
        }),
    ).await;

    match email_result {
        Ok(_) => tracing::info!("Welcome email sent to {}", email),
        Err(ref e) => tracing::error!("Failed to send welcome email to {}: {}", email, e),
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "status": "created",
            "account_id": account_id,
            "user_id": user_id,
            "email": email,
            "temporary_password": temp_password,
            "email_sent": email_result.as_ref().ok().is_some(),
        })),
    ))
}

// ── Admin Email Template Management ──

/// GET /api/v1/admin/email-templates — list all email templates
pub async fn admin_list_email_templates(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let rows = sqlx::query(
        r#"SELECT id, name, subject, template_type, is_html, is_default, created_at, updated_at
           FROM email_templates ORDER BY name ASC"#
    )
    .fetch_all(&state.db)
    .await?;

    let mut result = Vec::new();
    for row in &rows {
        let id: Uuid = row.try_get("id")?;
        let name: String = row.try_get("name")?;
        let subject: Option<String> = row.try_get("subject").ok();
        let template_type: Option<String> = row.try_get("template_type").ok();
        let is_html: Option<bool> = row.try_get("is_html").ok();
        let is_default: Option<bool> = row.try_get("is_default").ok();
        let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
        let updated_at: chrono::DateTime<chrono::Utc> = row.try_get("updated_at")?;

        result.push(json!({
            "id": id.to_string(),
            "name": name,
            "subject": subject,
            "template_type": template_type,
            "is_html": is_html,
            "is_default": is_default,
            "created_at": created_at.to_rfc3339(),
            "updated_at": updated_at.to_rfc3339(),
        }));
    }

    Ok(Json(json!({"templates": result, "count": result.len()})))
}

/// POST /api/v1/admin/email-templates — create a new email template
pub async fn admin_create_email_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let template_type = req.get("template_type").and_then(|v| v.as_str()).unwrap_or("custom").to_string();
    let subject = req.get("subject").and_then(|v| v.as_str());
    let body = req.get("body").and_then(|v| v.as_str());
    let html_body = req.get("html_body").and_then(|v| v.as_str());
    let is_html = req.get("is_html").and_then(|v| v.as_bool()).unwrap_or(true);
    let is_default = req.get("is_default").and_then(|v| v.as_bool()).unwrap_or(false);

    if name.is_empty() {
        return Err(AppError::Validation("Template name is required".to_string()));
    }

    let template_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO email_templates (id, aid, name, subject, body, html_body, template_type, is_html, is_default)
           VALUES ($1, '00000000-0000-0000-0000-000000000000'::uuid, $2, $3, $4, $5, $6, $7, $8)"#,
    )
    .bind(template_id)
    .bind(&name)
    .bind(subject)
    .bind(body)
    .bind(html_body)
    .bind(&template_type)
    .bind(is_html)
    .bind(is_default)
    .execute(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"id": template_id.to_string(), "status": "created"}))))
}

/// PUT /api/v1/admin/email-templates/{id} — update an email template
pub async fn admin_update_email_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(template_id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM email_templates WHERE id = $1"
    )
    .bind(template_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if existing == 0 {
        return Err(AppError::NotFound("Email template not found".to_string()));
    }

    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE email_templates SET name = $1 WHERE id = $2")
            .bind(name)
            .bind(template_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(subject) = req.get("subject").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE email_templates SET subject = $1 WHERE id = $2")
            .bind(subject)
            .bind(template_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(body) = req.get("body").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE email_templates SET body = $1 WHERE id = $2")
            .bind(body)
            .bind(template_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(html_body) = req.get("html_body").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE email_templates SET html_body = $1 WHERE id = $2")
            .bind(html_body)
            .bind(template_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(is_html) = req.get("is_html").and_then(|v| v.as_bool()) {
        sqlx::query("UPDATE email_templates SET is_html = $1 WHERE id = $2")
            .bind(is_html)
            .bind(template_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(template_type) = req.get("template_type").and_then(|v| v.as_str()) {
        sqlx::query("UPDATE email_templates SET template_type = $1 WHERE id = $2")
            .bind(template_type)
            .bind(template_id)
            .execute(&state.db)
            .await?;
    }

    sqlx::query("UPDATE email_templates SET updated_at = NOW() WHERE id = $1")
        .bind(template_id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({"status": "updated"})))
}

/// DELETE /api/v1/admin/email-templates/{id} — delete an email template
pub async fn admin_delete_email_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(template_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&claims)?;

    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM email_templates WHERE id = $1"
    )
    .bind(template_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    if existing == 0 {
        return Err(AppError::NotFound("Email template not found".to_string()));
    }

    sqlx::query("DELETE FROM email_templates WHERE id = $1")
        .bind(template_id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({"status": "deleted"})))
}

// ── Helper ──

fn require_admin(claims: &Claims) -> Result<(), AppError> {
    if !claims.perm_is_super_admin.unwrap_or(false) {
        return Err(AppError::Forbidden("Admin access required".to_string()));
    }
    Ok(())
}
