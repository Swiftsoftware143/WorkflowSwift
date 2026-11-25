//! WorkflowSwift's own MCP door (kanban t_6364d995) — `POST /mcp` on the app host.
//!
//! The transport, the protocol identity, the 404-not-403 rule and the audit line are NOT written
//! here: they come from the shared crate `mcp-door`, pinned to one revision in `Cargo.toml` so a
//! door cannot change under this product between builds. What belongs to WorkflowSwift is the one
//! thing the crate cannot know:
//!
//! * **the credential** — this app's `api_keys` table, resolved by the app's own
//!   [`crate::auth::api_key_auth::authenticate`] on EVERY call, so a revoked key, an expired key, a
//!   deactivated owning user or a plan without `api_access` fails on the very next request;
//! * **the tenancy** — almost every table here keys `aid` (the account id), so the caller's `aid`
//!   from the key row is the only workspace a tool may read;
//! * **the tools** — the app's own features, in [`tools`], each a fixed statement with the tenant
//!   filter inside it.
//!
//! ## Why the request path is an adapter instead of `Door::router()`
//!
//! The crate is built against axum 0.7 and this app runs axum 0.8, so the two `Router` types cannot
//! be merged. `Door::dispatch` is the crate's public, socket-free request path; this module mounts a
//! one-route axum 0.8 handler that hands the headers and the raw body to it and copies the response
//! back out. Nothing about authentication, protocol negotiation, tool dispatch or auditing is
//! re-implemented in the adapter — it is HTTP glue only. (The crate's own `/healthz` is not mounted:
//! this app already answers `/` and `/api/v1/health`, and a second health surface would be a new
//! unmonitored door.)
//!
//! ## What this door never exposes
//!
//! `api_keys`, `user_api_keys`, `n8n_account_config`, `provider_keys`, `webhooks`,
//! `integration_targets.api_key`, `credit_transactions`, `account_erasures` — no tool reads them.
//! The WRITE tools (kanban t_bb4d4d33) are the five entities' create/update/archive, and the
//! parity tools (kanban t_8df5d174, [`parity`]) add the action and CRUD families that close the
//! "every UI action reachable by key" rule. Nothing can close or erase an account.

pub mod parity;
pub mod tools;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method};
use axum::routing::post;
use axum::{response::Response, Router};
use mcp_door::{AuthError, Caller, CredentialResolver, Door, Role};

use crate::auth::api_key_auth;
use crate::error::AppError;
use crate::AppState;

/// Where the app's door name appears on the wire and in every audit line.
pub const DOOR_NAME: &str = "workflowswift";

/// The tool registry this door serves. Ten reads plus fifteen writes (see [`tools`]).
type Registry = mcp_door::Registry;

/// Resolve `Authorization: Bearer <api key>` to the account the data is keyed by.
///
/// The work is [`api_key_auth::authenticate`], the SAME function the REST API's middleware calls:
/// discriminator lookup, dual-read hash verification (sha256 contract, legacy argon2 fallback),
/// `is_active`, `expires_at`, the owning user's `is_active`, and the account's `api_access` plan
/// flag. Re-implementing any of that here would be a second credential implementation that could
/// drift away from the one the API enforces.
///
/// The ROLE is derived from the app's own answer rather than assumed: a key whose `permissions`
/// allow a write scope is a workspace key, one restricted to reads is a read-only key. Both may use
/// this door — the read-only key sees only the ten reads, while the workspace key also sees the
/// fifteen write tools, which carry `min_role Workspace`.
pub struct KeyResolver {
    state: AppState,
}

impl KeyResolver {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl CredentialResolver for KeyResolver {
    async fn resolve(&self, bearer: &str) -> Result<Caller, AuthError> {
        match api_key_auth::authenticate(&self.state, bearer, &Method::POST).await {
            // The key's own row allows a write scope: the account's full credential.
            Ok(claims) => Ok(caller(&claims, Role::Workspace)),
            // A key restricted to reads answers 403 on the write-scope probe. That is a valid
            // credential with less reach, not a bad one.
            Err(AppError::Forbidden(_)) => {
                match api_key_auth::authenticate(&self.state, bearer, &Method::GET).await {
                    Ok(claims) => Ok(caller(&claims, Role::ReadOnly)),
                    Err(_) => Err(AuthError::Invalid),
                }
            }
            // The database could not answer: still a 401 to the caller, but the audit line says
            // why — an outage must not read as "this key was revoked".
            Err(AppError::Database(_)) => Err(AuthError::Unavailable("database".into())),
            Err(_) => Err(AuthError::Invalid),
        }
    }
}

/// The audit identity names the KEY (its console label) and the account it belongs to — never the
/// key's prefix or hash, which are part of the secret. The account stays in the subject so a write
/// stays attributable to the tenancy boundary even when the label is the shared default.
fn caller(claims: &crate::auth::models::Claims, role: Role) -> Caller {
    match uuid::Uuid::parse_str(&claims.aid) {
        Ok(aid) => {
            let label = claims
                .key_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("account");
            // `#<key row id>` is the stable actor id the card asks for: a label can be reused and
            // a key's VALUE can be rotated, but the row id identifies the credential throughout.
            let actor = match claims.key_id {
                Some(kid) => format!("workflowswift:{label}#{kid}@{aid}"),
                None => format!("workflowswift:{label}@{aid}"),
            };
            Caller::new(actor, role).with_tenant(aid)
        }
        // A claim this app cannot parse is not a caller. `Caller::validate` would also refuse an
        // empty subject, but failing here keeps the reason precise.
        Err(_) => Caller::new("", role),
    }
}

/// Build the door. The registry is validated by the crate at this point, so a half-annotated tool
/// fails at STARTUP (this function is called once, before the listener binds) rather than on a
/// customer's first call.
pub fn build_door(state: &AppState) -> Door {
    let registry: Registry = {
        let mut reg: Registry = tools::registry(state.db.clone());
        reg = parity::register(reg, state.clone());
        reg
    };
    Door::builder(
        DOOR_NAME,
        env!("CARGO_PKG_VERSION"),
        registry,
        Arc::new(KeyResolver::new(state.clone())),
    )
    .path("/mcp")
    .tool_timeout(Duration::from_secs(20))
    .audit(audit_path())
    .instructions(
        "WorkflowSwift's read+write MCP surface. The bearer token IS the workspace: every tool reads \
         or writes only the account that key belongs to and no tool takes a workspace argument. \
         Reads cover workflows and their steps, runs, pending triggers, templates, dashboards, \
         clients, leads, competitors, brand monitors, tickets, automations, integration \
         destinations and invoices. Writes are create/update/archive for workflows, templates, \
         dashboards, clients, tags, leads, competitors, brand monitors, tickets and automations, \
         plus the action families: run a workflow, settle a waiting run step, run an automation, \
         import a template, create a rendition, and prospect search/enrich. Archive is always soft \
         (is_active = false or archived_at = now()) and nothing here hard-deletes or closes an \
         account. Run results are returned as 2,000-character excerpts of untrusted data.",
    )
    .build()
    .unwrap_or_else(|e| panic!("WorkflowSwift MCP door refused to start: {e}"))
}

/// `MCP_AUDIT_PATH` lets a probe write beside the service instead of into it; production keeps the
/// one path the compose file bind-mounts, so the lines outlive a container recreate.
fn audit_path() -> String {
    std::env::var("MCP_AUDIT_PATH").unwrap_or_else(|_| "/app/data/mcp-audit.log".to_string())
}

/// The one route this module mounts: `POST /mcp`, answered by the shared crate.
pub fn router(state: &AppState) -> Router {
    let door = Arc::new(build_door(state));
    Router::new().route("/mcp", post(dispatch)).with_state(door)
}

/// HTTP glue: hand the crate the headers and the raw bytes, copy its response back.
///
/// `Door::dispatch` already resolved the credential, negotiated the protocol version, ran the tool
/// and wrote the audit line before it returned — this function only moves the response.
async fn dispatch(State(door): State<Arc<Door>>, headers: HeaderMap, body: Bytes) -> Response {
    let (parts, body_07) = door.dispatch(&headers, &body).await.into_parts();
    // axum 0.7's body type is not axum 0.8's, but both implement `http_body::Body`, so collecting
    // the bytes is the whole conversion. A door response is always small JSON.
    let bytes = match http_body_util::BodyExt::collect(body_07).await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => Bytes::new(),
    };
    // `parts` is the `http` crate's own Parts (one `http` version in this tree), so the status and
    // the headers — including `WWW-Authenticate` on a 401 — carry over untouched.
    Response::from_parts(parts, axum::body::Body::from(bytes))
}
