//! Industry Data Sources Handler — Admin configures data sources per industry
//! Each source powers specific widgets. Sources cost credits per API call.

use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::{error::AppError, AppState};

#[derive(Deserialize)]
pub struct CreateSourceRequest {
    pub industry_slug: String,
    pub source_name: String,
    pub source_type: Option<String>,
    pub endpoint: Option<String>,
    pub refresh_cadence: Option<String>,
    pub credit_cost: Option<i32>,
    pub config: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub struct WidgetMappingRequest {
    pub industry_slug: String,
    pub widget_key: String,
    pub source_id: Uuid,
    pub display_order: Option<i32>,
}

/// GET /api/v1/admin/industry-sources — list all data sources (opt. ?industry=)
pub async fn list_industry_sources(
    State(s): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Query(q): Query<serde_json::Value>,
) -> Result<impl IntoResponse, AppError> {
    let industry = q.get("industry").and_then(|v| v.as_str());

    let sources = if let Some(ind) = industry {
        sqlx::query_as::<_, (Uuid, String, String, String, Option<String>, String, i32, bool)>(
            "SELECT id, industry_slug, source_name, source_type, endpoint, refresh_cadence, credit_cost, is_active
             FROM industry_data_sources WHERE industry_slug = $1 ORDER BY source_name"
        ).bind(ind).fetch_all(&s.db).await?
    } else {
        sqlx::query_as::<_, (Uuid, String, String, String, Option<String>, String, i32, bool)>(
            "SELECT id, industry_slug, source_name, source_type, endpoint, refresh_cadence, credit_cost, is_active
             FROM industry_data_sources ORDER BY industry_slug, source_name"
        ).fetch_all(&s.db).await?
    };

    let result: Vec<serde_json::Value> = sources
        .into_iter()
        .map(|s| {
            json!({
                "id": s.0, "industry": s.1, "name": s.2, "type": s.3,
                "endpoint": s.4, "cadence": s.5, "credit_cost": s.6, "active": s.7
            })
        })
        .collect();

    Ok(Json(json!({"sources": result})))
}

/// POST /api/v1/admin/industry-sources — create or update a data source
pub async fn upsert_industry_source(
    State(s): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Json(req): Json<CreateSourceRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _ =     sqlx::query(
        "INSERT INTO industry_data_sources (id, industry_slug, source_name, source_type, endpoint, refresh_cadence, credit_cost, config)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
         ON CONFLICT (industry_slug, source_name) DO UPDATE SET
           source_type = $4, endpoint = $5, refresh_cadence = $6, credit_cost = $7, config = $8, updated_at = NOW()"
    )
    .bind(Uuid::new_v4())
    .bind(&req.industry_slug)
    .bind(&req.source_name)
    .bind(req.source_type.as_deref().unwrap_or("api"))
    .bind(&req.endpoint)
    .bind(req.refresh_cadence.as_deref().unwrap_or("daily"))
    .bind(req.credit_cost.unwrap_or(1))
    .bind(&req.config)
    .execute(&s.db).await?;

    Ok(Json(json!({"status": "saved"})))
}

/// POST /api/v1/admin/industry-sources/seed — seed default data sources for all industries
pub async fn seed_industry_sources(
    State(s): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> Result<impl IntoResponse, AppError> {
    let industries: Vec<(String,)> =
        sqlx::query_as("SELECT slug FROM template_categories WHERE is_active = true")
            .fetch_all(&s.db)
            .await?;

    let mut count = 0;
    for (slug,) in &industries {
        let common_sources = vec![
            (
                "market_research",
                "api",
                "/api/v1/satellite/market-research",
                "daily",
                2,
            ),
            ("news_feed", "rss", "/api/v1/satellite/news", "hourly", 1),
            (
                "competitor_intel",
                "api",
                "/api/v1/satellite/competitors",
                "daily",
                3,
            ),
            ("lead_finder", "api", "/api/v1/satellite/leads", "daily", 5),
            (
                "trend_analytics",
                "api",
                "/api/v1/satellite/trends",
                "weekly",
                2,
            ),
        ];

        for (name, stype, endpoint, cadence, cost) in &common_sources {
            sqlx::query(
                "INSERT INTO industry_data_sources (id, industry_slug, source_name, source_type, endpoint, refresh_cadence, credit_cost)
                 VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (industry_slug, source_name) DO NOTHING"
            )
            .bind(Uuid::new_v4()).bind(slug).bind(name).bind(*stype).bind(*endpoint).bind(*cadence).bind(*cost)
            .execute(&s.db).await;
            let _ = true;
        }
        count += 1;
    }

    Ok(Json(
        json!({"status": "seeded", "industries_updated": count}),
    ))
}

/// DELETE /api/v1/admin/industry-sources/:id — remove a data source
pub async fn delete_industry_source(
    State(s): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    sqlx::query("DELETE FROM industry_data_sources WHERE id = $1")
        .bind(id)
        .execute(&s.db)
        .await?;
    Ok(Json(json!({"status": "deleted"})))
}
