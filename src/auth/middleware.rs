use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use std::sync::OnceLock;

use crate::AppState;
use crate::error::AppError;
use super::models::Claims;

static PUBLIC_PATHS: OnceLock<Vec<String>> = OnceLock::new();

fn is_public_path(path: &str) -> bool {
    let paths = PUBLIC_PATHS.get_or_init(|| {
        vec![
            "/api/v1/auth/login".to_string(),
            "/api/v1/auth/register".to_string(),
            "/api/v1/auth/forgot-password".to_string(),
            "/api/v1/auth/reset-password".to_string(),
            "/api/v1/health".to_string(),
            "/api/v1/admin/portfolio-sync".to_string(),
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

    if is_public_path(&path) {
        return Ok(next.run(req).await);
    }

    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Unauthorized)?;

    let token = auth_header.strip_prefix("Bearer ")
        .ok_or_else(|| AppError::Unauthorized)?;

    let claims = verify_token(token, &state.config.jwt_secret)
        .map_err(|_| AppError::Unauthorized)?;

    let mut req = req;
    req.extensions_mut().insert(claims);

    Ok(next.run(req).await)
}

pub fn verify_token(token: &str, secret: &str) -> Result<Claims, AppError> {
    use jsonwebtoken::{decode, DecodingKey, Validation, Algorithm};

    let decoding_key = DecodingKey::from_secret(secret.as_bytes());
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 30;
    validation.validate_exp = true;

    let token_data = decode::<Claims>(token, &decoding_key, &validation)?;
    Ok(token_data.claims)
}

pub fn create_token(claims: &Claims, secret: &str) -> Result<String, AppError> {
    use jsonwebtoken::{encode, Header, EncodingKey};

    let encoding_key = EncodingKey::from_secret(secret.as_bytes());
    Ok(encode(&Header::default(), claims, &encoding_key)?)
}
