//! Platform billing settings (admin): how a customer pays while card payments are off.
//!
//! Routes:
//!   GET /api/v1/admin/billing-settings — the operator's saved words (empty string = feature off)
//!   PUT /api/v1/admin/billing-settings — save them (`{"payment_instructions": "..."}`)
//!
//! Mounted inside `admin_routes` (which is nested at `/api/v1/admin`). That nest carries NO role
//! layer in this app — `auth::middleware` only proves a credential, and each admin handler decides
//! — so BOTH arms call `admin_settings_handler::require_admin` (the app's ONE admin gate, keyed on
//! `claims.perm_is_super_admin`). MEASURED 2026-11-16 on the first deploy of this file: without
//! that call a workspace owner's token read AND wrote the platform setting with 200. Do not drop
//! it; `require_admin` here is live code, not a second unreachable check.
//!
//! Why this exists: with no `payment_providers` row, `POST /api/v1/checkout/create` refuses every
//! paid tier and the plan REQUEST is the buying path. The customer was told a person would reply
//! and never how to actually pay. FunnelSwift, IncentiveSwift and ADASwift closed this gap on
//! 2026-11-16; this is the WorkflowSwift side.
//!
//! The stored value is read in two places, both through `billing_instructions::load`: the
//! customer's till status (`GET /api/v1/checkout/status`) and the plan-request receipt mail.

use crate::auth::models::Claims;
use crate::billing_instructions;
use crate::error::{ApiResult, AppError};
use crate::handlers::admin_settings_handler::require_admin;
use crate::state::AppState;
use axum::{extract::Extension, extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};

/// GET /api/v1/admin/billing-settings
pub async fn get_billing_settings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    require_admin(&claims)?;
    let saved = billing_instructions::load(&state.db).await;
    Ok(Json(json!({
        "payment_instructions": saved.clone().unwrap_or_default(),
        "payment_instructions_set": saved.is_some(),
        "max_length": billing_instructions::MAX_LEN,
        "message": if saved.is_some() {
            "A customer who asks to buy is shown these words, and they are printed in the receipt email."
        } else {
            "Nothing is shown to a customer who asks to buy, and the receipt email is unchanged."
        },
    })))
}

#[derive(Deserialize)]
pub struct BillingSettingsBody {
    /// The operator's own words. Absent keeps the stored value; an empty string CLEARS it (the
    /// feature goes back to off).
    pub payment_instructions: Option<String>,
}

/// PUT /api/v1/admin/billing-settings
pub async fn update_billing_settings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<BillingSettingsBody>,
) -> ApiResult<Json<Value>> {
    require_admin(&claims)?;
    let Some(text) = body.payment_instructions else {
        return Err(AppError::BadRequest(
            "Send {\"payment_instructions\": \"...\"}. An empty string clears it.".to_string(),
        ));
    };
    billing_instructions::save(&state.db, &text)
        .await
        .map_err(AppError::BadRequest)?;

    let saved = billing_instructions::load(&state.db).await;
    Ok(Json(json!({
        "success": true,
        "payment_instructions": saved.clone().unwrap_or_default(),
        "payment_instructions_set": saved.is_some(),
    })))
}
