use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::features;
use crate::models::template::*;
use crate::AppState;

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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Determine which templates this account's plan allows
    let plan_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT plan_id FROM account_plans WHERE aid = $1 AND status = 'active'",
    )
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .flatten();

    // Resolve plan tier so the client can gate on free vs paid (slug + price).
    let plan_info: Option<serde_json::Value> = match plan_id {
        Some(pid) => {
            sqlx::query("SELECT slug, price_monthly AS price FROM plan_tiers WHERE id = $1")
                .bind(pid)
                .fetch_optional(&state.db)
                .await?
                .map(|row| {
                    let slug: String = row.try_get("slug").unwrap_or_default();
                    let price: Option<f64> = row.try_get("price").unwrap_or(None);
                    json!({
                        "slug": slug,
                        "is_paid": price.unwrap_or(0.0) > 0.0,
                    })
                })
        }
        None => None,
    };

    // If a specific industry is requested, also check user has access to it
    if let Some(ref industry_slug) = query.industry {
        let has_industry: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM account_industries WHERE aid = $1 AND industry_slug = $2 AND is_active = true)"
        )
        .bind(aid)
        .bind(industry_slug)
        .fetch_one(&state.db)
        .await
        .unwrap_or(false);

        // If account doesn't own this industry, fall back to public templates
        if !has_industry {
            // Allow view but only show public templates for that industry
            let templates = if let Some(surface_id) = query.surface {
                sqlx::query_as::<_, WorkflowTemplate>(
                    r#"SELECT wt.* FROM workflow_templates wt
                       INNER JOIN template_categories tc ON tc.slug = wt.category AND tc.is_active = true
                       WHERE tc.slug = $1 AND wt.is_public = true
                       AND (wt.surface_id = $2 OR wt.surface_id IS NULL)
                       ORDER BY wt.name ASC"#,
                )
                .bind(industry_slug)
                .bind(surface_id)
                .fetch_all(&state.db)
                .await?
            } else {
                sqlx::query_as::<_, WorkflowTemplate>(
                    r#"SELECT wt.* FROM workflow_templates wt
                       INNER JOIN template_categories tc ON tc.slug = wt.category AND tc.is_active = true
                       WHERE tc.slug = $1 AND wt.is_public = true
                       ORDER BY wt.name ASC"#,
                )
                .bind(industry_slug)
                .fetch_all(&state.db)
                .await?
            };
            return Ok(Json(json!({"templates": templates})));
        }
    }

    // For account-owned templates, combine with plan capabilities
    // Also include PUBLIC templates (General category) that show for everyone
    let templates = if let Some(ref industry_slug) = query.industry {
        let templates: Vec<WorkflowTemplate> = if let Some(surface_id) = query.surface {
            sqlx::query_as::<_, WorkflowTemplate>(
                r#"SELECT DISTINCT wt.* FROM workflow_templates wt
                   INNER JOIN template_categories tc ON tc.slug = wt.category AND tc.is_active = true
                   WHERE (wt.aid = $1 OR (wt.is_public = true))
                   AND tc.slug = $2
                   AND (wt.surface_id = $3 OR wt.surface_id IS NULL)
                   ORDER BY wt.name ASC"#,
            )
            .bind(aid)
            .bind(industry_slug)
            .bind(surface_id)
            .fetch_all(&state.db)
            .await?
        } else {
            sqlx::query_as::<_, WorkflowTemplate>(
                r#"SELECT DISTINCT wt.* FROM workflow_templates wt
                   INNER JOIN template_categories tc ON tc.slug = wt.category AND tc.is_active = true
                   WHERE (wt.aid = $1 OR (wt.is_public = true))
                   AND tc.slug = $2
                   ORDER BY wt.name ASC"#,
            )
            .bind(aid)
            .bind(industry_slug)
            .fetch_all(&state.db)
            .await?
        };
        templates
    } else if let Some(surface_id) = query.surface {
        sqlx::query_as::<_, WorkflowTemplate>(
            "SELECT DISTINCT * FROM workflow_templates WHERE (aid = $1 OR (is_public = true)) AND (surface_id = $2 OR surface_id IS NULL) ORDER BY name ASC",
        )
        .bind(aid)
        .bind(surface_id)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, WorkflowTemplate>(
            "SELECT DISTINCT * FROM workflow_templates WHERE aid = $1 OR (is_public = true) ORDER BY name ASC",
        )
        .bind(aid)
        .fetch_all(&state.db)
        .await?
    };

    // Also attach what public templates are available based on the plan + industries    // Also attach what public templates are available based on the user's plan + industries
    let available_public: Vec<serde_json::Value> = if let Some(pid) = plan_id {
        sqlx::query(
            r#"SELECT wt.id::text, wt.name, wt.description, wt.category, tc.name as industry_name
               FROM v_plan_industry_templates vpit
               JOIN workflow_templates wt ON wt.id = vpit.template_id
               JOIN template_categories tc ON tc.slug = vpit.industry_slug
               WHERE vpit.plan_id = $1
                  AND (wt.aid = $2 OR wt.is_public = true)
               ORDER BY wt.name"#,
        )
        .bind(pid)
        .bind(aid)
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
        "available_public": available_public,
        "plan": plan_info
    })))
}

/// Refuse a template step the steps API would not accept.
///
/// `workflow_template_steps.step_type` is copied VERBATIM into `workflow_steps` by
/// `POST /templates/{id}/install` (below), so a template may only carry step types the engine has
/// an arm for — otherwise installing it manufactures the exact defect kanban t_fe60cdf5 closed at
/// the steps API: a workflow made entirely of steps that fall through the engine's `_` arm
/// (`skipped`, `unexecutable`, named in the run's warnings[]) and never run anything.
///
/// Before kanban t_27a15474 nothing on the template path validated the column at all, and the
/// seeded Government Contracting template held ten lifecycle STAGE NAMES ('discover', 'qualify',
/// 'team', ...) in it: the `name` column already carries those labels ('Discover', 'Qualify', ...),
/// so the stage words were simply the wrong vocabulary for `step_type`. Migration 073 remaps the
/// seeded rows and this guard is what stops the class from coming back — on `create_template` and
/// `import_template` (so a tenant cannot build such a template) and on `install` (so a template that
/// predates the guard is refused with a named reason instead of installing into a dead workflow).
///
/// A Notify step's deliverable channel is the same rule one level down, so it is checked here too
/// by the steps API's own function (kanban t_08be842f).
fn assert_template_step_runnable(
    step_type: &str,
    name: &str,
    config: &Option<serde_json::Value>,
) -> Result<(), AppError> {
    if !crate::execution::is_executable_step_type(step_type) {
        return Err(AppError::Validation(format!(
            "Template step '{}' has step type '{}', which has no executor in this app. Installing \
             this template would create a workflow whose steps do nothing. Valid step types are: {}",
            name,
            step_type,
            crate::execution::executable_step_type_list()
        )));
    }
    super::workflow_handler::assert_notify_channel_ok(step_type, config)
}

pub async fn create_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateTemplateRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_templates", "Templates").await?;

    // Validate EVERY step before the template row exists: a refused step must not leave an empty
    // template behind (t_27a15474).
    for step in &req.steps {
        assert_template_step_runnable(&step.step_type, &step.name, &step.config)?;
    }

    let template_id = Uuid::new_v4();
    let template = sqlx::query_as::<_, WorkflowTemplate>(
        r#"INSERT INTO workflow_templates (id, aid, name, description, category, tags, is_public, surface_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           RETURNING *"#,
    )
    .bind(template_id)
    .bind(aid)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.category)
    .bind(&req.tags)
    .bind(false)
    .bind(req.surface_id)
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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let template = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1 AND aid = $2",
    )
    .bind(id)
    .bind(aid)
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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1 AND aid = $2",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Template not found".to_string()))?;

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&existing.name)
        .to_string();
    // Surface is reassignable here too. A body field that is absent (or null / not a uuid)
    // keeps the stored surface; read before `existing` is partially moved below.
    let surface_id = req
        .get("surface_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .or(existing.surface_id);
    let description = req
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or(existing.description);
    let category = req
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or(&existing.category)
        .to_string();
    let tags = req.get("tags").cloned().or(existing.tags);

    sqlx::query(
        r#"UPDATE workflow_templates SET name=$1, description=$2, category=$3, tags=$4, surface_id=$5 WHERE id=$6"#,
    )
    .bind(&name)
    .bind(&description)
    .bind(&category)
    .bind(&tags)
    .bind(surface_id)
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
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM workflow_templates WHERE id = $1 AND aid = $2")
        .bind(id)
        .bind(aid)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Template not found".to_string()));
    }

    Ok(Json(json!({"message": "Template deleted"})))
}

/// POST /api/v1/templates/{id}/install
/// Installs a template as a new workflow for the current account.
/// Copies the template steps into a new workflow and returns the workflow.
pub async fn install_template_as_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<HashMap<String, serde_json::Value>>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_workflows", "Workflows").await?;

    // Fetch template
    let template = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1 AND (aid = $2 OR is_public = true)",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(
        "Template not found or not accessible".to_string(),
    ))?;

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

    // Refuse BEFORE the workflow row exists: a template that carries a step type this app cannot
    // execute is answered with a named reason instead of installing into a workflow whose every
    // step is `skipped`/`unexecutable` (kanban t_27a15474; the seeded template hit this for all ten
    // of its rows). The check runs up front so a refusal cannot leave a half-built workflow behind.
    for step in &template_steps {
        assert_template_step_runnable(&step.step_type, &step.name, &step.config)?;
    }

    // Allow caller to override name/description/surface_id
    let workflow_name = req
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&template.name)
        .to_string();

    let workflow_description = req
        .get("description")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| template.description.clone());

    let surface_id = req
        .get("surface_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .or(template.surface_id);

    let workflow_id = Uuid::new_v4();

    // Create the workflow
    let workflow = sqlx::query_as::<_, crate::models::workflow::Workflow>(
        r#"INSERT INTO workflows (id, aid, name, description, category, tags, surface_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           RETURNING *"#,
    )
    .bind(workflow_id)
    .bind(aid)
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

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "workflow": workflow,
            "steps": steps,
            "message": format!("Template '{}' installed as workflow", template.name)
        })),
    ))
}

pub async fn get_template_steps(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let _aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let steps = sqlx::query_as::<_, WorkflowTemplateStep>(
        "SELECT * FROM workflow_template_steps WHERE template_id = $1 ORDER BY sort_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"steps": steps})))
}

// ===== Template JSON Export / Import (added 2026-08-22) =====

// GET /templates/{id}/export?download=1
// Returns a portable JSON payload: the template's metadata + its steps as an array.
// The client can save this as a .json file and re-import it (here or on any account)
// via POST /templates/import. Surface/timestamps/internal ids are not round-tripped.
pub async fn export_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let template = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1 AND (aid = $2 OR is_public = true)",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Template not found".to_string()))?;

    let steps = sqlx::query_as::<_, WorkflowTemplateStep>(
        "SELECT * FROM workflow_template_steps WHERE template_id = $1 ORDER BY sort_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let steps_json: Vec<serde_json::Value> = steps
        .iter()
        .map(|s| {
            json!({
                "step_type": s.step_type,
                "name": s.name,
                "description": s.description,
                "sort_order": s.sort_order,
                "config": s.config,
            })
        })
        .collect();

    let payload = json!({
        "schema_version": 1,
        "template": {
            "name": template.name,
            "description": template.description,
            "category": template.category,
            "category_id": template.category_id,
            "tags": template.tags,
            "steps": steps_json,
        }
    });
    Ok((
        StatusCode::OK,
        Json(json!({"template": payload, "exported": true})),
    ))
}

// POST /templates/import
// Body matches the CreateTemplateRequest shape ({ name, description, category, category_id, tags, steps[] }).
// Creates a NEW template + steps owned by the calling account (account-private, is_public=false).
// Mirrors create_template insertion so import behaves identically to manual creation.
pub async fn import_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateTemplateRequest>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(&state.db, aid, "max_templates", "Templates").await?;

    // Same guard as create_template, and for the same reason: this is the other door a template can
    // come in through, and install copies its steps verbatim into a workflow (kanban t_27a15474).
    for step in &req.steps {
        assert_template_step_runnable(&step.step_type, &step.name, &step.config)?;
    }

    let template_id = Uuid::new_v4();
    let template = sqlx::query_as::<_, WorkflowTemplate>(
        r#"INSERT INTO workflow_templates (id, aid, name, description, category, tags, is_public, surface_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           RETURNING *"#,
    )
    .bind(template_id)
    .bind(aid)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.category)
    .bind(&req.tags)
    .bind(false)
    .bind(req.surface_id)
    .fetch_one(&state.db)
    .await?;

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

    Ok((
        StatusCode::CREATED,
        Json(json!({"template": template, "imported": true})),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A template step whose type the engine has no arm for must be refused on the template path
    /// exactly as `POST /workflows/{id}/steps` refuses it: install copies the row VERBATIM into
    /// `workflow_steps`, so accepting it would manufacture a workflow of steps that do nothing
    /// (kanban t_27a15474). The refusal has to name both the offending type and the valid list, or
    /// the caller cannot act on it.
    #[test]
    fn a_stage_name_step_type_is_refused() {
        let err = assert_template_step_runnable("manage", "Manage", &None)
            .expect_err("a stage name is not a step type");
        let msg = err.to_string();
        assert!(
            msg.contains("'manage'"),
            "the refusal must name the type: {msg}"
        );
        assert!(
            msg.contains("manual"),
            "the refusal must list the valid types: {msg}"
        );
        // The whole seeded lifecycle vocabulary is in this class, not just one word.
        for stage in [
            "discovery",
            "qualify",
            "team",
            "propose",
            "submit",
            "track",
            "manage",
            "intel",
            "outreach",
            "dashboard",
        ] {
            assert!(
                assert_template_step_runnable(stage, "Stage", &None).is_err(),
                "'{stage}' is a lifecycle stage, not a step type"
            );
        }
    }

    /// The two shapes an install actually has to accept: the Data Card the template opens with and
    /// the `manual` human gates the rest of the lifecycle becomes (migration 073) — plus the Notify
    /// channel rule the steps API enforces, which must hold on this path too.
    #[test]
    fn executable_step_types_pass_and_an_undeliverable_channel_does_not() {
        assert!(assert_template_step_runnable("data-card", "Discover", &None).is_ok());
        assert!(assert_template_step_runnable("manual", "Qualify", &Some(json!({}))).is_ok());
        assert!(assert_template_step_runnable(
            "notify",
            "Outreach",
            &Some(json!({"channel": "webhook"}))
        )
        .is_ok());
        let err =
            assert_template_step_runnable("notify", "Outreach", &Some(json!({"channel": "email"})))
                .expect_err("email has no sender in this app");
        assert!(
            err.to_string().contains("email"),
            "the refusal must name the channel: {err}"
        );
    }
}
