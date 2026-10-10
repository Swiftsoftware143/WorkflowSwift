//! `branding_handler` — the logo + settings half of per-account email branding (kanban t_c3cfe7ba,
//! ported from missedcallrespondr / ADASwift c33fcb8 / FunnelSwift t_c06a32eb).
//!
//! Routes, and they are deliberately asymmetric:
//!
//! * `GET  /api/v1/settings/branding` — authenticated, returns the caller's OWN branding document.
//! * `PUT  /api/v1/settings/branding` — authenticated, writes `brand_name` / `brand_color`, and
//!   PRESERVES the stored `logo_url` when the caller omits it.
//! * `POST /api/v1/settings/branding/logo` — authenticated, the image as EITHER `multipart/form-data`
//!   (the fleet idiom, first non-empty part) OR the RAW body (this app's historical avatar idiom),
//!   the caller's OWN account.
//! * `DELETE /api/v1/settings/branding/logo` — same, removes the logo.
//! * `GET  /api/v1/branding/logo/:account_id` — PUBLIC by design (`auth::route_policy::PUBLIC_ROUTES`):
//!   a mail client renders `<img src>` with no credential of any kind, so a logo that needed a token
//!   would simply never appear. The route can only ever return the image one account uploaded, keyed
//!   by an unguessable uuid, with the content type sniffed from the bytes at upload time; an account
//!   with no logo answers 404.
//!
//! The image sniff/cap is NOT re-implemented here — this app's ONE image recogniser
//! ([`crate::auth::handlers::sniff_image`]) is shared, exactly as its avatar upload uses it.
//!
//! `logo_url` is written HERE and nowhere else. `put_branding` preserves the stored value when the
//! account saves name/colour, so a panel echo cannot un-reference a logo.

use axum::{
    body::{Body, Bytes},
    extract::{Extension, FromRequest, Multipart, Path, Request, State},
    http::{header, HeaderMap},
    response::Response,
    Json,
};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::handlers::{sniff_image, MAX_AVATAR_BYTES};
use crate::auth::models::Claims;
use crate::branding;
use crate::error::{ApiResult, AppError};
use crate::state::AppState;

/// The URL the console and the mail both point at. Version-stamped because the bytes behind it
/// change while the path stays the same, and both the browser and any cache key on the URL.
fn logo_url_for(account_id: Uuid) -> String {
    format!(
        "/api/v1/branding/logo/{account_id}?v={}",
        chrono::Utc::now().timestamp()
    )
}

/// The caller's account id.
fn account_id(claims: &Claims) -> ApiResult<Uuid> {
    Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)
}

/// Read the stored branding document for this account, defaulting to `{}`.
async fn read_doc(state: &AppState, account_id: Uuid) -> Value {
    sqlx::query_scalar::<_, Option<Value>>("SELECT (settings -> $2) FROM accounts WHERE id = $1")
        .bind(account_id)
        .bind(branding::SETTINGS_KEY)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .flatten()
        .unwrap_or_else(|| json!({}))
}

/// Write the whole branding document for this account (jsonb merge into the settings column).
async fn write_doc(state: &AppState, account_id: Uuid, doc: &Value) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE accounts SET settings = \
         jsonb_set(COALESCE(settings, '{}'::jsonb), ARRAY[$2], $3::jsonb, true) WHERE id = $1",
    )
    .bind(account_id)
    .bind(branding::SETTINGS_KEY)
    .bind(doc)
    .execute(&state.db)
    .await?;
    Ok(())
}

/// The stored document as (brand_name, brand_color, logo_url), for a write that must not lose them.
fn fields(doc: &Value) -> (String, String, String) {
    let s = |k: &str| doc.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    (s("brand_name"), s("brand_color"), s("logo_url"))
}

/// `GET /api/v1/settings/branding` — the caller's own branding document.
pub async fn get_branding(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> ApiResult<Json<Value>> {
    Ok(Json(read_doc(&state, account_id(&claims)?).await))
}

/// `PUT /api/v1/settings/branding` — save name/colour, keeping a logo that is still stored.
///
/// An omitted `logo_url` is the normal case (the panel echoes name+colour only), so the stored value
/// is preserved; an explicit `""` clears it (alongside the DELETE route).
pub async fn put_branding(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(incoming): Json<Value>,
) -> ApiResult<Json<Value>> {
    let aid = account_id(&claims)?;
    branding::validate_value(&incoming).map_err(AppError::BadRequest)?;

    let (stored_name, stored_color, stored_logo) = fields(&read_doc(&state, aid).await);
    let name = incoming
        .get("brand_name")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or(stored_name);
    let color = incoming
        .get("brand_color")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or(stored_color);
    // preserve the stored logo when the key is absent; honour an explicit value (including "")
    let logo = match incoming.get("logo_url") {
        Some(v) => v.as_str().unwrap_or("").to_string(),
        None => stored_logo,
    };

    let doc = branding::document(&name, &color, &logo);
    write_doc(&state, aid, &doc).await?;
    Ok(Json(doc))
}

/// `POST /api/v1/settings/branding/logo` — store the caller's account logo and return its URL.
///
/// TWO body shapes, ONE handler (fleet-normalised 2026-10-10). The other three apps take the logo as
/// `multipart/form-data`; this app historically took it as the RAW body. A client that used the
/// fleet's form idiom got a 400 "Unsupported logo" here, so the handler now reads BOTH:
/// `multipart/form-data` takes the first non-empty file part, anything else is the body verbatim.
/// The format is decided by the bytes in either arm (never a caller-declared content type), a
/// non-image and an empty body are refused 400, and a body over [`MAX_AVATAR_BYTES`] is refused 400
/// as well. One row per account (upsert), so re-uploading replaces the logo rather than accumulating
/// rows.
pub async fn upload_logo(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
) -> ApiResult<Json<Value>> {
    let aid = account_id(&claims)?;

    let content_type_header = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let body: Vec<u8> = if content_type_header.starts_with("multipart/form-data") {
        let mut multipart = Multipart::from_request(request, &state)
            .await
            .map_err(|e| AppError::BadRequest(format!("Invalid multipart body: {e}")))?;
        let mut picked: Option<Bytes> = None;
        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| AppError::BadRequest(format!("Invalid multipart body: {e}")))?
        {
            let data = field
                .bytes()
                .await
                .map_err(|e| AppError::BadRequest(format!("Could not read the logo: {e}")))?;
            if !data.is_empty() {
                picked = Some(data);
                break;
            }
        }
        picked
            .ok_or_else(|| AppError::BadRequest("No logo data received".to_string()))?
            .to_vec()
    } else {
        axum::body::to_bytes(request.into_body(), MAX_AVATAR_BYTES + 1)
            .await
            .map_err(|e| AppError::BadRequest(format!("Could not read the logo: {e}")))?
            .to_vec()
    };
    let body = Bytes::from(body);

    if body.is_empty() {
        return Err(AppError::BadRequest("No logo data received".to_string()));
    }
    if body.len() > MAX_AVATAR_BYTES {
        return Err(AppError::BadRequest(
            "Logo must be 2 MB or smaller".to_string(),
        ));
    }
    let content_type = sniff_image(&body).ok_or_else(|| {
        AppError::BadRequest("Unsupported logo — use a PNG, JPEG, GIF or WebP image".to_string())
    })?;

    sqlx::query(
        "INSERT INTO account_logos (account_id, content_type, bytes, updated_at) \
         VALUES ($1, $2, $3, NOW()) \
         ON CONFLICT (account_id) DO UPDATE SET content_type = EXCLUDED.content_type, \
         bytes = EXCLUDED.bytes, updated_at = NOW()",
    )
    .bind(aid)
    .bind(content_type)
    .bind(body.as_ref())
    .execute(&state.db)
    .await?;

    // Write ONLY the logo_url of the branding document, so name/colour survive an upload.
    let (name, color, _) = fields(&read_doc(&state, aid).await);
    let logo_url = logo_url_for(aid);
    let doc = branding::document(&name, &color, &logo_url);
    write_doc(&state, aid, &doc).await?;

    Ok(Json(json!({
        "status": "ok",
        "logo_url": logo_url,
        "content_type": content_type,
        "branding": doc,
    })))
}

/// `DELETE /api/v1/settings/branding/logo` — remove the logo and un-reference it.
pub async fn delete_logo(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> ApiResult<Json<Value>> {
    let aid = account_id(&claims)?;
    let removed = sqlx::query("DELETE FROM account_logos WHERE account_id = $1")
        .bind(aid)
        .execute(&state.db)
        .await?
        .rows_affected();

    // Keep name/colour; clear only the reference.
    let (name, color, _) = fields(&read_doc(&state, aid).await);
    write_doc(&state, aid, &branding::document(&name, &color, "")).await?;

    Ok(Json(json!({"status": "ok", "removed": removed})))
}

/// `GET /api/v1/branding/logo/:account_id` — stream an account's logo. No credential (see the module
/// docs); ids are unguessable uuids and no other tenant datum is reachable from here.
pub async fn get_logo(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> ApiResult<Response> {
    let row = sqlx::query("SELECT bytes, content_type FROM account_logos WHERE account_id = $1")
        .bind(account_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("No logo for this account".to_string()))?;

    let bytes: Vec<u8> = row.try_get("bytes")?;
    let content_type: String = row.try_get("content_type")?;

    let mut resp = Response::new(Body::from(bytes));
    let ct = header::HeaderValue::from_str(&content_type)
        .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream"));
    resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    // NOT cached: the URL carries a ?v= stamp, but a stale copy of a replaced logo must never be
    // served from an intermediate cache, so the endpoint itself is no-store.
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    // The stored type is pinned and the browser is told not to sniff, so a mislabelled upload can
    // never be served back as something executable.
    resp.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    Ok(resp)
}
