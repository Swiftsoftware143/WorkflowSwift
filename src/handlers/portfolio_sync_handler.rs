//! Internal portfolio sync handler — receives broadcasts from CoreSwift CRM.
//! Protected by x-internal-key header, not JWT.

use crate::{
    error::{ApiResult, AppError},
    AppState,
};
use axum::{extract::State, http::HeaderMap, response::IntoResponse, Json};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// RFC 4648 Base32, UPPERCASE, padding stripped — the exact encoding the fleet's derived-password
/// rule specifies. `base64` is Base64 only, so this is the one codec that must be written by hand.
fn base32_encode_upper_no_pad(data: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity((data.len() * 8).div_ceil(5));
    let mut buffer: u32 = 0;
    let mut bits: usize = 0;
    for &byte in data {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buffer >> bits) & 0x1F) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((buffer << (5 - bits)) & 0x1F) as usize;
        out.push(ALPHABET[idx] as char);
    }
    out
}

/// The fleet's derived portfolio password — deterministic, never transmitted except in the one
/// welcome mail, never logged, and identical across every app that holds the same
/// `INTERNAL_SYNC_KEY`.
///
/// ```text
/// raw      = HMAC-SHA256(key = INTERNAL_SYNC_KEY, message = "portfolio:" + portfolio_uuid_string)
/// password = Base32(raw), RFC 4648, UPPERCASE, padding stripped, FIRST 20 CHARACTERS
/// ```
///
/// Because it is a pure function of the sync key and the portfolio id, a re-sync reproduces the
/// same value — so it can never invalidate a credential the user has since changed (the derivation
/// is only applied when the account has no usable password at all, and an existing hash is never
/// touched).
fn derive_password(sync_key: &str, portfolio_id: Uuid) -> String {
    let message = format!("portfolio:{portfolio_id}");
    let mut mac =
        HmacSha256::new_from_slice(sync_key.as_bytes()).expect("HMAC accepts a key of any length");
    mac.update(message.as_bytes());
    let raw = mac.finalize().into_bytes();
    let b32 = base32_encode_upper_no_pad(&raw);
    b32.chars().take(20).collect()
}

/// What the provisioning step did, so each arm can answer in the same words (the fleet standard,
/// same shape as IncentiveSwift's `ProvisionOutcome`).
struct ProvisionOutcome {
    /// A login was CREATED (or completed) by this call — the only case the welcome mail goes out.
    user_created: bool,
    /// The derived password, present only when `user_created` (moved into the mail, never returned).
    password: String,
    key_prefix: Option<String>,
    key_minted: bool,
    /// The plan slug this sync SEATED on the account, or None when it already had an active plan.
    plan_seated: Option<String>,
}

impl ProvisionOutcome {
    fn none() -> Self {
        Self {
            user_created: false,
            password: String::new(),
            key_prefix: None,
            key_minted: false,
            plan_seated: None,
        }
    }
}

/// Seat this app's HIGHEST tier on a mirrored account when it holds no active plan — David's rule
/// for his own companies (2026-10-04), and the fleet standard MissedCall already applies in its
/// portfolio handler (`plans_handler::seat_top_plan_on`).
///
/// WHY (measured 2026-11-15): the sync door created the account and the company and stopped, so a
/// synced portfolio company carried NO `account_plans` row and the entitlement resolver fell back
/// to the first active tier — six of this app's eight portfolio accounts held Enterprise while the
/// two the sync had just repaired held nothing.
///
/// Idempotent by construction: an account that already has an ACTIVE plan row is never re-seated
/// (`account_plans_one_active_per_aid` is a partial UNIQUE index, so a blind INSERT on a re-sync
/// would raise 23505). Best-effort in the sense that "no active tier exists" is not an error.
async fn seat_top_plan(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
) -> Result<Option<String>, AppError> {
    let has_active: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM account_plans WHERE aid = $1 AND status = 'active')",
    )
    .bind(account_id)
    .fetch_one(&mut **tx)
    .await?;
    if has_active {
        return Ok(None);
    }
    let top: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, slug FROM plan_tiers WHERE is_active = true \
         ORDER BY sort_order DESC NULLS LAST, price_monthly DESC NULLS LAST LIMIT 1",
    )
    .fetch_optional(&mut **tx)
    .await?;
    let Some((plan_id, slug)) = top else {
        tracing::warn!(account = %account_id, "portfolio sync: no active plan exists to seat");
        return Ok(None);
    };
    sqlx::query(
        "INSERT INTO account_plans (aid, plan_id, status, started_at) \
         VALUES ($1, $2, 'active', NOW()) ON CONFLICT DO NOTHING",
    )
    .bind(account_id)
    .bind(plan_id)
    .execute(&mut **tx)
    .await?;
    // The plan is seated — fund it, or the workspace cannot run anything (kanban t_b0774174).
    // Same ONE rule the signup uses; gated on an empty ledger so a re-sync never re-credits.
    crate::handlers::credit_handler::grant_plan_welcome_credits(tx, account_id, plan_id).await?;
    tracing::info!(account = %account_id, plan = %slug, "portfolio sync: account seated on the top plan");
    Ok(Some(slug))
}

/// Ensure the mirrored account holds its owner login and exactly one live API key — the fleet
/// portfolio provisioning standard (2026-11-15) — inside the caller's transaction, idempotently.
///
/// This app's workspace is an `accounts` row and the OWNER LOGIN is a `users` row (`aid` →
/// `accounts.id`), so "owner missing" is read as "no usable password on this address".
///
/// * no user for the address → INSERT the owner with the DERIVED password, role `user` (the same
///   role a public signup gets), `user_created = true`;
/// * user with an empty/unusable hash (the `AfterTheSend` marker) → set the derived password;
/// * user with a working hash → untouched, `user_created = false` (no re-mail, no reset);
/// * one live key per account (`api_keys_one_active_per_aid`): minted only when there is none.
async fn provision_owner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sync_key: &str,
    account_id: Uuid,
    portfolio_id: Uuid,
    name: &str,
    email: &str,
) -> Result<ProvisionOutcome, AppError> {
    let existing: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, password_hash FROM users WHERE aid = $1 AND lower(email) = lower($2) LIMIT 1",
    )
    .bind(account_id)
    .bind(email)
    .fetch_optional(&mut **tx)
    .await?;

    let (user_id, user_created, password) = match existing {
        Some((uid, hash)) if crate::auth::api_key_auth::is_usable_hash(&hash) => {
            (uid, false, String::new())
        }
        Some((uid, _)) => {
            let derived = derive_password(sync_key, portfolio_id);
            let hash = crate::auth::api_key_auth::argon2_hash(derived.clone()).await?;
            sqlx::query(
                "UPDATE users SET password_hash = $1, password_undelivered = false, \
                 updated_at = NOW() WHERE id = $2",
            )
            .bind(&hash)
            .bind(uid)
            .execute(&mut **tx)
            .await?;
            (uid, true, derived)
        }
        None => {
            let uid = Uuid::new_v4();
            let derived = derive_password(sync_key, portfolio_id);
            let hash = crate::auth::api_key_auth::argon2_hash(derived.clone()).await?;
            sqlx::query(
                r#"INSERT INTO users (id, aid, email, password_hash, name, role, is_active, created_at, updated_at)
                   VALUES ($1, $2, $3, $4, $5, 'user', true, NOW(), NOW())"#,
            )
            .bind(uid)
            .bind(account_id)
            .bind(email)
            .bind(&hash)
            .bind(name)
            .execute(&mut **tx)
            .await?;
            (uid, true, derived)
        }
    };

    let live_prefix: Option<String> = sqlx::query_scalar(
        "SELECT prefix FROM api_keys WHERE aid = $1 AND is_active = true LIMIT 1",
    )
    .bind(account_id)
    .fetch_optional(&mut **tx)
    .await?;

    let (key_prefix, key_minted) = match live_prefix {
        Some(prefix) => (Some(prefix), false),
        None => {
            // `mint_api_key_on` is the ONE writer for a programmatic key — hash + sealed copy +
            // prefix + ledger row, all on this connection so they commit or roll back together.
            let minted = crate::handlers::api_key_handler::mint_api_key_on(
                tx,
                user_id,
                account_id,
                "auto-provisioned",
                "",
            )
            .await?;
            (Some(minted.prefix), true)
        }
    };

    Ok(ProvisionOutcome {
        user_created,
        password,
        key_prefix,
        key_minted,
        // `provision_for_company` fills this from `seat_top_plan` — the caller's own answer.
        plan_seated: None,
    })
}

/// Provision for a company row this arm has just written, or report "nothing to do" when the
/// caller sent no address (a sync without an email stays a legal mirror, exactly as before).
async fn provision_for_company(
    state: &AppState,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    portfolio_id: Uuid,
    name: &str,
    email: &str,
) -> Result<ProvisionOutcome, AppError> {
    // Seat the top tier FIRST and independently of the address: a company the hub sends without an
    // email still has no business landing on the free fallback (see `seat_top_plan`).
    let plan_seated = seat_top_plan(tx, account_id).await?;
    if email.trim().is_empty() {
        return Ok(ProvisionOutcome {
            plan_seated,
            ..ProvisionOutcome::none()
        });
    }
    let mut prov = provision_owner(
        tx,
        &state.config.internal_sync_key,
        account_id,
        portfolio_id,
        name,
        email,
    )
    .await?;
    prov.plan_seated = plan_seated;
    Ok(prov)
}

/// Send the welcome/credentials mail for a login this sync just created — best effort, AFTER the
/// commit, and only to the portfolio's own address (never to the operator). A mail failure must
/// never fail the sync: the account exists and the derived password is reproducible on a re-sync.
async fn send_welcome(
    state: &AppState,
    account_id: Uuid,
    name: &str,
    email: &str,
    password: &str,
) -> bool {
    const APP_URL: &str = "https://app.workflowswift.com";
    let vars = json!({
        "name": name,
        "email": email,
        "app_url": APP_URL,
        "url": APP_URL,
        "password": password,
    });
    match crate::email::send_email(state, Some(account_id), email, "welcome", &vars).await {
        Ok(()) => {
            tracing::info!(email = %email, account = %account_id, "portfolio sync: welcome email sent");
            true
        }
        Err(e) => {
            tracing::error!(
                email = %email,
                account = %account_id,
                error = %e,
                "portfolio sync: account provisioned but the WELCOME EMAIL FAILED"
            );
            false
        }
    }
}

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

    // 3. Provision — owner login if missing, one live key if missing, welcome mail only when the
    //    login was actually created (the fleet standard, same as the create arm).
    let prov = match payload_pid {
        Some(pid) => {
            provision_for_company(state, &mut tx, found.account_id, pid, name, email).await?
        }
        None => ProvisionOutcome::none(),
    };

    tx.commit().await?;

    let welcome_sent = if prov.user_created {
        send_welcome(state, found.account_id, name, email, &prov.password).await
    } else {
        false
    };

    Ok(Json(json!({
        "status": if prov.user_created || prov.key_minted { "synced" } else { "already_provisioned" },
        "adopted": true,
        "account_id": found.account_id.to_string(),
        // Fleet shape: FunnelSwift/ADASwift/MissedCall call this `tenant_id`. Emit both so a single
        // contract can read the workspace id from every app without branching on the app name.
        "tenant_id": found.account_id.to_string(),
        "portfolio_id": found
            .company_id
            .map(|c| c.to_string())
            .or_else(|| payload_pid.map(|p| p.to_string()))
            .unwrap_or_default(),
        "user_created": prov.user_created,
        "key_minted": prov.key_minted,
        "welcome_sent": welcome_sent,
        "key_prefix": prov.key_prefix,
        "plan_seated": prov.plan_seated,
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

    // What provisioning did, reported at the end for the arms that do not return early (update,
    // delete). The create and adopt arms answer from inside themselves.
    let mut prov = ProvisionOutcome::none();
    let mut welcome_account: Option<Uuid> = None;

    match action {
        "create" => {
            if let (Some(pid), Some(tid)) = (portfolio_id, tenant_id) {
                // WorkflowSwift uses accounts not tenants.
                // NOT marked with `probe_harness` on purpose (kanban t_4a744754): this
                // mirror runs inside the x-internal-key portfolio-sync door.
                //
                // The account, its company row and its OWNER LOGIN are ONE unit of work (2026-11-15):
                // the mirror used to write the two rows and stop, so the owner the hub sent got a
                // company with no way to sign in. A failure in any part rolls the whole create back.
                let mut tx = state.db.begin().await?;
                sqlx::query("INSERT INTO accounts (id, name, account_slug) VALUES ($1, $2, CONCAT($3, '-', LEFT(CAST($1 AS TEXT), 8))) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name")
                    .bind(tid).bind(&name).bind(&slug)
                    .execute(&mut *tx).await.map_err(|e| match e {
                        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => AppError::Duplicate(
                            format!("workspace slug '{slug}' is already in use by another account"),
                        ),
                        other => other.into(),
                    })?;
                sqlx::query("INSERT INTO portfolio_companies (id, aid, name, slug, email) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, slug = EXCLUDED.slug, email = EXCLUDED.email, updated_at = NOW()")
                    .bind(pid).bind(tid).bind(&name).bind(&slug).bind(&email)
                    .execute(&mut *tx).await?;
                let prov = provision_for_company(&state, &mut tx, tid, pid, &name, &email).await?;
                tx.commit().await?;
                let welcome_sent = if prov.user_created {
                    send_welcome(&state, tid, &name, &email, &prov.password).await
                } else {
                    false
                };
                return Ok(Json(json!({
                            "status": if prov.user_created || prov.key_minted { "synced" } else { "already_provisioned" },
                            "account_id": tid.to_string(),
                            "portfolio_id": pid.to_string(),
                            "user_created": prov.user_created,
                            "key_minted": prov.key_minted,
                            "welcome_sent": welcome_sent,
                            "key_prefix": prov.key_prefix,
                "plan_seated": prov.plan_seated,
                        })));
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
                    // The catch-up mirror is a portfolio company too: its owner gets a login, in
                    // the same unit of work (2026-11-15).
                    prov = provision_for_company(&state, &mut tx, tid, pid, &name, &email).await?;
                    welcome_account = Some(tid);
                    tx.commit().await?;
                } else {
                    // The company row exists. Its OWNER LOGIN may still be missing — the pre-fix
                    // mirror never created one — so provisioning runs on the account the row is
                    // scoped to. This is the arm that repairs a company that already synced.
                    let aid: Option<Uuid> =
                        sqlx::query_scalar("SELECT aid FROM portfolio_companies WHERE id = $1")
                            .bind(pid)
                            .fetch_optional(&state.db)
                            .await?;
                    if let Some(aid) = aid {
                        let mut tx = state.db.begin().await?;
                        prov =
                            provision_for_company(&state, &mut tx, aid, pid, &name, &email).await?;
                        welcome_account = Some(aid);
                        tx.commit().await?;
                    }
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

    let welcome_sent = match welcome_account {
        Some(aid) if prov.user_created => {
            send_welcome(&state, aid, &name, &email, &prov.password).await
        }
        _ => false,
    };

    Ok(Json(json!({
        "status": if prov.user_created || prov.key_minted {
            "synced"
        } else if action == "update" {
            "already_provisioned"
        } else {
            "synced"
        },
        "user_created": prov.user_created,
        "key_minted": prov.key_minted,
        "welcome_sent": welcome_sent,
        "key_prefix": prov.key_prefix,
        "plan_seated": prov.plan_seated,
    })))
}
