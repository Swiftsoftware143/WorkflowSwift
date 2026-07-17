use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WorkflowExecutionLog {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub workflow_id: Uuid,
    pub step_id: Option<Uuid>,
    pub step_type: String,
    pub step_name: String,
    pub sort_order: i32,
    pub status: String,
    pub provider: Option<String>,
    pub input_data: Option<serde_json::Value>,
    pub output_data: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub duration_ms: Option<i32>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WorkflowTriggerQueue {
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub aid: Uuid,
    pub trigger_type: String,
    pub trigger_source: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub status: String,
    pub client_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
}
