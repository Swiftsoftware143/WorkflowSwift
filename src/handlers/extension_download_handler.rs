use axum::{
    body::Body,
    http::{header, StatusCode},
    response::Response,
};

// The zip is a tracked build artifact; since 2026-09-21 it lives in exactly one place
// (extensions/dist/, produced by extensions/build.sh). The repo-root copy this used to
// point at was deleted, which broke the build here — keep the path pinned to dist/.
static EXTENSION_ZIP_BYTES: &[u8] =
    include_bytes!("../../extensions/dist/swift-market-intel-extension.zip");

pub async fn download_extension() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"swift-market-intel-extension.zip\"",
        )
        .header(
            header::CONTENT_LENGTH,
            EXTENSION_ZIP_BYTES.len().to_string(),
        )
        .body(Body::from(EXTENSION_ZIP_BYTES))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("Failed to serve extension"))
                .unwrap()
        })
}
