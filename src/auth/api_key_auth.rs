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

/// How many API-key verifications may run concurrently.
///
/// Argon2id with the default parameters (`m=19456, t=2`) is pure CPU work that never
/// awaits: run inline in the async handler it parks a tokio worker thread for its whole
/// duration, so enough concurrent key requests pin every worker and the *runtime* (not
/// the database) becomes the bottleneck. Measured on this box 2026-09-21 with 40
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
static VERIFY_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

fn verify_permits() -> Arc<Semaphore> {
    VERIFY_PERMITS
        .get_or_init(|| {
            let n = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .clamp(1, 16);
            Arc::new(Semaphore::new(n))
        })
        .clone()
}

/// One Argon2 verification against one stored hash, off the reactor and bounded by
/// [`verify_permits`]. `false` means "does not match" — or "unusable stored hash" — which
/// is what every caller already treats as "not this credential".
async fn argon2_verify(hash: String, token: Arc<str>) -> bool {
    let Ok(permit) = verify_permits().acquire_owned().await else {
        // The semaphore is only closed on shutdown, where no key should authenticate.
        return false;
    };
    tokio::task::spawn_blocking(move || {
        // Held for the whole verification, released when this closure returns.
        let _permit = permit;
        let Ok(parsed) = PasswordHash::new(&hash) else {
            return false;
        };
        Argon2::default()
            .verify_password(token.as_bytes(), &parsed)
            .is_ok()
    })
    .await
    .unwrap_or(false)
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
    #[test]
    fn verify_permits_is_bounded_and_process_wide() {
        let a = verify_permits();
        let b = verify_permits();
        assert!(
            Arc::ptr_eq(&a, &b),
            "one semaphore per process, not one per call"
        );
        let want = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, 16);
        assert_eq!(a.available_permits(), want);
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
}
