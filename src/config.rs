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
    /// Ceiling on concurrent requests inside the unauthenticated password-auth routes
    /// (`POST /auth/{login,register,forgot-password,reset-password}`). Requests over the
    /// ceiling are shed with 429 instead of queueing, so the queue behind the Argon2 semaphore
    /// cannot grow without bound. Raise it if real traffic ever approaches it.
    pub auth_in_flight_cap: usize,
    /// How long a request BODY may take to arrive on the routes that read one, measured from the
    /// headers. A body that has not finished arriving within this many seconds is answered `408`
    /// and its task, connection and partially-read body buffer are released (kanban t_e7cba83e);
    /// the handler's own work is not bounded by it. `BODY_READ_DEADLINE_SECS`.
    pub body_read_deadline_secs: u64,
    /// How far a Stripe delivery's `t=` stamp may be from THIS host's clock before the receiver
    /// refuses it even though its HMAC verified (kanban t_72a4bcdf, the freshness arm of the
    /// `stripe_webhook` contract — the sibling of the arm ADASwift shipped as t_08628ca6). An
    /// absolute difference, so a stamp in the future is bounded the same way as one in the past.
    /// Defaults to Stripe's own 300 s; a host whose clock wanders can be widened without a rebuild.
    /// `STRIPE_WEBHOOK_TOLERANCE_SECS`.
    pub stripe_signature_tolerance_secs: i64,
    pub internal_sync_key: String,
    pub n8n_url: String,
    pub n8n_webhook_url: String,
    pub n8n_api_key: String,
    pub callback_base_url: String,
    pub funnelswift_url: String,
    pub coreswift_url: String,
    /// PayPal's public webhook identifier (PayPal dashboard → app → Webhooks).
    ///
    /// NOT a secret and NOT the shared internal sync key: it names WHICH webhook
    /// configuration PayPal must verify an inbound signature against, so it must be
    /// independent of INTERNAL_SYNC_KEY and rotating that credential cannot invalidate
    /// webhook verification (the shape ADASwift t_2be56050/t_9a1da415 shipped).
    ///
    /// Optional on purpose: an unset value is *not* an outage, it is an unconfigured
    /// receiver — `POST /api/v1/webhooks/paypal` then answers
    /// `503 paypal_not_configured` and processes nothing (kanban t_5cf44e1b). Resolution
    /// order is this value, then the active `paypal` provider row's `webhook_secret`
    /// (the field the admin console's Payment providers panel writes), so PayPal can be
    /// enabled from the console without a redeploy.
    pub paypal_webhook_id: String,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let host = env::var("APP_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = env::var("APP_PORT")
            .unwrap_or_else(|_| "8085".to_string())
            .parse::<u16>()
            .expect("Invalid APP_PORT");

        let database_url =
            env::var("DATABASE_URL").expect("DATABASE_URL environment variable is required");

        let jwt_secret =
            env::var("JWT_SECRET").expect("JWT_SECRET environment variable is required");

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

        // Password-auth load-shedding ceiling. Unset or unparseable falls back to the default
        // rather than refusing to boot: a typo in this value must not be an outage. The floor
        // is 8 (one per core here), so a stale low value cannot shed ordinary traffic.
        let auth_in_flight_cap = env::var("AUTH_IN_FLIGHT_CAP")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(crate::rate_limit::DEFAULT_AUTH_IN_FLIGHT_CAP)
            .max(8);

        // Body-read deadline (kanban t_e7cba83e). Same posture as the ceiling above: unset or
        // unparseable falls back to the default rather than refusing to boot, and the value is
        // clamped so a mistyped one cannot become an outage — 0 would answer 408 to every request
        // that carries a body, and a very large value would restore the unbounded hold this
        // closes.
        let body_read_deadline_secs = env::var("BODY_READ_DEADLINE_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(crate::rate_limit::DEFAULT_BODY_READ_DEADLINE_SECS)
            .clamp(5, 300);

        // Stripe signature freshness (kanban t_72a4bcdf). Same posture again: unset or unparseable
        // falls back to the default (Stripe's own 300 s) rather than refusing to boot, and the value
        // is clamped so a mistyped one cannot become an outage — 30 s would refuse ordinary
        // retries and a clock-skewed genuine delivery, and a day-sized value would hand a captured
        // `Stripe-Signature` header a day-long replay window.
        let stripe_signature_tolerance_secs = env::var("STRIPE_WEBHOOK_TOLERANCE_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(crate::handlers::checkout_handler::DEFAULT_STRIPE_SIGNATURE_TOLERANCE_SECS)
            .clamp(30, 86_400);

        let internal_sync_key = env::var("INTERNAL_SYNC_KEY").unwrap_or_default();
        let n8n_url = env::var("N8N_URL").unwrap_or_else(|_| "http://localhost:5681".to_string());
        let n8n_webhook_url =
            env::var("N8N_WEBHOOK_URL").unwrap_or_else(|_| "http://localhost:5679".to_string());
        let n8n_api_key = env::var("N8N_API_KEY").unwrap_or_default();
        let callback_base_url = env::var("CALLBACK_BASE_URL")
            .unwrap_or_else(|_| "http://workflowswift:8085".to_string());
        let funnelswift_url =
            env::var("FUNNELSWIFT_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
        let coreswift_url = env::var("CORESWIFT_URL").unwrap_or_default();
        // Optional, empty when unset: see the field's doc comment. Read once here so the
        // webhook receiver never has to touch the environment per request.
        let paypal_webhook_id = env::var("PAYPAL_WEBHOOK_ID").unwrap_or_default();

        Self {
            host,
            port,
            database_url,
            jwt_secret,
            jwt_access_expiry,
            jwt_refresh_expiry,
            db_min_connections,
            db_max_connections,
            auth_in_flight_cap,
            body_read_deadline_secs,
            stripe_signature_tolerance_secs,
            internal_sync_key,
            n8n_url,
            n8n_webhook_url,
            n8n_api_key,
            callback_base_url,
            funnelswift_url,
            coreswift_url,
            paypal_webhook_id,
        }
    }
}
