use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::features;
use crate::AppState;
use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use rand::Rng;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

pub async fn create_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_api_keys", "Api Keys").await?;
    features::enforce_plan_flag(&state.db, aid, "api_access", "API access").await?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let target_url = req.get("target_url").and_then(|v| v.as_str()).unwrap_or("");

    let random_part: String = (0..16)
        .map(|_| format!("{:x}", rand::thread_rng().gen_range(0..16)))
        .collect();
    let raw_key = format!("workflowswift_{}", random_part);
    // `prefix` is a real lookup hint from here on: 8 hex digits of sha256(raw key), so
    // verification is an indexed equality lookup instead of a scan of every tenant's key
    // rows (see auth::api_key_auth::authenticate). It is still what the owner sees as the
    // key's identifier in GET /api-keys, and it reveals nothing about the key itself.
    let prefix = crate::auth::api_key_auth::discriminator(&raw_key);

    // Hashing is a 19 MiB CPU-bound job that never awaits: it runs off the reactor under the
    // process-wide Argon2 semaphore (auth::api_key_auth::argon2_hash) so minting a key cannot
    // park a tokio worker thread. The failure is still a 500 — `AppError::Hash` displays as
    // "Hashing error: {msg}", i.e. the same words the old `Internal` mapping logged.
    let key_hash = crate::auth::api_key_auth::argon2_hash(raw_key.clone()).await?;

    sqlx::query(
        r#"INSERT INTO api_keys (id, aid, user_id, name, key_hash, prefix, target_url)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(user_id)
    .bind(name)
    .bind(&key_hash)
    .bind(&prefix)
    .bind(target_url)
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "key": raw_key,
            "prefix": prefix,
            "name": name,
            "message": "Save this key — it will not be shown again"
        })),
    ))
}

pub async fn list_api_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let rows = sqlx::query(
        r#"SELECT id::text, name, prefix, target_url, is_active, last_used_at::text, created_at::text
           FROM api_keys WHERE aid = $1 ORDER BY created_at DESC"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let keys: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.try_get::<&str, _>("id").unwrap_or(""),
                "name": row.try_get::<&str, _>("name").unwrap_or(""),
                "prefix": row.try_get::<&str, _>("prefix").unwrap_or(""),
                "target_url": row.try_get::<Option<&str>, _>("target_url").unwrap_or(None),
                "is_active": row.try_get::<bool, _>("is_active").unwrap_or(false),
                // created_at / last_used_at are selected above; surface them so the app SPA can
                // show "created" and "last used" without a second round trip.
                "created_at": row.try_get::<Option<&str>, _>("created_at").unwrap_or(None),
                "last_used_at": row.try_get::<Option<&str>, _>("last_used_at").unwrap_or(None),
            })
        })
        .collect();

    Ok(Json(json!({"api_keys": keys})))
}

pub async fn delete_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let key_id =
        Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid key ID".to_string()))?;

    let result = sqlx::query("DELETE FROM api_keys WHERE id = $1 AND aid = $2")
        .bind(key_id)
        .bind(aid)
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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let key_id =
        Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid key ID".to_string()))?;

    let name = req.get("name").and_then(|v| v.as_str());
    let target_url = req.get("target_url").and_then(|v| v.as_str());
    let is_active = req.get("is_active").and_then(|v| v.as_bool());

    if name.is_none() && target_url.is_none() && is_active.is_none() {
        return Err(AppError::BadRequest("No fields to update".to_string()));
    }

    sqlx::query(
        r#"UPDATE api_keys SET name = COALESCE($1, name), target_url = COALESCE($2, target_url), is_active = COALESCE($3, is_active), updated_at = NOW()
           WHERE id = $4 AND aid = $5"#,
    )
    .bind(name)
    .bind(target_url)
    .bind(is_active)
    .bind(key_id)
    .bind(aid)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({"status": "updated"})))
}
