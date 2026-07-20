use axum::{
    extract::{State, Json},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;
use sqlx::Row;

use crate::AppState;
use crate::error::{AppError, ApiResult};
use crate::auth::models::Claims;

/// GET /api/v1/user/workspaces
/// List all portfolio companies for the authenticated user's account.
/// These act as workspaces the user can switch between.
pub async fn list_user_workspaces(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let mut workspaces: Vec<serde_json::Value> = Vec::<serde_json::Value>::new();

    // Check if user has workspaces
    let count: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM portfolio_companies WHERE aid = $1"
    )
    .bind(aid)
    .fetch_one(&state.db)
    .await?;

    if count == 0 {
        // Auto-seed a default workspace
        let ws_id = Uuid::new_v4();
        let _ = sqlx::query(
            "INSERT INTO portfolio_companies (id, aid, name, slug) VALUES ($1, $2, 'Default Workspace', 'default')"
        )
        .bind(ws_id)
        .bind(aid)
        .execute(&state.db)
        .await;

        workspaces.push(json!({
            "id": ws_id.to_string(),
            "name": "Default Workspace",
            "slug": "default",
            "created_at": chrono::Utc::now().to_rfc3339()
        }));

        return Ok(Json(json!({"workspaces": workspaces})));
    }

    let rows = sqlx::query(
        r#"SELECT id::text, name, slug, created_at::text
           FROM portfolio_companies
           WHERE aid = $1
           ORDER BY name ASC"#,
    )
    .bind(aid)
    .fetch_all(&state.db)
    .await?;

    for r in rows {
        workspaces.push(json!({
            "id": r.try_get::<&str, _>("id").unwrap_or(""),
            "name": r.try_get::<&str, _>("name").unwrap_or(""),
            "slug": r.try_get::<&str, _>("slug").unwrap_or(""),
            "created_at": r.try_get::<&str, _>("created_at").unwrap_or(""),
        }));
    }

    Ok(Json(json!({"workspaces": workspaces})))
}

/// POST /api/v1/user/workspaces
/// Create a new workspace (portfolio company) for the user.
pub async fn create_user_workspace(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("name is required".into()))?;

    if name.trim().is_empty() {
        return Err(AppError::BadRequest("name must not be empty".into()));
    }

    // Generate slug from name
    let base_slug = name.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .trim_matches('-')
        .to_string();
    let slug = if base_slug.is_empty() { "workspace" } else { &base_slug };

    // Ensure slug is unique for this account
    let mut final_slug = slug.to_string();
    let mut counter = 1;
    loop {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM portfolio_companies WHERE aid = $1 AND slug = $2)"
        )
        .bind(aid)
        .bind(&final_slug)
        .fetch_one(&state.db)
        .await?;

        if !exists {
            break;
        }
        final_slug = format!("{}-{}", slug, counter);
        counter += 1;
    }

    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO portfolio_companies (id, aid, name, slug)
           VALUES ($1, $2, $3, $4)"#,
    )
    .bind(id)
    .bind(aid)
    .bind(name)
    .bind(&final_slug)
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "workspace": {
                "id": id.to_string(),
                "name": name,
                "slug": final_slug,
            }
        })),
    ))
}

/// DELETE /api/v1/user/workspaces/:id
pub async fn delete_user_workspace(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query(
        "DELETE FROM portfolio_companies WHERE id = $1 AND aid = $2",
    )
    .bind(id)
    .bind(aid)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Workspace not found".into()));
    }

    Ok(Json(json!({"status": "deleted"})))
}

/// GET /api/v1/user/workspaces/:id/stats
/// Get workspace-scoped stats (counts of workflows, clients, instances, automations)
pub async fn get_workspace_stats(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Verify workspace belongs to user's account
    let owned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM portfolio_companies WHERE id = $1 AND aid = $2)"
    )
    .bind(id)
    .bind(aid)
    .fetch_one(&state.db)
    .await?;

    if !owned {
        return Err(AppError::NotFound("Workspace not found".into()));
    }

    let workflow_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workflows WHERE aid = $1 AND portfolio_company_id = $2"
    )
    .bind(aid).bind(id)
    .fetch_one(&state.db).await.unwrap_or(0);

    let client_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM clients WHERE aid = $1 AND portfolio_company_id = $2 AND is_active = true"
    )
    .bind(aid).bind(id)
    .fetch_one(&state.db).await.unwrap_or(0);

    let instance_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workflow_instances WHERE aid = $1 AND portfolio_company_id = $2"
    )
    .bind(aid).bind(id)
    .fetch_one(&state.db).await.unwrap_or(0);

    let automation_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM automations WHERE aid = $1 AND portfolio_company_id = $2"
    )
    .bind(aid).bind(id)
    .fetch_one(&state.db).await.unwrap_or(0);

    Ok(Json(json!({
        "stats": {
            "workflows": workflow_count,
            "clients": client_count,
            "instances": instance_count,
            "automations": automation_count,
        }
    })))
}
