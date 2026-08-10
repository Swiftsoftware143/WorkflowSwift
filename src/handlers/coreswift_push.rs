use reqwest::Client;
use serde_json::json;
use uuid::Uuid;

use crate::state::AppState;

const SWIFTSOFTWARE_TENANT: &str = "abd8ad22-aa01-4642-9a9f-6bef6a03d85b";
const SOFTWARE_LIST: &str = "WorkflowSwift Clients";
const SOURCE_APP: &str = "workflowswift";

/// Best-effort push of a new WorkflowSwift signup to CoreSwift under SwiftSoftware.
/// Creates contact → adds to WorkflowSwift Clients list → tags with plan.
/// Fire-and-forget — never fails the registration response.
pub async fn push_signup_to_coreswift(
    state: &AppState,
    account_id: Uuid,
    user_email: &str,
    user_name: &str,
    plan_slug: &str,
) {
    let cs_url = state.config.coreswift_url.trim_end_matches('/');
    if cs_url.is_empty() {
        return;
    }

    let internal_key = &state.config.internal_sync_key;
    let http = Client::new();

    let first_name: &str;
    let last_name: &str;
    let parts: Vec<&str> = user_name.splitn(2, ' ').collect();
    first_name = parts.first().unwrap_or(&"User");
    last_name = parts.get(1).unwrap_or(&"");

    // 1. Create contact in CoreSwift under SwiftSoftware
    let contact_id = match create_contact(
        &http,
        cs_url,
        internal_key,
        first_name,
        last_name,
        user_email,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("coreswift_push: create_contact failed: {e}");
            return;
        }
    };

    // 2. Add to WorkflowSwift Clients list
    let _ = add_to_list(&http, cs_url, internal_key, SOFTWARE_LIST, contact_id).await;

    // 3. Tag with plan: e.g., "workflowswift:Pro"
    let tag_name = format!("{}:{}", SOURCE_APP, plan_slug);
    let _ = ensure_tag(
        &http,
        cs_url,
        internal_key,
        contact_id,
        &tag_name,
        "#8b5cf6",
    )
    .await;

    // 4. Also tag with source
    let source_tag = format!("Source: {}", SOURCE_APP);
    let _ = ensure_tag(
        &http,
        cs_url,
        internal_key,
        contact_id,
        &source_tag,
        "#10b981",
    )
    .await;

    tracing::info!(
        account_id = %account_id,
        email = %user_email,
        tag = %tag_name,
        "coreswift_push: synced to WorkflowSwift Clients list under SwiftSoftware"
    );
}

async fn create_contact(
    http: &Client,
    base: &str,
    key: &str,
    first_name: &str,
    last_name: &str,
    email: &str,
) -> Result<Uuid, String> {
    let url = format!("{}/api/internal/contacts", base);
    let payload = json!({
        "tenant_id": SWIFTSOFTWARE_TENANT,
        "first_name": first_name,
        "last_name": last_name,
        "email": email,
    });

    let resp = http
        .post(&url)
        .header("x-internal-key", key)
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }

    let data: serde_json::Value = resp.json().await.map_err(|e| format!("parse: {e}"))?;

    Uuid::parse_str(data["id"].as_str().ok_or("no id field")?)
        .map_err(|e| format!("invalid uuid: {e}"))
}

async fn add_to_list(
    http: &Client,
    base: &str,
    key: &str,
    list_name: &str,
    contact_id: Uuid,
) -> Result<(), String> {
    let url = format!("{}/api/internal/lists", base);

    // Get or create the list by name
    let list_payload = json!({
        "tenant_id": SWIFTSOFTWARE_TENANT,
        "name": list_name,
        "list_type": "static"
    });

    let resp = http
        .post(&url)
        .header("x-internal-key", key)
        .json(&list_payload)
        .send()
        .await
        .map_err(|e| format!("list get/create failed: {e}"))?;

    let data: serde_json::Value = resp.json().await.unwrap_or_default();
    let list_id = data["id"].as_str().ok_or("no list id")?;

    // Add contact to list
    let member_url = format!("{}/api/internal/lists/{}/members", base, list_id);
    let member_payload = json!({
        "tenant_id": SWIFTSOFTWARE_TENANT,
        "contact_id": contact_id,
    });

    let resp = http
        .post(&member_url)
        .header("x-internal-key", key)
        .json(&member_payload)
        .send()
        .await
        .map_err(|e| format!("member add failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("member add returned {}", resp.status()));
    }

    Ok(())
}

async fn ensure_tag(
    http: &Client,
    base: &str,
    key: &str,
    contact_id: Uuid,
    tag_name: &str,
    tag_color: &str,
) -> Result<(), String> {
    let tag_url = format!("{}/api/internal/tags", base);
    let tag_payload = json!({
        "tenant_id": SWIFTSOFTWARE_TENANT,
        "name": tag_name,
        "color": tag_color
    });

    let resp = http
        .post(&tag_url)
        .header("x-internal-key", key)
        .json(&tag_payload)
        .send()
        .await
        .map_err(|e| format!("tag create failed: {e}"))?;

    let data: serde_json::Value = resp.json().await.unwrap_or_default();
    let tag_id = data["id"].as_str().ok_or("no tag id")?;

    let assign_url = format!("{}/api/internal/tags/assign", base);
    let assign_payload = json!({
        "tenant_id": SWIFTSOFTWARE_TENANT,
        "tag_id": tag_id,
        "entity_id": contact_id,
        "entity_type": "contact"
    });

    let _ = http
        .post(&assign_url)
        .header("x-internal-key", key)
        .json(&assign_payload)
        .send()
        .await;

    Ok(())
}
