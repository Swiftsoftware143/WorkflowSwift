//! The AI Action step's ONE outbound call (kanban t_03e4d3d9).
//!
//! Until this module existed the app offered an AI Action step, collected the tenant's provider
//! key (`provider_keys`, aid-scoped, encrypted at rest by `security::provider_key_crypto`) and had
//! NO code that called an LLM provider: the engine's arm POSTed a platform-shaped body at a webhook
//! that is not registered, fell back to a second one (also 404) and then reported
//! `status: "completed"` carrying `generated_content: <the prompt>` — a generation that never
//! happened. The console's hint said "NOT LIVE YET" and the n8n mirror emitted an HTTP Request node
//! at `http://localhost:18792` (nothing inside the n8n container).
//!
//! The product decision (this card) is BYOK: **the credential is the tenant's own provider key**,
//! the same mapping `GET /api/v1/integrations/resolve?step_type=ai-action` serves and the console's
//! Provider Keys panel collects. Consequences, both deliberate:
//!
//! * **Cost: 0 credits.** No platform credential is involved and no platform money is spent; the
//!   only bill is the provider's own, against the tenant's key. That is the `credit_cost: 0` the
//!   resolve endpoint already returns for a `user_key` resolution, so nothing here has to invent a
//!   price. With no connected key the step does not fall back to a platform key — it is `skipped`
//!   with a named reason (there is no platform key to fall back to).
//! * **Destination: a CONSTANT per provider.** The URL is never read out of step config or a
//!   tenant row, so this path adds no SSRF surface — a tenant cannot redirect its own credential to
//!   an address of its choosing. (`provider_keys.base_url`, which tenants CAN set for the generic
//!   dispatch path, is deliberately ignored here.)
//!
//! The four providers are exactly the ones the app already serves for `ai-action`; the default
//! model per provider is pinned so a step that names only a provider still produces a predictable
//! call. A per-step `config.model` may override it.

use serde_json::json;

/// One LLM provider this app can call. `key` is the vocabulary value stored in
/// `provider_keys.provider` and in a step's `config.provider`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiProvider {
    pub key: &'static str,
    pub label: &'static str,
    /// Constant destination. Never a tenant value — see the module docs.
    pub base_url: &'static str,
    /// The model used when a step does not name one.
    pub default_model: &'static str,
    pub api: ApiShape,
}

/// How a provider's chat endpoint is shaped. Three shapes cover the four providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiShape {
    /// `POST {base}/chat/completions`, `Authorization: Bearer`, OpenAI message array.
    /// Used by OpenAI and DeepSeek (DeepSeek is OpenAI-compatible).
    OpenAiCompatible,
    /// `POST {base}/v1/messages`, `x-api-key` + `anthropic-version`.
    Anthropic,
    /// `POST {base}/v1beta/models/{model}:generateContent`, `x-goog-api-key`.
    Gemini,
}

/// The provider vocabulary for an AI Action step. ONE list: the engine's arm, the resolve endpoint
/// and the console's Provider Keys panel all read it, so the app cannot serve a provider it cannot
/// call (the drift that made this card necessary).
pub const AI_PROVIDERS: &[AiProvider] = &[
    AiProvider {
        key: "openai",
        label: "OpenAI",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o-mini",
        api: ApiShape::OpenAiCompatible,
    },
    AiProvider {
        key: "anthropic",
        label: "Anthropic",
        base_url: "https://api.anthropic.com",
        default_model: "claude-sonnet-4-20250514",
        api: ApiShape::Anthropic,
    },
    AiProvider {
        key: "deepseek",
        label: "DeepSeek",
        base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        api: ApiShape::OpenAiCompatible,
    },
    AiProvider {
        key: "gemini",
        label: "Google Gemini",
        base_url: "https://generativelanguage.googleapis.com",
        default_model: "gemini-2.0-flash",
        api: ApiShape::Gemini,
    },
];

/// Look up a provider by its vocabulary key.
pub fn provider_by_key(key: &str) -> Option<&'static AiProvider> {
    AI_PROVIDERS.iter().find(|p| p.key == key)
}

/// The vocabulary as one line, for a refusal body / a step result's reason.
pub fn provider_key_list() -> String {
    AI_PROVIDERS
        .iter()
        .map(|p| p.key)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The provider keys as an owned list (the resolve endpoint's shape).
pub fn provider_keys() -> Vec<&'static str> {
    AI_PROVIDERS.iter().map(|p| p.key).collect()
}

/// Call `provider` with the tenant's key. `Ok((status_code, raw_body))` is an ANSWER from the
/// provider (any status — a 401 is an answer, and it is the caller's job to fail the step on a
/// non-2xx); `Err` is a transport failure / timeout, i.e. the destination was never reached.
///
/// The key is validated as an HTTP header value first: reqwest PANICS on an invalid one, so a
/// stored credential carrying a control character must surface as a step error instead of taking
/// the process down.
pub async fn generate(
    provider: &AiProvider,
    model: &str,
    api_key: &str,
    prompt: &str,
) -> Result<(u16, String), String> {
    ensure_header_safe(api_key).map_err(|e| {
        format!(
            "the stored {} key is not usable as an HTTP header: {}",
            provider.key, e
        )
    })?;

    let (url, request) = build_request(provider, model, api_key);

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("could not build the HTTP client: {}", e))?;

    let resp = client
        .post(&url)
        .headers(request)
        .json(&body_for(provider, model, prompt))
        .send()
        .await
        .map_err(|e| format!("could not reach {} ({}): {}", provider.label, url, e))?;

    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, text))
}

/// The provider's message out of its own response body, or `None` when the body carries none.
///
/// Four providers, three shapes, and NO invented fallback: a caller that cannot find a message
/// reports a step error rather than storing the prompt as the answer (the defect this card is
/// about).
pub fn extract_content(provider: &AiProvider, body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let text = match provider.api {
        // {"choices":[{"message":{"content":"…"}}]}
        ApiShape::OpenAiCompatible => parsed
            .get("choices")?
            .get(0)?
            .get("message")?
            .get("content")?
            .as_str()?,
        // {"content":[{"type":"text","text":"…"}]}
        ApiShape::Anthropic => parsed.get("content")?.get(0)?.get("text")?.as_str()?,
        // {"candidates":[{"content":{"parts":[{"text":"…"}]}}]}
        ApiShape::Gemini => parsed
            .get("candidates")?
            .get(0)?
            .get("content")?
            .get("parts")?
            .get(0)?
            .get("text")?
            .as_str()?,
    };
    if text.trim().is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// The request line + auth headers for a provider, as `(url, headers)`.
fn build_request(
    provider: &AiProvider,
    model: &str,
    api_key: &str,
) -> (String, reqwest::header::HeaderMap) {
    use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let url = match provider.api {
        ApiShape::OpenAiCompatible => format!("{}/chat/completions", provider.base_url),
        ApiShape::Anthropic => format!("{}/v1/messages", provider.base_url),
        ApiShape::Gemini => format!(
            "{}/v1beta/models/{}:generateContent",
            provider.base_url, model
        ),
    };

    match provider.api {
        ApiShape::OpenAiCompatible => {
            if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", api_key)) {
                headers.insert(AUTHORIZATION, v);
            }
        }
        ApiShape::Anthropic => {
            if let Ok(v) = HeaderValue::from_str(api_key) {
                headers.insert("x-api-key", v);
            }
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        }
        ApiShape::Gemini => {
            if let Ok(v) = HeaderValue::from_str(api_key) {
                headers.insert("x-goog-api-key", v);
            }
        }
    }

    (url, headers)
}

/// The body each shape expects.
fn body_for(provider: &AiProvider, model: &str, prompt: &str) -> serde_json::Value {
    match provider.api {
        ApiShape::OpenAiCompatible => json!({
            "model": model,
            "messages": [{ "role": "user", "content": prompt }],
            "max_tokens": 1000,
            "temperature": 0.7,
            "stream": false
        }),
        ApiShape::Anthropic => json!({
            "model": model,
            "max_tokens": 1000,
            "messages": [{ "role": "user", "content": prompt }]
        }),
        ApiShape::Gemini => json!({
            "contents": [{ "parts": [{ "text": prompt }] }]
        }),
    }
}

/// A stored credential is about to become an HTTP header value, and reqwest panics on an invalid
/// one. Check it first so a malformed stored value surfaces as a step error, not a crash.
fn ensure_header_safe(value: &str) -> Result<(), String> {
    reqwest::header::HeaderValue::from_str(value)
        .map(|_| ())
        .map_err(|_| "value carries a character an HTTP header cannot (e.g. CR/LF)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vocabulary_is_the_four_providers_the_app_serves() {
        assert_eq!(
            provider_key_list(),
            "openai, anthropic, deepseek, gemini",
            "the ai-action provider vocabulary"
        );
        assert!(provider_by_key("deepseek").is_some());
        assert!(provider_by_key("mystery").is_none());
        assert!(provider_by_key("").is_none());
    }

    #[test]
    fn every_provider_has_a_constant_https_destination_and_a_default_model() {
        for p in AI_PROVIDERS {
            assert!(
                p.base_url.starts_with("https://"),
                "{} must be https",
                p.key
            );
            assert!(
                !p.default_model.is_empty(),
                "{} needs a default model",
                p.key
            );
            // No environment interpolation and no tenant input: the destination is a constant.
            assert!(
                !p.base_url.contains("{}"),
                "{} destination is not a template",
                p.key
            );
        }
    }

    #[test]
    fn the_request_url_matches_each_provider_shape() {
        let openai = provider_by_key("openai").unwrap();
        let (url, _) = build_request(openai, openai.default_model, "sk-x");
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");

        let anthropic = provider_by_key("anthropic").unwrap();
        let (url, headers) = build_request(anthropic, anthropic.default_model, "sk-x");
        assert_eq!(url, "https://api.anthropic.com/v1/messages");
        assert!(headers.contains_key("x-api-key"));
        assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");

        let gemini = provider_by_key("gemini").unwrap();
        let (url, headers) = build_request(gemini, "gemini-2.0-flash", "sk-x");
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent"
        );
        assert!(headers.contains_key("x-goog-api-key"));

        let deepseek = provider_by_key("deepseek").unwrap();
        let (url, headers) = build_request(deepseek, deepseek.default_model, "sk-x");
        assert_eq!(url, "https://api.deepseek.com/v1/chat/completions");
        assert_eq!(headers.get("authorization").unwrap(), "Bearer sk-x");
    }

    #[test]
    fn content_is_read_from_each_provider_response_shape_and_never_invented() {
        let openai = provider_by_key("openai").unwrap();
        assert_eq!(
            extract_content(openai, r#"{"choices":[{"message":{"content":"hello"}}]}"#).unwrap(),
            "hello"
        );
        assert!(extract_content(openai, r#"{"error":{"message":"bad key"}}"#).is_none());
        // A 200 whose body carries no message must NOT become the prompt.
        assert!(extract_content(openai, r#"{"choices":[]}"#).is_none());
        assert!(
            extract_content(openai, r#"{"choices":[{"message":{"content":"   "}}]}"#).is_none()
        );

        let anthropic = provider_by_key("anthropic").unwrap();
        assert_eq!(
            extract_content(
                anthropic,
                r#"{"content":[{"type":"text","text":"hi there"}]}"#
            )
            .unwrap(),
            "hi there"
        );
        assert!(extract_content(anthropic, r#"{"type":"error"}"#).is_none());

        let gemini = provider_by_key("gemini").unwrap();
        assert_eq!(
            extract_content(
                gemini,
                r#"{"candidates":[{"content":{"parts":[{"text":"gemini says hi"}]}}]}"#
            )
            .unwrap(),
            "gemini says hi"
        );
        assert!(extract_content(gemini, r#"{"candidates":[]}"#).is_none());
    }

    #[test]
    fn a_malformed_stored_key_is_refused_before_it_reaches_a_header() {
        assert!(ensure_header_safe("sk-abc123").is_ok());
        assert!(ensure_header_safe("sk-abc\r\nX-Evil: 1").is_err());
    }

    #[test]
    fn the_request_body_carries_the_prompt_and_the_model_for_each_shape() {
        let openai = provider_by_key("openai").unwrap();
        let b = body_for(openai, "gpt-4o-mini", "summarise this");
        assert_eq!(b["model"], "gpt-4o-mini");
        assert_eq!(b["messages"][0]["content"], "summarise this");

        let anthropic = provider_by_key("anthropic").unwrap();
        let b = body_for(anthropic, "claude-sonnet-4-20250514", "summarise this");
        assert_eq!(b["model"], "claude-sonnet-4-20250514");
        assert_eq!(b["messages"][0]["content"], "summarise this");

        let gemini = provider_by_key("gemini").unwrap();
        let b = body_for(gemini, "gemini-2.0-flash", "summarise this");
        assert_eq!(b["contents"][0]["parts"][0]["text"], "summarise this");
    }
}
