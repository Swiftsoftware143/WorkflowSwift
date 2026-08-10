use axum::{
    extract::{Json, State},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::models::prospecting::*;
use crate::AppState;

pub async fn search_businesses(
    State(_state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let _aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let query = req
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if query.is_empty() {
        return Err(AppError::Validation("query is required".to_string()));
    }

    let limit = req
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .min(50);

    // Mock search results — in production this would call external APIs
    let results: Vec<BusinessSearchResult> = (0..limit as usize)
        .map(|i| BusinessSearchResult {
            name: format!("{} Business #{}", query, i + 1),
            website: Some(format!(
                "https://{}-business-{}.com",
                query.to_lowercase().replace(' ', "-"),
                i + 1
            )),
            industry: Some(
                vec![
                    "Technology",
                    "Healthcare",
                    "Finance",
                    "Retail",
                    "Manufacturing",
                ][i % 5]
                    .to_string(),
            ),
            location: Some(
                vec![
                    "New York, NY",
                    "San Francisco, CA",
                    "Austin, TX",
                    "Miami, FL",
                    "Chicago, IL",
                ][i % 5]
                    .to_string(),
            ),
            description: Some(format!(
                "A sample {} business found for query '{}'.",
                vec!["tech", "healthcare", "financial", "retail", "manufacturing"][i % 5],
                query
            )),
        })
        .collect();

    Ok(Json(json!({
        "results": results,
        "total": results.len(),
        "query": query,
    })))
}

pub async fn enrich_business(
    State(_state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let _aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let business_name = req
        .get("business_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if business_name.is_empty() {
        return Err(AppError::Validation(
            "business_name is required".to_string(),
        ));
    }

    let website = req
        .get("website")
        .and_then(|v| v.as_str())
        .map(String::from);

    // Mock enriched data — in production this would call enrichment APIs
    let enriched = EnrichedBusiness {
        business_name: business_name.clone(),
        website,
        industry: Some(vec!["Technology", "Healthcare", "Finance"][rand_number() % 3].to_string()),
        location: Some(
            vec!["New York, NY", "San Francisco, CA", "Austin, TX"][rand_number() % 3].to_string(),
        ),
        estimated_size: Some(
            vec![
                "1-10 employees",
                "11-50 employees",
                "51-200 employees",
                "201-1000 employees",
            ][rand_number() % 4]
                .to_string(),
        ),
        social_links: vec![
            format!(
                "https://linkedin.com/company/{}",
                business_name.to_lowercase().replace(' ', "-")
            ),
            format!(
                "https://twitter.com/{}",
                business_name.to_lowercase().replace(' ', "")
            ),
        ],
        contact_email: Some(format!(
            "contact@{}.com",
            business_name.to_lowercase().replace(' ', "")
        )),
        contact_phone: Some(format!("+1-555-{:04}", rand_number() % 10000)),
        insights: vec![
            format!(
                "{} appears to be in the growth stage with recent hiring activity.",
                business_name
            ),
            "Competitor analysis suggests 15-25% market overlap with your offerings.".to_string(),
            "Recommended outreach channel: LinkedIn direct messaging.".to_string(),
        ],
    };

    Ok(Json(json!({"enriched_business": enriched})))
}

fn rand_number() -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    nanos as usize
}
