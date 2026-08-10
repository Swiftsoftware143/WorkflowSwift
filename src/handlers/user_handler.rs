use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::email;
use crate::error::{ApiResult, AppError};
use crate::models::user::User;
use crate::AppState;

/// GET /api/v1/users — list users in the current account
pub async fn list_users(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let users = sqlx::query_as::<_, User>(
        "SELECT id, aid, email, password_hash, name, role, is_active, last_login_at, created_at, updated_at, perm_is_super_admin, permissions FROM users WHERE aid = $1 ORDER BY name ASC",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let user_list: Vec<serde_json::Value> = users
        .into_iter()
        .map(|u| {
            json!({
                "id": u.id,
                "email": u.email,
                "name": u.name,
                "role": u.role,
                "is_active": u.is_active,
                "permissions": u.permissions,
            })
        })
        .collect();

    Ok(Json(json!({"users": user_list})))
}

/// GET /api/v1/users/team — list team members (users with role != 'user' owner) in the current account
pub async fn list_team_members(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let users = sqlx::query_as::<_, User>(
        "SELECT id, aid, email, password_hash, name, role, is_active, last_login_at, created_at, updated_at, perm_is_super_admin, permissions FROM users WHERE aid = $1 AND role = 'team_member' ORDER BY name ASC",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let user_list: Vec<serde_json::Value> = users
        .into_iter()
        .map(|u| {
            json!({
                "id": u.id,
                "email": u.email,
                "name": u.name,
                "role": u.role,
                "is_active": u.is_active,
                "permissions": u.permissions,
            })
        })
        .collect();

    Ok(Json(json!({"users": user_list})))
}

/// POST /api/v1/users/invite — invite a team member under the current account
pub async fn invite_user(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let email = req
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let role = req
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("team_member")
        .to_string();
    let permissions = req.get("permissions").cloned().unwrap_or(json!({}));

    // Only allow valid roles — can't create another 'user' (account owner) or 'super_admin' via invite
    if role == "user" || role == "admin" || role == "super_admin" {
        return Err(AppError::Validation(
            "Cannot create account owners via invite. Use admin account creation for that."
                .to_string(),
        ));
    }

    if email.is_empty() || name.is_empty() {
        return Err(AppError::Validation(
            "Email and name are required".to_string(),
        ));
    }

    // Check duplicate
    let existing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE email = $1")
        .bind(&email)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

    if existing > 0 {
        return Err(AppError::Duplicate(
            "A user with this email already exists".to_string(),
        ));
    }

    // Generate temp password
    use argon2::password_hash::SaltString;
    use argon2::{Argon2, PasswordHasher};
    use rand::rngs::OsRng;

    let temp_password = Uuid::new_v4().to_string();
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(temp_password.as_bytes(), &salt)
        .map_err(|e| AppError::Hash(e.to_string()))?
        .to_string();

    let user_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO users (id, aid, email, password_hash, name, role, is_active, permissions)
           VALUES ($1, $2, $3, $4, $5, $6, true, $7::jsonb)"#,
    )
    .bind(user_id)
    .bind(aid)
    .bind(&email)
    .bind(&hash)
    .bind(&name)
    .bind(&role)
    .bind(permissions.to_string())
    .execute(&state.db)
    .await?;

    // Get the account name for the email
    let account_name: String = sqlx::query_scalar("SELECT name FROM accounts WHERE id = $1")
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_else(|| "Your Team".to_string());

    // Send invite email
    let result = email::send_email(
        &state,
        &email,
        "team_invite",
        &json!({
            "name": name,
            "email": email,
            "password": temp_password,
            "account_name": account_name,
            "app_url": "https://app.workflowswift.com",
        }),
    )
    .await;

    match result {
        Ok(_) => tracing::info!("Team invite email sent to {}", email),
        Err(ref e) => {
            tracing::error!("Failed to send team invite email to {}: {}", email, e);
            // Still return success — the user was created
        }
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "user": {
                "id": user_id,
                "email": email,
                "name": name,
                "role": role,
                "is_active": true,
                "permissions": permissions,
            },
            "temporary_password": temp_password,
            "email_sent": result.as_ref().ok().is_some(),
        })),
    ))
}

/// DELETE /api/v1/users/{id} — remove a user (owner can remove team members; admin can remove any)
pub async fn remove_user(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let caller_is_super = claims.perm_is_super_admin.unwrap_or(false);

    // Find the user to remove
    let user = sqlx::query_as::<_, User>(
        "SELECT id, aid, email, password_hash, name, role, is_active, last_login_at, created_at, updated_at, perm_is_super_admin, permissions FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

    // Cannot remove yourself
    let caller_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    if user.id == caller_id {
        return Err(AppError::Validation("Cannot remove yourself".to_string()));
    }

    // Cannot remove super_admin
    if user.perm_is_super_admin {
        return Err(AppError::Forbidden(
            "Cannot remove the super admin".to_string(),
        ));
    }

    // Must be in same account, or caller must be super_admin
    if user.aid != aid && !caller_is_super {
        return Err(AppError::Forbidden(
            "User is not in your account".to_string(),
        ));
    }

    // If not super_admin, can only remove team_members
    if !caller_is_super && user.role != "team_member" {
        return Err(AppError::Forbidden(
            "You can only remove team members".to_string(),
        ));
    }

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({"message": "User removed"})))
}

/// PUT /api/v1/users/{id}/permissions — update a user's permissions
pub async fn update_user_permissions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let caller_is_super = claims.perm_is_super_admin.unwrap_or(false);

    let permissions = req
        .get("permissions")
        .ok_or_else(|| AppError::Validation("permissions field is required".to_string()))?;

    // Find the user
    let user = sqlx::query_as::<_, User>(
        "SELECT id, aid, email, password_hash, name, role, is_active, last_login_at, created_at, updated_at, perm_is_super_admin, permissions FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

    // Must be in same account, or caller must be super_admin
    if user.aid != aid && !caller_is_super {
        return Err(AppError::Forbidden(
            "User is not in your account".to_string(),
        ));
    }

    // Super admin permissions cannot be changed
    if user.perm_is_super_admin {
        return Err(AppError::Forbidden(
            "Cannot change super admin permissions".to_string(),
        ));
    }

    sqlx::query("UPDATE users SET permissions = $1::jsonb WHERE id = $2")
        .bind(permissions.to_string())
        .bind(id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({"message": "Permissions updated"})))
}

#[derive(Deserialize)]
pub struct RemoveQuery {
    pub aid: Option<String>,
}
