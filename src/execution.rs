//! In-process workflow execution engine.
//!
//! ONE engine, two entry points:
//!   * `POST /api/v1/incoming`  — a Swift tool pushes a lead (capture path)
//!   * `POST /api/v1/workflows/{id}/run` / `{id}/start` — the user presses Run
//!
//! Historically the user-facing Run path shelled out to `docker cp` +
//! `docker exec user-n8n-main n8n import:workflow`. That container does not
//! exist, and the app container has no docker binary and no docker socket, so
//! Run was a guaranteed HTTP 500. The inbound path, by contrast, always
//! executed the steps in this process. This module is that proven loop,
//! lifted verbatim so both paths run the same code.
//!
//! n8n is OPTIONAL: the per-plan `n8n_deploy` flag decides whether a run also
//! mirrors the workflow into n8n. It can never fail a run.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
// The dashboard series key space lives with its writers; the `data-card` step below reads through
// the same helpers so a step's metric_key resolves exactly like a widget's config.metric_key does.
use crate::handlers::industry_handler::{
    canonical_metric_key, latest_widget_metric, metric_key_candidates,
};
use crate::state::AppState;

/// Everything a step can see about the run that triggered it.
#[derive(Debug, Clone)]
pub struct StepContext {
    pub source: String,
    pub campaign_slug: String,
    pub contact: Value,
    pub data: Option<Value>,
    pub source_entry_id: Option<String>,
    pub context: Value,
}

impl StepContext {
    /// Context for a user-initiated run from the app UI: no external lead.
    pub fn manual(aid: Uuid, workflow_id: Uuid, triggered_by: &str, payload: Value) -> Self {
        Self {
            source: "manual".to_string(),
            campaign_slug: "manual".to_string(),
            contact: json!({}),
            data: Some(payload.clone()),
            source_entry_id: None,
            context: json!({
                "source": "manual",
                "campaign_slug": "manual",
                "triggered_by": triggered_by,
                "aid": aid,
                "workflow_id": workflow_id,
                "payload": payload,
                "started_at": Utc::now().to_rfc3339(),
            }),
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct StepRow {
    pub id: Uuid,
    pub step_type: String,
    pub name: String,
    pub config: Option<Value>,
    pub sort_order: i32,
    pub integration_target_id: Option<Uuid>,
}

/// Result of walking every step of one instance.
#[derive(Debug)]
pub struct ExecutionOutcome {
    pub steps: Vec<Value>,
    pub status: String,
    /// Steps that finished the walk without executing: `manual`/`approval`
    /// (a human must approve or reject) and `delay`/`wait` (a timer must fire).
    /// These are settled by the background worker (src/execution_worker.rs) or by
    /// the decision endpoint, both of which resume this same walk — so a run that
    /// leaves one behind is reported `pending`, never `completed`, until it moves.
    pub pending_steps: i32,
    pub failed_steps: i32,
}

/// Every step type this engine has an arm for. The API's accepted vocabulary IS this list:
/// `workflow_handler::validate_workflow_steps` reads it, and `create_workflow_step` refuses
/// anything else — so the app can no longer accept a step type it will not run. Before kanban
/// t_fe60cdf5 there were two hand-kept lists and eleven of the accepted names had no arm here.
pub const EXECUTABLE_STEP_TYPES: &[&str] = &[
    "http-request",
    "action",
    "ai-action",
    "data-card",
    "data_card",
    "notify",
    "delay",
    "wait",
    "fork",
    "branch",
    "render_video",
    "render_media",
    "render_image",
    "render_audio",
    "loop",
    "condition",
    "manual",
    "webhook",
];

/// Step types that were OFFERED or accepted and are now RETIRED because nothing in this app can
/// deliver what they advertise (kanban t_02519738). They stay in this list — and out of
/// `EXECUTABLE_STEP_TYPES` — so the write path refuses them and a row written straight into the
/// database is an honest `skipped` step whose `unexecutable` marker reaches the run's `warnings[]`.
///
/// * `export` — the CONSOLE offered it ("CSV / Resend / SendGrid / CoreSwift CRM", step config
///   `destination`) and the engine POSTed a platform-shaped body to
///   `{N8N_WEBHOOK_URL}/webhook/workflowswift-export` — not registered (404) — and returned
///   `status: "completed"` for BOTH the 404 and a transport failure, so a tenant's Export step was
///   recorded and shown as completed having exported nothing (measured live 2026-10-02). No
///   exporter exists on either side: CSV has no store or download surface in this app,
///   Resend/SendGrid are credentials the platform does not hold, and the CoreSwift CRM path is an
///   inbound lead push, not an export contract. Same disposition as `research`/`openclaw`
///   (t_fe60cdf5) and the undeliverable Notify channels (t_08be842f).
/// * `publish` / `generate` — the same platform webhooks (`workflowswift-publish`,
///   `workflowswift-generate`), the same 404, never offered by the console and with 0 rows
///   fleet-wide. The n8n mirror had already retired both into a pass-through that NAMES what is
///   missing (kanban t_642b6894); the engine's arms are retired here the same way — by not being
///   step types at all.
/// * `research` / `openclaw` — retired before this list existed (kanban t_fe60cdf5; nothing in this
///   app performs either), and named here so the write path's refusal says "retired" for them too.
/// * `transform` / `code` / `format` — the TENANT-JAVASCRIPT family (kanban t_81602ca1). All three
///   were accepted by the write path and had an engine arm that ran NO code: it read `format`/`tone`
///   and answered `status: "completed"` with the note "Formatting queued — will transform content
///   for selected platform" (measured live 2026-10-02: a workflow with `transform`, `code` and
///   `format` steps ran and the console recorded THREE completed `type: "format"` steps, none of
///   which transformed anything). The step's only real behaviour was on the other side: the n8n
///   mirror emitted a real `n8n-nodes-base.code` node carrying the tenant's `config.code` VERBATIM
///   (`transform`/`code`) or interpolating the tenant's `config.input_content` straight into
///   generated JavaScript (`format`). That node cannot run on this install at all — n8n 2.34.6 in
///   `swift-n8n` has no task runner configured (`N8N_RUNNERS_*` absent) and n8n's own Code node
///   throws its opaque `Unknown error` (`ERR_ASSERTION`), measured end to end AND on a hand-made
///   bare `webhook → Code` graph, so it is the INSTALL and not the converter's node shape. That is
///   "unavailable", NOT "proven safe": enabling the runner would execute tenant-supplied JavaScript
///   inside the container that holds n8n's encryption key and every stored credential (2 encrypted
///   credentials — `postgres`, `telegramApi` — 1 user, 40 workflows, measured 2026-10-02), and the
///   destination gate the mirror added for t_2741ac13 cannot see it because the code is not a URL.
///   The card DECIDED to fail closed rather than enable it: no tenant code runs here, so no step
///   type may promise it. Same disposition as `export`/`publish`/`generate` — a step type nothing
///   can deliver is not a step type.
/// * `design` — the engine's arm read `style`/`dimensions`, generated nothing (no provider, no
///   route, no row writer) and answered `status: "completed"` with the note "Design queued — will
///   generate visual assets via configured provider" (kanban t_f10ada7a, measured live
///   2026-10-02: a `design` step ran and the run reported `completed` with an EMPTY `warnings[]`
///   while nothing was designed). The n8n mirror has been honest about this since kanban
///   t_642b6894 — its `design` node is a pass-through whose notes say "no design route exists in
///   WorkflowSwift" — and the console NEVER offered `design` in its Builder picker, so the two
///   executors disagreed about a type no tenant could pick. 0 rows fleet-wide
///   (`workflow_steps` 3: n8n/data-card/integration; `workflow_template_steps` 10: data-card 1,
///   manual 9), so retiring it strands nothing. Implementing it (which provider, which key, what
///   it costs, and the `style`/`dimensions` field set the console does not collect) is a product
///   call, not this card's; until that exists no step type may claim `completed` for a design
///   nobody generated.
pub const RETIRED_STEP_TYPES: &[&str] = &[
    "export",
    "publish",
    "generate",
    "research",
    "openclaw",
    // kanban t_81602ca1: the tenant-JavaScript family.
    "transform",
    "code",
    "format",
    // kanban t_f10ada7a: `design` advertised generated visual assets and generated nothing.
    "design",
];

/// Is this a step type the engine can execute? The write path's and the validator's one rule.
pub fn is_executable_step_type(step_type: &str) -> bool {
    EXECUTABLE_STEP_TYPES.contains(&step_type)
}

/// Was this step type retired (offered/accepted once, delivers nothing)? The write path's refusal
/// names the shape, and a stored row of this type is `unexecutable` in the walk.
pub fn is_retired_step_type(step_type: &str) -> bool {
    RETIRED_STEP_TYPES.contains(&step_type)
}

/// Why an AI Action step did nothing: the account has no key connected for the provider the step
/// names (kanban t_03e4d3d9). The step runs on the TENANT'S OWN key, so there is no platform
/// fallback to silently try — the reason says where to add one.
fn ai_action_no_key_reason(provider: &str) -> String {
    format!(
        "AI Action did nothing: no '{}' key is connected for this account, and this step runs on \
         your own provider key (0 credits). Add one under Provider Keys, then re-run.",
        provider
    )
}

/// Why an AI Action step did nothing: the step names no provider, or one this app cannot call.
fn ai_action_no_provider_reason(named: &str) -> String {
    if named.is_empty() {
        format!(
            "AI Action did nothing: the step names no provider. This app calls exactly: {}.",
            crate::ai_llm::provider_key_list()
        )
    } else {
        format!(
            "AI Action did nothing: '{}' is not an LLM provider this app calls. Valid providers: {}.",
            named,
            crate::ai_llm::provider_key_list()
        )
    }
}

/// An AI Action step that did nothing, carrying the provider it named so the console can point at
/// the right Provider Keys row. `skipped` + `undeliverable`, exactly like a Notify channel with no
/// sender (kanban t_08be842f): the reason reaches the run's `warnings[]`.
fn ai_action_undeliverable_result(step: usize, provider: &str, why: &str) -> Value {
    let mut result = step_without_an_executor_result(step, "ai-action", why);
    result["provider"] = json!(provider);
    result
}

/// The result of the AI Action arm's one outbound call (kanban t_03e4d3d9).
///
/// `Ok((status, body))` is the PROVIDER'S answer: the status code is stored, so `classify_step_status`
/// maps a non-2xx to a `failed` step (a 401 from the provider is a failed step, never a
/// completion), and `generated_content` is only ever the provider's own message. A 2xx whose body
/// carries no message is an `error` — the arm this replaced answered `completed` with
/// `generated_content: <the prompt>`, i.e. a generation that never happened.
///
/// `Err` is a transport failure / timeout: the destination was never reached, so it is an `error`
/// naming why, never a completion.
fn ai_action_step_result(
    step: usize,
    provider: &str,
    model: &str,
    call: Result<(u16, String), String>,
) -> Value {
    match call {
        Ok((status, body)) => {
            let content = if (200..300).contains(&status) {
                crate::ai_llm::provider_by_key(provider)
                    .and_then(|p| crate::ai_llm::extract_content(p, &body))
            } else {
                None
            };
            match content {
                Some(text) => json!({
                    "step": step,
                    "type": "ai-action",
                    "status": status,
                    "provider": provider,
                    "model": model,
                    "generated_content": text,
                    "response": truncate_for_log(&body, 500),
                }),
                None if (200..300).contains(&status) => json!({
                    "step": step,
                    "type": "ai-action",
                    "status": "error",
                    "provider": provider,
                    "model": model,
                    "error": format!(
                        "{} answered HTTP {} without a message, so the step generated nothing",
                        provider, status
                    ),
                    "response": truncate_for_log(&body, 500),
                }),
                None => json!({
                    "step": step,
                    "type": "ai-action",
                    "status": status,
                    "provider": provider,
                    "model": model,
                    "response": truncate_for_log(&body, 500),
                }),
            }
        }
        Err(e) => json!({
            "step": step,
            "type": "ai-action",
            "status": "error",
            "provider": provider,
            "model": model,
            "error": e,
        }),
    }
}

/// The AI Action decision — the ONE implementation, shared by the in-process engine arm above
/// (kanban t_03e4d3d9) and the n8n mirror's callback (`POST /api/v1/n8n/ai-action`, kanban
/// t_9f556c5c).
///
/// The mirror is executed by the n8n container, which holds no LLM provider credential, so the
/// mirrored copy of an AI Action step cannot make the call itself. Instead of passing the step
/// through (a `n8n-nodes-base.noOp` that silently did nothing when a tenant triggered the copy),
/// the graph CALLS BACK here: an externally-triggered mirror — or one the tenant activated in its
/// own n8n — then runs exactly the step this workflow describes, because the callback reads the
/// step's STORED config and an edited graph cannot make this app run a different call.
///
/// BYOK: the credential is the TENANT'S OWN key from `provider_keys` (the same mapping
/// `/integrations/resolve?step_type=ai-action` serves), so the call costs 0 credits and spends no
/// platform money; the destination is a CONSTANT per provider, never a value out of step config,
/// so no new SSRF surface is opened.
///
/// Four outcomes, none of them a fabricated completion: a no/unknown provider or a blank prompt
/// gives `skipped` naming what is missing; a provider named with no key connected gives `skipped`
/// naming the Provider Keys row; a provider that answered gives its HTTP status and its own
/// message as `generated_content`; a transport failure or a 2xx carrying no message is an
/// `error`, never `completed`.
pub async fn ai_action_outcome(
    db: &PgPool,
    aid: Uuid,
    step: usize,
    prompt: &str,
    named_provider: &str,
    model_override: Option<&str>,
) -> Value {
    if prompt.trim().is_empty() {
        return ai_action_undeliverable_result(
            step,
            named_provider,
            "AI Action did nothing: the step has a blank Prompt. Write the prompt this step \
             should send to your provider, then re-run.",
        );
    }
    let Some(provider) = crate::ai_llm::provider_by_key(named_provider) else {
        return ai_action_undeliverable_result(
            step,
            named_provider,
            &ai_action_no_provider_reason(named_provider),
        );
    };
    // `provider_keys.base_url` is deliberately NOT honoured: the destination is the provider
    // constant, so a tenant cannot point its own credential at an address of its choosing (the
    // SSRF surface the step arms' `gate_step_destination` exists to close).
    let stored =
        crate::handlers::provider_keys_handler::get_provider_key(db, aid, provider.key).await;
    match stored {
        Err(e) => json!({
            "step": step,
            "type": "ai-action",
            "status": "error",
            "provider": provider.key,
            "error": format!("could not read the account's {} key: {}", provider.key, e),
        }),
        Ok(None) => ai_action_undeliverable_result(
            step,
            provider.key,
            &ai_action_no_key_reason(provider.key),
        ),
        Ok(Some((api_key, _base_url, _metadata))) if api_key.is_empty() => {
            ai_action_undeliverable_result(
                step,
                provider.key,
                &ai_action_no_key_reason(provider.key),
            )
        }
        Ok(Some((api_key, _base_url, _metadata))) => {
            let model = model_override
                .filter(|m| !m.trim().is_empty())
                .unwrap_or(provider.default_model);
            let call = crate::ai_llm::generate(provider, model, &api_key, prompt).await;
            ai_action_step_result(step, provider.key, model, call)
        }
    }
}

/// The accepted vocabulary as one line, for a 400 body / a validation error.
pub fn executable_step_type_list() -> String {
    EXECUTABLE_STEP_TYPES.join(", ")
}

/// Every Notify channel this product KNOWS. The tenant console offers exactly these
/// (`www-app/index.html`, the Notify step's Channel select), `create_workflow_step` /
/// `update_workflow_step` and `validate_workflow_steps` refuse anything else, and the n8n mirror's
/// notify arm emits a real node only for these.
///
/// `email` and `sms` were RETIRED (kanban t_08be842f) because neither had a sender anywhere in
/// this app. They are BACK (kanban t_d3ff37ef) — and the reason they can be back is that the two
/// things that retired them now exist:
///
/// * `email` — the app owns a mail path (`email::send_email`, behind Admin > Settings > Email
///   Provider) and a new app route relays a Notify step's mail through it (`POST
///   /api/v1/notify/dispatch`), so the mirror never needs — and never gets — an SMTP credential.
/// * `sms` — a real provider seam now exists (`crate::sms`, behind Admin > Settings > SMS).
///
/// Being IN this list is not the same as being OFFERABLE. Both sender-backed channels are
/// FAIL-CLOSED: `notify_channels_for` reports the channels this install can actually deliver on,
/// and the write path accepts `email`/`sms` only when that sender is configured.
///
/// `webhook` POSTs `{message, data}` to a URL the tenant owns — the outbound call both the engine
/// and the n8n mirror really make. It needs no provider, so it is always available.
pub const NOTIFY_CHANNELS: &[&str] = &["webhook", "email", "sms"];

/// The channels whose availability depends on a configured SENDER. `webhook` is deliberately not
/// here — it is an outbound call to a URL the tenant owns.
pub const SENDER_BACKED_NOTIFY_CHANNELS: &[&str] = &["email", "sms"];

/// Channels that are NOT part of this product at all: never sold, never deliverable, and refused
/// wherever a channel is checked. `slack`/`telegram` were retired with the callback that never
/// existed (kanban t_642b6894) and stay retired.
pub const RETIRED_NOTIFY_CHANNELS: &[&str] = &["slack", "telegram"];

/// Is this a Notify channel the product KNOWS? The engine's and the mirror's vocabulary rule.
/// Whether it can be DELIVERED right now is a separate question — see `notify_channel_available`.
pub fn is_notify_channel(channel: &str) -> bool {
    NOTIFY_CHANNELS.contains(&channel)
}

/// Can this channel be delivered on right now, given which senders this install has configured?
/// This is the rule the write path gates on, so nothing is ever offered that cannot be sent.
pub fn notify_channel_available(channel: &str, senders: &crate::notify::NotifySenders) -> bool {
    senders.available(channel)
}

/// The channels this install may currently OFFER, as one line, for a 400 body / a validation
/// error. The configured set, not the vocabulary (`notify_channel_list`).
pub fn notify_channels_for(senders: &crate::notify::NotifySenders) -> String {
    senders.offered().join(", ")
}

/// The whole Notify channel vocabulary as one line. Used where the product describes what it
/// supports rather than what one install has configured.
pub fn notify_channel_list() -> String {
    NOTIFY_CHANNELS.join(", ")
}

/// The retired/never-supported channels as one line, for a refusal that has to name them.
pub fn retired_notify_channel_list() -> String {
    RETIRED_NOTIFY_CHANNELS.join(", ")
}

/// Cap a response body before it goes into a step result and the execution log.
fn truncate_for_log(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}… [{} more bytes]",
        &text[..end],
        text.len().saturating_sub(end)
    )
}

/// The result for a step type this engine has no arm for.
///
/// It is `skipped` and carries the `unexecutable` marker, which `run_in_process` turns into a
/// `warnings[]` entry the console shows.
///
/// The arm this replaced answered status `warning` and told the operator the workflow continues.
/// `classify_step_status` maps that to "warning", which is neither failed nor pending, so the
/// instance was recorded **completed** with an empty `warnings[]`: a run that reported success
/// around a step that never happened (kanban t_fe60cdf5).
fn unexecutable_step_result(step: usize, step_type: &str) -> Value {
    json!({
        "step": step,
        "type": step_type,
        "status": "skipped",
        "unexecutable": step_type,
        "reason": format!(
            "Step type '{}' has no executor in this app — the step did nothing. It cannot be added \
             from the console; supported types: {}",
            step_type,
            EXECUTABLE_STEP_TYPES.join(", ")
        ),
    })
}

/// The result of the `notify` arm's one outbound call (kanban t_e0e6a42e).
///
/// An `Ok` carries the upstream status **code**, which `classify_step_status` maps to `failed`
/// for anything outside 2xx-3xx — so a destination that answers 404/500 fails the step instead
/// of reading as a completion. An `Err` (a destination the SSRF gate refused, a transport
/// failure, a timeout) is an explicit `error` with the reason. The arm this replaced collapsed
/// BOTH into `status: "completed"`, which is how a 404 from a webhook that does not exist was
/// reported as a successful Notify step.
fn notify_step_result(
    step: usize,
    channel: &str,
    url: &str,
    call: Result<(u16, String), String>,
) -> Value {
    match call {
        Ok((status, body)) => json!({
            "step": step,
            "type": "notify",
            "status": status,
            "channel": channel,
            "recipient": url,
            "response": truncate_for_log(&body, 500),
        }),
        Err(e) => json!({
            "step": step,
            "type": "notify",
            "status": "error",
            "channel": channel,
            "recipient": url,
            "error": e,
        }),
    }
}

/// A Notify step that could not send anything, with the reason NAMING the missing sender.
///
/// Two shapes reach it, both of them a row written straight into the database (the steps API
/// refuses a channel outside `NOTIFY_CHANNELS` and requires a `recipient`): a channel retired by
/// kanban t_08be842f (`email`/`sms` — this app has no mail relay reachable from a step and no SMS
/// provider) and a blank Webhook URL. It is `skipped`, not `completed`: the step did nothing, and
/// the walk's roll-up would otherwise report a run around it as a clean success. `undeliverable`
/// carries the reason to the run's `warnings[]` (workflow_handler::run_in_process).
fn notify_undeliverable_result(step: usize, channel: &str, reason: &str) -> Value {
    json!({
        "step": step,
        "type": "notify",
        "status": "skipped",
        "channel": channel,
        "undeliverable": reason,
        "reason": reason,
    })
}

/// A step this app cannot execute AT ALL — the console offers it (or the API accepted it) but no
/// path in this crate performs the work, so the step did nothing and `why` NAMES the missing piece.
///
/// It is `skipped`, not `failed` (nothing upstream failed — there is nothing upstream) and
/// certainly not `completed`; `undeliverable` carries the reason to the run's `warnings[]`
/// (`workflow_handler::run_in_process`), the same shape a Notify channel with no sender gets
/// (kanban t_08be842f).
fn step_without_an_executor_result(step: usize, step_type: &str, why: &str) -> Value {
    json!({
        "step": step,
        "type": step_type,
        "status": "skipped",
        "undeliverable": why,
        "reason": why,
    })
}

/// One outbound call for the step arms whose destination comes from the step's own config
/// (`webhook`, `http-request`/`action`, `render_*`).
///
/// Both halves of the SSRF pair are deliberate:
///   * `webhook_security::gate_step_destination` refuses a destination inside the box BEFORE any
///     socket is opened — these arms put the target's body in the step result, so without the gate
///     a step is an authenticated read primitive against `127.0.0.1`, the docker bridge or the
///     cloud metadata address;
///   * the client is built with `redirect::Policy::none()`, because following a 30x would be a free
///     hop past that gate (a validated public host that redirects into loopback).
///
/// Returns `(status_code, body)`; each caller decides what a non-2xx means for its step type.
async fn step_outbound_call(
    method: &str,
    url: &str,
    payload: &Value,
    timeout_secs: u64,
) -> Result<(u16, String), String> {
    crate::security::webhook_security::gate_step_destination(url).await?;

    let verb = reqwest::Method::from_bytes(method.trim().to_uppercase().as_bytes())
        .map_err(|e| format!("Unsupported HTTP method '{}': {}", method, e))?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("Could not build the HTTP client: {}", e))?;

    let mut req = client.request(verb.clone(), url);
    if verb != reqwest::Method::GET {
        req = req.json(payload);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    Ok((status, resp.text().await.unwrap_or_default()))
}

/// The run a rendition row belongs to.
///
/// Grouped rather than passed as ten positional arguments: two of the NOT NULL columns
/// (`provider_asset_id`, `provider_asset_url`) and the `user_id` all come from different places,
/// and a struct makes the one caller say which is which (clippy `too_many_arguments`).
struct RenditionRequest<'a> {
    aid: Uuid,
    context: &'a Value,
    workflow_id: Uuid,
    instance_id: Uuid,
    step_type: &'a str,
    step_name: &'a str,
    provider: &'a str,
    asset_type: &'a str,
    /// The provider's raw response body, kept verbatim in the row's metadata.
    body: &'a str,
}

/// Write the `account_renditions` row a `render_*` step promises — the console's Builder says a
/// render step's "output is logged as a rendition and shows up under Renditions".
///
/// The provider's own response decides the row: `provider_asset_id` and `provider_asset_url` are
/// NOT NULL, and a response without them is reported as a step error that names what the provider
/// actually sent — the step never invents an id or a URL to make a green row (kanban t_fe60cdf5).
async fn record_rendition(state: &AppState, req: RenditionRequest<'_>) -> Result<Uuid, String> {
    let parsed: Value = serde_json::from_str(req.body).unwrap_or(Value::Null);
    let pick = |keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|k| {
            parsed
                .get(*k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
    };
    let id_keys = [
        "id",
        "asset_id",
        "render_id",
        "video_id",
        "file_id",
        "job_id",
    ];
    let url_keys = [
        "url",
        "video_url",
        "asset_url",
        "generated_url",
        "download_url",
        "output_url",
    ];
    let asset_id = pick(&id_keys).ok_or_else(|| {
        format!(
            "the render provider '{}' answered HTTP 2xx without an asset id (looked for {} in: {})",
            req.provider,
            id_keys.join("/"),
            truncate_for_log(req.body, 200)
        )
    })?;
    let asset_url = pick(&url_keys).ok_or_else(|| {
        format!(
            "the render provider '{}' answered HTTP 2xx without an asset URL (looked for {} in: {})",
            req.provider,
            url_keys.join("/"),
            truncate_for_log(req.body, 200)
        )
    })?;
    let preview_url = pick(&["preview_url", "thumbnail_url", "url", "video_url"]);
    let thumbnail_url = pick(&["thumbnail_url", "thumbnail"]);

    // The run's user: the JWT subject on a manual run (the engine puts it in the context), else the
    // account's first active user — an incoming capture has no user of its own. Never invented: no
    // user means the step fails and says why, rather than writing a row that belongs to nobody.
    let user_id = match req
        .context
        .get("triggered_by")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(u) => Some(u),
        None => sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM users WHERE aid = $1 AND is_active ORDER BY created_at LIMIT 1",
        )
        .bind(req.aid)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| format!("DB error resolving the run's user: {}", e))?,
    }
    .ok_or_else(|| "this account has no user to attach the rendition to".to_string())?;

    let provider_category: Option<String> =
        sqlx::query_scalar("SELECT category FROM available_providers WHERE key = $1")
            .bind(req.provider)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();

    let rendition_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO account_renditions
           (id, aid, user_id, workflow_id, instance_id, step_type, step_name, provider,
            provider_asset_id, provider_asset_url, preview_url, thumbnail_url, asset_type,
            provider_category, retention_expires_at, status, metadata)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                   NOW() + INTERVAL '90 days', 'active', $15)"#,
    )
    .bind(rendition_id)
    .bind(req.aid)
    .bind(user_id)
    .bind(req.workflow_id)
    .bind(req.instance_id)
    .bind(req.step_type)
    .bind(req.step_name)
    .bind(req.provider)
    .bind(&asset_id)
    .bind(&asset_url)
    .bind(&preview_url)
    .bind(&thumbnail_url)
    .bind(req.asset_type)
    .bind(&provider_category)
    .bind(json!({ "provider_response": parsed, "logged_by": "src/execution.rs" }))
    .execute(&state.db)
    .await
    .map_err(|e| format!("DB error writing the rendition: {}", e))?;

    Ok(rendition_id)
}

/// The `status` a step result really has.
///
/// Three step arms (webhook, n8n, publish) store a raw HTTP status *code* in
/// `status`. Reading that with `as_str()` fell through to the default
/// "completed", so an upstream step that answered 500 was recorded — and
/// displayed — as a success. Numbers are classified here instead.
pub fn classify_step_status(result: &Value) -> String {
    match result.get("status") {
        Some(Value::Number(n)) => {
            let code = n.as_u64().unwrap_or(0);
            if (200..400).contains(&code) {
                "completed".to_string()
            } else {
                "failed".to_string()
            }
        }
        Some(Value::String(s)) => match s.as_str() {
            "error" | "failed" => "failed".to_string(),
            "pending" => "pending".to_string(),
            "skipped" => "skipped".to_string(),
            "warning" => "warning".to_string(),
            "in_progress" => "in_progress".to_string(),
            _ => "completed".to_string(),
        },
        _ => "completed".to_string(),
    }
}

/// Human-readable reason for a step that did not succeed, for the log row.
fn step_error_text(result: &Value) -> Option<String> {
    for key in ["error", "reason", "message"] {
        if let Some(s) = result.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    match result.get("status") {
        // A numeric `status` is the upstream HTTP code a `webhook`/`http-request`/`notify`/
        // `render_*` arm stored. It belongs in the log row only when the call FAILED: applied
        // without this check the walk wrote `error_message = "upstream returned HTTP 200"` on a
        // `completed` step, i.e. every successful outbound call carried a fake error (measured live
        // 2026-10-02, kanban t_02519738).
        Some(Value::Number(n)) => {
            let code = n.as_u64().unwrap_or(0);
            if (200..400).contains(&code) {
                None
            } else {
                Some(format!("upstream returned HTTP {}", code))
            }
        }
        _ => None,
    }
}

/// Result of a `data-card` step.
///
/// A lookup failure becomes `status: "error"` with the reason in `error`: `classify_step_status`
/// reads that as `failed` (so the step — and the run — report the truth) and `step_error_text`
/// copies it into the `workflow_execution_logs.error_message`. The pre-fix arm used
/// `unwrap_or(None)`, so a query that never ran produced `metric_value: null` and a *completed*
/// step, indistinguishable from a widget nobody had pushed to.
fn data_card_result(
    step: usize,
    widget_name: &str,
    metric_key: &str,
    lookup: Result<Option<Value>, sqlx::Error>,
) -> Value {
    match lookup {
        Ok(metric_value) => json!({
            "step": step,
            "type": "data-card",
            "status": "completed",
            "widget_name": widget_name,
            "metric_key": metric_key,
            "resolved_metric_key": if metric_key.is_empty() {
                String::new()
            } else {
                canonical_metric_key(metric_key)
            },
            "metric_value": metric_value,
        }),
        Err(e) => json!({
            "step": step,
            "type": "data-card",
            "status": "error",
            "widget_name": widget_name,
            "metric_key": metric_key,
            "error": format!("dashboard series lookup failed: {e}"),
        }),
    }
}

/// How long a `delay`/`wait` step holds the run.
///
/// `duration_ms` is the canonical config key (the n8n converter's wait node and
/// the step validator both read it); `duration` is a human string ("5s", "90",
/// "30m", "1h", "2d") that older workflows — and the smoke harness — carry
/// instead. A config with neither keeps the engine's original 1h default.
pub fn delay_duration_ms(config: &Value) -> i64 {
    if let Some(ms) = config.get("duration_ms").and_then(|v| v.as_i64()) {
        if ms > 0 {
            return ms;
        }
    }
    if let Some(secs) = config.get("duration_seconds").and_then(|v| v.as_i64()) {
        if secs > 0 {
            return secs.saturating_mul(1000);
        }
    }
    if let Some(ms) = config
        .get("duration")
        .and_then(|v| v.as_str())
        .and_then(parse_duration_str)
    {
        return ms;
    }
    if let Some(secs) = config.get("duration").and_then(|v| v.as_i64()) {
        if secs > 0 {
            return secs.saturating_mul(1000);
        }
    }
    3_600_000
}

/// `"5s"` / `"90"` / `"30m"` / `"2h"` / `"1d"` / `"1w"` -> milliseconds.
/// `None` when the value carries no number at all.
pub fn parse_duration_str(s: &str) -> Option<i64> {
    let t = s.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    let unit_ms: f64 = match t.chars().last()? {
        's' => 1000.0,
        'm' => 60_000.0,
        'h' => 3_600_000.0,
        'd' => 86_400_000.0,
        'w' => 604_800_000.0,
        _ => 1000.0, // bare number: seconds
    };
    let num: String = t
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    let n: f64 = num.parse().ok()?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    Some((n * unit_ms) as i64)
}

/// The instant a `delay`/`wait` step becomes due. Stored on the instance step's
/// `due_date` so the background worker (src/execution_worker.rs) knows when to
/// advance it — before this, nothing wrote that column and nothing read it.
pub fn delay_due_at(config: &Value, now: DateTime<Utc>) -> DateTime<Utc> {
    now + chrono::Duration::milliseconds(delay_duration_ms(config))
}

/// A settle request for ONE instance step.
#[derive(Debug, Clone, Copy)]
pub struct AdvanceRequest {
    pub step_instance_id: Uuid,
    pub decision: Decision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Settle the step as completed: the delay's timer fired, or a human approved.
    Approve,
    /// Settle the step as failed: a human rejected the gate.
    Reject,
}

/// What the walk knows about the steps this instance already has. Empty for a
/// fresh run, populated by `resume_instance`.
#[derive(Debug, Default)]
struct ResumePlan {
    /// sort_order -> (instance-step id, status) of every row already written.
    existing: std::collections::HashMap<i32, (Uuid, String)>,
    /// The one step this walk is allowed to settle now.
    advance: Option<AdvanceRequest>,
    /// Who is settling it: "worker" (timer) or the user id (human decision).
    actor: String,
}

impl ResumePlan {
    fn fresh() -> Self {
        Self {
            existing: std::collections::HashMap::new(),
            advance: None,
            actor: "engine".to_string(),
        }
    }
}

/// Find or create a system client for automated instances, so a run never
/// needs a human-created client_id (`workflow_instances.client_id` is NOT NULL).
pub async fn find_or_create_system_client(
    db: &PgPool,
    aid: Uuid,
    source: &str,
) -> Result<Uuid, AppError> {
    let sys_email = format!("incoming+{}@workflowswift.local", source);

    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM clients WHERE email = $1 AND aid = $2")
            .bind(&sys_email)
            .bind(aid)
            .fetch_optional(db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

    if let Some(existing_id) = existing {
        return Ok(existing_id);
    }

    let client_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO clients (id, aid, name, email, is_active)
           VALUES ($1, $2, $3, $4, true)
           ON CONFLICT (id) DO NOTHING"#,
    )
    .bind(client_id)
    .bind(aid)
    .bind(format!("System ({})", source))
    .bind(&sys_email)
    .execute(db)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to create system client: {}", e)))?;

    Ok(client_id)
}

pub async fn load_steps(db: &PgPool, workflow_id: Uuid) -> Result<Vec<StepRow>, AppError> {
    sqlx::query_as::<_, StepRow>(
        r#"SELECT id, step_type, name, config, sort_order, integration_target_id
           FROM workflow_steps
           WHERE workflow_id = $1
           ORDER BY sort_order ASC"#,
    )
    .bind(workflow_id)
    .fetch_all(db)
    .await
    .map_err(|e| AppError::Internal(format!("DB error: {}", e)))
}

/// Insert the instance row. `status` is 'active' for capture-path instances and
/// 'running' for user-initiated ones.
pub async fn create_instance(
    db: &PgPool,
    workflow_id: Uuid,
    aid: Uuid,
    client_id: Uuid,
    name: &str,
    status: &str,
) -> Result<Uuid, AppError> {
    let instance_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO workflow_instances (id, workflow_id, client_id, aid, name, status, current_step_order)
           VALUES ($1, $2, $3, $4, $5, $6, 0)"#,
    )
    .bind(instance_id)
    .bind(workflow_id)
    .bind(client_id)
    .bind(aid)
    .bind(name)
    .bind(status)
    .execute(db)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to create workflow instance: {}", e)))?;

    Ok(instance_id)
}

/// Walk every step of `instance_id` in this process and finalise the instance.
pub async fn execute_steps(
    state: &AppState,
    aid: Uuid,
    workflow_id: Uuid,
    instance_id: Uuid,
    ctx: &StepContext,
) -> Result<ExecutionOutcome, AppError> {
    let steps = load_steps(&state.db, workflow_id).await?;
    walk(
        state,
        aid,
        workflow_id,
        instance_id,
        ctx,
        &steps,
        &ResumePlan::fresh(),
    )
    .await
}

/// Continue an instance that stopped on a step this process cannot run by
/// itself: a `delay`/`wait` whose timer has fired, or a `manual`/`approval`
/// gate a human just decided.
///
/// This is the SAME walk as `execute_steps` — one engine, no second executor
/// (kanban t_1ff4b916 item 1). It reloads the workflow's steps, REPLAYS the
/// instance's existing step rows without re-running them, settles the single
/// step named in `advance`, then executes whatever comes after it. A step that
/// already reached a terminal status is never executed twice, so a resume can
/// never double-send a webhook — and it never charges a credit: billing happens
/// once at trigger time, never here (item 3).
///
/// `actor` is who settled the step ("worker" for a timer, the user id for a
/// human decision); it is written into the run history.
pub async fn resume_instance(
    state: &AppState,
    instance_id: Uuid,
    advance: Option<AdvanceRequest>,
    actor: &str,
) -> Result<ExecutionOutcome, AppError> {
    let instance = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT workflow_id, aid FROM workflow_instances WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?
    .ok_or_else(|| AppError::NotFound("Instance not found".to_string()))?;

    let (workflow_id, aid) = instance;

    let rows = sqlx::query_as::<_, (i32, Uuid, String)>(
        "SELECT sort_order, id, status FROM workflow_instance_steps WHERE instance_id = $1",
    )
    .bind(instance_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

    let mut existing = std::collections::HashMap::new();
    for (sort_order, step_id, status) in rows {
        existing.insert(sort_order, (step_id, status));
    }

    let steps = load_steps(&state.db, workflow_id).await?;
    let ctx = recover_context(state, instance_id, aid, workflow_id).await;

    let plan = ResumePlan {
        existing,
        advance,
        actor: actor.to_string(),
    };

    walk(state, aid, workflow_id, instance_id, &ctx, &steps, &plan).await
}

/// Rebuild the run context for a resumed instance from the input the engine
/// logged for its first step: a resumed run must see the same lead/payload the
/// original run did, or the step behind the wait would behave differently from
/// the step in front of it.
async fn recover_context(
    state: &AppState,
    instance_id: Uuid,
    aid: Uuid,
    workflow_id: Uuid,
) -> StepContext {
    let first_input: Option<Value> = sqlx::query_scalar(
        r#"SELECT input_data FROM workflow_execution_logs
           WHERE instance_id = $1 ORDER BY sort_order ASC, created_at ASC LIMIT 1"#,
    )
    .bind(instance_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let logged = first_input.unwrap_or_else(|| json!({}));
    let as_text = |v: &Value| v.as_str().map(|s| s.to_string());

    let source = as_text(&logged["source"]).unwrap_or_else(|| "manual".to_string());
    let campaign_slug = as_text(&logged["campaign_slug"]).unwrap_or_else(|| "manual".to_string());
    let contact = logged.get("contact").cloned().unwrap_or_else(|| json!({}));
    let data = logged.get("data").cloned();
    let source_entry_id = as_text(&logged["source_entry_id"]);
    let context = match logged.get("context") {
        Some(c) if !c.is_null() => c.clone(),
        _ => json!({
            "source": source,
            "campaign_slug": campaign_slug,
            "aid": aid,
            "workflow_id": workflow_id,
            "resumed": true,
        }),
    };

    StepContext {
        source,
        campaign_slug,
        contact,
        data,
        source_entry_id,
        context,
    }
}

/// The shared walk.
async fn walk(
    state: &AppState,
    aid: Uuid,
    workflow_id: Uuid,
    instance_id: Uuid,
    ctx: &StepContext,
    steps: &[StepRow],
    plan: &ResumePlan,
) -> Result<ExecutionOutcome, AppError> {
    // Locals the lifted loop expects. `source`/`slug` keep their original names
    // so the step arms below stay identical to the proven inbound implementation.
    let source: &str = ctx.source.as_str();
    let slug: &str = ctx.campaign_slug.as_str();
    let contact: Value = ctx.contact.clone();
    let data: Option<Value> = ctx.data.clone();
    let context: Value = ctx.context.clone();
    let source_entry_id: Option<String> = ctx.source_entry_id.clone();

    // Execute each workflow step
    let mut step_results: Vec<Value> = vec![];

    for (i, step) in steps.iter().enumerate() {
        let step_instance_id = Uuid::new_v4();
        let step_type = &step.step_type;
        let step_config = step.config.clone().unwrap_or(json!({}));
        let step_started = std::time::Instant::now();

        // ── Resume path: a step this instance already has is REPLAYED, not re-run ──
        // (kanban t_1ff4b916). Only `resume_instance` populates `plan.existing`;
        // a fresh run never enters this block.
        if let Some((prior_id, prior_status)) = plan.existing.get(&(i as i32)).cloned() {
            match prior_status.as_str() {
                "completed" | "skipped" => {
                    step_results.push(json!({
                        "step": i, "type": step_type, "status": "completed",
                        "note": "already executed on an earlier run — not run again",
                    }));
                    continue;
                }
                "failed" => {
                    step_results.push(json!({
                        "step": i, "type": step_type, "status": "failed",
                        "note": "already failed on an earlier run",
                    }));
                    continue;
                }
                _ => {}
            }

            // pending / in_progress: exactly the step a resume may settle, and
            // only when this call named it.
            let decision = match plan.advance.filter(|a| a.step_instance_id == prior_id) {
                Some(a) => a.decision,
                None => {
                    step_results.push(json!({
                        "step": i, "type": step_type, "status": "pending",
                        "note": "left pending by this call",
                    }));
                    continue;
                }
            };

            let ok = decision == Decision::Approve;
            let new_status = if ok { "completed" } else { "failed" };
            let note = json!({
                "step": i,
                "type": step_type,
                "status": new_status,
                "settled_by": plan.actor,
                "decision": if ok { "approve" } else { "reject" },
                "at": Utc::now().to_rfc3339(),
            });
            let err_text: Option<String> = if ok {
                None
            } else {
                Some(format!("{} rejected this step", plan.actor))
            };

            sqlx::query(
                r#"UPDATE workflow_instance_steps
                   SET status = $1, completed_at = NOW(), notes = $2
                   WHERE id = $3"#,
            )
            .bind(new_status)
            .bind(note.to_string())
            .bind(prior_id)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

            // Close the log row the engine left open for this step, so the run
            // history shows the wait/gate ENDING, not just starting.
            sqlx::query(
                r#"UPDATE workflow_execution_logs
                   SET status = $1, output_data = $2, error_message = $3,
                       duration_ms = GREATEST(0, (EXTRACT(EPOCH FROM (NOW() - COALESCE(started_at, NOW()))) * 1000))::int,
                       completed_at = NOW()
                   WHERE instance_id = $4 AND sort_order = $5
                     AND status IN ('pending', 'running', 'in_progress')"#,
            )
            .bind(new_status)
            .bind(&note)
            .bind(err_text)
            .bind(instance_id)
            .bind(i as i32)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

            tracing::info!(
                instance_id = %instance_id,
                step_order = i,
                step_type = %step_type,
                decision = if ok { "approve" } else { "reject" },
                actor = %plan.actor,
                "instance step settled"
            );

            step_results.push(note);

            if !ok {
                // A rejected gate fails the run: nothing behind it may execute.
                break;
            }
            continue;
        }

        // Create the instance step record. A `delay`/`wait` stores WHEN it is due
        // on the row itself — the background worker selects on exactly that, and
        // nothing wrote the column before this.
        let step_due: Option<DateTime<Utc>> = if step_type == "delay" || step_type == "wait" {
            Some(delay_due_at(&step_config, Utc::now()))
        } else {
            None
        };

        sqlx::query(
                r#"INSERT INTO workflow_instance_steps (id, instance_id, step_type, name, sort_order, status, due_date)
                   VALUES ($1, $2, $3, $4, $5, 'in_progress', $6)"#,
            )
            .bind(step_instance_id)
            .bind(instance_id)
            .bind(step_type)
            .bind(&step.name)
            .bind(i as i32)
            .bind(step_due)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

        // Execution trace: one log row per step attempt, opened here and closed
        // after the arm runs. This is the run history the spec asks for
        // (kanban t_a6a1b299 item 2).
        let log_id = Uuid::new_v4();
        sqlx::query(
                r#"INSERT INTO workflow_execution_logs
                   (id, instance_id, workflow_id, step_id, step_type, step_name, sort_order, status, input_data, started_at)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, 'running', $8, NOW())"#,
            )
            .bind(log_id)
            .bind(instance_id)
            .bind(workflow_id)
            .bind(step.id)
            .bind(step_type)
            .bind(&step.name)
            .bind(i as i32)
            .bind(json!({
                "config": step_config,
                "source": source,
                "campaign_slug": slug,
                "contact": contact,
                "data": data,
                "source_entry_id": source_entry_id,
                // The run context is logged so a resumed run can rebuild it
                // (resume_instance -> recover_context) instead of inventing one.
                "context": context,
            }))
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

        // Update current step on instance
        sqlx::query(
                "UPDATE workflow_instances SET current_step_order = $1, updated_at = NOW() WHERE id = $2"
            )
            .bind(i as i32 + 1)
            .bind(instance_id)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

        let result = match step_type.as_str() {
            "integration_dispatch" | "integration" => {
                // Dispatch to the configured integration target using stored API key
                if let Some(target_id) = step.integration_target_id {
                    let dispatch_payload = json!({
                        "contact": contact,
                        "data": data,
                        "source": source,
                        "campaign_slug": slug,
                        "source_entry_id": source_entry_id,
                        "context": context,
                    });

                    match crate::handlers::integration_dispatch_handler::forward_dispatch(
                        &state.db,
                        target_id,
                        aid,
                        &dispatch_payload,
                    )
                    .await
                    {
                        Ok(resp) => {
                            json!({
                                "step": i,
                                "type": "integration_dispatch",
                                "status": "completed",
                                "target_id": target_id.to_string(),
                                "response": resp,
                            })
                        }
                        Err(e) => {
                            tracing::warn!("Integration dispatch step {} failed: {}", i, e);
                            json!({
                                "step": i,
                                "type": "integration_dispatch",
                                "status": "error",
                                "error": e,
                            })
                        }
                    }
                } else {
                    json!({
                        "step": i,
                        "type": "integration_dispatch",
                        "status": "skipped",
                        "reason": "No integration_target_id set on step",
                    })
                }
            }
            // `webhook` keeps its shipped POST; `http-request` / `action` are the same call with the
            // method the console's Builder gives them. All three take the tenant's URL, so all three
            // run through the destination gate (kanban t_fe60cdf5) — the n8n mirror already emits a
            // real HttpRequest node for every one of these names.
            "webhook" | "http-request" | "action" => {
                let url = step_config
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if url.is_empty() {
                    json!({"step": i, "type": step_type, "status": "skipped", "reason": "No URL configured"})
                } else {
                    let method = if step_type == "webhook" {
                        "POST".to_string()
                    } else {
                        step_config
                            .get("method")
                            .and_then(|v| v.as_str())
                            .unwrap_or("GET")
                            .to_uppercase()
                    };
                    let payload = json!({
                        "contact": contact,
                        "data": data,
                        "source": source,
                        "campaign_slug": slug,
                        "source_entry_id": source_entry_id,
                    });
                    match step_outbound_call(&method, url, &payload, 15).await {
                        Ok((status, body)) => json!({
                            "step": i, "type": step_type, "status": status,
                            "method": method, "url": url, "response": body,
                        }),
                        Err(e) => json!({
                            "step": i, "type": step_type, "status": "error",
                            "method": method, "url": url, "error": e,
                        }),
                    }
                }
            }
            // `render_*`: call the tenant's provider endpoint, then write the rendition row the
            // console promises. The endpoint is the tenant's own provider URL — the same one the
            // n8n mirror's node posts to — so it runs through the same destination gate.
            //
            // Bounded at 60s because a Run is a synchronous HTTP request; a provider that only
            // accepts a job and answers later still logs its job id as the asset.
            "render_video" | "render_image" | "render_audio" | "render_media" => {
                let provider = step_config
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let endpoint = step_config
                    .get("endpoint")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let asset_type = match step_type.as_str() {
                    "render_image" => "image",
                    "render_audio" => "audio",
                    _ => "video",
                };
                if provider.is_empty() || endpoint.is_empty() {
                    json!({"step": i, "type": step_type, "status": "skipped",
                           "reason": "A render step needs both `provider` and `endpoint`"})
                } else {
                    let payload = json!({
                        "provider": provider,
                        "asset_type": asset_type,
                        "step_name": step.name,
                        "contact": contact,
                        "data": data,
                        "source": source,
                        "campaign_slug": slug,
                        "source_entry_id": source_entry_id,
                        "context": context,
                    });
                    match step_outbound_call("POST", endpoint, &payload, 60).await {
                        Err(e) => json!({"step": i, "type": step_type, "status": "error",
                                         "provider": provider, "endpoint": endpoint, "error": e}),
                        Ok((code, body)) if !(200..300).contains(&code) => json!({
                            "step": i, "type": step_type, "status": "error",
                            "provider": provider, "endpoint": endpoint, "status_code": code,
                            "response": truncate_for_log(&body, 500),
                            "error": format!("the render provider answered HTTP {}", code)}),
                        Ok((code, body)) => match record_rendition(
                            state,
                            RenditionRequest {
                                aid,
                                context: &context,
                                workflow_id,
                                instance_id,
                                step_type,
                                step_name: &step.name,
                                provider,
                                asset_type,
                                body: &body,
                            },
                        )
                        .await
                        {
                            Ok(rendition_id) => json!({
                                "step": i, "type": step_type, "status": "completed",
                                "provider": provider, "asset_type": asset_type,
                                "status_code": code,
                                "rendition_id": rendition_id.to_string()}),
                            Err(e) => json!({"step": i, "type": step_type, "status": "error",
                                             "provider": provider, "endpoint": endpoint, "error": e}),
                        },
                    }
                }
            }
            "n8n" | "n8n_workflow" => {
                let workflow_id = step_config.get("n8n_workflow_id").and_then(|v| v.as_str());
                let n8n_url = &state.config.n8n_webhook_url;
                let n8n_api_key = &state.config.n8n_api_key;

                if let Some(wf_id) = workflow_id {
                    let n8n_payload = json!({
                        "workflow_id": wf_id,
                        "data": {
                            "contact": contact,
                            "campaign_slug": slug,
                            "source": source,
                            "source_entry_id": source_entry_id,
                        }
                    });

                    let webhook_url = format!(
                        "{}/webhook/incoming/{}",
                        n8n_url.trim_end_matches('/'),
                        wf_id
                    );
                    let client = reqwest::Client::new();
                    let mut req = client.post(&webhook_url).json(&n8n_payload);
                    if !n8n_api_key.is_empty() {
                        req = req.header("X-API-Key", n8n_api_key);
                    }

                    match req.send().await {
                        Ok(resp) => {
                            let status_code = resp.status().as_u16();
                            let body = resp.text().await.unwrap_or_default();
                            json!({"step": i, "type": "n8n", "status": status_code, "n8n_workflow_id": wf_id, "response": body})
                        }
                        Err(e) => {
                            json!({"step": i, "type": "n8n", "status": "error", "error": e.to_string()})
                        }
                    }
                } else {
                    json!({"step": i, "type": "n8n", "status": "skipped", "reason": "No n8n_workflow_id in step config"})
                }
            }
            "manual" | "approval" => {
                // Manual step — pending until a human decides. The decision
                // endpoint (POST /instances/{id}/steps/{step_id}/decision)
                // resumes the run from exactly here (kanban t_1ff4b916 item 2).
                json!({
                    "step": i,
                    "type": "manual",
                    "status": "pending",
                    "note": "Awaiting approve/reject — POST /api/v1/instances/{id}/steps/{step_id}/decision",
                })
            }
            "delay" | "wait" => {
                let due_at = delay_due_at(&step_config, Utc::now());
                json!({
                    "step": i,
                    "type": "delay",
                    "duration_ms": delay_duration_ms(&step_config),
                    "due_at": due_at.to_rfc3339(),
                    "status": "pending",
                    "note": "Waiting for its due time — the background worker advances it",
                })
            }
            "ai-action" | "ai_action" => {
                // The console OFFERS AI Action and collects the tenant's provider key (Provider
                // Keys panel). This arm IS the LLM path (kanban t_03e4d3d9) — before it, nothing
                // in this crate called a provider: the step POSTed a platform-shaped body to
                // `{N8N_WEBHOOK_URL}/webhook/workflowswift-generate` — not registered (404) — fell
                // back to `/webhook/incoming/content-gen` (also 404, measured live 2026-10-02) and
                // then reported `status: "completed"` carrying `generated_content: <the prompt>`:
                // a generation that never happened, wrapped around a webhook that never existed.
                //
                // BYOK, and the product decision is written down in `crate::ai_llm`: the credential
                // is the TENANT'S OWN key from `provider_keys` (the same mapping
                // `/integrations/resolve?step_type=ai-action` serves), so the call costs 0 credits
                // and spends no platform money; the destination is a CONSTANT per provider, never a
                // value out of step config, so no new SSRF surface is opened.
                //
                // Four outcomes, none of them a fabricated completion:
                //   * no/unknown provider, or a blank prompt -> `skipped` naming what is missing;
                //   * provider named but no key connected  -> `skipped` naming the Provider Keys row;
                //   * the provider answered                 -> the provider's HTTP status + its own
                //                                              message as `generated_content`;
                //   * transport failure / no message in a 2xx -> `error`, never `completed`.
                let prompt = step_config
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let named_provider = step_config
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let model = step_config.get("model").and_then(|v| v.as_str());

                ai_action_outcome(&state.db, aid, i, prompt, named_provider, model).await
            }

            // `transform`, `code` and `format` used to have an arm here, and it was the app's half of
            // the divergence kanban t_81602ca1 settled. It ran NO JavaScript: it read `format` /
            // `tone`, built a payload it dropped on the floor and answered `status: "completed"`
            // with the note "Formatting queued — will transform content for selected platform", so
            // the console recorded a transformation that never happened while the n8n mirror's own
            // `n8n-nodes-base.code` node (carrying the tenant's `config.code` verbatim) failed on
            // this install. Nothing in this app executes tenant JavaScript, so the three types are
            // RETIRED (see `RETIRED_STEP_TYPES`): the write path refuses them, and a row written
            // straight into the database falls through to `unexecutable_step_result` below — a
            // `skipped` step with a reason that reaches the run's `warnings[]`. Re-adding any of
            // them means shipping an executor for it, in THIS app, first.
            // `design` used to have an arm here, and it was the app's half of the divergence kanban
            // t_f10ada7a settled. It read `style` / `dimensions`, generated nothing at all — no
            // provider, no route, no row writer — and answered `status: "completed"` with the note
            // "Design queued — will generate visual assets via configured provider", so the console
            // recorded a Design step as done and the run came back `completed` with an EMPTY
            // `warnings[]` (measured live 2026-10-02). Nothing in this app renders a design, and
            // `design` was never offered by the console picker, so it is RETIRED (see
            // `RETIRED_STEP_TYPES`): the write path refuses it, and a row written straight into the
            // database falls through to `unexecutable_step_result` below — a `skipped` step with a
            // reason that reaches the run's `warnings[]`. Re-adding it means shipping a design
            // executor (which provider, which key, what it costs) in THIS app, first.
            // `publish` and `export` used to have arms here. Both POSTed a platform-shaped body to
            // a webhook that is not registered — `{N8N_WEBHOOK_URL}/webhook/workflowswift-publish`
            // and `…/webhook/workflowswift-export` — and `export` (the CONSOLE-offered one)
            // returned `status: "completed"` for the 404 AND for a transport failure, so a tenant's
            // Export step was shown as completed having exported nothing (measured live 2026-10-02,
            // kanban t_02519738). No exporter exists to point them at: CSV has no store or download
            // surface in this app, Resend/SendGrid are credentials the platform does not hold, and
            // the CoreSwift CRM path is an inbound lead push. Both step types are RETIRED
            // (`RETIRED_STEP_TYPES`): the write path refuses them, the console no longer offers
            // `export`, and a row written straight into the database reaches the `_` arm below as
            // an honest `skipped` step whose `unexecutable` marker lands in the run's `warnings[]`.
            "notify" => {
                let channel = step_config
                    .get("channel")
                    .and_then(|v| v.as_str())
                    // `webhook` is the one channel NOTIFY_CHANNELS accepts; a step stored without a
                    // channel (or with a retired one, e.g. the old `email` default) must not fall
                    // back to a channel the product cannot deliver on.
                    .unwrap_or("webhook");
                let recipient = step_config
                    .get("recipient")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let message = step_config
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // The `webhook` channel is an OUTBOUND POST of `{message, data}` to the URL the
                // tenant configured as the step's Recipient — the console's own words ("POSTs the
                // run data to a webhook URL you own", www-app/index.html) and the exact call the
                // n8n mirror emits (src/n8n_converter.rs, notify arm). It is NOT a platform
                // endpoint.
                //
                // This arm used to POST a platform-shaped body
                // (`{action, channel, recipient, subject, message, context, contact, data}`) to
                // `{N8N_WEBHOOK_URL}/webhook/workflowswift-notify` — a webhook that is not
                // registered (404) — and returned `status: "completed"` for ANY outcome, 404 and
                // transport error included (measured live 2026-10-02, kanban t_e0e6a42e: the step
                // reported completed, the run reported completed, and the tenant's own URL was
                // never called). A retired channel or a blank URL is not a completed step either.
                let senders = crate::notify::load_senders(state).await;
                if !is_notify_channel(channel) || RETIRED_NOTIFY_CHANNELS.contains(&channel) {
                    json!(notify_undeliverable_result(
                        i,
                        channel,
                        &format!(
                            "Notify channel '{}' has no sender in this app (available channels: \
                             {}; retired: {})",
                            channel,
                            notify_channels_for(&senders),
                            retired_notify_channel_list()
                        )
                    ))
                } else if channel != "webhook" && !notify_channel_available(channel, &senders) {
                    // FAIL-CLOSED: the channel exists in the product but THIS install has no
                    // sender for it. Nothing is sent and the step names exactly what is missing —
                    // a channel that cannot be delivered is never recorded as completed.
                    json!(notify_undeliverable_result(
                        i,
                        channel,
                        &format!(
                            "Notify channel '{}' is not configured on this install — set the \
                             provider in Admin > Settings > {}, or this step sends nothing",
                            channel,
                            if channel == "sms" { "SMS" } else { "Email" }
                        )
                    ))
                } else if channel != "webhook" {
                    // `email` | `sms`: the APP sends, through its own panel-configured provider,
                    // to the account's OWN people only. `crate::notify` resolves the recipient,
                    // refuses anything that is not one of the account's own people, applies the
                    // per-account hourly cap and records the outcome — the same code the mirror's
                    // `POST /api/v1/notify/dispatch` route runs, so the two doors cannot drift.
                    crate::notify::deliver(
                        state,
                        aid,
                        Some(workflow_id),
                        Some(i as i32),
                        channel,
                        &step_config,
                    )
                    .await
                    .to_step_json(i)
                } else if recipient.is_empty() {
                    json!(notify_undeliverable_result(
                        i,
                        channel,
                        "Notify step has a blank Webhook URL, so it sent nothing"
                    ))
                } else {
                    // The tenant's URL runs through the same SSRF destination gate as every other
                    // step whose target comes from step config — this arm never ran it before,
                    // because it never called the tenant's URL at all.
                    let payload = json!({ "message": message, "data": data });
                    let call = step_outbound_call("POST", recipient, &payload, 15).await;
                    notify_step_result(i, channel, recipient, call)
                }
            }
            "data-card" | "data_card" => {
                // Pull data from dashboard and attach to context.
                //
                // `metric_key` is a widget's `config.metric_key`, so it is resolved through the SAME
                // key space every writer uses (`metric_key_candidates`): a bare config key resolves to
                // its canonical `n8n_` row, and an already-prefixed config key keeps the legacy
                // double-prefixed row readable. The raw equality lookup this arm used to do matched
                // NEITHER shape, so a Data Card added from the Builder's own widget picker (which
                // stores `config.metric_key` verbatim) silently produced an empty card.
                let widget_name = step_config
                    .get("widget_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let metric_key = step_config
                    .get("metric_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let lookup: Result<Option<Value>, sqlx::Error> = if metric_key.is_empty() {
                    Ok(None)
                } else {
                    let lookup_keys = metric_key_candidates(metric_key);
                    latest_widget_metric(&state.db, aid, &lookup_keys)
                        .await
                        .map_err(|e| {
                            tracing::error!(
                                error = %e,
                                %aid,
                                metric_keys = ?lookup_keys,
                                "dashboard data-card lookup failed — failing the step instead of \
                                 returning an empty card"
                            );
                            e
                        })
                };

                data_card_result(i, widget_name, metric_key, lookup)
            }
            "fork" | "branch" => {
                let branches: Vec<serde_json::Value> = step_config
                    .get("branches")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                json!({"step": i, "type": "fork", "status": "completed", "branches": branches.len(), "note": "Workflow will fork into parallel branches"})
            }
            "loop" => {
                let iterations = step_config
                    .get("iterations")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                json!({"step": i, "type": "loop", "status": "completed", "iterations": iterations, "note": format!("Will loop {} times or until condition met", iterations)})
            }
            "condition" | "ifelse" => {
                let condition = step_config
                    .get("condition")
                    .and_then(|v| v.as_str())
                    .unwrap_or("true");
                json!({"step": i, "type": "condition", "status": "completed", "condition": condition, "note": "Condition evaluation queued"})
            }
            _ => unexecutable_step_result(i, step_type),
        };

        // Mark step instance as completed (or error)
        let step_status = classify_step_status(&result);
        let completed_at = if step_status == "completed" {
            Some(Utc::now())
        } else {
            None
        };
        let duration_ms = step_started.elapsed().as_millis() as i32;

        sqlx::query(
                r#"UPDATE workflow_instance_steps SET status = $1, completed_at = $2, notes = $3 WHERE id = $4"#,
            )
            .bind(&step_status)
            .bind(completed_at)
            .bind(result.to_string())
            .bind(step_instance_id)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

        // Close the log row: the trace now says what actually happened, with the
        // upstream status code and the reason when it did not succeed.
        sqlx::query(
                r#"UPDATE workflow_execution_logs
                   SET status = $1, output_data = $2, error_message = $3, duration_ms = $4, completed_at = NOW()
                   WHERE id = $5"#,
            )
            .bind(&step_status)
            .bind(&result)
            .bind(step_error_text(&result))
            .bind(duration_ms)
            .bind(log_id)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

        step_results.push(result);

        // A waiting step holds everything behind it: a `delay`/`wait` is waiting
        // for its timer and a `manual`/`approval` for a human. Running the steps
        // behind the wait immediately (what this loop used to do) made a Wait step
        // meaningless. The worker / the decision endpoint resumes from exactly here.
        if step_status == "pending" || step_status == "in_progress" {
            tracing::info!(
                instance_id = %instance_id,
                step_order = i,
                step_type = %step_type,
                status = %step_status,
                "walk stopped at a waiting step — the rest runs when it is settled"
            );
            break;
        }
    }

    // Finalise the instance from what the steps actually did.
    //
    // A `pending`/`in_progress` STEP means the walk stopped at a step that is
    // settled by something else: a `delay`/`wait` by its due time (the
    // execution worker in `src/execution_worker.rs`) and a `manual`/`approval`
    // by a human calling the decision endpoint. That is a run that is still
    // GOING, so the instance is `in_progress` — not `completed` (which was the
    // old lie) and not `pending` either, because `pending` reads as "nothing
    // happened" when in fact every runnable step has already executed and a
    // timer/decision is now in flight. `completed_at` stays NULL either way:
    // only a terminal state stamps it.
    let mut pending_steps: i32 = 0;
    let mut failed_steps: i32 = 0;
    for r in &step_results {
        match classify_step_status(r).as_str() {
            "failed" => failed_steps += 1,
            "pending" | "in_progress" => pending_steps += 1,
            _ => {}
        }
    }

    let instance_status = if failed_steps > 0 {
        "failed"
    } else if pending_steps > 0 {
        "in_progress"
    } else {
        "completed"
    };

    let terminal = instance_status == "completed" || instance_status == "failed";

    sqlx::query(
            "UPDATE workflow_instances SET status = $1, completed_at = $2, updated_at = NOW() WHERE id = $3"
        )
        .bind(instance_status)
        .bind(if terminal { Some(Utc::now()) } else { None })
        .bind(instance_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("DB error: {}", e)))?;

    Ok(ExecutionOutcome {
        steps: step_results,
        status: instance_status.to_string(),
        pending_steps,
        failed_steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numeric_status_codes_are_classified_not_defaulted() {
        // These three arms store an HTTP code in `status`; reading it with as_str()
        // used to fall through to "completed".
        assert_eq!(classify_step_status(&json!({"status": 200})), "completed");
        assert_eq!(classify_step_status(&json!({"status": 302})), "completed");
        assert_eq!(classify_step_status(&json!({"status": 404})), "failed");
        assert_eq!(classify_step_status(&json!({"status": 503})), "failed");
        // Named statuses keep their meaning.
        assert_eq!(
            classify_step_status(&json!({"status": "pending"})),
            "pending"
        );
        assert_eq!(classify_step_status(&json!({"status": "error"})), "failed");
        assert_eq!(
            classify_step_status(&json!({"status": "skipped"})),
            "skipped"
        );
        assert_eq!(classify_step_status(&json!({})), "completed");
    }

    #[test]
    fn error_text_prefers_the_reason_and_falls_back_to_the_code() {
        assert_eq!(
            step_error_text(&json!({"error": "boom"})).as_deref(),
            Some("boom")
        );
        assert_eq!(
            step_error_text(&json!({"reason": "No URL configured"})).as_deref(),
            Some("No URL configured")
        );
        assert_eq!(
            step_error_text(&json!({"status": 404})).as_deref(),
            Some("upstream returned HTTP 404")
        );
        assert_eq!(step_error_text(&json!({"status": "completed"})), None);
        // A SUCCESS code is not an error (kanban t_02519738): every successful outbound step used
        // to carry `error_message = "upstream returned HTTP 200"` in its log row.
        assert_eq!(step_error_text(&json!({"status": 200})), None);
        assert_eq!(step_error_text(&json!({"status": 204})), None);
        assert_eq!(step_error_text(&json!({"status": 302})), None);
        assert_eq!(
            step_error_text(&json!({"status": 500})).as_deref(),
            Some("upstream returned HTTP 500")
        );
    }

    /// kanban t_02519738: `export` (the console-offered arm), `publish` and `generate` addressed a
    /// platform webhook that is not registered and reported an outcome the app never produced.
    ///
    /// The control leg is the PRE-FIX shape: a 404 body from `/webhook/workflowswift-export`
    /// wrapped with `status: "completed"`, exactly what the running binary returned and the walk
    /// recorded as success. The retired types now reach the walk's `_` arm.
    #[test]
    fn a_retired_step_type_is_not_a_completed_step() {
        let body = "{\"code\":404,\"message\":\"The requested webhook \\\"POST \
                    workflowswift-export\\\" is not registered.\"}";
        let control = json!({
            "step": 1, "type": "export", "status": "completed", "format": "csv",
            "targets": ["coreswift"], "response": body,
        });
        assert_eq!(
            classify_step_status(&control),
            "completed",
            "the pre-fix export arm reported a 404 as a completed Export step"
        );

        for retired in RETIRED_STEP_TYPES {
            let result = unexecutable_step_result(1, retired);
            assert_eq!(
                classify_step_status(&result),
                "skipped",
                "'{retired}' must not read as completed ({result})"
            );
            assert_eq!(
                result.get("unexecutable").and_then(|v| v.as_str()),
                Some(*retired),
                "the walk's marker must NAME the retired type so the run warning can"
            );
            assert!(
                result
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .contains("has no executor in this app"),
                "{result}"
            );
            assert!(
                step_error_text(&result)
                    .unwrap_or_default()
                    .contains("has no executor"),
                "the reason reaches the execution log row"
            );
        }
    }

    /// kanban t_03e4d3d9: the arm used to report `completed` carrying
    /// `generated_content: <the prompt>` — a generation that never happened. The first block is
    /// that PRE-FIX shape as a control; every shape the arm can now produce is asserted below, and
    /// none of them claims content nobody generated.
    #[test]
    fn an_ai_action_step_never_claims_content_nobody_generated() {
        let control = json!({
            "step": 1, "type": "generate", "status": "completed", "provider": "openai",
            "note": "AI generation queued (n8n not available: connect error)",
            "generated_content": "Summarise the incoming lead",
        });
        assert_eq!(
            classify_step_status(&control),
            "completed",
            "the pre-fix arm claimed a generation that never happened"
        );

        // No key connected -> `skipped`, naming the provider and where to connect it.
        let no_key =
            ai_action_undeliverable_result(1, "deepseek", &ai_action_no_key_reason("deepseek"));
        assert_eq!(classify_step_status(&no_key), "skipped");
        assert!(no_key.get("generated_content").is_none());
        let reason = no_key
            .get("undeliverable")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            reason.contains("deepseek"),
            "the reason names the provider: {reason}"
        );
        assert!(reason.contains("Provider Keys"), "and the fix: {reason}");
        assert_eq!(step_error_text(&no_key).as_deref(), Some(reason));

        // A provider this app cannot call -> `skipped` naming the whole vocabulary.
        let unknown =
            ai_action_undeliverable_result(1, "mystery", &ai_action_no_provider_reason("mystery"));
        assert_eq!(classify_step_status(&unknown), "skipped");
        assert!(unknown
            .get("undeliverable")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .contains("openai, anthropic, deepseek, gemini"));

        // The provider ANSWERED: its status code is kept and `generated_content` is its own message.
        let answered = ai_action_step_result(
            1,
            "deepseek",
            "deepseek-chat",
            Ok((
                200,
                r#"{"choices":[{"message":{"content":"a real answer"}}]}"#.to_string(),
            )),
        );
        assert_eq!(classify_step_status(&answered), "completed");
        assert_eq!(answered.get("generated_content").unwrap(), "a real answer");

        // A 401 from the provider is a FAILED step, never a completion.
        let refused = ai_action_step_result(
            1,
            "openai",
            "gpt-4o-mini",
            Ok((401, r#"{"error":{"message":"invalid key"}}"#.to_string())),
        );
        assert_eq!(classify_step_status(&refused), "failed");
        assert!(refused.get("generated_content").is_none());
        assert_eq!(
            step_error_text(&refused).as_deref(),
            Some("upstream returned HTTP 401")
        );

        // A transport failure -> `error`, naming why.
        let transport = ai_action_step_result(
            1,
            "openai",
            "gpt-4o-mini",
            Err("could not reach OpenAI: connect error".to_string()),
        );
        assert_eq!(classify_step_status(&transport), "failed");
        assert!(transport
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .contains("could not reach"));

        // A 2xx whose body carries no message is an ERROR, not a completed step with nothing in it.
        let empty = ai_action_step_result(
            1,
            "gemini",
            "gemini-2.0-flash",
            Ok((200, r#"{"candidates":[]}"#.to_string())),
        );
        assert_eq!(classify_step_status(&empty), "failed");
        assert!(empty.get("generated_content").is_none());
    }

    #[test]
    fn delay_config_shapes_all_resolve_to_a_due_time() {
        // Canonical key (the n8n converter's wait node and the step validator
        // both use duration_ms), the human string older workflows carry, and the
        // bare-second form. None of these could be read by anything before.
        assert_eq!(delay_duration_ms(&json!({"duration_ms": 5000})), 5_000);
        assert_eq!(delay_duration_ms(&json!({"duration": "5s"})), 5_000);
        assert_eq!(delay_duration_ms(&json!({"duration": "30m"})), 1_800_000);
        assert_eq!(delay_duration_ms(&json!({"duration": "1h"})), 3_600_000);
        assert_eq!(delay_duration_ms(&json!({"duration": "2d"})), 172_800_000);
        assert_eq!(delay_duration_ms(&json!({"duration": "90"})), 90_000);
        assert_eq!(delay_duration_ms(&json!({"duration_seconds": 5})), 5_000);
        // Nothing usable in the config keeps the engine's original 1h default.
        assert_eq!(delay_duration_ms(&json!({})), 3_600_000);
        assert_eq!(delay_duration_ms(&json!({"duration": "soon"})), 3_600_000);
        assert_eq!(delay_duration_ms(&json!({"duration_ms": 0})), 3_600_000);

        let now = Utc::now();
        let due = delay_due_at(&json!({"duration": "5s"}), now);
        assert_eq!((due - now).num_seconds(), 5);
    }

    #[test]
    fn a_reject_decision_is_the_only_thing_that_settles_a_gate_as_failed() {
        // Pins the mapping the walk uses: approve -> completed, reject -> failed.
        assert!(Decision::Approve != Decision::Reject);
        let plan = ResumePlan::fresh();
        assert!(plan.advance.is_none());
        assert!(plan.existing.is_empty());
    }

    #[test]
    fn a_failing_data_card_lookup_is_a_failed_step_not_an_empty_card() {
        let broken = data_card_result(
            1,
            "Trends",
            "site-flipping_trends",
            Err(sqlx::Error::RowNotFound),
        );
        assert_eq!(classify_step_status(&broken), "failed");
        assert_eq!(
            step_error_text(&broken)
                .unwrap_or_default()
                .starts_with("dashboard series lookup failed"),
            true,
            "the reason must reach the execution log: {broken}"
        );

        // A widget nobody has pushed to stays a successful, legitimately empty card.
        let empty = data_card_result(1, "Trends", "site-flipping_trends", Ok(None));
        assert_eq!(classify_step_status(&empty), "completed");
        assert!(empty.get("metric_value").unwrap().is_null());
        assert_eq!(
            empty.get("resolved_metric_key").unwrap(),
            "n8n_site-flipping_trends",
            "the result names the canonical key the read used"
        );
        assert_eq!(
            empty.get("metric_key").unwrap(),
            "site-flipping_trends",
            "and still echoes the key the step was configured with"
        );

        // A series that IS there comes back in the step result.
        let found = data_card_result(
            1,
            "Trends",
            "n8n_site-flipping_trends",
            Ok(Some(json!({"value": 42}))),
        );
        assert_eq!(classify_step_status(&found), "completed");
        assert_eq!(found.get("metric_value").unwrap()["value"], json!(42));
    }

    /// Executed, not asserted on paper: a lazy pool pointed at a closed port makes the exact query
    /// the `data-card` arm now runs fail, and the result is classified the way the walk classifies
    /// it. The control leg is the exact pre-fix shape — the same failing query with
    /// `.unwrap_or(None)` — which the walk recorded as a *completed* step with a null metric.
    #[tokio::test]
    async fn a_broken_lookup_fails_the_step_while_the_pre_fix_shape_looked_like_success() {
        let db = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(500))
            .connect_lazy("postgres://probe:***@127.0.0.1:1/none")
            .expect("lazy pool");
        let lookup_keys = metric_key_candidates("site-flipping_trends");

        let lookup = latest_widget_metric(&db, Uuid::nil(), &lookup_keys).await;
        assert!(
            lookup.is_err(),
            "a query that cannot run must be an error, got {lookup:?}"
        );
        let result = data_card_result(1, "Trends", "site-flipping_trends", lookup);
        assert_eq!(classify_step_status(&result), "failed");
        assert!(step_error_text(&result)
            .unwrap_or_default()
            .starts_with("dashboard series lookup failed"));

        // control: the pre-fix read, on the same failing query, produced a null metric that the walk
        // reported as a completed step — indistinguishable from a widget nobody had pushed to.
        let swallowed: Option<Value> = sqlx::query_scalar(
            r#"SELECT metric_value FROM dashboard_data WHERE aid = $1 AND metric_key = $2 ORDER BY recorded_at DESC LIMIT 1"#,
        )
        .bind(Uuid::nil())
        .bind("site-flipping_trends")
        .fetch_optional(&db)
        .await
        .unwrap_or(None);
        assert_eq!(swallowed, None, "the pre-fix shape hid the failure");
        let control = json!({"step": 1, "type": "data-card", "status": "completed", "metric_value": swallowed});
        assert_eq!(
            classify_step_status(&control),
            "completed",
            "and the walk called that success"
        );
    }

    /// The step types the engine's `match step_type.as_str()` branches on, read out of THIS file's
    /// own source so the list cannot drift from the code. Arm lines sit at twelve spaces of
    /// indentation; every nested match (e.g. render_*'s asset_type) is deeper and is excluded.
    fn engine_arm_keys() -> Vec<String> {
        let src = include_str!("execution.rs");
        let start = src
            .find("let result = match step_type.as_str() {")
            .expect("the engine's match is in this file");
        let end = src[start..]
            .find("\n        };\n")
            .map(|off| start + off)
            .expect("the end of the match block");
        let mut keys: Vec<String> = Vec::new();
        for line in src[start..end].lines() {
            if !line.starts_with("            \"") || !line.contains("=>") {
                continue;
            }
            for part in line.split('"').skip(1).step_by(2) {
                let key = part.trim();
                if !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_' || c == '-')
                {
                    keys.push(key.to_string());
                }
            }
        }
        keys.sort();
        keys.dedup();
        keys
    }

    /// The card's core invariant (kanban t_fe60cdf5): the vocabulary the API accepts is the
    /// vocabulary the engine can execute. Eleven accepted names used to have no arm, so a workflow
    /// could be created, deployed and "run" while every one of those steps did nothing.
    #[test]
    fn every_accepted_step_type_has_an_arm() {
        let arms = engine_arm_keys();
        assert!(
            arms.len() >= 20,
            "the arm scan found only {} keys — the scan is broken, not the code: {arms:?}",
            arms.len()
        );
        for t in EXECUTABLE_STEP_TYPES {
            assert!(
                arms.iter().any(|a| a == t),
                "EXECUTABLE_STEP_TYPES accepts '{t}' but no `match step_type.as_str()` arm covers \
                 it (arms: {arms:?})"
            );
        }
        // The retired names must not be accepted: nothing in this app executes either.
        for retired in [
            "research",
            "openclaw",
            // kanban t_02519738: offered/accepted once, delivered nothing.
            "export",
            "publish",
            "generate",
            // kanban t_81602ca1: the tenant-JavaScript family — the engine's arm ran no code at all
            // and answered `completed`, while the n8n mirror's Code node failed on this install.
            "transform",
            "code",
            "format",
            // kanban t_f10ada7a: the engine's arm answered `completed` for a design it never made.
            "design",
        ] {
            assert!(
                !is_executable_step_type(retired),
                "'{retired}' must be retired — no live path in this app runs it"
            );
            assert!(
                is_retired_step_type(retired),
                "'{retired}' is retired and must be in RETIRED_STEP_TYPES so the write path's \
                 refusal can name it"
            );
            assert!(
                !arms.iter().any(|a| a == retired),
                "'{retired}' is retired but the engine still has an arm for it: {arms:?}"
            );
        }
        // ...and the console mut not offer them (the other half of the same vocabulary).
        assert!(
            !include_str!("../www-app/index.html").contains("{ k:'export'"),
            "the console must not offer the retired export step"
        );
        // `design` was retired without ever having been offered by the picker (kanban t_f10ada7a);
        // pin that so a future entry cannot advertise a type the write path now refuses.
        assert!(
            !include_str!("../www-app/index.html").contains("k:'design'"),
            "the console must not offer the retired design step"
        );
    }

    /// Files under `migrations/` are read with `std::fs` and NOT with the `include_str!` macro: the
    /// fleet's from-zero harness detects the app's migration runner by grepping `src/` for an
    /// `include_str!` whose path points into the migrations directory and, finding one, concludes
    /// that the install path is a hardcoded array of exactly those files — it then reports the other
    /// 80 as UNREGISTERED and refuses the deploy (measured on this app, deploy-app.sh step 0b: rc 3
    /// APPLY-FAIL, because the truncated list starts at `013_seed_data.sql`, whose `plan_tiers` does
    /// not exist yet). This app's runner (`src/db.rs`) reads the directory at runtime, so the harness
    /// must keep seeing `read_dir` — hence no `include_str!` here, not even inside a comment.
    fn migration_source(name: &str) -> String {
        std::fs::read_to_string(format!("{}/migrations/{name}", env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|e| panic!("migrations/{name} is not readable: {e}"))
    }

    /// Migration 073's census list must agree with this constant: a list that drifts would make the
    /// migration report a runnable row as leftover (or the reverse). Each side is read out of its
    /// own source, so neither can be edited alone (kanban t_27a15474).
    #[test]
    fn the_migration_vocabulary_matches_the_engine() {
        let sql = migration_source("073_template_steps_executable_types.sql");
        let start = sql
            .find("step_type NOT IN (")
            .expect("the migration's vocabulary census is in the file");
        let end = start + sql[start..].find(')').expect("the census list terminator");
        let mut declared: Vec<String> = sql[start..end]
            .split('\'')
            .skip(1)
            .step_by(2)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        declared.sort();
        declared.dedup();
        let mut engine: Vec<String> = EXECUTABLE_STEP_TYPES
            .iter()
            .map(|s| s.to_string())
            .collect();
        engine.sort();
        engine.dedup();
        // Migration 073 is ALREADY APPLIED, so its embedded list is a SNAPSHOT of the vocabulary of
        // 2026-10-02 and cannot be edited — sqlx checksums it and an edited applied migration makes
        // the app exit(1) at boot. What must hold is that every step type the migration NAMES is a
        // type this app KNOWS: executable, or explicitly RETIRED (kanban t_02519738 moved `export`,
        // `publish` and `generate` from the first set to the second). A word in neither set would
        // mean the migration blessed a type nobody has ever run.
        for t in &declared {
            assert!(
                is_executable_step_type(t) || is_retired_step_type(t),
                "migration 073 names step type '{t}', which this app neither executes nor marks \
                 retired — the vocabulary drifted (executable today: {engine:?})"
            );
        }
        // And a retirement may only RE-CLASSIFY a type the app really accepted: the three this card
        // retired are in the snapshot (073 and the steps API accepted all three). `research` and
        // `openclaw` were retired BEFORE 073 was written and were never in its list.
        for r in [
            "export",
            "publish",
            "generate",
            // kanban t_81602ca1: all three are named in 073's snapshot.
            "transform",
            "code",
            "format",
        ] {
            assert!(
                declared.iter().any(|d| d == r),
                "'{r}' is retired but migration 073's vocabulary snapshot never named it, so it was \
                 never an accepted step type"
            );
        }
    }

    /// The ten rows migrations/013_seed_data.sql seeded hold lifecycle STAGE NAMES in `step_type`
    /// (the human label already lives in `name`), which is the whole defect of kanban t_27a15474:
    /// install copies the column verbatim into `workflow_steps`. Migration 073 must remap every one
    /// of them, and none of them may ever be mistaken for a step type.
    #[test]
    fn the_seeded_template_stage_names_are_remapped() {
        let seed = migration_source("013_seed_data.sql");
        let mig = migration_source("073_template_steps_executable_types.sql");
        const PREFIX: &str = "(uuid_generate_v4(), gov_template_id, '";
        let mut stages: Vec<String> = Vec::new();
        for line in seed.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix(PREFIX) {
                if let Some((stage, _)) = rest.split_once('\'') {
                    stages.push(stage.to_string());
                }
            }
        }
        assert_eq!(
            stages.len(),
            10,
            "the seed's ten template step rows were not parsed: {stages:?}"
        );
        for stage in &stages {
            assert!(
                !is_executable_step_type(stage),
                "'{stage}' is a stage name, not a step type — the seed was wrong"
            );
            assert!(
                mig.contains(&format!("('{stage}', ")),
                "migration 073 does not remap the seeded stage '{stage}'"
            );
        }
    }

    /// The console is the surface the product sells from: every step type its Builder offers must
    /// be executable, or the tenant builds a step that silently does nothing.
    #[test]
    fn the_console_picker_offers_only_executable_step_types() {
        let spa = include_str!("../www-app/index.html");
        let mut offered: Vec<String> = Vec::new();
        for decl in ["const STEP_TYPES = [", "const STEP_TYPES_EXTRA = ["] {
            let start = spa
                .find(decl)
                .expect("the picker arrays are in the served console");
            let end = start + spa[start..].find("];").expect("array terminator");
            for (idx, _) in spa[start..end].match_indices("k:'") {
                let rest = &spa[start + idx + 3..];
                if let Some(key) = rest.split('\'').next() {
                    offered.push(key.to_string());
                }
            }
        }
        offered.sort();
        offered.dedup();
        assert!(
            offered.len() >= 8,
            "the picker parse found only {} types — the parse is broken: {offered:?}",
            offered.len()
        );
        for t in &offered {
            assert!(
                is_executable_step_type(t),
                "the console offers '{t}' but the engine has no arm for it (kanban t_fe60cdf5)"
            );
        }
        assert!(
            !offered.iter().any(|t| t == "research"),
            "research is retired and must not be offered"
        );
    }

    /// The Notify step's channel list is the same kind of vocabulary as the step types, and since
    /// kanban t_d3ff37ef it is SENDER-BACKED: the console must offer exactly the channels this
    /// install can deliver on, so it carries NO hardcoded list — it renders
    /// `S.notifyChannels.channels`, which is what `GET /api/v1/notify/channels` answers
    /// (`crate::notify::NotifySenders::offered`).
    ///
    /// What this test can pin from the served console's own source is the SHAPE: the options come
    /// from the fetched, configured set; the offline fallback is exactly `webhook`; and no retired
    /// channel is named. What the RUNTIME set contains is pinned where it is decided — `crate::notify`
    /// and `n8n_converter`'s `notify_channels_emit_nodes_that_can_actually_run`.
    #[test]
    fn the_console_offers_only_deliverable_notify_channels() {
        let spa = include_str!("../www-app/index.html");
        assert!(
            spa.contains("const channels = (S.notifyChannels && S.notifyChannels.channels) || ['webhook'];"),
            "the notify Channel select must render the FETCHED, configured set — not a literal list"
        );
        assert!(
            spa.contains("channels.map(c => html`<option key=${c} value=${c}>"),
            "the notify Channel select must build its options from that set"
        );
        // The offline fallback (the fetch failed) is `webhook` alone: it needs no provider, so it is
        // the one channel that can honestly be offered with nothing configured.
        assert!(
            spa.contains("channels:['webhook']"),
            "the notify offline fallback must be webhook alone"
        );
        // No retired channel may be named anywhere in the notify arm.
        let start = spa
            .find("case 'notify': {")
            .expect("the notify cfgFields arm is in the served console");
        // A byte window has to land on a char boundary — the console is full of em dashes, and
        // slicing through one panics (measured: "byte index … is not a char boundary").
        let mut end = (start + 2600).min(spa.len());
        while end > start && !spa.is_char_boundary(end) {
            end -= 1;
        }
        let arm = &spa[start..end];
        for retired in ["slack", "telegram", "discord", "sendgrid", "smtp"] {
            assert!(
                !arm.contains(retired),
                "'{retired}' is retired and must not be offered by the Notify step (kanban t_642b6894 / t_d3ff37ef)"
            );
        }
        // The recipient rule is visible in the console too: the sender-backed channels pick from the
        // account's own people, and carry no free-text address field.
        assert!(
            spa.contains("scopes = (S.notifyChannels && S.notifyChannels.scopes)"),
            "the notify recipient picker must render the scopes the API reports"
        );
        assert!(
            arm.contains("recipient_scope"),
            "the sender-backed notify channels must pick a recipient scope, never a typed address"
        );
        // The vocabulary itself is still the write path's rule, and `webhook` is still in it.
        assert!(is_notify_channel("webhook"));
        assert_eq!(NOTIFY_CHANNELS.to_vec(), vec!["webhook", "email", "sms"]);
        assert_eq!(RETIRED_NOTIFY_CHANNELS.to_vec(), vec!["slack", "telegram"]);
    }

    /// The old arm's answer, and the new one, are what the bug was: `classify_step_status` mapped
    /// `warning` to itself — neither failed nor pending — so the instance came out `completed`.
    /// The replacement is `skipped` AND carries `unexecutable`, which `run_in_process` reads.
    #[test]
    fn a_step_with_no_executor_is_marked_not_merely_warned() {
        let result = unexecutable_step_result(3, "research");
        assert_eq!(result["status"], "skipped");
        assert_eq!(result["unexecutable"], "research");
        assert_eq!(result["type"], "research");
        assert!(result["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("no executor"));
        assert_eq!(classify_step_status(&result), "skipped");

        // control: the pre-fix shape. `warning` is not failed and not pending, so a run built from
        // it reported success — the defect this card exists for.
        // (the old arm's own message is deliberately NOT repeated here: the file-level assertion
        // below greps this file for it, and a copy of the string in the test would defeat that.)
        let control = json!({"step": 3, "type": "research", "status": "warning"});
        assert_eq!(classify_step_status(&control), "warning");
        assert!(control.get("unexecutable").is_none());
        // The needle is assembled at run time: a literal here would be embedded by
        // `include_str!` and the file would always "contain" the phrase it is grepping for.
        let retired = ["marked as warning", " but workflow continues"].concat();
        assert!(
            !include_str!("execution.rs").contains(&retired),
            "the silent-warning string must be gone from the engine"
        );
    }

    /// The Notify arm (kanban t_e0e6a42e): a destination that refuses the call must fail the
    /// step, and a step that could not send anything must say so.
    ///
    /// Executed, not asserted on paper — `notify_step_result` is the exact function the arm hands
    /// its call result to, and `classify_step_status` is what the walk (and the instance roll-up)
    /// reads. The control leg is the PRE-FIX shape: the same 404 body, wrapped with
    /// `status: "completed"` the way the old arm did it, which the walk called success.
    #[test]
    fn a_notify_webhook_that_answers_404_is_not_a_completed_step() {
        let body = "{\"code\":404,\"message\":\"The requested webhook \\\"POST \
                    workflowswift-notify\\\" is not registered.\"}";

        // PRE-FIX control: any status code read as completed, with the 404 body in `response`.
        let control = json!({
            "step": 0, "type": "notify", "status": "completed", "channel": "webhook",
            "recipient": "https://tenant.example.com/hook", "response": body,
        });
        assert_eq!(
            classify_step_status(&control),
            "completed",
            "the pre-fix arm reported a 404 as a completed Notify step"
        );

        // The arm now: the upstream code IS the step status, so the walk records a failure and
        // the log row names what came back.
        let refused = notify_step_result(
            0,
            "webhook",
            "https://tenant.example.com/hook",
            Ok((404, body.to_string())),
        );
        assert_eq!(classify_step_status(&refused), "failed");
        assert_eq!(refused.get("status").unwrap(), &json!(404));
        assert_eq!(
            step_error_text(&refused).unwrap(),
            "upstream returned HTTP 404"
        );
        assert_eq!(refused.get("response").unwrap(), &json!(body));

        // A 2xx is still a completion.
        let delivered = notify_step_result(
            0,
            "webhook",
            "https://tenant.example.com/hook",
            Ok((200, "{\"ok\":true}".to_string())),
        );
        assert_eq!(classify_step_status(&delivered), "completed");

        // A transport failure / a destination the SSRF gate refused is an explicit error, not a
        // completion with a note.
        let unreachable = notify_step_result(
            0,
            "webhook",
            "http://127.0.0.1:9/hook",
            Err(
                "'127.0.0.1' resolves to 127.0.0.1 — a loopback/private/link-local address is \
                 not a valid destination for a workflow step"
                    .to_string(),
            ),
        );
        assert_eq!(classify_step_status(&unreachable), "failed");
        assert_eq!(unreachable.get("status").unwrap(), &json!("error"));
        assert!(step_error_text(&unreachable)
            .unwrap_or_default()
            .contains("not a valid destination"));
        assert!(
            unreachable.get("note").is_none(),
            "the old arm's 'Notify queued' note was the lie this replaces"
        );

        // A step that could not send anything NAMES the missing sender and is not a completion.
        let retired = notify_undeliverable_result(
            0,
            "email",
            "Notify channel 'email' has no sender in this app (the console's Notify channel is webhook)",
        );
        assert_eq!(classify_step_status(&retired), "skipped");
        assert!(retired
            .get("undeliverable")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .contains("no sender in this app"));
        let blank =
            notify_undeliverable_result(0, "webhook", "Notify step has a blank Webhook URL");
        assert_eq!(classify_step_status(&blank), "skipped");
        assert!(blank
            .get("undeliverable")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .contains("blank Webhook URL"));
    }

    /// The destination gate is a refusal of the address, not of the URL shape: this box's own
    /// public IP (what the acceptance probe posts to) passes, the box's own loopback does not.
    #[tokio::test]
    async fn a_public_destination_is_allowed_and_a_loopback_one_is_refused() {
        use crate::security::webhook_security::gate_step_destination;
        assert!(gate_step_destination("http://209.222.97.179:18099/hit/x")
            .await
            .is_ok());
        assert!(gate_step_destination("https://example.com/hook")
            .await
            .is_ok());
        for refused in [
            "http://127.0.0.1:8085/api/v1/health",
            "http://192.168.1.1/hook",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]:8080/",
        ] {
            let err = gate_step_destination(refused)
                .await
                .expect_err(&format!("{refused} must be refused"));
            assert!(err.contains("not a valid destination"), "{err}");
        }
        // fail closed on a host that cannot resolve
        assert!(gate_step_destination("http://no-such-host.invalid/hook")
            .await
            .is_err());
        assert!(gate_step_destination("not-a-url").await.is_err());
    }
}
