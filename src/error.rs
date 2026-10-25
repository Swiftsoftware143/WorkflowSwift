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

    /// The caller IS signed in but the `current_password` they supplied does not match the stored
    /// one (programme card t_2cb77960, this app's card t_39cea779). Deliberately distinct from
    /// [`AppError::InvalidCredentials`]: a wrong CURRENT password on the change-password form must
    /// name the field the user has to fix — the fleet contract is exactly the sentence
    /// "Current password is incorrect" — instead of the generic "Invalid credentials" a failed
    /// sign-in answers, which would leave the account screen with nothing useful to show.
    #[error("Current password is incorrect")]
    InvalidPassword,

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

    /// A request body bigger than the upload envelope. **413**, carrying the sentence the file
    /// picker shows, because this variant exists to stop the extractor's own words
    /// ("length limit exceeded") reaching a customer who simply chose a large photo.
    #[error("{0}")]
    PayloadTooLarge(String),
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

    /// An upstream provider (the CoreSwift hub) was reached and REFUSED, or could not be
    /// reached at all. **424 Failed Dependency**, carrying the provider's own words.
    ///
    /// WHY 424 AND NOT 400/500: the manual CoreSwift push discarded the hub's reason entirely
    /// (the shared helper returned a bare `bool`), so a rejected key was reported to the
    /// operator as a generic "push failed or account not connected". 424 says "the action
    /// depended on another action and that action failed" — exactly true — and unlike 502 it is
    /// not rewritten by the CDN edge.
    #[error("Upstream refused: {0}")]
    UpstreamRefused(String),

    #[error("Too many requests: {0}")]
    TooManyRequests(String),

    /// 429 — too many FAILED sign-ins from one client address (`crate::login_throttle`). Its
    /// own variant so the answer carries `Retry-After` as well as a sentence naming how long
    /// to wait; `TooManyRequests` above is the provider/notification throttle, which has no
    /// retry window to name.
    #[error("Too many failed sign-ins")]
    TooManyAttempts { retry_after_secs: u64 },

    #[error("JWT error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // The paused-sign-in answer carries its retry budget in a header as well as in the
        // sentence, so a client (and a log reader) can obey the pause without parsing prose.
        let retry_after_secs = match &self {
            AppError::TooManyAttempts { retry_after_secs } => Some(*retry_after_secs),
            _ => None,
        };
        let (status, message) = match &self {
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "Authentication required".to_string(),
            ),
            AppError::InvalidCredentials => {
                (StatusCode::UNAUTHORIZED, "Invalid credentials".to_string())
            }
            AppError::InvalidPassword => (
                StatusCode::UNAUTHORIZED,
                "Current password is incorrect".to_string(),
            ),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::Duplicate(msg) => (StatusCode::CONFLICT, msg.clone()),
            AppError::Validation(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg.clone()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::PayloadTooLarge(msg) => (StatusCode::PAYLOAD_TOO_LARGE, msg.clone()),
            AppError::UpgradeRequired(msg) => (StatusCode::PAYMENT_REQUIRED, msg.clone()),
            AppError::UpstreamRefused(msg) => {
                tracing::warn!(upstream_error = %msg, "Upstream provider refused");
                (StatusCode::FAILED_DEPENDENCY, msg.clone())
            }
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
            AppError::TooManyAttempts { retry_after_secs } => {
                let minutes = retry_after_secs.div_ceil(60).max(1);
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "Too many failed sign-in attempts from this address. Try again in about \
                         {minutes} minute{}. If you have forgotten your password, use \
                         \"Forgot your password?\".",
                        if minutes == 1 { "" } else { "s" }
                    ),
                )
            }
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

        let mut response = (status, body).into_response();
        if let Some(value) = retry_after_secs
            .and_then(|secs| axum::http::HeaderValue::from_str(&secs.to_string()).ok())
        {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, value);
        }
        response
    }
}

pub type ApiResult<T> = Result<T, AppError>;
