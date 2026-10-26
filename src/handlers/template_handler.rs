use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use std::collections::{HashMap, HashSet};
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

/// The ids of the templates this account OWNS, for the `is_mine` flag below.
///
/// `workflow_templates.aid` is nullable: the platform's own seeded blueprints are `aid IS NULL`
/// with `is_public = true`, so `GET /templates` returns a mix of "mine" and "public" rows and the
/// serialised row carries no ownership field at all. The console therefore had no way to tell the
/// two apart and drew Edit/Del on every row — including the public ones, where `update_template` /
/// `delete_template` scope by `aid = $2` and answer 404. Measured live: PUT and DELETE on a seeded
/// public template both 404 (kanban t_be118f36).
async fn owned_template_ids(db: &sqlx::PgPool, aid: Uuid) -> Result<HashSet<Uuid>, sqlx::Error> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM workflow_templates WHERE aid = $1")
        .bind(aid)
        .fetch_all(db)
        .await?;
    Ok(ids.into_iter().collect())
}

/// Serialise template rows with the two fields the row shape was missing: whether the CALLER's
/// account owns the row (`is_mine`) and how many steps it carries (`steps_count`).
///
/// Additive on purpose (kanban t_be118f36): every existing key of `WorkflowTemplate` is preserved
/// byte for byte, and the account id itself is never exposed — the flag is a boolean computed
/// server-side, so a public template shared across tenants does not leak the owner's
/// `workflow_templates.aid` to them. The console gates Edit / Del / Edit-steps on it, so no panel
/// draws a control the API will refuse with 404.
///
/// `steps_count` is the same rule for the INSTALL action (kanban t_6c589b6b). `install` refuses a
/// template that carries no steps with a 400 ("Template has no steps. Cannot install an empty
/// template."), and the row shape carried no count, so the console drew "Install as Workflow" on
/// every row it was allowed to install — on four of the five seeded platform blueprints that button
/// could only ever fail. A button that cannot do anything is worse than no button: the console now
/// draws the install control only where this count is non-zero.
fn templates_with_ownership(
    rows: Vec<WorkflowTemplate>,
    owned: &HashSet<Uuid>,
    step_counts: &HashMap<Uuid, i64>,
) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|t| {
            let mut v = serde_json::to_value(t).unwrap_or_else(|_| json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert("is_mine".to_string(), json!(owned.contains(&t.id)));
                obj.insert(
                    "steps_count".to_string(),
                    json!(step_counts.get(&t.id).copied().unwrap_or(0)),
                );
            }
            v
        })
        .collect()
}

/// How many steps each of these templates carries, for the `steps_count` flag above.
///
/// ONE grouped query for the whole page — never one per row. A template absent from the result
/// carries zero steps, which is exactly what the console must not offer to install.
async fn template_step_counts(
    db: &sqlx::PgPool,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, i64>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query(
        "SELECT template_id, count(*) AS n FROM workflow_template_steps \
         WHERE template_id = ANY($1) GROUP BY template_id",
    )
    .bind(ids)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            (
                r.try_get::<Uuid, _>("template_id").unwrap_or_default(),
                r.try_get::<i64, _>("n").unwrap_or(0),
            )
        })
        .collect())
}

pub async fn list_templates(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListTemplatesQuery>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let owned = owned_template_ids(&state.db, aid).await?;

    // Determine which templates this account's plan allows
    let plan_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT plan_id FROM account_plans WHERE aid = $1 AND status = 'active'",
    )
    .bind(aid)
    .fetch_optional(&state.db)
    .await?
    .flatten();

    // Resolve plan tier so the client can gate on free vs paid (slug + price).
    //
    // `plan_tiers.price_monthly` is NUMERIC(10,2) and this crate's sqlx carries no decimal feature,
    // so `try_get::<Option<f64>, _>("price")` FAILS on it and the `.unwrap_or(None)` that used to sit
    // here silently turned every account's price into 0.0 — i.e. `is_paid: false` for every tenant,
    // paid ones included. The console gates the whole Templates gallery on that flag
    // (`lockAll = !isPaid`), so its install control was withdrawn for EVERY tenant and the gallery
    // read "Free plan (professional) — upgrade to unlock templates". Measured live 2026-10-14 as a
    // tenant on the $79 professional tier (kanban t_6c589b6b). Read the column the way the rest of
    // this app reads it — `price_monthly::text` (kanban t_01fa9bbc, src/models/plan.rs) — and parse
    // the decimal string. Every other NUMERIC column in the schema is read as text already.
    let plan_info: Option<serde_json::Value> = match plan_id {
        Some(pid) => {
            // `name` rides along so the console can print the plan the tenant is actually on
            // instead of the internal slug ("Free plan (free)", measured live 2026-10-26).
            sqlx::query(
                "SELECT slug, name, price_monthly::text AS price FROM plan_tiers WHERE id = $1",
            )
            .bind(pid)
            .fetch_optional(&state.db)
            .await?
            .map(|row| {
                let slug: String = row.try_get("slug").unwrap_or_default();
                let name: String = row.try_get("name").unwrap_or_default();
                let price: f64 = row
                    .try_get::<Option<String>, _>("price")
                    .ok()
                    .flatten()
                    .and_then(|t| t.trim().parse::<f64>().ok())
                    .unwrap_or(0.0);
                json!({
                    "slug": slug,
                    "name": name,
                    "is_paid": price > 0.0,
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
            let counts = template_step_counts(
                &state.db,
                &templates.iter().map(|t| t.id).collect::<Vec<_>>(),
            )
            .await?;
            return Ok(Json(json!({
                "templates": templates_with_ownership(templates, &owned, &counts)
            })));
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

    let counts = template_step_counts(
        &state.db,
        &templates.iter().map(|t| t.id).collect::<Vec<_>>(),
    )
    .await?;

    Ok(Json(json!({
        "templates": templates_with_ownership(templates, &owned, &counts),
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
    senders: &crate::notify::NotifySenders,
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
    // A Notify step's channel is SENDER-BACKED (kanban t_d3ff37ef): `email` / `sms` pass only where
    // this install has the matching provider configured.
    super::workflow_handler::assert_notify_channel_ok(senders, step_type, config)?;
    // An AI Action step's provider is the same rule one level down (kanban t_03e4d3d9): a template
    // that names a provider this app cannot call would install a dead step into a live workflow.
    super::workflow_handler::assert_ai_provider_ok(step_type, config)
}

/// A template step in the order install would copy it into the new workflow.
///
/// `sort_order` decides, exactly as it does for the workflow itself; the tiebreak only makes a tie
/// deterministic (step id for a stored row, position for an incoming request), mirroring
/// `workflow_handler::load_ordered_steps`' `(sort_order, id)` walk.
struct TemplateStepOrder {
    sort_order: i32,
    tiebreak: String,
    step_type: String,
    name: String,
}

impl TemplateStepOrder {
    fn from_request(position: usize, step: &TemplateStep) -> Self {
        Self {
            sort_order: step.sort_order,
            tiebreak: format!("{position:06}"),
            step_type: step.step_type.clone(),
            name: step.name.clone(),
        }
    }

    fn from_row(step: &WorkflowTemplateStep) -> Self {
        Self {
            sort_order: step.sort_order,
            tiebreak: step.id.to_string(),
            step_type: step.step_type.clone(),
            name: step.name.clone(),
        }
    }

    /// The step that lands at position 0 — the one the Builder calls "step 1".
    fn first(steps: &[TemplateStepOrder]) -> Option<&TemplateStepOrder> {
        steps
            .iter()
            .min_by(|a, b| (a.sort_order, &a.tiebreak).cmp(&(b.sort_order, &b.tiebreak)))
    }
}

/// Refuse a template whose step 1 is not a Data Card.
///
/// The steps API refuses to build a workflow that opens with anything else
/// (`workflow_handler::assert_first_step_is_data_card`, the rule docs/user-guide.md and the
/// Builder's own picker both state), and `install` copies a template's steps into a NEW workflow
/// verbatim. Without this the template path was a second door into `workflow_steps` that could
/// manufacture exactly the shape the Builder refuses — measured live before kanban t_96e77263:
/// a template with a `manual` step at position 0 was created (201) and installed (201).
///
/// The grandfather clause is deliberately NOT applied here, and this is the difference from the
/// workflow edit paths: those grandfather an EXISTING workflow so live data stays editable. The
/// workflow this guard protects does not exist yet — create_template builds a new template and
/// install builds a new workflow — so there is nothing to grandfather and a non-Data-Card step 1
/// is refused outright. The rule the app ships is unchanged: the inbound-capture workflows
/// (`integration` first) keep running and stay editable, they just cannot be authored as new
/// templates or installed as new workflows through this API.
///
/// An empty template has no step 1 and passes here; `install` refuses it separately ("Template has
/// no steps").
fn assert_template_opens_with_data_card(steps: &[TemplateStepOrder]) -> Result<(), AppError> {
    let first = match TemplateStepOrder::first(steps) {
        Some(first) => first,
        None => return Ok(()),
    };
    if super::workflow_handler::is_data_card(&first.step_type) {
        return Ok(());
    }
    Err(AppError::Validation(format!(
        "Template step '{}' (step type '{}') would be step 1 of the workflow this template \
         installs, but step 1 of a workflow must be a Data Card ('data-card') — it is what pulls \
         the run's data, and the Builder refuses to build a workflow whose step 1 is not a Data \
         Card. Make the template's first step (the lowest sort_order) a Data Card.",
        first.name, first.step_type
    )))
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
    let template_notify_senders = crate::notify::load_senders(&state).await;
    for step in &req.steps {
        assert_template_step_runnable(
            &template_notify_senders,
            &step.step_type,
            &step.name,
            &step.config,
        )?;
    }
    // …and the ORDER too (kanban t_96e77263): install copies these steps into a new workflow
    // verbatim, so a template that opens with anything but a Data Card would install a workflow the
    // Builder refuses to build. Refused before the template row exists, like the check above.
    assert_template_opens_with_data_card(
        &req.steps
            .iter()
            .enumerate()
            .map(|(i, s)| TemplateStepOrder::from_request(i, s))
            .collect::<Vec<_>>(),
    )?;

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
    let template_notify_senders = crate::notify::load_senders(&state).await;
    for step in &template_steps {
        assert_template_step_runnable(
            &template_notify_senders,
            &step.step_type,
            &step.name,
            &step.config,
        )?;
    }

    // …and the ORDER, for the same reason and at the same point (kanban t_96e77263): this call is
    // what copies the template's position-0 step into a NEW workflow's position 0, so a stored
    // template that opens with anything but a Data Card is refused here with a named reason
    // instead of installing a workflow the Builder would refuse to build.
    assert_template_opens_with_data_card(
        &template_steps
            .iter()
            .map(TemplateStepOrder::from_row)
            .collect::<Vec<_>>(),
    )?;

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
    let template_notify_senders = crate::notify::load_senders(&state).await;
    for step in &req.steps {
        assert_template_step_runnable(
            &template_notify_senders,
            &step.step_type,
            &step.name,
            &step.config,
        )?;
    }
    // The ordering rule is the same guard one level up (kanban t_96e77263) — an imported file is
    // still a template, and a template's step 1 is the installed workflow's step 1.
    assert_template_opens_with_data_card(
        &req.steps
            .iter()
            .enumerate()
            .map(|(i, s)| TemplateStepOrder::from_request(i, s))
            .collect::<Vec<_>>(),
    )?;

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

    fn rust_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                rust_sources(&p, out);
            } else if p.extension().map(|x| x == "rs").unwrap_or(false) {
                out.push(p);
            }
        }
    }

    /// A float/NUMERIC column must be decoded as the Rust type its SQL type actually maps to — never
    /// through a `try_get::<f64>` whose fallback hides the mismatch.
    ///
    /// Measured live 2026-10-14 (kanban t_6c589b6b): `plan_tiers.price_monthly` is NUMERIC(10,2) and
    /// this crate's sqlx features carry no decimal support (`Cargo.toml`: runtime-tokio-rustls,
    /// postgres, uuid, chrono, json, migrate), so `try_get::<Option<f64>, _>("price")` answered Err
    /// and the `.unwrap_or(None)` beside it turned the $79 professional tier into price 0.0 — i.e.
    /// `is_paid: false` for every tenant. The tenant console gates the whole Templates gallery on
    /// that flag, so its install control was withdrawn for EVERY tenant and the gallery read "Free
    /// plan (professional) — upgrade to unlock templates". Not one build or test caught it: the
    /// decode fails at RUNTIME and the caller's own fallback makes the hole look like data.
    ///
    /// The rules this pins: a NUMERIC column is read with an explicit `::text` cast and parsed (the
    /// app's convention, src/models/plan.rs / kanban t_01fa9bbc), and REAL/float4 decodes as `f32`
    /// (FLOAT8 is what `f64` asks for). A silent `.unwrap_or(None)` on a float decode is the shape
    /// that hides both mistakes, so no file under src/ may carry one.
    #[test]
    fn numeric_columns_are_never_decoded_as_f64() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_sources(&root, &mut files);
        assert!(
            files.len() > 20,
            "the scan must actually walk the crate's sources (found {})",
            files.len()
        );
        // The needles are built with `concat!` so THIS file's own source does not contain the
        // pattern it is hunting — a lint that flags itself can never go green.
        let needles = [
            concat!("try_get::<Option<", "f64", ">"),
            concat!("try_get::<", "f64", ","),
        ];
        let mut hits = Vec::new();
        for f in &files {
            let body = std::fs::read_to_string(f).unwrap_or_default();
            for (n, line) in body.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                if needles.iter().any(|needle| line.contains(needle)) {
                    hits.push(format!("{}:{} {}", f.display(), n + 1, line.trim()));
                }
            }
        }
        assert!(
            hits.is_empty(),
            "a column is being decoded as f64 — the NUMERIC columns in this schema \
             (plan_tiers.price_monthly, credit_packages.price, invoices.amount, \
             checkout_sessions.amount) cannot decode into f64 with this crate's sqlx, so the read \
             silently yields Err and any fallback hides it; a REAL/float4 column likewise is f32, \
             not f64. Read NUMERIC as `::text` and parse it (src/models/plan.rs), and float4 as \
             `f32`. Hits: {hits:#?}"
        );
    }

    /// A template step whose type the engine has no arm for must be refused on the template path
    /// exactly as `POST /workflows/{id}/steps` refuses it: install copies the row VERBATIM into
    /// `workflow_steps`, so accepting it would manufacture a workflow of steps that do nothing
    /// (kanban t_27a15474). The refusal has to name both the offending type and the valid list, or
    /// the caller cannot act on it.
    #[test]
    fn a_stage_name_step_type_is_refused() {
        let none = crate::notify::NotifySenders {
            email: false,
            sms: false,
        };
        let err = assert_template_step_runnable(&none, "manage", "Manage", &None)
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
                assert_template_step_runnable(&none, stage, "Stage", &None).is_err(),
                "'{stage}' is a lifecycle stage, not a step type"
            );
        }
    }

    /// The two shapes an install actually has to accept: the Data Card the template opens with and
    /// the `manual` human gates the rest of the lifecycle becomes (migration 073) — plus the Notify
    /// channel rule the steps API enforces, which must hold on this path too.
    #[test]
    fn executable_step_types_pass_and_an_undeliverable_channel_does_not() {
        // No sender configured at all: webhook still passes, both sender-backed channels do not.
        let none = crate::notify::NotifySenders {
            email: false,
            sms: false,
        };
        assert!(assert_template_step_runnable(&none, "data-card", "Discover", &None).is_ok());
        assert!(
            assert_template_step_runnable(&none, "manual", "Qualify", &Some(json!({}))).is_ok()
        );
        assert!(assert_template_step_runnable(
            &none,
            "notify",
            "Outreach",
            &Some(json!({"channel": "webhook"}))
        )
        .is_ok());
        for channel in ["email", "sms"] {
            let err = assert_template_step_runnable(
                &none,
                "notify",
                "Outreach",
                &Some(json!({ "channel": channel })),
            )
            .unwrap_err();
            assert!(
                err.to_string().contains(channel),
                "the refusal must name the channel: {err}"
            );
        }

        // With a mail sender configured, `email` is the same rule the steps API applies: accepted.
        let mail = crate::notify::NotifySenders {
            email: true,
            sms: false,
        };
        assert!(assert_template_step_runnable(
            &mail,
            "notify",
            "Outreach",
            &Some(json!({"channel": "email"}))
        )
        .is_ok());
        assert!(
            assert_template_step_runnable(
                &mail,
                "notify",
                "Outreach",
                &Some(json!({"channel": "sms"}))
            )
            .is_err(),
            "sms is still refused while no SMS provider is configured"
        );

        // A sender-backed channel may not ALSO carry the retired free-text `recipient` (kanban
        // t_d3ff37ef): that box used to mean "any address", which is exactly the open-relay shape
        // the recipient rule removes. Refused at the WRITE path, not only at send time.
        let err = assert_template_step_runnable(
            &mail,
            "notify",
            "Outreach",
            &Some(json!({"channel": "email", "recipient": "stranger@example.com"})),
        )
        .expect_err("a free-text destination is not an audience");
        assert!(
            err.to_string().contains("free-text"),
            "the refusal must say why: {err}"
        );
    }

    // ── The ordering rule on the template path (kanban t_96e77263) ────────────────────────────────
    //
    // A template's position-0 step becomes the installed workflow's position-0 step, and the Builder
    // refuses to build a workflow that opens with anything but a Data Card. Measured live before the
    // fix: `POST /templates` with a `manual` step at sort_order 0 answered 201 and
    // `POST /templates/{id}/install` answered 201 — manufacturing the shape the Builder refuses.

    fn req_step(step_type: &str, name: &str, sort_order: i32) -> TemplateStep {
        TemplateStep {
            step_type: step_type.to_string(),
            name: name.to_string(),
            description: None,
            sort_order,
            config: None,
        }
    }

    fn template_row(
        id: Uuid,
        step_type: &str,
        name: &str,
        sort_order: i32,
    ) -> WorkflowTemplateStep {
        WorkflowTemplateStep {
            id,
            template_id: Uuid::new_v4(),
            step_type: step_type.to_string(),
            name: name.to_string(),
            description: None,
            sort_order,
            config: None,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn a_template_that_opens_with_a_data_card_is_accepted() {
        let steps = vec![
            TemplateStepOrder::from_request(0, &req_step("data-card", "Discover", 0)),
            TemplateStepOrder::from_request(1, &req_step("manual", "Qualify", 1)),
        ];
        assert!(assert_template_opens_with_data_card(&steps).is_ok());
    }

    #[test]
    fn a_template_that_opens_with_anything_else_is_refused_and_names_the_step() {
        let steps = vec![TemplateStepOrder::from_request(
            0,
            &req_step("manual", "Qualify", 0),
        )];
        let err = assert_template_opens_with_data_card(&steps)
            .expect_err("a template whose step 1 is a manual step must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("Qualify"),
            "must name the offending step: {msg}"
        );
        assert!(msg.contains("manual"), "must name its type: {msg}");
        assert!(
            msg.contains("data-card"),
            "must name the rule it breaks: {msg}"
        );
    }

    /// The step that SITS at position 0 decides, not the order the steps arrive in — the lowest
    /// `sort_order` is what install copies first.
    #[test]
    fn the_lowest_sort_order_decides_not_the_array_order() {
        let steps = vec![
            TemplateStepOrder::from_request(0, &req_step("data-card", "Discover", 3)),
            TemplateStepOrder::from_request(1, &req_step("manual", "Qualify", 0)),
        ];
        let err = assert_template_opens_with_data_card(&steps)
            .expect_err("sort_order 0 is the manual step, whatever the JSON order is");
        assert!(err.to_string().contains("Qualify"), "{err}");
    }

    /// An empty template has no step 1 to refuse; `install` refuses it separately with its own
    /// message ("Template has no steps").
    #[test]
    fn an_empty_template_has_no_step_one_to_refuse() {
        assert!(assert_template_opens_with_data_card(&[]).is_ok());
    }

    /// The install leg reads STORED rows, so the rule has to hold for a template written straight
    /// into the table (what a pre-rule template looks like). There is deliberately no grandfather
    /// here: the workflow being protected does not exist yet.
    #[test]
    fn a_stored_template_that_opens_with_a_manual_step_is_refused() {
        let rows = [
            template_row(Uuid::new_v4(), "manual", "Qualify", 0),
            template_row(Uuid::new_v4(), "data-card", "Discover", 1),
        ];
        let ordered: Vec<TemplateStepOrder> =
            rows.iter().map(TemplateStepOrder::from_row).collect();
        let err = assert_template_opens_with_data_card(&ordered)
            .expect_err("a stored template whose first step is manual must not install");
        assert!(err.to_string().contains("Qualify"), "{err}");
    }
}
