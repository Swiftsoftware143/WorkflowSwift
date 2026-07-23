use std::env;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub jwt_secret: String,
    pub jwt_access_expiry: i64,
    pub jwt_refresh_expiry: i64,
    pub db_min_connections: u32,
    pub db_max_connections: u32,
    pub internal_sync_key: String,
    pub n8n_url: String,
    pub n8n_webhook_url: String,
    pub n8n_api_key: String,
    pub callback_base_url: String,
    pub funnelswift_url: String,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let host = env::var("APP_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = env::var("APP_PORT")
            .unwrap_or_else(|_| "8085".to_string())
            .parse::<u16>()
            .expect("Invalid APP_PORT");

        let database_url = env::var("DATABASE_URL")
            .expect("DATABASE_URL environment variable is required");

        let jwt_secret = env::var("JWT_SECRET")
            .expect("JWT_SECRET environment variable is required");

        let jwt_access_expiry = env::var("JWT_ACCESS_TOKEN_EXPIRY")
            .unwrap_or_else(|_| "86400".to_string())
            .parse::<i64>()
            .expect("Invalid JWT_ACCESS_TOKEN_EXPIRY");

        let jwt_refresh_expiry = env::var("JWT_REFRESH_TOKEN_EXPIRY")
            .unwrap_or_else(|_| "2592000".to_string())
            .parse::<i64>()
            .expect("Invalid JWT_REFRESH_TOKEN_EXPIRY");

        let db_min_connections = env::var("DB_MIN_CONNECTIONS")
            .unwrap_or_else(|_| "2".to_string())
            .parse::<u32>()
            .expect("Invalid DB_MIN_CONNECTIONS");

        let db_max_connections = env::var("DB_MAX_CONNECTIONS")
            .unwrap_or_else(|_| "10".to_string())
            .parse::<u32>()
            .expect("Invalid DB_MAX_CONNECTIONS");

        let internal_sync_key = env::var("INTERNAL_SYNC_KEY").unwrap_or_default();
        let n8n_url = env::var("N8N_URL").unwrap_or_else(|_| "http://localhost:5681".to_string());
        let n8n_webhook_url = env::var("N8N_WEBHOOK_URL").unwrap_or_else(|_| "http://localhost:5679".to_string());
        let n8n_api_key = env::var("N8N_API_KEY").unwrap_or_default();
        let callback_base_url = env::var("CALLBACK_BASE_URL").unwrap_or_else(|_| "http://workflowswift:8085".to_string());
        let funnelswift_url = env::var("FUNNELSWIFT_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());

        Self {
            host, port, database_url, jwt_secret,
            jwt_access_expiry, jwt_refresh_expiry,
            db_min_connections, db_max_connections,
            internal_sync_key,
            n8n_url,
            n8n_webhook_url,
            n8n_api_key,
            callback_base_url,
            funnelswift_url,
        }
    }
}
