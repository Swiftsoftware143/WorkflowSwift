use crate::email::send_reset_email;
use axum::{
    body::{Body, Bytes},
    extract::{Extension, Path, State},
    http::{header, StatusCode},
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
            plan_slug: req.plan_slug.as_deref().unwrap_or("free"),
            industry_slug: req.industry_slug.as_deref().unwrap_or("site-flipping"),
            role: "user",
        },
    )
    .await?;

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
        "SELECT id, aid, email, name, role, is_active, last_login_at, created_at, username, company \
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

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
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
    /// Longest value any of the three columns accepts from the form. Over-long is a 400, never a 500.
    const MAX_FIELD: usize = 200;

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let name = req.get("name").and_then(|v| v.as_str());
    let username = req.get("username").and_then(|v| v.as_str());
    let company = req.get("company").and_then(|v| v.as_str());

    if let Some(n) = name {
        if n.trim().is_empty() {
            return Err(AppError::BadRequest("name cannot be empty".to_string()));
        }
    }
    for (label, v) in [("name", name), ("username", username), ("company", company)] {
        if let Some(v) = v {
            if v.chars().count() > MAX_FIELD {
                return Err(AppError::BadRequest(format!("{label} is too long")));
            }
        }
    }

    if let Some(n) = name {
        sqlx::query("UPDATE users SET name = $1, updated_at = NOW() WHERE id = $2")
            .bind(n.trim())
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

/// Largest profile picture this route accepts. The contract says 2 MB; the protected router's
/// `DefaultBodyLimit` is set a little above this so a body that is OVER the cap is refused by
/// [`upload_avatar`] with this app's JSON 400, not by the body-read extractor with an unreadable
/// text/plain 413.
pub const MAX_AVATAR_BYTES: usize = 2 * 1024 * 1024;

/// Identify an image by its MAGIC BYTES, never by a caller-supplied content type or filename
/// (FunnelSwift t_ff948669's decision, reused here). Returns the content type to store, or `None`
/// for anything that is not one of the four accepted formats.
fn sniff_image(b: &[u8]) -> Option<&'static str> {
    if b.len() >= 8 && b.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if b.len() >= 3 && b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if b.len() >= 6 && (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
        return Some("image/gif");
    }
    if b.len() >= 12 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// `POST /api/v1/auth/avatar` — the raw image bytes, per user (card t_39cea779).
///
/// The body IS the picture: no multipart envelope, no filename, no caller-declared content type is
/// trusted. The format is decided by the bytes, a non-image and an empty body are refused 400, and a
/// body over [`MAX_AVATAR_BYTES`] is refused 400 as well. One row per user (upsert), so re-uploading
/// replaces the picture rather than accumulating rows. Private: the caller must present their session.
pub async fn upload_avatar(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    body: Bytes,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    if body.is_empty() {
        return Err(AppError::BadRequest("No picture data received".to_string()));
    }
    if body.len() > MAX_AVATAR_BYTES {
        return Err(AppError::BadRequest(
            "Profile picture must be 2 MB or smaller".to_string(),
        ));
    }
    let content_type = sniff_image(&body).ok_or_else(|| {
        AppError::BadRequest("Unsupported picture — use a PNG, JPEG, GIF or WebP image".to_string())
    })?;

    sqlx::query(
        "INSERT INTO user_avatars (user_id, bytes, content_type, updated_at) VALUES ($1, $2, $3, NOW()) \
         ON CONFLICT (user_id) DO UPDATE SET bytes = EXCLUDED.bytes, \
         content_type = EXCLUDED.content_type, updated_at = NOW()",
    )
    .bind(user_id)
    .bind(body.as_ref())
    .bind(content_type)
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

    let mut resp = Response::new(Body::from(bytes));
    let ct = header::HeaderValue::from_str(&content_type)
        .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream"));
    resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    // NOT cached (card t_39cea779). The URL is stable per user, so ANY max-age would keep serving
    // the PREVIOUS face after a re-upload — the exact opposite of the "byte-identical" promise the
    // account screen depends on. The console adds its own cache-buster to the <img>; the endpoint
    // itself must always answer with the CURRENT bytes.
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(resp)
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
        match send_reset_email(&state, &user.email, &token).await {
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

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
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
