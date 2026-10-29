//! Prospecting — a REAL business search, or an honest refusal.
//!
//! What this replaced (measured 2026-10-29): both endpoints fabricated their answers. A search for
//! "dentist Miami FL" returned businesses literally named `dentist Miami FL Business #1..#20` with
//! invented websites, and `enrich` invented a `contact_email` (`contact@<name>.com`), a
//! `contact_phone` (`+1-555-####`) and three "insights" out of thin air. The response carried
//! `sample_data: true` and the console painted a small caveat under the results, but the rows were
//! still clickable, still looked like companies, and still carried an address a customer could
//! mail — a reputation hazard dressed up as a demo, and a feature that could never become real.
//!
//! The rule now: this app never invents a company, a website, an email or a phone number. A search
//! either uses the credential the ACCOUNT has connected and returns what the provider really said,
//! or it REFUSES with the one instruction that fixes it — 424 Failed Dependency, because the action
//! depended on another action (connecting the provider) that has not happened. 424 rather than 502
//! on purpose: the CDN in front of this app rewrites a 502 body into its own page, which would hide
//! the sentence the operator needs.
//!
//! WHERE THE CREDENTIAL LIVES (and why not the process environment): the fleet gate refuses a
//! server-wide third-party key read via `std::env::var` — one key on the box would serve every
//! tenant and bill whoever owns it. Prospecting therefore reads the account's OWN key from
//! `provider_keys` (provider `google_places`, the catalogue's data provider, stored `enc:v1:`
//! ciphertext, decrypted here through `crate::security::provider_key_crypto`), which the customer
//! connects on the console's Provider Keys screen. `base_url` on that row, when set, overrides the
//! endpoint — the same seam ADASwift's discovery uses, and what makes the path provable against a
//! local stub with no vendor credential.

use axum::{
    extract::{Json, State},
    response::IntoResponse,
    Extension,
};
use serde_json::{json, Value};
use sqlx::Row;
use std::time::Duration;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::models::prospecting::*;
use crate::security::provider_key_crypto as key_crypto;
use crate::AppState;

/// The provider whose per-account key powers searching. It is the catalogue's data provider
/// (`available_providers.key = 'google_places'`), so the console can offer it on Provider Keys.
const PROVIDER: &str = "google_places";
const DEFAULT_PLACES_URL: &str = "https://places.googleapis.com/v1/places:searchText";
/// The fields we ask for. Every field below is echoed to the customer verbatim or dropped — none of
/// them is inferred. Phone is requested because Google publishes it; headcount is not requested
/// because no source here publishes it.
const FIELD_MASK: &str =
    "places.displayName,places.websiteUri,places.formattedAddress,places.primaryTypeDisplayName,places.nationalPhoneNumber";
const PROVIDER_TIMEOUT_SECS: u64 = 20;

/// Shown when the account has not connected the provider. It names the cause and the fix, and says
/// plainly that nothing was made up — the previous behaviour was to invent twenty companies.
const SEARCH_NOT_CONNECTED: &str = "Prospecting is not connected on this account yet: no business-search provider key is installed, so a search cannot return real companies. Nothing was invented for you. Connect the Google Places key under Provider Keys on this account, then run the search again.";

/// The same, for the per-result Enrich action — an unconfigured account used to receive a
/// plausible-looking contact email instead.
const ENRICH_NOT_CONNECTED: &str = "Enrichment is not connected on this account yet: no business-search provider key is installed, so there is no real contact data to add. Nothing was invented for you. Connect the Google Places key under Provider Keys on this account, then try Enrich again.";

struct PlacesCredential {
    api_key: String,
    url: String,
}

/// The account's own provider credential, decrypted, or `None` when it has not connected one.
/// A row whose `is_active` is false, or whose ciphertext will not open (a rotated master key), is
/// treated as "not connected" rather than as a 500: the customer's next move is the same either way.
async fn credential(state: &AppState, aid: Uuid) -> ApiResult<Option<PlacesCredential>> {
    let row = sqlx::query(
        r#"SELECT api_key, base_url FROM provider_keys
           WHERE aid = $1 AND provider = $2 AND is_active = true
           ORDER BY updated_at DESC LIMIT 1"#,
    )
    .bind(aid)
    .bind(PROVIDER)
    .fetch_optional(&state.db)
    .await?;

    let Some(row) = row else { return Ok(None) };

    let stored: String = row.try_get("api_key").unwrap_or_default();
    if stored.trim().is_empty() {
        return Ok(None);
    }

    let plain = match key_crypto::decrypt_from_storage(&state.db, &stored).await {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(error = %e, "prospecting: provider key could not be decrypted; treating as not connected");
            return Ok(None);
        }
    };
    if plain.trim().is_empty() {
        return Ok(None);
    }

    let base: Option<String> = row.try_get("base_url").ok().flatten();
    let url = base
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_PLACES_URL.to_string());

    Ok(Some(PlacesCredential {
        api_key: plain.trim().to_string(),
        url,
    }))
}

fn text_of(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn nested_text(v: &Value, outer: &str, inner: &str) -> Option<String> {
    v.get(outer)
        .and_then(|o| o.get(inner))
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn place_name(p: &Value) -> Option<String> {
    nested_text(p, "displayName", "text").or_else(|| text_of(p, "name"))
}

/// Turn the provider body into our result rows. A place with no name is dropped rather than shown
/// as an untitled company — the same rule ADASwift's discovery applies to a hit with no domain.
fn parse_places(body: &Value) -> Vec<BusinessSearchResult> {
    let Some(arr) = body.get("places").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|p| {
            let name = place_name(p)?;
            Some(BusinessSearchResult {
                name,
                website: text_of(p, "websiteUri"),
                industry: nested_text(p, "primaryTypeDisplayName", "text"),
                location: text_of(p, "formattedAddress"),
                description: text_of(p, "nationalPhoneNumber"),
            })
        })
        .collect()
}

/// One provider round-trip. Returns the parsed rows or a refusal carrying the provider's own words —
/// never an empty success, which a caller cannot tell apart from "no such businesses".
async fn search_places(
    cred: &PlacesCredential,
    query: &str,
    limit: i64,
) -> ApiResult<Vec<BusinessSearchResult>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(PROVIDER_TIMEOUT_SECS))
        .build()
        .map_err(|e| AppError::Internal(format!("search client: {e}")))?;

    let resp = client
        .post(&cred.url)
        .header("X-Goog-Api-Key", cred.api_key.clone())
        .header("X-Goog-FieldMask", FIELD_MASK)
        .json(&json!({
            "textQuery": query,
            "maxResultCount": limit.clamp(1, 20),
        }))
        .send()
        .await
        .map_err(|e| {
            AppError::UpstreamRefused(format!(
                "The business-search provider could not be reached ({e}). No results were invented."
            ))
        })?;

    let status = resp.status();
    let raw = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        let shown = raw.chars().take(300).collect::<String>();
        return Err(AppError::UpstreamRefused(format!(
            "The business-search provider refused the search ({}): {}",
            status.as_u16(),
            shown.trim()
        )));
    }

    let body: Value = serde_json::from_str(&raw).map_err(|e| {
        AppError::UpstreamRefused(format!(
            "The business-search provider answered with something that is not JSON ({e})."
        ))
    })?;

    Ok(parse_places(&body))
}

pub async fn search_businesses(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let query = req
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    if query.is_empty() {
        return Err(AppError::Validation("query is required".to_string()));
    }

    let limit = req
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .clamp(1, 20);

    let cred = credential(&state, aid)
        .await?
        .ok_or_else(|| AppError::UpstreamRefused(SEARCH_NOT_CONNECTED.to_string()))?;

    let results = search_places(&cred, &query, limit).await?;

    Ok(Json(json!({
        "results": results,
        "total": results.len(),
        "query": query,
        "provider": PROVIDER,
        // Kept so an older console cannot mistake a real answer for the retired demo: this is a
        // provider answer or a refusal, never sample data.
        "sample_data": false,
    })))
}

pub async fn enrich_business(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let business_name = req
        .get("business_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    if business_name.is_empty() {
        return Err(AppError::Validation(
            "business_name is required".to_string(),
        ));
    }

    let website = req
        .get("website")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let cred = credential(&state, aid)
        .await?
        .ok_or_else(|| AppError::UpstreamRefused(ENRICH_NOT_CONNECTED.to_string()))?;

    // Enrichment is the provider's answer for THIS business, not a new invention: we ask for the
    // business by name and take the first real hit. Every field below comes from that hit, so an
    // empty field means the provider did not know it — never that we filled it in.
    let hits = search_places(&cred, &business_name, 5).await?;
    let wanted = business_name.to_lowercase();
    let hit = hits
        .into_iter()
        .find(|h| h.name.to_lowercase().contains(&wanted));

    let hit = match hit {
        Some(h) => h,
        None => {
            return Err(AppError::NotFound(format!(
                "The connected search provider has no match for '{business_name}', so there is nothing real to add."
            )))
        }
    };

    let enriched = EnrichedBusiness {
        business_name: hit.name,
        website: hit.website.or(website),
        industry: hit.industry,
        location: hit.location,
        // The provider does not publish headcount, a contact email or a social profile for a place,
        // and this app will NOT guess them.
        estimated_size: None,
        social_links: Vec::new(),
        contact_email: None,
        // A phone number IS something the provider publishes; `description` carries it.
        contact_phone: hit.description,
        insights: Vec::new(),
    };

    Ok(Json(json!({
        "enriched_business": enriched,
        "provider": PROVIDER,
        "sample_data": false,
    })))
}
