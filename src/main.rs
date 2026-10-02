#![allow(dead_code, unused_variables, unused_assignments, unused_must_use)]
#![allow(
    clippy::doc_lazy_continuation,
    clippy::let_underscore_future,
    clippy::collapsible_match,
    clippy::redundant_pattern_matching,
    clippy::needless_late_init,
    clippy::type_complexity
)]
mod ai_llm;
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
    // Host-side applier mode (kanban t_0be214ec), checked BEFORE anything else: it needs only
    // DATABASE_URL, renders with the same code the server runs, and must never boot a second API
    // on the live port (no config load, no migrations, no worker, no listener).
    //
    //   workflowswift-api apply-site-settings            write only the files whose bytes change
    //   workflowswift-api apply-site-settings --check    render and report, write NOTHING
    //   workflowswift-api apply-site-settings --emit DIR also drop the rendered bytes under DIR
    if std::env::args().nth(1).as_deref() == Some("apply-site-settings") {
        apply_site_settings_mode().await;
        return;
    }

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

    // At-rest seal for the system-mail credential (kanban t_a794cb09). All three writers of the
    // `admin_settings.email` row seal before they store; this is what converges a row that arrives
    // plaintext from a database restored out of an older dump (or from a writer added later).
    // Never fatal: a broken credential row must not stop the app booting.
    match crate::email::seal_legacy_config_secrets(&pool).await {
        Ok(0) => {}
        Ok(n) => tracing::warn!(
            rows = n,
            "admin_settings.email: sealed legacy plaintext credential(s) at rest"
        ),
        Err(e) => tracing::error!(
            "admin_settings.email credential backfill failed (plaintext may remain at rest): {}",
            e
        ),
    }

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

/// The host-side applier (`workflowswift-api apply-site-settings`, driven by
/// /opt/swift/bin/ws-site-apply.sh from cron). It is the ONLY writer of
/// /opt/swift/nginx/www/workflowswift/*.html.
///
/// It prints one machine-readable summary line — `site artifacts: written=N skipped=M` — and one
/// line per file it touched or deliberately left alone, so the cron log and the card's proof can
/// both read what happened without a second probe.
///
/// `--check` renders and reports `state=unchanged|would-write` with both sha256s but writes
/// NOTHING, which is the instrument a reconciliation uses BEFORE the first real apply: the applier
/// is idempotent only once the DB and the served bytes agree, so a row holding stale copy would
/// rewrite the live homepage on the first run (the near-miss caught on ADASwift, card t_1f427190).
/// `--emit DIR` drops the rendered bytes elsewhere for a byte-for-byte diff.
async fn apply_site_settings_mode() {
    use sha2::{Digest, Sha256};

    let args: Vec<String> = std::env::args().collect();
    let check = args.iter().any(|a| a == "--check");
    let emit_dir = args
        .iter()
        .position(|a| a == "--emit")
        .and_then(|i| args.get(i + 1))
        .cloned();

    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("apply-site-settings: DATABASE_URL is not set");
            std::process::exit(2);
        }
    };

    let db = db::connect(&url, 1, 4).await;

    let settings = match crate::handlers::site_handler::load_settings(&db).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "apply-site-settings: cannot read the site settings row: {:?}",
                e
            );
            std::process::exit(1);
        }
    };

    let (targets, skipped) = crate::handlers::site_handler::plan(&settings);

    // --emit: drop the rendered bytes somewhere else so they can be compared byte-for-byte with the
    // served file WITHOUT this process writing anything under SITE_ROOT.
    if let Some(dir) = emit_dir {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!(
                "apply-site-settings: cannot create --emit dir {}: {}",
                dir, e
            );
            std::process::exit(1);
        }
        for (path, rendered) in &targets {
            let name = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "rendered".to_string());
            let dest = std::path::Path::new(&dir).join(name);
            if let Err(e) = std::fs::write(&dest, rendered.as_bytes()) {
                eprintln!(
                    "apply-site-settings: cannot write {}: {}",
                    dest.display(),
                    e
                );
                std::process::exit(1);
            }
            println!("emit {} -> {}", path, dest.display());
        }
    }

    if check {
        for (path, reason) in &skipped {
            println!("skip {} {}", path, reason);
        }
        for (path, rendered) in &targets {
            let now = std::fs::read_to_string(path).unwrap_or_default();
            let state = if now == *rendered {
                "unchanged"
            } else {
                "would-write"
            };
            println!(
                "check {} state={} sha256={} served_sha256={}",
                path,
                state,
                hex::encode(Sha256::digest(rendered.as_bytes())),
                hex::encode(Sha256::digest(now.as_bytes())),
            );
        }
        println!(
            "site artifacts (check, nothing written): targets={} skipped={}",
            targets.len(),
            skipped.len()
        );
        return;
    }

    let (written, skipped) = crate::handlers::site_handler::apply_to_disk(&settings);
    for path in &written {
        println!("write {} (the rendered bytes differ from the file)", path);
    }
    for (path, reason) in &skipped {
        println!("skip {} {}", path, reason);
    }
    // Exactly one machine-readable line for the cron log.
    println!(
        "site artifacts: written={} skipped={}",
        written.len(),
        skipped.len()
    );
}
