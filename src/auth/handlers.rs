use crate::email::send_reset_email;
use axum::{
    extract::{Extension, Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use super::middleware::create_token;
use super::models::*;
use crate::auth::api_key_auth::{argon2_hash, argon2_verify_result, is_usable_hash};
use crate::error::{ApiResult, AppError};
use crate::security::address_identity::{self, AddressIdentity};
use crate::security::email_addr;
use crate::AppState;
use std::sync::Arc;

pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<impl IntoResponse> {
    // David's signup model (as IncentiveSwift/FunnelSwift ship): the page collects NAME + EMAIL
    // only, so `password` may arrive empty. The server then mints one and emails it; the user
    // confirms their address by signing in with it. A caller that still supplies one is honoured
    // and validated exactly as before.
    if req.email.is_empty() || req.name.is_empty() {
        return Err(AppError::Validation(
            "Name and email are required".to_string(),
        ));
    }
    if !req.password.is_empty() && req.password.len() < 6 {
        return Err(AppError::Validation(
            "Password must be at least 6 characters".to_string(),
        ));
    }
    let password = if req.password.is_empty() {
        super::signup::generate_temp_password()
    } else {
        req.password.clone()
    };

    // ── Address boundary (kanban t_09e76b27) ────────────────────────────────────────────────
    // FIRST, before any SELECT and long before any INSERT. `users.email` is both the login identity
    // and the only address the welcome/credentials mail can ever reach; this handler used to ask
    // only `req.email.is_empty()`, so any string became a real login. Normalises (trim +
    // lowercase) as well as validates, and the normalised value is what is checked, stored and
    // mailed. A malformed value is refused here, one layer before the credentials mail that would
    // never reach the address it was given.
    let email = email_addr::normalize(&req.email).map_err(AppError::Validation)?;
    // ── VALIDATE THE WRITE: name lengths (kanban-class defect, measured live 2026-10-16) ────────
    // `create_account` derives the workspace label as `"{name}'s Workspace"` and both it and
    // `users.name` are `varchar(255)`, so a 254-character name made the first INSERT of the
    // account transaction the one the database refused: `500 {"code":500,…"Database error"}`, no
    // account, and nothing telling the visitor their name was the problem. Refused BEFORE anything
    // is hashed or written, as a field error the console can read. (`account_slug` is
    // `varchar(100)` and `account_name` is `varchar(255)`, so the caller's explicit values are
    // checked too.)
    crate::security::display_name::normalize(&req.name).map_err(AppError::Validation)?;
    if let Some(account_name) = req.account_name.as_deref().filter(|s| !s.trim().is_empty()) {
        crate::security::display_name::check_len(account_name, 255, "account_name")
            .map_err(AppError::Validation)?;
    }
    if let Some(account_slug) = req.account_slug.as_deref().filter(|s| !s.trim().is_empty()) {
        crate::security::display_name::check_len(
            account_slug,
            crate::security::display_name::MAX_SLUG_CHARS,
            "account_slug",
        )
        .map_err(AppError::Validation)?;
    }

    // Refuse a duplicate BEFORE hashing. This route needs no credential, so spending the Argon2
    // hash on an address that already exists would be a free amplification. `create_account`
    // enforces the same rule again before its first write (design §3.1 rule 3).
    if super::signup::email_taken(&state.db, &email).await? {
        return Err(AppError::Duplicate(
            "A user with this email already exists".to_string(),
        ));
    }

    // Hash password — off the reactor, bounded by the process-wide Argon2 semaphore
    // (argon2_hash). Hashing is 19 MiB of CPU that never awaits, so done inline here it
    // would park a tokio worker for the whole hash and `register` needs no credential
    // to reach.
    let password_hash = argon2_hash(password.clone()).await?;

    // ── ONE writer (design §3.1 rule 4, kanban t_ede5f6ed) ──────────────────────────────────
    // The account itself is minted by `crate::auth::signup::create_account`, the SAME function the
    // fleet-internal `POST /api/v1/internal/provision-free-account` calls (FunnelSwift tag → free
    // account). This handler keeps only what is public-signup-specific: the input validation, the
    // address boundary, the pre-hash duplicate check and the token response.
    // ── A PUBLIC caller may not CHOOSE a paid tier (measured live 2026-10-21) ────────────────
    // `create_account` honours whatever `plan_slug` it is handed, and this route is the only door
    // that takes one from an anonymous body. Measured: `POST /api/v1/auth/register
    // {"name":…,"email":…,"plan_slug":"enterprise"}` minted an ACTIVE `account_plans` row on the
    // Enterprise tier — the paid plan, for nothing, to anyone who asked. A slug is still honoured
    // when it names a tier that costs nothing (so a free-branded tier keeps working); every priced
    // tier falls back to "free", which is where a self-serve signup belongs until a checkout or an
    // operator moves it. The internal provisioning door passes its own slug and never comes here.
    // BOTH prices must be zero (measured live 2026-10-21). The guard above tested
    // `COALESCE(price_monthly, 0) = 0` ALONE, so a tier priced only per YEAR — monthly NULL,
    // yearly 999.00 — read as 0 and an anonymous caller naming that slug was minted on it. Proved
    // on the pre-fix binary: `POST /api/v1/auth/register {"plan_slug":"probe-annual-only-…"}`
    // answered 201 and the stranger's `account_plans` row came back `active|probe-annual-only-…|
    // NULL|999.00`. The ANNUAL column was never consulted, so every annual-only tier was free to
    // anyone who asked. Both columns are now read with the same NULL-tolerant test: a tier that
    // records NO price on either side is genuinely free, and any tier that records a price on
    // either side falls back to "free" like every other priced tier.
    let requested_plan = req.plan_slug.as_deref().unwrap_or("free").trim();
    let plan_slug: String = if requested_plan.eq_ignore_ascii_case("free") {
        "free".to_string()
    } else {
        sqlx::query_scalar::<_, String>(
            "SELECT slug FROM plan_tiers \
             WHERE slug = $1 AND is_active = true \
               AND COALESCE(price_monthly, 0) = 0 AND COALESCE(price_yearly, 0) = 0",
        )
        .bind(requested_plan)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_else(|| "free".to_string())
    };
    let ids = super::signup::create_account(
        &state,
        super::signup::NewAccount {
            email: &email,
            name: &req.name,
            password_hash: &password_hash,
            // The server-minted (or caller-supplied) plaintext, so the `welcome` template can
            // carry the credential line the signup now depends on.
            password_plain: Some(&password),
            account_name: req.account_name.as_deref(),
            account_slug: req.account_slug.as_deref(),
            plan_slug: plan_slug.as_str(),
            industry_slug: req.industry_slug.as_deref().unwrap_or("site-flipping"),
            role: "user",
            // The account is usable without the mail: the caller owns this hash, and
            // `register` hands the session back in its response (kanban t_6d58f5be).
            credential_delivery: super::signup::CredentialDelivery::WithTheAccount,
            // Attribution for the workspace this route is about to mint (kanban t_4a744754, extended
            // 2026-10-17): the `X-Swift-Harness` HEADER names the harness when the caller sends one,
            // otherwise the recipient's own harness/reserved domain decides
            // (`probe_harness::for_signup`). Neither arm reads the body, and a real customer's domain
            // matches neither, so their account row is written exactly as before.
            probe_harness: crate::probe_harness::for_signup(&headers, &email).as_deref(),
        },
    )
    .await?;

    // The tier the visitor picked on the pricing page becomes a WORKABLE request (cron 2026-10-23).
    // `create_account` above may only bind a zero-priced tier, so the paid pick would otherwise be
    // dropped in silence — which is exactly what the nginx comment on the register redirect describes.
    // Filed as the same `source='plan_request'` ticket the console's request button writes.
    let plan_request = match req
        .plan_slug
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        Some(picked) => {
            match crate::handlers::tickets_handler::file_signup_plan_request(
                &state.db,
                ids.account_id,
                picked,
            )
            .await
            {
                Ok(filed) => filed,
                Err(e) => {
                    tracing::warn!("signup plan pick '{picked}' was not recorded: {e}");
                    None
                }
            }
        }
        None => None,
    };

    let aid = ids.account_id;
    let user_id = ids.user_id;
    let account_name = ids.account_name;
    let account_slug = ids.account_slug;
    let now = Utc::now();

    // Create JWT
    let now_ts = chrono::Utc::now().timestamp() as usize;
    let claims = Claims {
        sub: user_id.to_string(),
        aid: aid.to_string(),
        role: "user".to_string(),
        exp: now_ts + state.config.jwt_access_expiry as usize,
        iat: now_ts,
        perm_is_super_admin: Some(false),
    };
    let token = create_token(&claims, &state.config.jwt_secret)?;

    let refresh_claims = Claims {
        sub: user_id.to_string(),
        aid: aid.to_string(),
        role: "user".to_string(),
        exp: now_ts + state.config.jwt_refresh_expiry as usize,
        iat: now_ts,
        perm_is_super_admin: Some(false),
    };
    let refresh_token = create_token(&refresh_claims, &state.config.jwt_secret)?;

    let user_response = UserResponse {
        id: user_id,
        aid,
        email: email.clone(),
        name: req.name,
        role: "user".to_string(),
        is_active: true,
        last_login_at: None,
        created_at: now,
    };

    // NOTE: no CoreSwift push on signup. The fleet standard makes CoreSwift INBOUND only
    // (captured leads flow down into the hub) and rejects the old pattern of pushing a
    // tenant's signup into a hardcoded SwiftSoftware tenant with a global env key. Leads
    // captured by this account's workflows are delivered by
    // `handlers::coreswift_external::push_lead_to_coreswift` using the account's own BYOK key.

    Ok((
        StatusCode::CREATED,
        Json(json!(RegisterResponse {
            access_token: token,
            refresh_token,
            token_type: "Bearer".to_string(),
            expires_in: state.config.jwt_access_expiry,
            user: user_response,
            account: AccountResponse {
                id: aid,
                name: account_name,
                slug: account_slug,
                is_active: true,
            },
            // What the signup page reads to tell the visitor their pick reached the team
            // (`{plan_slug, plan_name}`), null when they picked Free or named no tier.
            plan_request: plan_request.map(|(slug, name)| json!({
                "plan_slug": slug,
                "plan_name": name,
            })),
        })),
    ))
}

/// How many rows one address may map to before login refuses it WITHOUT hashing anything. Far
/// beyond any real multi-tenant address (and unreachable from `register`, which refuses an address
/// that exists anywhere), so it only ever binds on a table stuffed by a fixture or a privileged
/// writer. It exists so the Argon2 work one unauthenticated request can buy is a constant.
const MAX_LOGIN_CANDIDATES: usize = 8;

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<impl IntoResponse> {
    // The same normalisation the writers store by, matched case-insensitively so an account stored
    // with capitals (or created before normalisation existed) still resolves when the customer
    // retypes their address with different casing. A malformed value is NOT refused here: login
    // answers its own invalid-credentials response for every wrong input, and it must not become an
    // account-existence oracle. It simply matches nothing.
    //
    // The unique index is (aid, email) — NOT email — so one address CAN exist in several tenants,
    // and a login carries no tenant. The credential is therefore what picks the row; every
    // candidate is fetched, oldest first, so nothing below depends on HEAP order. That lottery is
    // exactly what made this route non-deterministic (t_db00b05c: three rows for the platform
    // operator's address, one of them answerable with a 500).
    let candidates = sqlx::query_as::<_, User>(
        "SELECT * FROM users WHERE lower(email) = $1 ORDER BY created_at ASC, id ASC LIMIT $2",
    )
    .bind(email_addr::lookup_key(&req.email))
    .bind(MAX_LOGIN_CANDIDATES as i64 + 1)
    .fetch_all(&state.db)
    .await?;

    if candidates.len() > MAX_LOGIN_CANDIDATES {
        // More rows than the cap: ambiguous by construction. Refuse BEFORE spending a single hash,
        // so a stuffed address cannot be used as an Argon2 amplifier through an unauthenticated
        // route. Nothing is leaked — this is the same answer as a wrong password.
        tracing::warn!(
            rows = candidates.len(),
            "login refused: more than {} rows carry one address",
            MAX_LOGIN_CANDIDATES
        );
        return Err(AppError::InvalidCredentials);
    }

    // A stored hash that is not a usable Argon2 PHC string is not a credential: no password can
    // match it, and it must never turn a login into a 500. Two live cases — a user created by a
    // checkout whose credential mail never went out (`checkout_handler::deliver_credentials`
    // leaves `password_hash` empty on purpose), and a fixture/legacy row holding a placeholder
    // (`'x'`, measured live). Both are SKIPPED, which is an ordinary invalid-credentials answer
    // rather than an internal error, and it keeps a broken row from being a probe target.
    let mut winner: Option<User> = None;
    let mut verified = 0usize;
    for candidate in candidates {
        if !is_usable_hash(&candidate.password_hash) {
            continue;
        }
        // Verify password — off the reactor, bounded by the same process-wide semaphore the
        // API-key path uses (argon2_verify_result). Reached without any credential, so inline
        // this is a free way for an unauthenticated caller to park every worker thread. A genuine
        // runtime failure is still an error; an unusable stored hash is not (filtered above).
        if argon2_verify_result(
            candidate.password_hash.clone(),
            Arc::from(req.password.as_str()),
        )
        .await?
        {
            verified += 1;
            if winner.is_none() {
                winner = Some(candidate);
            }
        }
    }

    let user = match (verified, winner) {
        // Exactly one row holds this credential: it IS the identity, whatever the heap order.
        (1, Some(user)) => user,
        // No row does — an unknown address, a wrong password, or a row with no usable hash.
        (0, _) => return Err(AppError::InvalidCredentials),
        // Several tenants' rows hold this same credential and a login carries no tenant. Minting a
        // session for one of them would be the heap-order lottery again, so refuse — and say it
        // once, in the log, where an operator can see the address needs one identity.
        (n, _) => {
            tracing::warn!(
                rows = n,
                "login refused: one address matches more than one stored credential"
            );
            return Err(AppError::InvalidCredentials);
        }
    };

    if !user.is_active {
        return Err(AppError::Forbidden("Account is deactivated".to_string()));
    }

    // Update last_login
    sqlx::query("UPDATE users SET last_login_at = NOW() WHERE id = $1")
        .bind(user.id)
        .execute(&state.db)
        .await?;

    // Generate JWT
    let now_ts = chrono::Utc::now().timestamp() as usize;
    let claims = Claims {
        sub: user.id.to_string(),
        aid: user.aid.to_string(),
        role: user.role.clone(),
        exp: now_ts + state.config.jwt_access_expiry as usize,
        iat: now_ts,
        perm_is_super_admin: Some(user.perm_is_super_admin),
    };
    let token = create_token(&claims, &state.config.jwt_secret)?;

    let refresh_claims = Claims {
        sub: user.id.to_string(),
        aid: user.aid.to_string(),
        role: user.role.clone(),
        exp: now_ts + state.config.jwt_refresh_expiry as usize,
        iat: now_ts,
        perm_is_super_admin: Some(user.perm_is_super_admin),
    };
    let refresh_token = create_token(&refresh_claims, &state.config.jwt_secret)?;

    Ok(Json(json!(TokenResponse {
        access_token: token,
        refresh_token,
        token_type: "Bearer".to_string(),
        expires_in: state.config.jwt_access_expiry,
        user: user.into(),
    })))
}

/// The real plan tier name for an account (programme card t_2cb77960, this app's card t_39cea779).
///
/// Resolution is the SAME one `crate::features` already uses for every limit decision
/// (`features::resolve_plan_id`: an active `account_plans` row, else `accounts.plan_id`, else the
/// first active tier), so a label read here can never disagree with the plan the gates enforce. A
/// plan-less account falls back to "Free" — a label the account can always read, never a blank and
/// never the role word "User". A database hiccup is NOT fatal (this is a display field, not an
/// authorization decision), so it degrades to the default rather than 500-ing the account screen.
async fn plan_name_for(db: &sqlx::PgPool, aid: Uuid) -> String {
    let Some(pid) = crate::features::resolve_plan_id(db, aid)
        .await
        .ok()
        .flatten()
    else {
        return "Free".to_string();
    };
    sqlx::query_scalar::<_, String>("SELECT name FROM plan_tiers WHERE id = $1")
        .bind(pid)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "Free".to_string())
}

/// `GET /api/v1/auth/me` — the signed-in account, INCLUDING the real plan tier (card t_39cea779).
///
/// Before this card the route answered the raw user row, which names no tier, so the console could
/// print nothing for the level label (and, on the fleet's FunnelSwift, printed the role word
/// "User"). This answer carries `plan_name` from the account's real plan, the optional `company`
/// and `username` the Profile screen edits, and `avatar_url` when a picture exists.
pub async fn me(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let row = sqlx::query(
        "SELECT id, aid, email, name, role, is_active, last_login_at, created_at, username, company, perm_is_super_admin \
         FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("User not found".to_string()))?;

    let aid: Uuid = row
        .try_get("aid")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let plan_name = plan_name_for(&state.db, aid).await;

    let has_avatar: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_avatars WHERE user_id = $1)")
            .bind(user_id)
            .fetch_one(&state.db)
            .await?;

    let me = MeResponse {
        id: row
            .try_get("id")
            .map_err(|e| AppError::Internal(e.to_string()))?,
        aid,
        email: row
            .try_get("email")
            .map_err(|e| AppError::Internal(e.to_string()))?,
        name: row
            .try_get("name")
            .map_err(|e| AppError::Internal(e.to_string()))?,
        role: row
            .try_get("role")
            .map_err(|e| AppError::Internal(e.to_string()))?,
        is_active: row
            .try_get("is_active")
            .map_err(|e| AppError::Internal(e.to_string()))?,
        last_login_at: row
            .try_get("last_login_at")
            .map_err(|e| AppError::Internal(e.to_string()))?,
        created_at: row
            .try_get("created_at")
            .map_err(|e| AppError::Internal(e.to_string()))?,
        username: row
            .try_get("username")
            .map_err(|e| AppError::Internal(e.to_string()))?,
        company: row
            .try_get("company")
            .map_err(|e| AppError::Internal(e.to_string()))?,
        plan_name,
        perm_is_super_admin: row
            .try_get("perm_is_super_admin")
            .map_err(|e| AppError::Internal(e.to_string()))?,
        avatar_url: if has_avatar {
            Some(format!("/api/v1/auth/avatar/{user_id}"))
        } else {
            None
        },
    };

    Ok(Json(json!({ "user": me })))
}

/// `PUT /api/v1/auth/password` (fleet path) and `POST /api/v1/auth/change-password` (this app's
/// original path) both land here (card t_39cea779).
///
/// `{current_password, new_password}` with a 8-character floor — the fleet contract, raised from
/// this app's original 6 to match FunnelSwift/MissedCall. A wrong CURRENT password answers
/// 401 "Current password is incorrect" NAMING the field, not the generic invalid-credentials body,
/// so the account screen can show which box to fix. The stored hash is verified off the reactor
/// behind the process-wide Argon2 semaphore, and a stored hash that is not a usable Argon2 PHC
/// string is treated as a wrong password rather than a 500 (the same rule `login` applies).
pub async fn change_password(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ChangePasswordRequest>,
) -> ApiResult<impl IntoResponse> {
    if req.new_password.len() < 8 {
        return Err(AppError::BadRequest(
            "New password must be at least 8 characters".to_string(),
        ));
    }

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::Unauthorized)?;

    if !is_usable_hash(&user.password_hash)
        || !argon2_verify_result(
            user.password_hash.clone(),
            Arc::from(req.current_password.as_str()),
        )
        .await?
    {
        return Err(AppError::InvalidPassword);
    }

    // Hash new password
    let new_hash = argon2_hash(req.new_password.clone()).await?;

    // SESSION REVOCATION (kanban t_80787a0f): a password change/reset also ends every JWT
    // minted BEFORE it (`users.sessions_valid_from`), refused at the boundary read.
    sqlx::query(
        "UPDATE users SET password_hash = $1, sessions_valid_from = NOW(), updated_at = NOW() \
           WHERE id = $2",
    )
    .bind(&new_hash)
    .bind(user.id)
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::OK,
        Json(json!({"message": "Password updated successfully"})),
    ))
}

/// `PUT /api/v1/auth/profile` — `{name?, username?, company?}` (programme t_2cb77960, card
/// t_39cea779).
///
/// The rules, in the words the console depends on. An ABSENT key leaves the stored value untouched,
/// so a save that only sends `name` cannot blank a company the account already set. `name`, when
/// present, must be non-blank (400) — it is the account's display name. `username` and `company`
/// may be CLEARED by sending an empty string, because both columns are nullable. Over-long values
/// are a 400, never a 500. Answers `{"status":"ok"}` — the shape the fleet profile contract names.
pub async fn update_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    /// Longest value `username` / `company` accept from the form. Over-long is a 400, never a
    /// 500. `name` is NOT bounded here: it goes through `security::display_name`, the same guard
    /// the signup door uses, so a name signup accepted can always be saved again.
    const MAX_FIELD: usize = 200;

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let name = req.get("name").and_then(|v| v.as_str());
    let username = req.get("username").and_then(|v| v.as_str());
    let company = req.get("company").and_then(|v| v.as_str());

    // The SAME rule the signup door applies (measured 2026-10-17): a 210-character name was
    // accepted at signup (cap 243) and then REFUSED here with `400 name is too long`, so the
    // customer could never save their own profile again. One guard, both doors.
    let normalised_name = match name {
        Some(n) => Some(crate::security::display_name::normalize(n).map_err(AppError::Validation)?),
        None => None,
    };
    for (label, v) in [("username", username), ("company", company)] {
        if let Some(v) = v {
            if v.chars().count() > MAX_FIELD {
                return Err(AppError::BadRequest(format!("{label} is too long")));
            }
        }
    }

    if let Some(n) = normalised_name {
        sqlx::query("UPDATE users SET name = $1, updated_at = NOW() WHERE id = $2")
            .bind(n)
            .bind(user_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(u) = username {
        let val: Option<&str> = if u.trim().is_empty() {
            None
        } else {
            Some(u.trim())
        };
        sqlx::query("UPDATE users SET username = $1, updated_at = NOW() WHERE id = $2")
            .bind(val)
            .bind(user_id)
            .execute(&state.db)
            .await?;
    }
    if let Some(c) = company {
        let val: Option<&str> = if c.trim().is_empty() {
            None
        } else {
            Some(c.trim())
        };
        sqlx::query("UPDATE users SET company = $1, updated_at = NOW() WHERE id = $2")
            .bind(val)
            .bind(user_id)
            .execute(&state.db)
            .await?;
    }

    Ok(Json(json!({"status": "ok"})))
}

/// The upload envelope the protected router allows. This is the SHARED constant
/// ([`crate::image_store::ENVELOPE_BYTES`]) so the router's body limit, the handler's cap and the
/// sentence a customer is shown can never drift apart again: the route limit sits just above the
/// envelope, and a body over it is answered 413 with the picker's own message.
pub const MAX_AVATAR_BYTES: usize = crate::image_store::ENVELOPE_BYTES;

/// Identify an image by its MAGIC BYTES. Kept as a re-export of the shared sniffer so any caller
/// written against the old name still compiles, with ONE implementation behind it.
#[allow(dead_code)]
pub(crate) fn sniff_image(b: &[u8]) -> Option<&'static str> {
    crate::image_store::sniff(b)
}

/// `POST /api/v1/auth/avatar` — the image bytes, per user (card t_39cea779).
///
/// TWO body shapes, ONE handler. The other apps take the picture as `multipart/form-data`; this
/// app historically took it as the RAW body. A client that used the fleet's form idiom got a 400
/// "Unsupported picture" here, so BOTH are read — by the ONE shared path
/// ([`crate::image_store::read_request_image`]), which also owns the size cap and the magic-byte
/// sniff. A non-image and an empty body are refused 400; a body over the cap is refused **413**
/// with the sentence the console's own picker shows. One row per user (upsert), so re-uploading
/// replaces the picture rather than accumulating rows. Private: the caller
/// must present their session.
pub async fn upload_avatar(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    headers: HeaderMap,
    request: Request,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    // The ONE shared image path (crate::image_store): a multipart form OR a raw image body, the
    // 2 MB envelope, the magic-byte sniff, and the fleet's 413 sentence when the file is too big.
    let (content_type, bytes) =
        crate::image_store::read_request_image(&state, &headers, request).await?;

    sqlx::query(
        "INSERT INTO user_avatars (user_id, bytes, content_type, updated_at) VALUES ($1, $2, $3, NOW()) \
         ON CONFLICT (user_id) DO UPDATE SET bytes = EXCLUDED.bytes, \
         content_type = EXCLUDED.content_type, updated_at = NOW()",
    )
    .bind(user_id)
    .bind(&bytes)
    .bind(&content_type)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "status": "ok",
        "avatar_url": format!("/api/v1/auth/avatar/{user_id}"),
    })))
}

/// `GET /api/v1/auth/avatar/:user_id` — serve the stored picture (card t_39cea779).
///
/// ANONYMOUS BY CONSTRUCTION and narrow by design: an `<img src>` cannot carry a bearer token, so
/// the route is on `route_policy::PUBLIC_ROUTES`. It returns one thing — the bytes one user
/// uploaded, keyed by an unguessable uuid, under the content type sniffed at upload time. No tenant
/// column, no credential, no row of user data; a user with no picture answers 404. The authenticated
/// upload twin stays private.
pub async fn get_avatar(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
) -> ApiResult<Response> {
    let row = sqlx::query("SELECT bytes, content_type FROM user_avatars WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("No picture for this account".to_string()))?;

    let bytes: Vec<u8> = row
        .try_get("bytes")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let content_type: String = row
        .try_get("content_type")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // The ONE serve path. NOT cached (card t_39cea779): the URL is stable per user, so ANY max-age
    // would keep serving the PREVIOUS face after a re-upload — the exact opposite of the
    // "byte-identical" promise the account screen depends on.
    Ok(crate::image_store::image_response(content_type, bytes))
}

/// `DELETE /api/v1/auth/avatar` — remove the caller's OWN profile picture.
///
/// The upload twin only ever REPLACES the row (`ON CONFLICT (user_id) DO UPDATE`), so before this
/// route existed a picture could be swapped for another one but never taken down: an operator who
/// wanted their initials back had no way to ask, and the console could only offer "Upload picture".
/// The email-branding logo has had its own DELETE since it shipped, so this closes the same gap on
/// the profile.
///
/// It writes the SESSION's own user — there is no path id to spoof, the same rule `upload_avatar`
/// follows — and it is idempotent: `removed` reports whether a row was actually there, so calling it
/// on an account with no picture answers 200 rather than inventing a 404 for a state that is already
/// what the caller asked for. `avatar_url` comes back empty so the caller can repaint from the
/// response instead of re-reading `/auth/me`.
///
/// ONE row carries the picture here: the bytes in `user_avatars`. Unlike the apps whose `users`
/// table also holds an `avatar_url` POINTER, this app derives the answer for `/auth/me` from the row
/// existing (`SELECT EXISTS(... user_avatars ...)`), so deleting the row is the whole statement and
/// there is no second place to leave stale.
pub async fn delete_avatar(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let removed = sqlx::query("DELETE FROM user_avatars WHERE user_id = $1")
        .bind(user_id)
        .execute(&state.db)
        .await?
        .rows_affected();

    Ok(Json(json!({
        "status": "ok",
        "removed": removed,
        "avatar_url": "",
    })))
}

// ── CHANGE OF SIGN-IN ADDRESS (cron 2026-10-18) ────────────────────────────────────────────────
//
// Signup collects NAME + EMAIL only, so ONE typo produced an account that could never receive
// another message from us, with no way out: the Profile screen had no address field and no route
// anywhere in the app could write `users.email`.
//
// The change is PROVEN, not merely requested. The confirmation link goes to the NEW address and
// only that mailbox can complete it; `users.email` moves at CONFIRMATION time, never at request
// time, so a mistyped new address cannot lock the customer out of their own account.
//
// A reserved/harness address (example.invalid and friends — `security::probe_addr`) is WITHHELD at
// source by `email::send_email` exactly like signup and password reset: the pending row is still
// written so the flow stays exercisable end-to-end, but no provider request is made and the Email
// Provider panel records a `withheld` non-send.

#[derive(Debug, serde::Deserialize)]
pub struct RequestEmailChangeRequest {
    pub new_email: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct ConfirmEmailChangeRequest {
    pub token: String,
}

/// `POST /api/v1/auth/email-change` — ask to move the caller's OWN sign-in address.
///
/// It writes the session's own row (no path id to spoof, the same rule `upload_avatar` follows) and
/// stores a pending request ONLY. Uniqueness is checked here so a clash reads as a sentence rather
/// than as a 500 out of the `(aid, email)` unique index; the check spans every account because this
/// app's sign-in resolves an address to an IDENTITY (`security::address_identity`) and two rows
/// sharing one address is exactly the ambiguity `forgot_password` refuses to guess at.
pub async fn request_email_change(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RequestEmailChangeRequest>,
) -> ApiResult<impl IntoResponse> {
    let new_email = email_addr::normalize(&req.new_email).map_err(AppError::BadRequest)?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let current: Option<(String, String)> =
        sqlx::query_as("SELECT email, name FROM users WHERE id = $1 AND is_active = true")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?;
    let (current_email, name) = current.ok_or(AppError::Unauthorized)?;

    if current_email.eq_ignore_ascii_case(&new_email) {
        return Err(AppError::BadRequest(
            "That is already your sign-in address".to_string(),
        ));
    }

    let taken: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users WHERE lower(email) = $1 AND is_active = true AND id <> $2 LIMIT 1",
    )
    .bind(&new_email)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    if taken.is_some() {
        return Err(AppError::BadRequest(
            "That address is already used by another account".to_string(),
        ));
    }

    let token = Uuid::new_v4().to_string();
    let expires_at = Utc::now() + chrono::Duration::hours(2);

    // One live request per user: retire the previous one before inserting the new one.
    sqlx::query("UPDATE email_changes SET used = true WHERE user_id = $1 AND used = false")
        .bind(user_id)
        .execute(&state.db)
        .await
        .ok();

    sqlx::query(
        "INSERT INTO email_changes (user_id, new_email, token, expires_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(&new_email)
    .bind(&token)
    .bind(expires_at)
    .execute(&state.db)
    .await?;

    let app_url = "https://app.workflowswift.com";
    let link = format!("{app_url}/confirm-email-change?token={token}");
    let withheld = crate::security::probe_addr::harness_domain(&new_email).is_some();

    let vars = json!({
        "name": name,
        "new_email": new_email,
        "link": link,
        "token": token,
        "app_url": app_url,
    });
    if let Err(e) =
        crate::email::send_email(&state, Some(aid), &new_email, "email_change", &vars).await
    {
        tracing::error!(
            "email-change: the confirmation mail could not be handed to the provider for {}: {}",
            new_email,
            e
        );
        // No link reached the customer, so the pending row must not look usable.
        sqlx::query("UPDATE email_changes SET used = true WHERE token = $1")
            .bind(&token)
            .execute(&state.db)
            .await
            .ok();
        return Err(AppError::BadRequest(
            "We could not send the confirmation link, so nothing was changed. Try again in a moment."
                .to_string(),
        ));
    }

    Ok(Json(json!({
        "status": "pending",
        "delivered": !withheld,
        "new_email": new_email,
        "message": if withheld {
            "This address is a reserved test address, so no mail was sent. Nothing has been changed."
        } else {
            "Check the new address for a confirmation link. Your sign-in email stays the same until you open it."
        },
    })))
}

/// `POST /api/v1/auth/email-change/confirm` — anonymous by necessity: the token IS the credential
/// the customer just received, and a link opened in a browser carries no session.
pub async fn confirm_email_change(
    State(state): State<AppState>,
    Json(req): Json<ConfirmEmailChangeRequest>,
) -> ApiResult<impl IntoResponse> {
    let token = req.token.trim().to_string();
    if token.is_empty() {
        return Err(AppError::BadRequest(
            "The confirmation link is missing its code".to_string(),
        ));
    }

    let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT id, user_id, new_email FROM email_changes
         WHERE token = $1 AND used = false AND expires_at > NOW()",
    )
    .bind(&token)
    .fetch_optional(&state.db)
    .await?;
    let (change_id, user_id, new_email) = row.ok_or_else(|| {
        AppError::BadRequest("That confirmation link is invalid or has expired".to_string())
    })?;

    // The address may have been taken between the request and this confirmation.
    let taken: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users WHERE lower(email) = $1 AND is_active = true AND id <> $2 LIMIT 1",
    )
    .bind(&new_email)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    if taken.is_some() {
        return Err(AppError::BadRequest(
            "That address has since been used by another account, so nothing was changed"
                .to_string(),
        ));
    }

    let moved: Option<String> = sqlx::query_scalar(
        "UPDATE users SET email = $1, updated_at = NOW() WHERE id = $2 AND is_active = true RETURNING email",
    )
    .bind(&new_email)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    let moved = moved
        .ok_or_else(|| AppError::BadRequest("That account is no longer active".to_string()))?;

    sqlx::query("UPDATE email_changes SET used = true WHERE id = $1")
        .bind(change_id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({
        "status": "ok",
        "email": moved,
        "message": "Your sign-in address has been changed. Use it the next time you sign in.",
    })))
}

pub async fn forgot_password(
    State(state): State<AppState>,
    Json(req): Json<ForgotPasswordRequest>,
) -> ApiResult<impl IntoResponse> {
    // Read through the same normalisation by which addresses are stored, and case-insensitively so
    // a pre-boundary row still resolves. The row is picked by IDENTITY, never by heap order
    // (t_8bcd0a8e): the unique index is `(aid, email)`, so one address can exist in several tenants
    // while this request carries neither a tenant nor a credential — nothing the caller sent can
    // choose between rows. An address that still maps to more than one identity therefore mints
    // NOTHING (see below): the reset mail reaches the one address either way, so a token for an
    // arbitrary row would silently reset ANOTHER TENANT's identity. No failure arm on purpose: a
    // malformed value matches nothing, and existing-vs-not must stay unobservable (this response is
    // already uniform, and it stays uniform in every arm below).
    let reset_target =
        match address_identity::resolve_address(&state.db, &req.email, |user: &User| {
            user.password_hash.as_str()
        })
        .await?
        {
            AddressIdentity::Unique(user) => Some(user),
            AddressIdentity::Unknown => None,
            AddressIdentity::Ambiguous { rows, identities } => {
                tracing::warn!(
                    rows,
                    identities,
                    "password reset refused: one address maps to more than one identity"
                );
                None
            }
        };

    if let Some(user) = reset_target {
        let token = Uuid::new_v4().to_string();
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(24);

        sqlx::query("UPDATE password_resets SET used = true WHERE user_id = $1 AND used = false")
            .bind(user.id)
            .execute(&state.db)
            .await
            .ok();

        sqlx::query("INSERT INTO password_resets (user_id, token, expires_at) VALUES ($1, $2, $3)")
            .bind(user.id)
            .bind(&token)
            .bind(expires_at)
            .execute(&state.db)
            .await?;

        // A send failure here is NOT returned to the caller, on purpose: answering 5xx only for
        // addresses that exist turns forgot-password into an account-existence oracle, and the
        // reset token is already in the DB either way. The failure is surfaced where an admin
        // looks instead — `send_email` records last_send_ok/last_send_error/last_send_template in
        // the `admin_settings.email` row the Email Provider panel renders (t_b28d3432).
        match send_reset_email(&state, Some(user.aid), &user.email, &token).await {
            Ok(_) => tracing::info!("Password reset email sent to {}", user.email),
            Err(e) => tracing::error!(
                "Failed to send password reset email to {}: {}",
                user.email,
                e
            ),
        }
        // Send password reset email via SMTP
    }

    Ok((
        StatusCode::OK,
        Json(json!({"message": "If the email exists, a password reset link has been sent"})),
    ))
}

pub async fn reset_password(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> ApiResult<impl IntoResponse> {
    if req.new_password.len() < 6 {
        return Err(AppError::Validation(
            "New password must be at least 6 characters".to_string(),
        ));
    }

    let reset = sqlx::query_as::<_, (Uuid, Uuid, String, chrono::DateTime<chrono::Utc>, bool, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, user_id, token, expires_at, used, created_at FROM password_resets WHERE token = $1 AND used = false AND expires_at > NOW()",
    )
    .bind(&req.token)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::BadRequest("Invalid or expired reset token".to_string()))?;

    // Off the reactor, bounded by the same Argon2 semaphore (see auth::api_key_auth).
    let new_hash = argon2_hash(req.new_password.clone()).await?;

    // SESSION REVOCATION (kanban t_80787a0f): a password change/reset also ends every JWT
    // minted BEFORE it (`users.sessions_valid_from`), refused at the boundary read.
    sqlx::query(
        "UPDATE users SET password_hash = $1, sessions_valid_from = NOW(), updated_at = NOW() \
           WHERE id = $2",
    )
    .bind(&new_hash)
    .bind(reset.1)
    .execute(&state.db)
    .await?;

    sqlx::query("UPDATE password_resets SET used = true WHERE id = $1")
        .bind(reset.0)
        .execute(&state.db)
        .await?;

    Ok((
        StatusCode::OK,
        Json(json!({"message": "Password has been reset successfully"})),
    ))
}

pub async fn get_usage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Value>, AppError> {
    let aid: uuid::Uuid = claims
        .aid
        .parse()
        .map_err(|_| AppError::BadRequest("Invalid account".into()))?;
    let usage = crate::features::get_usage_json(&state.db, aid).await;
    Ok(Json(usage))
}
