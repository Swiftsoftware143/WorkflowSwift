// WorkflowSwift Dashboard Tabs Handler
// Brand Monitor, Competitor Watch, and Prospecting
// July 3, 2026

use axum::{
    extract::{Json as AxumJson, Path, State},
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::AppState;

// ─── Brand Monitor ───

#[derive(Debug, Deserialize)]
pub struct CreateBrandMonitorRequest {
    pub brand_name: String,
    pub keywords: Option<Vec<String>>,
    pub sources: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct BrandMonitorItem {
    pub id: String,
    pub brand_name: String,
    pub keywords: Vec<String>,
    pub sources: Vec<String>,
    pub is_active: bool,
    pub created_at: String,
    pub result_count: i64,
}

/// POST /api/v1/dashboard/brand-monitor — Start tracking a brand
pub async fn create_brand_monitor(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumJson(req): AxumJson<CreateBrandMonitorRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let id = Uuid::new_v4();

    sqlx::query(
        r#"INSERT INTO brand_monitor_items (id, aid, brand_name, keywords, sources)
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(id)
    .bind(aid)
    .bind(&req.brand_name)
    .bind(req.keywords.unwrap_or_default())
    .bind(
        req.sources
            .unwrap_or_else(|| vec!["web".into(), "news".into(), "social".into()]),
    )
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "id": id.to_string(),
        "brand_name": req.brand_name,
        "message": "Brand monitor created. Data will populate on next fetch cycle."
    })))
}

/// GET /api/v1/dashboard/brand-monitor — List tracked brands with results
pub async fn list_brand_monitors(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        r#"SELECT b.id::text, b.brand_name, b.keywords, b.sources, b.is_active, b.created_at::text,
                  (SELECT COUNT(*) FROM brand_monitor_results r WHERE r.brand_item_id = b.id) as result_count
           FROM brand_monitor_items b
           WHERE b.aid = $1
           ORDER BY b.created_at DESC"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<&str, _>("id").unwrap_or(""),
                "brand_name": r.try_get::<String, _>("brand_name").unwrap_or_default(),
                "keywords": r.try_get::<Vec<String>, _>("keywords").unwrap_or_default(),
                "sources": r.try_get::<Vec<String>, _>("sources").unwrap_or_default(),
                "is_active": r.try_get::<bool, _>("is_active").unwrap_or(false),
                "created_at": r.try_get::<&str, _>("created_at").unwrap_or(""),
                "result_count": r.try_get::<i64, _>("result_count").unwrap_or(0),
            })
        })
        .collect();

    Ok(Json(json!({"brands": items})))
}

/// GET /api/v1/dashboard/brand-monitor/{id}/results — Get mentions for a brand
pub async fn get_brand_monitor_results(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        r#"SELECT id::text, source, source_url, title, snippet, sentiment, published_at::text, fetched_at::text
           FROM brand_monitor_results
           WHERE brand_item_id = $1 AND aid = $2
           ORDER BY fetched_at DESC
           LIMIT 100"#,
    )
    .bind(id)
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let results: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<&str, _>("id").unwrap_or(""),
                "source": r.try_get::<&str, _>("source").unwrap_or(""),
                "source_url": r.try_get::<&str, _>("source_url").unwrap_or(""),
                "title": r.try_get::<&str, _>("title").unwrap_or(""),
                "snippet": r.try_get::<&str, _>("snippet").unwrap_or(""),
                "sentiment": r.try_get::<&str, _>("sentiment").unwrap_or(""),
                "published_at": r.try_get::<&str, _>("published_at").unwrap_or(""),
                "fetched_at": r.try_get::<&str, _>("fetched_at").unwrap_or(""),
            })
        })
        .collect();

    Ok(Json(json!({"results": results})))
}

/// DELETE /api/v1/dashboard/brand-monitor/{id} — Stop tracking a brand
pub async fn delete_brand_monitor(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM brand_monitor_items WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Brand monitor not found".into()));
    }

    Ok(Json(json!({"deleted": true})))
}

// ─── Competitor Watch ───

#[derive(Debug, Deserialize)]
pub struct CreateCompetitorWatchRequest {
    pub competitor_name: String,
    pub competitor_website: Option<String>,
    pub competitor_social: Option<serde_json::Value>,
    pub watch_focus: Option<Vec<String>>,
}

/// POST /api/v1/dashboard/competitor-watch — Start watching a competitor
pub async fn create_competitor_watch(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumJson(req): AxumJson<CreateCompetitorWatchRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let id = Uuid::new_v4();

    sqlx::query(
        r#"INSERT INTO competitor_watch_items (id, aid, competitor_name, competitor_website, competitor_social, watch_focus)
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(id)
    .bind(aid)
    .bind(&req.competitor_name)
    .bind(&req.competitor_website)
    .bind(req.competitor_social.unwrap_or(json!({})))
    .bind(req.watch_focus.unwrap_or_else(|| vec!["pricing".into(), "content".into(), "reviews".into(), "activity".into()]))
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "id": id.to_string(),
        "competitor_name": req.competitor_name,
        "message": "Competitor watch created. Changes will be tracked on next cycle."
    })))
}

/// GET /api/v1/dashboard/competitor-watch — List watched competitors
pub async fn list_competitor_watches(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        r#"SELECT c.id::text, c.competitor_name, c.competitor_website, c.competitor_social,
                  c.watch_focus, c.is_active, c.created_at::text,
                  (SELECT COUNT(*) FROM competitor_watch_results r WHERE r.competitor_id = c.id) as change_count
           FROM competitor_watch_items c
           WHERE c.aid = $1
           ORDER BY c.created_at DESC"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let items: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<&str, _>("id").unwrap_or(""),
            "competitor_name": r.try_get::<String, _>("competitor_name").unwrap_or_default(),
            "competitor_website": r.try_get::<Option<String>, _>("competitor_website").unwrap_or(None),
            "competitor_social": r.try_get::<serde_json::Value, _>("competitor_social").unwrap_or(json!({})),
            "watch_focus": r.try_get::<Vec<String>, _>("watch_focus").unwrap_or_default(),
            "is_active": r.try_get::<bool, _>("is_active").unwrap_or(false),
            "created_at": r.try_get::<&str, _>("created_at").unwrap_or(""),
            "change_count": r.try_get::<i64, _>("change_count").unwrap_or(0),
        })
    }).collect();

    Ok(Json(json!({"competitors": items})))
}

/// GET /api/v1/dashboard/competitor-watch/{id}/changes — Get detected changes
pub async fn get_competitor_changes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        r#"SELECT id::text, change_type, description, source_url, detected_at::text, alert_sent
           FROM competitor_watch_results
           WHERE competitor_id = $1 AND aid = $2
           ORDER BY detected_at DESC
           LIMIT 100"#,
    )
    .bind(id)
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let changes: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<&str, _>("id").unwrap_or(""),
                "change_type": r.try_get::<&str, _>("change_type").unwrap_or(""),
                "description": r.try_get::<&str, _>("description").unwrap_or(""),
                "source_url": r.try_get::<&str, _>("source_url").unwrap_or(""),
                "detected_at": r.try_get::<&str, _>("detected_at").unwrap_or(""),
                "alert_sent": r.try_get::<bool, _>("alert_sent").unwrap_or(false),
            })
        })
        .collect();

    Ok(Json(json!({"changes": changes})))
}

/// DELETE /api/v1/dashboard/competitor-watch/{id} — Stop watching a competitor
pub async fn delete_competitor_watch(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM competitor_watch_items WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Competitor watch not found".into()));
    }

    Ok(Json(json!({"deleted": true})))
}

// ─── Prospecting ───

#[derive(Debug, Deserialize)]
pub struct CreateProspectingRequest {
    pub industry: String,
    pub city: String,
    pub state: String,
}

#[derive(Debug, Serialize)]
pub struct ProspectingItem {
    pub id: String,
    pub industry: String,
    pub city: String,
    pub state: String,
    pub search_query: String,
    pub is_active: bool,
    pub created_at: String,
    pub result_count: i64,
}

/// POST /api/v1/dashboard/prospecting — Create a prospect search
pub async fn create_prospecting(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumJson(req): AxumJson<CreateProspectingRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let id = Uuid::new_v4();

    sqlx::query(
        r#"INSERT INTO prospecting_items (id, aid, industry, city, state)
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(id)
    .bind(aid)
    .bind(&req.industry)
    .bind(&req.city)
    .bind(&req.state)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "id": id.to_string(),
        "search_query": format!("{} in {}, {}", req.industry, req.city, req.state),
        "message": "Prospecting search created. Results will populate from connected data sources."
    })))
}

/// GET /api/v1/dashboard/prospecting — List prospect searches
pub async fn list_prospectings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        r#"SELECT p.id::text, p.industry, p.city, p.state, p.search_query, p.is_active, p.created_at::text,
                  (SELECT COUNT(*) FROM prospecting_results r WHERE r.prospecting_id = p.id) as result_count
           FROM prospecting_items p
           WHERE p.aid = $1
           ORDER BY p.created_at DESC"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<&str, _>("id").unwrap_or(""),
                "industry": r.try_get::<String, _>("industry").unwrap_or_default(),
                "city": r.try_get::<String, _>("city").unwrap_or_default(),
                "state": r.try_get::<String, _>("state").unwrap_or_default(),
                "search_query": r.try_get::<String, _>("search_query").unwrap_or_default(),
                "is_active": r.try_get::<bool, _>("is_active").unwrap_or(false),
                "created_at": r.try_get::<&str, _>("created_at").unwrap_or(""),
                "result_count": r.try_get::<i64, _>("result_count").unwrap_or(0),
            })
        })
        .collect();

    Ok(Json(json!({"searches": items})))
}

/// GET /api/v1/dashboard/prospecting/{id}/results — Get prospect results
pub async fn get_prospecting_results(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        r#"SELECT id::text, business_name, business_website, business_phone, business_email,
                  business_address, source, source_url, social_links, rating, review_count, fetched_at::text
           FROM prospecting_results
           WHERE prospecting_id = $1 AND aid = $2
           ORDER BY rating DESC NULLS LAST, business_name ASC
           LIMIT 200"#,
    )
    .bind(id)
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let results: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<&str, _>("id").unwrap_or(""),
            "business_name": r.try_get::<String, _>("business_name").unwrap_or_default(),
            "business_website": r.try_get::<Option<String>, _>("business_website").unwrap_or(None),
            "business_phone": r.try_get::<Option<String>, _>("business_phone").unwrap_or(None),
            "business_email": r.try_get::<Option<String>, _>("business_email").unwrap_or(None),
            "business_address": r.try_get::<Option<String>, _>("business_address").unwrap_or(None),
            "source": r.try_get::<&str, _>("source").unwrap_or(""),
            "source_url": r.try_get::<&str, _>("source_url").unwrap_or(""),
            "social_links": r.try_get::<serde_json::Value, _>("social_links").unwrap_or(json!({})),
            "rating": r.try_get::<Option<f64>, _>("rating").unwrap_or(None),
            "review_count": r.try_get::<Option<i32>, _>("review_count").unwrap_or(None),
            "fetched_at": r.try_get::<&str, _>("fetched_at").unwrap_or(""),
        })
    }).collect();

    Ok(Json(json!({"results": results})))
}

/// DELETE /api/v1/dashboard/prospecting/{id} — Remove a prospect search
pub async fn delete_prospecting(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM prospecting_items WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Prospecting search not found".into()));
    }

    Ok(Json(json!({"deleted": true})))
}

// ─── Dashboard Tab Config ───

/// GET /api/v1/dashboard/tabs — Get tab configuration
pub async fn get_dashboard_tabs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Ensure default tabs exist
    let default_tabs = [
        ("brand_monitor", "Brand Monitor"),
        ("competitor_watch", "Competitor Watch"),
        ("prospecting", "Prospecting"),
    ];

    for (i, (tab_type, label)) in default_tabs.iter().enumerate() {
        sqlx::query(
            r#"INSERT INTO dashboard_tab_config (id, aid, tab_type, tab_label, sort_order, is_visible)
               VALUES ($1, $2, $3, $4, $5, true)
               ON CONFLICT (aid, tab_type) DO NOTHING"#,
        )
        .bind(Uuid::new_v4())
        .bind(aid)
        .bind(tab_type)
        .bind(label)
        .bind(i as i32)
        .execute(&state.db)
        .await
        .ok();
    }

    let rows = sqlx::query(
        r#"SELECT id::text, tab_type, tab_label, sort_order, is_visible
           FROM dashboard_tab_config
           WHERE aid = $1
           ORDER BY sort_order"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let tabs: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<&str, _>("id").unwrap_or(""),
                "tab_type": r.try_get::<&str, _>("tab_type").unwrap_or(""),
                "tab_label": r.try_get::<&str, _>("tab_label").unwrap_or(""),
                "sort_order": r.try_get::<i32, _>("sort_order").unwrap_or(0),
                "is_visible": r.try_get::<bool, _>("is_visible").unwrap_or(false),
            })
        })
        .collect();

    Ok(Json(json!({"tabs": tabs})))
}

// ─── Connect dashboard data to workflow ───

#[derive(Debug, Deserialize)]
pub struct ConnectToWorkflowRequest {
    pub dashboard_tab_type: String,
    pub source_item_id: String,
    pub source_table: String,
    pub workflow_id: String,
    pub trigger_on: Option<String>,
}

/// POST /api/v1/dashboard/connect-workflow — Link dashboard data to a workflow
pub async fn connect_to_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumJson(req): AxumJson<ConnectToWorkflowRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let id = Uuid::new_v4();
    let source_id = Uuid::parse_str(&req.source_item_id)
        .map_err(|_| AppError::BadRequest("Invalid source_item_id".into()))?;
    let workflow_id = Uuid::parse_str(&req.workflow_id)
        .map_err(|_| AppError::BadRequest("Invalid workflow_id".into()))?;

    sqlx::query(
        r#"INSERT INTO dashboard_workflow_links (id, aid, dashboard_tab_type, source_item_id, source_table, workflow_id, trigger_on)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
    )
    .bind(id)
    .bind(aid)
    .bind(&req.dashboard_tab_type)
    .bind(source_id)
    .bind(&req.source_table)
    .bind(workflow_id)
    .bind(req.trigger_on.unwrap_or_else(|| "new_result".to_string()))
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "id": id.to_string(),
        "message": "Dashboard data connected to workflow. New results will trigger execution."
    })))
}
