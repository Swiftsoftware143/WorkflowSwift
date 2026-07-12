use crate::email::send_reset_email;
use axum::{
    extract::{State, Extension},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::handlers::industry_handler;
use super::models::*;
use super::middleware::create_token;

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<impl IntoResponse> {
    // Validate input
    if req.email.is_empty() || req.password.is_empty() || req.name.is_empty() {
        return Err(AppError::Validation("Name, email, and password are required".to_string()));
    }
    if req.password.len() < 6 {
        return Err(AppError::Validation("Password must be at least 6 characters".to_string()));
    }

    // Check if user already exists
    let existing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE email = $1")
        .bind(&req.email)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

    if existing > 0 {
        return Err(AppError::Duplicate("A user with this email already exists".to_string()));
    }

    // Hash password
    use argon2::{Argon2, PasswordHasher};
    use argon2::password_hash::SaltString;
    use rand::rngs::OsRng;

    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(|e| AppError::Hash(e.to_string()))?
        .to_string();

    // Create tenant
    let tenant_name = req.tenant_name.unwrap_or_else(|| format!("{}'s Workspace", req.name));
    let tenant_slug = req.tenant_slug.unwrap_or_else(|| {
        req.name.to_lowercase().replace(' ', "-").chars().take(30).collect()
    });

    let tenant_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tenants (id, name, slug, is_active) VALUES ($1, $2, $3, true)",
    )
    .bind(tenant_id)
    .bind(&tenant_name)
    .bind(&tenant_slug)
    .execute(&state.db)
    .await?;

    // Seed default tags for this tenant
    let default_tag_names = vec!["active", "archived", "priority"];
    for tag_name in &default_tag_names {
        sqlx::query(
            "INSERT INTO tags (id, tenant_id, name) VALUES ($1, $2, $3)",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(tag_name)
        .execute(&state.db)
        .await
        .ok();
    }

    // Create user (admin role)
    let user_id = Uuid::new_v4();
    let now = Utc::now();
    sqlx::query(
        r#"INSERT INTO users (id, tenant_id, email, password_hash, name, role, is_active, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, 'admin', true, $6, $6)"#,
    )
    .bind(user_id)
    .bind(tenant_id)
    .bind(&req.email)
    .bind(&password_hash)
    .bind(&req.name)
    .bind(now)
    .execute(&state.db)
    .await?;

    // Auto-generate API keys for the new user
    use crate::handlers::integration_center_handler;
    let _ = integration_center_handler::seed_user_keys(&state.db, user_id, tenant_id).await;

    // Assign plan if provided, or use default Free plan
    let plan_slug = req.plan_slug.as_deref().unwrap_or("free");
    let plan_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM plan_tiers WHERE slug = $1 AND is_active = true"
    )
    .bind(plan_slug)
    .fetch_optional(&state.db)
    .await?;

    if let Some(pid) = plan_id {
        sqlx::query(
            r#"INSERT INTO tenant_plans (tenant_id, plan_id, status, started_at)
               VALUES ($1, $2, 'active', NOW())"#
        )
        .bind(tenant_id)
        .bind(pid)
        .execute(&state.db)
        .await
        .ok();
    }

    // Set industry and seed dashboard if provided
    let industry_slug = req.industry_slug.as_deref().unwrap_or("site-flipping");
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM template_categories WHERE slug = $1 AND is_active = true)"
    )
    .bind(industry_slug)
    .fetch_one(&state.db)
    .await
    .unwrap_or(false);

    if exists {
        sqlx::query("UPDATE tenants SET industry_slug = $1 WHERE id = $2")
            .bind(industry_slug)
            .bind(tenant_id)
            .execute(&state.db)
            .await?;

        // Use human-readable category name for dashboard, not slug
        let industry_name: String = sqlx::query_scalar(
            "SELECT name FROM template_categories WHERE slug = $1"
        )
        .bind(industry_slug)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_else(|| industry_slug.to_string());

        // Create dashboard and seed widgets
        let dashboard_id = Uuid::new_v4();
        let dashboard_name = format!("{} Dashboard", industry_name);
        sqlx::query(
            r#"INSERT INTO dashboards (id, tenant_id, name, description)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(dashboard_id)
        .bind(tenant_id)
        .bind(&dashboard_name)
        .bind(format!("Your {} dashboard", industry_name))
        .execute(&state.db)
        .await?;

        industry_handler::seed_default_widgets_internal(&state, tenant_id, dashboard_id, industry_slug).await;

        // Also register in tenant_industries (for multi-industry support)
        sqlx::query(
            r#"INSERT INTO tenant_industries (tenant_id, industry_slug, dashboard_id)
               VALUES ($1, $2, $3)
               ON CONFLICT (tenant_id, industry_slug) DO NOTHING"#
        )
        .bind(tenant_id)
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
        aid: tenant_id.to_string(),
        role: "admin".to_string(),
        exp: now_ts + state.config.jwt_access_expiry as usize,
        iat: now_ts,
    };
    let token = create_token(&claims, &state.config.jwt_secret)?;

    let refresh_claims = Claims {
        sub: user_id.to_string(),
        aid: tenant_id.to_string(),
        role: "admin".to_string(),
        exp: now_ts + state.config.jwt_refresh_expiry as usize,
        iat: now_ts,
    };
    let refresh_token = create_token(&refresh_claims, &state.config.jwt_secret)?;

    let user_response = UserResponse {
        id: user_id,
        tenant_id,
        email: req.email,
        name: req.name,
        role: "admin".to_string(),
        is_active: true,
        last_login_at: None,
        created_at: now,
    };

    Ok((
        StatusCode::CREATED,
        Json(json!(RegisterResponse {
            access_token: token,
            refresh_token,
            token_type: "Bearer".to_string(),
            expires_in: state.config.jwt_access_expiry,
            user: user_response,
            tenant: TenantResponse {
                id: tenant_id,
                name: tenant_name,
                slug: tenant_slug,
                is_active: true,
            },
        })),
    ))
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<impl IntoResponse> {
    use argon2::{Argon2, PasswordHash, PasswordVerifier};

    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE email = $1")
        .bind(&req.email)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::InvalidCredentials)?;

    if !user.is_active {
        return Err(AppError::Forbidden("Account is deactivated".to_string()));
    }

    // Verify password
    let parsed_hash = PasswordHash::new(&user.password_hash)
        .map_err(|e| AppError::Hash(e.to_string()))?;
    let argon2 = Argon2::default();
    argon2
        .verify_password(req.password.as_bytes(), &parsed_hash)
        .map_err(|_| AppError::InvalidCredentials)?;

    // Update last_login
    sqlx::query("UPDATE users SET last_login_at = NOW() WHERE id = $1")
        .bind(user.id)
        .execute(&state.db)
        .await?;

    // Generate JWT
    let now_ts = chrono::Utc::now().timestamp() as usize;
    let claims = Claims {
        sub: user.id.to_string(),
        aid: user.tenant_id.to_string(),
        role: user.role.clone(),
        exp: now_ts + state.config.jwt_access_expiry as usize,
        iat: now_ts,
    };
    let token = create_token(&claims, &state.config.jwt_secret)?;

    let refresh_claims = Claims {
        sub: user.id.to_string(),
        aid: user.tenant_id.to_string(),
        role: user.role.clone(),
        exp: now_ts + state.config.jwt_refresh_expiry as usize,
        iat: now_ts,
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
    use argon2::{Argon2, PasswordHash, PasswordVerifier, PasswordHasher};
    use argon2::password_hash::SaltString;
    use rand::rngs::OsRng;

    if req.new_password.len() < 6 {
        return Err(AppError::Validation("New password must be at least 6 characters".to_string()));
    }

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::Unauthorized)?;

    // Verify current password
    let parsed_hash = PasswordHash::new(&user.password_hash)
        .map_err(|e| AppError::Hash(e.to_string()))?;
    let argon2 = Argon2::default();
    argon2
        .verify_password(req.current_password.as_bytes(), &parsed_hash)
        .map_err(|_| AppError::InvalidCredentials)?;

    // Hash new password
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let new_hash = argon2
        .hash_password(req.new_password.as_bytes(), &salt)
        .map_err(|e| AppError::Hash(e.to_string()))?
        .to_string();

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(&new_hash)
        .bind(user.id)
        .execute(&state.db)
        .await?;

    Ok((StatusCode::OK, Json(json!({"message": "Password updated successfully"}))))
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

    Ok(Json(json!({"user": UserResponse::from(user), "message": "Profile updated"})))
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
            .await.ok();

        sqlx::query(
            "INSERT INTO password_resets (user_id, token, expires_at) VALUES ($1, $2, $3)",
        )
        .bind(user.id)
        .bind(&token)
        .bind(expires_at)
        .execute(&state.db)
        .await?;

        match send_reset_email(&user.email, &token).await {
            Ok(_) => tracing::info!("Password reset email sent to {}", user.email),
            Err(e) => tracing::error!("Failed to send password reset email to {}: {}", user.email, e),
        }
        // Send password reset email via SMTP
    }

    Ok((StatusCode::OK, Json(json!({"message": "If the email exists, a password reset link has been sent"}))))
}

pub async fn reset_password(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> ApiResult<impl IntoResponse> {
    use argon2::{Argon2, PasswordHasher};
    use argon2::password_hash::SaltString;
    use rand::rngs::OsRng;

    if req.new_password.len() < 6 {
        return Err(AppError::Validation("New password must be at least 6 characters".to_string()));
    }

    let reset = sqlx::query_as::<_, (Uuid, Uuid, String, chrono::DateTime<chrono::Utc>, bool, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, user_id, token, expires_at, used, created_at FROM password_resets WHERE token = $1 AND used = false AND expires_at > NOW()",
    )
    .bind(&req.token)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::BadRequest("Invalid or expired reset token".to_string()))?;

    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let new_hash = argon2
        .hash_password(req.new_password.as_bytes(), &salt)
        .map_err(|e| AppError::Hash(e.to_string()))?
        .to_string();

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(&new_hash)
        .bind(reset.1)
        .execute(&state.db)
        .await?;

    sqlx::query("UPDATE password_resets SET used = true WHERE id = $1")
        .bind(reset.0)
        .execute(&state.db)
        .await?;

    Ok((StatusCode::OK, Json(json!({"message": "Password has been reset successfully"}))))
}

