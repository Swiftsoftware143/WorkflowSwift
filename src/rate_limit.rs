use axum::{
    extract::{State, Request},
    http::StatusCode,
    middleware::Next,
    response::Response,
    Extension,
};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use governor::clock::DefaultClock;
use governor::middleware::NoOpMiddleware;
use governor::state::direct::NotKeyed;
use governor::state::InMemoryState;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::collections::HashMap;
use std::sync::Mutex;
use tracing::{warn, info};

use crate::AppState;
use crate::auth::models::Claims;

/// Per-tenant rate limiter keyed by tenant_id
#[derive(Clone)]
pub struct RateLimiters {
    /// Per-tenant rate limiters for API endpoints
    tenant_limiters: Arc<Mutex<HashMap<String, Arc<DefaultDirectRateLimiter>>>>,
    /// Default max requests per tenant per second
    max_per_second: u32,
    /// Default burst
    burst: u32,
}

impl RateLimiters {
    pub fn new(max_per_second: u32, burst: u32) -> Self {
        Self {
            tenant_limiters: Arc::new(Mutex::new(HashMap::new())),
            max_per_second,
            burst,
        }
    }

    /// Get or create a rate limiter for a tenant
    pub fn for_tenant(&self, tenant_id: &str) -> Arc<DefaultDirectRateLimiter> {
        let mut limiters = self.tenant_limiters.lock().unwrap();
        if let Some(limiter) = limiters.get(tenant_id) {
            return limiter.clone();
        }
        // Create a new rate limiter: max_per_second requests per second, with burst
        let quota = Quota::per_second(
            NonZeroU32::new(self.max_per_second).unwrap()
        )
        .allow_burst(
            NonZeroU32::new(self.burst).unwrap()
        );
        let limiter = Arc::new(RateLimiter::direct(quota));
        limiters.insert(tenant_id.to_string(), limiter.clone());
        limiter
    }

    /// Cleanup old limiters to prevent memory leaks
    pub fn cleanup(&self) {
        // This is a no-op for now; limiters are lightweight
        // In production, you'd periodically remove entries not seen in N minutes
    }
}

/// Middleware: rate limit by tenant ID
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    let tenant_id = &claims.aid;
    let limiter = state.rate_limiters.for_tenant(tenant_id);

    if limiter.check().is_err() {
        warn!("Rate limit exceeded for tenant {}", tenant_id);
        return Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header("Retry-After", "1")
            .body(axum::body::Body::from("{\"error\":\"Rate limit exceeded. Wait before retrying.\",\"status\":429}"))
            .unwrap_or_else(|_| Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .body(axum::body::Body::empty())
                .unwrap()
            );
    }

    next.run(request).await
}

/// Cached provider key resolution to avoid DB hits on every step execution
#[derive(Clone)]
pub struct ProviderKeyCache {
    /// Simple in-memory cache: tenant_id:provider -> (api_key, base_url, metadata)
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
    fn key(tenant_id: &str, provider: &str) -> String {
        format!("{}:{}", tenant_id, provider)
    }

    /// Get cached value, returns None if expired or missing
    pub fn get(&self, tenant_id: &str, provider: &str) -> Option<(String, Option<String>, serde_json::Value)> {
        let cache = self.cache.lock().unwrap();
        let key = Self::key(tenant_id, provider);
        if let Some((api_key, base_url, metadata, expires_at)) = cache.get(&key) {
            if *expires_at > chrono::Utc::now().timestamp() {
                return Some((api_key.clone(), base_url.clone(), metadata.clone()));
            }
        }
        None
    }

    /// Set a value in the cache
    pub fn set(&self, tenant_id: &str, provider: &str, api_key: String, base_url: Option<String>, metadata: serde_json::Value) {
        let mut cache = self.cache.lock().unwrap();
        let key = Self::key(tenant_id, provider);
        let expires_at = chrono::Utc::now().timestamp() + self.ttl_seconds;
        cache.insert(key, (api_key, base_url, metadata, expires_at));
    }

    /// Invalidate a specific entry (called when keys are updated/deleted)
    pub fn invalidate(&self, tenant_id: &str, provider: &str) {
        let mut cache = self.cache.lock().unwrap();
        let key = Self::key(tenant_id, provider);
        cache.remove(&key);
    }

    /// Invalidate all entries for a tenant
    pub fn invalidate_tenant(&self, tenant_id: &str) {
        let mut cache = self.cache.lock().unwrap();
        cache.retain(|k, _| !k.starts_with(&format!("{}:", tenant_id)));
    }

    /// Get total cached entries count
    pub fn len(&self) -> usize {
        self.cache.lock().unwrap().len()
    }
}
