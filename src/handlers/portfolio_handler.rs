use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::features;
use crate::AppState;
use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

pub async fn list_portfolio_companies(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    // `portfolio_companies.domain` — the company's own web domain. Column added in
    // migrations/058_portfolio_companies_domain.sql: the internal sync contract carries `domain`
    // (ADASwift's twin endpoint stores it on tenants/clients) and this handler already reads it,
    // but the table never had the column, so every sync 42703'd on the statement below.
    let rows = sqlx::query("SELECT id::text, name, slug, email, description, domain, settings::text, created_at::text FROM portfolio_companies WHERE aid = $1 ORDER BY name")
        .bind(aid)
        .fetch_all(&state.db).await?;
    let companies: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({"id": r.try_get::<&str,_>("id").unwrap_or(""), "name": r.try_get::<&str,_>("name").unwrap_or(""), "slug": r.try_get::<&str,_>("slug").unwrap_or(""), "email": r.try_get::<&str,_>("email").unwrap_or(""), "description": r.try_get::<&str,_>("description").unwrap_or(""), "domain": r.try_get::<&str,_>("domain").unwrap_or("")})
    }).collect();
    Ok(Json(json!({"portfolio_companies": companies})))
}

pub async fn create_portfolio_company(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    features::enforce_feature_limit(
        &state.db,
        aid,
        "max_portfolio_companys",
        "Portfolio Companys",
    )
    .await?;
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Company")
        .to_string();
    let slug = req
        .get("slug")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| name.to_lowercase().replace(' ', "-"));
    // The console's Add Company form collects a website, an email and a description. This handler
    // used to read only `name`/`slug`, so all three were dropped on the floor: the operator filled
    // them in, got a 201 and never learned they were never stored (the DB has the columns).
    let domain = req
        .get("domain")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let email = req
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = req
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO portfolio_companies (id, aid, name, slug, domain, email, description) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(id)
        .bind(aid)
        .bind(&name)
        .bind(&slug)
        .bind(&domain)
        .bind(&email)
        .bind(&description)
        .execute(&state.db)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"id": id.to_string(), "name": name, "slug": slug, "domain": domain, "email": email, "description": description}),
        ),
    ))
}

pub async fn get_portfolio_company(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let company_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid ID".into()))?;
    // The console's Portfolio panel shows the company's website/email/description; GET-by-id used
    // to return only id/name/slug, so a client that pre-filled an edit form from this route saw
    // three fields blank that the list route returns.
    let row = sqlx::query("SELECT id::text, name, slug, email, description, domain, settings::text, created_at::text FROM portfolio_companies WHERE id = $1 AND aid = $2")
        .bind(company_id).bind(aid)
        .fetch_optional(&state.db).await?
        .ok_or(AppError::NotFound("Company not found".into()))?;
    Ok(Json(
        json!({"id": row.try_get::<&str,_>("id").unwrap_or(""), "name": row.try_get::<&str,_>("name").unwrap_or(""), "slug": row.try_get::<&str,_>("slug").unwrap_or(""), "email": row.try_get::<&str,_>("email").unwrap_or(""), "description": row.try_get::<&str,_>("description").unwrap_or(""), "domain": row.try_get::<&str,_>("domain").unwrap_or("")}),
    ))
}

pub async fn update_portfolio_company(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let company_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid ID".into()))?;
    // One statement so `rows_affected` is the honest "did anything change" signal (and so a partial
    // write cannot leave `name` updated but `description` not). COALESCE keeps the fleet's convention
    // that an absent field is "not submitted" — the console skips blank inputs on purpose, so a blank
    // box can never clear a stored value. A PUT for an id this account does not own used to answer
    // 200 {"status":"updated"} while updating 0 rows; the console told the operator "Saved!" for a
    // company that was never touched.
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let slug = req
        .get("slug")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let domain = req
        .get("domain")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let email = req
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let description = req
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let result = sqlx::query(
        "UPDATE portfolio_companies SET name = COALESCE($1, name), slug = COALESCE($2, slug), \
         domain = COALESCE($3, domain), email = COALESCE($4, email), \
         description = COALESCE($5, description), updated_at = NOW() \
         WHERE id = $6 AND aid = $7",
    )
    .bind(&name)
    .bind(&slug)
    .bind(&domain)
    .bind(&email)
    .bind(&description)
    .bind(company_id)
    .bind(aid)
    .execute(&state.db)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Company not found".into()));
    }
    Ok(Json(json!({"status": "updated"})))
}

/// POST /api/v1/internal/portfolio-companies — internal sync, no JWT
pub async fn internal_create_portfolio_company(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let key = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // config.rs defaults INTERNAL_SYNC_KEY to "", and an unset key would then authenticate an
    // empty/absent x-internal-key header. Refuse when this server has no key configured.
    if state.config.internal_sync_key.is_empty() || key != state.config.internal_sync_key {
        return Err(AppError::Unauthorized);
    }

    // The account key is named `tenant_id` by the fleet's tenant-sync contract: FunnelSwift builds
    // ONE body and posts it to all five receivers, and ADASwift, MissedCall, CoreSwift-CRM and
    // IncentiveSwift all read `tenant_id` from it. This receiver used to require `aid` only, so the
    // WorkflowSwift leg of every tenant sync was answered 400 before any SQL ran and no
    // portfolio_companies row was ever created (kanban t_8b887f91). `aid` stays accepted as an
    // alias for callers that already speak it.
    let aid = ["tenant_id", "aid"]
        .iter()
        .find_map(|field| {
            body.get(*field)
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        })
        .ok_or_else(|| AppError::BadRequest("tenant_id (or aid) required".into()))?;

    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Company")
        .to_string();
    let slug = body
        .get("slug")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| name.to_lowercase().replace(' ', "-"));
    let email = body
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let domain = body
        .get("domain")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let id = Uuid::new_v4();

    // Ensure account exists (FK constraint).
    // NOT marked with `probe_harness` on purpose (kanban t_4a744754): an AUTHENTICATED
    // x-internal-key portfolio door, not the served, unauthenticated signup that the
    // `X-Swift-Harness` marker exists for.
    sqlx::query(
        "INSERT INTO accounts (id, name, account_slug) VALUES ($1, $2, CONCAT($3, '-', LEFT(CAST($1 AS TEXT), 8))) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, account_slug = EXCLUDED.account_slug"
    )
    .bind(aid)
    .bind(&name)
    .bind(&slug)
    .execute(&state.db)
    .await.ok();

    sqlx::query(
        "INSERT INTO portfolio_companies (id, aid, name, slug, email, description, domain) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, domain = EXCLUDED.domain"
    )
    .bind(id)
    .bind(aid)
    .bind(&name)
    .bind(&slug)
    .bind(&email)
    .bind(&description)
    .bind(&domain)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({"status": "synced", "id": id.to_string()})))
}

pub async fn delete_portfolio_company(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let company_id = Uuid::parse_str(&id).map_err(|_| AppError::BadRequest("Invalid ID".into()))?;
    let result = sqlx::query("DELETE FROM portfolio_companies WHERE id = $1 AND aid = $2")
        .bind(company_id)
        .bind(aid)
        .execute(&state.db)
        .await?;
    // "Deleted" for an id that was never there (or belongs to another account) told the operator the
    // row was gone when nothing happened.
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Company not found".into()));
    }
    Ok(Json(json!({"status": "deleted"})))
}
