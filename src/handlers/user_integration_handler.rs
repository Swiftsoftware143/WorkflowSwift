//! Step-provider resolution for the workflow Builder.
//!
//! This file used to be the CRUD surface for the legacy `user_integrations` store — list, upsert,
//! native toggle, delete and health-check. That store was RETIRED by kanban t_cb839034: it was
//! written by `POST /api/v1/integrations` and read by NO delivery path in this crate (0 rows live),
//! while every delivery path — `integration_dispatch_handler::forward_dispatch`'s fallback,
//! `src/ai_llm.rs`'s BYOK key and `src/execution.rs` — reads `provider_keys`, the store the admin
//! console's Provider Keys panel (`www-admin/index.html` -> POST /api/v1/provider-keys) writes and
//! the account-scoped `GET /api/v1/integrations/resolve` below resolves against since t_88082a4c.
//! Two BYOK stores where only one is read is the defect the card measured; the unread one, its six
//! routes, its dead admin panel ("My Integrations", whose Test/Remove buttons were bound to
//! nothing) and the table itself are gone with migration
//! `076_retire_user_integrations.sql`.
//!
//! What survives here is what a caller actually reads: `GET /api/v1/integrations/resolve`.

use axum::{
    extract::{Json, Query, State},
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::error::{ApiResult, AppError};
use crate::AppState;

// ──────────────────────────────────────────────
// Step resolution — find the right provider for a step
// ──────────────────────────────────────────────

/// Where a step type's provider vocabulary comes from. One variant per arm of the resolver, so a
/// test can pin that a RETIRED step type never maps to a source that returns a provider list
/// (kanban t_88082a4c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderSource {
    /// `crate::ai_llm::AI_PROVIDERS` — the engine's own BYOK vocabulary.
    AiEngine,
    /// `crate::email::EMAIL_PROVIDERS` — the mail sender's provider vocabulary.
    EmailSender,
    /// The app's live integration destination catalogue (`integration_destinations`).
    DestinationCatalogue,
    /// `crate::execution::NOTIFY_CHANNELS` — the engine's own Notify channel vocabulary.
    NotifyChannels,
    /// No provider is involved: the resolver answers `source: "none"` and advertises nothing.
    None,
}

/// The step types this resolver has a provider arm for, and what backs each vocabulary.
///
/// Anything else — including every RETIRED step type — maps to `None`. `playwright` / `browser`
/// used to have an arm advertising `browserbase`: the write path never accepted either step type
/// and `browserbase` exists in no configuration store in this app, so no caller could ever have
/// such a step and no tenant could ever configure one (kanban t_88082a4c).
fn step_type_provider_source(step_type: &str) -> ProviderSource {
    match step_type {
        "ai-action" | "ai_prompt" => ProviderSource::AiEngine,
        "email" => ProviderSource::EmailSender,
        "integration" => ProviderSource::DestinationCatalogue,
        "notify" => ProviderSource::NotifyChannels,
        _ => ProviderSource::None,
    }
}

/// Is this one of the three native SwiftSoftware products the app connects to directly? The same
/// set `integration_center_handler::get_destinations` flags as `is_native` and the one the app has
/// a base_url for without any tenant configuration.
fn is_native_provider(provider: &str) -> bool {
    matches!(provider, "coreswift" | "funnelswift" | "incentiveswift")
}

/// The provider vocabulary for an `integration` step, read from the app's own destination
/// catalogue (`integration_destinations`).
///
/// That table IS this app's definition of an integration destination: it is FK-bound to
/// `available_providers` (so a provider that exists in no database cannot be a destination — the
/// guard in migrations/066 is why the catalogue holds three providers and not forty-six), it is
/// served live to the admin console by `GET /integration-destinations`, and it is exactly the set
/// `integration_center_handler::get_provider_base_url` and the destination-value cascade can build
/// a live URL for. Before kanban t_88082a4c this arm was a hand-kept list of twelve names, nine of
/// which NO configuration store knew (`hubspot`, `salesforce`, `mailchimp`, `activecampaign`,
/// `convertkit`, `slack`, `discord`, `stripe`, `google_sheets`): a target created for one of them
/// reached `forward_dispatch`'s "Integration target has no webhook_url or provider preset"
/// refusal, so the endpoint advertised providers a tenant could never configure and the engine
/// could never deliver. Read live, so an operator adding a destination widens this vocabulary
/// without a code change.
async fn integration_provider_vocabulary(db: &sqlx::PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT provider FROM integration_destinations ORDER BY provider",
    )
    .fetch_all(db)
    .await
}

/// The account's stored credential for `provider`, from `provider_keys` — the store the app's own
/// delivery paths read (`forward_dispatch`'s fallback and the AI Action step's BYOK key).
/// Returns `(provider, base_url, has_key)`; `None` when the account has no active row.
async fn lookup_account_provider_key(
    db: &sqlx::PgPool,
    aid: Uuid,
    provider: &str,
) -> Result<Option<(String, Option<String>, bool)>, sqlx::Error> {
    let row = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
        r#"SELECT provider, base_url, api_key
           FROM provider_keys
           WHERE aid = $1 AND provider = $2 AND is_active = true
           LIMIT 1"#,
    )
    .bind(aid)
    .bind(provider)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|(provider, base_url, api_key)| {
        let has_key = api_key.map(|k| !k.is_empty()).unwrap_or(false);
        (provider, base_url, has_key)
    }))
}

/// Check what provider/engine a user's step should route to.
/// Returns the resolution result: user's key, system default, or error.
/// GET /api/v1/integrations/resolve?step_type=ai-action&provider=openai
///
/// The provider vocabulary per step type is NOT a hand-kept list any more (kanban t_88082a4c).
/// Each arm names the store that backs it (engine const, admin picker vocabulary, or the app's own
/// live catalogue), so the endpoint cannot advertise a provider no console can configure and no
/// delivery path can reach — which is what nine of the twelve names in the old `integration` arm
/// were, and what `export` was.
pub async fn resolve_step_provider(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let step_type = params.get("step_type").map(|s| s.as_str()).unwrap_or("");
    let requested_provider = params.get("provider");
    // The LLM step types are the ones with no platform fallback: their credential is the tenant's.
    let is_llm_step = matches!(step_type, "ai-action" | "ai_prompt");

    if step_type.is_empty() {
        return Err(AppError::BadRequest("step_type is required".into()));
    }

    // A RETIRED step type is refused BY NAME, exactly as the write path refuses it (kanban
    // t_02519738). This endpoint used to answer 200 with a provider list for `export` while
    // `create_workflow_step` rejects that same type with 422 — a vocabulary that advertised a step
    // type the app will not run.
    if crate::execution::is_retired_step_type(step_type) {
        return Err(AppError::Validation(format!(
            "Step type '{}' is retired: nothing in this app performs it, so no provider resolves \
             for it. Valid step types are: {}",
            step_type,
            crate::execution::executable_step_type_list()
        )));
    }

    // Map step types to the providers they can use. Each arm reads its vocabulary from the store
    // that backs it, so none of them can drift from what the app can really configure + deliver.
    let provider_options: Vec<String> = match step_type_provider_source(step_type) {
        // ONE vocabulary with the engine (kanban t_03e4d3d9): the providers this app can actually
        // call are `crate::ai_llm::AI_PROVIDERS` — the list the AI Action step resolves its BYOK
        // key against and the console's Provider Keys panel collects.
        ProviderSource::AiEngine => crate::ai_llm::provider_keys()
            .into_iter()
            .map(String::from)
            .collect(),
        // ONE vocabulary with the mail sender: `crate::email::EMAIL_PROVIDERS` is the same four
        // the Admin > Settings > Email picker offers and `send_email_request` has an arm for. This
        // arm used to also answer for `export`, a RETIRED step type — refused above now.
        ProviderSource::EmailSender => crate::email::EMAIL_PROVIDERS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        // The providers an integration step can route to are the ones the app's OWN destination
        // catalogue defines — read live from the DB, never a hand-kept list.
        ProviderSource::DestinationCatalogue => integration_provider_vocabulary(&state.db).await?,
        // ONE vocabulary with the engine's Notify channels (`crate::execution::NOTIFY_CHANNELS`).
        // This arm used to name slack/discord/sendgrid/smtp: every one of those is a channel
        // t_08be842f RETIRED (no sender exists in this product) that the Notify step refuses.
        ProviderSource::NotifyChannels => crate::execution::NOTIFY_CHANNELS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        // No provider is involved in this step type — including the deleted `playwright` | `browser`
        // arm, whose only name (`browserbase`) exists in no config store and whose step types the
        // write path never accepted, so no caller could ever have such a step.
        ProviderSource::None => vec![],
    };

    // Check if the ACCOUNT has a credential for the requested provider (or any matching provider).
    // The store is `provider_keys` — the one every delivery path in this app reads (`forward_dispatch`
    // falls back to it, and `crate::ai_llm` / `execution.rs` name it as the BYOK store for the AI
    // Action step) — NOT the legacy `user_integrations` table, which holds 0 rows and is read by no
    // delivery path (kanban t_88082a4c): a key the tenant really stored under Provider Keys never
    // resolved here, so the endpoint answered "no key connected" for a connected provider.
    let integrations = if provider_options.is_empty() {
        None
    } else if let Some(req_prov) = requested_provider {
        // The caller explicitly chose a provider.
        lookup_account_provider_key(&state.db, aid, req_prov).await?
    } else {
        // System auto-resolves — find the first matching provider the account has.
        let mut result = None;
        for prov in &provider_options {
            if let Some(row) = lookup_account_provider_key(&state.db, aid, prov).await? {
                result = Some(row);
                break;
            }
        }
        result
    };

    let resolution = if provider_options.is_empty() {
        // No provider is involved in this step type. Answering with the platform fallback here
        // advertised a 1-credit platform run for a step that calls no provider at all.
        json!({
            "source": "none",
            "credit_cost": 0,
            "message": format!(
                "Step type '{}' resolves no provider: this step type calls no provider in \
                 WorkflowSwift.",
                step_type
            ),
            "available_providers": provider_options
        })
    } else if let Some((provider, base_url, has_key)) = integrations {
        if is_native_provider(&provider) {
            json!({
                "source": "native",
                "provider": provider,
                "credit_cost": 0,
                "base_url": base_url,
            })
        } else if has_key {
            json!({
                "source": "user_key",
                "provider": provider,
                "credit_cost": 0,
                "has_key": true,
                "base_url": base_url,
            })
        } else if is_llm_step {
            // There is no platform LLM credential (kanban t_03e4d3d9): the app must not advertise a
            // "system" run at 1 credit that nothing can perform. An AI step runs on the tenant's own
            // key, or it does not run.
            json!({
                "source": "none",
                "provider": provider,
                "credit_cost": 0,
                "message": format!(
                    "No {} key connected for this account. AI Action runs on YOUR provider key (0 \
                     credits) — add one under Provider Keys.",
                    provider
                ),
                "available_providers": provider_options
            })
        } else {
            // Fall back to system default
            json!({
                "source": "system_default",
                "provider": provider,
                "credit_cost": 1,
                "message": "Using WorkflowSwift system — 1 credit per call"
            })
        }
    } else if is_llm_step {
        json!({
            "source": "none",
            "credit_cost": 0,
            "message": "No connected provider key for this account. AI Action runs on YOUR provider \
                        key (0 credits) — connect one under Provider Keys.",
            "available_providers": provider_options
        })
    } else {
        // No user integration found — fall back to system
        json!({
            "source": "system_default",
            "credit_cost": 1,
            "message": "Using WorkflowSwift system — 1 credit per call",
            "available_providers": provider_options
        })
    };

    Ok(Json(json!({
        "step_type": step_type,
        "resolution": resolution
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A RETIRED step type must never map to a source that returns a provider list: this endpoint
    /// answered `200` with `["sendgrid","smtp","mailgun"]` for `export`, a type the write path
    /// refuses by name with 422 (kanban t_88082a4c / t_02519738).
    #[test]
    fn no_retired_step_type_has_a_provider_source() {
        for retired in crate::execution::RETIRED_STEP_TYPES {
            assert_eq!(
                step_type_provider_source(retired),
                ProviderSource::None,
                "retired step type '{retired}' must not resolve a provider list"
            );
        }
    }

    /// The dead `playwright` | `browser` arm is gone. Its only name, `browserbase`, exists in no
    /// configuration store in this app and the write path never accepted either step type.
    #[test]
    fn the_dead_browser_arm_resolves_no_provider() {
        for dead in ["playwright", "browser"] {
            assert_eq!(
                step_type_provider_source(dead),
                ProviderSource::None,
                "'{dead}' was never an accepted step type"
            );
        }
    }

    /// The `notify` arm speaks the ENGINE's channel vocabulary: a Notify step refuses every channel
    /// outside `NOTIFY_CHANNELS`, and `slack`/`discord`/`sendgrid`/`smtp` are all retired there
    /// (t_08be842f), so none of them may be advertised as a provider for a Notify step.
    ///
    /// `email` and `sms` are BACK (kanban t_d3ff37ef) and are part of `NOTIFY_CHANNELS` again — but
    /// the resolver's answer is unchanged: `notify` resolves to the ENGINE's channel set, not to a
    /// static provider list, because which of those channels is offerable depends on which senders
    /// this install has configured (`NotifySenders`, `execution::notify_channel_available`).
    #[test]
    fn the_notify_arm_is_the_engine_channel_vocabulary() {
        assert_eq!(
            step_type_provider_source("notify"),
            ProviderSource::NotifyChannels
        );
        assert_eq!(
            crate::execution::NOTIFY_CHANNELS.to_vec(),
            vec!["webhook", "email", "sms"]
        );
        for resigned in ["slack", "discord", "sendgrid", "smtp"] {
            assert!(
                !crate::execution::is_notify_channel(resigned),
                "notify channel '{resigned}' is retired and must not be advertised"
            );
        }
        // The retired list and the sender-backed list are the other two halves of the vocabulary:
        // a retired channel is refused everywhere, and a sender-backed one is only offerable where
        // the matching sender exists (pinned in `crate::notify`'s own tests).
        assert!(crate::execution::RETIRED_NOTIFY_CHANNELS.contains(&"slack"));
        assert_eq!(
            crate::execution::SENDER_BACKED_NOTIFY_CHANNELS.to_vec(),
            vec!["email", "sms"]
        );
    }

    /// Every step type the tenant console offers in its Builder picker either has no provider arm
    /// or is one of the two arms the engine itself backs (`ai-action` → the AI provider list,
    /// `notify` → its channels). The resolver must not invent a provider for any other step type.
    #[test]
    fn console_offered_step_types_do_not_invent_providers() {
        for offered in [
            "action",
            "ai-action",
            "condition",
            "data-card",
            "delay",
            "fork",
            "http-request",
            "manual",
            "notify",
            "render_audio",
            "render_image",
            "render_video",
            "webhook",
        ] {
            let source = step_type_provider_source(offered);
            assert!(
                matches!(
                    source,
                    ProviderSource::None
                        | ProviderSource::AiEngine
                        | ProviderSource::NotifyChannels
                ),
                "console step type '{offered}' maps to {source:?}, which the console never configures"
            );
        }
    }
}
