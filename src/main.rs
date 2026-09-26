#![allow(dead_code, unused_variables, unused_assignments, unused_must_use)]
#![allow(
    clippy::doc_lazy_continuation,
    clippy::let_underscore_future,
    clippy::collapsible_match,
    clippy::redundant_pattern_matching,
    clippy::needless_late_init,
    clippy::type_complexity
)]
mod auth;
mod config;
mod db;
mod email;
mod error;
mod execution;
mod execution_worker;
mod features;
mod handlers;
mod models;
mod n8n_converter;
mod n8n_provision;
mod rate_limit;
mod routes;
mod security;
mod state;

use std::time::Duration;
use tokio::signal;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

pub use error::AppError;
pub use state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .with_thread_ids(true)
        .init();

    let config = config::AppConfig::from_env();
    let pool = db::connect(
        &config.database_url,
        config.db_min_connections,
        config.db_max_connections,
    )
    .await;

    // Run migrations using sqlx::query (no macro)
    tracing::info!("Running database migrations...");
    db::run_migrations(&pool).await;

    // At-rest encryption posture for BYOK provider credentials. This belongs in the boot log:
    // without the master key, provider key writes fail closed, and that must be visible before
    // a customer hits it (never silently fall back to plaintext).
    if crate::security::provider_key_crypto::is_configured() {
        tracing::info!("Provider key encryption: enabled (AES-256 at rest, enc:v1 format)");
    } else {
        tracing::warn!(
            "Provider key encryption: DISABLED — PROVIDER_KEY_ENC_SECRET is missing; BYOK provider key writes will fail closed"
        );
    }

    let rate_limiters = crate::rate_limit::RateLimiters::new(30, 10);
    // Pre-auth ceiling, per client identity, on API-key credentials: the per-account limit
    // above is applied only AFTER auth, so it cannot protect the Argon2 verification that
    // auth performs. Set deliberately above the per-account limit (30/s) so a legitimate
    // client always exhausts its own account budget first, and bounded at 10k buckets
    // because the key is a client-supplied header.
    let pre_auth_limiters = crate::rate_limit::RateLimiters::with_capacity(60, 60, 10_000);
    // Load-shedding ceiling on the unauthenticated password-auth routes. The Argon2 semaphore
    // bounds the hashing work a login flood can do; this bounds how many requests may be
    // waiting for it, so the queue cannot grow without limit and the box keeps answering.
    let auth_in_flight = crate::rate_limit::AuthInFlight::new(config.auth_in_flight_cap);
    tracing::info!(
        "Password-auth load shedding: {} concurrent requests on /auth/login|register|forgot-password|reset-password, 429 above that",
        auth_in_flight.cap()
    );
    // Body-read deadline (kanban t_e7cba83e): how long a request body may take to ARRIVE before
    // the request is answered 408 and its task, connection and partial body buffer are released.
    // In the boot log for the same reason as the ceiling above — the bound an operator relies on
    // has to be visible without reading the source.
    let body_read_deadline =
        crate::rate_limit::BodyReadDeadline::from_secs(config.body_read_deadline_secs);
    tracing::info!(
        "Request body-read deadline: {:?} on every route that reads a body, 408 above that (BODY_READ_DEADLINE_SECS)",
        body_read_deadline.duration()
    );

    // Stripe signature freshness (kanban t_72a4bcdf): how far a delivery's `t=` stamp may be from
    // this host's clock before the receiver refuses it even though its HMAC verified. In the boot
    // log for the same reason as the two bounds above — an operator diagnosing "every Stripe
    // delivery is being refused" has to be able to read the value in force, and it is also the value
    // that says whether THIS box's clock is the suspect.
    tracing::info!(
        "Stripe webhook signature tolerance: {}s from this host's clock; a correctly-signed delivery \
         further away is answered 503 stripe_signature_timestamp_out_of_tolerance and logged \
         (STRIPE_WEBHOOK_TOLERANCE_SECS, clamped 30..86400, default {})",
        config.stripe_signature_tolerance_secs,
        crate::handlers::checkout_handler::DEFAULT_STRIPE_SIGNATURE_TOLERANCE_SECS
    );
    let provider_key_cache = crate::rate_limit::ProviderKeyCache::new(300); // 5 min TTL

    let state = AppState {
        db: pool,
        config: config.clone(),
        rate_limiters,
        pre_auth_limiters,
        auth_in_flight,
        provider_key_cache,
    };

    // The background execution worker. Nothing else can advance a `delay`/`wait`
    // step, so without this a run that waits stays pending forever (kanban
    // t_1ff4b916). It only wakes steps whose due time has passed and hands them
    // to the same engine (src/execution.rs) — it is not a second executor.
    execution_worker::spawn(state.clone());

    let app = routes::create_router(state.clone())
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive());

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("Starting WorkflowSwift API server on {}", addr);

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%addr, error = %e, "Failed to bind address");
            std::process::exit(1);
        }
    };

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        tracing::error!("Server error: {}", e);
        std::process::exit(1);
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Ctrl+C received, starting graceful shutdown");
        }
        _ = terminate => {
            tracing::info!("SIGTERM received, starting graceful shutdown");
        }
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    tracing::info!("Server shutdown complete");
}
