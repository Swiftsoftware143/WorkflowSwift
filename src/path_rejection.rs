//! Path-parameter rejections answer the app's own JSON 404 — never axum's parser sentence.
//!
//! `Path<Uuid>` is used by dozens of handlers in this app. When a caller hands one a stale,
//! truncated or hand-typed id, axum's extractor fails before the handler runs and its rejection is
//! rendered as `400 text/plain`:
//!
//! ```text
//! Invalid URL: Cannot parse `id` with value `not-a-uuid`: UUID parsing failed: invalid character:
//! found `n` at 0
//! ```
//!
//! That sentence is the framework talking to a browser: it names no resource, it is not JSON, and a
//! console doing `await r.json()` on it throws. Measured live 2026-10-26 with a real session:
//! `GET /api/v1/tickets/{id}` and `GET /api/v1/workflows/{id}/steps` still answered that way, while
//! `/workflows/{id}` (fixed per handler earlier the same day), `/plans/{id}`, `/accounts/{id}`,
//! `/email-templates/{id}`, `/brand-monitor/{id}`, `/competitor-watch/{id}`,
//! `/industry-sources/{id}`, `/prospecting/{id}`, `/workflows/{id}/logs` and
//! `/accounts/{id}/plan` already answered a clean JSON 404.
//!
//! That is the argument against fixing this per handler: the earlier pass fixed the one route it
//! measured, and two sibling routes kept leaking. This layer fixes the class in one place — it sits
//! outside auth and outside every handler, so it sees every extractor rejection, including the
//! PUT/POST/DELETE arms of the same routes and any handler added later.
//!
//! It is deliberately narrow: it only touches a `400` whose body is `text/plain` AND begins with
//! axum's `Invalid URL` marker, and re-emits every other response byte for byte. A malformed id then
//! gets the same answer as an id with no row, using this app's own error envelope
//! (`{"error":true,"message":…,"code":404}`).

use axum::body::{to_bytes, Body};
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// The first bytes of every axum `Path`/`Query` rejection, and of nothing else this app emits.
const REJECTION_MARKER: &[u8] = b"Invalid URL";

/// A rejection body is one short sentence; this only bounds the buffer.
const MAX_REJECTION_BYTES: usize = 64 * 1024;

pub async fn path_rejection_is_not_found(request: Request, next: Next) -> Response {
    let response = next.run(request).await;

    // Every error this app writes through `AppError` is `application/json`. Axum's extractor
    // rejections are the only `text/plain` bodies on the API surface, so this check keeps the
    // common path cheap and leaves binary/streaming responses unread.
    let is_plain_text = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with("text/plain"))
        .unwrap_or(false);
    if response.status() != StatusCode::BAD_REQUEST || !is_plain_text {
        return response;
    }

    let (parts, body) = response.into_parts();
    let bytes = match to_bytes(body, MAX_REJECTION_BYTES).await {
        Ok(b) => b,
        // Cannot judge it, so do not pretend: a 400 we could not read stays a 400.
        Err(_) => return Response::from_parts(parts, Body::empty()),
    };

    match not_found_for(&bytes) {
        Some(replacement) => replacement,
        // Not the framework's sentence — put the response back exactly as it arrived.
        None => Response::from_parts(parts, Body::from(bytes)),
    }
}

/// The whole decision, with no I/O: does this body carry axum's parser sentence?
fn not_found_for(bytes: &[u8]) -> Option<Response> {
    if !bytes.starts_with(REJECTION_MARKER) {
        return None;
    }
    Some(
        (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({
                "error": true,
                "message": "Not found",
                "code": 404,
            })),
        )
            .into_response(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes as read_body;

    const LEAKED: &[u8] =
        b"Invalid URL: Cannot parse `id` with value `not-a-uuid`: UUID parsing failed";

    #[test]
    fn the_parser_sentence_becomes_a_json_404() {
        let out = not_found_for(LEAKED).expect("the parser sentence must be replaced");
        assert_eq!(out.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            out.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }

    #[test]
    fn an_application_400_is_not_touched() {
        assert!(
            not_found_for(br#"{"error":true,"message":"Name is required","code":400}"#).is_none()
        );
    }

    #[test]
    fn other_plain_text_is_not_touched() {
        assert!(not_found_for(b"some other plain text refusal").is_none());
    }

    #[tokio::test]
    async fn the_replacement_uses_this_apps_error_envelope() {
        let out = not_found_for(LEAKED).unwrap();
        let (_, body) = out.into_parts();
        let text = String::from_utf8(read_body(body, 4096).await.unwrap().to_vec()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("must be valid JSON");
        assert_eq!(parsed["code"], 404);
        assert_eq!(parsed["error"], true);
        assert!(!text.contains("Invalid URL"), "{text}");
    }
}
