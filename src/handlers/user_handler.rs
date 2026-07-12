use axum::{
    extract::{State, Json},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::models::user::User;

pub async fn list_users(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let users = sqlx::query_as::<_, User>(
        "SELECT id, tenant_id, email, password_hash, name, role, is_active, last_login_at, created_at, updated_at FROM users WHERE tenant_id = $1 ORDER BY name ASC",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    let user_list: Vec<serde_json::Value> = users.into_iter().map(|u| {
        json!({
            "id": u.id,
            "email": u.email,
            "name": u.name,
            "role": u.role,
            "is_active": u.is_active,
        })
    }).collect();

    Ok(Json(json!({"users": user_list})))
}

pub async fn invite_user(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let email = req.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let role = req.get("role").and_then(|v| v.as_str()).unwrap_or("member").to_string();

    if email.is_empty() || name.is_empty() {
        return Err(AppError::Validation("Email and name are required".to_string()));
    }

    // Check duplicate
    let existing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE email = $1")
        .bind(&email)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

    if existing > 0 {
        return Err(AppError::Duplicate("A user with this email already exists".to_string()));
    }

    // Create user with a temporary password (would send invite email in production)
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
    sqlx::query(
        r#"INSERT INTO users (id, tenant_id, email, password_hash, name, role, is_active)
           VALUES ($1, $2, $3, $4, $5, $6, true)"#,
    )
    .bind(user_id)
    .bind(tenant_id)
    .bind(&email)
    .bind(&hash)
    .bind(&name)
    .bind(&role)
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "user": {
                "id": user_id,
                "email": email,
                "name": name,
                "role": role,
                "is_active": true,
            },
            "temporary_password": temp_password,
        })),
    ))
}
