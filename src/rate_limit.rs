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

/// Per-account rate limiter keyed by aid
#[derive(Clone)]
pub struct RateLimiters {
    /// Per-account rate limiters for API endpoints
    account_limiters: Arc<Mutex<HashMap<String, Arc<DefaultDirectRateLimiter>>>>,
    /// Default max requests per account per second
    max_per_second: u32,
    /// Default burst
    burst: u32,
}

impl RateLimiters {
    pub fn new(max_per_second: u32, burst: u32) -> Self {
        Self {
            account_limiters: Arc::new(Mutex::new(HashMap::new())),
            max_per_second,
            burst,
        }
    }

    /// Get or create a rate limiter for an account
    pub fn for_account(&self, aid: &str) -> Arc<DefaultDirectRateLimiter> {
        let mut limiters = self.account_limiters.lock().unwrap();
        if let Some(limiter) = limiters.get(aid) {
            return limiter.clone();
        }
        // Create a new rate limiter: max_per_second requests per second, with burst
        let quota = Quota::per_second(NonZeroU32::new(self.max_per_second).unwrap())
            .allow_burst(NonZeroU32::new(self.burst).unwrap());
        let limiter = Arc::new(RateLimiter::direct(quota));
        limiters.insert(aid.to_string(), limiter.clone());
        limiter
    }

    /// Cleanup old limiters to prevent memory leaks
    pub fn cleanup(&self) {
        // This is a no-op for now; limiters are lightweight
        // In production, you'd periodically remove entries not seen in N minutes
    }
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
        return Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header("Retry-After", "1")
            .body(axum::body::Body::from(
                "{\"error\":\"Rate limit exceeded. Wait before retrying.\",\"status\":429}",
            ))
            .unwrap_or_else(|_| {
                Response::builder()
                    .status(StatusCode::TOO_MANY_REQUESTS)
                    .body(axum::body::Body::empty())
                    .unwrap()
            });
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
