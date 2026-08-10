use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PlanTier {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub price_monthly: Option<String>,
    pub price_yearly: Option<String>,
    pub features: Option<serde_json::Value>,
    pub checkout_url: Option<String>,
    pub is_active: bool,
    pub sort_order: Option<i32>,
    pub payment_provider: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AccountPlan {
    pub id: Uuid,
    pub aid: Uuid,
    pub plan_id: Uuid,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Invoice {
    pub id: Uuid,
    pub aid: Uuid,
    pub plan_id: Uuid,
    pub amount: serde_json::Value,
    pub status: String,
    pub due_date: Option<DateTime<Utc>>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
