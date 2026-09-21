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
    let provider_key_cache = crate::rate_limit::ProviderKeyCache::new(300); // 5 min TTL

    let state = AppState {
        db: pool,
        config: config.clone(),
        rate_limiters,
        pre_auth_limiters,
        provider_key_cache,
    };

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
