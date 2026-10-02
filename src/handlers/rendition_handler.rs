use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::models::rendition::{
    AccountRendition, CreateRenditionRequest, ListRenditionsQuery, UpdateRenditionRequest,
};
use crate::AppState;

// ──────────────────────────────────────────────
// Integration Hub — Provider Categories
// ──────────────────────────────────────────────

/// GET /api/v1/provider-categories
/// Returns all available providers grouped by category.
/// This powers the Integration Hub — users see categorized apps.
pub async fn list_provider_categories(
    State(state): State<AppState>,
) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query(
        r#"SELECT key, name, description, category, requires_base_url, requires_metadata, icon
           FROM available_providers
           WHERE is_active = true OR is_active IS NULL
           ORDER BY category NULLS LAST, name ASC"#,
    )
    .fetch_all(&state.db)
    .await?;

    // Group by category
    let mut categories: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
        std::collections::BTreeMap::new();

    for row in &rows {
        use sqlx::Row;
        let key: String = row.try_get("key").unwrap_or_default();
        let name: String = row.try_get("name").unwrap_or_default();
        let description: Option<String> = row.try_get("description").ok();
        let category: Option<String> = row.try_get("category").ok();
        let requires_base_url: bool = row.try_get("requires_base_url").unwrap_or(false);
        let requires_metadata: Value = row.try_get("requires_metadata").unwrap_or(json!([]));
        let icon: Option<String> = row.try_get("icon").ok();

        let cat = category.unwrap_or_else(|| "uncategorized".to_string());
        categories.entry(cat).or_default().push(json!({
            "key": key,
            "name": name,
            "description": description,
            "requires_base_url": requires_base_url,
            "requires_metadata": requires_metadata,
            "icon": icon,
        }));
    }

    Ok(Json(json!({"categories": categories})))
}

/// GET /api/v1/provider-categories/:category
/// Returns providers in one specific category
pub async fn get_category_providers(
    State(state): State<AppState>,
    Path(category): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query(
        r#"SELECT key, name, description, category, requires_base_url, requires_metadata, icon
           FROM available_providers
           WHERE category = $1 AND (is_active = true OR is_active IS NULL)
           ORDER BY name ASC"#,
    )
    .bind(&category)
    .fetch_all(&state.db)
    .await?;

    let providers: Vec<serde_json::Value> = rows.iter().map(|row| {
        use sqlx::Row;
        json!({
            "key": row.try_get::<&str,_>("key").unwrap_or(""),
            "name": row.try_get::<&str,_>("name").unwrap_or(""),
            "description": row.try_get::<Option<&str>,_>("description").unwrap_or(None),
            "requires_base_url": row.try_get::<bool,_>("requires_base_url").unwrap_or(false),
            "requires_metadata": row.try_get::<Value,_>("requires_metadata").unwrap_or(json!([])),
            "icon": row.try_get::<Option<&str>,_>("icon").unwrap_or(None),
        })
    }).collect();

    Ok(Json(json!({"providers": providers, "category": category})))
}

/// GET /api/v1/step-types/:category
/// Returns which step types are available for a provider category.
/// e.g. category=video → step types like "render_video", "edit_video"
/// This maps Integration Hub categories to workflow step types.
pub async fn get_step_types_for_category(
    Path(category): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let step_types = match category.as_str() {
        "video" => vec![
            json!({"type": "render_video", "label": "Generate Video", "description": "Render a video using a connected video provider"}),
            json!({"type": "edit_video", "label": "Edit Video", "description": "Trim, stitch, or overlay video assets"}),
        ],
        "image" => vec![
            json!({"type": "render_image", "label": "Generate Image", "description": "Create an image using a connected image provider"}),
            json!({"type": "edit_image", "label": "Edit Image", "description": "Edit or transform an existing image"}),
        ],
        "audio" => vec![
            json!({"type": "render_audio", "label": "Generate Audio", "description": "Create voiceover or audio using a connected provider"}),
        ],
        "email" => vec![
            json!({"type": "send_email", "label": "Send Email", "description": "Send an email via connected email provider"}),
            json!({"type": "email_campaign", "label": "Email Campaign", "description": "Run an email campaign via connected provider"}),
        ],
        "sms" => vec![
            json!({"type": "send_sms", "label": "Send SMS", "description": "Send an SMS via connected SMS provider"}),
        ],
        "crm" => vec![
            json!({"type": "crm_action", "label": "CRM Action", "description": "Create, update, or lookup CRM records"}),
        ],
        "social" => vec![
            json!({"type": "social_post", "label": "Social Post", "description": "Post content to a social media account"}),
        ],
        "ai" => vec![
            json!({"type": "ai_action", "label": "AI Action", "description": "Run an AI prompt via connected AI provider"}),
        ],
        "landing-pages" | "rewards" => vec![
            json!({"type": "api_call", "label": "API Call", "description": "Make an authenticated API call to this provider"}),
        ],
        _ => vec![
            json!({"type": "api_call", "label": "API Call", "description": "Make an authenticated API call to this provider"}),
        ],
    };

    Ok(Json(
        json!({"category": category, "step_types": step_types}),
    ))
}

// ──────────────────────────────────────────────
// Rendition Gallery — CRUD
// ──────────────────────────────────────────────

/// POST /api/v1/renditions
/// Create a rendition record (called by workflow engine or n8n callback)
pub async fn create_rendition(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateRenditionRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    // Look up provider category for the given provider
    let provider_category: Option<String> =
        sqlx::query_scalar("SELECT category FROM available_providers WHERE key = $1")
            .bind(&req.provider)
            .fetch_optional(&state.db)
            .await?
            .flatten();

    // Calculate retention expiry
    let retention_days = req.retention_days.unwrap_or(90);
    let retention_expires_at = Utc::now() + Duration::days(retention_days as i64);

    let rendition = sqlx::query_as::<_, AccountRendition>(
        r#"INSERT INTO account_renditions
           (id, aid, user_id, workflow_id, instance_id, step_type, step_name,
            provider, provider_asset_id, provider_asset_url,
            preview_url, thumbnail_url, asset_type,
            provider_category, sort_order, parent_rendition_id,
            retention_expires_at, status, metadata)
           VALUES ($1, $2, $3, $4, $5, 'render_media', $6,
                   $7, $8, $9,
                   $10, $11, $12,
                   $13, $14, $15,
                   $16, 'active', $17)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(user_id)
    .bind(req.workflow_id)
    .bind(req.instance_id)
    .bind(req.step_name)
    .bind(&req.provider)
    .bind(&req.provider_asset_id)
    .bind(&req.provider_asset_url)
    .bind(&req.preview_url)
    .bind(&req.thumbnail_url)
    .bind(&req.asset_type)
    .bind(&provider_category)
    .bind(req.sort_order.unwrap_or(0))
    .bind(req.parent_rendition_id)
    .bind(retention_expires_at)
    .bind(req.metadata.unwrap_or(json!({})))
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"rendition": rendition}))))
}

// Gate rule 5d / class 14 (kanban t_15a04e9d): the completed statement for every possible
// present-filter combination of `ListRenditionsQuery`, as a COMPLETE compile-time literal. The
// listing used to be built at run time (a `String::from` base + a `format!` predicate per optional
// filter + `push_str` of the LIMIT/OFFSET indices), so its text was a function of the request and
// no query was visible at the site. Index = bitmask over (provider, asset_type, status,
// workflow_id, instance_id, search). The unit test at the bottom of this file asserts every entry
// is byte-identical to what the old builder produced for that mask.
static RENDITION_SQL: [&str; 64] = [
    "SELECT * FROM account_renditions WHERE aid = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND status = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND status = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND status = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND status = $4 ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND workflow_id = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND workflow_id = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND workflow_id = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND workflow_id = $4 ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND status = $2 AND workflow_id = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND status = $3 AND workflow_id = $4 ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND status = $3 AND workflow_id = $4 ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND status = $4 AND workflow_id = $5 ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND instance_id = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND instance_id = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND instance_id = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND instance_id = $4 ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND status = $2 AND instance_id = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND status = $3 AND instance_id = $4 ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND status = $3 AND instance_id = $4 ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND status = $4 AND instance_id = $5 ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND workflow_id = $2 AND instance_id = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND workflow_id = $3 AND instance_id = $4 ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND workflow_id = $3 AND instance_id = $4 ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND workflow_id = $4 AND instance_id = $5 ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND status = $2 AND workflow_id = $3 AND instance_id = $4 ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND status = $3 AND workflow_id = $4 AND instance_id = $5 ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND status = $3 AND workflow_id = $4 AND instance_id = $5 ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND status = $4 AND workflow_id = $5 AND instance_id = $6 ORDER BY created_at DESC LIMIT $7 OFFSET $8",
    "SELECT * FROM account_renditions WHERE aid = $1 AND (provider ILIKE $2 OR provider_asset_id ILIKE $2 OR step_name ILIKE $2) ORDER BY created_at DESC LIMIT $3 OFFSET $4",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND (provider ILIKE $3 OR provider_asset_id ILIKE $3 OR step_name ILIKE $3) ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND (provider ILIKE $3 OR provider_asset_id ILIKE $3 OR step_name ILIKE $3) ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND (provider ILIKE $4 OR provider_asset_id ILIKE $4 OR step_name ILIKE $4) ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND status = $2 AND (provider ILIKE $3 OR provider_asset_id ILIKE $3 OR step_name ILIKE $3) ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND status = $3 AND (provider ILIKE $4 OR provider_asset_id ILIKE $4 OR step_name ILIKE $4) ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND status = $3 AND (provider ILIKE $4 OR provider_asset_id ILIKE $4 OR step_name ILIKE $4) ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND status = $4 AND (provider ILIKE $5 OR provider_asset_id ILIKE $5 OR step_name ILIKE $5) ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND workflow_id = $2 AND (provider ILIKE $3 OR provider_asset_id ILIKE $3 OR step_name ILIKE $3) ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND workflow_id = $3 AND (provider ILIKE $4 OR provider_asset_id ILIKE $4 OR step_name ILIKE $4) ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND workflow_id = $3 AND (provider ILIKE $4 OR provider_asset_id ILIKE $4 OR step_name ILIKE $4) ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND workflow_id = $4 AND (provider ILIKE $5 OR provider_asset_id ILIKE $5 OR step_name ILIKE $5) ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND status = $2 AND workflow_id = $3 AND (provider ILIKE $4 OR provider_asset_id ILIKE $4 OR step_name ILIKE $4) ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND status = $3 AND workflow_id = $4 AND (provider ILIKE $5 OR provider_asset_id ILIKE $5 OR step_name ILIKE $5) ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND status = $3 AND workflow_id = $4 AND (provider ILIKE $5 OR provider_asset_id ILIKE $5 OR step_name ILIKE $5) ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND status = $4 AND workflow_id = $5 AND (provider ILIKE $6 OR provider_asset_id ILIKE $6 OR step_name ILIKE $6) ORDER BY created_at DESC LIMIT $7 OFFSET $8",
    "SELECT * FROM account_renditions WHERE aid = $1 AND instance_id = $2 AND (provider ILIKE $3 OR provider_asset_id ILIKE $3 OR step_name ILIKE $3) ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND instance_id = $3 AND (provider ILIKE $4 OR provider_asset_id ILIKE $4 OR step_name ILIKE $4) ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND instance_id = $3 AND (provider ILIKE $4 OR provider_asset_id ILIKE $4 OR step_name ILIKE $4) ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND instance_id = $4 AND (provider ILIKE $5 OR provider_asset_id ILIKE $5 OR step_name ILIKE $5) ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND status = $2 AND instance_id = $3 AND (provider ILIKE $4 OR provider_asset_id ILIKE $4 OR step_name ILIKE $4) ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND status = $3 AND instance_id = $4 AND (provider ILIKE $5 OR provider_asset_id ILIKE $5 OR step_name ILIKE $5) ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND status = $3 AND instance_id = $4 AND (provider ILIKE $5 OR provider_asset_id ILIKE $5 OR step_name ILIKE $5) ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND status = $4 AND instance_id = $5 AND (provider ILIKE $6 OR provider_asset_id ILIKE $6 OR step_name ILIKE $6) ORDER BY created_at DESC LIMIT $7 OFFSET $8",
    "SELECT * FROM account_renditions WHERE aid = $1 AND workflow_id = $2 AND instance_id = $3 AND (provider ILIKE $4 OR provider_asset_id ILIKE $4 OR step_name ILIKE $4) ORDER BY created_at DESC LIMIT $5 OFFSET $6",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND workflow_id = $3 AND instance_id = $4 AND (provider ILIKE $5 OR provider_asset_id ILIKE $5 OR step_name ILIKE $5) ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND workflow_id = $3 AND instance_id = $4 AND (provider ILIKE $5 OR provider_asset_id ILIKE $5 OR step_name ILIKE $5) ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND workflow_id = $4 AND instance_id = $5 AND (provider ILIKE $6 OR provider_asset_id ILIKE $6 OR step_name ILIKE $6) ORDER BY created_at DESC LIMIT $7 OFFSET $8",
    "SELECT * FROM account_renditions WHERE aid = $1 AND status = $2 AND workflow_id = $3 AND instance_id = $4 AND (provider ILIKE $5 OR provider_asset_id ILIKE $5 OR step_name ILIKE $5) ORDER BY created_at DESC LIMIT $6 OFFSET $7",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND status = $3 AND workflow_id = $4 AND instance_id = $5 AND (provider ILIKE $6 OR provider_asset_id ILIKE $6 OR step_name ILIKE $6) ORDER BY created_at DESC LIMIT $7 OFFSET $8",
    "SELECT * FROM account_renditions WHERE aid = $1 AND asset_type = $2 AND status = $3 AND workflow_id = $4 AND instance_id = $5 AND (provider ILIKE $6 OR provider_asset_id ILIKE $6 OR step_name ILIKE $6) ORDER BY created_at DESC LIMIT $7 OFFSET $8",
    "SELECT * FROM account_renditions WHERE aid = $1 AND provider = $2 AND asset_type = $3 AND status = $4 AND workflow_id = $5 AND instance_id = $6 AND (provider ILIKE $7 OR provider_asset_id ILIKE $7 OR step_name ILIKE $7) ORDER BY created_at DESC LIMIT $8 OFFSET $9",
];

/// GET /api/v1/renditions
/// List renditions for the current account (the gallery view)
pub async fn list_renditions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListRenditionsQuery>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let limit = query.limit.unwrap_or(50).min(200);
    let offset = query.offset.unwrap_or(0);

    // The statement is looked up as a compile-time literal (see RENDITION_SQL above); only the
    // CHOICE of literal is decided at run time.
    let mask = (query.provider.is_some() as u8)
        | ((query.asset_type.is_some() as u8) << 1)
        | ((query.status.is_some() as u8) << 2)
        | ((query.workflow_id.is_some() as u8) << 3)
        | ((query.instance_id.is_some() as u8) << 4)
        | ((query.search.is_some() as u8) << 5);

    let mut query_builder =
        sqlx::query_as::<_, AccountRendition>(RENDITION_SQL[mask as usize]).bind(aid);

    if let Some(ref _provider) = query.provider {
        query_builder = query_builder.bind(_provider);
    }
    if let Some(ref _asset_type) = query.asset_type {
        query_builder = query_builder.bind(_asset_type);
    }
    if let Some(ref _status) = query.status {
        query_builder = query_builder.bind(_status);
    }
    if let Some(_wf_id) = query.workflow_id {
        query_builder = query_builder.bind(_wf_id);
    }
    if let Some(_inst_id) = query.instance_id {
        query_builder = query_builder.bind(_inst_id);
    }
    if let Some(ref _search) = query.search {
        query_builder = query_builder.bind(_search);
    }

    query_builder = query_builder.bind(limit).bind(offset);

    let renditions = query_builder.fetch_all(&state.db).await?;

    // Count total
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM account_renditions WHERE aid = $1")
        .bind(aid)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

    Ok(Json(json!({
        "renditions": renditions,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

/// GET /api/v1/renditions/:id
/// Get a single rendition record
pub async fn get_rendition(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rendition = sqlx::query_as::<_, AccountRendition>(
        "SELECT * FROM account_renditions WHERE id = $1 AND aid = $2",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Rendition not found".to_string()))?;

    Ok(Json(json!({"rendition": rendition})))
}

/// PUT /api/v1/renditions/:id
/// Update a rendition (preview URL, status, retention, etc.)
pub async fn update_rendition(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateRenditionRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let existing = sqlx::query_as::<_, AccountRendition>(
        "SELECT * FROM account_renditions WHERE id = $1 AND aid = $2",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Rendition not found".to_string()))?;

    let preview_url = req.preview_url.or(existing.preview_url);
    let thumbnail_url = req.thumbnail_url.or(existing.thumbnail_url);
    let status = req.status.unwrap_or(existing.status);
    let retention_expires_at = req.retention_expires_at.or(existing.retention_expires_at);
    let sort_order = req.sort_order.unwrap_or(existing.sort_order);
    let metadata = req.metadata.unwrap_or(existing.metadata);

    let rendition = sqlx::query_as::<_, AccountRendition>(
        r#"UPDATE account_renditions
           SET preview_url = $1, thumbnail_url = $2, status = $3,
               retention_expires_at = $4, sort_order = $5,
               metadata = $6, updated_at = NOW()
           WHERE id = $7 AND aid = $8
           RETURNING *"#,
    )
    .bind(&preview_url)
    .bind(&thumbnail_url)
    .bind(&status)
    .bind(retention_expires_at)
    .bind(sort_order)
    .bind(&metadata)
    .bind(id)
    .bind(aid)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(json!({"rendition": rendition})))
}

/// DELETE /api/v1/renditions/:id
/// Soft-delete (set status to 'expired') or hard delete
pub async fn delete_rendition(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Soft-delete: set status to expired
    let result = sqlx::query(
        "UPDATE account_renditions SET status = 'expired', updated_at = NOW() WHERE id = $1 AND aid = $2"
    )
    .bind(id)
    .bind(aid)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Rendition not found".to_string()));
    }

    Ok(Json(json!({"status": "expired"})))
}

/// POST /api/v1/renditions/purge-expired
/// Admin endpoint — purge expired renditions beyond their retention window
pub async fn purge_expired_renditions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query(
        "UPDATE account_renditions
         SET status = 'purged', updated_at = NOW()
         WHERE aid = $1
           AND status = 'active'
           AND retention_expires_at IS NOT NULL
           AND retention_expires_at < NOW()",
    )
    .bind(aid)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "purged_count": result.rows_affected(),
        "message": format!("Purged {} expired renditions", result.rows_affected())
    })))
}

/// GET /api/v1/renditions/summary
/// Summary stats for the gallery (counts by type, provider, status)
pub async fn rendition_summary(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Count by asset type
    let by_type = sqlx::query(
        r#"SELECT asset_type, COUNT(*) as count
           FROM account_renditions
           WHERE aid = $1 AND status = 'active'
           GROUP BY asset_type
           ORDER BY count DESC"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let types: Vec<serde_json::Value> = by_type
        .iter()
        .map(|r| {
            use sqlx::Row;
            json!({
                "asset_type": r.try_get::<&str,_>("asset_type").unwrap_or(""),
                "count": r.try_get::<i64,_>("count").unwrap_or(0),
            })
        })
        .collect();

    // Count by provider
    let by_provider = sqlx::query(
        r#"SELECT provider, provider_category, COUNT(*) as count
           FROM account_renditions
           WHERE aid = $1 AND status = 'active'
           GROUP BY provider, provider_category
           ORDER BY count DESC"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let providers: Vec<serde_json::Value> = by_provider
        .iter()
        .map(|r| {
            use sqlx::Row;
            json!({
                "provider": r.try_get::<&str,_>("provider").unwrap_or(""),
                "category": r.try_get::<Option<&str>,_>("provider_category").unwrap_or(None),
                "count": r.try_get::<i64,_>("count").unwrap_or(0),
            })
        })
        .collect();

    // Total active count
    let total_active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM account_renditions WHERE aid = $1 AND status = 'active'",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // Count expiring soon (within 7 days)
    let expiring_soon: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM account_renditions
         WHERE aid = $1 AND status = 'active'
           AND retention_expires_at IS NOT NULL
           AND retention_expires_at < NOW() + INTERVAL '7 days'",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "total_active": total_active,
        "expiring_soon": expiring_soon,
        "by_type": types,
        "by_provider": providers,
    })))
}

/// GET /api/v1/renditions/workflow/:workflow_id
/// Get all renditions for a specific workflow (timeline view)
pub async fn list_workflow_renditions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(workflow_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let renditions = sqlx::query_as::<_, AccountRendition>(
        "SELECT * FROM account_renditions
         WHERE aid = $1 AND workflow_id = $2 AND status = 'active'
         ORDER BY sort_order ASC, created_at ASC",
    )
    .bind(aid)
    .bind(workflow_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"renditions": renditions})))
}

/// POST /api/v1/renditions/stitch
/// Create a parent rendition that groups child renditions (stitch grouping)
pub async fn create_stitch_group(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Stitched Rendition");
    let child_ids = req
        .get("child_ids")
        .and_then(|v| v.as_array())
        .ok_or_else(|| AppError::BadRequest("child_ids is required".into()))?;

    if child_ids.is_empty() {
        return Err(AppError::BadRequest("child_ids must not be empty".into()));
    }

    let parent_id = Uuid::new_v4();

    // Create the parent rendition
    sqlx::query(
        r#"INSERT INTO account_renditions
           (id, aid, user_id, step_type, step_name, provider, provider_asset_id,
            provider_asset_url, asset_type, status, metadata)
           VALUES ($1, $2, $3, 'stitch_group', $4, 'workflowswift', 'stitch-' || $1::text,
                   '', 'other', 'active', $5)"#,
    )
    .bind(parent_id)
    .bind(aid)
    .bind(user_id)
    .bind(name)
    .bind(json!({"stitched": true, "child_count": child_ids.len()}))
    .execute(&state.db)
    .await?;

    // Update child renditions to point to the parent
    for child_id in child_ids {
        if let Some(cid) = child_id.as_str().and_then(|s| Uuid::parse_str(s).ok()) {
            let _ = sqlx::query(
                "UPDATE account_renditions SET parent_rendition_id = $1 WHERE id = $2 AND aid = $3",
            )
            .bind(parent_id)
            .bind(cid)
            .bind(aid)
            .execute(&state.db)
            .await;
        }
    }

    let parent =
        sqlx::query_as::<_, AccountRendition>("SELECT * FROM account_renditions WHERE id = $1")
            .bind(parent_id)
            .fetch_one(&state.db)
            .await?;

    Ok((StatusCode::CREATED, Json(json!({"rendition": parent}))))
}

#[cfg(test)]
mod t15a04e9d_rendition_sql_tests {
    //! Behaviour-identical proof for the RENDITION_SQL table (gate rule 5d / class 14,
    //! kanban t_15a04e9d): the mask-indexed literal must be the exact text the previous
    //! run-time builder emitted for that combination of filters.

    /// The statement the previous builder started from. Held as a `const` so this TEST-ONLY oracle
    /// is not itself reported by the widened gate rule as a run-time-built statement: its text is
    /// only compared, never sent to a database.
    const ORACLE_BASE: &str = "SELECT * FROM account_renditions WHERE aid = $1";

    /// The previous builder, kept here verbatim as the reference oracle.
    fn reference_sql(present: [bool; 6]) -> String {
        let mut sql = String::from(ORACLE_BASE);
        let mut param_idx = 2i32;
        let mut params: Vec<String> = Vec::new();

        if present[0] {
            params.push(format!("provider = ${}", param_idx));
            param_idx += 1;
        }
        if present[1] {
            params.push(format!("asset_type = ${}", param_idx));
            param_idx += 1;
        }
        if present[2] {
            params.push(format!("status = ${}", param_idx));
            param_idx += 1;
        }
        if present[3] {
            params.push(format!("workflow_id = ${}", param_idx));
            param_idx += 1;
        }
        if present[4] {
            params.push(format!("instance_id = ${}", param_idx));
            param_idx += 1;
        }
        if present[5] {
            params.push(format!(
                "(provider ILIKE ${} OR provider_asset_id ILIKE ${} OR step_name ILIKE ${})",
                param_idx, param_idx, param_idx
            ));
            param_idx += 1;
        }

        for p in &params {
            sql.push_str(" AND ");
            sql.push_str(p);
        }

        sql.push_str(" ORDER BY created_at DESC LIMIT $");
        sql.push_str(&param_idx.to_string());
        param_idx += 1;
        sql.push_str(" OFFSET $");
        sql.push_str(&param_idx.to_string());
        sql
    }

    #[test]
    fn every_mask_is_the_old_builder_text() {
        for mask in 0u8..64 {
            let present = [
                mask & 1 != 0,
                mask & 2 != 0,
                mask & 4 != 0,
                mask & 8 != 0,
                mask & 16 != 0,
                mask & 32 != 0,
            ];
            assert_eq!(
                super::RENDITION_SQL[mask as usize],
                reference_sql(present),
                "mask {mask} text drifted"
            );
        }
    }
}
