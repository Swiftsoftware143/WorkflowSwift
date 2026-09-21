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
use crate::{features, AppState};

/// Roles a workspace member may hold. The platform vocabulary is deliberately excluded:
/// `user` is the account owner, `admin` is the singleton platform admin (`idx_unique_admin`
/// allows exactly one row for it), `super_admin` is platform staff and `agency_admin` is the
/// platform-operations role — `plan_handler` authorizes global plan list/update/delete on
/// `admin`/`agency_admin`, so a tenant must never be able to mint one of those from its own
/// team screen. `company_admin` is this fleet's tenant-admin role (ADASwift, IncentiveSwift,
/// missedcallrespondr and multi-directory all write it for a tenant's own admin).
const TENANT_ROLES: [&str; 2] = ["team_member", "company_admin"];

/// Canonical tenant role for a user-supplied string, or `None` when it is not invitable.
fn tenant_role(raw: &str) -> Option<&'static str> {
    let r = raw.trim();
    TENANT_ROLES.iter().find(|k| **k == r).copied()
}

/// Managing the team (invite / remove / re-role) is an admin action: a plain member must not
/// be able to add people (each one consumes a paid seat) or widen anyone's permissions.
fn is_tenant_admin(claims: &Claims) -> bool {
    matches!(
        claims.role.as_str(),
        "user" | "company_admin" | "admin" | "super_admin" | "agency_admin"
    )
}

fn require_tenant_admin(claims: &Claims) -> Result<(), AppError> {
    if is_tenant_admin(claims) {
        Ok(())
    } else {
        Err(AppError::Forbidden(
            "Only the account owner or an account admin can manage the team".to_string(),
        ))
    }
}

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
    require_tenant_admin(&claims)?;
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

    // Only tenant-level roles are invitable. 'user' (account owner), 'admin' (singleton
    // platform admin), 'super_admin' and 'agency_admin' (platform operations — plan CRUD)
    // are refused: a tenant workspace must not be able to mint a platform role.
    let role = match tenant_role(&role) {
        Some(r) => r.to_string(),
        None => {
            return Err(AppError::Validation(
                "Role must be one of: team_member, company_admin".to_string(),
            ))
        }
    };

    if email.is_empty() || name.is_empty() {
        return Err(AppError::Validation(
            "Email and name are required".to_string(),
        ));
    }

    // max_users is sold as a per-plan seat limit — enforce it here or the paid limit is a
    // no-op. The count includes the account owner (Free = 2 seats = owner + 1 invitee).
    features::enforce_feature_limit(&state.db, aid, "max_users", "Team members").await?;

    // Check duplicate. Deliberately GLOBAL, not per-account: `email` is the login key
    // (`SELECT * FROM users WHERE email = $1` in auth::handlers) and register() makes the same
    // global check, so one email must map to exactly one user. `users_aid_email_key` alone
    // would let two tenants own the same address and make login ambiguous.
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

    // Generate temp password. A 19 MiB Argon2 hash is CPU-bound work that never awaits: it
    // goes through the process-wide semaphore on the blocking pool (see
    // auth::api_key_auth::argon2_hash) so inviting a user cannot park a tokio worker thread.
    let temp_password = Uuid::new_v4().to_string();
    let hash = crate::auth::api_key_auth::argon2_hash(temp_password.clone()).await?;

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
    require_tenant_admin(&claims)?;
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

    // If not super_admin, can only remove tenant-level members (never the account owner or a
    // platform role). Membership in TENANT_ROLES is what makes the team screen's Remove button
    // reversible for every role it can hand out.
    if !caller_is_super && !TENANT_ROLES.contains(&user.role.as_str()) {
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
    require_tenant_admin(&claims)?;

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

    // Only tenant-level members are scoped here: the account owner's and the platform roles'
    // scope is not tenant-editable.
    if !caller_is_super && !TENANT_ROLES.contains(&user.role.as_str()) {
        return Err(AppError::Forbidden(
            "You can only scope team members".to_string(),
        ));
    }

    sqlx::query("UPDATE users SET permissions = $1::jsonb WHERE id = $2")
        .bind(permissions.to_string())
        .bind(id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({"message": "Permissions updated"})))
}

/// PUT /api/v1/users/{id}/role — set a team member's role inside the caller's account.
/// The card this exists for asks the team screen to *set* each role, not only show it: before
/// this there was no way to promote a member to tenant admin (or demote one) without SQL.
pub async fn set_user_role(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_tenant_admin(&claims)?;
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let caller_is_super = claims.perm_is_super_admin.unwrap_or(false);

    let requested = req.get("role").and_then(|v| v.as_str()).unwrap_or("");
    let role = match tenant_role(requested) {
        Some(r) => r,
        None => {
            return Err(AppError::Validation(
                "Role must be one of: team_member, company_admin".to_string(),
            ))
        }
    };

    let user = sqlx::query_as::<_, User>(
        "SELECT id, aid, email, password_hash, name, role, is_active, last_login_at, created_at, updated_at, perm_is_super_admin, permissions FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

    if user.aid != aid && !caller_is_super {
        return Err(AppError::Forbidden(
            "User is not in your account".to_string(),
        ));
    }

    // Never re-role the account owner, the super admin, or a platform role.
    if user.perm_is_super_admin || user.role == "user" {
        return Err(AppError::Forbidden(
            "The account owner's role cannot be changed".to_string(),
        ));
    }
    if !TENANT_ROLES.contains(&user.role.as_str()) {
        return Err(AppError::Forbidden(
            "Only team members can be re-roled".to_string(),
        ));
    }

    // Self-demotion on the last admin is how a workspace locks itself out.
    let caller_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    if user.id == caller_id {
        return Err(AppError::Validation(
            "You cannot change your own role".to_string(),
        ));
    }

    sqlx::query("UPDATE users SET role = $1, updated_at = now() WHERE id = $2")
        .bind(role)
        .bind(id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({
        "message": "Role updated",
        "user": { "id": user.id, "email": user.email, "name": user.name, "role": role }
    })))
}

#[derive(Deserialize)]
pub struct RemoveQuery {
    pub aid: Option<String>,
}
