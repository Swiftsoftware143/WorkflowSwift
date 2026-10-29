use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::models::brand_monitor::*;
use crate::AppState;

pub async fn list_brand_monitors(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let monitors = sqlx::query_as::<_, BrandMonitor>(
        "SELECT * FROM brand_monitors WHERE aid = $1 ORDER BY brand_name ASC",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"brand_monitors": monitors})))
}

pub async fn create_brand_monitor(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let brand_name = req
        .get("brand_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if brand_name.is_empty() {
        return Err(AppError::Validation("brand_name is required".to_string()));
    }

    let keywords: Option<Vec<String>> = req.get("keywords").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    });

    let platforms: Vec<String> = req
        .get("platforms")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if platforms.is_empty() {
        return Err(AppError::Validation(
            "At least one platform is required".to_string(),
        ));
    }

    let monitor = sqlx::query_as::<_, BrandMonitor>(
        r#"INSERT INTO brand_monitors (id, aid, brand_name, keywords, platforms, is_active)
           VALUES ($1, $2, $3, $4::text[], $5::text[], true)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM brand_monitors WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Brand monitor not found".to_string()));
    }

    Ok(Json(
        json!({"message": "Brand monitor deleted successfully"}),
    ))
}

pub async fn search_brand_mentions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let brand_monitor_id = req
        .get("brand_monitor_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(AppError::Validation(
            "Valid brand_monitor_id is required".to_string(),
        ))?;

    let _monitor = sqlx::query_as::<_, BrandMonitor>(
        "SELECT * FROM brand_monitors WHERE id = $1 AND aid = $2 AND is_active = true",
    )
    .bind(brand_monitor_id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(
        "Active brand monitor not found".to_string(),
    ))?;

    let max_results = req
        .get("max_results")
        .and_then(|v| v.as_i64())
        .unwrap_or(10);
    let _ = max_results;

    // A scan used to RETURN FOUR INVENTED MENTIONS: platform cycled through
    // twitter/reddit/news/blogs, `https://example.com/mention/<n>` URLs, a fabricated snippet, a
    // cycled sentiment and a made-up 0.85+ relevance score — and it still stamped `last_scanned_at`,
    // so the panel showed a fresh scan over content nobody had found. This app has no mention
    // provider connected (the catalogue's data provider is Google Places, which answers places, not
    // web mentions), so a scan cannot be real. It now says so instead of pretending: 424 Failed
    // Dependency carries the cause and the fix. Nothing is written and `last_scanned_at` is NOT
    // touched, because no scan happened.
    let _ = (state, _monitor);
    Err(AppError::UpstreamRefused(
        "Brand Monitor is not connected on this account yet: no web-mention provider is installed, so a scan cannot find real mentions. Nothing was invented for you. Connect a mention-search provider for this app, then scan again."
            .to_string(),
    ))
}
