//! Fleet-standard mint surface (P1c, card t_29efde4c) — `/api/personal-api-keys`.
//!
//! The fleet's canonical path for an account's OWN programmatic keys. Five routes, all PRIVATE
//! (anonymous -> 401), each a thin wrapper over the app's existing handler — NOT a second
//! credential implementation: hashing, sealing, the multi-key cap, audit and tenancy all stay in
//! `handlers::api_key_handler`, which is the ONE writer for `api_keys` rows in this app.
//!
//! Route table (paths are relative to the `/api/personal-api-keys` nest):
//!
//! | Method | Path          | Handler (delegated)   |
//! |--------|---------------|-----------------------|
//! | GET    | `/`           | `list_api_keys`       |
//! | POST   | `/`           | `create_api_key`      |
//! | DELETE | `/{id}`       | `delete_api_key`      |
//! | POST   | `/{id}/rotate`| `regenerate_api_key`  |
//! | POST   | `/{id}/reveal`| `reveal_api_key`      |
//!
//! Why this module exists at all: the protected router is nested at `/api/v1`, and the canonical
//! path sits OUTSIDE that prefix. It is mounted on the ROOT router in `routes::create_router` and
//! carries the SAME `auth::middleware::auth_middleware` layer the protected router carries — without
//! it the handlers have no `Claims` extension and every call answers 500.

use axum::{
    routing::{delete, get, post},
    Router,
};

use crate::handlers::api_key_handler;
use crate::AppState;

/// The canonical sub-router. State comes from the caller (`routes::create_router`), which also
/// applies the auth middleware layer before nesting it at `/api/personal-api-keys`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/",
            get(api_key_handler::list_api_keys).post(api_key_handler::create_api_key),
        )
        .route("/{id}", delete(api_key_handler::delete_api_key))
        .route("/{id}/rotate", post(api_key_handler::regenerate_api_key))
        .route("/{id}/reveal", post(api_key_handler::reveal_api_key))
}
