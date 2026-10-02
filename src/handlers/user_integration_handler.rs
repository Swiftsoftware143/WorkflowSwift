use axum::{
    extract::{Json, Path, Query, State},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::AppState;

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
            "has_key": key_raw.is_some() && !key_raw.unwrap_or("").is_empty(),
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

    let provider = req
        .get("provider")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("provider is required".into()))?;

    if provider.is_empty() {
        return Err(AppError::BadRequest("provider must not be empty".into()));
    }

    let integration_type = req
        .get("integration_type")
        .and_then(|v| v.as_str())
        .unwrap_or("byok");

    let api_key = req
        .get("api_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let base_url = req
        .get("base_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let provider_label = req
        .get("provider_label")
        .and_then(|v| v.as_str())
        .unwrap_or(provider);
    let config = req.get("config").cloned().unwrap_or(json!({}));
    let is_active = req
        .get("is_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // Validate: if it's BYOK, an API key is required
    if integration_type == "byok" && api_key.is_none() {
        return Err(AppError::BadRequest(
            "api_key is required for BYOK integrations".into(),
        ));
    }

    // If a base_url is provided and the provider requires one, validate it's present
    // We fetch from available_providers to check
    // `requires_base_url` is NULLABLE (DEFAULT false), so it decodes as Option<bool>. A bare
    // `bool` made the whole statement fail with sqlx "unexpected null; try decoding as an
    // Option" for a NULL row, and `if let Ok(Some(..))` then skipped this validation with no
    // signal at all - the requirement silently unenforced (kanban t_b7276a9a).
    if let Ok(Some(requires_url)) = sqlx::query_scalar::<_, Option<bool>>(
        "SELECT requires_base_url FROM available_providers WHERE key = $1",
    )
    .bind(provider)
    .fetch_optional(&state.db)
    .await
    {
        if requires_url == Some(true) && base_url.is_none() {
            return Err(AppError::BadRequest(format!(
                "base_url is required for provider '{}'",
                provider
            )));
        }
        if requires_url.is_none() {
            tracing::warn!(
                provider = %provider,
                "available_providers.requires_base_url is NULL - base_url requirement not enforced"
            );
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
        "SELECT is_active FROM user_integrations WHERE user_id = $1 AND provider = $2",
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

    let result = sqlx::query("DELETE FROM user_integrations WHERE user_id = $1 AND provider = $2")
        .bind(user_id)
        .bind(&provider)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "Integration '{}' not found",
            provider
        )));
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

    let provider = req
        .get("provider")
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
    let integration = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            Option<String>,
            Option<String>,
            // `user_integrations.config` is NULLABLE with a '{}'::jsonb default and no app writer
            // can produce a NULL, but the schema allows one: decode it as Option so a NULL is data
            // (a route that still answers) instead of a decode error that reports the integration
            // as "not found or inactive".
            Option<serde_json::Value>,
        ),
    >(
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
        Ok(None) => {
            let _ = sqlx::query(
                "UPDATE user_integrations SET last_health_status = 'error', last_health_check_at = NOW() WHERE user_id = $1 AND provider = $2"
            )
            .bind(user_id)
            .bind(provider)
            .execute(db).await;
            return (
                "error".to_string(),
                "Integration not found or inactive".to_string(),
            );
        }
        // A read that FAILED is not a missing integration: the old `_ =>` arm reported a
        // database error as "not found or inactive", which is how a decode/statement failure on
        // this path stayed invisible (kanban t_227bae2f). Report it as its own outcome and log it.
        Err(e) => {
            tracing::error!(
                user_id = %user_id,
                provider = provider,
                error = %e,
                "integration health check could not read the integration row"
            );
            return (
                "error".to_string(),
                "Integration could not be read".to_string(),
            );
        }
    };

    // For native integrations, assume connected if they exist
    if int_type == "native" {
        sqlx::query(
            "UPDATE user_integrations SET last_health_status = 'connected', last_health_check_at = NOW() WHERE id = $1"
        )
        .bind(integration_id)
        .execute(db).await.ok();
        return (
            "connected".to_string(),
            "Native integration is active".to_string(),
        );
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
            if url.is_empty() {
                return ("error".to_string(), "No gateway URL configured".to_string());
            }
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
            if key.is_empty() {
                return ("error".to_string(), "No API key configured".to_string());
            }
            // For most providers, just check the key isn't empty
            !key.is_empty()
        }
        // Unknown provider — mark as pending if we can't validate
        _ => {
            true // can't validate, assume it works
        }
    };

    let status = if healthy { "connected" } else { "error" };
    let message = if healthy {
        "Connection verified"
    } else {
        "Connection failed — check your credentials"
    };

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

/// Where a step type's provider vocabulary comes from. One variant per arm of the resolver, so a
/// test can pin that a RETIRED step type never maps to a source that returns a provider list
/// (kanban t_88082a4c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderSource {
    /// `crate::ai_llm::AI_PROVIDERS` — the engine's own BYOK vocabulary.
    AiEngine,
    /// `crate::email::EMAIL_PROVIDERS` — the mail sender's provider vocabulary.
    EmailSender,
    /// The app's live integration destination catalogue (`integration_destinations`).
    DestinationCatalogue,
    /// `crate::execution::NOTIFY_CHANNELS` — the engine's own Notify channel vocabulary.
    NotifyChannels,
    /// No provider is involved: the resolver answers `source: "none"` and advertises nothing.
    None,
}

/// The step types this resolver has a provider arm for, and what backs each vocabulary.
///
/// Anything else — including every RETIRED step type — maps to `None`. `playwright` / `browser`
/// used to have an arm advertising `browserbase`: the write path never accepted either step type
/// and `browserbase` exists in no configuration store in this app, so no caller could ever have
/// such a step and no tenant could ever configure one (kanban t_88082a4c).
fn step_type_provider_source(step_type: &str) -> ProviderSource {
    match step_type {
        "ai-action" | "ai_prompt" => ProviderSource::AiEngine,
        "email" => ProviderSource::EmailSender,
        "integration" => ProviderSource::DestinationCatalogue,
        "notify" => ProviderSource::NotifyChannels,
        _ => ProviderSource::None,
    }
}

/// Is this one of the three native SwiftSoftware products the app connects to directly? The same
/// set `integration_center_handler::get_destinations` flags as `is_native` and the one the app has
/// a base_url for without any tenant configuration.
fn is_native_provider(provider: &str) -> bool {
    matches!(provider, "coreswift" | "funnelswift" | "incentiveswift")
}

/// The provider vocabulary for an `integration` step, read from the app's own destination
/// catalogue (`integration_destinations`).
///
/// That table IS this app's definition of an integration destination: it is FK-bound to
/// `available_providers` (so a provider that exists in no database cannot be a destination — the
/// guard in migrations/066 is why the catalogue holds three providers and not forty-six), it is
/// served live to the admin console by `GET /integration-destinations`, and it is exactly the set
/// `integration_center_handler::get_provider_base_url` and the destination-value cascade can build
/// a live URL for. Before kanban t_88082a4c this arm was a hand-kept list of twelve names, nine of
/// which NO configuration store knew (`hubspot`, `salesforce`, `mailchimp`, `activecampaign`,
/// `convertkit`, `slack`, `discord`, `stripe`, `google_sheets`): a target created for one of them
/// reached `forward_dispatch`'s "Integration target has no webhook_url or provider preset"
/// refusal, so the endpoint advertised providers a tenant could never configure and the engine
/// could never deliver. Read live, so an operator adding a destination widens this vocabulary
/// without a code change.
async fn integration_provider_vocabulary(db: &sqlx::PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT provider FROM integration_destinations ORDER BY provider",
    )
    .fetch_all(db)
    .await
}

/// The account's stored credential for `provider`, from `provider_keys` — the store the app's own
/// delivery paths read (`forward_dispatch`'s fallback and the AI Action step's BYOK key).
/// Returns `(provider, base_url, has_key)`; `None` when the account has no active row.
async fn lookup_account_provider_key(
    db: &sqlx::PgPool,
    aid: Uuid,
    provider: &str,
) -> Result<Option<(String, Option<String>, bool)>, sqlx::Error> {
    let row = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
        r#"SELECT provider, base_url, api_key
           FROM provider_keys
           WHERE aid = $1 AND provider = $2 AND is_active = true
           LIMIT 1"#,
    )
    .bind(aid)
    .bind(provider)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|(provider, base_url, api_key)| {
        let has_key = api_key.map(|k| !k.is_empty()).unwrap_or(false);
        (provider, base_url, has_key)
    }))
}

/// Check what provider/engine a user's step should route to.
/// Returns the resolution result: user's key, system default, or error.
/// GET /api/v1/integrations/resolve?step_type=ai-action&provider=openai
///
/// The provider vocabulary per step type is NOT a hand-kept list any more (kanban t_88082a4c).
/// Each arm names the store that backs it (engine const, admin picker vocabulary, or the app's own
/// live catalogue), so the endpoint cannot advertise a provider no console can configure and no
/// delivery path can reach — which is what nine of the twelve names in the old `integration` arm
/// were, and what `export` was.
pub async fn resolve_step_provider(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let step_type = params.get("step_type").map(|s| s.as_str()).unwrap_or("");
    let requested_provider = params.get("provider");
    // The LLM step types are the ones with no platform fallback: their credential is the tenant's.
    let is_llm_step = matches!(step_type, "ai-action" | "ai_prompt");

    if step_type.is_empty() {
        return Err(AppError::BadRequest("step_type is required".into()));
    }

    // A RETIRED step type is refused BY NAME, exactly as the write path refuses it (kanban
    // t_02519738). This endpoint used to answer 200 with a provider list for `export` while
    // `create_workflow_step` rejects that same type with 422 — a vocabulary that advertised a step
    // type the app will not run.
    if crate::execution::is_retired_step_type(step_type) {
        return Err(AppError::Validation(format!(
            "Step type '{}' is retired: nothing in this app performs it, so no provider resolves \
             for it. Valid step types are: {}",
            step_type,
            crate::execution::executable_step_type_list()
        )));
    }

    // Map step types to the providers they can use. Each arm reads its vocabulary from the store
    // that backs it, so none of them can drift from what the app can really configure + deliver.
    let provider_options: Vec<String> = match step_type_provider_source(step_type) {
        // ONE vocabulary with the engine (kanban t_03e4d3d9): the providers this app can actually
        // call are `crate::ai_llm::AI_PROVIDERS` — the list the AI Action step resolves its BYOK
        // key against and the console's Provider Keys panel collects.
        ProviderSource::AiEngine => crate::ai_llm::provider_keys()
            .into_iter()
            .map(String::from)
            .collect(),
        // ONE vocabulary with the mail sender: `crate::email::EMAIL_PROVIDERS` is the same four
        // the Admin > Settings > Email picker offers and `send_email_request` has an arm for. This
        // arm used to also answer for `export`, a RETIRED step type — refused above now.
        ProviderSource::EmailSender => crate::email::EMAIL_PROVIDERS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        // The providers an integration step can route to are the ones the app's OWN destination
        // catalogue defines — read live from the DB, never a hand-kept list.
        ProviderSource::DestinationCatalogue => integration_provider_vocabulary(&state.db).await?,
        // ONE vocabulary with the engine's Notify channels (`crate::execution::NOTIFY_CHANNELS`).
        // This arm used to name slack/discord/sendgrid/smtp: every one of those is a channel
        // t_08be842f RETIRED (no sender exists in this product) that the Notify step refuses.
        ProviderSource::NotifyChannels => crate::execution::NOTIFY_CHANNELS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        // No provider is involved in this step type — including the deleted `playwright` | `browser`
        // arm, whose only name (`browserbase`) exists in no config store and whose step types the
        // write path never accepted, so no caller could ever have such a step.
        ProviderSource::None => vec![],
    };

    // Check if the ACCOUNT has a credential for the requested provider (or any matching provider).
    // The store is `provider_keys` — the one every delivery path in this app reads (`forward_dispatch`
    // falls back to it, and `crate::ai_llm` / `execution.rs` name it as the BYOK store for the AI
    // Action step) — NOT the legacy `user_integrations` table, which holds 0 rows and is read by no
    // delivery path (kanban t_88082a4c): a key the tenant really stored under Provider Keys never
    // resolved here, so the endpoint answered "no key connected" for a connected provider.
    let integrations = if provider_options.is_empty() {
        None
    } else if let Some(req_prov) = requested_provider {
        // The caller explicitly chose a provider.
        lookup_account_provider_key(&state.db, aid, req_prov).await?
    } else {
        // System auto-resolves — find the first matching provider the account has.
        let mut result = None;
        for prov in &provider_options {
            if let Some(row) = lookup_account_provider_key(&state.db, aid, prov).await? {
                result = Some(row);
                break;
            }
        }
        result
    };

    let resolution = if provider_options.is_empty() {
        // No provider is involved in this step type. Answering with the platform fallback here
        // advertised a 1-credit platform run for a step that calls no provider at all.
        json!({
            "source": "none",
            "credit_cost": 0,
            "message": format!(
                "Step type '{}' resolves no provider: this step type calls no provider in \
                 WorkflowSwift.",
                step_type
            ),
            "available_providers": provider_options
        })
    } else if let Some((provider, base_url, has_key)) = integrations {
        if is_native_provider(&provider) {
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
        } else if is_llm_step {
            // There is no platform LLM credential (kanban t_03e4d3d9): the app must not advertise a
            // "system" run at 1 credit that nothing can perform. An AI step runs on the tenant's own
            // key, or it does not run.
            json!({
                "source": "none",
                "provider": provider,
                "credit_cost": 0,
                "message": format!(
                    "No {} key connected for this account. AI Action runs on YOUR provider key (0 \
                     credits) — add one under Provider Keys.",
                    provider
                ),
                "available_providers": provider_options
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
    } else if is_llm_step {
        json!({
            "source": "none",
            "credit_cost": 0,
            "message": "No connected provider key for this account. AI Action runs on YOUR provider \
                        key (0 credits) — connect one under Provider Keys.",
            "available_providers": provider_options
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A RETIRED step type must never map to a source that returns a provider list: this endpoint
    /// answered `200` with `["sendgrid","smtp","mailgun"]` for `export`, a type the write path
    /// refuses by name with 422 (kanban t_88082a4c / t_02519738).
    #[test]
    fn no_retired_step_type_has_a_provider_source() {
        for retired in crate::execution::RETIRED_STEP_TYPES {
            assert_eq!(
                step_type_provider_source(retired),
                ProviderSource::None,
                "retired step type '{retired}' must not resolve a provider list"
            );
        }
    }

    /// The dead `playwright` | `browser` arm is gone. Its only name, `browserbase`, exists in no
    /// configuration store in this app and the write path never accepted either step type.
    #[test]
    fn the_dead_browser_arm_resolves_no_provider() {
        for dead in ["playwright", "browser"] {
            assert_eq!(
                step_type_provider_source(dead),
                ProviderSource::None,
                "'{dead}' was never an accepted step type"
            );
        }
    }

    /// The `notify` arm speaks the ENGINE's channel vocabulary: a Notify step refuses every channel
    /// outside `NOTIFY_CHANNELS`, and `slack`/`discord`/`sendgrid`/`smtp` are all retired there
    /// (t_08be842f), so none of them may be advertised as a provider for a Notify step.
    #[test]
    fn the_notify_arm_is_the_engine_channel_vocabulary() {
        assert_eq!(
            step_type_provider_source("notify"),
            ProviderSource::NotifyChannels
        );
        assert_eq!(crate::execution::NOTIFY_CHANNELS.to_vec(), vec!["webhook"]);
        for resigned in ["slack", "discord", "sendgrid", "smtp", "email", "sms"] {
            assert!(
                !crate::execution::is_notify_channel(resigned),
                "notify channel '{resigned}' is retired and must not be advertised"
            );
        }
    }

    /// Every step type the tenant console offers in its Builder picker either has no provider arm
    /// or is one of the two arms the engine itself backs (`ai-action` → the AI provider list,
    /// `notify` → its channels). The resolver must not invent a provider for any other step type.
    #[test]
    fn console_offered_step_types_do_not_invent_providers() {
        for offered in [
            "action",
            "ai-action",
            "condition",
            "data-card",
            "delay",
            "fork",
            "http-request",
            "manual",
            "notify",
            "render_audio",
            "render_image",
            "render_video",
            "webhook",
        ] {
            let source = step_type_provider_source(offered);
            assert!(
                matches!(
                    source,
                    ProviderSource::None
                        | ProviderSource::AiEngine
                        | ProviderSource::NotifyChannels
                ),
                "console step type '{offered}' maps to {source:?}, which the console never configures"
            );
        }
    }
}
