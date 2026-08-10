use axum::{
    extract::{Json, State},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::models::plan::Invoice;
use crate::AppState;

pub async fn list_invoices(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let invoices = sqlx::query_as::<_, Invoice>(
        "SELECT * FROM invoices WHERE aid = $1 ORDER BY created_at DESC",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"invoices": invoices})))
}

pub async fn get_invoice(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let invoice = sqlx::query_as::<_, Invoice>("SELECT * FROM invoices WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Invoice not found".to_string()))?;

    Ok(Json(json!({"invoice": invoice})))
}
