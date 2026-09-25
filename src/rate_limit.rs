use axum::{
    body::{Body, Bytes},
    extract::{FromRequest, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Extension,
};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::warn;

use crate::auth::models::Claims;
use crate::AppState;

/// Bucket shared by every identity once the limiter table is at capacity.
const OVERFLOW_BUCKET: &str = "__overflow__";

/// Take a limiter-table lock, recovering from poisoning instead of panicking: this runs
/// on every request, so a panic elsewhere in the process must not become a permanent 500.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Per-account rate limiter keyed by aid
#[derive(Clone)]
pub struct RateLimiters {
    /// Per-account rate limiters for API endpoints
    account_limiters: Arc<Mutex<HashMap<String, Arc<DefaultDirectRateLimiter>>>>,
    /// Default max requests per account per second
    max_per_second: u32,
    /// Default burst
    burst: u32,
    /// Upper bound on the number of buckets held (0 = unbounded)
    max_entries: usize,
}

impl RateLimiters {
    pub fn new(max_per_second: u32, burst: u32) -> Self {
        Self::with_capacity(max_per_second, burst, 10_000)
    }

    /// `max_entries` bounds the bucket table. Callers of the pre-auth limiter are keyed by
    /// a client-controlled identity, so an unbounded table would be a memory-exhaustion
    /// vector of its own.
    pub fn with_capacity(max_per_second: u32, burst: u32, max_entries: usize) -> Self {
        Self {
            account_limiters: Arc::new(Mutex::new(HashMap::new())),
            max_per_second,
            burst,
            max_entries,
        }
    }

    /// Get or create a rate limiter for an account (or, for the pre-auth limiter, a
    /// client identity).
    pub fn for_account(&self, aid: &str) -> Arc<DefaultDirectRateLimiter> {
        let mut limiters = lock(&self.account_limiters);
        if let Some(limiter) = limiters.get(aid) {
            return limiter.clone();
        }
        // Table full: new identities share one bucket instead of growing the map.
        let key = if self.max_entries > 0 && limiters.len() >= self.max_entries {
            warn!(
                "rate limiter table at capacity ({} buckets) — new identities share one bucket",
                limiters.len()
            );
            OVERFLOW_BUCKET
        } else {
            aid
        };
        if let Some(limiter) = limiters.get(key) {
            return limiter.clone();
        }
        // Create a new rate limiter: max_per_second requests per second, with burst
        let quota = Quota::per_second(NonZeroU32::new(self.max_per_second.max(1)).unwrap())
            .allow_burst(NonZeroU32::new(self.burst.max(1)).unwrap());
        let limiter = Arc::new(RateLimiter::direct(quota));
        limiters.insert(key.to_string(), limiter.clone());
        limiter
    }

    /// Cleanup old limiters to prevent memory leaks
    pub fn cleanup(&self) {
        // This is a no-op for now; limiters are lightweight
        // In production, you'd periodically remove entries not seen in N minutes
    }
}

/// The 429 every limiter answers with. Fallible builders are handled, never unwrapped:
/// an empty 429 is still a 429 and must not panic a request path.
fn rate_limited_response() -> Response {
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("Retry-After", "1")
        .body(axum::body::Body::from(
            "{\"error\":\"Rate limit exceeded. Wait before retrying.\",\"status\":429}",
        ))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .body(axum::body::Body::empty())
                .unwrap_or_else(|_| Response::new(axum::body::Body::empty()))
        })
}

/// Client identity for the pre-auth limiter, read from headers the fronting proxy sets:
/// Cloudflare's `CF-Connecting-IP`, then nginx's `X-Real-IP` (`proxy_set_header X-Real-IP
/// $remote_addr`, which replaces any client-supplied value), then the rightmost
/// `X-Forwarded-For` hop — the one nginx appended, not the spoofable left-hand entries.
/// A caller presenting none of them shares the `unknown` bucket, so omitting the address
/// is not a way around the limiter.
fn client_identity(headers: &axum::http::HeaderMap) -> String {
    let read = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    if let Some(ip) = read("cf-connecting-ip") {
        return ip;
    }
    if let Some(ip) = read("x-real-ip") {
        return ip;
    }
    if let Some(xff) = read("x-forwarded-for") {
        if let Some(last) = xff
            .rsplit(',')
            .map(|hop| hop.trim())
            .find(|h| !h.is_empty())
        {
            return last.to_string();
        }
    }
    "unknown".to_string()
}

/// Middleware: throttle a request BEFORE any credential is verified.
///
/// `rate_limit_middleware` cannot do this: it extracts `Claims`, which exist only after
/// `auth_middleware` has verified the credential — and an API key is verified with Argon2
/// against stored hashes, so that is real CPU. In axum the LAST `.layer()` call is the
/// OUTERMOST, so `routes.rs` layers this one after auth_middleware and it runs first.
///
/// It applies to API-key credentials, the only credential class on this router whose
/// verification is expensive (a JWT is an HMAC check): a caller presenting
/// `workflowswift_...` junk is answered 429 before any hash work is queued. The limit sits
/// above the per-account limit (30/s, applied after auth), so a well-behaved client is
/// never throttled here before its own account budget runs out. Neither the credential nor
/// a hash of it is logged.
pub async fn pre_auth_rate_limit_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    let credential_len = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|token| token.trim())
        .filter(|token| crate::auth::api_key_auth::is_api_key(token))
        .map(|token| token.len());

    if let Some(len) = credential_len {
        let identity = client_identity(request.headers());
        let limiter = state.pre_auth_limiters.for_account(&identity);
        if limiter.check().is_err() {
            warn!(
                "Pre-auth rate limit exceeded for {} (api key credential, {} chars)",
                identity, len
            );
            return rate_limited_response();
        }
    }

    next.run(request).await
}

/// Default ceiling on concurrent requests inside the unauthenticated password-auth routes.
///
/// Chosen as 8x the Argon2 bound (`auth::api_key_auth::argon2_permits()`, one permit per
/// core = 8 here): the ceiling must never be reached by real traffic, and 64 concurrent
/// logins is already ~8x more work than the box can hash at once, so everything above it is
/// queue that exists only to be shed. Override with `AUTH_IN_FLIGHT_CAP`.
pub const DEFAULT_AUTH_IN_FLIGHT_CAP: usize = 64;

/// How long one unauthenticated password-auth request may HOLD an in-flight slot.
///
/// The ceiling is a count of concurrent requests, and a slot is held for the whole request —
/// including the handler's body read. Without a bound, a client that sends headers and then
/// stalls its body owns its slot for ever, so 64 such sockets pin the ceiling permanently and
/// every real login is shed with 429 (kanban t_5ad88ced, reproduced live 2026-09-22). A
/// genuine login finishes its Argon2 work in well under a second even under load, so this
/// budget is orders of magnitude above any legitimate request and only ever fires on a
/// request that is not going to complete anyway.
pub const AUTH_IN_FLIGHT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Load-shedding bound on the unauthenticated password-auth routes (kanban t_92bafdf8).
///
/// The Argon2 semaphore bounds the *work* a password flood can do (8 concurrent 19 MiB hash
/// jobs; 300 wrong-password logins are answered in 2.24 s no matter how fast they arrive).
/// It does not bound the *arrival*: every login that cannot get a hashing permit waits,
/// holding a task, a connection and — once the handler's `Json` extractor runs — a body
/// buffer. Nothing caps how many such waiters exist, so the queue itself is unbounded and a
/// large enough flood pushes legitimate logins behind it (measured on the pre-change binary:
/// 300 logins at 400 in flight = every one answered, median 1.3 s, and the unrelated
/// `/api/v1/health` canary p95 went to 899 ms).
///
/// This bounds the queue, one level ABOVE the hashing permit: at most `cap` requests are
/// allowed to be inside these routes at once. The next request is answered `429` with
/// `Retry-After: 1` immediately — it never waits, never has its body read, never reaches
/// Argon2. Nothing is remembered about the caller, so there is no lockout to trip and no
/// address to hold responsible: the shed is a function of how busy the box is right now,
/// and a shed request succeeds as soon as the flood ends.
///
/// Why not key this on the client address or the account: the address is not an identity
/// (behind Cloudflare every office/NAT egress shares one), a body-aware key is not available
/// to middleware (headers only), and a per-account lockout hands an attacker a DoS against
/// any victim whose email they know. Losing a request to a transient 429 is recoverable by
/// retrying; losing an account is not.
#[derive(Clone)]
pub struct AuthInFlight {
    permits: Arc<Semaphore>,
    cap: usize,
}

impl AuthInFlight {
    pub fn new(cap: usize) -> Self {
        // A cap of zero would shed every password request, so it is clamped rather than
        // honoured: misconfiguration must not lock the product out.
        let cap = cap.max(1);
        Self {
            permits: Arc::new(Semaphore::new(cap)),
            cap,
        }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    /// How many password-auth requests are inside the routes right now.
    pub fn in_flight(&self) -> usize {
        self.cap - self.permits.available_permits()
    }

    /// Non-blocking admission. `None` means the ceiling is reached and this request must be
    /// shed instead of parked — waiting here is exactly the unbounded queue this closes.
    pub fn try_enter(&self) -> Option<OwnedSemaphorePermit> {
        self.permits.clone().try_acquire_owned().ok()
    }
}

/// The 429 a shed request gets. Distinct from [`rate_limited_response`] on purpose: this one
/// says the request was not admitted, not that a credential ran out of budget, and it carries
/// the ceiling so an operator reading a client log knows which bound was hit.
fn auth_shed_response(cap: usize) -> Response {
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("Retry-After", "1")
        .body(axum::body::Body::from(format!(
            "{{\"error\":\"Too many concurrent authentication requests. Retry shortly.\",\
             \"status\":429,\"limit\":{cap}}}"
        )))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header("Retry-After", "1")
                .body(axum::body::Body::empty())
                .unwrap_or_else(|_| Response::new(axum::body::Body::empty()))
        })
}

/// Middleware: admit a request into an unauthenticated password-auth route only while there
/// is room, and shed it with a 429 the moment there is not.
///
/// Mounted on the `auth_public` sub-router (`src/routes.rs`), so it covers exactly
/// `POST /auth/{login,register,forgot-password,reset-password}` — the routes that do Argon2
/// work or send mail for an unauthenticated caller — and touches nothing else.
pub async fn password_auth_shed_middleware(
    State(in_flight): State<AuthInFlight>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    match in_flight.try_enter() {
        // The permit is held for the whole request, which is what makes the ceiling a count
        // of concurrent requests rather than a rate. But "the whole request" includes the
        // handler's body read, so a client that sends headers and then STALLS the body used to
        // hold its slot for as long as it liked: 64 such sockets reached the ceiling and kept
        // it reached indefinitely, and every legitimate login got 429 for ever (reproduced
        // live 2026-09-22: a real login answered 429 at t+3s and still 429 at t+20s while 64
        // stalled sockets were open, and 401 normally the moment they closed). A slot is
        // therefore bounded: a request that cannot finish inside the budget is shed like any
        // other over-capacity request, so the ceiling always clears on its own.
        Some(_permit) => {
            match tokio::time::timeout(AUTH_IN_FLIGHT_TIMEOUT, next.run(request)).await {
                Ok(response) => response,
                Err(_) => {
                    warn!(
                    "password-auth request held a slot for over {:?} (stalled body?) — shed with 429",
                    AUTH_IN_FLIGHT_TIMEOUT
                );
                    auth_shed_response(in_flight.cap())
                }
            }
        }
        None => {
            let path = request.uri().path().to_string();
            warn!(
                "password-auth in-flight cap reached ({}/{} in flight): shed {} with 429",
                in_flight.in_flight(),
                in_flight.cap(),
                path
            );
            auth_shed_response(in_flight.cap())
        }
    }
}

/// Default body-read deadline: how long the body of a request that carries one may take to
/// arrive before the request is answered `408` and its task, connection and partially-read body
/// buffer are released (kanban t_e7cba83e). See [`body_read_deadline_middleware`] for why the
/// bound is on the body and not on the handler.
///
/// 30 s is orders of magnitude above the time a real body on these routes takes (a Stripe/PayPal
/// event, an n8n run result, a lead POST: kilobytes over a same-region link, tens of
/// milliseconds) and still generous to a slow sender: a full 2 MiB body may arrive as slowly as
/// ~70 KiB/s and complete inside it. Override with `BODY_READ_DEADLINE_SECS` (clamped to
/// `5..=300` in `config.rs`, so neither a typo nor a fat finger can shed real traffic).
pub const DEFAULT_BODY_READ_DEADLINE_SECS: u64 = 30;

/// How long a request body may take to arrive, as configured.
///
/// Its own type, like [`AuthInFlight`], so the middleware can be mounted (and unit-tested)
/// without an `AppState` — and so the number the middleware enforces is the number the boot log
/// printed, never re-derived per request.
#[derive(Clone, Copy, Debug)]
pub struct BodyReadDeadline(std::time::Duration);

impl BodyReadDeadline {
    /// From the configured seconds. Clamping lives in `config.rs`; this is a plain carrier.
    pub fn from_secs(seconds: u64) -> Self {
        Self(std::time::Duration::from_secs(seconds))
    }

    /// The deadline as a duration.
    pub fn duration(self) -> std::time::Duration {
        self.0
    }
}

/// The `408` a request whose body never finished arriving is answered with.
///
/// Carries the deadline so a client log says which bound was hit, and `Connection: close`
/// because the declared body was never consumed: this connection cannot be reused for another
/// request, and a client that keeps waiting for a drain that will not come learns nothing from
/// silence.
fn body_deadline_response(deadline: std::time::Duration) -> Response {
    Response::builder()
        .status(StatusCode::REQUEST_TIMEOUT)
        .header("Connection", "close")
        .body(Body::from(format!(
            "{{\"error\":\"Request body was not received in time. Retry.\",\"status\":408,\
             \"body_deadline_seconds\":{}}}",
            deadline.as_secs()
        )))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::REQUEST_TIMEOUT)
                .header("Connection", "close")
                .body(Body::empty())
                .unwrap_or_else(|_| Response::new(Body::empty()))
        })
}

/// Middleware: bound how long a request BODY may take to arrive, and answer `408` if it has not
/// finished within [`BodyReadDeadline`] of the headers.
///
/// **Why a body deadline and not a handler budget** (kanban t_e7cba83e). The routes this is
/// mounted on all do their own credential or signature work *after* the body has to be read:
/// `webhooks/stripe` and `webhooks/paypal` verify a signature over the raw bytes,
/// `instances/{id}/callback` and the `internal/*` routes check `X-Internal-Key` inside the
/// handler, `/incoming` is a public receiver. An in-route wall-clock budget over those routes
/// would have to be sized for signature verification plus whatever the handler dispatches (an
/// n8n proxy call), and elapsing it drops work that was legitimately in progress — for a payment
/// webhook that is a LOST PAYMENT EVENT, not a retried login. This middleware bounds only the
/// thing that is actually unbounded: a client that sends a request head and then stops. A handler
/// that legitimately takes seconds is untouched, because the deadline is over by the time it
/// runs.
///
/// **The deadline is total, measured from the headers** — not a per-chunk idle timeout that
/// resets on every frame (`tower_http::timeout::RequestBodyTimeoutLayer` works that way). A
/// resetting timeout is not a bound here at all: a client that dribbles one byte every N-1
/// seconds holds the task, the connection and the buffer for ever. On elapse the inner future is
/// dropped, so the partially-read body goes with it, the request is answered `408` and the
/// server logs the event — the hold ends, visibly.
///
/// **What a legitimately slow sender gets.** A body that *is* arriving is read at full speed; the
/// deadline only fires on one that has stopped. A mobile client uploading a few KiB over a bad
/// link needs well under a second; a 2 MiB body has 30 s, i.e. ~70 KiB/s. A webhook burst is
/// unaffected (the bound is per request, not a rate). If a body really does exceed the deadline,
/// the request is answered `408` and closed; Stripe and PayPal retry failed deliveries for days,
/// and the event was already undeliverable at that point — the sender had stopped mid-body.
///
/// On success the bytes read here are handed to the inner service as an already-complete body, so
/// the handler's own extractor (`Json`, `Bytes`) sees exactly the bytes the client sent — same
/// bytes, same headers, same limit, because the read below *is* the same extractor: a body over
/// axum's `DefaultBodyLimit` is rejected through the identical code path, with the identical
/// `413`.
pub async fn body_read_deadline_middleware(
    State(deadline): State<BodyReadDeadline>,
    request: Request,
    next: Next,
) -> Response {
    let deadline = deadline.duration();

    // The head is cloned rather than rebuilt so the inner request keeps the method, uri, version,
    // headers and every extension an outer layer inserted. Cloning `Parts` copies the extensions
    // map (shared handles), so nothing inserted by a layer above is lost.
    let (parts, body) = request.into_parts();
    let probe = Request::from_parts(parts.clone(), body);

    // `()` is the extractor state: `Bytes::from_request` takes its limit from the request's own
    // extensions (axum's `DefaultBodyLimit`), not from the state, so the limit the handler would
    // have applied is the limit applied here.
    match tokio::time::timeout(deadline, Bytes::from_request(probe, &())).await {
        Ok(Ok(bytes)) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        // Over the body limit, or a read error on the way in: exactly the rejection the handler's
        // own extractor would have produced, produced by the same extractor.
        Ok(Err(rejection)) => rejection.into_response(),
        Err(_elapsed) => {
            warn!(
                "request body not received within {:?} (stalled body) — answered 408 for {} {}",
                deadline, parts.method, parts.uri
            );
            body_deadline_response(deadline)
        }
    }
}

/// Middleware: rate limit by account ID
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    let aid = &claims.aid;
    let limiter = state.rate_limiters.for_account(aid);

    if limiter.check().is_err() {
        warn!("Rate limit exceeded for account {}", aid);
        return rate_limited_response();
    }

    next.run(request).await
}

/// Cached provider key resolution to avoid DB hits on every step execution
#[derive(Clone)]
pub struct ProviderKeyCache {
    /// Simple in-memory cache: aid:provider -> (api_key, base_url, metadata)
    cache: Arc<Mutex<HashMap<String, (String, Option<String>, serde_json::Value, i64)>>>,
    /// TTL in seconds
    ttl_seconds: i64,
}

impl ProviderKeyCache {
    pub fn new(ttl_seconds: i64) -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            ttl_seconds,
        }
    }

    /// Build the cache key
    fn key(aid: &str, provider: &str) -> String {
        format!("{}:{}", aid, provider)
    }

    /// Get cached value, returns None if expired or missing
    pub fn get(
        &self,
        aid: &str,
        provider: &str,
    ) -> Option<(String, Option<String>, serde_json::Value)> {
        let cache = self.cache.lock().unwrap();
        let key = Self::key(aid, provider);
        if let Some((api_key, base_url, metadata, expires_at)) = cache.get(&key) {
            if *expires_at > chrono::Utc::now().timestamp() {
                return Some((api_key.clone(), base_url.clone(), metadata.clone()));
            }
        }
        None
    }

    /// Set a value in the cache
    pub fn set(
        &self,
        aid: &str,
        provider: &str,
        api_key: String,
        base_url: Option<String>,
        metadata: serde_json::Value,
    ) {
        let mut cache = self.cache.lock().unwrap();
        let key = Self::key(aid, provider);
        let expires_at = chrono::Utc::now().timestamp() + self.ttl_seconds;
        cache.insert(key, (api_key, base_url, metadata, expires_at));
    }

    /// Invalidate a specific entry (called when keys are updated/deleted)
    pub fn invalidate(&self, aid: &str, provider: &str) {
        let mut cache = self.cache.lock().unwrap();
        let key = Self::key(aid, provider);
        cache.remove(&key);
    }

    /// Invalidate all entries for an account
    pub fn invalidate_account(&self, aid: &str) {
        let mut cache = self.cache.lock().unwrap();
        cache.retain(|k, _| !k.starts_with(&format!("{}:", aid)));
    }

    /// Get total cached entries count
    pub fn len(&self) -> usize {
        self.cache.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    /// The bound is the whole point: `cap` requests are admitted, the next one is shed, and a
    /// released permit re-admits. Nothing here is time-based, so the assertion is exact.
    #[test]
    fn auth_in_flight_sheds_at_the_cap_and_readmits_on_release() {
        let aif = AuthInFlight::new(2);
        assert_eq!(aif.cap(), 2);
        assert_eq!(aif.in_flight(), 0);
        let a = aif.try_enter().expect("1st admission");
        let b = aif.try_enter().expect("2nd admission");
        assert_eq!(aif.in_flight(), 2);
        assert!(
            aif.try_enter().is_none(),
            "the request after the cap must be shed, not parked"
        );
        assert_eq!(aif.in_flight(), 2, "a shed request must not be counted");
        drop(a);
        let c = aif.try_enter().expect("a released permit re-admits");
        drop((b, c));
        assert_eq!(aif.in_flight(), 0);
    }

    /// A misconfigured cap of zero must not shed the whole product.
    #[test]
    fn zero_cap_is_clamped_to_one() {
        let aif = AuthInFlight::new(0);
        assert_eq!(aif.cap(), 1);
        let _held = aif.try_enter().expect("one request always gets in");
        assert!(aif.try_enter().is_none());
    }

    /// Same property, but through the real middleware on a real router: while the only permit
    /// is held, the route answers 429 with Retry-After and the ceiling in the body; once it is
    /// released the same request is served normally.
    #[tokio::test]
    async fn shed_middleware_answers_429_only_while_the_cap_is_reached() {
        let aif = AuthInFlight::new(1);
        let app = Router::new()
            .route("/login", post(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                aif.clone(),
                password_auth_shed_middleware,
            ));

        let held = aif.try_enter().expect("occupy the only slot");
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::post("/login")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router");
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok()),
            Some("1")
        );
        let body = axum::body::to_bytes(resp.into_body(), 4096)
            .await
            .expect("body");
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("\"limit\":1"), "body was {text}");

        drop(held);
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::post("/login")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a request below the cap must never be shed"
        );
    }

    /// The production shape: the layer is applied to a sub-router that is then NESTED and
    /// MERGED into the app. A layer that only works when applied last would be useless here,
    /// so this test mounts exactly like `src/routes.rs` does.
    #[tokio::test]
    async fn shed_survives_nest_and_merge() {
        let aif = AuthInFlight::new(1);
        let auth_public = Router::new()
            .route("/login", post(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                aif.clone(),
                password_auth_shed_middleware,
            ));
        let public = Router::new()
            .nest("/auth", auth_public)
            .route("/health", axum::routing::get(|| async { "ok" }));
        let api = Router::new()
            .merge(public)
            .merge(Router::new().route("/other", axum::routing::get(|| async { "ok" })));
        let app = Router::new()
            .route("/", axum::routing::get(|| async { "ok" }))
            .nest("/api/v1", api);

        let held = aif.try_enter().expect("occupy the only slot");
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::post("/api/v1/auth/login")
                    .body(Body::empty())
                    .expect("req"),
            )
            .await
            .expect("router");
        assert_eq!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "the layer on the nested sub-router did not run"
        );
        // An unrelated route must stay open while the auth route is shedding.
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::get("/api/v1/health")
                    .body(Body::empty())
                    .expect("req"),
            )
            .await
            .expect("router");
        assert_eq!(resp.status(), StatusCode::OK);
        drop(held);
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::post("/api/v1/auth/login")
                    .body(Body::empty())
                    .expect("req"),
            )
            .await
            .expect("router");
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ── body-read deadline (kanban t_e7cba83e) ──────────────────────────────────────────────
    //
    // Three properties, and they are the ones a later refactor must not break: the deadline fires
    // on a body that never arrives; a body that does arrive reaches the handler byte for byte
    // (Stripe/PayPal sign the raw bytes, so anything else silently breaks payments); and the
    // middleware does not widen the limit on how much a single request may buffer.

    /// A request body that produces no frame and never ends: the client-side shape of the hold
    /// this middleware bounds (head sent, body never arrives).
    struct StalledBody;

    impl futures_core::Stream for StalledBody {
        type Item = Result<Bytes, std::io::Error>;

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Pending
        }
    }

    /// The production mount shape: one route whose handler reads the body, wrapped by the layer.
    fn body_deadline_app(deadline: std::time::Duration) -> Router {
        Router::new()
            .route("/webhook", post(|body: Bytes| async move { body }))
            .layer(axum::middleware::from_fn_with_state(
                BodyReadDeadline(deadline),
                body_read_deadline_middleware,
            ))
    }

    /// A stalled body must end the hold: 408, `Connection: close`, promptly. Before this
    /// middleware the same request held a task, a connection and a partially-read body buffer
    /// for ever (measured live at 45 s and still parked).
    #[tokio::test]
    async fn stalled_body_is_answered_408_within_the_deadline() {
        // 150 ms, not the configured 30 s: the assertion is about WHICH requests the deadline
        // fires on, not how long the operator set it to.
        let app = body_deadline_app(std::time::Duration::from_millis(150));
        let started = std::time::Instant::now();
        let resp = app
            .oneshot(
                HttpRequest::post("/webhook")
                    .header("content-length", "100000")
                    .body(Body::from_stream(StalledBody))
                    .expect("request"),
            )
            .await
            .expect("router");
        let elapsed = started.elapsed();

        assert_eq!(
            resp.status(),
            StatusCode::REQUEST_TIMEOUT,
            "a body that never arrives must be answered, not parked"
        );
        assert_eq!(
            resp.headers()
                .get("connection")
                .and_then(|v| v.to_str().ok()),
            Some("close"),
            "the declared body was never consumed, so the connection must not be reused"
        );
        let body = axum::body::to_bytes(resp.into_body(), 4096)
            .await
            .expect("body");
        assert!(
            String::from_utf8_lossy(&body).contains("\"status\":408"),
            "body was {}",
            String::from_utf8_lossy(&body)
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "the deadline must fire promptly, took {elapsed:?}"
        );
    }

    /// The bytes the client sends are the bytes the handler sees. This is why the deadline reads
    /// the body instead of replacing it: the webhook routes verify an HMAC over exactly these
    /// bytes.
    #[tokio::test]
    async fn complete_body_reaches_the_handler_unchanged() {
        let app = body_deadline_app(std::time::Duration::from_secs(30));
        let payload = br#"{"type":"checkout.session.completed","sig":"t=1,v1=deadbeef"}"#;
        let resp = app
            .oneshot(
                HttpRequest::post("/webhook")
                    .header("content-type", "application/json")
                    .header("stripe-signature", "t=1,v1=deadbeef")
                    .body(Body::from(payload.to_vec()))
                    .expect("request"),
            )
            .await
            .expect("router");

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096)
            .await
            .expect("body");
        assert_eq!(
            &body[..],
            &payload[..],
            "the handler must see the raw request bytes, unmodified"
        );
    }

    /// A body over axum's own 2 MiB limit gets the extractor's own rejection: the deadline must
    /// not become a wider hole for a single unauthenticated request to pin memory.
    #[tokio::test]
    async fn body_over_the_default_limit_is_rejected_413() {
        let app = body_deadline_app(std::time::Duration::from_secs(30));
        let oversized = vec![b'x'; 2 * 1024 * 1024 + 1];
        let resp = app
            .oneshot(
                HttpRequest::post("/webhook")
                    .body(Body::from(oversized))
                    .expect("request"),
            )
            .await
            .expect("router");
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
