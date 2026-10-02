use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use std::sync::OnceLock;

use super::models::Claims;
use crate::error::AppError;
use crate::AppState;

static PUBLIC_PATHS: OnceLock<Vec<String>> = OnceLock::new();

fn is_public_path(path: &str) -> bool {
    let paths = PUBLIC_PATHS.get_or_init(|| {
        vec![
            "/api/v1/auth/login".to_string(),
            "/api/v1/auth/register".to_string(),
            "/api/v1/industries".to_string(),
            "/api/v1/auth/forgot-password".to_string(),
            "/api/v1/auth/reset-password".to_string(),
            "/api/v1/health".to_string(),
            // The admin-sync name is deliberately NOT here (kanban t_b551a9a5): WorkflowSwift
            // registers no such route — the app's only portfolio-sync is the internal,
            // x-internal-key guarded POST /api/v1/internal/portfolio-sync (src/routes.rs) — and
            // no caller asks for an admin one (CoreSwift's hub posts to the internal variant; the
            // admin console's Portfolio Companies panel calls /admin/portfolio-companies; 0 n8n
            // workflows, 0 scripts, 0 served shells). A PUBLIC_PATHS entry is an auth SKIP, so an
            // unused one is a trap, not a placeholder.
            "/api/v1/plans".to_string(),
            "/api/v1/plans/".to_string(),
        ]
    });

    if paths.contains(&path.to_string()) {
        return true;
    }

    if path.starts_with("/api/v1/industries/") && path.ends_with("/templates") {
        return true;
    }

    false
}

pub async fn auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let path = req.uri().path().to_string();

    tracing::info!("AUTH_MIDDLEWARE: path={}", path);

    if is_public_path(&path) {
        return Ok(next.run(req).await);
    }

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
    use super::is_public_path;

    /// The list as declared, plus its one prefix rule: the paths the middleware is *written* to
    /// treat as public.
    #[test]
    fn declared_public_paths_and_prefix_rule_match() {
        assert!(is_public_path("/api/v1/auth/login"));
        assert!(is_public_path("/api/v1/auth/register"));
        assert!(is_public_path("/api/v1/auth/forgot-password"));
        assert!(is_public_path("/api/v1/auth/reset-password"));
        assert!(is_public_path("/api/v1/industries"));
        assert!(is_public_path("/api/v1/health"));
        assert!(is_public_path("/api/v1/plans"));
        assert!(is_public_path("/api/v1/industries/abc/templates"));
    }

    /// Tenant surfaces, and the dead admin-sync name this card removed, stay behind the
    /// credential check — in both spellings, because `is_public_path` is asked about whatever
    /// the router sees (kanban t_b551a9a5).
    #[test]
    fn tenant_surfaces_are_not_public() {
        assert!(!is_public_path("/api/v1/workflows"));
        assert!(!is_public_path("/api/v1/instances"));
        assert!(!is_public_path("/api/v1/admin/portfolio-companies"));
        assert!(!is_public_path("/api/v1/n8n/ai-action"));
        assert!(!is_public_path("/api/v1/admin/portfolio-sync"));
        assert!(!is_public_path("/admin/portfolio-sync"));
    }

    /// MEASURED GAP (kanban t_b551a9a5, 2026-10-02). axum strips the `/api/v1` nest prefix
    /// before the router's layers run, so this middleware is handed `/plans`, never
    /// `/api/v1/plans`: live, the AUTH_MIDDLEWARE log line for a request to /api/v1/plans reads
    /// `path=/plans`, and that route answers 401 anon yet 200 with a token. Every
    /// prefix-qualified entry above — and the `/api/v1/industries/…/templates` rule — is
    /// therefore unreachable, and the app's real public surface is the `public_routes`
    /// sub-router in src/routes.rs. Pinned here so the next lane fixes the list deliberately
    /// (its own card) instead of re-adding dead entries.
    #[test]
    fn prefixed_entries_never_match_what_the_middleware_is_handed() {
        assert!(!is_public_path("/plans"));
        assert!(!is_public_path("/industries"));
        assert!(!is_public_path("/health"));
    }
}
