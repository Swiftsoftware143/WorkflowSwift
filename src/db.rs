use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;

pub async fn connect(database_url: &str, min_connections: u32, max_connections: u32) -> PgPool {
    let options: PgConnectOptions = database_url.parse().expect("Invalid DATABASE_URL format");

    match PgPoolOptions::new()
        .min_connections(min_connections)
        .max_connections(max_connections)
        .connect_with(options)
        .await
    {
        Ok(pool) => pool,
        Err(e) => {
            tracing::error!(error = %e, "Failed to connect to database");
            std::process::exit(1);
        }
    }
}

/// Apply every `./migrations/*.sql` file that is not yet recorded in `_migrations`.
///
/// Each file runs as ONE batch over PostgreSQL's simple query protocol
/// (`sqlx::raw_sql`), which the server executes as a single implicit transaction:
/// the whole file lands, or none of it does. The previous implementation split the
/// file on every `';'` character and ran the fragments as separate statements,
/// downgraded each error to a `warn!` and then inserted the filename into
/// `_migrations` regardless. Measured 2026-09-21 (card t_bc8e40a0): a semicolon
/// inside a *prose comment* in 049_/051_extension_commands_ack.sql shattered the
/// file into a comment sliver, a syntax error and two half-statements, so
/// `acknowledged_at` and its index were never created while `_migrations` claimed
/// the file was applied — every later boot logged "already applied, skipping" and
/// the endpoint that writes that column would have 500'd in production, with a
/// green deploy and a green health check.
///
/// A file that fails is NOT recorded, so the next start retries it, and the
/// failure is reported at `error!` level. By default the process then exits
/// non-zero, so a schema that does not match the code fails the deploy instead of
/// being served; `MIGRATIONS_FATAL=0` is the single operational escape hatch (boot
/// anyway, still `error!`). There is deliberately no silent mode.
pub async fn run_migrations(pool: &PgPool) {
    let migration_dir = std::path::Path::new("./migrations");
    if !migration_dir.exists() {
        tracing::warn!("Migrations directory not found at ./migrations");
        return;
    }

    let mut entries: Vec<_> = match std::fs::read_dir(migration_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::error!(error = %e, "Failed to read migrations directory");
            return;
        }
    }
    .filter_map(|e| e.ok())
    .filter(|e| {
        e.path()
            .extension()
            .map(|ext| ext == "sql")
            .unwrap_or(false)
    })
    .collect();

    entries.sort_by_key(|e| e.file_name());

    // Create migrations tracking table
    if let Err(e) = sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS _migrations (
            id SERIAL PRIMARY KEY,
            filename VARCHAR(255) NOT NULL UNIQUE,
            applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )"#,
    )
    .execute(pool)
    .await
    {
        tracing::error!(error = %e, "Failed to create migrations tracking table");
        return;
    }

    // (filename, reason) for every file this boot could not apply and record.
    let mut failures: Vec<(String, String)> = Vec::new();

    for entry in &entries {
        let filename = entry.file_name().to_string_lossy().to_string();

        let already_applied =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM _migrations WHERE filename = $1")
                .bind(&filename)
                .fetch_one(pool)
                .await
                .unwrap_or(0);

        if already_applied > 0 {
            tracing::info!("Migration {} already applied, skipping", filename);
            continue;
        }

        let sql = match std::fs::read_to_string(entry.path()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(filename = %filename, error = %e, "Failed to read migration file");
                failures.push((filename.clone(), format!("read failed: {e}")));
                continue;
            }
        };

        tracing::info!("Applying migration: {}", filename);

        // The whole file, one statement batch: no splitting on ';', so a semicolon
        // inside a comment, a string literal or a dollar-quoted body can no longer
        // truncate the migration. Postgres wraps a multi-statement simple query in a
        // single implicit transaction, so a failure leaves nothing half-applied.
        match sqlx::raw_sql(&sql).execute(pool).await {
            Ok(_) => {
                if let Err(e) = sqlx::query("INSERT INTO _migrations (filename) VALUES ($1)")
                    .bind(&filename)
                    .execute(pool)
                    .await
                {
                    // The DDL landed but we could not write the ledger row. Re-running the
                    // file is harmless (every migration in this directory is idempotent),
                    // whereas recording it blind is exactly how an unapplied migration
                    // becomes invisible. So: loud, and leave it unrecorded.
                    tracing::error!(
                        filename = %filename,
                        error = %e,
                        "Migration applied but could NOT be recorded in _migrations — it will be re-run at the next start"
                    );
                    failures.push((filename.clone(), format!("ledger insert failed: {e}")));
                } else {
                    tracing::info!(filename = %filename, "Migration applied");
                }
            }
            Err(e) => {
                tracing::error!(
                    filename = %filename,
                    error = %e,
                    "MIGRATION FAILED — not recorded in _migrations, so the next start retries it"
                );
                failures.push((filename.clone(), e.to_string()));
            }
        }
    }

    if failures.is_empty() {
        tracing::info!("All migrations applied successfully");
        return;
    }

    for (filename, reason) in &failures {
        tracing::error!(filename = %filename, error = %reason, "migration NOT applied");
    }

    let fatal = std::env::var("MIGRATIONS_FATAL")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true);

    if fatal {
        tracing::error!(
            failed = failures.len(),
            "Refusing to serve: {} migration file(s) could not be applied, so the schema does not match this binary. Fix the file(s) and redeploy, or set MIGRATIONS_FATAL=0 to boot anyway.",
            failures.len()
        );
        std::process::exit(1);
    }

    tracing::error!(
        failed = failures.len(),
        "MIGRATIONS_FATAL=0: booting anyway with {} migration file(s) UNAPPLIED — the schema does not match this binary",
        failures.len()
    );
}
