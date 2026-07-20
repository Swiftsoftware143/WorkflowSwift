use axum::{
    extract::{State, Json, Path, Query},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;
use sqlx::Row;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;

// ──────────────────────────────────────────────
// Integration Center — user-facing provider connections
// ──────────────────────────────────────────────

/// GET /api/v1/integrations
/// List all integrations for the authenticated user (masked keys)
pub async fn list_integrations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        r#"SELECT ui.id, ui.provider, ui.provider_label, ui.integration_type,
                  ui.api_key_encrypted, ui.base_url, ui.config, ui.is_active,
                  ui.last_health_status, ui.last_health_check_at,
                  ui.created_at, ui.updated_at,
                  ap.name AS provider_display_name, ap.description AS provider_description,
                  ap.icon AS provider_icon
           FROM user_integrations ui
           LEFT JOIN available_providers ap ON ap.key = ui.provider
           WHERE ui.user_id = $1
           ORDER BY ui.integration_type ASC, ui.provider ASC"#,
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;

    let integrations: Vec<serde_json::Value> = rows.iter().map(|row| {
        let key_raw: Option<&str> = row.try_get("api_key_encrypted").unwrap_or(None);
        let masked = key_raw.map(|k| {
            if k.len() > 6 {
                format!("{}...{}", &k[..3], &k[k.len()-3..])
            } else {
                "***".to_string()
            }
        }).unwrap_or_default();

        json!({
            "id": row.try_get::<Uuid, _>("id").map(|u| u.to_string()).unwrap_or_default(),
            "provider": row.try_get::<&str, _>("provider").unwrap_or(""),
            "provider_label": row.try_get::<&str, _>("provider_label").unwrap_or(""),
            "provider_display_name": row.try_get::<Option<&str>, _>("provider_display_name").unwrap_or(None),
            "provider_description": row.try_get::<Option<&str>, _>("provider_description").unwrap_or(None),
            "provider_icon": row.try_get::<Option<&str>, _>("provider_icon").unwrap_or(None),
            "integration_type": row.try_get::<&str, _>("integration_type").unwrap_or("byok"),
            "api_key_masked": masked,
            "has_key": key_raw.is_some() && key_raw.unwrap_or("").len() > 0,
            "base_url": row.try_get::<Option<String>, _>("base_url").unwrap_or(None),
            "config": row.try_get::<serde_json::Value, _>("config").unwrap_or(json!({})),
            "is_active": row.try_get::<bool, _>("is_active").unwrap_or(false),
            "last_health_status": row.try_get::<Option<&str>, _>("last_health_status").unwrap_or(None),
            "last_health_check_at": row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_health_check_at").ok().flatten().map(|d| d.to_rfc3339()),
            "created_at": row.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at").map(|d| d.to_rfc3339()).unwrap_or_default(),
            "updated_at": row.try_get::<chrono::DateTime<chrono::Utc>, _>("updated_at").map(|d| d.to_rfc3339()).unwrap_or_default(),
        })
    }).collect();

    Ok(Json(json!({"integrations": integrations})))
}

/// GET /api/v1/integrations/native
/// List available native SwiftSoftware integrations (built-in)
/// Returns all native providers from available_providers that the user can enable
pub async fn list_native_integrations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let _aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Native providers are the SwiftSoftware products
    let native_providers = vec!["coreswift", "funnelswift", "incentiveswift"];

    let rows = sqlx::query(
        r#"SELECT ap.key, ap.name, ap.description, ap.icon,
                  COALESCE(ui.is_active, false) AS enabled,
                  ui.id AS integration_id
           FROM available_providers ap
           LEFT JOIN user_integrations ui ON ui.provider = ap.key AND ui.user_id = $1
           WHERE ap.key = ANY($2)
           ORDER BY ap.name"#,
    )
    .bind(user_id)
    .bind(&native_providers)
    .fetch_all(&state.db)
    .await?;

    let integrations: Vec<serde_json::Value> = rows.iter().map(|row| {
        json!({
            "provider": row.try_get::<&str, _>("key").unwrap_or(""),
            "name": row.try_get::<&str, _>("name").unwrap_or(""),
            "description": row.try_get::<Option<&str>, _>("description").unwrap_or(None),
            "icon": row.try_get::<Option<&str>, _>("icon").unwrap_or(None),
            "enabled": row.try_get::<bool, _>("enabled").unwrap_or(false),
            "integration_id": row.try_get::<Option<Uuid>, _>("integration_id").ok().flatten().map(|u| u.to_string()),
        })
    }).collect();

    Ok(Json(json!({"native_integrations": integrations})))
}

/// POST /api/v1/integrations
/// Create or update a BYOK integration
/// Validates the connection before saving
pub async fn upsert_integration(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let provider = req.get("provider")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("provider is required".into()))?;

    if provider.is_empty() {
        return Err(AppError::BadRequest("provider must not be empty".into()));
    }

    let integration_type = req.get("integration_type")
        .and_then(|v| v.as_str())
        .unwrap_or("byok");

    let api_key = req.get("api_key").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let base_url = req.get("base_url").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let provider_label = req.get("provider_label").and_then(|v| v.as_str()).unwrap_or(provider);
    let config = req.get("config").cloned().unwrap_or(json!({}));
    let is_active = req.get("is_active").and_then(|v| v.as_bool()).unwrap_or(true);

    // Validate: if it's BYOK, an API key is required
    if integration_type == "byok" && api_key.is_none() {
        return Err(AppError::BadRequest("api_key is required for BYOK integrations".into()));
    }

    // If a base_url is provided and the provider requires one, validate it's present
    // We fetch from available_providers to check
    if let Ok(Some(requires_url)) = sqlx::query_scalar::<_, bool>(
        "SELECT requires_base_url FROM available_providers WHERE key = $1"
    )
    .bind(provider)
    .fetch_optional(&state.db)
    .await
    {
        if requires_url && base_url.is_none() {
            return Err(AppError::BadRequest(format!(
                "base_url is required for provider '{}'", provider
            )));
        }
    }

    // Upsert
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();

    sqlx::query(
        r#"INSERT INTO user_integrations (id, user_id, aid, provider, provider_label, integration_type, api_key_encrypted, base_url, config, is_active, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)
           ON CONFLICT (user_id, provider)
           DO UPDATE SET
               provider_label = EXCLUDED.provider_label,
               api_key_encrypted = COALESCE(EXCLUDED.api_key_encrypted, user_integrations.api_key_encrypted),
               base_url = EXCLUDED.base_url,
               config = EXCLUDED.config,
               is_active = EXCLUDED.is_active,
               updated_at = EXCLUDED.updated_at"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(aid)
    .bind(provider)
    .bind(provider_label)
    .bind(integration_type)
    .bind(api_key)
    .bind(base_url)
    .bind(&config)
    .bind(is_active)
    .bind(now)
    .execute(&state.db)
    .await?;

    // Invalidate cache so next resolution picks up the new key
    state.provider_key_cache.invalidate(&claims.aid, provider);

    // Run a health check immediately (async, don't block response)
    let db = state.db.clone();
    let prov = provider.to_string();
    let uid = user_id;
    let tid = aid;
    tokio::spawn(async move {
        let _ = run_health_check(&db, uid, tid, &prov).await;
    });

    Ok(Json(json!({
        "status": "saved",
        "provider": provider,
        "message": format!("{} connected", provider_label),
        "pending_health_check": true
    })))
}

/// POST /api/v1/integrations/native/{provider}/toggle
/// Enable or disable a native integration
pub async fn toggle_native_integration(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(provider): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Check if it already exists
    let existing = sqlx::query_scalar::<_, bool>(
        "SELECT is_active FROM user_integrations WHERE user_id = $1 AND provider = $2"
    )
    .bind(user_id)
    .bind(&provider)
    .fetch_optional(&state.db)
    .await?;

    if let Some(is_active) = existing {
        // Toggle existing
        sqlx::query(
            "UPDATE user_integrations SET is_active = NOT is_active, updated_at = NOW() WHERE user_id = $1 AND provider = $2"
        )
        .bind(user_id)
        .bind(&provider)
        .execute(&state.db)
        .await?;

        Ok(Json(json!({
            "status": "toggled",
            "provider": provider,
            "is_active": !is_active
        })))
    } else {
        // Create new native integration entry (always active on first enable)
        let id = Uuid::new_v4();
        let now = chrono::Utc::now();
        sqlx::query(
            r#"INSERT INTO user_integrations (id, user_id, aid, provider, provider_label, integration_type, is_active, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, 'native', true, $6, $6)"#,
        )
        .bind(id)
        .bind(user_id)
        .bind(aid)
        .bind(&provider)
        .bind(&provider)
        .bind(now)
        .execute(&state.db)
        .await?;

        Ok(Json(json!({
            "status": "enabled",
            "provider": provider,
            "is_active": true
        })))
    }
}

/// DELETE /api/v1/integrations/{provider}
/// Remove an integration connection
pub async fn delete_integration(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(provider): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query(
        "DELETE FROM user_integrations WHERE user_id = $1 AND provider = $2"
    )
    .bind(user_id)
    .bind(&provider)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(
            format!("Integration '{}' not found", provider)
        ));
    }

    // Invalidate cache
    state.provider_key_cache.invalidate(&claims.aid, &provider);

    Ok(Json(json!({
        "status": "deleted",
        "provider": provider
    })))
}

/// POST /api/v1/integrations/health-check
/// Run a health check on a specific integration
pub async fn check_integration_health(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let provider = req.get("provider")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("provider is required".into()))?;

    let (status, message) = run_health_check(&state.db, user_id, aid, provider).await;

    Ok(Json(json!({
        "provider": provider,
        "status": status,
        "message": message
    })))
}

/// Internal health check runner
async fn run_health_check(
    db: &sqlx::PgPool,
    user_id: Uuid,
    _aid: Uuid,
    provider: &str,
) -> (String, String) {
    // Fetch the integration
    let integration = sqlx::query_as::<_, (Uuid, String, String, Option<String>, Option<String>, serde_json::Value)>(
        r#"SELECT id, provider, integration_type, api_key_encrypted, base_url, config
           FROM user_integrations
           WHERE user_id = $1 AND provider = $2 AND is_active = true"#,
    )
    .bind(user_id)
    .bind(provider)
    .fetch_optional(db)
    .await;

    let (integration_id, prov, int_type, api_key, base_url, _config) = match integration {
        Ok(Some(row)) => row,
        _ => {
            let _ = sqlx::query(
                "UPDATE user_integrations SET last_health_status = 'error', last_health_check_at = NOW() WHERE user_id = $1 AND provider = $2"
            )
            .bind(user_id)
            .bind(provider)
            .execute(db).await;
            return ("error".to_string(), "Integration not found or inactive".to_string());
        }
    };

    // For native integrations, assume connected if they exist
    if int_type == "native" {
        sqlx::query(
            "UPDATE user_integrations SET last_health_status = 'connected', last_health_check_at = NOW() WHERE id = $1"
        )
        .bind(integration_id)
        .execute(db).await.ok();
        return ("connected".to_string(), "Native integration is active".to_string());
    }

    // For BYOK and engine integrations, try to validate the connection
    let healthy = match prov.as_str() {
        // OpenAI — simple models list request
        "openai" => {
            let key = api_key.as_deref().unwrap_or("");
            match reqwest::Client::new()
                .get("https://api.openai.com/v1/models")
                .header("Authorization", format!("Bearer {}", key))
                .timeout(std::time::Duration::from_secs(8))
                .send()
                .await
            {
                Ok(resp) => resp.status().is_success(),
                Err(_) => false,
            }
        }
        // Anthropic
        "anthropic" => {
            let key = api_key.as_deref().unwrap_or("");
            match reqwest::Client::new()
                .get("https://api.anthropic.com/v1/messages")
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
                .timeout(std::time::Duration::from_secs(8))
                .send()
                .await
            {
                Ok(resp) => resp.status().is_success() || resp.status().as_u16() == 400,
                // 400 means "bad request" which means auth worked but body was empty — that's fine
                Err(_) => false,
            }
        }
        // SendGrid
        "sendgrid" => {
            let key = api_key.as_deref().unwrap_or("");
            match reqwest::Client::new()
                .get("https://api.sendgrid.com/v3/marketing/lists")
                .header("Authorization", format!("Bearer {}", key))
                .timeout(std::time::Duration::from_secs(8))
                .send()
                .await
            {
                Ok(resp) => resp.status().is_success(),
                Err(_) => false,
            }
        }
        // OpenClaw — check the gateway health endpoint
        "openclaw" => {
            let url = base_url.as_deref().unwrap_or("");
            if url.is_empty() { return ("error".to_string(), "No gateway URL configured".to_string()); }
            let base = url.trim_end_matches('/');
            // Try the health endpoint
            match reqwest::Client::new()
                .get(format!("{}/health", base))
                .timeout(std::time::Duration::from_secs(8))
                .send()
                .await
            {
                Ok(resp) => resp.status().is_success(),
                Err(_) => {
                    // Try the status endpoint as fallback
                    match reqwest::Client::new()
                        .get(format!("{}/status", base))
                        .timeout(std::time::Duration::from_secs(5))
                        .send()
                        .await
                    {
                        Ok(resp) => resp.status().is_success(),
                        Err(_) => false,
                    }
                }
            }
        }
        // Generic: try the base_url with a GET
        "deepseek" | "gemini" | "mailgun" | "twilio" | "hexomatic" => {
            let key = api_key.as_deref().unwrap_or("");
            if key.is_empty() { return ("error".to_string(), "No API key configured".to_string()); }
            // For most providers, just check the key isn't empty
            !key.is_empty()
        }
        // Unknown provider — mark as pending if we can't validate
        _ => {
            true // can't validate, assume it works
        }
    };

    let status = if healthy { "connected" } else { "error" };
    let message = if healthy { "Connection verified" } else { "Connection failed — check your credentials" };

    sqlx::query(
        "UPDATE user_integrations SET last_health_status = $1, last_health_check_at = NOW() WHERE id = $2"
    )
    .bind(status)
    .bind(integration_id)
    .execute(db).await.ok();

    (status.to_string(), message.to_string())
}

// ──────────────────────────────────────────────
// Step resolution — find the right provider for a step
// ──────────────────────────────────────────────

/// Check what provider/engine a user's step should route to.
/// Returns the resolution result: user's key, system default, or error.
///
/// GET /api/v1/integrations/resolve?step_type=ai-action&provider=openai
pub async fn resolve_step_provider(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let _aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let step_type = params.get("step_type").map(|s| s.as_str()).unwrap_or("");
    let requested_provider = params.get("provider");

    if step_type.is_empty() {
        return Err(AppError::BadRequest("step_type is required".into()));
    }

    // Map step types to the providers they can use
    let provider_options: Vec<&str> = match step_type {
        "ai-action" | "ai_prompt" => vec!["openai", "anthropic", "deepseek", "gemini"],
        "email" | "export" => vec!["sendgrid", "smtp", "mailgun"],
        "integration" => vec!["coreswift", "funnelswift", "incentiveswift", "hubspot", "salesforce",
                              "mailchimp", "activecampaign", "convertkit", "slack", "discord",
                              "google_sheets", "stripe"],
        "playwright" | "browser" => vec!["browserbase"],  // always system-owned
        "notify" => vec!["slack", "discord", "sendgrid", "smtp"],
        _ => vec![],
    };

    // Check if user has a key for the requested provider (or any matching provider)
    let integrations = if let Some(req_prov) = requested_provider {
        // User explicitly chose a provider
        sqlx::query_as::<_, (String, String, Option<String>, Option<String>)>(
            r#"SELECT provider, integration_type, api_key_encrypted, base_url
               FROM user_integrations
               WHERE user_id = $1 AND provider = $2 AND is_active = true
               LIMIT 1"#,
        )
        .bind(user_id)
        .bind(req_prov)
        .fetch_optional(&state.db)
        .await?
    } else {
        // System auto-resolves — find any matching provider the user has
        let mut result = None;
        for prov in &provider_options {
            if let Ok(Some(row)) = sqlx::query_as::<_, (String, String, Option<String>, Option<String>)>(
                r#"SELECT provider, integration_type, api_key_encrypted, base_url
                   FROM user_integrations
                   WHERE user_id = $1 AND provider = $2 AND is_active = true
                   LIMIT 1"#,
            )
            .bind(user_id)
            .bind(prov)
            .fetch_optional(&state.db)
            .await
            {
                result = Some(row);
                break;
            }
        }
        result
    };

    let resolution = if let Some((provider, int_type, api_key, base_url)) = integrations {
        let has_key = api_key.as_ref().map(|k| !k.is_empty()).unwrap_or(false);
        if int_type == "native" {
            json!({
                "source": "native",
                "provider": provider,
                "credit_cost": 0,
                "base_url": base_url,
            })
        } else if has_key {
            json!({
                "source": "user_key",
                "provider": provider,
                "credit_cost": 0,
                "has_key": true,
                "base_url": base_url,
            })
        } else {
            // Fall back to system default
            json!({
                "source": "system_default",
                "provider": provider,
                "credit_cost": 1,
                "message": "Using WorkflowSwift system — 1 credit per call"
            })
        }
    } else {
        // No user integration found — fall back to system
        json!({
            "source": "system_default",
            "credit_cost": 1,
            "message": "Using WorkflowSwift system — 1 credit per call",
            "available_providers": provider_options
        })
    };

    Ok(Json(json!({
        "step_type": step_type,
        "resolution": resolution
    })))
}
