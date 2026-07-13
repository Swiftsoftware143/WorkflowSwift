use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct BrandMonitor {
    pub id: Uuid,
    pub aid: Uuid,
    pub brand_name: String,
    pub keywords: Option<Vec<String>>,
    pub platforms: Vec<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_scanned_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrandMention {
    pub platform: String,
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub sentiment: String,
    pub relevance_score: f64,
}

#[derive(Debug, Deserialize)]
pub struct SearchMentionsRequest {
    pub brand_monitor_id: Uuid,
    pub max_results: Option<i32>,
}
