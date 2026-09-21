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
    /// `plan_tiers.price_monthly` / `price_yearly` are NUMERIC(10,2) and sqlx has no
    /// String or f64 decode for numeric, so every read casts the column to text
    /// (`price_monthly::text as price_monthly`) and it lands here as a decimal string.
    /// A query that returns these columns WITHOUT the cast 500s (kanban t_3d0c5623).
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
    /// `invoices.amount` is NUMERIC(10,2) (migrations/010_create_plans.sql). sqlx cannot decode
    /// NUMERIC into `serde_json::Value` — it is not a Postgres type at all — so the read paths in
    /// invoice_handler cast the column to text (`amount::text`) and it lands here as a decimal
    /// string, the same convention `plan_tiers.price_monthly` already uses (kanban t_01fa9bbc).
    pub amount: String,
    pub status: String,
    pub due_date: Option<DateTime<Utc>>,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
