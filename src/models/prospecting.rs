use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProspectingLead {
    pub id: Uuid,
    pub aid: Uuid,
    pub business_name: String,
    pub website: Option<String>,
    pub industry: Option<String>,
    pub location: Option<String>,
    pub estimated_size: Option<String>,
    pub social_links: Option<Vec<String>>,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    pub enriched: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusinessSearchResult {
    pub name: String,
    pub website: Option<String>,
    pub industry: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedBusiness {
    pub business_name: String,
    pub website: Option<String>,
    pub industry: Option<String>,
    pub location: Option<String>,
    pub estimated_size: Option<String>,
    pub social_links: Vec<String>,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    pub insights: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchBusinessesRequest {
    pub query: String,
    pub limit: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct EnrichBusinessRequest {
    pub business_name: String,
    pub website: Option<String>,
}
