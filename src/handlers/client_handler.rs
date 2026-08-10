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
use crate::features;
use crate::models::client::{Client, CreateClientRequest, UpdateClientRequest};
use crate::AppState;

pub async fn list_clients(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let clients = sqlx::query_as::<_, Client>(
        "SELECT * FROM clients WHERE aid = $1 AND is_active = true ORDER BY name ASC",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"clients": clients})))
}

pub async fn create_client(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateClientRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_clients", "Clients").await?;

    if req.name.is_empty() {
        return Err(AppError::Validation("Client name is required".to_string()));
    }

    let client = sqlx::query_as::<_, Client>(
        r#"INSERT INTO clients (id, aid, name, email, phone, website, industry, notes)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(&req.name)
    .bind(&req.email)
    .bind(&req.phone)
    .bind(&req.website)
    .bind(&req.industry)
    .bind(&req.notes)
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"client": client}))))
}

pub async fn get_client(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let client = sqlx::query_as::<_, Client>("SELECT * FROM clients WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Client not found".to_string()))?;

    // Get contacts for this client
    let contacts = sqlx::query(
        r#"SELECT id::text, client_id::text, name, email, phone, role, is_primary, created_at::text
           FROM client_contacts WHERE client_id = $1 ORDER BY is_primary DESC, name ASC"#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    use sqlx::Row;
    let contacts_json: Vec<serde_json::Value> = contacts
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<&str, _>("id").unwrap_or(""),
                "name": r.try_get::<&str, _>("name").unwrap_or(""),
                "email": r.try_get::<Option<&str>, _>("email").unwrap_or(None),
                "phone": r.try_get::<Option<&str>, _>("phone").unwrap_or(None),
                "role": r.try_get::<Option<&str>, _>("role").unwrap_or(None),
                "is_primary": r.try_get::<bool, _>("is_primary").unwrap_or(false),
            })
        })
        .collect();

    Ok(Json(json!({"client": client, "contacts": contacts_json})))
}

pub async fn update_client(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateClientRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let existing = sqlx::query_as::<_, Client>("SELECT * FROM clients WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound("Client not found".to_string()))?;

    let name = req.name.unwrap_or(existing.name);
    let email = req.email.or(existing.email);
    let phone = req.phone.or(existing.phone);
    let website = req.website.or(existing.website);
    let industry = req.industry.or(existing.industry);
    let notes = req.notes.or(existing.notes);

    let client = sqlx::query_as::<_, Client>(
        r#"UPDATE clients SET name = $1, email = $2, phone = $3, website = $4, industry = $5, notes = $6, updated_at = NOW()
           WHERE id = $7 AND aid = $8
           RETURNING *"#,
    )
    .bind(&name)
    .bind(&email)
    .bind(&phone)
    .bind(&website)
    .bind(&industry)
    .bind(&notes)
    .bind(id)
    .bind(aid)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(json!({"client": client})))
}

pub async fn delete_client(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query(
        "UPDATE clients SET is_active = false, updated_at = NOW() WHERE id = $1 AND aid = $2",
    )
    .bind(id)
    .bind(aid)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Client not found".to_string()));
    }

    Ok(Json(json!({"message": "Client deleted successfully"})))
}
