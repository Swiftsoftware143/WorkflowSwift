//! REST twins for the two WorkflowSwift entities that had a read tool and no REST write path
//! (kanban `t_bb4d4d33`): `clients` and `dashboards`.
//!
//! The MCP door writes these rows; a company CEO told to "use the API" must be able to do the same
//! over HTTP with the same credential and the same scoping. So every handler here:
//!
//! * takes the caller's tenant from `Claims.aid` only — the tenant is never read from the body or
//!   the path;
//! * puts that tenant in the WHERE clause of every statement, so a foreign id is a zero-row
//!   select/update and answers **404**, never 403 (do not confirm existence to a stranger);
//! * archives (`is_active = false`) rather than deleting; the MCP door's `hard` flag is the only
//!   path to a real delete and it is audited there;
//! * binds only the columns the entity owns — `plan_id` is a platform/billing column and is never
//!   settable through this surface.
//!
//! The credential is the ordinary `/api/v1` one, so a `workflowswift_` API key and a console JWT
//! both work: `auth::auth_middleware` resolves either into the same `Claims`.

use axum::{
    extract::{Json, Path, State},
    response::IntoResponse,
    Extension,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::AppState;

const CLIENT_NAME_CHARS: usize = 255;
const CLIENT_EMAIL_CHARS: usize = 255;
const CLIENT_PHONE_CHARS: usize = 50;
const CLIENT_INDUSTRY_CHARS: usize = 100;
const DASHBOARD_NAME_CHARS: usize = 255;

fn tenant(claims: &Claims) -> ApiResult<Uuid> {
    Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)
}

fn text_arg(req: &Value, key: &str) -> Option<String> {
    req.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn capped(value: Option<String>, max: usize, field: &str) -> ApiResult<Option<String>> {
    match value {
        Some(v) if v.chars().count() > max => Err(AppError::Validation(format!(
            "{field} must be {max} characters or fewer"
        ))),
        other => Ok(other),
    }
}

fn required_name(value: Option<String>, max: usize, field: &str) -> ApiResult<String> {
    let Some(v) = value else {
        return Err(AppError::Validation(format!("{field} is required")));
    };
    if v.chars().count() > max {
        return Err(AppError::Validation(format!(
            "{field} must be {max} characters or fewer"
        )));
    }
    Ok(v)
}

// ── clients ─────────────────────────────────────────────────────────────────────────────────────

pub async fn list_clients(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = tenant(&claims)?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "select json_build_object( \
             'id', c.id, 'name', c.name, 'email', c.email, 'phone', c.phone, \
             'website', c.website, 'industry', c.industry, 'notes', c.notes, \
             'portfolio_company_id', c.portfolio_company_id, 'sort_order', c.sort_order, \
             'is_active', c.is_active, 'created_at', c.created_at, 'updated_at', c.updated_at) \
           from clients c \
          where c.aid = $1 \
          order by c.sort_order nulls last, c.name",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({ "clients": rows })))
}

pub async fn get_client(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = tenant(&claims)?;
    let row: Option<Value> = sqlx::query_scalar(
        "select json_build_object( \
             'id', c.id, 'name', c.name, 'email', c.email, 'phone', c.phone, \
             'website', c.website, 'industry', c.industry, 'notes', c.notes, \
             'portfolio_company_id', c.portfolio_company_id, 'sort_order', c.sort_order, \
             'is_active', c.is_active, 'created_at', c.created_at, 'updated_at', c.updated_at) \
           from clients c where c.id = $1 and c.aid = $2",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?;
    match row {
        Some(client) => Ok(Json(json!({ "client": client }))),
        None => Err(AppError::NotFound("Client not found".to_string())),
    }
}

pub async fn create_client(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = tenant(&claims)?;
    let name = required_name(text_arg(&req, "name"), CLIENT_NAME_CHARS, "name")?;
    let email = capped(text_arg(&req, "email"), CLIENT_EMAIL_CHARS, "email")?;
    let phone = capped(text_arg(&req, "phone"), CLIENT_PHONE_CHARS, "phone")?;
    let industry = capped(
        text_arg(&req, "industry"),
        CLIENT_INDUSTRY_CHARS,
        "industry",
    )?;
    let website = text_arg(&req, "website");
    let notes = text_arg(&req, "notes");
    let portfolio_company_id = match req.get("portfolio_company_id").and_then(|v| v.as_str()) {
        Some(raw) => Some(
            Uuid::parse_str(raw)
                .map_err(|_| AppError::Validation("portfolio_company_id is not a uuid".into()))?,
        ),
        None => None,
    };
    let sort_order = req.get("sort_order").and_then(|v| v.as_i64());

    let row: Value = sqlx::query_scalar(
        "insert into clients (id, aid, name, email, phone, website, industry, notes, \
                              portfolio_company_id, sort_order, is_active, created_at, updated_at) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, true, now(), now()) \
         returning json_build_object( \
             'id', id, 'name', name, 'email', email, 'phone', phone, 'website', website, \
             'industry', industry, 'notes', notes, 'portfolio_company_id', portfolio_company_id, \
             'sort_order', sort_order, 'is_active', is_active, 'created_at', created_at, \
             'updated_at', updated_at)",
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(name)
    .bind(email)
    .bind(phone)
    .bind(website)
    .bind(industry)
    .bind(notes)
    .bind(portfolio_company_id)
    .bind(sort_order)
    .fetch_one(&state.db)
    .await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({ "client": row })),
    ))
}

pub async fn update_client(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = tenant(&claims)?;
    // Merge in Rust, then ONE fixed UPDATE: no statement is built at run time (gate rule 5d).
    let current: Option<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<Uuid>,
        Option<i32>,
    )> = sqlx::query_as(
        "select name, email, phone, website, industry, notes, portfolio_company_id, sort_order \
           from clients where id = $1 and aid = $2",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?;
    let Some((cur_name, cur_email, cur_phone, cur_site, cur_industry, cur_notes, cur_pc, cur_sort)) =
        current
    else {
        return Err(AppError::NotFound("Client not found".to_string()));
    };

    let name = match text_arg(&req, "name") {
        Some(v) => required_name(Some(v), CLIENT_NAME_CHARS, "name")?,
        None => cur_name,
    };
    let email = match req.get("email") {
        Some(_) => capped(text_arg(&req, "email"), CLIENT_EMAIL_CHARS, "email")?,
        None => cur_email,
    };
    let phone = match req.get("phone") {
        Some(_) => capped(text_arg(&req, "phone"), CLIENT_PHONE_CHARS, "phone")?,
        None => cur_phone,
    };
    let website = match req.get("website") {
        Some(_) => text_arg(&req, "website"),
        None => cur_site,
    };
    let industry = match req.get("industry") {
        Some(_) => capped(
            text_arg(&req, "industry"),
            CLIENT_INDUSTRY_CHARS,
            "industry",
        )?,
        None => cur_industry,
    };
    let notes = match req.get("notes") {
        Some(_) => text_arg(&req, "notes"),
        None => cur_notes,
    };
    let portfolio_company_id = match req.get("portfolio_company_id") {
        Some(Value::String(raw)) => Some(
            Uuid::parse_str(raw)
                .map_err(|_| AppError::Validation("portfolio_company_id is not a uuid".into()))?,
        ),
        Some(Value::Null) => None,
        _ => cur_pc,
    };
    let sort_order = req
        .get("sort_order")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
        .or(cur_sort);

    let row: Value = sqlx::query_scalar(
        "update clients \
            set name = $3, email = $4, phone = $5, website = $6, industry = $7, notes = $8, \
                portfolio_company_id = $9, sort_order = $10, updated_at = now() \
          where id = $1 and aid = $2 \
          returning json_build_object( \
             'id', id, 'name', name, 'email', email, 'phone', phone, 'website', website, \
             'industry', industry, 'notes', notes, 'portfolio_company_id', portfolio_company_id, \
             'sort_order', sort_order, 'is_active', is_active, 'created_at', created_at, \
             'updated_at', updated_at)",
    )
    .bind(id)
    .bind(aid)
    .bind(name)
    .bind(email)
    .bind(phone)
    .bind(website)
    .bind(industry)
    .bind(notes)
    .bind(portfolio_company_id)
    .bind(sort_order)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({ "client": row })))
}

pub async fn delete_client(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = tenant(&claims)?;
    let affected = sqlx::query(
        "update clients set is_active = false, updated_at = now() where id = $1 and aid = $2",
    )
    .bind(id)
    .bind(aid)
    .execute(&state.db)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound("Client not found".to_string()));
    }
    Ok(Json(json!({ "ok": true, "id": id, "archived": true })))
}

// ── dashboards ──────────────────────────────────────────────────────────────────────────────────

pub async fn list_dashboards(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = tenant(&claims)?;
    let rows: Vec<Value> = sqlx::query_scalar(
        "select json_build_object( \
             'id', d.id, 'name', d.name, 'description', d.description, 'layout', d.layout, \
             'sort_order', d.sort_order, 'is_active', d.is_active, \
             'created_at', d.created_at, 'updated_at', d.updated_at) \
           from dashboards d where d.aid = $1 \
          order by d.sort_order nulls last, d.name",
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({ "dashboards": rows })))
}

pub async fn get_dashboard(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = tenant(&claims)?;
    let row: Option<Value> = sqlx::query_scalar(
        "select json_build_object( \
             'id', d.id, 'name', d.name, 'description', d.description, 'layout', d.layout, \
             'sort_order', d.sort_order, 'is_active', d.is_active, \
             'created_at', d.created_at, 'updated_at', d.updated_at) \
           from dashboards d where d.id = $1 and d.aid = $2",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?;
    match row {
        Some(dashboard) => Ok(Json(json!({ "dashboard": dashboard }))),
        None => Err(AppError::NotFound("Dashboard not found".to_string())),
    }
}

pub async fn create_dashboard(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = tenant(&claims)?;
    let name = required_name(text_arg(&req, "name"), DASHBOARD_NAME_CHARS, "name")?;
    let description = text_arg(&req, "description");
    let layout = req.get("layout").cloned().unwrap_or(Value::Null);
    let layout = if layout.is_null() { None } else { Some(layout) };
    let sort_order = req.get("sort_order").and_then(|v| v.as_i64());

    let row: Value = sqlx::query_scalar(
        "insert into dashboards (id, aid, name, description, layout, sort_order, is_active, \
                                 created_at, updated_at) \
         values ($1, $2, $3, $4, $5, $6, true, now(), now()) \
         returning json_build_object( \
             'id', id, 'name', name, 'description', description, 'layout', layout, \
             'sort_order', sort_order, 'is_active', is_active, \
             'created_at', created_at, 'updated_at', updated_at)",
    )
    .bind(Uuid::new_v4())
    .bind(aid)
    .bind(name)
    .bind(description)
    .bind(layout)
    .bind(sort_order)
    .fetch_one(&state.db)
    .await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({ "dashboard": row })),
    ))
}

pub async fn update_dashboard(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = tenant(&claims)?;
    let current: Option<(String, Option<String>, Option<Value>, Option<i32>)> = sqlx::query_as(
        "select name, description, layout, sort_order from dashboards where id = $1 and aid = $2",
    )
    .bind(id)
    .bind(aid)
    .fetch_optional(&state.db)
    .await?;
    let Some((cur_name, cur_desc, cur_layout, cur_sort)) = current else {
        return Err(AppError::NotFound("Dashboard not found".to_string()));
    };

    let name = match text_arg(&req, "name") {
        Some(v) => required_name(Some(v), DASHBOARD_NAME_CHARS, "name")?,
        None => cur_name,
    };
    let description = match req.get("description") {
        Some(_) => text_arg(&req, "description"),
        None => cur_desc,
    };
    let layout = match req.get("layout") {
        Some(Value::Null) => None,
        Some(v) => Some(v.clone()),
        None => cur_layout,
    };
    let sort_order = req
        .get("sort_order")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
        .or(cur_sort);

    let row: Value = sqlx::query_scalar(
        "update dashboards set name = $3, description = $4, layout = $5, sort_order = $6, \
                              updated_at = now() \
          where id = $1 and aid = $2 \
          returning json_build_object( \
             'id', id, 'name', name, 'description', description, 'layout', layout, \
             'sort_order', sort_order, 'is_active', is_active, \
             'created_at', created_at, 'updated_at', updated_at)",
    )
    .bind(id)
    .bind(aid)
    .bind(name)
    .bind(description)
    .bind(layout)
    .bind(sort_order)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({ "dashboard": row })))
}

pub async fn delete_dashboard(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = tenant(&claims)?;
    let affected = sqlx::query(
        "update dashboards set is_active = false, updated_at = now() where id = $1 and aid = $2",
    )
    .bind(id)
    .bind(aid)
    .execute(&state.db)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound("Dashboard not found".to_string()));
    }
    Ok(Json(json!({ "ok": true, "id": id, "archived": true })))
}
