use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use super::models::Claims;
use crate::error::AppError;
use crate::AppState;

// There is deliberately NO path allowlist in this module (kanban t_73f6724d).
//
// `auth_middleware` is layered on the `/api/v1`-nested protected router, so axum has already
// stripped the nest prefix from `req.uri().path()` before this layer runs: a live request to
// `/api/v1/plans` is handed `path=/plans` (measured 2026-10-02 — the AUTH_MIDDLEWARE log line).
// An earlier revision carried a list of `/api/v1/...` "public" paths plus an
// `industries/…/templates` prefix rule; every entry was unreachable, so the skip branch was dead
// code that nonetheless READ as a security control. Two ways that hurt: a future lane can add an
// entry believing it opens a route, or "fix" the comparison to be prefix-aware and silently flip
// `/api/v1/plans` from 401-anon to public.
//
// Deleted rather than repaired, because the deletion is behaviour-neutral (the list provably never
// matched anything the middleware is handed) while the repair is not. The app's one public surface
// is the `public_routes` sub-router in `src/routes.rs`, which carries no auth layer at all: a route
// is public by being registered THERE, never by appearing in a list here. The pin test below fails
// if an auth-skip list is added back.

pub async fn auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let path = req.uri().path().to_string();

    tracing::info!("AUTH_MIDDLEWARE: path={}", path);

    tracing::debug!(
        "auth_middleware: checking Authorization header for {}",
        path
    );

    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Unauthorized)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or_else(|| AppError::Unauthorized)?;

    // Two credential types are accepted on the same header, exactly as the
    // shipped Chrome extension presents them: a `workflowswift_` API key, or a
    // JWT. Each path is verified independently; neither weakens the other.
    let method = req.method().clone();
    let claims = if super::api_key_auth::is_api_key(token) {
        super::api_key_auth::authenticate(&state, token, &method).await?
    } else {
        verify_token(token, &state.config.jwt_secret).map_err(|_| AppError::Unauthorized)?
    };

    // A signature only proves the token was minted by us — not that the account it
    // names still exists. Signatures also outlive their account for the token's whole
    // 24 h lifetime, so a token minted before a tenant was deleted stays cryptographically
    // valid afterwards. Every handler below binds `claims.aid` straight into SQL as the
    // tenant: with the account gone that insert trips `workflows_tenant_id_fkey` and the
    // caller gets a 500 for what is really an authentication failure ("this credential
    // belongs to nobody"). Resolve the account here, once, so a dead tenant is refused
    // with 401 before any handler runs — and so no handler can forget the check.
    if !account_is_live(&state.db, &claims.aid).await? {
        return Err(AppError::Unauthorized);
    }

    // …and the USER behind the credential must still exist and be enabled (kanban t_53fcb666,
    // fleet-wide). `claims.sub` names a `users` row; everything downstream is built from the
    // claims, so deleting an account used to leave a working credential behind for its whole life
    // (measured on a sibling app 2026-10-20: the same token still answered 200 from /auth/me and
    // from a platform-admin read after the user + tenant rows were deleted). Deleting the row is
    // how the fleet remediates a compromised account and how its probe sweep cleans up throwaways,
    // so deletion has to BE the revocation. One primary-key read per authenticated request, on
    // BOTH credential doors — a JWT and an API key both carry the owning user's id in `sub`.
    //
    // The same read answers the DEMOTION half (kanban t_28184826). This app's platform authority is
    // the `perm_is_super_admin` FLAG, and every admin handler reads it from
    // `claims.perm_is_super_admin` (admin_settings_handler, credit_handler, site_handler,
    // user_handler, tickets_handler). So demoting a super admin — one `UPDATE users SET
    // perm_is_super_admin = false` — left their existing token holding the whole admin surface
    // until it expired. A token that CLAIMS the flag for a row that no longer has it is refused
    // here, once, instead of trusting each handler to notice.
    match user_grant(&state.db, &claims.sub).await? {
        Some((true, row_super, valid_from)) => {
            if super_admin_claim_outranks_row(claims.perm_is_super_admin, row_super) {
                tracing::warn!(
                    subject = %claims.sub,
                    "refused a super-admin claim the users row no longer grants"
                );
                return Err(AppError::Unauthorized);
            }
            // SESSION REVOCATION (kanban t_80787a0f): the same read returns the instant this
            // account last changed or reset its password, so a session minted before it stops here
            // — 401, exactly like a deleted account. NULL (never rotated) changes nothing. An API
            // key is minted fresh claims with `iat = now` per request, so a durable integration
            // credential is never caught by this rule.
            if session_predates_password_change(Some(claims.iat as i64), valid_from) {
                tracing::warn!(
                    subject = %claims.sub,
                    "refused a session minted before the account's password change"
                );
                return Err(AppError::Unauthorized);
            }
        }
        _ => return Err(AppError::Unauthorized),
    }

    let mut req = req;
    req.extensions_mut().insert(claims);

    Ok(next.run(req).await)
}

/// Is `aid` a live account?
///
/// `false` means the credential names a tenant that no longer exists, which is an
/// authentication failure, not a missing resource: callers must be refused 401, never
/// allowed to reach a handler that would bind the id into SQL and surface a 500.
///
/// `aid` arrives as a string from the token. An unparseable value is treated the same
/// as a deleted account — either way there is no tenant behind it.
async fn account_is_live(db: &sqlx::PgPool, aid: &str) -> Result<bool, AppError> {
    let Ok(aid) = uuid::Uuid::parse_str(aid) else {
        return Ok(false);
    };

    let exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM accounts WHERE id = $1)")
            .bind(aid)
            .fetch_one(db)
            .await?;

    Ok(exists)
}

/// Does a token's super-admin CLAIM exceed the authority its `users` row still grants?
/// (kanban t_28184826)
///
/// This app's platform authority is the `perm_is_super_admin` FLAG, not a role word, and every
/// admin handler authorises on `claims.perm_is_super_admin`. Since the flag is carried inside the
/// token, DEMOTING a super admin — one `UPDATE users SET perm_is_super_admin = false` — used to
/// leave the existing token holding the whole admin surface until it expired. Only a claim of the
/// flag over a row that no longer has it is refused; `None`/`false` claims (every API key, and every
/// ordinary session) are left exactly as they were.
pub fn super_admin_claim_outranks_row(claim: Option<bool>, row_grants: bool) -> bool {
    claim == Some(true) && !row_grants
}

/// What the user named by a credential's `sub` still grants: `(is_active, perm_is_super_admin)`.
///
/// The USER half of the boundary (kanban t_53fcb666): `account_is_live` answers for the workspace,
/// this answers for the person. `None` for a `sub` that is not even a uuid — such a claim names no
/// row, which is the same failure as a deleted account.
///
/// The DEMOTION half (kanban t_28184826) is the second field: `perm_is_super_admin` is the live
/// platform authority, so a token that still CARRIES the flag after the row lost it is refused.
async fn user_grant(
    db: &sqlx::PgPool,
    sub: &str,
) -> Result<Option<(bool, bool, Option<i64>)>, AppError> {
    let Ok(user_id) = uuid::Uuid::parse_str(sub) else {
        return Ok(None);
    };

    // The third field is `users.sessions_valid_from` as a unix epoch (`None` = this account never
    // changed or reset its password, so no session is restricted); a session whose `iat` predates
    // it is refused, which is what makes a password change revoke the sessions before it (kanban
    // t_80787a0f).
    let row: Option<(bool, bool, Option<i64>)> = sqlx::query_as(
        "SELECT is_active, perm_is_super_admin,
                floor(EXTRACT(EPOCH FROM sessions_valid_from))::bigint
           FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?;

    Ok(row)
}

/// Does a session minted at `jwt_iat` predate the instant its account last changed or reset its
/// password? (kanban t_80787a0f)
///
/// Pure, so both arms are unit-tested. `valid_from` is `users.sessions_valid_from` as a unix epoch
/// (`None` = never restricted, which is every account until it rotates its password, so every
/// token already in the wild keeps working). A session with no `iat` cannot prove it is newer than
/// the change, and the safe answer for a credential is "cannot prove → refuse". `iat` is second-granularity (and the change's own timestamp is floored to a second for this reason), so a session minted in the SAME second as the change cannot be told apart from a re-login and is allowed.
pub fn session_predates_password_change(jwt_iat: Option<i64>, valid_from: Option<i64>) -> bool {
    match valid_from {
        None => false,
        Some(v) => match jwt_iat {
            None => true,
            Some(iat) => iat < v,
        },
    }
}

pub fn verify_token(token: &str, secret: &str) -> Result<Claims, AppError> {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

    let decoding_key = DecodingKey::from_secret(secret.as_bytes());
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 30;
    validation.validate_exp = true;

    let token_data = decode::<Claims>(token, &decoding_key, &validation)?;
    Ok(token_data.claims)
}

pub fn create_token(claims: &Claims, secret: &str) -> Result<String, AppError> {
    use jsonwebtoken::{encode, EncodingKey, Header};

    let encoding_key = EncodingKey::from_secret(secret.as_bytes());
    Ok(encode(&Header::default(), claims, &encoding_key)?)
}

#[cfg(test)]
mod tests {
    use super::{session_predates_password_change, super_admin_claim_outranks_row};
    /// The reported defect (kanban t_28184826): this app's platform authority is the
    /// `perm_is_super_admin` flag carried in the token, read by every admin handler. Demoting a
    /// super admin (`UPDATE users SET perm_is_super_admin = false`) left the existing token holding
    /// the whole admin surface until it expired. A claim of the flag the row no longer grants must
    /// be refused, while every lower/absent claim keeps its current behaviour — including the API
    /// key door, which always mints `Some(false)`.
    #[test]
    fn a_super_admin_claim_the_row_no_longer_grants_is_refused() {
        assert!(super_admin_claim_outranks_row(Some(true), false));
        // A real super admin is untouched.
        assert!(!super_admin_claim_outranks_row(Some(true), true));
        // Every other claim is not this rule's business.
        assert!(!super_admin_claim_outranks_row(Some(false), true));
        assert!(!super_admin_claim_outranks_row(Some(false), false));
        assert!(!super_admin_claim_outranks_row(None, true));
        assert!(!super_admin_claim_outranks_row(None, false));
    }

    /// No path allowlist may live in this module (kanban t_73f6724d).
    ///
    /// MEASURED 2026-10-02, live: axum strips the `/api/v1` nest prefix before this layer's router
    /// runs, so the middleware is handed `/plans`, never `/api/v1/plans` (the live AUTH_MIDDLEWARE
    /// log line for a request to /api/v1/plans reads `path=/plans`), and that route answers 401 anon
    /// yet 200 with a token. A prefix-qualified "public path" list here is therefore unreachable —
    /// one existed and was deleted as dead code that read as a security control. The app's only
    /// public surface is the `public_routes` sub-router (src/routes.rs), which carries no auth layer.
    /// Adding an auth-skip list back here must be a test failure, not a silent auth widening.
    #[test]
    fn no_path_allowlist_in_the_auth_middleware() {
        let src = include_str!("middleware.rs");
        // Built at run time so neither needle can match its own source text.
        let allowlist = concat!("PUBLIC", "_PATHS");
        let skip_branch = concat!("fn ", "is_public_path");
        assert!(
            !src.contains(allowlist),
            "an auth-skip allowlist is back in the middleware"
        );
        assert!(
            !src.contains(skip_branch),
            "an auth-skip branch is back in the middleware"
        );
    }

    /// The reported defect (kanban t_80787a0f): changing or resetting the password revoked
    /// nothing. The rule is one comparison — a session minted BEFORE `users.sessions_valid_from`
    /// is refused — and it must change nothing else: a NULL timestamp (every account that never
    /// rotated), a session minted at or after the change, and a session with no `iat` on an
    /// unrestricted row all stay exactly as they were.
    #[test]
    fn a_session_minted_before_the_password_change_is_refused_and_nothing_else_is() {
        // NULL column: never restricted, so every token already in the wild keeps working.
        assert!(!session_predates_password_change(Some(1_000), None));
        // Minted BEFORE the change -> refused.
        assert!(session_predates_password_change(Some(1_000), Some(2_000)));
        // Minted AT the change or after it -> allowed (a re-login must work).
        assert!(!session_predates_password_change(Some(2_000), Some(2_000)));
        assert!(!session_predates_password_change(Some(3_000), Some(2_000)));
        // A session that cannot prove it is newer is refused rather than trusted.
        assert!(session_predates_password_change(None, Some(2_000)));
        assert!(!session_predates_password_change(None, None));
    }
}
