use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateWorkspace {
    pub name: String,
}

#[derive(Deserialize)]
pub struct RenameWorkspace {
    pub name: String,
}

fn tenant(claims: &Claims) -> Result<Uuid, AppError> {
    Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::UnauthorizedWith("Invalid session".to_string()))
}

fn clean_name(raw: &str) -> Result<String, AppError> {
    let n = raw.trim();
    if n.is_empty() {
        return Err(AppError::Validation("A workspace needs a name.".into()));
    }
    if n.chars().count() > 120 {
        return Err(AppError::Validation(
            "That name is too long (120 characters maximum).".into(),
        ));
    }
    Ok(n.to_string())
}

fn row_json(r: &sqlx::postgres::PgRow) -> serde_json::Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "name": r.get::<String, _>("name"),
        "is_default": r.get::<bool, _>("is_default"),
        // Kept on the wire because the operator console's generic Workspaces panel renders a
        // `created_at` column; a missing key would paint a blank cell there.
        "created_at": r.try_get::<Option<String>, _>("created_at").unwrap_or(None),
    })
}

/// GET /api/workspaces
pub async fn list_workspaces(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = tenant(&claims)?;
    let rows = sqlx::query(
        "SELECT id, name, is_default, created_at::text AS created_at FROM workspaces \
          WHERE tenant_id = $1 ORDER BY is_default DESC, lower(name)",
    )
    .bind(tid)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    let items: Vec<serde_json::Value> = rows.iter().map(row_json).collect();
    Ok(Json(json!({ "count": items.len(), "workspaces": items })))
}

/// POST /api/workspaces
pub async fn create_workspace(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateWorkspace>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = tenant(&claims)?;
    let name = clean_name(&req.name)?;

    // Plan ceiling, when the plan states one. This app sells its allowances in
    // `plan_tiers.features` (JSONB) reached through `account_plans`; a plan that names no
    // `max_workspaces` is uncapped, because a ceiling exists only where one was sold.
    let ceiling: Option<i64> = sqlx::query_scalar(
        "SELECT NULLIF(t.features->>'max_workspaces','')::bigint
           FROM account_plans ap
           JOIN plan_tiers t ON t.id = ap.plan_id
          WHERE ap.aid = $1 AND ap.status = 'active' AND t.is_active
          ORDER BY t.sort_order DESC
          LIMIT 1",
    )
    .bind(tid)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    // A plan whose blob names no `max_workspaces` returns SQL NULL — `fetch_optional` gives
    // `Some(None)` for that, and the flattened value is the "no ceiling sold" answer.
    .flatten();

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspaces WHERE tenant_id = $1")
        .bind(tid)
        .fetch_one(&state.db)
        .await
        .map_err(AppError::Database)?;

    if let Some(cap) = ceiling {
        if count >= cap {
            return Err(AppError::UpgradeRequired(format!(
                "Your plan includes {cap} workspace(s). Upgrade to add more."
            )));
        }
    }

    let dup: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM workspaces WHERE tenant_id = $1 AND lower(name) = lower($2)",
    )
    .bind(tid)
    .bind(&name)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;
    if dup.is_some() {
        return Err(AppError::Duplicate(
            "You already have a workspace with that name.".into(),
        ));
    }

    // the very first workspace is always the default
    let make_default = count == 0;
    let row = sqlx::query(
        "INSERT INTO workspaces (tenant_id, name, is_default) VALUES ($1, $2, $3) \
         RETURNING id, name, is_default, created_at::text AS created_at",
    )
    .bind(tid)
    .bind(&name)
    .bind(make_default)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(json!({ "workspace": row_json(&row) })))
}

/// PATCH /api/workspaces/{id}  (rename)
pub async fn rename_workspace(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<RenameWorkspace>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = tenant(&claims)?;
    let name = clean_name(&req.name)?;

    let dup: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM workspaces WHERE tenant_id = $1 AND lower(name) = lower($2) AND id <> $3",
    )
    .bind(tid)
    .bind(&name)
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;
    if dup.is_some() {
        return Err(AppError::Duplicate(
            "You already have a workspace with that name.".into(),
        ));
    }

    let row = sqlx::query(
        "UPDATE workspaces SET name = $1, updated_at = now() WHERE id = $2 AND tenant_id = $3 \
         RETURNING id, name, is_default",
    )
    .bind(&name)
    .bind(id)
    .bind(tid)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("Workspace not found.".into()))?;

    Ok(Json(json!({ "workspace": row_json(&row) })))
}

/// POST /api/workspaces/{id}/default
pub async fn set_default_workspace(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = tenant(&claims)?;

    let exists: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM workspaces WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(tid)
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::Database)?;
    if exists.is_none() {
        return Err(AppError::NotFound("Workspace not found.".into()));
    }

    let mut tx = state.db.begin().await.map_err(AppError::Database)?;
    sqlx::query("UPDATE workspaces SET is_default = false, updated_at = now() WHERE tenant_id = $1 AND is_default")
        .bind(tid)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;
    sqlx::query("UPDATE workspaces SET is_default = true, updated_at = now() WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tid)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;
    tx.commit().await.map_err(AppError::Database)?;

    Ok(Json(json!({ "ok": true, "default_workspace_id": id })))
}

/// DELETE /api/workspaces/{id}
pub async fn delete_workspace(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = tenant(&claims)?;

    let rows = sqlx::query("SELECT id, is_default FROM workspaces WHERE tenant_id = $1")
        .bind(tid)
        .fetch_all(&state.db)
        .await
        .map_err(AppError::Database)?;

    if rows.len() <= 1 {
        return Err(AppError::BadRequest(
            "An account must always keep at least one workspace.".into(),
        ));
    }

    let target = rows
        .iter()
        .find(|r| r.get::<Uuid, _>("id") == id)
        .ok_or_else(|| AppError::NotFound("Workspace not found.".into()))?;
    let was_default = target.get::<bool, _>("is_default");

    let mut tx = state.db.begin().await.map_err(AppError::Database)?;
    sqlx::query("DELETE FROM workspaces WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tid)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;

    // never leave the account without a default
    if was_default {
        sqlx::query(
            "UPDATE workspaces SET is_default = true, updated_at = now() \
             WHERE id = (SELECT id FROM workspaces WHERE tenant_id = $1 ORDER BY created_at LIMIT 1)",
        )
        .bind(tid)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;
    }
    tx.commit().await.map_err(AppError::Database)?;

    Ok(Json(json!({ "ok": true, "deleted": id })))
}
