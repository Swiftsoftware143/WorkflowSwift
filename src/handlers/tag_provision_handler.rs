//! Tag provision webhook — receives FunnelSwift system tag assignments
//! and auto-provisions a free-tier lead/client record in WorkflowSwift.
//!
//! POST /api/v1/internal/tag-provision
//! Protected by X-Internal-Key header matching INTERNAL_SYNC_KEY env var.

use axum::{extract::State, http::HeaderMap, Json};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;
use crate::error::{AppError, ApiResult};

/// Payload received from FunnelSwift tag webhook
#[derive(Debug, Deserialize)]
pub struct TagProvisionRequest {
    pub contact: TagProvisionContact,
    pub tag: TagProvisionTag,
    pub source: String,
    pub timestamp: String,
}

#[derive(Debug, Deserialize)]
pub struct TagProvisionContact {
    pub id: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company: Option<String>,
    pub custom_fields: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct TagProvisionTag {
    pub name: String,
    pub campaign_id: Option<String>,
    pub metadata: Option<Value>,
}

/// POST /api/v1/internal/tag-provision
/// Receives FunnelSwift tag webhook, validates internal key,
/// creates a lead/client record in WorkflowSwift idempotently.
pub async fn handle_tag_provision(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TagProvisionRequest>,
) -> ApiResult<impl IntoResponse> {
    // 1. Validate internal key
    let key = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = state.config.internal_sync_key.as_str();
    if key != expected {
        tracing::warn!(
            "tag_provision: invalid internal key (got {}, expected {})",
            key, expected
        );
        return Err(AppError::Forbidden("Invalid internal key".into()));
    }

    let email = req.contact.email.as_deref().unwrap_or("").trim().to_lowercase();
    let first_name = req.contact.first_name.as_deref().unwrap_or("").trim().to_string();
    let last_name = req.contact.last_name.as_deref().unwrap_or("").trim().to_string();
    let company_name = req.contact.company.as_deref().unwrap_or("").trim().to_string();
    let phone = req.contact.phone.as_deref().unwrap_or("").trim().to_string();

    tracing::info!(
        "tag_provision: received for tag={} email={} first={} last={} company={}",
        req.tag.name, email, first_name, last_name, company_name
    );

    // 2. Find a default account if no campaign_id is provided
    let default_aid = sqlx::query_scalar::<_, Uuid>(
        r#"SELECT id FROM accounts ORDER BY created_at ASC LIMIT 1"#
    )
    .fetch_optional(&state.db)
    .await?
    .unwrap_or(Uuid::nil());
    
    let aid = req.tag.campaign_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .unwrap_or(default_aid);

    // 3. Check if a client already exists with this email
    if !email.is_empty() {
        let existing: Option<(Uuid,)> = sqlx::query_as(
            r#"SELECT id FROM clients WHERE email = $1 AND is_active = true LIMIT 1"#
        )
        .bind(&email)
        .fetch_optional(&state.db)
        .await?;

        if let Some((client_id,)) = existing {
            tracing::info!("tag_provision: client already exists id={}", client_id);
            return Ok((axum::http::StatusCode::OK, Json(json!({
                "status": "already_exists",
                "client_id": client_id.to_string(),
            }))));
        }
    }

    // 4. Create a new client record
    let client_id = Uuid::new_v4();
    let client_name = if !first_name.is_empty() && !last_name.is_empty() {
        format!("{} {}", first_name, last_name)
    } else if !first_name.is_empty() {
        first_name.clone()
    } else if !company_name.is_empty() {
        company_name.clone()
    } else {
        format!("FS-Lead-{}", &client_id.to_string()[..8])
    };

    sqlx::query(
        r#"INSERT INTO clients (id, aid, name, email, phone, is_active, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, true, NOW(), NOW())"#
    )
    .bind(client_id)
    .bind(aid)
    .bind(&client_name)
    .bind(if email.is_empty() { None } else { Some(&email) })
    .bind(if phone.is_empty() { None } else { Some(&phone) })
    .execute(&state.db)
    .await?;

    tracing::info!(
        "tag_provision: created client {} ({}) in account {}",
        client_id, client_name, aid
    );

    Ok((axum::http::StatusCode::CREATED, Json(json!({
        "status": "provisioned",
        "client_id": client_id.to_string(),
        "account_id": aid.to_string(),
    }))))
}
