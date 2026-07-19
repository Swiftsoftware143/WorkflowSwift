use axum::{
    response::{IntoResponse, Response},
    body::Body,
    http::{StatusCode, header},
};

static EXTENSION_ZIP_BYTES: &[u8] = include_bytes!("../../swift-market-intel-extension.zip");

pub async fn download_extension() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_DISPOSITION, "attachment; filename=\"swift-market-intel-extension.zip\"")
        .header(header::CONTENT_LENGTH, EXTENSION_ZIP_BYTES.len().to_string())
        .body(Body::from(EXTENSION_ZIP_BYTES))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("Failed to serve extension"))
                .unwrap()
        })
}
