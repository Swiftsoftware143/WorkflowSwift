use axum::{
    extract::{State, Path, Json},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::models::brand_monitor::*;

pub async fn list_brand_monitors(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let monitors = sqlx::query_as::<_, BrandMonitor>(
        "SELECT * FROM brand_monitors WHERE tenant_id = $1 ORDER BY brand_name ASC",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"brand_monitors": monitors})))
}

pub async fn create_brand_monitor(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let brand_name = req.get("brand_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if brand_name.is_empty() {
        return Err(AppError::Validation("brand_name is required".to_string()));
    }

    let keywords: Option<Vec<String>> = req.get("keywords")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect());

    let platforms: Vec<String> = req.get("platforms")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    if platforms.is_empty() {
        return Err(AppError::Validation("At least one platform is required".to_string()));
    }

    let monitor = sqlx::query_as::<_, BrandMonitor>(
        r#"INSERT INTO brand_monitors (id, tenant_id, brand_name, keywords, platforms, is_active)
           VALUES ($1, $2, $3, $4::text[], $5::text[], true)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(&brand_name)
    .bind(&keywords)
    .bind(&platforms)
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"brand_monitor": monitor}))))
}

pub async fn delete_brand_monitor(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query(
        "DELETE FROM brand_monitors WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Brand monitor not found".to_string()));
    }

    Ok(Json(json!({"message": "Brand monitor deleted successfully"})))
}

pub async fn search_brand_mentions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let brand_monitor_id = req.get("brand_monitor_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(AppError::Validation("Valid brand_monitor_id is required".to_string()))?;

    let _monitor = sqlx::query_as::<_, BrandMonitor>(
        "SELECT * FROM brand_monitors WHERE id = $1 AND tenant_id = $2 AND is_active = true",
    )
    .bind(brand_monitor_id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Active brand monitor not found".to_string()))?;

    let max_results = req.get("max_results")
        .and_then(|v| v.as_i64())
        .unwrap_or(10);

    // Mock search results — in production this would call external APIs
    let mentions: Vec<BrandMention> = (0..max_results.min(20) as usize).map(|i| {
        BrandMention {
            platform: vec![String::from("twitter"), String::from("reddit"), String::from("news"), String::from("blogs")][i % 4].clone(),
            title: format!("Brand mention {} for monitor", i + 1),
            url: format!("https://example.com/mention/{}", i + 1),
            snippet: format!("This is a sample mention snippet #{} related to the monitored brand.", i + 1),
            sentiment: vec![String::from("positive"), String::from("neutral"), String::from("negative")][i % 3].clone(),
            relevance_score: 0.85 + (i as f64 * 0.01).min(0.15),
        }
    }).collect();

    // Update last_scanned_at
    sqlx::query("UPDATE brand_monitors SET last_scanned_at = NOW() WHERE id = $1")
        .bind(brand_monitor_id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({
        "mentions": mentions,
        "total": mentions.len(),
    })))
}
