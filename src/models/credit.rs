use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CreditPackage {
    pub id: Uuid,
    pub name: String,
    pub credits: i32,
    pub price: serde_json::Value,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CreditTransaction {
    pub id: Uuid,
    pub aid: Uuid,
    pub amount: i32,
    pub transaction_type: String,
    pub description: Option<String>,
    pub reference_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditBalance {
    pub aid: Uuid,
    pub balance: i64,
}
