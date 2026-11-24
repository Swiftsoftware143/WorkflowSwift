use axum::{
    extract::{Path, State},
    Extension, Json,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::auth::middleware::create_token;
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

/// May this session rename / delete / re-point the default workspace? The SAME rule the team
/// roster uses (`handlers::user_handler::is_tenant_admin`): the account owner (`user`), a tenant
/// `company_admin`, and the platform operators (`admin` / `super_admin` / `agency_admin`). A plain
/// member (`team_member`) may only switch and add — never rename, delete or change the default.
fn may_manage(claims: &Claims) -> bool {
    matches!(
        claims.role.as_str(),
        "user" | "company_admin" | "admin" | "super_admin" | "agency_admin"
    )
}

/// The portfolio businesses (ZaarHub, SwiftImpact, Giraudy Capital, SwiftSoftware) always keep
/// their account, whatever their workspace count (David's standing rule: the sister companies
/// always keep their account). Matched on the account's own name and slug.
fn protected_marker(name: &str, slug: &str) -> Option<&'static str> {
    const MARKERS: &[&str] = &[
        "workflowswift",
        "swiftsoftware",
        "swiftimpact",
        "zaarhub",
        "giraudy",
    ];
    let hay = format!("{name} {slug}").to_ascii_lowercase();
    MARKERS.iter().copied().find(|m| hay.contains(m))
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

    // ONE PRESS AT A TIME PER ACCOUNT (cron 2026-11-18). The ceiling count, the duplicate-name
    // probe and the INSERT are three statements, so two overlapping presses — a double-click on
    // "Create workspace", or a create racing a rename — both read dup = none and the loser hits
    // `ux_workspaces_tenant_name`, which surfaced to the customer as HTTP 500 "Database error".
    // The tenant-scoped advisory lock makes the three reads and the write one critical section, so
    // the second press sees the first row and gets the ordinary 409 ("you already have a workspace
    // with that name"), and the plan ceiling can no longer be beaten by parallelism. It is the
    // SAME lock `set_default_workspace` and `delete_workspace` already take, and it is taken FIRST
    // for the deadlock reason the delete documents: a press that holds the lock must never wait on
    // a row this transaction would then hold. Measured live before the fix (pre-fix binary, this
    // app): with the tenant lock held by another session a create still answered in 0.01s, i.e. it
    // never took the lock — the three statements above ran unprotected.
    let mut tx = state.db.begin().await.map_err(AppError::Database)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1::text))")
        .bind(tid.to_string())
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;

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
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::Database)?
    // A plan whose blob names no `max_workspaces` returns SQL NULL — `fetch_optional` gives
    // `Some(None)` for that, and the flattened value is the "no ceiling sold" answer.
    .flatten();

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM workspaces WHERE tenant_id = $1")
        .bind(tid)
        .fetch_one(&mut *tx)
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
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::Database)?;
    if dup.is_some() {
        return Err(AppError::Duplicate(
            "You already have a workspace with that name.".into(),
        ));
    }

    // the very first workspace is always the default
    let make_default = count == 0;
    let inserted = sqlx::query(
        "INSERT INTO workspaces (tenant_id, name, is_default) VALUES ($1, $2, $3) \
         RETURNING id, name, is_default, created_at::text AS created_at",
    )
    .bind(tid)
    .bind(&name)
    .bind(make_default)
    .fetch_one(&mut *tx)
    .await;
    // THE BELT: any path that reaches this INSERT with a taken name (the lock only covers the
    // presses that take it) answers the same readable 409 its own pre-check does, never a raw
    // database error. Same arm `rename_workspace` / `set_default_workspace` / `delete_workspace`
    // carry for their own constraints.
    let row = match inserted {
        Ok(row) => row,
        Err(sqlx::Error::Database(db)) if db.constraint() == Some("ux_workspaces_tenant_name") => {
            return Err(AppError::Duplicate(
                "You already have a workspace with that name.".into(),
            ));
        }
        Err(e) => return Err(AppError::Database(e)),
    };

    tx.commit().await.map_err(AppError::Database)?;

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
    if !may_manage(&claims) {
        return Err(AppError::Forbidden(
            "Only the workspace owner or an admin can rename a workspace.".into(),
        ));
    }
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

    let updated = sqlx::query(
        "UPDATE workspaces SET name = $1, updated_at = now() WHERE id = $2 AND tenant_id = $3 \
         RETURNING id, name, is_default, created_at::text AS created_at",
    )
    .bind(&name)
    .bind(id)
    .bind(tid)
    .fetch_optional(&state.db)
    .await;
    // TWO OVERLAPPING RENAMES ARE ONE ANSWER, NOT A DATABASE ERROR (cron 2026-11-18). The
    // duplicate-name probe above and this UPDATE are two statements, so two presses that overlap —
    // a double-click on Save, or a rename racing a workspace CREATE — both read `dup = none`, both
    // UPDATE, and the loser hit `ux_workspaces_tenant_name` as a raw `AppError::Database`, i.e.
    // HTTP 500 "Database error" for a customer who only saved twice. The create route now closes
    // its half of this race with the tenant advisory lock; this answers the collision the way its
    // own pre-check does — a readable 409 — so no request can produce a 500 from a taken name.
    let row = match updated {
        Ok(row) => row,
        Err(sqlx::Error::Database(db)) if db.constraint() == Some("ux_workspaces_tenant_name") => {
            return Err(AppError::Duplicate(
                "You already have a workspace with that name.".into(),
            ));
        }
        Err(e) => return Err(AppError::Database(e)),
    }
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
    if !may_manage(&claims) {
        return Err(AppError::Forbidden(
            "Only the workspace owner or an admin can change the default workspace.".into(),
        ));
    }

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

    // ONE PRESS AT A TIME PER ACCOUNT (kanban t_6ae34893). The clear and the set are two separate
    // statements, so two overlapping presses aimed at DIFFERENT workspaces both clear the old
    // default and then both set their own — and the partial unique index
    // `ux_workspaces_one_default` (one default workspace per tenant, migration 105) refuses the
    // loser. Measured live on the pre-fix binary: 24 simultaneous presses answered 2 x HTTP 500
    // "Database error", which is exactly what a double-click on the star showed the customer. A
    // tenant-scoped advisory lock makes the clear+set pair one critical section, so overlapping
    // presses serialise and every one of them answers 200. The constraint arm below is the belt:
    // any future path reaching this UPDATE without the lock answers a readable 409 instead of a
    // raw database error.
    let mut tx = state.db.begin().await.map_err(AppError::Database)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1::text))")
        .bind(tid.to_string())
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;
    sqlx::query("UPDATE workspaces SET is_default = false, updated_at = now() WHERE tenant_id = $1 AND is_default")
        .bind(tid)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;
    let promoted = sqlx::query("UPDATE workspaces SET is_default = true, updated_at = now() WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tid)
        .execute(&mut *tx)
        .await;
    match promoted {
        Ok(_) => {}
        Err(sqlx::Error::Database(db)) if db.constraint() == Some("ux_workspaces_one_default") => {
            return Err(AppError::Duplicate(
                "Another workspace was made the default at the same moment. Try again.".into(),
            ));
        }
        Err(e) => return Err(AppError::Database(e)),
    }
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
    if !may_manage(&claims) {
        return Err(AppError::Forbidden(
            "Only the workspace owner or an admin can delete a workspace.".into(),
        ));
    }

    // Portfolio businesses are never removed, whatever their workspace count (David's standing
    // rule: the sister companies always keep their account). Matched on the account's own name and
    // slug — the SAME marker set the tenant purge door uses.
    let (aname, aslug): (String, String) =
        sqlx::query_as("SELECT name, account_slug FROM accounts WHERE id = $1")
            .bind(tid)
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::Database)?
            .ok_or_else(|| AppError::NotFound("Workspace not found.".into()))?;
    if let Some(marker) = protected_marker(&aname, &aslug) {
        return Err(AppError::Duplicate(format!(
            "This is a portfolio business ('{marker}') and its workspaces cannot be deleted."
        )));
    }

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

    // Refuse deleting the workspace the caller is signed into: the session's `wsid`, or — for a
    // token minted before the switcher shipped — the account's default. Deleting the floor you are
    // standing on would strand the session; switch away first.
    let signed_in = claims
        .wsid
        .as_deref()
        .and_then(|s| Uuid::parse_str(s).ok())
        .filter(|w| rows.iter().any(|r| r.get::<Uuid, _>("id") == *w))
        .or_else(|| {
            rows.iter()
                .find(|r| r.get::<bool, _>("is_default"))
                .map(|r| r.get::<Uuid, _>("id"))
        });
    if signed_in == Some(id) {
        return Err(AppError::Duplicate(
            "You are signed into this workspace. Switch to a different workspace before deleting it."
                .into(),
        ));
    }

    let mut tx = state.db.begin().await.map_err(AppError::Database)?;
    // The SAME tenant-scoped lock `set_default_workspace` takes, and taken FIRST so both writers
    // acquire it before touching workspace rows — the other order lets a press hold the advisory
    // lock while waiting for this row, and this delete hold the row while waiting for the lock (a
    // deadlock the customer would see as a 500). Without it, the replacement-default UPDATE below
    // is a second, unlocked writer of the one-default rule and can race a press into
    // `ux_workspaces_one_default`. Measured live before the fix: with the tenant lock held by
    // another session a delete answered in 0.01s, i.e. it never took it.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1::text))")
        .bind(tid.to_string())
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;
    sqlx::query("DELETE FROM workspaces WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tid)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;

    // never leave the account without a default
    if was_default {
        let promoted = sqlx::query(
            "UPDATE workspaces SET is_default = true, updated_at = now() \
             WHERE id = (SELECT id FROM workspaces WHERE tenant_id = $1 ORDER BY created_at LIMIT 1)",
        )
        .bind(tid)
        .execute(&mut *tx)
        .await;
        match promoted {
            Ok(_) => {}
            Err(sqlx::Error::Database(db))
                if db.constraint() == Some("ux_workspaces_one_default") =>
            {
                return Err(AppError::Duplicate(
                    "Another workspace was made the default at the same moment. Try again.".into(),
                ));
            }
            Err(e) => return Err(AppError::Database(e)),
        }
    }
    tx.commit().await.map_err(AppError::Database)?;

    Ok(Json(json!({ "ok": true, "deleted": id })))
}

/// POST /api/workspaces/{id}/switch — move this session's active workspace.
///
/// The switcher re-mints the session token with `wsid` set to the target workspace, reusing the
/// SAME `create_token` the login/register handlers use — no second auth route. The caller may only
/// switch to a workspace inside their OWN account (`aid`); any other id answers 404, so the door
/// cannot leak which accounts exist. The session keeps its subject, role and account — only the
/// workspace the console shows changes.
pub async fn switch_workspace(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = tenant(&claims)?;

    let row = sqlx::query(
        "SELECT id, name, is_default, created_at::text AS created_at FROM workspaces \
          WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tid)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("Workspace not found.".into()))?;

    let now = Utc::now().timestamp() as usize;
    let access_claims = Claims {
        sub: claims.sub.clone(),
        aid: claims.aid.clone(),
        role: claims.role.clone(),
        exp: now + state.config.jwt_access_expiry as usize,
        iat: now,
        perm_is_super_admin: claims.perm_is_super_admin,
        pwd_minted: false,
        wsid: Some(id.to_string()),
        via_api_key: false,
    };
    let access_token = create_token(&access_claims, &state.config.jwt_secret)?;

    let refresh_claims = Claims {
        sub: claims.sub.clone(),
        aid: claims.aid.clone(),
        role: claims.role.clone(),
        exp: now + state.config.jwt_refresh_expiry as usize,
        iat: now,
        perm_is_super_admin: claims.perm_is_super_admin,
        pwd_minted: false,
        wsid: Some(id.to_string()),
        via_api_key: false,
    };
    let refresh_token = create_token(&refresh_claims, &state.config.jwt_secret)?;

    Ok(Json(json!({
        "token": access_token,
        "refresh_token": refresh_token,
        "workspace": row_json(&row),
    })))
}
