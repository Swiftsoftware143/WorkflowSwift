#![allow(dead_code)]

mod email;
mod config;
mod db;
mod error;
mod state;
mod models;
mod handlers;
mod auth;
mod features;
mod routes;
mod rate_limit;
mod n8n_converter;
mod n8n_provision;
mod security;

use std::time::Duration;
use tokio::signal;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

pub use state::AppState;
pub use error::AppError;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .with_thread_ids(true)
        .init();

    let config = config::AppConfig::from_env();
    let pool = db::connect(&config.database_url, config.db_min_connections, config.db_max_connections).await;

    // Run migrations using sqlx::query (no macro)
    tracing::info!("Running database migrations...");
    db::run_migrations(&pool).await;

    let rate_limiters = crate::rate_limit::RateLimiters::new(100, 200);
    let provider_key_cache = crate::rate_limit::ProviderKeyCache::new(300); // 5 min TTL

    let state = AppState {
        db: pool,
        config: config.clone(),
        rate_limiters,
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
