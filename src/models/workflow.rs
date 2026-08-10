use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Workflow {
    pub id: Uuid,
    pub aid: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub lifecycle_summary: Option<String>,
    pub tags: Option<serde_json::Value>,
    pub is_active: bool,
    pub surface_id: Option<Uuid>,
    pub trigger_type: Option<String>,
    pub trigger_config: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WorkflowStep {
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub step_type: String,
    pub name: String,
    pub description: Option<String>,
    pub sort_order: i32,
    pub config: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkflowRequest {
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub lifecycle_summary: Option<String>,
    pub tags: Option<serde_json::Value>,
    pub surface_id: Option<Uuid>,
    pub trigger_type: Option<String>,
    pub trigger_config: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkflowRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub lifecycle_summary: Option<String>,
    pub tags: Option<serde_json::Value>,
    pub trigger_type: Option<String>,
    pub trigger_config: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkflowStepRequest {
    pub step_type: String,
    pub name: String,
    pub description: Option<String>,
    pub config: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkflowStepRequest {
    pub step_type: String,
    pub name: String,
    pub description: Option<String>,
    pub sort_order: Option<i32>,
    pub config: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct ReorderStepsRequest {
    pub step_ids: Vec<Uuid>,
}
