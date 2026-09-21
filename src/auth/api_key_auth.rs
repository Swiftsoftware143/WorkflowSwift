//! API-key authentication.
//!
//! Keys minted by `handlers::api_key_handler::create_api_key` look like
//! `workflowswift_<16 hex>` and are stored **argon2-hashed** with a random salt, so the
//! key itself is not recoverable from the row and cannot be looked up by value.
//! `api_keys.prefix` (varchar(8), shown to the key's owner by `GET /api-keys`) is
//! therefore used as a real lookup hint: it holds `discriminator()` — the first 8 hex
//! digits of sha256(raw key) — so verification is an indexed equality lookup on that
//! hint followed by **one** argon2 check, never a scan over every tenant's key rows.
//! Keys minted before the hint existed still carry the constant `LEGACY_PREFIX` and
//! are found by scanning that (shrinking) subset, and the first successful use of such
//! a key backfills its hint. See `authenticate()`.
//!
//! The raw key is never logged, never stored and never returned on a read path.

use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::http::Method;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::sync::{Arc, OnceLock};
use tokio::sync::Semaphore;
use uuid::Uuid;

use super::models::Claims;
use crate::error::AppError;
use crate::features;
use crate::AppState;

/// Prefix of every key minted by the API-key handler (see `create_api_key`).
pub const KEY_PREFIX: &str = "workflowswift_";

/// The `api_keys.prefix` value written by every key minted before `prefix` became a
/// lookup hint. Rows still holding it have no usable discriminator, so they can only
/// be found by scanning that subset.
pub const LEGACY_PREFIX: &str = "workflo";

/// The 8-character lookup hint stored in `api_keys.prefix`: the first 8 hex digits of
/// `sha256(raw key)`.
///
/// `prefix` is varchar(8) and is returned to the key's owner by `GET /api-keys`, so it
/// is product-visible and must be worthless to anyone else. A truncated digest of the
/// whole key satisfies that: it exposes no slice of the key and cannot be inverted back
/// into it.
pub fn discriminator(raw_key: &str) -> String {
    let digest = hex::encode(Sha256::digest(raw_key.as_bytes()));
    digest[..8].to_string()
}

/// Longest credential we will even attempt to hash-verify, so a huge header
/// cannot turn into unbounded argon2 work.
const MAX_KEY_LEN: usize = 128;

/// How many Argon2 jobs — a key/password *verification* or a password *hash* — may run
/// concurrently.
///
/// Argon2id with the default parameters (`m=19456, t=2`) is pure CPU work that never
/// awaits: run inline in the async handler it parks a tokio worker thread for its whole
/// duration, so enough concurrent credential requests pin every worker and the *runtime*
/// (not the database) becomes the bottleneck. Measured on this box 2026-09-21 with 40
/// concurrent valid-key requests: the median latency of a request that performs exactly
/// one verification went 27 ms -> 280 ms, and an unrelated unauthenticated
/// `GET /api/v1/health` — which does no hashing at all — went from ~2 ms to 131 ms median
/// (249 ms max), i.e. it was waiting for a free worker.
///
/// Verification therefore runs on the blocking pool, bounded by this semaphore so a
/// credential flood queues in the scheduler instead of occupying every worker: the permit
/// count is the machine's usable parallelism, i.e. exactly as many 19 MiB hash jobs as
/// there are cores to run them on. Excess requests wait here, not on a worker thread, so
/// the runtime keeps accepting, throttling (a 429 is produced without hashing) and
/// answering everyone else.
///
/// Hashing is the same work as verifying, so it shares THIS semaphore (see
/// [`argon2_hash`]): one bound covers all Argon2 in the process, and the password paths
/// (`login`/`register`/`change_password`/`reset_password`) cannot out-compete the API-key
/// path for worker threads or for cores.
static ARGON2_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

/// The configured Argon2 concurrency bound: the machine's usable parallelism, clamped to
/// `1..=16`. Read separately from [`argon2_permits`] so the bound can be asserted without
/// racing other tasks for permits.
fn argon2_permit_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16)
}

fn argon2_permits() -> Arc<Semaphore> {
    ARGON2_PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(argon2_permit_count())))
        .clone()
}

/// One Argon2 verification against one stored hash, off the reactor and bounded by
/// [`argon2_permits`], saying *why* it failed.
///
/// `Err(AppError::Hash)` means the stored hash itself is unusable; `Ok(false)` is a plain
/// mismatch. The password paths keep the two apart (`500 "Password hashing error"` vs
/// `401 Invalid credentials`), while [`argon2_verify`] collapses both into the boolean the
/// API-key path wants.
pub(crate) async fn argon2_verify_result(hash: String, secret: Arc<str>) -> Result<bool, AppError> {
    let permit = argon2_permits().acquire_owned().await.map_err(|_| {
        // The semaphore is only closed on shutdown, where nothing should verify.
        AppError::Hash("verification semaphore closed".to_string())
    })?;
    tokio::task::spawn_blocking(move || {
        // Held for the whole verification, released when this closure returns.
        let _permit = permit;
        let parsed = PasswordHash::new(&hash).map_err(|e| AppError::Hash(e.to_string()))?;
        Ok(Argon2::default()
            .verify_password(secret.as_bytes(), &parsed)
            .is_ok())
    })
    .await
    .map_err(|e| AppError::Hash(format!("verification task failed: {e}")))?
}

/// One Argon2 verification against one stored hash, off the reactor and bounded by
/// [`argon2_permits`]. `false` means "does not match" — or "unusable stored hash" — which
/// is what every caller already treats as "not this credential".
async fn argon2_verify(hash: String, token: Arc<str>) -> bool {
    argon2_verify_result(hash, token).await.unwrap_or(false)
}

/// One Argon2 password hash, off the reactor and bounded by the SAME [`argon2_permits`]
/// semaphore as verification.
///
/// Hashing is the identical 19 MiB CPU-bound work as verifying, so it parks a worker
/// thread in exactly the same way: `register` is reachable unauthenticated, and
/// `change_password`/`reset_password` each hash too. Holding a permit for the whole hash
/// is what keeps total Argon2 concurrency at `available_parallelism()` no matter which
/// credential path a flood arrives on.
pub(crate) async fn argon2_hash(password: String) -> Result<String, AppError> {
    use argon2::password_hash::SaltString;
    use argon2::PasswordHasher;
    use rand::rngs::OsRng;

    let permit = argon2_permits()
        .acquire_owned()
        .await
        .map_err(|_| AppError::Hash("hashing semaphore closed".to_string()))?;
    tokio::task::spawn_blocking(move || {
        // Held for the whole hash, released when this closure returns.
        let _permit = permit;
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| AppError::Hash(e.to_string()))
    })
    .await
    .map_err(|e| AppError::Hash(format!("hashing task failed: {e}")))?
}

/// True when a bearer credential is an API key rather than a JWT.
pub fn is_api_key(token: &str) -> bool {
    token.starts_with(KEY_PREFIX)
}

/// The scope an HTTP method requires of a key: reads vs writes.
fn required_scope(method: &Method) -> &'static str {
    match *method {
        Method::GET | Method::HEAD | Method::OPTIONS => "read",
        _ => "write",
    }
}

/// `permissions` is a jsonb array of scope strings.
///
/// * absent / `[]` (the default written at creation time) — the key carries the
///   account scope exactly like a JWT for its owning user.
/// * non-empty — the key must be granted the scope the request needs; anything
///   else is refused, so a key is never granted more than its row allows.
fn permissions_allow(permissions: &serde_json::Value, scope: &str) -> bool {
    let Some(list) = permissions.as_array() else {
        return true;
    };
    if list.is_empty() {
        return true;
    }
    list.iter().filter_map(|v| v.as_str()).any(|s| {
        let s = s.trim().to_ascii_lowercase();
        s == "*" || s == "all" || s == scope || (scope == "read" && s == "readonly")
    })
}

/// Resolve an API key to the owning account/user and build the same `Claims`
/// shape JWT auth produces, so every downstream handler stays account-scoped.
pub async fn authenticate(
    state: &AppState,
    token: &str,
    method: &Method,
) -> Result<Claims, AppError> {
    if token.len() > MAX_KEY_LEN || token.len() <= KEY_PREFIX.len() {
        return Err(AppError::Unauthorized);
    }

    let hint = discriminator(token);

    // 1) Indexed equality lookup on the key's own discriminator: one row (at most a
    //    handful, on a hint collision) for every key minted since `prefix` became a
    //    hint — instead of argon2-verifying every row in the table, which made one
    //    tenant's request pay for every other tenant's keys.
    let mut matched = verify_candidates(state, &hint, token).await?;
    let mut legacy_hit = false;

    // 2) Fallback for keys minted before the hint existed: they still carry the constant
    //    prefix and can only be found by scanning that subset. The subset shrinks on its
    //    own (the backfill below moves a key to the fast path the first time it is used)
    //    and is empty once the last of them is retired, which is when the scan cost of an
    //    unknown credential reaches zero.
    if matched.is_none() && hint != LEGACY_PREFIX {
        matched = verify_candidates(state, LEGACY_PREFIX, token).await?;
        legacy_hit = matched.is_some();
    }

    // Unknown / wrong key: indistinguishable from a bad JWT to the caller.
    let row = matched.ok_or(AppError::Unauthorized)?;

    let key_id: Uuid = row.try_get("id")?;
    let aid: Uuid = row.try_get("aid")?;
    let user_id: Uuid = row.try_get("user_id")?;
    let is_active: bool = row.try_get("is_active").unwrap_or(false);
    let expires_at: Option<DateTime<Utc>> = row.try_get("expires_at").ok().flatten();
    let permissions: serde_json::Value = row
        .try_get("permissions")
        .unwrap_or_else(|_| serde_json::json!([]));

    // Self-heal: the first successful use of a pre-hint key writes its discriminator, so
    // that key stops costing a scan on every later request. Best-effort — a failure here
    // must never fail the authentication that just succeeded.
    if legacy_hit {
        let backfill = sqlx::query("UPDATE api_keys SET prefix = $2 WHERE id = $1 AND prefix = $3")
            .bind(key_id)
            .bind(&hint)
            .bind(LEGACY_PREFIX)
            .execute(&state.db)
            .await;
        if let Err(e) = backfill {
            tracing::warn!(error = %e, key_id = %key_id, "legacy prefix backfill failed");
        } else {
            tracing::info!(key_id = %key_id, "api key moved to the indexed prefix path");
        }
    }

    if !is_active {
        tracing::warn!(key_id = %key_id, "api key rejected: revoked");
        return Err(AppError::Unauthorized);
    }
    if let Some(exp) = expires_at {
        if exp <= Utc::now() {
            tracing::warn!(key_id = %key_id, "api key rejected: expired");
            return Err(AppError::Unauthorized);
        }
    }
    if !permissions_allow(&permissions, required_scope(method)) {
        tracing::warn!(key_id = %key_id, "api key rejected: scope not granted");
        return Err(AppError::Forbidden(
            "API key is not permitted to perform this action".to_string(),
        ));
    }

    // The owning user must still be active — a revoked user must not keep
    // working through a key that was minted before the revocation.
    let user_active: Option<bool> =
        sqlx::query_scalar("SELECT is_active FROM users WHERE id = $1 AND aid = $2")
            .bind(user_id)
            .bind(aid)
            .fetch_optional(&state.db)
            .await?;
    if user_active != Some(true) {
        tracing::warn!(key_id = %key_id, "api key rejected: owning user inactive");
        return Err(AppError::Unauthorized);
    }

    // Plan gate: `api_access` off means the account may not use API keys at all.
    features::enforce_plan_flag(&state.db, aid, "api_access", "API access").await?;

    // Touch last_used_at. Best-effort: a failure here must not fail the request.
    if let Err(e) = sqlx::query("UPDATE api_keys SET last_used_at = NOW() WHERE id = $1")
        .bind(key_id)
        .execute(&state.db)
        .await
    {
        tracing::warn!(error = %e, "api key last_used_at update failed");
    }

    let now = Utc::now().timestamp() as usize;
    Ok(Claims {
        sub: user_id.to_string(),
        aid: aid.to_string(),
        // Least privilege: a key is never an admin session. Admin-only handlers
        // (role/super-admin checks) stay reachable by JWT only.
        role: "api_key".to_string(),
        exp: now + 3600,
        iat: now,
        perm_is_super_admin: Some(false),
    })
}

/// Argon2-verify `token` against the rows carrying one `prefix`, returning the first
/// match. Every candidate costs one full Argon2 verification, which is exactly why the
/// discriminator exists: the hint normally narrows the table to a single row.
async fn verify_candidates(
    state: &AppState,
    prefix: &str,
    token: &str,
) -> Result<Option<sqlx::postgres::PgRow>, AppError> {
    let rows = sqlx::query(
        "SELECT id, aid, user_id, key_hash, permissions, is_active, expires_at \
         FROM api_keys WHERE prefix = $1",
    )
    .bind(prefix)
    .fetch_all(&state.db)
    .await?;

    let token: Arc<str> = Arc::from(token);
    for row in rows {
        let hash: String = row.try_get("key_hash").unwrap_or_default();
        if hash.is_empty() {
            continue;
        }
        if argon2_verify(hash, Arc::clone(&token)).await {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source-level guard, because the defect this module's helpers fix kept coming back:
    /// an `Argon2::default()` inlined in an `async fn` parks a tokio worker thread for the
    /// whole 19 MiB hash, so the runtime (not the DB, not the network) becomes the bottleneck
    /// for every unrelated request. Every hash and every verification in this crate must go
    /// through [`argon2_hash`] / [`argon2_verify_result`], which hold the shared
    /// [`argon2_permits`] semaphore and run on the blocking pool. THIS file is the only place
    /// allowed to construct Argon2 directly — it is where the permits live.
    ///
    /// Sites this guard would have caught: the API-key path (t_5154cfcd), the password paths
    /// (t_b94d6d57), and then checkout `deliver_credentials`, `invite_user`,
    /// `admin_create_account`, `create_api_key`, `generate_user_key` and `seed_user_keys`
    /// (t_d8a2b14c — two of those reached from UNAUTHENTICATED routes).
    #[test]
    fn argon2_is_only_constructed_in_this_module() {
        fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("read src dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    rs_files(&path, out);
                } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                    out.push(path);
                }
            }
        }

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rs_files(&src, &mut files);

        let mut offenders = Vec::new();
        for file in files {
            if file.to_string_lossy().ends_with("auth/api_key_auth.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).expect("read source file");
            for (n, line) in text.lines().enumerate() {
                if line.contains("Argon2::default()") || line.contains(".hash_password(") {
                    offenders.push(format!("{}:{}", file.display(), n + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "Argon2 must be constructed only in auth::api_key_auth (off the reactor, under \
             argon2_permits); found inline uses at: {}",
            offenders.join(", ")
        );
    }

    #[test]
    fn empty_permissions_mean_account_scope() {
        assert!(permissions_allow(&serde_json::json!([]), "read"));
        assert!(permissions_allow(&serde_json::json!([]), "write"));
    }

    #[test]
    fn explicit_permissions_are_enforced() {
        let read_only = serde_json::json!(["read"]);
        assert!(permissions_allow(&read_only, "read"));
        assert!(!permissions_allow(&read_only, "write"));

        let wildcard = serde_json::json!(["*"]);
        assert!(permissions_allow(&wildcard, "write"));
    }

    #[test]
    fn api_key_detection() {
        assert!(is_api_key("workflowswift_0123456789abcdef"));
        assert!(!is_api_key("eyJhbGciOiJIUzI1NiJ9.e30.x"));
    }

    #[test]
    fn discriminator_is_stable_short_and_not_the_legacy_constant() {
        let key = "workflowswift_0123456789abcdef";
        let d = discriminator(key);
        // deterministic, exactly varchar(8), lowercase hex
        assert_eq!(d, discriminator(key));
        assert_eq!(d.len(), 8);
        assert!(d.bytes().all(|b| b.is_ascii_hexdigit()));
        // distinguishes near-identical keys, and leaks no slice of the key
        assert_ne!(d, discriminator("workflowswift_0123456789abcdee"));
        assert_ne!(d, LEGACY_PREFIX);
        assert!(!key.contains(&d));
    }

    /// The bound is what stops a credential flood from saturating the runtime: it must
    /// exist, be shared for the process lifetime, and match the machine's parallelism.
    ///
    /// The permit count is asserted from [`argon2_permit_count`] rather than
    /// `available_permits() == want`: the tests in this module run in parallel and the two
    /// A/B tests legitimately hold permits for seconds at a time, so an equality on
    /// *available* permits is a false assertion. The observable bound is still asserted —
    /// a semaphore can only ever have fewer permits available than it was created with.
    #[test]
    fn verify_permits_is_bounded_and_process_wide() {
        let a = argon2_permits();
        let b = argon2_permits();
        assert!(
            Arc::ptr_eq(&a, &b),
            "one semaphore per process, not one per call"
        );
        let want = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, 16);
        assert_eq!(argon2_permit_count(), want);
        assert!(
            a.available_permits() <= want,
            "the semaphore was created with more than the machine's parallelism: \
             available={} want={want}",
            a.available_permits()
        );
    }

    /// The off-reactor path must not change what authenticates: matching secret -> true,
    /// wrong secret -> false, unusable stored hash -> false (never a panic).
    #[tokio::test]
    async fn argon2_verify_decides_correctly_from_the_blocking_pool() {
        use argon2::password_hash::SaltString;
        use argon2::PasswordHasher;
        use rand::rngs::OsRng;

        let raw = "workflowswift_0123456789abcdef";
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(raw.as_bytes(), &salt)
            .expect("hash a test key")
            .to_string();

        assert!(argon2_verify(hash.clone(), Arc::from(raw)).await);
        assert!(!argon2_verify(hash.clone(), Arc::from("workflowswift_fedcba9876543210")).await);
        assert!(!argon2_verify("not-a-phc-string".to_string(), Arc::from(raw)).await);
    }

    /// Controlled A/B of exactly the change on this card, in one runtime, on one machine.
    ///
    /// 40 concurrent verifications are run twice on an 8-worker runtime. Alongside them a
    /// "heartbeat" task does `sleep(2ms)` in a loop and records how much LATER than 2 ms it
    /// was actually woken: that delay is the runtime failing to schedule an unrelated task,
    /// which is what a request from any other tenant experiences.
    ///
    /// * `inline` reproduces the pre-fix shape — `verify_password` called straight from the
    ///   async task body, so the worker thread is parked for the whole hash.
    /// * `bounded` is the shipped shape — [`argon2_verify`], off the reactor and behind
    ///   [`verify_permits`].
    ///
    /// The two phases measure the same work on the same box seconds apart, so a busy
    /// neighbour moves BOTH numbers and the ratio is the signal, not the absolute value.
    ///
    /// The fixture hash uses deliberately cheap parameters (4 MiB, t=1) to keep the test
    /// around a second long: the mechanism under test is "CPU-bound work that never
    /// yields", which is the same for any parameter set. Verifying with `Argon2::default()`
    /// also pins down that the PHC string stored in the row — not the verifier's own
    /// parameters — decides the work done, which is what the card's candidate 2 would hinge
    /// on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn off_reactor_verification_keeps_the_runtime_responsive() {
        use argon2::password_hash::SaltString;
        use argon2::PasswordHasher;
        use argon2::{Algorithm, Params, Version};
        use rand::rngs::OsRng;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Mutex;
        use std::time::{Duration, Instant};

        /// The pre-fix code path, verbatim: verify on the worker thread.
        fn verify_inline(hash: &str, token: &str) -> bool {
            let Ok(parsed) = PasswordHash::new(hash) else {
                return false;
            };
            Argon2::default()
                .verify_password(token.as_bytes(), &parsed)
                .is_ok()
        }

        let raw = "workflowswift_0123456789abcdef";
        let salt = SaltString::generate(&mut OsRng);
        let cheap = Params::new(4 * 1024, 1, 1, None).expect("test params");
        let hash = Argon2::new(Algorithm::Argon2id, Version::V0x13, cheap)
            .hash_password(raw.as_bytes(), &salt)
            .expect("hash a test key")
            .to_string();
        assert!(verify_inline(&hash, raw), "the fixture hash must verify");

        // Measure how late the runtime wakes an unrelated 2 ms sleep.
        async fn heartbeat_ms(during: Duration) -> Vec<f64> {
            let stop = Arc::new(AtomicBool::new(false));
            let out = Arc::new(Mutex::new(Vec::<f64>::new()));
            let task = {
                let (stop, out) = (Arc::clone(&stop), Arc::clone(&out));
                tokio::spawn(async move {
                    while !stop.load(Ordering::Relaxed) {
                        let t0 = Instant::now();
                        tokio::time::sleep(Duration::from_millis(2)).await;
                        let late = t0.elapsed().as_secs_f64() * 1000.0 - 2.0;
                        out.lock().unwrap_or_else(|e| e.into_inner()).push(late);
                    }
                })
            };
            tokio::time::sleep(during).await;
            stop.store(true, Ordering::Relaxed);
            let _ = task.await;
            // Bound to a local: a guard in tail position outlives `out` itself.
            let collected = out.lock().unwrap_or_else(|e| e.into_inner()).clone();
            collected
        }

        fn p(v: &mut Vec<f64>, q: f64) -> f64 {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            if v.is_empty() {
                return 0.0;
            }
            v[((v.len() - 1) as f64 * q) as usize]
        }

        // Phase 1 — the pre-fix shape.
        let inline_phase = {
            let (hash, raw) = (hash.clone(), Arc::<str>::from(raw));
            let beat = tokio::spawn(heartbeat_ms(Duration::from_millis(600)));
            let t0 = Instant::now();
            let jobs: Vec<_> = (0..40)
                .map(|_| {
                    let (hash, raw) = (hash.clone(), Arc::clone(&raw));
                    tokio::spawn(async move { verify_inline(&hash, &raw) })
                })
                .collect();
            for job in jobs {
                assert!(job.await.expect("inline job did not panic"));
            }
            let wall = t0.elapsed();
            (beat.await.expect("heartbeat"), wall)
        };
        // Phase 2 — the shipped shape.
        let bounded_phase = {
            let (hash, raw) = (hash.clone(), Arc::<str>::from(raw));
            let beat = tokio::spawn(heartbeat_ms(Duration::from_millis(600)));
            let t0 = Instant::now();
            let jobs: Vec<_> = (0..40)
                .map(|_| {
                    let (hash, raw) = (hash.clone(), Arc::clone(&raw));
                    tokio::spawn(async move { argon2_verify(hash, raw).await })
                })
                .collect();
            for job in jobs {
                assert!(job.await.expect("off-reactor job did not panic"));
            }
            let wall = t0.elapsed();
            (beat.await.expect("heartbeat"), wall)
        };

        let (mut inline_late, inline_wall) = inline_phase;
        let (mut bounded_late, bounded_wall) = bounded_phase;
        let (ime, bme) = (p(&mut inline_late, 0.5), p(&mut bounded_late, 0.5));
        let (imm, bmm) = (p(&mut inline_late, 1.0), p(&mut bounded_late, 1.0));
        let (inn, bnn) = (inline_late.len(), bounded_late.len());
        println!(
            "off-reactor A/B: inline n={inn} median={ime:.0}ms max={imm:.0}ms wall={inline_wall:.2?} \
             | bounded n={bnn} median={bme:.0}ms max={bmm:.0}ms wall={bounded_wall:.2?}"
        );

        // A starved runtime cannot deliver the 2 ms heartbeat at all, so the COUNT of
        // delivered beats and the WORST delay are the signal — a percentile over a handful
        // of samples would hide the stall.
        assert!(
            bmm * 5.0 < imm.max(1.0),
            "unrelated work must not wait behind verification: inline max={imm:.1}ms \
             (n={inn}) vs bounded max={bmm:.1}ms (n={bnn})"
        );
        assert!(
            bnn >= 3 * inn.max(1),
            "the off-reactor phase must deliver many more heartbeats: inline n={inn} vs \
             bounded n={bnn}"
        );
    }

    /// The hashing side must produce a usable PHC string, off the reactor, and `false`
    /// must stay the answer for a wrong password checked against it.
    #[tokio::test]
    async fn argon2_hash_round_trips_off_the_reactor() {
        let hash = argon2_hash("Proof!12345".to_string())
            .await
            .expect("hash a password");
        assert!(
            hash.starts_with("$argon2id$"),
            "a password hash must be an argon2id PHC string, got {hash}"
        );
        assert!(
            argon2_verify_result(hash.clone(), Arc::from("Proof!12345"))
                .await
                .expect("verify the correct password"),
            "the hash this helper produced must verify"
        );
        assert!(
            !argon2_verify_result(hash.clone(), Arc::from("wrong-password"))
                .await
                .expect("verify a wrong password"),
            "a wrong password must not verify"
        );
        assert!(
            argon2_verify_result("not-a-phc-string".to_string(), Arc::from("Proof!12345"))
                .await
                .is_err(),
            "an unusable stored hash must be an error, not a silent mismatch"
        );
    }

    /// The same controlled A/B as `off_reactor_verification_keeps_the_runtime_responsive`,
    /// for the HASHING side (`register`/`change_password`/`reset_password`).
    ///
    /// 40 concurrent hashes at the real production parameters (`Argon2::default()`, which is
    /// what [`argon2_hash`] uses) run twice on an 8-worker runtime, with an unrelated 2 ms
    /// heartbeat measuring how long it waits:
    /// * `inline` is the pre-fix shape — `hash_password` called straight from the async task
    ///   body (`register` did exactly this), parking a worker for the whole hash.
    /// * `bounded` is the shipped shape — [`argon2_hash`], off the reactor behind
    ///   [`argon2_permits`], the same semaphore the verification path uses.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn off_reactor_hashing_keeps_the_runtime_responsive() {
        use argon2::password_hash::SaltString;
        use argon2::PasswordHasher;
        use rand::rngs::OsRng;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Mutex;
        use std::time::{Duration, Instant};

        /// The pre-fix code path, verbatim as `register` used to run it.
        fn hash_inline(password: &str) -> String {
            let salt = SaltString::generate(&mut OsRng);
            Argon2::default()
                .hash_password(password.as_bytes(), &salt)
                .expect("hash a test password")
                .to_string()
        }

        // Measure how late the runtime wakes an unrelated 2 ms sleep.
        async fn heartbeat_ms(during: Duration) -> Vec<f64> {
            let stop = Arc::new(AtomicBool::new(false));
            let out = Arc::new(Mutex::new(Vec::<f64>::new()));
            let task = {
                let (stop, out) = (Arc::clone(&stop), Arc::clone(&out));
                tokio::spawn(async move {
                    while !stop.load(Ordering::Relaxed) {
                        let t0 = Instant::now();
                        tokio::time::sleep(Duration::from_millis(2)).await;
                        let late = t0.elapsed().as_secs_f64() * 1000.0 - 2.0;
                        out.lock().unwrap_or_else(|e| e.into_inner()).push(late);
                    }
                })
            };
            tokio::time::sleep(during).await;
            stop.store(true, Ordering::Relaxed);
            let _ = task.await;
            let collected = out.lock().unwrap_or_else(|e| e.into_inner()).clone();
            collected
        }

        fn p(v: &mut Vec<f64>, q: f64) -> f64 {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            if v.is_empty() {
                return 0.0;
            }
            v[((v.len() - 1) as f64 * q) as usize]
        }

        // Phase 1 — the pre-fix shape.
        let inline_phase = {
            let beat = tokio::spawn(heartbeat_ms(Duration::from_millis(600)));
            let t0 = Instant::now();
            let jobs: Vec<_> = (0..40)
                .map(|_| tokio::spawn(async move { hash_inline("Proof!12345") }))
                .collect();
            for job in jobs {
                assert!(
                    job.await
                        .expect("inline hash did not panic")
                        .starts_with("$argon2id$"),
                    "the inline shape must still produce a real hash"
                );
            }
            let wall = t0.elapsed();
            (beat.await.expect("heartbeat"), wall)
        };
        // Phase 2 — the shipped shape.
        let bounded_phase = {
            let beat = tokio::spawn(heartbeat_ms(Duration::from_millis(600)));
            let t0 = Instant::now();
            let jobs: Vec<_> = (0..40)
                .map(|_| tokio::spawn(async move { argon2_hash("Proof!12345".to_string()).await }))
                .collect();
            for job in jobs {
                let hash = job
                    .await
                    .expect("off-reactor job did not panic")
                    .expect("off-reactor hash failed");
                assert!(
                    hash.starts_with("$argon2id$"),
                    "the shipped shape must produce a real hash"
                );
            }
            let wall = t0.elapsed();
            (beat.await.expect("heartbeat"), wall)
        };

        let (mut inline_late, inline_wall) = inline_phase;
        let (mut bounded_late, bounded_wall) = bounded_phase;
        let (ime, bme) = (p(&mut inline_late, 0.5), p(&mut bounded_late, 0.5));
        let (imm, bmm) = (p(&mut inline_late, 1.0), p(&mut bounded_late, 1.0));
        let (inn, bnn) = (inline_late.len(), bounded_late.len());
        println!(
            "off-reactor HASH A/B: inline n={inn} median={ime:.0}ms max={imm:.0}ms wall={inline_wall:.2?} \
             | bounded n={bnn} median={bme:.0}ms max={bmm:.0}ms wall={bounded_wall:.2?}"
        );

        // Same signal as the verification A/B: the COUNT of delivered 2 ms beats and the
        // WORST delay, because a starved runtime cannot deliver the heartbeat at all.
        assert!(
            bnn >= 3 * inn.max(1),
            "the off-reactor hash phase must deliver many more heartbeats: inline n={inn} vs \
             bounded n={bnn}"
        );
        assert!(
            bmm * 3.0 < imm.max(1.0),
            "unrelated work must not wait behind hashing: inline max={imm:.1}ms \
             (n={inn}) vs bounded max={bmm:.1}ms (n={bnn})"
        );
    }
}
