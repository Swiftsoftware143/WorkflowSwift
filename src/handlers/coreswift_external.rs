//! CoreSwift external push — the ONE CoreSwift code path for WorkflowSwift.
//!
//! Direction is INBOUND (spoke -> hub): WorkflowSwift is capture software (workflows
//! capture and qualify leads); CoreSwift is the hub and the single home for ALL leads.
//! Every captured lead is delivered to the hub's `POST /api/external/contacts` using the
//! account's own BYOK personal key (stored in `provider_keys`, provider = "coreswift").
//!
//! This replaces the legacy `handlers/coreswift_push.rs` anti-pattern (hardcoded
//! SwiftSoftware tenant UUID + a global `X-Internal-Key` read from env) which the
//! Integration Center standard rejects: keys are per-tenant, never env-only, never a
//! single admin-pasted key for everybody.
//!
//! Resolution is NOT re-implemented here: the base URL cascade (provider_keys.base_url ->
//! integration_provider_presets.base_url -> constant) and the API key lookup are the
//! existing `integration_center_handler` resolvers, so there is exactly one way to find
//! a CoreSwift connection in this crate.

use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::handlers::integration_center_handler::{get_provider_api_key, get_provider_base_url};
use crate::state::AppState;

/// Canonical capture-app slug used for hub attribution ("leads by capture app").
pub const SOURCE_APP: &str = "workflowswift";

/// Fleet default when neither the tenant override nor the catalogue preset carries a URL.
pub const DEFAULT_CORESWIFT_URL: &str = "https://coreswiftcrm.com";

/// A lead captured by WorkflowSwift, normalised for the hub's contact payload.
#[derive(Debug, Clone, Default)]
pub struct CapturedLead {
    pub full_name: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub company: Option<String>,
    pub source: Option<String>,
    pub notes: Option<String>,
}

impl CapturedLead {
    /// Build from a single "name" field (the `leads` table keeps one name column).
    pub fn from_name(
        name: &str,
        email: Option<String>,
        phone: Option<String>,
        company: Option<String>,
        source: Option<String>,
    ) -> Self {
        let trimmed = name.trim();
        let (first, last) = match trimmed.split_once(' ') {
            Some((f, l)) => (f.to_string(), l.trim().to_string()),
            None => (trimmed.to_string(), String::new()),
        };
        Self {
            full_name: Some(trimmed.to_string()),
            first_name: Some(first),
            last_name: if last.is_empty() { None } else { Some(last) },
            email: email.filter(|s| !s.trim().is_empty()),
            phone: phone.filter(|s| !s.trim().is_empty()),
            company: company.filter(|s| !s.trim().is_empty()),
            source: source.filter(|s| !s.trim().is_empty()),
            notes: None,
        }
    }

    /// Build from a decomposed contact (incoming HTTP ingest payload).
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        first_name: Option<String>,
        last_name: Option<String>,
        email: Option<String>,
        phone: Option<String>,
        company: Option<String>,
        source: Option<String>,
    ) -> Self {
        let clean = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
        let first_name = clean(first_name);
        let last_name = clean(last_name);
        let full_name = match (&first_name, &last_name) {
            (Some(f), Some(l)) => Some(format!("{} {}", f, l)),
            (Some(f), None) => Some(f.clone()),
            (None, Some(l)) => Some(l.clone()),
            (None, None) => None,
        };
        Self {
            full_name,
            first_name,
            last_name,
            email: clean(email),
            phone: clean(phone),
            company: clean(company),
            source: clean(source),
            notes: None,
        }
    }

    /// True when there is something worth delivering to the hub.
    pub fn is_pushable(&self) -> bool {
        self.email.is_some() || self.phone.is_some() || self.full_name.is_some()
    }
}

/// Resolve the account's CoreSwift connection as (api_key, base_url).
///
/// Returns `None` when the account has no active `coreswift` key -> the caller degrades
/// gracefully (capture still succeeds, lead stays local).
pub async fn get_coreswift_connection(state: &AppState, aid: Uuid) -> Option<(String, String)> {
    let api_key = get_provider_api_key(&state.db, aid, "coreswift").await?;
    if api_key.trim().is_empty() {
        return None;
    }

    let base_url = match get_provider_base_url(&state.db, aid, "coreswift").await {
        Some(u) if !u.trim().is_empty() => u,
        _ => {
            let cfg = state.config.coreswift_url.trim().to_string();
            if cfg.is_empty() {
                DEFAULT_CORESWIFT_URL.to_string()
            } else {
                cfg
            }
        }
    };

    Some((api_key, base_url.trim_end_matches('/').to_string()))
}

/// THE shared inbound helper. Every capture path and the manual push endpoint call this.
///
/// Quietly returns `false` when the account is not connected; logs real failures.
/// Never fails the caller — a capture must complete even when CoreSwift is down or absent.
pub async fn push_lead_to_coreswift(
    state: &AppState,
    aid: Uuid,
    lead: &CapturedLead,
    list_id: Option<String>,
    tags: &[String],
    fields: Value,
    context_label: &str,
) -> bool {
    if !lead.is_pushable() {
        tracing::debug!("CoreSwift push skipped ({context_label}): nothing to push");
        return false;
    }

    let (api_key, base_url) = match get_coreswift_connection(state, aid).await {
        Some(c) => c,
        None => {
            tracing::debug!(
                "CoreSwift push skipped ({context_label}): account {aid} not connected"
            );
            return false;
        }
    };

    let mut body = Map::new();
    if let Some(v) = lead.first_name.as_ref() {
        body.insert("first_name".into(), json!(v));
    }
    if let Some(v) = lead.last_name.as_ref() {
        body.insert("last_name".into(), json!(v));
    }
    if let Some(v) = lead.full_name.as_ref() {
        body.insert("name".into(), json!(v));
    }
    if let Some(v) = lead.email.as_ref() {
        body.insert("email".into(), json!(v));
    }
    if let Some(v) = lead.phone.as_ref() {
        body.insert("phone".into(), json!(v));
    }
    if let Some(v) = lead.company.as_ref() {
        body.insert("company".into(), json!(v));
    }
    if let Some(v) = lead.notes.as_ref() {
        body.insert("notes".into(), json!(v));
    }
    // Attribution: the hub canonicalises and records this as source / metadata.source_app.
    body.insert("source".into(), json!(SOURCE_APP));
    body.insert("source_app".into(), json!(SOURCE_APP));
    if let Some(s) = lead.source.as_ref() {
        body.insert("capture_source".into(), json!(s));
    }
    if let Some(lid) = list_id.as_ref().filter(|l| !l.trim().is_empty()) {
        body.insert("list_id".into(), json!(lid));
    }
    if !tags.is_empty() {
        body.insert("tags".into(), json!(tags));
    }
    if !fields.is_null() {
        body.insert("fields".into(), fields);
    }

    let url = format!("{base_url}/api/external/contacts");
    let resp = match reqwest::Client::new()
        .post(&url)
        .bearer_auth(&api_key)
        .json(&Value::Object(body))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("CoreSwift push failed (network) for aid {aid} ({context_label}): {e}");
            return false;
        }
    };

    let status = resp.status();
    if status.is_success() {
        tracing::info!("CoreSwift push OK for aid {aid} ({context_label})");
        true
    } else {
        let text: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect();
        tracing::warn!("CoreSwift push returned {status} for aid {aid} ({context_label}): {text}");
        false
    }
}

/// Proxy the hub's list picker: GET {base}/api/external/lists.
/// Returns the hub's JSON body on success, a human-readable error otherwise.
pub async fn hub_get_lists(state: &AppState, aid: Uuid) -> Result<Value, String> {
    let (api_key, base_url) = get_coreswift_connection(state, aid)
        .await
        .ok_or_else(|| "Not connected: store a CoreSwift key first".to_string())?;

    let resp = reqwest::Client::new()
        .get(format!("{base_url}/api/external/lists"))
        .bearer_auth(&api_key)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("CoreSwift unreachable: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let text: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect();
        return Err(format!("CoreSwift returned {status}: {text}"));
    }

    resp.json::<Value>()
        .await
        .map_err(|e| format!("CoreSwift returned invalid JSON: {e}"))
}

/// Live connection probe used by "Test connection" — hits the hub with the stored key.
pub async fn hub_probe(state: &AppState, aid: Uuid) -> Result<(u16, String), String> {
    let (api_key, base_url) = get_coreswift_connection(state, aid)
        .await
        .ok_or_else(|| "Not connected: store a CoreSwift key first".to_string())?;

    let resp = reqwest::Client::new()
        .get(format!("{base_url}/api/external/lists"))
        .bearer_auth(&api_key)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("CoreSwift unreachable at {base_url}: {e}"))?;

    let code = resp.status().as_u16();
    let body: String = resp
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect();
    Ok((code, body))
}
