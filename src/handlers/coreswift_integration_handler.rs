//! Canonical CoreSwift spoke endpoints (fleet standard, same paths in every app):
//!
//!     GET  /api/v1/integrations/coreswift/status -> {"connected": bool, "base_url": "..."}
//!     GET  /api/v1/integrations/coreswift/lists  -> proxy hub GET  /api/external/lists
//!     POST /api/v1/integrations/coreswift/push   -> proxy hub POST /api/external/contacts
//!
//! These delegate to `handlers::coreswift_external` — the same module the automatic
//! capture-time push uses — so there is exactly ONE CoreSwift code path in this crate,
//! not a second "manual push" implementation that can drift from the capture path.

use axum::{extract::State, response::IntoResponse, Extension, Json};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::handlers::coreswift_external::{
    hub_get_lists, hub_probe, push_lead_to_coreswift, CapturedLead,
};
use crate::state::AppState;

fn account_id(claims: &Claims) -> ApiResult<Uuid> {
    Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)
}

/// GET /api/v1/integrations/coreswift/status
pub async fn coreswift_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    let conn = crate::handlers::coreswift_external::get_coreswift_connection(&state, aid).await;

    match conn {
        Some((_key, base_url)) => Ok(Json(json!({
            "provider": "coreswift",
            "connected": true,
            "base_url": base_url,
        }))),
        None => Ok(Json(json!({
            "provider": "coreswift",
            "connected": false,
            "base_url": serde_json::Value::Null,
        }))),
    }
}

/// GET /api/v1/integrations/coreswift/lists — proxy the hub's list picker.
pub async fn coreswift_lists(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;
    match hub_get_lists(&state, aid).await {
        Ok(body) => Ok((axum::http::StatusCode::OK, Json(body))),
        Err(e) => Err(AppError::BadRequest(e)),
    }
}

/// POST /api/v1/integrations/coreswift/push — THE manual fallback for the inbound path.
///
/// Body may carry the lead inline (`name`/`first_name`/`email`/`phone`/`company`); when it
/// does not, the account's most recently captured lead is pushed. Same helper either way.
pub async fn coreswift_push(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;

    let str_field = |k: &str| -> Option<String> {
        body.get(k)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    let list_id = str_field("list_id");
    let tags: Vec<String> = body
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let fields = body.get("fields").cloned().unwrap_or(json!({}));

    let lead = if body.get("email").is_some()
        || body.get("name").is_some()
        || body.get("first_name").is_some()
        || body.get("phone").is_some()
    {
        CapturedLead::from_parts(
            str_field("first_name"),
            str_field("last_name"),
            str_field("email"),
            str_field("phone"),
            str_field("company"),
            str_field("source"),
        )
    } else {
        // Manual fallback with no body: push the latest captured lead for this account.
        let row = sqlx::query(
            "SELECT name, email, phone, company, source FROM leads WHERE aid = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(aid)
        .fetch_optional(&state.db)
        .await?;

        let row = row.ok_or_else(|| {
            AppError::BadRequest("No captured lead to push — capture a lead first".into())
        })?;

        CapturedLead::from_name(
            &row.try_get::<String, _>("name").unwrap_or_default(),
            row.try_get::<Option<String>, _>("email").unwrap_or(None),
            row.try_get::<Option<String>, _>("phone").unwrap_or(None),
            row.try_get::<Option<String>, _>("company").unwrap_or(None),
            row.try_get::<Option<String>, _>("source").unwrap_or(None),
        )
    };

    let pushed = push_lead_to_coreswift(
        &state,
        aid,
        &lead,
        list_id,
        &tags,
        fields,
        "manual /integrations/coreswift/push",
    )
    .await;

    if !pushed {
        return Err(AppError::BadRequest(
            "CoreSwift push failed or account not connected (store a CoreSwift key via POST /api/v1/provider-keys)"
                .into(),
        ));
    }

    Ok(Json(json!({
        "status": "pushed",
        "provider": "coreswift",
        "contact": {
            "name": lead.full_name,
            "email": lead.email,
            "phone": lead.phone,
            "company": lead.company,
        },
    })))
}

/// POST /api/v1/provider-keys/:provider/test — live probe of a stored key.
/// CoreSwift is probed against the hub's `/api/external/lists`.
pub async fn test_provider_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(provider): axum::extract::Path<String>,
) -> ApiResult<impl IntoResponse> {
    let aid = account_id(&claims)?;

    if provider != "coreswift" {
        return Ok(Json(json!({
            "provider": provider,
            "ok": serde_json::Value::Null,
            "message": "No live connection test available for this provider yet",
        })));
    }

    match hub_probe(&state, aid).await {
        Ok((code, body)) => Ok(Json(json!({
            "provider": "coreswift",
            "ok": code == 200,
            "status": code,
            "message": if code == 200 {
                "CoreSwift reachable — key accepted".to_string()
            } else {
                format!("CoreSwift returned {code}: {body}")
            },
        }))),
        Err(e) => Ok(Json(json!({
            "provider": "coreswift",
            "ok": false,
            "message": e,
        }))),
    }
}
