//! Feature limits enforcement for WorkflowSwift.

use crate::error::AppError;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct FeatureLimitResult {
    pub allowed: bool,
    pub limit: i32,
    pub usage: i64,
    pub feature_key: String,
}

pub async fn check_feature_limit(
    db: &PgPool,
    tenant_id: Uuid,
    feature_key: &str,
) -> Result<FeatureLimitResult, AppError> {
    let plan_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT plan_id FROM tenant_plans WHERE tenant_id = $1 AND status = 'active'"
    )
    .bind(tenant_id)
    .fetch_optional(db)
    .await?
    .flatten();

    let plan_id = match plan_id {
        Some(id) => id,
        None => return Ok(FeatureLimitResult { allowed: false, limit: 0, usage: 0, feature_key: feature_key.to_string() }),
    };

    let limit_value: Option<i32> = sqlx::query_scalar(
        "SELECT limit_value FROM feature_limits WHERE plan_id = $1 AND feature_key = $2"
    )
    .bind(plan_id)
    .bind(feature_key)
    .fetch_optional(db)
    .await?
    .flatten();

    let limit_value = match limit_value {
        Some(v) => v,
        None => return Ok(FeatureLimitResult { allowed: false, limit: 0, usage: 0, feature_key: feature_key.to_string() }),
    };

    if limit_value == -1 {
        return Ok(FeatureLimitResult { allowed: true, limit: -1, usage: 0, feature_key: feature_key.to_string() });
    }

    let usage: i64 = match feature_key {
        "max_workflows" => sqlx::query_scalar("SELECT COUNT(*) FROM workflows WHERE tenant_id = $1").bind(tenant_id).fetch_one(db).await?,
        "max_templates" => sqlx::query_scalar("SELECT COUNT(*) FROM workflow_templates WHERE tenant_id = $1").bind(tenant_id).fetch_one(db).await?,
        "max_instances" => sqlx::query_scalar("SELECT COUNT(*) FROM workflow_instances WHERE tenant_id = $1").bind(tenant_id).fetch_one(db).await?,
        "max_users" => sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE tenant_id = $1 AND is_active = true").bind(tenant_id).fetch_one(db).await?,
        "max_automations" => sqlx::query_scalar("SELECT COUNT(*) FROM automations WHERE tenant_id = $1").bind(tenant_id).fetch_one(db).await?,
        "max_integrations" => sqlx::query_scalar("SELECT COUNT(*) FROM integration_targets WHERE tenant_id = $1").bind(tenant_id).fetch_one(db).await?,
        "max_api_keys" => sqlx::query_scalar("SELECT COUNT(*) FROM api_keys WHERE tenant_id = $1").bind(tenant_id).fetch_one(db).await?,
        "max_clients" => sqlx::query_scalar("SELECT COUNT(*) FROM clients WHERE tenant_id = $1 AND is_active = true").bind(tenant_id).fetch_one(db).await?,
        "max_portfolio" => sqlx::query_scalar("SELECT COUNT(*) FROM portfolio_companies WHERE tenant_id = $1").bind(tenant_id).fetch_one(db).await?,
        "max_tags" => sqlx::query_scalar("SELECT COUNT(*) FROM tags WHERE tenant_id = $1").bind(tenant_id).fetch_one(db).await?,
        "max_industries" => {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM tenant_industries WHERE tenant_id = $1 AND is_active = true"
            )
            .bind(tenant_id).fetch_one(db).await?;
            if count == 0 { 1 } else { count }
        },
        _ => 0i64,
    };

    Ok(FeatureLimitResult {
        allowed: usage < limit_value as i64,
        limit: limit_value,
        usage,
        feature_key: feature_key.to_string(),
    })
}

pub async fn enforce_feature_limit(db: &PgPool, tenant_id: Uuid, feature_key: &str, label: &str) -> Result<(), AppError> {
    let result = check_feature_limit(db, tenant_id, feature_key).await?;
    if !result.allowed {
        let msg = if result.limit == 0 {
            format!("{} is not available on your current plan. Upgrade to access this feature.", label)
        } else {
            format!("{} limit reached ({}/{})", label, result.usage, result.limit)
        };
        return Err(AppError::BadRequest(msg));
    }
    Ok(())
}
