use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Tag {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub color: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct TagAssignment {
    pub id: Uuid,
    pub tag_id: Uuid,
    pub entity_type: String,
    pub entity_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct AssignTagRequest {
    pub tag_id: Uuid,
    pub entity_type: String,
    pub entity_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct UnassignTagRequest {
    pub tag_id: Uuid,
    pub entity_type: String,
    pub entity_id: Uuid,
}
