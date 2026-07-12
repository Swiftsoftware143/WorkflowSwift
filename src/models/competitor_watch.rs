use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Competitor {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub website: Option<String>,
    pub description: Option<String>,
    pub strengths: Option<Vec<String>>,
    pub weaknesses: Option<Vec<String>>,
    pub market_share: Option<f64>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitorCheckResult {
    pub competitor_id: Uuid,
    pub name: String,
    pub status: String,
    pub changes_detected: bool,
    pub summary: String,
    pub checked_at: DateTime<Utc>,
}
