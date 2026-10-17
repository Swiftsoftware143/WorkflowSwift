//! `image_store` — the ONE accept-store-serve path for a customer-supplied image.
//!
//! This app had its own copy of this capability, twice: the profile picture and the email-branding
//! logo each hand-rolled a multipart read, a raw-body fallback, a size cap and a magic-byte sniff.
//! The other four apps in the fleet call one shared helper, and the copies had already drifted in a
//! way a user can see: a photo over the cap answered
//!
//! ```text
//! 400 {"message":"Could not read the picture: length limit exceeded"}
//! ```
//!
//! while FunnelSwift, IncentiveSwift, ADASwift and MissedCall answer the sentence the picker shows
//! ("That file is too large to upload. The maximum size is 2 MB - …") with **413**. A user who
//! picks a big phone photo should be told what to do, not handed the extractor's words.
//!
//! So the capability lives here and nowhere else:
//!
//! * [`read_request_image`] — read an image from EITHER a `multipart/form-data` form or a RAW body.
//! * [`sniff`] — recognise an image by its BYTES, never by the caller's `Content-Type`.
//! * [`image_response`] — serve stored bytes back with the sniffed type pinned and no shared cache.
//!
//! The bytes are stored in the database on both surfaces: this container runs an image-baked
//! binary, so a file written at run time would die with the next container recreate.

use axum::{
    body::{to_bytes, Body},
    extract::{FromRequest, Multipart, Request},
    http::{header, HeaderMap, StatusCode},
    response::Response,
};

use crate::error::{ApiResult, AppError};

/// The fleet's request-body ceiling for an upload: an image PLUS its multipart framing.
pub const ENVELOPE_BYTES: usize = 2 * 1024 * 1024;

/// Multipart framing reserve (boundary lines, part headers, a real filename). Kept out of the
/// envelope so a file the picker accepts is never refused by the envelope instead of by this module
/// — which is the difference between the friendly sentence and a bare extractor error.
pub const ENVELOPE_HEADROOM: usize = 64 * 1024;

/// The most image bytes an upload may carry, and the number both consoles check before uploading,
/// so the client and the server refuse the SAME set of files.
pub const MAX_IMAGE_BYTES: usize = ENVELOPE_BYTES - ENVELOPE_HEADROOM;

/// The one sentence every app in the fleet answers with when the file is too big. Identical wording
/// on purpose: the console's picker shows it verbatim, and an operator comparing apps should never
/// see two different explanations of the same refusal.
pub const OVERSIZE_MESSAGE: &str =
    "That file is too large to upload. The maximum size is 2 MB - please choose a smaller image and try again.";

/// True when an extractor's error is its own length limit rather than a broken body.
fn is_length_limit(text: &str) -> bool {
    text.contains("length limit exceeded") || text.contains("body too large")
}

/// Recognise an accepted image by its MAGIC BYTES. SVG is deliberately refused: it is a scriptable
/// document, not a bitmap, and nothing here would sanitise it.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Read an uploaded image from EITHER a `multipart/form-data` form or a RAW image body.
///
/// The shape is chosen by the `Content-Type` header; the IMAGE is recognised by its bytes in either
/// arm. Both arms share the same empty-body refusal, the same envelope and the same magic-byte
/// sniff, so a client cannot get a different answer by changing how it posts the same file.
pub async fn read_request_image<S>(
    state: &S,
    headers: &HeaderMap,
    request: Request,
) -> ApiResult<(String, Vec<u8>)>
where
    S: Send + Sync,
{
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if content_type.starts_with("multipart/form-data") {
        let mut multipart = Multipart::from_request(request, state).await.map_err(|e| {
            if is_length_limit(&e.to_string()) {
                AppError::PayloadTooLarge(OVERSIZE_MESSAGE.to_string())
            } else {
                AppError::BadRequest(format!("Invalid multipart body: {e}"))
            }
        })?;
        return read_uploaded_image(&mut multipart).await;
    }

    let data = to_bytes(request.into_body(), ENVELOPE_BYTES)
        .await
        .map_err(|e| {
            if is_length_limit(&e.to_string()) {
                AppError::PayloadTooLarge(OVERSIZE_MESSAGE.to_string())
            } else {
                AppError::BadRequest(format!("Could not read the picture: {e}"))
            }
        })?;
    if data.is_empty() {
        return Err(AppError::BadRequest("No image was uploaded".into()));
    }
    if data.len() > MAX_IMAGE_BYTES {
        return Err(AppError::PayloadTooLarge(OVERSIZE_MESSAGE.to_string()));
    }
    let kind = sniff(&data)
        .ok_or_else(|| AppError::BadRequest("Upload a PNG, JPEG, GIF or WebP image".into()))?;
    Ok((kind.to_string(), data.to_vec()))
}

/// Read the first multipart part that carries a filename, cap it, and sniff it.
///
/// A part with no filename is SKIPPED, not refused, so a form carrying extra text parts still
/// works — and a text field can never be mistaken for the picture.
pub async fn read_uploaded_image(multipart: &mut Multipart) -> ApiResult<(String, Vec<u8>)> {
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        if is_length_limit(&e.to_string()) {
            AppError::PayloadTooLarge(OVERSIZE_MESSAGE.to_string())
        } else {
            AppError::BadRequest(format!("Multipart error: {e}"))
        }
    })? {
        if field.file_name().is_none() {
            continue;
        }
        let data = field.bytes().await.map_err(|e| {
            if is_length_limit(&e.to_string()) {
                AppError::PayloadTooLarge(OVERSIZE_MESSAGE.to_string())
            } else {
                AppError::BadRequest(format!("Could not read the picture: {e}"))
            }
        })?;
        if data.is_empty() {
            return Err(AppError::BadRequest("The image file is empty".into()));
        }
        if data.len() > MAX_IMAGE_BYTES {
            return Err(AppError::PayloadTooLarge(OVERSIZE_MESSAGE.to_string()));
        }
        let kind = sniff(&data)
            .ok_or_else(|| AppError::BadRequest("Upload a PNG, JPEG, GIF or WebP image".into()))?;
        return Ok((kind.to_string(), data.to_vec()));
    }
    Err(AppError::BadRequest("No image was uploaded".into()))
}

/// Serve stored image bytes back: the type sniffed at upload time, pinned with `nosniff`, and never
/// invited into a shared cache (these URLs are stable per account, so any shared max-age would keep
/// serving the PREVIOUS picture after a re-upload).
pub fn image_response(content_type: String, bytes: Vec<u8>) -> Response {
    let ct = header::HeaderValue::from_str(&content_type)
        .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream"));
    let mut resp = Response::new(Body::from(bytes));
    resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    resp.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    resp
}

/// Status the refusal carries. Kept beside the message so a handler can assert both.
pub const OVERSIZE_STATUS: StatusCode = StatusCode::PAYLOAD_TOO_LARGE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_recognises_the_four_allowed_formats_by_magic_bytes() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
        assert_eq!(sniff(&png), Some("image/png"));
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(sniff(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff(b"GIF87a...."), Some("image/gif"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff(&webp), Some("image/webp"));
    }

    #[test]
    fn sniff_refuses_scriptable_and_unknown_payloads() {
        assert_eq!(
            sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"),
            None
        );
        assert_eq!(sniff(b"<html><script>alert(1)</script></html>"), None);
        assert_eq!(sniff(b"RIFF1234WAVE"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn the_oversize_sentence_is_the_fleet_sentence() {
        assert!(OVERSIZE_MESSAGE.starts_with("That file is too large to upload."));
        assert!(OVERSIZE_MESSAGE.contains("2 MB"));
        assert_eq!(OVERSIZE_STATUS.as_u16(), 413);
    }
}
