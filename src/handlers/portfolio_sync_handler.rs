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
    // NOT marked with `probe_harness` on purpose (kanban t_4a744754): this is the
    // x-internal-key portfolio-sync door (an AUTHENTICATED machine call), not the served,
    // unauthenticated signup that the `X-Swift-Harness` marker exists for.
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

/// The account + company this app ALREADY holds for a portfolio company, found by the company's
/// stable identity (slug → name → email) rather than by the caller's id.
///
/// WHY (adopt-before-create, 2026-11-15): the hub changed which id it sends — it used to carry its
/// own ADMIN workspace id for every company, and now carries each company's OWN id
/// (`tenant_id == portfolio_id`, CoreSwift `src/portfolio/sync.rs`). This app already held
/// Giraudy/ZaarHub/SwiftImpact under the OLD ids, so a re-sync whose id matched nothing minted a
/// SECOND, empty account beside the real one. Matching on the identity this app already stores
/// finds the account that exists and adopts it instead of minting a duplicate.
///
/// This app has NO `tenants` table — the workspace is an `accounts` row (id == `users.aid`), which
/// carries `account_slug` and `name` but NO email column. The company's full identity (slug, name,
/// email) lives on `portfolio_companies` (aid → accounts.id). The account is therefore matched by
/// account_slug → name, and the company row — used only as a FALLBACK source of an account id for a
/// company whose account this app never mirrored — by slug → name → email.
struct AdoptedCompany {
    /// The existing accounts.id to reuse in place of the caller's id.
    account_id: Uuid,
    /// The existing portfolio_companies.id to refresh in place (None when this app holds an account
    /// for the company but no company row yet).
    company_id: Option<Uuid>,
}

/// Resolve the `portfolio_companies` row to refresh alongside the adopted account: the row already
/// linked to it, else the row matching the company's identity (slug → name → email) — the pre-fix
/// link may point at the shared/admin account while the adopted account is the company's own.
async fn resolve_company_id(
    db: &sqlx::PgPool,
    account_id: Uuid,
    name: &str,
    slug: &str,
    email: &str,
) -> Result<Option<Uuid>, AppError> {
    let by_aid: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM portfolio_companies WHERE aid = $1 LIMIT 1")
            .bind(account_id)
            .fetch_optional(db)
            .await?;
    if by_aid.is_some() {
        return Ok(by_aid);
    }
    if !slug.trim().is_empty() {
        let c: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM portfolio_companies WHERE slug = $1 LIMIT 1")
                .bind(slug.trim())
                .fetch_optional(db)
                .await?;
        if c.is_some() {
            return Ok(c);
        }
    }
    if !name.trim().is_empty() {
        let c: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM portfolio_companies WHERE name = $1 LIMIT 1")
                .bind(name.trim())
                .fetch_optional(db)
                .await?;
        if c.is_some() {
            return Ok(c);
        }
    }
    if !email.trim().is_empty() {
        let c: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM portfolio_companies WHERE email = $1 LIMIT 1")
                .bind(email.trim())
                .fetch_optional(db)
                .await?;
        if c.is_some() {
            return Ok(c);
        }
    }
    Ok(None)
}

/// Find the account this app already holds for a portfolio company — the `accounts` row by
/// account_slug → name first (the workspace itself, this app's tenant), then, as a fallback, the
/// `portfolio_companies` row's parent by slug → name → email (a company whose account this app
/// never mirrored). Returns None only when this app holds nothing for the company, which is exactly
/// the case the create/update arm below may mint a fresh account for.
async fn find_existing_company(
    db: &sqlx::PgPool,
    name: &str,
    slug: &str,
    email: &str,
) -> Result<Option<AdoptedCompany>, AppError> {
    // 1. The account (accounts — this app's tenant/workspace). account_slug → name. `accounts` has
    //    no email column, so the email leg of the workspace match has no home here and is covered by
    //    the company fallback below.
    if !slug.trim().is_empty() {
        let a: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM accounts WHERE account_slug = $1 LIMIT 1")
                .bind(slug.trim())
                .fetch_optional(db)
                .await?;
        if let Some(a) = a {
            return Ok(Some(AdoptedCompany {
                account_id: a,
                company_id: resolve_company_id(db, a, name, slug, email).await?,
            }));
        }
    }
    if !name.trim().is_empty() {
        let a: Option<Uuid> = sqlx::query_scalar("SELECT id FROM accounts WHERE name = $1 LIMIT 1")
            .bind(name.trim())
            .fetch_optional(db)
            .await?;
        if let Some(a) = a {
            return Ok(Some(AdoptedCompany {
                account_id: a,
                company_id: resolve_company_id(db, a, name, slug, email).await?,
            }));
        }
    }

    // 2. Fallback — a company whose account this app never mirrored: its aid is the account to
    //    adopt. slug → name → email.
    if !slug.trim().is_empty() {
        let a: Option<Uuid> =
            sqlx::query_scalar("SELECT aid FROM portfolio_companies WHERE slug = $1 LIMIT 1")
                .bind(slug.trim())
                .fetch_optional(db)
                .await?;
        if let Some(a) = a {
            return Ok(Some(AdoptedCompany {
                account_id: a,
                company_id: resolve_company_id(db, a, name, slug, email).await?,
            }));
        }
    }
    if !name.trim().is_empty() {
        let a: Option<Uuid> =
            sqlx::query_scalar("SELECT aid FROM portfolio_companies WHERE name = $1 LIMIT 1")
                .bind(name.trim())
                .fetch_optional(db)
                .await?;
        if let Some(a) = a {
            return Ok(Some(AdoptedCompany {
                account_id: a,
                company_id: resolve_company_id(db, a, name, slug, email).await?,
            }));
        }
    }
    if !email.trim().is_empty() {
        let a: Option<Uuid> =
            sqlx::query_scalar("SELECT aid FROM portfolio_companies WHERE email = $1 LIMIT 1")
                .bind(email.trim())
                .fetch_optional(db)
                .await?;
        if let Some(a) = a {
            return Ok(Some(AdoptedCompany {
                account_id: a,
                company_id: resolve_company_id(db, a, name, slug, email).await?,
            }));
        }
    }

    Ok(None)
}

/// Adopt an existing account for a portfolio re-sync: refresh its name and its company row
/// (re-pointing the company to the adopted account), returning the ORIGINAL ids. Never mints a
/// second account, never touches `account_slug` (the tenant's stable identifier, bound at mint).
async fn adopt_existing_company(
    state: &AppState,
    found: AdoptedCompany,
    payload_pid: Option<Uuid>,
    name: &str,
    slug: &str,
    email: &str,
) -> ApiResult<Json<Value>> {
    let mut tx = state.db.begin().await?;

    // 1. Refresh the existing account's name. `account_slug` is a stable tenant identifier (read by
    //    the admin console and bridge_handler); it is bound at mint and NEVER rewritten — the
    //    create arm below updates name only on conflict too, and re-deriving the slug here could
    //    23505 on a live account's slug.
    sqlx::query("UPDATE accounts SET name = $1, updated_at = NOW() WHERE id = $2")
        .bind(name)
        .bind(found.account_id)
        .execute(&mut *tx)
        .await?;

    // 2. Refresh + re-point the company row to the adopted account. The pre-fix link may point at a
    //    shared/admin account; the company's OWN account is the correct scoping parent.
    match (found.company_id, payload_pid) {
        (Some(cid), _) => {
            sqlx::query(
                "UPDATE portfolio_companies SET aid = $1, name = $2, slug = $3, email = $4, updated_at = NOW() WHERE id = $5",
            )
            .bind(found.account_id)
            .bind(name)
            .bind(slug)
            .bind(email)
            .bind(cid)
            .execute(&mut *tx)
            .await?;
        }
        (None, Some(pid)) => {
            sqlx::query(
                "INSERT INTO portfolio_companies (id, aid, name, slug, email) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO UPDATE SET aid = EXCLUDED.aid, name = EXCLUDED.name, slug = EXCLUDED.slug, email = EXCLUDED.email, updated_at = NOW()",
            )
            .bind(pid)
            .bind(found.account_id)
            .bind(name)
            .bind(slug)
            .bind(email)
            .execute(&mut *tx)
            .await?;
        }
        (None, None) => {}
    }

    tx.commit().await?;

    Ok(Json(json!({
        "status": "synced",
        "adopted": true,
        "account_id": found.account_id.to_string(),
        "portfolio_id": found
            .company_id
            .map(|c| c.to_string())
            .or_else(|| payload_pid.map(|p| p.to_string()))
            .unwrap_or_default(),
    })))
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

    // ── adopt-before-create (2026-11-15) ────────────────────────────────────────────────────────
    // A re-sync of a company this app ALREADY holds must adopt the existing account + company row,
    // never mint a duplicate beside them. The hub changed which id it sends (each company's OWN id
    // instead of the shared admin-workspace id), so the caller's id no longer matches the id this
    // app stored; the match is the company's stable identity — slug → name → email. Only when
    // NOTHING matches does the create/update arm below mint a fresh account.
    if action == "create" || action == "update" {
        if let Some(found) = find_existing_company(&state.db, &name, &slug, &email).await? {
            return adopt_existing_company(&state, found, portfolio_id, &name, &slug, &email).await;
        }
    }

    match action {
        "create" => {
            if let (Some(pid), Some(tid)) = (portfolio_id, tenant_id) {
                // WorkflowSwift uses accounts not tenants.
                // NOT marked with `probe_harness` on purpose (kanban t_4a744754): this
                // mirror runs inside the x-internal-key portfolio-sync door.
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
