//! The one credential boundary every guarded path passes through (kanban t_061a4e26).
//!
//! Precedent, copied not redesigned: `CoreSwift-CRM/src/auth/boundary.rs` @0ca91e5 (itself from
//! `FunnelSwift/src/auth/boundary.rs` @704fc21).
//!
//! WorkflowSwift already had a fail-closed shape — private by being registered on
//! `protected_routes`, public by being registered on `public_routes`, with the dead auth-skip list
//! inside `auth::middleware` deleted by kanban t_73f6724d — but nothing enforced that shape outside
//! the author's memory, and the census still found two routes that answered an anonymous caller
//! (see `route_policy`'s module docs). [`require_credential`] is mounted once, OUTSIDE the routing
//! layers, so every request to a path [`crate::auth::route_policy::is_guarded_path`] covers is
//! decided here first.
//!
//! It answers exactly one question — *does this caller present a credential?* — and it can only
//! refuse an anonymous caller. Authorization (which account, which role, which row) stays where it
//! already was: `auth::middleware::auth_middleware` on the protected router and the handler.
//!
//! The refusal is deliberately distinguishable from every handler's own answer: the body is
//! `{"error":"Authentication required","status":401}` and never the app's `AppError` shape
//! (`{"code":401,"error":true,"message":"Authentication required"}`), which is what makes the
//! boundary provable live — a curl that gets THIS body never reached a handler.

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::auth::route_policy;
use crate::AppState;

/// The one user-visible string for "no credential at all".
pub const BOUNDARY_REFUSAL: &str = "Authentication required";

/// Length-independent comparison, so a wrong key cannot be distinguished from a right one by how
/// long it took to be refused.
fn ct_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut diff = a.len() ^ b.len();
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= (x ^ y) as usize;
    }
    diff == 0
}

/// Arm for [`route_policy::INTERNAL_ROUTES`]: the app's own shared secret in `x-internal-key`.
///
/// An app with NO key configured never authorises anything — this is what closes
/// `POST /api/v1/incoming`'s fail-open branch, which treated an unconfigured key as "no check
/// needed" and would have answered an anonymous caller.
fn presents_internal_key(state: &AppState, req: &Request) -> bool {
    if state.config.internal_sync_key.is_empty() {
        return false;
    }
    match req
        .headers()
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
    {
        Some(key) => ct_eq(key, &state.config.internal_sync_key),
        None => false,
    }
}

/// Arm for every other guarded path: a credential this app already recognises.
///
/// * `workflowswift_<...>` — an issued API key. Its validation is a database read
///   (`auth::api_key_auth::authenticate` against `api_keys`) that `auth_middleware` performs on the
///   protected router, so the boundary requires only the app's key SHAPE and leaves the decision
///   where the credential lives.
/// * anything else — parsed as an app JWT with the SAME `auth::middleware::verify_token` the
///   protected router uses, so a token that passes here cannot fail their check.
fn presents_session_credential(state: &AppState, req: &Request) -> bool {
    match bearer(req) {
        Some(token) if crate::auth::api_key_auth::is_api_key(token) => true,
        Some(token) => {
            crate::auth::middleware::verify_token(token, &state.config.jwt_secret).is_ok()
        }
        None => false,
    }
}

fn bearer(req: &Request) -> Option<&str> {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn reject(status: StatusCode, error: &str) -> Response {
    (
        status,
        Json(json!({ "error": error, "status": status.as_u16() })),
    )
        .into_response()
}

/// Global fail-closed auth. Runs outside the routing layers, so it sees every request before any
/// router's own middleware does.
pub async fn require_credential(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();

    // Served surfaces and anything outside the API tree carry no credential and are not this
    // boundary's business — see `route_policy::is_guarded_path`.
    if !route_policy::is_guarded_path(&path) || route_policy::is_public_route(&path) {
        return next.run(req).await;
    }

    // ── the machine surface ─────────────────────────────────────────────────────────────────────
    // Named in `route_policy::INTERNAL_ROUTES`. Before this the only thing standing between an
    // anonymous caller and these handlers was the author's own key check — and `/incoming` skipped
    // its check entirely when no key was configured. The key is now demanded HERE too, so a route
    // added under one of these prefixes without an entry is an ordinary private route, and an
    // unconfigured key refuses instead of admitting.
    //
    // `/api/v1/instances/{id}/callback` is in this list because n8n presents the shared key; the
    // panel presents a JWT instead and the handler checks the instance's owner, so a recognised
    // session credential is accepted on this arm as well.
    if route_policy::is_internal_route(&path) {
        return if presents_internal_key(&state, &req) || presents_session_credential(&state, &req) {
            next.run(req).await
        } else {
            reject(StatusCode::UNAUTHORIZED, BOUNDARY_REFUSAL)
        };
    }

    // ── everything else: a credential this app recognises ───────────────────────────────────────
    if presents_session_credential(&state, &req) {
        next.run(req).await
    } else {
        reject(StatusCode::UNAUTHORIZED, BOUNDARY_REFUSAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;

    fn req_with(headers: &[(&str, &str)]) -> Request {
        let mut b = HttpRequest::builder().uri("/api/v1/workflows");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(Body::empty()).unwrap()
    }

    fn jwt(secret: &str, exp_offset: i64) -> String {
        use jsonwebtoken::{encode, EncodingKey, Header};
        #[derive(serde::Serialize)]
        struct C {
            sub: String,
            aid: String,
            role: String,
            exp: usize,
            iat: usize,
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let claims = C {
            sub: "canary-user-subject".to_string(),
            aid: "canary-account-aid".to_string(),
            role: "owner".to_string(),
            exp: (now + exp_offset) as usize,
            iat: now as usize,
        };
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    #[test]
    fn the_internal_key_compare_is_exact() {
        assert!(ct_eq("s3cret", "s3cret"));
        assert!(!ct_eq("s3cret", "s3cre"));
        assert!(!ct_eq("s3cre", "s3cret"));
        assert!(!ct_eq("", "s3cret"));
        assert!(!ct_eq("s3cret", ""));
        // An empty configured key is refused before the compare (see presents_internal_key).
        assert!(ct_eq("", ""));
    }

    /// The boundary reads the committed allowlist, not a local array: this pins the delegation so
    /// the two cannot drift.
    #[test]
    fn the_boundary_reads_the_committed_allowlist() {
        assert!(route_policy::is_public_route("/api/v1/health"));
        assert!(!route_policy::is_public_route("/api/v1/workflows"));
        assert!(route_policy::is_internal_route(
            "/api/v1/internal/portfolio-sync"
        ));
        assert!(route_policy::is_internal_route("/api/v1/incoming"));
        // the two routes this card moved: an UNLISTED sibling is neither public nor internal
        assert!(!route_policy::is_public_route("/api/v1/bridge-tasks"));
        assert!(!route_policy::is_internal_route("/api/v1/bridge-tasks"));
        assert!(!route_policy::is_public_route("/api/v1/internal/whatever"));
        assert!(!route_policy::is_internal_route(
            "/api/v1/internal/whatever"
        ));
    }

    /// A bearer is only a credential when the app can recognise it — this is the difference between
    /// "refuse anonymous" and "refuse everyone", and it is what keeps a garbage bearer from being
    /// handed to a route that has no check of its own.
    #[test]
    fn the_session_arm_recognises_a_jwt_and_the_app_key_shape() {
        const SECRET: &str = "boundary-unit-test-secret";
        let good = jwt(SECRET, 3600);
        assert!(route_policy::is_guarded_path("/api/v1/workflows"));

        // A valid JWT verifies; a JWT signed with another key, and an expired one, do not.
        assert!(crate::auth::middleware::verify_token(&good, SECRET).is_ok());
        assert!(crate::auth::middleware::verify_token(&jwt("other-secret", 3600), SECRET).is_err());
        assert!(crate::auth::middleware::verify_token(&jwt(SECRET, -7200), SECRET).is_err());

        // The app's own API-key prefix is a recognised credential SHAPE; its verification is the
        // database read `auth_middleware` performs.
        assert!(crate::auth::api_key_auth::is_api_key("workflowswift_abc"));
        assert!(!crate::auth::api_key_auth::is_api_key("not-a-key"));
        assert!(!crate::auth::api_key_auth::is_api_key(""));
    }

    #[test]
    fn the_bearer_extraction_is_exact() {
        assert_eq!(
            bearer(&req_with(&[("authorization", "Bearer tok")])),
            Some("tok")
        );
        assert_eq!(bearer(&req_with(&[("authorization", "tok")])), None);
        assert_eq!(bearer(&req_with(&[("authorization", "Bearer ")])), Some(""));
        assert_eq!(bearer(&req_with(&[])), None);
    }
}
