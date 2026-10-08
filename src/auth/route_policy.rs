//! Default-deny routing for the mounted surface (kanban t_061a4e26). Precedent, copied not
//! redesigned: `CoreSwift-CRM/src/auth/route_policy.rs` @0ca91e5, `FunnelSwift/src/auth/route_policy.rs`
//! @704fc21, `IncentiveSwift/src/security/route_policy.rs` @47c909a4.
//!
//! # The rule
//!
//! **A mounted route is PRIVATE unless it appears in one of the two lists below.**
//! [`crate::auth::boundary::require_credential`] is mounted once, OUTSIDE the routing layers
//! (`src/main.rs`), and reads only this module. Nothing else decides whether a route may be
//! reached anonymously.
//!
//! # The census (measured 2026-10-06, from `src/routes.rs`, then verified live with an anonymous
//! probe)
//!
//! ```text
//!   179 mounted `.route(..)` calls in the one routing file, and 39 `.nest(..)` targets
//!        (two of them — `/step-integrations` and `/available-integrations` — are deliberately
//!        commented out, see the note beside the list below)
//!   160 mounts reach `protected_routes`  (39 nested sub-routers, 153 route entries, plus the six
//!        direct mounts: /impersonate, /stop-impersonation, /checkout/{create,sessions},
//!        /payment-providers, /payment-providers/{provider_type})
//!    24 mounts reach `public_routes` (which merges `public_body_routes` and nests `auth_public`)
//!
//!   (+2 from kanban t_ede5f6ed: `/api/v1/internal/provision-free-account` joins the public body
//!    sub-router as a named key route, `/api/v1/admin/provisioning-settings` joins the protected
//!    admin router.)
//!
//!   live, with NO credential, against 127.0.0.1:8085:
//!    22 of those 24 are DELIBERATE   -> 12 [`PUBLIC_ROUTES`] + 10 [`INTERNAL_ROUTES`]
//!     2 of them were NOT             ->  `/api/v1/bridge-tasks`, `/api/v1/bridge-results`
//!
//!   (+1 from kanban t_39cea779: `/api/v1/auth/avatar/{user_id}`, the account-picture READ, joins
//!    [`PUBLIC_ROUTES`] — an `<img src>` carries no token — for 13 public + 10 internal = 23.)
//! ```
//!
//! # What this app's contribution is
//!
//! WorkflowSwift already had a fail-closed SHAPE — a request is public by being registered in the
//! `public_routes` sub-router and private by being registered in `protected_routes`, which carries
//! `auth::middleware::auth_middleware` — and an earlier lane (kanban t_73f6724d) deleted the dead
//! auth-skip list that used to sit inside that middleware. There is therefore no
//! `path.starts_with(..)` bypass anywhere in this app to close, and the census found **one**
//! accidental-anonymous class rather than a handful:
//!
//! 1. **Two anonymous file readers.** `GET /api/v1/bridge-tasks` and `GET /api/v1/bridge-results`
//!    were registered on the anonymous router and their handlers take only `State` — no extractor,
//!    no key check — then read `/opt/ai-bridge/{inbound,outbound}/*.json` off the host filesystem
//!    and return the parsed contents. They are NOT deliberate public surfaces: every caller sends a
//!    credential. The only console panel that reads them (`www-admin/index.html::renderBridge`) goes
//!    through the panel's `api()` helper, which attaches `Authorization: Bearer <jwt>`; and the
//!    fleet's own admin sweep (`/opt/swift/bin/ws-admin-api-sweep.py`, which mints a super-admin
//!    token) reads `/api/v1/bridge-tasks` the same way. They are now mounted on the PROTECTED router
//!    — same paths, so both callers are unaffected (proved live: 200 with a token before and after,
//!    401 anonymous after).
//! 2. **The default-allow shape that remains** is placement itself: with the old arrangement the
//!    only thing keeping a newly mounted route private was the author registering it in the right
//!    sub-router, and there was no committed list, no test, and no boundary. This module is that
//!    list; [`crate::auth::boundary::require_credential`] is that boundary. A route added to
//!    `public_routes` tomorrow without an entry here answers 401 anonymous instead of answering its
//!    handler.
//! 3. **A fail-OPEN machine receiver.** `POST /api/v1/incoming` checks `X-Internal-Key` only when
//!    the server HAS one configured (`if !state.config.internal_sync_key.is_empty()`), so an
//!    unconfigured (or blanked) key would make the receiver anonymous. It is named in
//!    [`INTERNAL_ROUTES`] and the key is now demanded at the boundary too, where an empty configured
//!    key can never match an empty header.
//!
//! # Credentials accepted
//!
//! * **App JWT** — `Authorization: Bearer <jwt>`, HS256 over `JWT_SECRET`, verified with the SAME
//!   `auth::middleware::verify_token` the protected router and the handlers use, so a token that
//!   passes here cannot fail their check.
//! * **Issued API key** — `Authorization: Bearer workflowswift_<...>`. The key is opaque and its
//!   validation is a database read (`auth::api_key_auth::authenticate`, against `api_keys`) that
//!   `auth_middleware` already performs, so the boundary requires only that the presented bearer has
//!   the app's key shape and leaves the decision where the credential lives.
//! * **`X-Internal-Key`** — the app's own `INTERNAL_SYNC_KEY`, for [`INTERNAL_ROUTES`] only,
//!   compared length-independently, and never accepted when the app has no key configured.
//!
//! The boundary never *widens* a caller's reach: it decides nothing about tenancy, roles, or which
//! account a request may touch. It can only refuse a caller that presents no credential at all.
//!
//! # Adding a route
//!
//! Leave it out of both lists and it is private. Add an entry only when the route must answer a
//! caller that presents no credential — and add the shape to the test module below, so the decision
//! and its reason are recorded with the code.

/// Routes that may be reached with NO credential at all.
///
/// Templates use axum's `{param}` spelling and match by segment (see [`matches_template`]), so
/// `/api/v1/instances/{id}/callback` accepts `/api/v1/instances/<uuid>/callback` but never
/// `/api/v1/instances/<uuid>/callback/extra`.
pub const PUBLIC_ROUTES: &[&str] = &[
    // --- liveness ------------------------------------------------------------------------------
    // Read by the fleet uptime watchdog, by nginx and by this app's deploy script (which fails the
    // deploy on anything but 200). Returns service name + crate version only, never tenant data.
    // (`/` is the same handler on the root router; it is outside the guarded prefix and so is not
    // listed here — see `is_guarded_path`.)
    "/api/v1/health",
    // --- account entry points ------------------------------------------------------------------
    // Login/register/recovery: a credential is CREATED here, so they are anonymous by definition.
    // All four are nested in `auth_public`, which carries the in-flight ceiling
    // (`rate_limit::password_auth_shed_middleware`) that bounds Argon2 work — the abuse control
    // lives on the same sub-router.
    "/api/v1/auth/login",
    "/api/v1/auth/register",
    "/api/v1/auth/forgot-password",
    "/api/v1/auth/reset-password",
    // --- public catalogues ---------------------------------------------------------------------
    // Read BEFORE a session exists: the signup wizard and the admin provider pickers populate
    // themselves from these (`industries`, `available-providers`, `provider-presets`). Each reads
    // platform-level vocabulary rows — `industries`, `available_providers`,
    // `integration_provider_presets` — and none of them takes an account or is scoped by one.
    "/api/v1/industries",
    "/api/v1/available-providers",
    "/api/v1/provider-presets",
    // --- public download -----------------------------------------------------------------------
    // The shipped Chrome extension's own zip. It is distributed to end users who have no account
    // yet; it contains no tenant data.
    "/api/v1/extension.zip",
    // --- the account picture READ --------------------------------------------------------------
    // An `<img src="/api/v1/auth/avatar/<uuid>">` carries no token, so this one route is public by
    // construction (card t_39cea779). It is narrow by design: one user's stored bytes, keyed by an
    // unguessable uuid, under the content type sniffed at upload time; 404 when there is no picture.
    // The authenticated POST twin (`/api/v1/auth/avatar`) is NOT here and must never be — the tests
    // below pin both directions.
    "/api/v1/auth/avatar/{user_id}",
    // --- bridge liveness ping ------------------------------------------------------------------
    // Returns the constant `{"status":"bridge-ok"}` and reads nothing. The two bridge LISTINGS that
    // used to sit beside it are NOT here: they return host filesystem contents and every caller
    // sends a credential, so they are private now (see the module docs, finding 1).
    "/api/v1/bridge-ping",
    // --- payment receivers whose own SIGNATURE is the credential ---------------------------------
    // Stripe and PayPal post here with no session; the receiver verifies the HMAC over the raw body
    // (`security::webhook_security` + the per-request freshness stamp) and answers 503 to a delivery
    // it cannot verify. The receiver reads the raw bytes before it checks anything, which is why
    // both are also mounted with the body-read deadline. This is a receiver surface, not a data
    // surface: an unverified delivery is refused, never answered with tenant data.
    "/api/v1/webhooks/stripe",
    "/api/v1/webhooks/paypal",
];

/// Service-to-service routes whose own shared key (`X-Internal-Key` = `INTERNAL_SYNC_KEY`) is the
/// credential.
///
/// Every handler in this list already checks that key itself — the boundary demands it as well, so
/// that a route added under one of these prefixes without an entry here is an ordinary PRIVATE route
/// rather than one whose safety depends on its author remembering.
///
/// The one receiver that also accepts a user SESSION is `/api/v1/instances/{id}/callback`: n8n
/// writes an execution result back with the shared key, but a caller that ran the instance from the
/// panel presents a JWT and the handler verifies the instance belongs to that account. The boundary
/// therefore accepts the shared key OR any credential the app recognises on this arm; the handler
/// still makes the real decision.
pub const INTERNAL_ROUTES: &[&str] = &[
    // The machine receivers. Six of these deserialize their `Json` body BEFORE the handler body
    // runs, i.e. an anonymous POST reaches handler code today — which is exactly why the key is
    // demanded at the boundary as well.
    "/api/v1/internal/dashboard-data-seed",
    "/api/v1/internal/portfolio-companies",
    "/api/v1/internal/portfolio-sync",
    "/api/v1/internal/tags/assign",
    "/api/v1/internal/tags/delete",
    // n8n's own failure report for a graph run it could not complete.
    "/api/v1/n8n/run-outcome",
    // A Notify step's email/SMS send, called by the generated graph.
    "/api/v1/notify/dispatch",
    // The inbound receiver. Its handler checks the key only when one is CONFIGURED (fail-open when
    // unset); the boundary closes that shape — `presents_internal_key` refuses an empty configured
    // key, so an unconfigured receiver is private rather than open.
    "/api/v1/incoming",
    // n8n's execution-result callback (shared key) or the panel (JWT scoped to the instance).
    "/api/v1/instances/{id}/callback",
    // FunnelSwift's tag → free-account receiver (design §3.1, kanban t_ede5f6ed). The handler
    // checks `x-internal-key` itself with the same fail-closed posture; naming it here is what
    // makes the boundary demand the key as well, so the route is never reachable on the session
    // arm alone.
    "/api/v1/internal/provision-free-account",
];

/// Is this path inside the API surface this boundary decides?
///
/// Only the `/api/v1` tree. The root `/` health probe and everything nginx serves from
/// `/opt/swift/nginx/www*/workflowswift/**` (the patient/user shells, `/robots.txt`, the static
/// assets) never reach a router here and carry no credential — they are outside the boundary by
/// construction, and a path that is not guarded is passed through untouched.
pub fn is_guarded_path(path: &str) -> bool {
    path == "/api/v1" || path.starts_with("/api/v1/")
}

/// May this path be reached with NO credential at all?
pub fn is_public_route(path: &str) -> bool {
    PUBLIC_ROUTES.iter().any(|t| matches_template(t, path))
}

/// Is this a service-to-service route, reached with the app's own shared key?
pub fn is_internal_route(path: &str) -> bool {
    INTERNAL_ROUTES.iter().any(|t| matches_template(t, path))
}

/// Does one template match one concrete path?
///
/// Segment-wise: the split lengths must agree and every template segment is either a `{param}` (any
/// one non-empty segment) or the identical literal. Deliberately stricter than a string prefix —
/// `/api/v1/tagsX` is a different route and must not be caught, and a template can never
/// accidentally swallow a longer path such as `/api/v1/internal/tags/assign/extra`.
fn matches_template(template: &str, path: &str) -> bool {
    let t: Vec<&str> = template.split('/').collect();
    let p: Vec<&str> = path.split('/').collect();
    if t.len() != p.len() {
        return false;
    }
    t.iter().zip(p.iter()).all(|(tseg, pseg)| {
        if tseg.starts_with('{') && tseg.ends_with('}') {
            !pseg.is_empty()
        } else {
            tseg == pseg
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{is_guarded_path, is_internal_route, is_public_route, matches_template};

    /// Every `.route(..)` this app mounts lives in this one file, so the census is a scan of it.
    const ROUTES_SRC: &str = include_str!("../routes.rs");

    /// Every `.route("<literal>"` path literal in the routing file, in source order.
    fn route_literals() -> Vec<&'static str> {
        let bytes = ROUTES_SRC.as_bytes();
        let mut out = Vec::new();
        let mut i = 0usize;
        while let Some(pos) = ROUTES_SRC[i..].find(".route(") {
            let mut j = i + pos + ".route(".len();
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                let start = j + 1;
                let mut k = start;
                while k < bytes.len() && bytes[k] != b'"' {
                    k += 1;
                }
                out.push(&ROUTES_SRC[start..k]);
            }
            i = j;
        }
        out
    }

    /// Is this keyed path backed by a route the router really mounts?
    ///
    /// The allowlist is written with the `/api/v1` prefix the boundary sees; the literals in
    /// `routes.rs` are relative to the sub-router that mounts them, and may themselves carry a nest
    /// prefix (`/internal/portfolio-sync`, `/webhooks/stripe`) or none at all (`/login`, `/health`).
    /// So an entry is backed when some mounted literal is a SUFFIX of it — `/api/v1/auth/login`
    /// resolves through `.nest("/auth", ..)` to the `/login` literal.
    fn mounted(entry: &str) -> Option<&'static str> {
        let relative = entry.strip_prefix("/api/v1")?;
        route_literals()
            .into_iter()
            .find(|lit| !lit.is_empty() && relative.ends_with(lit))
    }

    /// Every allowlist entry must name a route the router really mounts. A stale entry is a live
    /// auth widening waiting for a new route to be mounted at that path.
    #[test]
    fn every_allowlist_entry_names_a_mounted_route() {
        for entry in super::PUBLIC_ROUTES
            .iter()
            .chain(super::INTERNAL_ROUTES.iter())
        {
            assert!(
                mounted(entry).is_some(),
                "allowlist entry {} names a route src/routes.rs does not mount",
                entry
            );
            assert!(
                entry.starts_with("/api/v1/"),
                "{} is outside the guarded prefix",
                entry
            );
        }
        // the scan really sees the mounts (a silent zero would make the test vacuous)
        assert!(
            route_literals().len() > 150,
            "route literal scan found too few"
        );
    }

    #[test]
    fn neither_list_has_duplicates() {
        for list in [super::PUBLIC_ROUTES, super::INTERNAL_ROUTES] {
            let mut seen = std::collections::HashSet::new();
            for entry in list {
                assert!(seen.insert(*entry), "duplicate allowlist entry {}", entry);
            }
        }
    }

    #[test]
    fn the_two_lists_are_disjoint() {
        for entry in super::PUBLIC_ROUTES {
            assert!(
                !super::INTERNAL_ROUTES.contains(entry),
                "{} is in both lists — an anonymous entry wins by accident",
                entry
            );
        }
    }

    /// The two anonymous file readers this card found are private: an entry here would be exactly
    /// the regression the card exists to close.
    #[test]
    fn the_found_anonymous_file_readers_are_not_public() {
        for path in ["/api/v1/bridge-tasks", "/api/v1/bridge-results"] {
            assert!(!is_public_route(path), "{} must be private", path);
            assert!(!is_internal_route(path), "{} must be private", path);
            assert!(
                is_guarded_path(path),
                "{} must be decided by the boundary",
                path
            );
        }
        // ...and they are still mounted (the fix moved them, it did not delete them).
        assert!(mounted("/api/v1/bridge-tasks").is_some());
        assert!(mounted("/api/v1/bridge-results").is_some());
    }

    /// Tenant and operator surfaces must never be anonymous.
    #[test]
    fn tenant_and_operator_surfaces_are_not_public() {
        for path in [
            "/api/v1/auth/me",
            "/api/v1/auth/profile",
            "/api/v1/accounts",
            "/api/v1/users",
            "/api/v1/workflows",
            "/api/v1/instances",
            "/api/v1/templates",
            "/api/v1/dashboard/stats",
            "/api/v1/provider-keys",
            "/api/v1/api-keys",
            "/api/v1/credits/balance",
            "/api/v1/portfolio-companies",
            "/api/v1/leads",
            "/api/v1/tickets",
            "/api/v1/webhooks",
            "/api/v1/admin/accounts",
            "/api/v1/admin/plans",
            "/api/v1/admin/settings",
            "/api/v1/admin/users",
            "/api/v1/impersonate",
            "/api/v1/checkout/create",
        ] {
            assert!(!is_public_route(path), "{} must not be anonymous", path);
            assert!(!is_internal_route(path), "{} must not be a key route", path);
        }
    }

    /// ...while the deliberate participant surfaces stay public, so a future lane cannot "harden"
    /// the app by deleting a signup or a payment webhook.
    #[test]
    fn the_deliberate_public_surfaces_stay_public() {
        for path in [
            "/api/v1/health",
            "/api/v1/auth/login",
            "/api/v1/auth/register",
            "/api/v1/auth/forgot-password",
            "/api/v1/auth/reset-password",
            "/api/v1/industries",
            "/api/v1/available-providers",
            "/api/v1/provider-presets",
            "/api/v1/extension.zip",
            "/api/v1/bridge-ping",
            "/api/v1/webhooks/stripe",
            "/api/v1/webhooks/paypal",
            "/api/v1/auth/avatar/user-one",
        ] {
            assert!(is_public_route(path), "{} must stay anonymous", path);
        }
        // A POST is not made public by its path matching a GET-only entry in spirit: the lists are
        // paths, not method sets, and the boundary sees methods too — pinned in boundary.rs.
        assert!(is_public_route("/api/v1/webhooks/stripe"));
    }

    /// The account picture: the READ is public, the UPLOAD twin is NOT (card t_39cea779). An
    /// `<img src>` cannot carry a token, so the GET must be anonymous; but the POST writes bytes
    /// under the caller's own session and must never be. Both directions are pinned, and the
    /// template is segment-exact so a deeper path is not caught by it either way.
    #[test]
    fn the_avatar_read_is_public_and_the_upload_is_not() {
        assert!(is_public_route("/api/v1/auth/avatar/user-one"));
        assert!(
            !is_public_route("/api/v1/auth/avatar"),
            "the upload twin must never be anonymous"
        );
        assert!(
            !is_public_route("/api/v1/auth/avatar/x/extra"),
            "the template must not swallow a deeper path"
        );
        // ...and both routes are really mounted, so neither assertion is vacuous. (`mounted` compares
        // LITERALS, so the read is looked up with its `{user_id}` spelling, exactly as the allowlist
        // entry carries it.)
        assert!(mounted("/api/v1/auth/avatar").is_some());
        assert!(mounted("/api/v1/auth/avatar/{user_id}").is_some());
    }

    /// The fleet profile surface registers the fleet spelling of the password change beside this
    /// app's original one (card t_39cea779), so the contract path is really wired.
    #[test]
    fn the_fleet_password_path_is_mounted() {
        assert!(mounted("/api/v1/auth/password").is_some());
        assert!(mounted("/api/v1/auth/profile").is_some());
        assert!(mounted("/api/v1/auth/me").is_some());
    }

    #[test]
    fn internal_routes_are_named_and_not_public() {
        for path in [
            "/api/v1/internal/portfolio-sync",
            "/api/v1/internal/portfolio-companies",
            "/api/v1/internal/tags/assign",
            "/api/v1/internal/tags/delete",
            "/api/v1/internal/dashboard-data-seed",
            "/api/v1/n8n/run-outcome",
            "/api/v1/notify/dispatch",
            "/api/v1/incoming",
            "/api/v1/instances/instance-one/callback",
            "/api/v1/internal/provision-free-account",
        ] {
            assert!(is_internal_route(path), "{} is a named key route", path);
            assert!(!is_public_route(path), "{} must not be anonymous", path);
        }
        // The old arrangement had no `starts_with` bypass to close, and this pins that none is
        // introduced: an UNLISTED sibling under a wired prefix is an ordinary private route.
        for path in [
            "/api/v1/internal/whatever",
            "/api/v1/internal/tags/assignment",
            "/api/v1/n8n/whatever",
            "/api/v1/notify/whatever",
            "/api/v1/instances/instance-one/callback/extra",
        ] {
            assert!(!is_internal_route(path), "{} must be private", path);
            assert!(!is_public_route(path), "{} must be private", path);
        }
    }

    #[test]
    fn matching_is_segment_exact() {
        assert!(matches_template("/api/v1/tags", "/api/v1/tags"));
        assert!(!matches_template("/api/v1/tags", "/api/v1/tags/extra"));
        assert!(!matches_template("/api/v1/tags", "/api/v1/tagsX"));
        assert!(matches_template(
            "/api/v1/instances/{id}/callback",
            "/api/v1/instances/abc/callback"
        ));
        assert!(!matches_template(
            "/api/v1/instances/{id}/callback",
            "/api/v1/instances//callback"
        ));
        assert!(!matches_template(
            "/api/v1/instances/{id}/callback",
            "/api/v1/instances/abc/callback/1"
        ));
    }

    /// The served/non-API surface boundary: nothing outside `/api/v1` is decided here.
    #[test]
    fn served_surfaces_are_outside_the_guarded_prefix() {
        for path in [
            "/",
            "/robots.txt",
            "/extension.zip",
            "/admin/index.html",
            "/index.html",
        ] {
            assert!(!is_guarded_path(path), "{} must not be guarded", path);
        }
        assert!(is_guarded_path("/api/v1"));
        assert!(is_guarded_path("/api/v1/health"));
        assert!(!is_guarded_path("/api/v1x"));
        assert!(!is_guarded_path("/api/v2/health"));
    }

    /// The census shape the module docs claim, pinned so the numbers in the doc cannot drift away
    /// from the code without a test failing.
    #[test]
    fn the_census_shape_is_what_the_docs_say() {
        assert_eq!(super::PUBLIC_ROUTES.len(), 13, "PUBLIC_ROUTES size");
        assert_eq!(super::INTERNAL_ROUTES.len(), 10, "INTERNAL_ROUTES size");
        // The 22 deliberate anonymous placements the census found, minus the 2 moved to the
        // protected router, plus `/api/v1/auth/avatar/{user_id}` (the account-picture READ, card
        // t_39cea779) which was never anonymous before it existed.
        assert_eq!(
            super::PUBLIC_ROUTES.len() + super::INTERNAL_ROUTES.len(),
            23
        );
    }
}
