use axum::{
    extract::{State, Json, Path, Query},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use std::collections::HashMap;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::features;
use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::models::template::*;

#[derive(Debug, serde::Deserialize)]
pub struct ListTemplatesQuery {
    pub industry: Option<String>,
    pub surface: Option<Uuid>,
}

pub async fn list_templates(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListTemplatesQuery>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Determine which templates this tenant's plan allows
    let plan_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT plan_id FROM tenant_plans WHERE tenant_id = $1 AND status = 'active'"
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .flatten();

    // If a specific industry is requested, also check user has access to it
    if let Some(ref industry_slug) = query.industry {
        let has_industry: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM tenant_industries WHERE tenant_id = $1 AND industry_slug = $2 AND is_active = true)"
        )
        .bind(tenant_id)
        .bind(industry_slug)
        .fetch_one(&state.db)
        .await
        .unwrap_or(false);

        // If tenant doesn't own this industry, fall back to public templates
        if !has_industry {
            // Allow view but only show public templates for that industry
            let templates = if let Some(surface_id) = query.surface {
                sqlx::query_as::<_, WorkflowTemplate>(
                    r#"SELECT wt.* FROM workflow_templates wt
                       INNER JOIN industry_templates it ON it.template_id = wt.id
                       WHERE it.industry_slug = $1 AND wt.is_public = true
                       AND (wt.surface_id = $2 OR wt.surface_id IS NULL)
                       ORDER BY wt.name ASC"#
                )
                .bind(industry_slug)
                .bind(surface_id)
                .fetch_all(&state.db)
                .await?
            } else {
                sqlx::query_as::<_, WorkflowTemplate>(
                    r#"SELECT wt.* FROM workflow_templates wt
                       INNER JOIN industry_templates it ON it.template_id = wt.id
                       WHERE it.industry_slug = $1 AND wt.is_public = true
                       ORDER BY wt.name ASC"#
                )
                .bind(industry_slug)
                .fetch_all(&state.db)
                .await?
            };
            return Ok(Json(json!({"templates": templates})));
        }
    }

    // For tenant-owned templates, combine with plan capabilities
    // Plan capabilities control which public templates are visible/usable
    let templates = if let Some(ref industry_slug) = query.industry {
        if let Some(surface_id) = query.surface {
            sqlx::query_as::<_, WorkflowTemplate>(
                r#"SELECT wt.* FROM workflow_templates wt
                   INNER JOIN industry_templates it ON it.template_id = wt.id
                   WHERE wt.tenant_id = $1 AND it.industry_slug = $2
                   AND (wt.surface_id = $3 OR wt.surface_id IS NULL)
                   ORDER BY wt.name ASC"#
            )
            .bind(tenant_id)
            .bind(industry_slug)
            .bind(surface_id)
            .fetch_all(&state.db)
            .await?
        } else {
            sqlx::query_as::<_, WorkflowTemplate>(
                r#"SELECT wt.* FROM workflow_templates wt
                   INNER JOIN industry_templates it ON it.template_id = wt.id
                   WHERE wt.tenant_id = $1 AND it.industry_slug = $2
                   ORDER BY wt.name ASC"#
            )
            .bind(tenant_id)
            .bind(industry_slug)
            .fetch_all(&state.db)
            .await?
        }
    } else if let Some(surface_id) = query.surface {
        sqlx::query_as::<_, WorkflowTemplate>(
            "SELECT * FROM workflow_templates WHERE tenant_id = $1 AND (surface_id = $2 OR surface_id IS NULL) ORDER BY name ASC",
        )
        .bind(tenant_id)
        .bind(surface_id)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, WorkflowTemplate>(
            "SELECT * FROM workflow_templates WHERE tenant_id = $1 ORDER BY name ASC",
        )
        .bind(tenant_id)
        .fetch_all(&state.db)
        .await?
    };

    // Also attach what public templates are available based on the user's plan + industries
    let available_public: Vec<serde_json::Value> = if let Some(pid) = plan_id {
        sqlx::query(
            r#"SELECT wt.id::text, wt.name, wt.description, wt.category, tc.name as industry_name
               FROM v_plan_industry_templates vpit
               JOIN workflow_templates wt ON wt.id = vpit.template_id
               JOIN template_categories tc ON tc.slug = vpit.industry_slug
               WHERE vpit.plan_id = $1
                  AND (wt.tenant_id = $2 OR wt.is_public = true)
               ORDER BY wt.name"#
        )
        .bind(pid)
        .bind(tenant_id)
        .fetch_all(&state.db)
        .await?
        .iter()
        .map(|row| {
            json!({
                "id": row.try_get::<String, _>("id").unwrap_or_default(),
                "name": row.try_get::<String, _>("name").unwrap_or_default(),
                "description": row.try_get::<Option<String>, _>("description").ok().flatten(),
                "category": row.try_get::<String, _>("category").unwrap_or_default(),
                "industry_name": row.try_get::<String, _>("industry_name").unwrap_or_default(),
            })
        })
        .collect()
    } else {
        vec![]
    };

    Ok(Json(json!({
        "templates": templates,
        "available_public": available_public
    })))
}

pub async fn create_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateTemplateRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, tenant_id, "max_templates", "Templates").await?;

    let template_id = Uuid::new_v4();
    let template = sqlx::query_as::<_, WorkflowTemplate>(
        r#"INSERT INTO workflow_templates (id, tenant_id, name, description, category, tags, is_public)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           RETURNING *"#,
    )
    .bind(template_id)
    .bind(tenant_id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.category)
    .bind(&req.tags)
    .bind(false)
    .fetch_one(&state.db)
    .await?;

    // Insert template steps
    for step in &req.steps {
        sqlx::query(
            r#"INSERT INTO workflow_template_steps (id, template_id, step_type, name, description, sort_order, config)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind(Uuid::new_v4())
        .bind(template_id)
        .bind(&step.step_type)
        .bind(&step.name)
        .bind(&step.description)
        .bind(step.sort_order)
        .bind(&step.config)
        .execute(&state.db)
        .await?;
    }

    Ok((StatusCode::CREATED, Json(json!({"template": template}))))
}

pub async fn get_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let template = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Template not found".to_string()))?;

    let steps = sqlx::query_as::<_, WorkflowTemplateStep>(
        "SELECT * FROM workflow_template_steps WHERE template_id = $1 ORDER BY sort_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"template": template, "steps": steps})))
}

pub async fn update_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Template not found".to_string()))?;

    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or(&existing.name).to_string();
    let description = req.get("description").and_then(|v| v.as_str()).map(|s| s.to_string()).or(existing.description);
    let category = req.get("category").and_then(|v| v.as_str()).unwrap_or(&existing.category).to_string();
    let tags = req.get("tags").cloned().or(existing.tags);

    sqlx::query(
        r#"UPDATE workflow_templates SET name=$1, description=$2, category=$3, tags=$4 WHERE id=$5"#,
    )
    .bind(&name)
    .bind(&description)
    .bind(&category)
    .bind(&tags)
    .bind(id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({"message": "Template updated"})))
}

pub async fn delete_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM workflow_templates WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Template not found".to_string()));
    }

    Ok(Json(json!({"message": "Template deleted"})))
}

/// POST /api/v1/templates/{id}/install
/// Installs a template as a new workflow for the current tenant.
/// Copies the template steps into a new workflow and returns the workflow.
pub async fn install_template_as_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<HashMap<String, serde_json::Value>>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, tenant_id, "max_workflows", "Workflows").await?;

    // Fetch template
    let template = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1 AND (tenant_id = $2 OR is_public = true)",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Template not found or not accessible".to_string()))?;

    // Fetch template steps
    let template_steps = sqlx::query_as::<_, WorkflowTemplateStep>(
        "SELECT * FROM workflow_template_steps WHERE template_id = $1 ORDER BY sort_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    if template_steps.is_empty() {
        return Err(AppError::BadRequest(
            "Template has no steps. Cannot install an empty template.".to_string(),
        ));
    }

    // Allow caller to override name/description/surface_id
    let workflow_name = req.get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&template.name)
        .to_string();

    let workflow_description = req.get("description")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| template.description.clone());

    let surface_id = req.get("surface_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .or(template.surface_id);

    let workflow_id = Uuid::new_v4();

    // Create the workflow
    let workflow = sqlx::query_as::<_, crate::models::workflow::Workflow>(
        r#"INSERT INTO workflows (id, tenant_id, name, description, category, tags, surface_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           RETURNING *"#,
    )
    .bind(workflow_id)
    .bind(tenant_id)
    .bind(&workflow_name)
    .bind(&workflow_description)
    .bind(&template.category)
    .bind(&template.tags)
    .bind(surface_id)
    .fetch_one(&state.db)
    .await?;

    // Copy steps
    for step in &template_steps {
        sqlx::query(
            r#"INSERT INTO workflow_steps (id, workflow_id, step_type, name, description, sort_order, config)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind(Uuid::new_v4())
        .bind(workflow_id)
        .bind(&step.step_type)
        .bind(&step.name)
        .bind(&step.description)
        .bind(step.sort_order)
        .bind(&step.config)
        .execute(&state.db)
        .await?;
    }

    // Return the new workflow with its steps
    let steps = sqlx::query_as::<_, crate::models::workflow::WorkflowStep>(
        "SELECT * FROM workflow_steps WHERE workflow_id = $1 ORDER BY sort_order ASC",
    )
    .bind(workflow_id)
    .fetch_all(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({
        "workflow": workflow,
        "steps": steps,
        "message": format!("Template '{}' installed as workflow", template.name)
    }))))
}

pub async fn get_template_steps(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let _tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let steps = sqlx::query_as::<_, WorkflowTemplateStep>(
        "SELECT * FROM workflow_template_steps WHERE template_id = $1 ORDER BY sort_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"steps": steps})))
}
