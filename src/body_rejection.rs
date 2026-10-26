//! A malformed request body answers the app's own JSON — never axum's deserializer sentence.
//!
//! Every `Json<T>` body on this app is read by axum's extractor, which refuses a bad body BEFORE
//! the handler runs. Its refusal is `text/plain`, so a console doing `await r.json()` throws instead
//! of showing a message, and the sentence names our structs and the JSON grammar. Measured live
//! 2026-10-26 with seven malformed bodies against `/api/v1/auth/register`, `/login` and
//! `/forgot-password` — every one answered `text/plain`, e.g.
//!
//! ```text
//! Failed to deserialize the JSON body into the target type: missing field `name` at line 1 column 2
//! Failed to parse the request body as JSON: EOF while parsing a value at line 1 column 0
//! ```
//!
//! This is the sibling of [`crate::path_rejection`]: the same argument (fix the class in one place,
//! outside every handler, so a route added later inherits it), the same shape of fix, but for bodies
//! instead of path parameters. [`body_rejection_is_app_json`] touches ONLY a `400/413/415/422` whose
//! body is `text/plain`; every `AppError` response is already `application/json` and passes through
//! byte for byte, as does every successful response.
//!
//! The status is kept where it means something (413 — the upload was not malformed, it was too big)
//! and collapsed onto 400 otherwise, so the API keeps one bad-request shape.

use axum::body::{to_bytes, Body};
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Statuses an extractor can produce for a body it cannot read.
const EXTRACTOR_STATUSES: [StatusCode; 4] = [
    StatusCode::BAD_REQUEST,
    StatusCode::UNSUPPORTED_MEDIA_TYPE,
    StatusCode::UNPROCESSABLE_ENTITY,
    StatusCode::PAYLOAD_TOO_LARGE,
];

/// A rejection body is one short sentence; this only bounds the buffer.
const MAX_REJECTION_BYTES: usize = 64 * 1024;

/// The framing axum puts around the part the caller needs. Dropped first so the offending field can
/// be named; an unrecognised sentence is left as it is.
const EXTRACTOR_PREFIXES: [&str; 2] = [
    "Failed to deserialize the JSON body into the target type: ",
    "Failed to parse the request body as JSON: ",
];

pub async fn body_rejection_is_app_json(request: Request, next: Next) -> Response {
    let response = next.run(request).await;

    if !EXTRACTOR_STATUSES.contains(&response.status()) {
        return response;
    }
    let is_plain_text = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with("text/plain"))
        .unwrap_or(false);
    if !is_plain_text {
        return response;
    }

    let status = response.status();
    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, MAX_REJECTION_BYTES).await {
        Ok(b) => b,
        // Cannot judge it, so do not pretend: keep the status, answer JSON.
        Err(_) => {
            return refusal(
                StatusCode::BAD_REQUEST,
                "The request body could not be read.".to_string(),
            )
        }
    };

    let raw = String::from_utf8_lossy(&bytes);
    let message = plain_english(strip_framing(raw.trim()), status);
    let status = if status == StatusCode::PAYLOAD_TOO_LARGE {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::BAD_REQUEST
    };

    // Keep the headers the inner layers set (CORS, security, trace); replace status and body.
    parts.status = status;
    let out = json!({ "error": true, "message": message, "code": status.as_u16() }).to_string();
    parts.headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    if let Ok(len) = header::HeaderValue::from_str(&out.len().to_string()) {
        parts.headers.insert(header::CONTENT_LENGTH, len);
    }
    Response::from_parts(parts, Body::from(out))
}

fn refusal(status: StatusCode, message: String) -> Response {
    (
        status,
        axum::Json(json!({ "error": true, "message": message, "code": status.as_u16() })),
    )
        .into_response()
}

fn strip_framing(raw: &str) -> &str {
    EXTRACTOR_PREFIXES
        .iter()
        .find_map(|prefix| raw.strip_prefix(prefix))
        .map(str::trim)
        .unwrap_or(raw)
}

/// Turn the extractor's words into a sentence a customer can act on: specific about WHICH field is
/// at fault, silent about struct names, byte offsets and JSON grammar.
fn plain_english(raw: &str, status: StatusCode) -> String {
    let lower = raw.to_ascii_lowercase();

    if status == StatusCode::PAYLOAD_TOO_LARGE || lower.contains("length limit") {
        return "That upload is too large. Choose a smaller file.".to_string();
    }
    if status == StatusCode::UNSUPPORTED_MEDIA_TYPE {
        return "Send this form as JSON with a Content-Type of application/json.".to_string();
    }

    if let Some(rest) = lower.split("missing field").nth(1) {
        let field = rest
            .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '_')
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .next()
            .unwrap_or("")
            .to_string();
        return match field.as_str() {
            "email" => "Enter your email address.".to_string(),
            "password" => "Enter your password.".to_string(),
            "new_password" => "Choose a new password.".to_string(),
            "name" => "Enter your name.".to_string(),
            "token" | "code" => "Paste the reset code from your email.".to_string(),
            "" => "A required field is missing from this form.".to_string(),
            other => format!("The form is missing {other}."),
        };
    }

    if lower.contains("invalid type")
        || lower.contains("invalid value")
        || lower.contains("invalid length")
    {
        let shape = lower.contains("expected struct")
            || lower.starts_with("invalid type: null")
            || lower.contains("invalid length");
        return match leading_field(raw) {
            Some(field) => format!("{field}: that value is not valid for this form."),
            None if shape => "Send the form's fields as a JSON object.".to_string(),
            None => "One of the values in this form is not valid.".to_string(),
        };
    }

    if lower.contains("eof while parsing")
        || lower.contains("expected value")
        || lower.contains("invalid unicode")
        || lower.contains("key must be a string")
        || lower.contains("control character")
        || lower.contains("expected `,`")
        || lower.contains("expected ident")
    {
        return "The request body was not valid JSON.".to_string();
    }

    if let Some(field) = leading_field(raw) {
        return format!("{field}: that value is not valid for this form.");
    }

    "The request body could not be read.".to_string()
}

/// The framework prefixes a field-scoped complaint with `field: `. Only take a token that looks like
/// a field name, so an unrecognised sentence never becomes `Failed to: that value is not…`.
fn leading_field(raw: &str) -> Option<String> {
    let (head, _) = raw.split_once(':')?;
    let head = head.trim();
    let looks_like_field = !head.is_empty()
        && head.len() <= 40
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    looks_like_field.then(|| head.replace('_', " "))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exact bodies axum 0.7 produced, measured live on this app 2026-10-26.
    const MISSING_NAME: &str =
        "Failed to deserialize the JSON body into the target type: missing field `name` at line 1 column 2";
    const WRONG_TYPE: &str = "Failed to deserialize the JSON body into the target type: email: invalid type: integer `123`, expected a string at line 1 column 45";
    const EMPTY: &str =
        "Failed to parse the request body as JSON: EOF while parsing a value at line 1 column 0";
    const NULL_BODY: &str = "Failed to deserialize the JSON body into the target type: invalid type: null, expected struct RegisterRequest";

    #[test]
    fn a_missing_field_names_the_field_in_plain_words() {
        let msg = plain_english(
            strip_framing(MISSING_NAME),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
        assert_eq!(msg, "Enter your name.");
    }

    #[test]
    fn a_wrong_type_names_the_field_but_not_the_grammar() {
        let msg = plain_english(strip_framing(WRONG_TYPE), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(msg, "email: that value is not valid for this form.");
        for leak in ["invalid type", "expected a string", "at line"] {
            assert!(!msg.contains(leak), "leaked {leak}: {msg}");
        }
    }

    #[test]
    fn unparsable_json_says_so() {
        assert_eq!(
            plain_english(strip_framing(EMPTY), StatusCode::BAD_REQUEST),
            "The request body was not valid JSON."
        );
        // JSON that is not an OBJECT names the fix, not the type.
        assert_eq!(
            plain_english(strip_framing(NULL_BODY), StatusCode::UNPROCESSABLE_ENTITY),
            "Send the form's fields as a JSON object."
        );
    }

    #[test]
    fn a_too_large_upload_keeps_its_own_sentence() {
        assert_eq!(
            plain_english("length limit exceeded", StatusCode::PAYLOAD_TOO_LARGE),
            "That upload is too large. Choose a smaller file."
        );
    }

    #[test]
    fn a_handlers_own_400_is_not_rewritten() {
        // `is_plain_text` is the gate; a JSON body never reaches `plain_english`.
        let res = (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":true,"message":"Invalid credentials","code":400}"#,
        )
            .into_response();
        assert!(res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.starts_with("application/json"))
            .unwrap_or(false));
    }
}
