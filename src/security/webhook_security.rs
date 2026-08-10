//! Webhook Security — domain allowlisting + daily rate limiting for integration targets.
//!
//! Any outbound webhook from the platform must pass two gates:
//! 1. Domain allowlist — the hostname of the webhook URL must be in the target's active
//!    allowlist, unless the allowlist is empty (allow all).
//! 2. Daily rate cap — each integration target has a configurable daily limit. We count
//!    delivery_log entries for that target URL today and reject if over the limit.

use crate::error::AppError;
use sqlx::PgPool;
use url::Url;

/// Validate a webhook URL against the target's allowed domains list.
/// Returns Ok(()) if the domain passes, Err with a descriptive message otherwise.
pub fn validate_webhook_url(webhook_url: &str, allowed_domains: &[String]) -> Result<(), String> {
    let parsed = Url::parse(webhook_url)
        .map_err(|e| format!("Invalid webhook URL '{}': {}", webhook_url, e))?;

    let host = parsed
        .host_str()
        .ok_or_else(|| format!("Webhook URL '{}' has no host component", webhook_url))?;

    // If the allowed_domains list is empty, all domains are permitted
    if allowed_domains.is_empty() {
        return Ok(());
    }

    // Check if the hostname (or any subdomain of it) matches any allowed domain
    let host_lower = host.to_lowercase();
    for domain in allowed_domains {
        let domain_lower = domain.trim().to_lowercase();
        if host_lower == domain_lower || host_lower.ends_with(&format!(".{}", domain_lower)) {
            return Ok(());
        }
    }

    Err(format!(
        "Webhook URL domain '{}' is not in the allowed domains list: {:?}",
        host, allowed_domains
    ))
}

/// Check whether a given integration target has exceeded its daily webhook limit.
/// Returns Ok(true) if the target can fire, Ok(false) if over limit, or Err on DB failure.
pub async fn check_daily_limit(
    pool: &PgPool,
    target_id: &uuid::Uuid,
    daily_limit: i32,
) -> Result<bool, String> {
    if daily_limit <= 0 {
        return Err("Daily limit must be greater than 0".to_string());
    }

    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM delivery_log
           WHERE target = (SELECT webhook_url FROM integration_targets WHERE id = $1)
             AND attempted_at >= date_trunc('day', now() AT TIME ZONE 'UTC')::timestamptz
             AND attempted_at < date_trunc('day', now() AT TIME ZONE 'UTC')::timestamptz + INTERVAL '1 day'"#
    )
    .bind(target_id)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("DB error checking daily limit: {}", e))?;

    if count >= daily_limit as i64 {
        return Ok(false);
    }

    Ok(true)
}

/// Run both security checks before delivering a webhook.
/// Returns Ok(()) if all checks pass, AppError with descriptive message otherwise.
pub async fn check_webhook_security(
    pool: &PgPool,
    target_id: &uuid::Uuid,
    webhook_url: &str,
    allowed_domains: &[String],
    daily_limit: i32,
) -> Result<(), AppError> {
    // 1. Domain allowlist check
    validate_webhook_url(webhook_url, allowed_domains).map_err(|msg| {
        AppError::Forbidden(format!("Webhook blocked by security policy: {}", msg))
    })?;

    // 2. Daily limit check
    let within_limit = check_daily_limit(pool, target_id, daily_limit)
        .await
        .map_err(|msg| AppError::Internal(format!("Security check error: {}", msg)))?;

    if !within_limit {
        return Err(AppError::TooManyRequests(format!(
            "Webhook blocked by daily limit ({} calls/day). Reset at midnight UTC.",
            daily_limit
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_webhook_url_empty_allowlist() {
        assert!(validate_webhook_url("https://example.com/hook", &[]).is_ok());
        assert!(validate_webhook_url("http://evil.net/callback", &[]).is_ok());
    }

    #[test]
    fn test_validate_webhook_url_exact_match() {
        let domains = vec!["example.com".to_string(), "api.good.com".to_string()];
        assert!(validate_webhook_url("https://example.com/hook", &domains).is_ok());
        assert!(validate_webhook_url("https://api.good.com/v1/callback", &domains).is_ok());
    }

    #[test]
    fn test_validate_webhook_url_subdomain_match() {
        let domains = vec!["example.com".to_string()];
        assert!(validate_webhook_url("https://hooks.example.com/path", &domains).is_ok());
        assert!(validate_webhook_url("https://sub.hooks.example.com/path", &domains).is_ok());
    }

    #[test]
    fn test_validate_webhook_url_rejected() {
        let domains = vec!["example.com".to_string()];
        assert!(validate_webhook_url("https://evil.com/hook", &domains).is_err());
        assert!(validate_webhook_url("https://example.evil.com/hook", &domains).is_err());
        assert!(validate_webhook_url("http://192.168.1.1/pwn", &domains).is_err());
    }

    #[test]
    fn test_validate_webhook_url_case_insensitive() {
        let domains = vec!["EXAMPLE.COM".to_string()];
        assert!(validate_webhook_url("https://example.com/hook", &domains).is_ok());
        assert!(validate_webhook_url("https://Example.COM/Hook", &domains).is_ok());
    }

    #[test]
    fn test_validate_webhook_url_invalid_url() {
        let domains = vec!["example.com".to_string()];
        assert!(validate_webhook_url("not-a-url", &domains).is_err());
        assert!(validate_webhook_url("", &domains).is_err());
    }
}
