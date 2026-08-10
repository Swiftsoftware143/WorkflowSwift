use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

/// Reference record for a rendered asset.
/// No files stored — just pointers to the third-party provider.
#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct AccountRendition {
    pub id: Uuid,
    pub aid: Uuid,
    pub user_id: Uuid,
    pub workflow_id: Option<Uuid>,
    pub instance_id: Option<Uuid>,
    pub step_type: String,
    pub step_name: Option<String>,
    pub provider: String,
    pub provider_asset_id: String,
    pub provider_asset_url: String,
    pub preview_url: Option<String>,
    pub thumbnail_url: Option<String>,
    pub asset_type: String,
    pub provider_category: Option<String>,
    pub sort_order: i32,
    pub parent_rendition_id: Option<Uuid>,
    pub retention_expires_at: Option<DateTime<Utc>>,
    pub status: String,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Create a rendition record
#[derive(Debug, Deserialize)]
pub struct CreateRenditionRequest {
    pub workflow_id: Option<Uuid>,
    pub instance_id: Option<Uuid>,
    pub step_name: Option<String>,
    pub provider: String,
    pub provider_asset_id: String,
    pub provider_asset_url: String,
    pub preview_url: Option<String>,
    pub thumbnail_url: Option<String>,
    pub asset_type: String,
    pub sort_order: Option<i32>,
    pub parent_rendition_id: Option<Uuid>,
    pub retention_days: Option<i32>,
    pub metadata: Option<Value>,
}

/// List query params
#[derive(Debug, Deserialize)]
pub struct ListRenditionsQuery {
    pub provider: Option<String>,
    pub asset_type: Option<String>,
    pub workflow_id: Option<Uuid>,
    pub instance_id: Option<Uuid>,
    pub status: Option<String>,
    pub search: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Update request
#[derive(Debug, Deserialize)]
pub struct UpdateRenditionRequest {
    pub preview_url: Option<String>,
    pub thumbnail_url: Option<String>,
    pub status: Option<String>,
    pub retention_expires_at: Option<DateTime<Utc>>,
    pub sort_order: Option<i32>,
    pub metadata: Option<Value>,
}
