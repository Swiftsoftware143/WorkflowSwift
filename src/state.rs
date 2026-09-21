use sqlx::PgPool;

use crate::config::AppConfig;
use crate::rate_limit::{AuthInFlight, ProviderKeyCache, RateLimiters};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: AppConfig,
    pub rate_limiters: RateLimiters,
    /// Same machinery, different key and purpose: layered OUTSIDE auth so a credential is
    /// throttled before it is verified (an API key costs an Argon2 check to verify). Keyed
    /// by client identity, never by an account id — there are no claims to key on yet.
    pub pre_auth_limiters: RateLimiters,
    /// Load-shedding bound on the unauthenticated password-auth routes. The semaphore inside
    /// bounds *how much work* a password flood can do; this bounds *how many requests* may be
    /// waiting for that work at once. Keyed by nothing — it counts the process, not a caller.
    pub auth_in_flight: AuthInFlight,
    pub provider_key_cache: ProviderKeyCache,
}
