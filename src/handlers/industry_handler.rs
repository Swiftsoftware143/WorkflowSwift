use axum::{
    extract::{Json, Path, State},
    response::IntoResponse,
    Extension,
};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::features;
use crate::AppState;

/// GET /api/v1/industries — list all available industries with their templates
pub async fn list_industries(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query(
        "SELECT id::text, slug, name, description, icon, sort_order FROM template_categories WHERE is_active = true ORDER BY sort_order"
    )
    .fetch_all(&state.db)
    .await?;

    let mut industries = Vec::new();
    for row in rows {
        let slug: String = row.try_get("slug").unwrap_or_default();
        let id_text: String = row.try_get("id").unwrap_or_default();
        let name: String = row.try_get("name").unwrap_or_default();
        let description: Option<String> = row.try_get("description").ok();
        let icon: Option<String> = row.try_get("icon").ok();
        let sort_order: i32 = row.try_get("sort_order").unwrap_or(0);

        let template_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM workflow_templates WHERE category = $1 AND is_public = true",
        )
        .bind(&slug)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

        industries.push(json!({
            "id": id_text,
            "slug": slug,
            "name": name,
            "description": description,
            "icon": icon,
            "sort_order": sort_order,
            "template_count": template_count
        }));
    }

    Ok(Json(json!({"industries": industries})))
}

/// GET /api/v1/accounts/industry — get current account's active industry + all linked industries
/// Now returns all industries the account has, plus the "primary" one
pub async fn get_account_industry(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Get primary (legacy) industry slug from accounts table
    let primary_slug: String = sqlx::query_scalar(
        "SELECT COALESCE(industry_slug, 'site-flipping') FROM accounts WHERE id = $1",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or_default();

    // Get all industries this account has dashboards for
    let industry_rows = sqlx::query(
        r#"SELECT ti.industry_slug, tc.name as industry_name, tc.description as industry_description,
                  tc.icon, ti.dashboard_id, ti.is_active
           FROM account_industries ti
           LEFT JOIN template_categories tc ON tc.slug = ti.industry_slug
           WHERE ti.aid = $1
           ORDER BY ti.created_at"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    let mut industries = Vec::new();
    for row in industry_rows {
        let slug: String = row.try_get("industry_slug").unwrap_or_default();
        let name: String = row
            .try_get("industry_name")
            .unwrap_or_else(|_| slug.clone());
        let desc: Option<String> = row.try_get("industry_description").ok();
        let icon: Option<String> = row.try_get("icon").ok();
        let dashboard_id: Option<Uuid> = row.try_get("dashboard_id").ok();
        let is_active: bool = row.try_get("is_active").unwrap_or(true);

        industries.push(json!({
            "industry_slug": slug,
            "industry_name": name,
            "industry_description": desc,
            "icon": icon,
            "dashboard_id": dashboard_id.map(|id| id.to_string()),
            "is_active": is_active,
            "is_primary": slug == primary_slug
        }));
    }

    Ok(Json(json!({
        "primary_industry": primary_slug,
        "industries": industries
    })))
}

/// PUT /api/v1/accounts/industry — set or add account's industry (multi-industry)
/// Now supports setting primary industry OR adding a new industry dashboard
pub async fn set_account_industry(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let industry_slug = req
        .get("industry_slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Validation("industry_slug is required".to_string()))?;

    // Whether to set as primary (default true) or just add as secondary dashboard
    let set_as_primary = req
        .get("set_as_primary")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // Validate industry exists
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM template_categories WHERE slug = $1 AND is_active = true)",
    )
    .bind(industry_slug)
    .fetch_one(&state.db)
    .await
    .unwrap_or(false);

    if !exists {
        return Err(AppError::NotFound(format!(
            "Industry '{}' not found",
            industry_slug
        )));
    }

    // Check plan limits for multi-industry
    let is_new = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM account_industries WHERE aid = $1 AND industry_slug = $2",
    )
    .bind(aid)
    .bind(industry_slug)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0)
        == 0;

    if is_new {
        // Check how many industries this account already has
        let current_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM account_industries WHERE aid = $1 AND is_active = true",
        )
        .bind(aid)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

        features::enforce_feature_limit(&state.db, aid, "max_industries", "Industries").await?;
    }

    // Get the human-readable category name for this industry
    let industry_name: String =
        sqlx::query_scalar("SELECT name FROM template_categories WHERE slug = $1")
            .bind(industry_slug)
            .fetch_optional(&state.db)
            .await?
            .unwrap_or_else(|| industry_slug.to_string());

    // Dashboard name uses the human-readable category name, not the slug
    // This keeps dashboard names in sync with template category names
    let dashboard_name = format!("{} Dashboard", industry_name);
    let dashboard_id = match sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM dashboards WHERE aid = $1 AND name = $2 LIMIT 1",
    )
    .bind(aid)
    .bind(&dashboard_name)
    .fetch_optional(&state.db)
    .await?
    {
        Some(id) => id,
        None => {
            let id = Uuid::new_v4();
            sqlx::query(
                r#"INSERT INTO dashboards (id, aid, name, description)
                   VALUES ($1, $2, $3, $4)"#,
            )
            .bind(id)
            .bind(aid)
            .bind(&dashboard_name)
            .bind(format!("Your {} dashboard", industry_name))
            .execute(&state.db)
            .await?;

            seed_default_widgets_internal(&state, aid, id, industry_slug).await;
            id
        }
    };

    // Upsert into account_industries
    sqlx::query(
        r#"INSERT INTO account_industries (aid, industry_slug, dashboard_id, is_active)
           VALUES ($1, $2, $3, true)
           ON CONFLICT (aid, industry_slug)
           DO UPDATE SET dashboard_id = EXCLUDED.dashboard_id, is_active = true, updated_at = NOW()"#,
    )
    .bind(aid)
    .bind(industry_slug)
    .bind(dashboard_id)
    .execute(&state.db)
    .await?;

    // Set as primary industry if requested
    if set_as_primary {
        sqlx::query("UPDATE accounts SET industry_slug = $1 WHERE id = $2")
            .bind(industry_slug)
            .bind(aid)
            .execute(&state.db)
            .await?;
    }

    Ok(Json(json!({
        "industry_slug": industry_slug,
        "dashboard_id": dashboard_id.to_string(),
        "is_primary": set_as_primary,
        "message": format!("Industry '{}' dashboard is ready", industry_slug)
    })))
}

/// DELETE /api/v1/accounts/industry/{slug} — remove an industry dashboard
pub async fn remove_account_industry(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(slug): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Check if this is the primary industry — if so, can't remove, must switch primary first
    let primary: String = sqlx::query_scalar(
        "SELECT COALESCE(industry_slug, 'site-flipping') FROM accounts WHERE id = $1",
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await
    .unwrap_or_default();

    if primary == slug {
        // Check if there are other industries — auto-switch primary if possible
        let other: Option<String> = sqlx::query_scalar(
            "SELECT industry_slug FROM account_industries 
             WHERE aid = $1 AND industry_slug != $2 AND is_active = true 
             LIMIT 1",
        )
        .bind(aid)
        .bind(&slug)
        .fetch_optional(&state.db)
        .await?
        .flatten();

        if let Some(new_primary) = other {
            sqlx::query("UPDATE accounts SET industry_slug = $1 WHERE id = $2")
                .bind(&new_primary)
                .bind(aid)
                .execute(&state.db)
                .await?;
        } else {
            // Can't remove the last industry
            sqlx::query("UPDATE accounts SET industry_slug = 'site-flipping' WHERE id = $1")
                .bind(aid)
                .execute(&state.db)
                .await?;
        }
    }

    // Soft-deactivate (don't delete dashboards/data)
    sqlx::query(
        "UPDATE account_industries SET is_active = false, updated_at = NOW() WHERE aid = $1 AND industry_slug = $2"
    )
    .bind(aid)
    .bind(&slug)
    .execute(&state.db)
    .await?;

    Ok(Json(
        json!({"message": format!("Industry '{}' dashboard removed", slug)}),
    ))
}

/// Auto-generates smart dashboard widgets for ANY industry based on its slug and name.
/// Instead of hardcoded per-industry lists, it generates relevant KPI widgets dynamically:
/// - Always: 1 revenue/volume counter, 1 pipeline/active counter, 1 completion/conversion counter
/// - Always: 1 recent activity feed
/// - Additional widgets based on industry type (sales-pipeline vs project-tracker vs service-scheduler)
/// All widgets accept data pushed via n8n, APIs, or browser automation through the standard endpoints.
pub async fn seed_default_widgets_internal(
    state: &AppState,
    _aid: Uuid,
    dashboard_id: Uuid,
    industry_slug: &str,
) {
    let widgets = generate_industry_widgets(industry_slug);

    for (w_type, title, config, position) in widgets {
        let _result = sqlx::query(
            r#"INSERT INTO dashboard_widgets (id, dashboard_id, widget_type, title, config, position)
               VALUES ($1, $2, $3, $4, $5::jsonb, $6::jsonb)
               ON CONFLICT DO NOTHING"#,
        )
        .bind(Uuid::new_v4())
        .bind(dashboard_id)
        .bind(w_type)
        .bind(&title)
        .bind(&config)
        .bind(&position)
        .execute(&state.db)
        .await;
    }
}

/// Generates smart widgets for ANY industry based on its slug.
/// Pattern-based, not hardcoded per industry — new industries automatically get relevant KPIs.
fn generate_industry_widgets(
    industry_slug: &str,
) -> Vec<(&'static str, String, serde_json::Value, serde_json::Value)> {
    let mut widgets: Vec<(&'static str, String, serde_json::Value, serde_json::Value)> = Vec::new();

    // Extract base type from slug to determine KPI patterns
    let is_sales_pipeline = industry_slug.contains("lead")
        || industry_slug.contains("sales")
        || industry_slug.contains("recruit")
        || industry_slug.contains("agency");
    let is_project_tracker = industry_slug.contains("construction")
        || industry_slug.contains("development")
        || industry_slug.contains("flipping")
        || industry_slug.contains("publishing")
        || industry_slug.contains("education")
        || industry_slug.contains("training");
    let is_service_scheduler = industry_slug.contains("service")
        || industry_slug.contains("healthcare")
        || industry_slug.contains("wellness")
        || industry_slug.contains("professional");
    let is_ecommerce = industry_slug.contains("ecommerce") || industry_slug.contains("retail");
    let is_grant_funding = industry_slug.contains("grant") || industry_slug.contains("funding");
    let is_govcon = industry_slug.contains("government") || industry_slug.contains("contracting");

    // Helper: build metric_key
    let mk = |suffix: &str| -> String { format!("{}_{}", industry_slug, suffix) };

    // Row 0: Top-level stat counters (always present, industry-specific labels)
    if is_sales_pipeline {
        widgets.push((
            "stat-counter",
            "Active Leads".to_string(),
            mk_config(
                &mk("active_leads"),
                "Leads / candidates in pipeline",
                None,
                None,
            ),
            pos(0, 0, 2, 1),
        ));
        widgets.push((
            "stat-counter",
            "Conversion Rate".to_string(),
            mk_config(&mk("conversion_rate"), "Lead to close %", Some("%"), None),
            pos(0, 2, 2, 1),
        ));
        widgets.push((
            "revenue-card",
            "Revenue".to_string(),
            mk_config(&mk("revenue"), "Total closed deals", None, Some("USD")),
            pos(0, 4, 3, 1),
        ));
    } else if is_project_tracker {
        widgets.push((
            "stat-counter",
            "Active Projects".to_string(),
            mk_config(
                &mk("active_projects"),
                "In development / in progress",
                None,
                None,
            ),
            pos(0, 0, 2, 1),
        ));
        widgets.push((
            "stat-counter",
            "Completed".to_string(),
            mk_config(&mk("completed_count"), "Completed this period", None, None),
            pos(0, 2, 2, 1),
        ));
        widgets.push((
            "revenue-card",
            "Revenue".to_string(),
            mk_config(&mk("revenue"), "Total projects / sales", None, Some("USD")),
            pos(0, 4, 3, 1),
        ));
    } else if is_service_scheduler {
        widgets.push((
            "stat-counter",
            "Active Clients".to_string(),
            mk_config(
                &mk("active_clients"),
                "Current clients / patients",
                None,
                None,
            ),
            pos(0, 0, 2, 1),
        ));
        widgets.push((
            "stat-counter",
            "Appointments".to_string(),
            mk_config(&mk("appointments"), "Scheduled this week", None, None),
            pos(0, 2, 2, 1),
        ));
        widgets.push((
            "revenue-card",
            "Billings".to_string(),
            mk_config(&mk("billings"), "Invoiced / collected", None, Some("USD")),
            pos(0, 4, 3, 1),
        ));
    } else if is_ecommerce {
        widgets.push((
            "stat-counter",
            "Active Orders".to_string(),
            mk_config(&mk("active_orders"), "Orders in fulfillment", None, None),
            pos(0, 0, 2, 1),
        ));
        widgets.push((
            "stat-counter",
            "Products".to_string(),
            mk_config(&mk("products"), "Active listings / inventory", None, None),
            pos(0, 2, 2, 1),
        ));
        widgets.push((
            "revenue-card",
            "Sales".to_string(),
            mk_config(&mk("sales"), "Total revenue", None, Some("USD")),
            pos(0, 4, 3, 1),
        ));
    } else if is_grant_funding {
        widgets.push((
            "stat-counter",
            "Active Grants".to_string(),
            mk_config(&mk("active_grants"), "In progress / pending", None, None),
            pos(0, 0, 2, 1),
        ));
        widgets.push((
            "stat-counter",
            "Success Rate".to_string(),
            mk_config(&mk("success_rate"), "Awarded %", Some("%"), None),
            pos(0, 2, 2, 1),
        ));
        widgets.push((
            "revenue-card",
            "Funding".to_string(),
            mk_config(&mk("funding"), "Total awarded", None, Some("USD")),
            pos(0, 4, 3, 1),
        ));
    } else if is_govcon {
        widgets.push((
            "stat-counter",
            "Open Solicitations".to_string(),
            mk_config(
                &mk("solicitations"),
                "Matching your NAICS codes",
                None,
                None,
            ),
            pos(0, 0, 2, 1),
        ));
        widgets.push((
            "stat-counter",
            "Bids Submitted".to_string(),
            mk_config(&mk("bids_submitted"), "This quarter", None, None),
            pos(0, 2, 2, 1),
        ));
        widgets.push((
            "revenue-card",
            "Awarded Value".to_string(),
            mk_config(&mk("awarded"), "Total contract value", None, Some("USD")),
            pos(0, 4, 3, 1),
        ));
    } else {
        // Generic fallback — works for ANY industry not explicitly matched
        widgets.push((
            "stat-counter",
            "Active Items".to_string(),
            mk_config(&mk("active_items"), "Currently active", None, None),
            pos(0, 0, 2, 1),
        ));
        widgets.push((
            "stat-counter",
            "Completed".to_string(),
            mk_config(&mk("completed"), "Completed this period", None, None),
            pos(0, 2, 2, 1),
        ));
        widgets.push((
            "revenue-card",
            "Revenue".to_string(),
            mk_config(&mk("revenue"), "Total", None, Some("USD")),
            pos(0, 4, 3, 1),
        ));
    }

    // Row 1: Industry-specific secondary widgets
    if is_sales_pipeline {
        widgets.push((
            "stat-counter",
            "Follow-ups Due".to_string(),
            mk_config(&mk("followups_due"), "Tasks needing attention", None, None),
            pos(1, 0, 3, 1),
        ));
        widgets.push((
            "stat-counter",
            "Warm Outreach".to_string(),
            mk_config(
                &mk("warm_outreach"),
                "Emails / calls sent today",
                None,
                None,
            ),
            pos(1, 3, 4, 1),
        ));
    } else if is_project_tracker {
        widgets.push((
            "stat-counter",
            "Milestones Due".to_string(),
            mk_config(&mk("milestones_due"), "Upcoming deadlines", None, None),
            pos(1, 0, 3, 1),
        ));
        widgets.push((
            "connection-status",
            "Integrations".to_string(),
            mk_config(&mk("integrations"), "API connections status", None, None),
            pos(1, 3, 4, 1),
        ));
    } else if is_ecommerce {
        widgets.push((
            "stat-counter",
            "Abandoned Carts".to_string(),
            mk_config(&mk("abandoned_carts"), "Recovery opportunities", None, None),
            pos(1, 0, 3, 1),
        ));
        widgets.push((
            "stat-counter",
            "Avg Order Value".to_string(),
            mk_config(&mk("avg_order"), "Per transaction", None, Some("$")),
            pos(1, 3, 4, 1),
        ));
    } else if is_grant_funding {
        widgets.push((
            "stat-counter",
            "Applications Due".to_string(),
            mk_config(&mk("applications_due"), "Deadlines this month", None, None),
            pos(1, 0, 3, 1),
        ));
        widgets.push((
            "stat-counter",
            "Donors / Funders".to_string(),
            mk_config(&mk("donors"), "Active relationships", None, None),
            pos(1, 3, 4, 1),
        ));
    } else {
        // Generic secondary
        widgets.push((
            "stat-counter",
            "Pending Tasks".to_string(),
            mk_config(&mk("pending_tasks"), "Needing action", None, None),
            pos(1, 0, 3, 1),
        ));
        widgets.push((
            "stat-counter",
            "Today's Activity".to_string(),
            mk_config(&mk("today_activity"), "Actions taken today", None, None),
            pos(1, 3, 4, 1),
        ));
    }

    // Row 2: Activity feed (always present)
    widgets.push((
        "activity-feed",
        "Recent Activity".to_string(),
        mk_config(
            &mk("recent_activity"),
            "Latest events and updates",
            None,
            None,
        ),
        pos(2, 0, 7, 2),
    ));

    // Row 3: Chart widget for trends (always present)
    widgets.push((
        "chart-trend",
        "Trends".to_string(),
        mk_config(&mk("trends"), "Performance over time", None, None),
        pos(4, 0, 7, 2),
    ));

    widgets
}

fn mk_config(
    metric_key: &str,
    subtitle: &str,
    suffix: Option<&str>,
    currency: Option<&str>,
) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("metric_key".to_string(), json!(metric_key));
    m.insert("subtitle".to_string(), json!(subtitle));
    if let Some(s) = suffix {
        m.insert("suffix".to_string(), json!(s));
    }
    if let Some(c) = currency {
        m.insert("currency".to_string(), json!(c));
    }
    Value::Object(m)
}

fn pos(row: i32, col: i32, width: i32, height: i32) -> Value {
    json!({"row": row, "col": col, "width": width, "height": height})
}

/// GET /api/v1/dashboard/widgets — return widgets + current data for specified industry dashboard
/// Now accepts optional ?industry query param for multi-industry support
/// Defaults to the primary industry dashboard
pub async fn get_dashboard_widgets(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Determine which industry dashboard to load
    let industry_slug = match params.get("industry") {
        Some(slug) => {
            // Verify account actually has this industry
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM account_industries WHERE aid = $1 AND industry_slug = $2 AND is_active = true)"
            )
            .bind(aid)
            .bind(slug)
            .fetch_one(&state.db)
            .await
            .unwrap_or(false);

            if !exists {
                return Err(AppError::NotFound(format!(
                    "Industry '{}' not found for this account",
                    slug
                )));
            }
            slug.clone()
        }
        None => sqlx::query_scalar(
            "SELECT COALESCE(industry_slug, 'site-flipping') FROM accounts WHERE id = $1",
        )
        .bind(aid)
        .fetch_one(&state.db)
        .await
        .unwrap_or_default(),
    };

    // Use human-readable category name for dashboard name, matching template categories
    let industry_name: String =
        sqlx::query_scalar("SELECT name FROM template_categories WHERE slug = $1")
            .bind(&industry_slug)
            .fetch_optional(&state.db)
            .await?
            .unwrap_or_else(|| industry_slug.clone());

    let dashboard_name = format!("{} Dashboard", industry_name);

    let dashboard_id: Uuid = match sqlx::query_scalar(
        "SELECT id FROM dashboards WHERE aid = $1 AND name = $2 LIMIT 1",
    )
    .bind(aid)
    .bind(&dashboard_name)
    .fetch_optional(&state.db)
    .await?
    {
        Some(id) => id,
        None => {
            let id = Uuid::new_v4();
            sqlx::query(
                r#"INSERT INTO dashboards (id, aid, name, description)
                   VALUES ($1, $2, $3, $4)"#,
            )
            .bind(id)
            .bind(aid)
            .bind(&dashboard_name)
            .bind(format!("Your {} dashboard", industry_name))
            .execute(&state.db)
            .await?;
            seed_default_widgets_internal(&state, aid, id, &industry_slug).await;

            // Also ensure account_industries record exists
            sqlx::query(
                r#"INSERT INTO account_industries (aid, industry_slug, dashboard_id)
                   VALUES ($1, $2, $3)
                   ON CONFLICT (aid, industry_slug) DO UPDATE SET dashboard_id = EXCLUDED.dashboard_id"#
            )
            .bind(aid)
            .bind(&industry_slug)
            .bind(id)
            .execute(&state.db)
            .await?;

            id
        }
    };

    // Get widgets with manual row access
    let widget_rows = sqlx::query(
        r#"SELECT id::text as id_text, widget_type, title, config, position::text as pos_text
           FROM dashboard_widgets WHERE dashboard_id = $1
           ORDER BY (position->>'row')::int, (position->>'col')::int"#,
    )
    .bind(dashboard_id)
    .fetch_all(&state.db)
    .await?;

    let mut widget_list = Vec::new();
    for row in widget_rows {
        let id_text: String = row.try_get("id_text").unwrap_or_default();
        let widget_type: String = row.try_get("widget_type").unwrap_or_default();
        let title: String = row.try_get("title").unwrap_or_default();
        let config: serde_json::Value = row.try_get("config").unwrap_or(json!({}));
        let pos_text: String = row.try_get("pos_text").unwrap_or_default();

        let metric_key = config
            .get("metric_key")
            .and_then(|c| c.as_str())
            .unwrap_or("");

        // Fetch latest data for this widget's metric_key
        let data: Option<serde_json::Value> = sqlx::query_scalar(
            r#"SELECT metric_value FROM dashboard_data
               WHERE aid = $1 AND metric_key = $2
               ORDER BY recorded_at DESC LIMIT 1"#,
        )
        .bind(aid)
        .bind(format!("n8n_{}", metric_key))
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

        let position: serde_json::Value = if pos_text.is_empty() {
            json!({})
        } else {
            serde_json::from_str(&pos_text).unwrap_or(json!({}))
        };

        widget_list.push(json!({
            "id": id_text,
            "widget_type": widget_type,
            "title": title,
            "config": config,
            "position": position,
            "data": data.unwrap_or(json!(null))
        }));
    }

    let industry_name: String =
        sqlx::query_scalar("SELECT name FROM template_categories WHERE slug = $1")
            .bind(&industry_slug)
            .fetch_optional(&state.db)
            .await?
            .unwrap_or_else(|| industry_slug.clone());

    Ok(Json(json!({
        "dashboard_id": dashboard_id.to_string(),
        "industry_slug": industry_slug,
        "industry_name": industry_name,
        "widgets": widget_list
    })))
}

/// GET /api/v1/dashboard/data/{metric_key} — get specific metric data
pub async fn get_dashboard_metric(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(metric_key): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let data: Option<serde_json::Value> = sqlx::query_scalar(
        r#"SELECT metric_value FROM dashboard_data
           WHERE aid = $1 AND metric_key = $2
           ORDER BY recorded_at DESC LIMIT 1"#,
    )
    .bind(aid)
    .bind(&metric_key)
    .fetch_optional(&state.db)
    .await?;

    Ok(Json(json!({
        "metric_key": metric_key,
        "data": data.unwrap_or(json!(null))
    })))
}

/// POST /api/v1/dashboard/push-widget-data — push data for a specific widget
/// This is the universal endpoint used by n8n workflows, external API calls,
/// and browser automation scripts to push KPI data to the dashboard.
///
/// Accepts: { metric_key: string, value: number|object|array }
/// The metric_key must match a widget's config.metric_key for the data to display.
/// Data sources:
/// - n8n workflow results (POST from webhook node)
/// - External APIs (Stripe, Flippa, Google, etc. via n8n HTTP node)
/// - Browser automation (Playwright/Puppeteer scraping results)
/// Now accepts optional `industry_slug` to route data to the right dashboard
pub async fn push_widget_data(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let metric_key = req
        .get("metric_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Validation("metric_key required".to_string()))?;
    let metric_value = req
        .get("value")
        .ok_or_else(|| AppError::Validation("value required".to_string()))?;

    // If industry_slug specified, find that dashboard; otherwise use primary
    let dashboard_id: Uuid = if let Some(industry_slug) =
        req.get("industry_slug").and_then(|v| v.as_str())
    {
        // Look up the human-readable category name for dashboard name matching
        let industry_name: String =
            sqlx::query_scalar("SELECT name FROM template_categories WHERE slug = $1")
                .bind(industry_slug)
                .fetch_optional(&state.db)
                .await?
                .unwrap_or_else(|| industry_slug.to_string());

        let name = format!("{} Dashboard", industry_name);
        sqlx::query_scalar("SELECT id FROM dashboards WHERE aid = $1 AND name = $2 LIMIT 1")
            .bind(aid)
            .bind(&name)
            .fetch_optional(&state.db)
            .await?
            .unwrap_or_else(|| {
                // Auto-create if it doesn't exist
                let id = Uuid::new_v4();
                let _ = sqlx::query(
                r#"INSERT INTO dashboards (id, aid, name, description) VALUES ($1, $2, $3, $4)"#
            )
            .bind(id)
            .bind(aid)
            .bind(&name)
            .bind(format!("Your {} dashboard", industry_name))
            .execute(&state.db);
                id
            })
    } else {
        sqlx::query_scalar(
            "SELECT id FROM dashboards WHERE aid = $1 ORDER BY created_at DESC LIMIT 1"
        )
        .bind(aid)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_else(|| {
            let id = Uuid::new_v4();
            let _ = sqlx::query(
                r#"INSERT INTO dashboards (id, aid, name, description) VALUES ($1, $2, 'Default', 'Auto-created')"#
            )
            .bind(id)
            .bind(aid)
            .execute(&state.db);
            id
        })
    };

    let data_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO dashboard_data (id, dashboard_id, aid, metric_key, metric_value)
           VALUES ($1, $2, $3, $4, $5::jsonb)"#,
    )
    .bind(data_id)
    .bind(dashboard_id)
    .bind(aid)
    .bind(metric_key)
    .bind(metric_value)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "stored": true,
        "data_id": data_id.to_string(),
        "metric_key": metric_key
    })))
}
