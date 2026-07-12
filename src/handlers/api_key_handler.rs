use axum::{extract::{State, Json, Path}, http::StatusCode, response::IntoResponse, Extension};
use serde_json::json;
use uuid::Uuid;
use sqlx::Row;
use rand::Rng;
use crate::features;
use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;

pub async fn create_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, tenant_id, "max_api_keys", "Api Keys").await?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("default");
    let target_url = req.get("target_url").and_then(|v| v.as_str()).unwrap_or("");

    let random_part: String = (0..16).map(|_| format!("{:x}", rand::thread_rng().gen_range(0..16))).collect();
    let raw_key = format!("workflowswift_{}", random_part);
    let prefix = "workflo".to_string();

    let salt = argon2::password_hash::SaltString::generate(&mut rand::thread_rng());
    let hash = argon2::PasswordHasher::hash_password(&argon2::Argon2::default(), raw_key.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(format!("Hashing error: {}", e)))?;
    let key_hash = hash.serialize().to_string();

    sqlx::query(
        r#"INSERT INTO api_keys (id, tenant_id, user_id, name, key_hash, prefix, target_url)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(user_id)
    .bind(name)
    .bind(&key_hash)
    .bind(&prefix)
    .bind(target_url)
    .execute(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({
        "key": raw_key,
        "prefix": prefix,
        "name": name,
        "message": "Save this key — it will not be shown again"
    }))))
}

pub async fn list_api_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let rows = sqlx::query(
        r#"SELECT id::text, name, prefix, target_url, is_active, last_used_at::text, created_at::text
           FROM api_keys WHERE tenant_id = $1 ORDER BY created_at DESC"#,
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    let keys: Vec<serde_json::Value> = rows.iter().map(|row| {
        json!({
            "id": row.try_get::<&str, _>("id").unwrap_or(""),
            "name": row.try_get::<&str, _>("name").unwrap_or(""),
            "prefix": row.try_get::<&str, _>("prefix").unwrap_or(""),
            "target_url": row.try_get::<Option<&str>, _>("target_url").unwrap_or(None),
            "is_active": row.try_get::<bool, _>("is_active").unwrap_or(false),
        })
    }).collect();

    Ok(Json(json!({"api_keys": keys})))
}

pub async fn delete_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let key_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid key ID".to_string()))?;

    let result = sqlx::query("DELETE FROM api_keys WHERE id = $1 AND tenant_id = $2")
        .bind(key_id)
        .bind(tenant_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("API key not found".to_string()));
    }
    Ok(Json(json!({"status": "deleted"})))
}

pub async fn update_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let key_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid key ID".to_string()))?;

    let name = req.get("name").and_then(|v| v.as_str());
    let target_url = req.get("target_url").and_then(|v| v.as_str());
    let is_active = req.get("is_active").and_then(|v| v.as_bool());

    if name.is_none() && target_url.is_none() && is_active.is_none() {
        return Err(AppError::BadRequest("No fields to update".to_string()));
    }

    sqlx::query(
        r#"UPDATE api_keys SET name = COALESCE($1, name), target_url = COALESCE($2, target_url), is_active = COALESCE($3, is_active), updated_at = NOW()
           WHERE id = $4 AND tenant_id = $5"#,
    )
    .bind(name)
    .bind(target_url)
    .bind(is_active)
    .bind(key_id)
    .bind(tenant_id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({"status": "updated"})))
}
