use sqlx::PgPool;

use crate::config::AppConfig;
use crate::rate_limit::{ProviderKeyCache, RateLimiters};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: AppConfig,
    pub rate_limiters: RateLimiters,
    pub provider_key_cache: ProviderKeyCache,
}
