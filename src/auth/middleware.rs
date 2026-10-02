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
}
