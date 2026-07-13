use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Automation {
    pub id: Uuid,
    pub aid: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub trigger_type: String,
    pub trigger_config: Option<serde_json::Value>,
    pub action_type: String,
    pub action_config: Option<serde_json::Value>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AutomationRun {
    pub id: Uuid,
    pub automation_id: Uuid,
    pub status: String,
    pub trigger_data: Option<serde_json::Value>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
