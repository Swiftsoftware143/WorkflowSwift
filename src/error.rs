use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Authentication required")]
    Unauthorized,

    #[error("Invalid credentials")]
    InvalidCredentials,

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Resource already exists: {0}")]
    Duplicate(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("{0}")]
    BadRequest(String),
    #[error("Upgrade required: {0}")]
    UpgradeRequired(String),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Hashing error: {0}")]
    Hash(String),

    #[error("Internal server error: {0}")]
    Internal(String),

    /// An upstream dependency (n8n, an integration target) refused or could not
    /// answer. The message is passed through to the caller on purpose: the whole
    /// point of this variant is that "the thing you asked for did not happen"
    /// must be visible, not hidden behind a generic 500 or a 200.
    #[error("Upstream failure: {0}")]
    Upstream(String),

    #[error("Too many requests: {0}")]
    TooManyRequests(String),

    #[error("JWT error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "Authentication required".to_string(),
            ),
            AppError::InvalidCredentials => {
                (StatusCode::UNAUTHORIZED, "Invalid credentials".to_string())
            }
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::Duplicate(msg) => (StatusCode::CONFLICT, msg.clone()),
            AppError::Validation(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg.clone()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::UpgradeRequired(msg) => (StatusCode::PAYMENT_REQUIRED, msg.clone()),
            AppError::Hash(msg) => {
                tracing::error!("Password hashing error: {}", msg);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Password hashing error".to_string(),
                )
            }
            AppError::Internal(msg) => {
                tracing::error!("Internal error: {}", msg);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".to_string(),
                )
            }
            AppError::Database(e) => {
                tracing::error!(error = %e, "Database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Database error".to_string(),
                )
            }
            AppError::TooManyRequests(msg) => (StatusCode::TOO_MANY_REQUESTS, msg.clone()),
            AppError::Upstream(msg) => {
                tracing::warn!(upstream_error = %msg, "Upstream dependency failed");
                (StatusCode::BAD_GATEWAY, msg.clone())
            }
            AppError::Jwt(e) => {
                tracing::warn!(error = %e, "JWT error");
                (StatusCode::UNAUTHORIZED, "Invalid token".to_string())
            }
        };

        let body = Json(json!({
            "error": true,
            "message": message,
            "code": status.as_u16()
        }));

        (status, body).into_response()
    }
}

pub type ApiResult<T> = Result<T, AppError>;
