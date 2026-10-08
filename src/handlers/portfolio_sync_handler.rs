//! Internal portfolio sync handler — receives broadcasts from CoreSwift CRM.
//! Protected by x-internal-key header, not JWT.

use crate::{
    error::{ApiResult, AppError},
    AppState,
};
use axum::{extract::State, http::HeaderMap, response::IntoResponse, Json};
use serde_json::{json, Value};
use uuid::Uuid;

/// The `account_slug` a mirrored `accounts` row must carry: the caller's own slug (the hub's
/// contract is that `body.slug` is what lands in both rows), or this app's derived unique slug
/// when the caller sent none — ONE rule, so the arms of this door cannot drift apart
/// (kanban t_1a26f923 / t_ff66fbe3).
async fn resolve_account_slug(
    db: &sqlx::PgPool,
    name: &str,
    slug: &str,
) -> Result<String, AppError> {
    if slug.trim().is_empty() {
        crate::auth::signup::unique_account_slug(db, name).await
    } else {
        Ok(slug.to_string())
    }
}

/// Mirror the caller's account into THIS app's `accounts` table.
///
/// `portfolio_companies.aid` carries `portfolio_companies_tenant_id_fkey`
/// (REFERENCES accounts(id) ON DELETE CASCADE — read live with pg_constraint), so the parent row
/// has to exist HERE before any company row can be written. A caller-supplied id with no local
/// parent made the company INSERT raise 23503 and the caller got an opaque
/// `500 {"error":"Database error"}` that named nothing (kanban t_898f32a8, measured live at
/// 127.0.0.1:8085 on 2026-10-08). This is the same mirror the fleet uses on the twin receivers
/// (IncentiveSwift `ensure_account`, kanban t_b5784899 / t_95866a2c; MissedCall Respondr's
/// create+update arms, kanban t_8cbdbf2d).
///
/// Idempotent on the caller's id — the hub's id IS the mirror's id — and a slug that already
/// belongs to another account is refused with a 409 that NAMES it, never silently replaced.
async fn mirror_account(
    tx: &mut sqlx::PgConnection,
    aid: Uuid,
    name: &str,
    slug: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO accounts (id, name, account_slug) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING",
    )
    .bind(aid)
    .bind(name)
    .bind(slug)
    .execute(tx)
    .await
    .map_err(|e| match e {
        // The only unique guard this statement can trip which its arbiter does not absorb is
        // `tenants_slug_key` (UNIQUE on `accounts.account_slug`): a PK conflict IS the ON CONFLICT
        // target. Matched on the SQLSTATE, not on a constraint name (write-validation-parity).
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => AppError::Duplicate(
            format!("workspace slug '{slug}' is already in use by another account"),
        ),
        other => other.into(),
    })?;
    Ok(())
}

/// POST /api/v1/internal/portfolio-sync
pub async fn portfolio_sync_internal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
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

    let action = body
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("create");
    let portfolio_id = body
        .get("portfolio_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let tenant_id = body
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let slug = body
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let email = body
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    match action {
        "create" => {
            if let (Some(pid), Some(tid)) = (portfolio_id, tenant_id) {
                // WorkflowSwift uses accounts not tenants
                sqlx::query("INSERT INTO accounts (id, name, account_slug) VALUES ($1, $2, CONCAT($3, '-', LEFT(CAST($1 AS TEXT), 8))) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name")
                    .bind(tid).bind(&name).bind(&slug)
                    .execute(&state.db).await.ok();
                sqlx::query("INSERT INTO portfolio_companies (id, aid, name, slug, email) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, slug = EXCLUDED.slug, email = EXCLUDED.email, updated_at = NOW()")
                    .bind(pid).bind(tid).bind(&name).bind(&slug).bind(&email)
                    .execute(&state.db).await?;
            }
        }
        "update" => {
            if let Some(pid) = portfolio_id {
                let rows = sqlx::query("UPDATE portfolio_companies SET name = $1, slug = $2, email = $3, updated_at = NOW() WHERE id = $4")
                    .bind(&name).bind(&slug).bind(&email).bind(pid)
                    .execute(&state.db).await?;
                if rows.rows_affected() == 0 {
                    // Nothing matched, so this app holds no company row for `pid`: this is the arm
                    // that catches up after a `create` broadcast this app MISSED (the hub's
                    // broadcast is per-app, best-effort and only warns on a non-2xx — CoreSwift
                    // src/portfolio/sync.rs). The old fallback wrote the CALLER's tenant_id straight
                    // into `portfolio_companies`, whose `portfolio_companies_tenant_id_fkey` needs
                    // an `accounts` row HERE, so an unknown caller tenant raised 23503 and the
                    // caller got an opaque `500 {"error":"Database error"}` naming nothing (kanban
                    // t_898f32a8, measured live at 127.0.0.1:8085 on 2026-10-08). It now mirrors the
                    // caller's account exactly as the fleet's twin receivers do, and refuses with a
                    // 4xx that NAMES what is missing when it cannot mirror — never a 500, never a
                    // silent no-op.
                    let Some(tid) = tenant_id else {
                        return Err(AppError::BadRequest(
                            "update for a portfolio company this app has no row for requires 'tenant_id' to mirror as its parent".into(),
                        ));
                    };
                    let account_slug = resolve_account_slug(&state.db, &name, &slug).await?;
                    // The UPDATE above matched no row, so there is nothing to roll back; the
                    // transaction covers the two rows that must land together (the mirror parent
                    // and the company).
                    let mut tx = state.db.begin().await?;
                    mirror_account(&mut tx, tid, &name, &account_slug).await?;
                    sqlx::query("INSERT INTO portfolio_companies (id, aid, name, slug, email) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, slug = EXCLUDED.slug, email = EXCLUDED.email, updated_at = NOW()")
                        .bind(pid).bind(tid).bind(&name).bind(&account_slug).bind(&email)
                        .execute(&mut *tx).await?;
                    tx.commit().await?;
                }
            }
        }
        "delete" => {
            if let Some(pid) = portfolio_id {
                sqlx::query("DELETE FROM portfolio_companies WHERE id = $1")
                    .bind(pid)
                    .execute(&state.db)
                    .await?;
            }
        }
        _ => return Err(AppError::BadRequest("Invalid action".into())),
    }

    Ok(Json(json!({"status": "synced"})))
}
