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

const COLS: &str =
    "SELECT id, aid, plan_id, amount::text AS amount, status, due_date, paid_at, created_at FROM invoices";

pub async fn list_invoices(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let invoices =
        sqlx::query_as::<_, Invoice>(&format!("{COLS} WHERE aid = $1 ORDER BY created_at DESC"))
            .bind(aid)
            .fetch_all(&state.db)
            .await?;

    // `items` is what the admin shell's generic list renderer reads, `invoices` is the original
    // key kept for any existing caller — both point at the same rows.
    Ok(Json(
        json!({"invoices": &invoices, "items": &invoices, "count": invoices.len()}),
    ))
}

pub async fn get_invoice(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let invoice = sqlx::query_as::<_, Invoice>(&format!("{COLS} WHERE id = $1 AND aid = $2"))
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Invoice not found".to_string()))?;

    Ok(Json(json!({"item": &invoice, "invoice": invoice})))
}
