use crate::email::send_reset_email;
use axum::{
    extract::{Extension, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use super::middleware::create_token;
use super::models::*;
use crate::auth::api_key_auth::{argon2_hash, argon2_verify_result};
use crate::error::{ApiResult, AppError};
use crate::handlers::industry_handler;
use crate::AppState;
use std::sync::Arc;

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<impl IntoResponse> {
    // Validate input
    if req.email.is_empty() || req.password.is_empty() || req.name.is_empty() {
        return Err(AppError::Validation(
            "Name, email, and password are required".to_string(),
        ));
    }
    if req.password.len() < 6 {
        return Err(AppError::Validation(
            "Password must be at least 6 characters".to_string(),
        ));
    }

    // Check if user already exists
    let existing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE email = $1")
        .bind(&req.email)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

    if existing > 0 {
        return Err(AppError::Duplicate(
            "A user with this email already exists".to_string(),
        ));
    }

    // Hash password — off the reactor, bounded by the process-wide Argon2 semaphore
    // (argon2_hash). Hashing is 19 MiB of CPU that never awaits, so done inline here it
    // would park a tokio worker for the whole hash and `register` needs no credential
    // to reach.
    let password_hash = argon2_hash(req.password.clone()).await?;

    // Create account
    let account_name = req
        .account_name
        .unwrap_or_else(|| format!("{}'s Workspace", req.name));
    let account_slug = req.account_slug.unwrap_or_else(|| {
        req.name
            .to_lowercase()
            .replace(' ', "-")
            .chars()
            .take(30)
            .collect()
    });

    let aid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO accounts (id, name, account_slug, is_active) VALUES ($1, $2, $3, true)",
    )
    .bind(aid)
    .bind(&account_name)
    .bind(&account_slug)
    .execute(&state.db)
    .await?;

    // Seed default tags for this account
    let default_tag_names = vec!["active", "archived", "priority"];
    for tag_name in &default_tag_names {
        sqlx::query("INSERT INTO tags (id, aid, name) VALUES ($1, $2, $3)")
            .bind(Uuid::new_v4())
            .bind(aid)
            .bind(tag_name)
            .execute(&state.db)
            .await
            .ok();
    }

    // Create user (user role — only David is super_admin)
    let user_id = Uuid::new_v4();
    let now = Utc::now();
    sqlx::query(
        r#"INSERT INTO users (id, aid, email, password_hash, name, role, is_active, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, 'user', true, $6, $6)"#,
    )
    .bind(user_id)
    .bind(aid)
    .bind(&req.email)
    .bind(&password_hash)
    .bind(&req.name)
    .bind(now)
    .execute(&state.db)
    .await?;

    // Send welcome email with credentials
    let welcome_vars = serde_json::json!({
        "name": &req.name,
        "email": &req.email,
        "app_url": "https://app.workflowswift.com",
    });
    let _ = crate::email::send_email(&state, &req.email, "welcome", &welcome_vars).await;

    // Auto-generate API keys for the new user
    use crate::handlers::integration_center_handler;
    let _ = integration_center_handler::seed_user_keys(&state.db, user_id, aid).await;

    // Provision n8n tenant config for this account
    crate::n8n_provision::provision_n8n_for_account(&state.db, aid).await;

    // Assign plan if provided, or use default Free plan
    let plan_slug = req.plan_slug.as_deref().unwrap_or("free");
    let plan_id: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM plan_tiers WHERE slug = $1 AND is_active = true")
            .bind(plan_slug)
            .fetch_optional(&state.db)
            .await?;

    if let Some(pid) = plan_id {
        sqlx::query(
            r#"INSERT INTO account_plans (aid, plan_id, status, started_at)
               VALUES ($1, $2, 'active', NOW())"#,
        )
        .bind(aid)
        .bind(pid)
        .execute(&state.db)
        .await
        .ok();

        // Grant initial monthly credits for the plan
        let credits: Option<i32> = sqlx::query_scalar(
            r#"SELECT (features->>'credits_monthly')::integer
               FROM plan_tiers WHERE id = $1"#,
        )
        .bind(pid)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None);

        if let Some(amt) = credits {
            if amt > 0 {
                sqlx::query(
                    r#"INSERT INTO credit_transactions (id, aid, amount, transaction_type, description)
                       VALUES ($1, $2, $3, 'grant', 'Welcome credits: first month of ' || (
                         SELECT name FROM plan_tiers WHERE id = $4
                       ))"#
                )
                .bind(Uuid::new_v4())
                .bind(aid)
                .bind(amt)
                .bind(pid)
                .execute(&state.db)
                .await
                .ok();
            }
        }
    }

    // Set industry and seed dashboard if provided
    let industry_slug = req.industry_slug.as_deref().unwrap_or("site-flipping");
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM template_categories WHERE slug = $1 AND is_active = true)",
    )
    .bind(industry_slug)
    .fetch_one(&state.db)
    .await
    .unwrap_or(false);

    if exists {
        sqlx::query("UPDATE accounts SET industry_slug = $1 WHERE id = $2")
            .bind(industry_slug)
            .bind(aid)
            .execute(&state.db)
            .await?;

        // Use human-readable category name for dashboard, not slug
        let industry_name: String =
            sqlx::query_scalar("SELECT name FROM template_categories WHERE slug = $1")
                .bind(industry_slug)
                .fetch_optional(&state.db)
                .await?
                .unwrap_or_else(|| industry_slug.to_string());

        // Create dashboard and seed widgets
        let dashboard_id = Uuid::new_v4();
        let dashboard_name = format!("{} Dashboard", industry_name);
        sqlx::query(
            r#"INSERT INTO dashboards (id, aid, name, description)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(dashboard_id)
        .bind(aid)
        .bind(&dashboard_name)
        .bind(format!("Your {} dashboard", industry_name))
        .execute(&state.db)
        .await?;

        industry_handler::seed_default_widgets_internal(&state, aid, dashboard_id, industry_slug)
            .await;

        // Also register in account_industries (for multi-industry support)
        sqlx::query(
            r#"INSERT INTO account_industries (aid, industry_slug, dashboard_id)
               VALUES ($1, $2, $3)
               ON CONFLICT (aid, industry_slug) DO NOTHING"#,
        )
        .bind(aid)
        .bind(industry_slug)
        .bind(dashboard_id)
        .execute(&state.db)
        .await
        .ok();
    }

    // Create JWT
    let now_ts = chrono::Utc::now().timestamp() as usize;
    let claims = Claims {
        sub: user_id.to_string(),
        aid: aid.to_string(),
        role: "user".to_string(),
        exp: now_ts + state.config.jwt_access_expiry as usize,
        iat: now_ts,
        perm_is_super_admin: Some(false),
    };
    let token = create_token(&claims, &state.config.jwt_secret)?;

    let refresh_claims = Claims {
        sub: user_id.to_string(),
        aid: aid.to_string(),
        role: "user".to_string(),
        exp: now_ts + state.config.jwt_refresh_expiry as usize,
        iat: now_ts,
        perm_is_super_admin: Some(false),
    };
    let refresh_token = create_token(&refresh_claims, &state.config.jwt_secret)?;

    let user_response = UserResponse {
        id: user_id,
        aid,
        email: req.email,
        name: req.name,
        role: "user".to_string(),
        is_active: true,
        last_login_at: None,
        created_at: now,
    };

    // NOTE: no CoreSwift push on signup. The fleet standard makes CoreSwift INBOUND only
    // (captured leads flow down into the hub) and rejects the old pattern of pushing a
    // tenant's signup into a hardcoded SwiftSoftware tenant with a global env key. Leads
    // captured by this account's workflows are delivered by
    // `handlers::coreswift_external::push_lead_to_coreswift` using the account's own BYOK key.

    Ok((
        StatusCode::CREATED,
        Json(json!(RegisterResponse {
            access_token: token,
            refresh_token,
            token_type: "Bearer".to_string(),
            expires_in: state.config.jwt_access_expiry,
            user: user_response,
            account: AccountResponse {
                id: aid,
                name: account_name,
                slug: account_slug,
                is_active: true,
            },
        })),
    ))
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<impl IntoResponse> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE email = $1")
        .bind(&req.email)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::InvalidCredentials)?;

    if !user.is_active {
        return Err(AppError::Forbidden("Account is deactivated".to_string()));
    }

    // A user created by a checkout whose credential mail never went out has no usable password
    // (checkout_handler::deliver_credentials leaves `password_hash` empty on purpose). There is no
    // stored hash to parse, so this has to be an ordinary invalid-credentials answer rather than a
    // 500 from the hasher.
    if user.password_hash.trim().is_empty() {
        return Err(AppError::InvalidCredentials);
    }

    // Verify password — off the reactor, bounded by the same process-wide semaphore the
    // API-key path uses (argon2_verify_result). Reached without any credential, so inline
    // this is a free way for an unauthenticated caller to park every worker thread.
    // Error mapping is unchanged: bad password -> 401, unusable stored hash -> 500.
    if !argon2_verify_result(user.password_hash.clone(), Arc::from(req.password.as_str())).await? {
        return Err(AppError::InvalidCredentials);
    }

    // Update last_login
    sqlx::query("UPDATE users SET last_login_at = NOW() WHERE id = $1")
        .bind(user.id)
        .execute(&state.db)
        .await?;

    // Generate JWT
    let now_ts = chrono::Utc::now().timestamp() as usize;
    let claims = Claims {
        sub: user.id.to_string(),
        aid: user.aid.to_string(),
        role: user.role.clone(),
        exp: now_ts + state.config.jwt_access_expiry as usize,
        iat: now_ts,
        perm_is_super_admin: Some(user.perm_is_super_admin),
    };
    let token = create_token(&claims, &state.config.jwt_secret)?;

    let refresh_claims = Claims {
        sub: user.id.to_string(),
        aid: user.aid.to_string(),
        role: user.role.clone(),
        exp: now_ts + state.config.jwt_refresh_expiry as usize,
        iat: now_ts,
        perm_is_super_admin: Some(user.perm_is_super_admin),
    };
    let refresh_token = create_token(&refresh_claims, &state.config.jwt_secret)?;

    Ok(Json(json!(TokenResponse {
        access_token: token,
        refresh_token,
        token_type: "Bearer".to_string(),
        expires_in: state.config.jwt_access_expiry,
        user: user.into(),
    })))
}

pub async fn me(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("User not found".to_string()))?;

    Ok(Json(json!({ "user": UserResponse::from(user) })))
}

pub async fn change_password(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ChangePasswordRequest>,
) -> ApiResult<impl IntoResponse> {
    if req.new_password.len() < 6 {
        return Err(AppError::Validation(
            "New password must be at least 6 characters".to_string(),
        ));
    }

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::Unauthorized)?;

    // Verify current password, then hash the new one — both off the reactor behind the
    // process-wide Argon2 semaphore, error mapping unchanged (bad current password -> 401,
    // unusable stored hash -> 500).
    if !argon2_verify_result(
        user.password_hash.clone(),
        Arc::from(req.current_password.as_str()),
    )
    .await?
    {
        return Err(AppError::InvalidCredentials);
    }

    // Hash new password
    let new_hash = argon2_hash(req.new_password.clone()).await?;

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(&new_hash)
        .bind(user.id)
        .execute(&state.db)
        .await?;

    Ok((
        StatusCode::OK,
        Json(json!({"message": "Password updated successfully"})),
    ))
}

pub async fn update_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        if !name.trim().is_empty() {
            sqlx::query("UPDATE users SET name = $1 WHERE id = $2")
                .bind(name.trim())
                .bind(user_id)
                .execute(&state.db)
                .await?;
        }
    }

    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("User not found".to_string()))?;

    Ok(Json(
        json!({"user": UserResponse::from(user), "message": "Profile updated"}),
    ))
}

pub async fn forgot_password(
    State(state): State<AppState>,
    Json(req): Json<ForgotPasswordRequest>,
) -> ApiResult<impl IntoResponse> {
    if let Some(user) = sqlx::query_as::<_, User>("SELECT * FROM users WHERE email = $1")
        .bind(&req.email)
        .fetch_optional(&state.db)
        .await?
    {
        let token = Uuid::new_v4().to_string();
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(24);

        sqlx::query("UPDATE password_resets SET used = true WHERE user_id = $1 AND used = false")
            .bind(user.id)
            .execute(&state.db)
            .await
            .ok();

        sqlx::query("INSERT INTO password_resets (user_id, token, expires_at) VALUES ($1, $2, $3)")
            .bind(user.id)
            .bind(&token)
            .bind(expires_at)
            .execute(&state.db)
            .await?;

        // A send failure here is NOT returned to the caller, on purpose: answering 5xx only for
        // addresses that exist turns forgot-password into an account-existence oracle, and the
        // reset token is already in the DB either way. The failure is surfaced where an admin
        // looks instead — `send_email` records last_send_ok/last_send_error/last_send_template in
        // the `admin_settings.email` row the Email Provider panel renders (t_b28d3432).
        match send_reset_email(&state, &user.email, &token).await {
            Ok(_) => tracing::info!("Password reset email sent to {}", user.email),
            Err(e) => tracing::error!(
                "Failed to send password reset email to {}: {}",
                user.email,
                e
            ),
        }
        // Send password reset email via SMTP
    }

    Ok((
        StatusCode::OK,
        Json(json!({"message": "If the email exists, a password reset link has been sent"})),
    ))
}

pub async fn reset_password(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> ApiResult<impl IntoResponse> {
    if req.new_password.len() < 6 {
        return Err(AppError::Validation(
            "New password must be at least 6 characters".to_string(),
        ));
    }

    let reset = sqlx::query_as::<_, (Uuid, Uuid, String, chrono::DateTime<chrono::Utc>, bool, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, user_id, token, expires_at, used, created_at FROM password_resets WHERE token = $1 AND used = false AND expires_at > NOW()",
    )
    .bind(&req.token)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::BadRequest("Invalid or expired reset token".to_string()))?;

    // Off the reactor, bounded by the same Argon2 semaphore (see auth::api_key_auth).
    let new_hash = argon2_hash(req.new_password.clone()).await?;

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(&new_hash)
        .bind(reset.1)
        .execute(&state.db)
        .await?;

    sqlx::query("UPDATE password_resets SET used = true WHERE id = $1")
        .bind(reset.0)
        .execute(&state.db)
        .await?;

    Ok((
        StatusCode::OK,
        Json(json!({"message": "Password has been reset successfully"})),
    ))
}

pub async fn get_usage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Value>, AppError> {
    let aid: uuid::Uuid = claims
        .aid
        .parse()
        .map_err(|_| AppError::BadRequest("Invalid account".into()))?;
    let usage = crate::features::get_usage_json(&state.db, aid).await;
    Ok(Json(usage))
}
