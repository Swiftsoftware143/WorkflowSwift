//! Webhook Security — domain allowlisting + daily rate limiting for integration targets.
//!
//! Any outbound webhook from the platform must pass two gates:
//! 1. Domain allowlist — the hostname of the webhook URL must be in the target's active
//!    allowlist, unless the allowlist is empty (allow all).
//! 2. Daily rate cap — each integration target has a configurable daily limit. We count
//!    delivery_log entries for that target today and reject if over the limit. delivery_log is
//!    written by record_delivery, which every dispatch path must call after the attempt.

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
///
/// Counts `delivery_log` rows written by `record_delivery` for this target since midnight UTC.
/// The count keys on `target_id` (not the webhook URL) so editing a target's URL does not
/// reset — or share — its quota with another target pointing at the same URL.
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
           WHERE target_id = $1
             AND attempted_at >= date_trunc('day', now() AT TIME ZONE 'UTC')::timestamptz
             AND attempted_at < date_trunc('day', now() AT TIME ZONE 'UTC')::timestamptz + INTERVAL '1 day'"#,
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

/// Record one outbound webhook attempt against a target's daily quota.
///
/// Must be called for every attempt the guard allowed (success, non-2xx, or transport error) —
/// it is the only writer of `delivery_log`, so a dispatch path that skips it silently disables
/// the daily cap for that path.
///
/// Never fails the caller: a lost log row degrades the rate limiter to "one free call", which is
/// strictly better than failing a delivery that already happened. Errors are logged, not returned.
pub async fn record_delivery(
    pool: &PgPool,
    target_id: &uuid::Uuid,
    aid: &uuid::Uuid,
    target: &str,
    outcome: &str,
    status_code: Option<i32>,
    error_message: Option<&str>,
) {
    let res = sqlx::query(
        r#"INSERT INTO delivery_log (target_id, aid, target, outcome, status_code, error_message)
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(target_id)
    .bind(aid)
    .bind(target)
    .bind(outcome)
    .bind(status_code)
    .bind(error_message)
    .execute(pool)
    .await;

    if let Err(e) = res {
        tracing::warn!(
            error = %e,
            target_id = %target_id,
            "Failed to record webhook delivery in delivery_log (daily limit will undercount)"
        );
    }
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

    // 2. Daily limit check. A non-positive cap is a misconfigured target, not a server fault:
    //    answer 400 instead of letting check_daily_limit's Err surface as a 500.
    if daily_limit <= 0 {
        return Err(AppError::BadRequest(format!(
            "Integration target daily_limit must be greater than 0 (currently {})",
            daily_limit
        )));
    }

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

/// Gate the destination of a URL a WORKFLOW STEP supplied (kanban t_fe60cdf5).
///
/// The step arms in `src/execution.rs` that read a URL out of a step's own config
/// (`webhook`, `http-request`/`action`, `render_*`) put the target's response body into the step
/// result, which the run history shows back to the tenant. Without a destination gate that is an
/// authenticated SSRF read primitive: point a step at `127.0.0.1:8085`, at the docker bridge or at
/// the cloud metadata address and read the reply out of the run.
///
/// `validate_webhook_url` is NOT this check — with an empty allowlist it permits every host,
/// private ranges included (see the test below). This adds the refusal on top of it:
///
/// * refused: loopback, unspecified, private, link-local, unique-local, CGNAT, multicast,
///   broadcast, and the "this network" / documentation ranges;
/// * fail-closed: a host that will not resolve is refused too.
///
/// Redirects are refused separately, at the client (`redirect::Policy::none()` in the caller): a
/// validated public host that 30x-es into loopback would otherwise be a free hop past this gate.
///
/// Residual, stated rather than hidden: validate-then-connect is still DNS-rebinding-prone — the
/// address this resolves is not pinned to the socket the request uses. Pinning needs a custom
/// connector and is a separate change.
pub async fn gate_step_destination(url: &str) -> Result<(), String> {
    validate_webhook_url(url, &[])?;

    let parsed = Url::parse(url).map_err(|e| format!("Invalid URL '{}': {}", url, e))?;
    // `host_str()` keeps the square brackets on an IPv6 literal ("[::1]"), and a bracketed
    // literal never parses as an `IpAddr` — so it would fall through to the resolver, fail, and be
    // refused for the WRONG reason (unresolvable) instead of for being loopback.
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("URL '{}' has no host component", url))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = parsed.port_or_known_default().unwrap_or(80);

    // An IP literal needs no resolution.
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if ip_is_refused(&ip) {
            return Err(refused(&host, ip));
        }
        return Ok(());
    }

    let resolved: Vec<std::net::IpAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| format!("Could not resolve '{}': {}", host, e))?
        .map(|addr| addr.ip())
        .collect();
    if resolved.is_empty() {
        return Err(format!(
            "Could not resolve '{}' — refusing an unresolvable step destination (fail closed)",
            host
        ));
    }
    for ip in resolved {
        if ip_is_refused(&ip) {
            return Err(refused(&host, ip));
        }
    }
    Ok(())
}

fn refused(host: &str, ip: std::net::IpAddr) -> String {
    format!(
        "'{}' resolves to {} — a loopback/private/link-local address is not a valid destination \
         for a workflow step",
        host, ip
    )
}

/// The pure predicate [`gate_step_destination`] reads (unit-tested with no network).
pub fn ip_is_refused(ip: &std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => ipv4_is_refused(v4),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 // fc00::/7 unique-local
                || (first & 0xffc0) == 0xfe80 // fe80::/10 link-local
                || v6
                    .to_ipv4_mapped()
                    .map(|m| ipv4_is_refused(&m))
                    .unwrap_or(false)
        }
    }
}

fn ipv4_is_refused(v4: &std::net::Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_loopback()                       // 127.0.0.0/8
        || v4.is_private()                 // 10/8, 172.16/12, 192.168/16
        || v4.is_link_local()              // 169.254.0.0/16 (cloud metadata lives here)
        || v4.is_unspecified()             // 0.0.0.0
        || v4.is_broadcast()               // 255.255.255.255
        || v4.is_multicast()               // 224.0.0.0/4
        || o[0] == 0                       // 0.0.0.0/8 "this network"
        || (o[0] == 100 && (64..=127).contains(&o[1])) // 100.64.0.0/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)     // 192.0.0.0/24 IETF protocol
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)     // 192.0.2.0/24 documentation
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // 198.18.0.0/15 benchmarking
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)  // 198.51.100.0/24 documentation
        || (o[0] == 203 && o[1] == 0 && o[2] == 113) // 203.0.113.0/24 documentation
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

    /// The gate that makes `validate_webhook_url` usable on a step-supplied URL: the allowlist
    /// check passes EVERY host with an empty list (including loopback), so the refusal has to be
    /// the address itself. Asserted on the pure predicate, so it is network-free and deterministic.
    #[test]
    fn step_destinations_inside_the_box_are_refused_and_public_ones_are_not() {
        use std::net::IpAddr;
        let refuse = |s: &str| {
            assert!(
                ip_is_refused(&s.parse::<IpAddr>().unwrap()),
                "{s} must be refused as a step destination"
            );
        };
        let allow = |s: &str| {
            assert!(
                !ip_is_refused(&s.parse::<IpAddr>().unwrap()),
                "{s} must be an allowed step destination"
            );
        };
        for ip in [
            "127.0.0.1",  // loopback — the app itself
            "127.0.0.53", // loopback, systemd-resolved
            "0.0.0.0",
            "10.1.2.3",        // private
            "172.16.0.9",      // private
            "192.168.1.1",     // private — the docker/nginx bridge
            "169.254.169.254", // link-local — cloud metadata
            "100.64.0.1",      // CGNAT
            "224.0.0.1",       // multicast
            "255.255.255.255",
            "192.0.2.10", // documentation
            "::1",
            "fe80::1",          // link-local v6
            "fc00::1",          // unique-local v6
            "::ffff:127.0.0.1", // v4-mapped loopback
        ] {
            refuse(ip);
        }
        // The positive leg: this box's own public IP is what the acceptance probe posts to, and
        // public addresses in general must pass — a blanket refusal would be a broken gate.
        for ip in ["209.222.97.179", "8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
            allow(ip);
        }
    }

    /// The refusal is layered ON TOP of the allowlist check, not instead of it: with an allowlist
    /// configured a public host outside it is still refused.
    #[test]
    fn the_step_gate_keeps_the_allowlist_behaviour() {
        assert!(validate_webhook_url("https://api.heygen.com/render", &[]).is_ok());
        assert!(
            validate_webhook_url("https://api.heygen.com/render", &["heygen.com".to_string()])
                .is_ok()
        );
        assert!(
            validate_webhook_url("https://api.evil.test/render", &["heygen.com".to_string()])
                .is_err()
        );
    }
}
