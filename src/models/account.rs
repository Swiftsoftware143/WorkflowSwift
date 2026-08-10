use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Account {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub logo_url: Option<String>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub custom_domain: Option<String>,
    pub branding_name: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub footer_year: Option<String>,
    #[serde(default)]
    pub footer_company: Option<String>,
    #[serde(default)]
    pub hexomatic_key: Option<String>,
    #[serde(default)]
    pub industry_slug: Option<String>,
}
