use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
    Extension,
};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::Mutex;
use tracing::warn;

use crate::auth::models::Claims;
use crate::AppState;

/// Bucket shared by every identity once the limiter table is at capacity.
const OVERFLOW_BUCKET: &str = "__overflow__";

/// Take a limiter-table lock, recovering from poisoning instead of panicking: this runs
/// on every request, so a panic elsewhere in the process must not become a permanent 500.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Per-account rate limiter keyed by aid
#[derive(Clone)]
pub struct RateLimiters {
    /// Per-account rate limiters for API endpoints
    account_limiters: Arc<Mutex<HashMap<String, Arc<DefaultDirectRateLimiter>>>>,
    /// Default max requests per account per second
    max_per_second: u32,
    /// Default burst
    burst: u32,
    /// Upper bound on the number of buckets held (0 = unbounded)
    max_entries: usize,
}

impl RateLimiters {
    pub fn new(max_per_second: u32, burst: u32) -> Self {
        Self::with_capacity(max_per_second, burst, 10_000)
    }

    /// `max_entries` bounds the bucket table. Callers of the pre-auth limiter are keyed by
    /// a client-controlled identity, so an unbounded table would be a memory-exhaustion
    /// vector of its own.
    pub fn with_capacity(max_per_second: u32, burst: u32, max_entries: usize) -> Self {
        Self {
            account_limiters: Arc::new(Mutex::new(HashMap::new())),
            max_per_second,
            burst,
            max_entries,
        }
    }

    /// Get or create a rate limiter for an account (or, for the pre-auth limiter, a
    /// client identity).
    pub fn for_account(&self, aid: &str) -> Arc<DefaultDirectRateLimiter> {
        let mut limiters = lock(&self.account_limiters);
        if let Some(limiter) = limiters.get(aid) {
            return limiter.clone();
        }
        // Table full: new identities share one bucket instead of growing the map.
        let key = if self.max_entries > 0 && limiters.len() >= self.max_entries {
            warn!(
                "rate limiter table at capacity ({} buckets) — new identities share one bucket",
                limiters.len()
            );
            OVERFLOW_BUCKET
        } else {
            aid
        };
        if let Some(limiter) = limiters.get(key) {
            return limiter.clone();
        }
        // Create a new rate limiter: max_per_second requests per second, with burst
        let quota = Quota::per_second(NonZeroU32::new(self.max_per_second.max(1)).unwrap())
            .allow_burst(NonZeroU32::new(self.burst.max(1)).unwrap());
        let limiter = Arc::new(RateLimiter::direct(quota));
        limiters.insert(key.to_string(), limiter.clone());
        limiter
    }

    /// Cleanup old limiters to prevent memory leaks
    pub fn cleanup(&self) {
        // This is a no-op for now; limiters are lightweight
        // In production, you'd periodically remove entries not seen in N minutes
    }
}

/// The 429 every limiter answers with. Fallible builders are handled, never unwrapped:
/// an empty 429 is still a 429 and must not panic a request path.
fn rate_limited_response() -> Response {
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("Retry-After", "1")
        .body(axum::body::Body::from(
            "{\"error\":\"Rate limit exceeded. Wait before retrying.\",\"status\":429}",
        ))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .body(axum::body::Body::empty())
                .unwrap_or_else(|_| Response::new(axum::body::Body::empty()))
        })
}

/// Client identity for the pre-auth limiter, read from headers the fronting proxy sets:
/// Cloudflare's `CF-Connecting-IP`, then nginx's `X-Real-IP` (`proxy_set_header X-Real-IP
/// $remote_addr`, which replaces any client-supplied value), then the rightmost
/// `X-Forwarded-For` hop — the one nginx appended, not the spoofable left-hand entries.
/// A caller presenting none of them shares the `unknown` bucket, so omitting the address
/// is not a way around the limiter.
fn client_identity(headers: &axum::http::HeaderMap) -> String {
    let read = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    if let Some(ip) = read("cf-connecting-ip") {
        return ip;
    }
    if let Some(ip) = read("x-real-ip") {
        return ip;
    }
    if let Some(xff) = read("x-forwarded-for") {
        if let Some(last) = xff
            .rsplit(',')
            .map(|hop| hop.trim())
            .find(|h| !h.is_empty())
        {
            return last.to_string();
        }
    }
    "unknown".to_string()
}

/// Middleware: throttle a request BEFORE any credential is verified.
///
/// `rate_limit_middleware` cannot do this: it extracts `Claims`, which exist only after
/// `auth_middleware` has verified the credential — and an API key is verified with Argon2
/// against stored hashes, so that is real CPU. In axum the LAST `.layer()` call is the
/// OUTERMOST, so `routes.rs` layers this one after auth_middleware and it runs first.
///
/// It applies to API-key credentials, the only credential class on this router whose
/// verification is expensive (a JWT is an HMAC check): a caller presenting
/// `workflowswift_...` junk is answered 429 before any hash work is queued. The limit sits
/// above the per-account limit (30/s, applied after auth), so a well-behaved client is
/// never throttled here before its own account budget runs out. Neither the credential nor
/// a hash of it is logged.
pub async fn pre_auth_rate_limit_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    let credential_len = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|token| token.trim())
        .filter(|token| crate::auth::api_key_auth::is_api_key(token))
        .map(|token| token.len());

    if let Some(len) = credential_len {
        let identity = client_identity(request.headers());
        let limiter = state.pre_auth_limiters.for_account(&identity);
        if limiter.check().is_err() {
            warn!(
                "Pre-auth rate limit exceeded for {} (api key credential, {} chars)",
                identity, len
            );
            return rate_limited_response();
        }
    }

    next.run(request).await
}

/// Middleware: rate limit by account ID
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    let aid = &claims.aid;
    let limiter = state.rate_limiters.for_account(aid);

    if limiter.check().is_err() {
        warn!("Rate limit exceeded for account {}", aid);
        return rate_limited_response();
    }

    next.run(request).await
}

/// Cached provider key resolution to avoid DB hits on every step execution
#[derive(Clone)]
pub struct ProviderKeyCache {
    /// Simple in-memory cache: aid:provider -> (api_key, base_url, metadata)
    cache: Arc<Mutex<HashMap<String, (String, Option<String>, serde_json::Value, i64)>>>,
    /// TTL in seconds
    ttl_seconds: i64,
}

impl ProviderKeyCache {
    pub fn new(ttl_seconds: i64) -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            ttl_seconds,
        }
    }

    /// Build the cache key
    fn key(aid: &str, provider: &str) -> String {
        format!("{}:{}", aid, provider)
    }

    /// Get cached value, returns None if expired or missing
    pub fn get(
        &self,
        aid: &str,
        provider: &str,
    ) -> Option<(String, Option<String>, serde_json::Value)> {
        let cache = self.cache.lock().unwrap();
        let key = Self::key(aid, provider);
        if let Some((api_key, base_url, metadata, expires_at)) = cache.get(&key) {
            if *expires_at > chrono::Utc::now().timestamp() {
                return Some((api_key.clone(), base_url.clone(), metadata.clone()));
            }
        }
        None
    }

    /// Set a value in the cache
    pub fn set(
        &self,
        aid: &str,
        provider: &str,
        api_key: String,
        base_url: Option<String>,
        metadata: serde_json::Value,
    ) {
        let mut cache = self.cache.lock().unwrap();
        let key = Self::key(aid, provider);
        let expires_at = chrono::Utc::now().timestamp() + self.ttl_seconds;
        cache.insert(key, (api_key, base_url, metadata, expires_at));
    }

    /// Invalidate a specific entry (called when keys are updated/deleted)
    pub fn invalidate(&self, aid: &str, provider: &str) {
        let mut cache = self.cache.lock().unwrap();
        let key = Self::key(aid, provider);
        cache.remove(&key);
    }

    /// Invalidate all entries for an account
    pub fn invalidate_account(&self, aid: &str) {
        let mut cache = self.cache.lock().unwrap();
        cache.retain(|k, _| !k.starts_with(&format!("{}:", aid)));
    }

    /// Get total cached entries count
    pub fn len(&self) -> usize {
        self.cache.lock().unwrap().len()
    }
}
