//! SMS sender — the app's second outbound transport, built to the SAME discipline as
//! [`crate::email`] (kanban t_d3ff37ef).
//!
//! The Notify step's `sms` channel used to be a channel a tenant could pick that delivered
//! nothing (retired by kanban t_08be842f) because no SMS provider existed in this app and no
//! `sms` credential existed in n8n either. This module is the missing sender.
//!
//! Rules it keeps, all of which are the point of the card:
//!
//! * the credential lives ONLY in `admin_settings.sms`, sealed at rest with the app's `enc:v1:`
//!   envelope (the same machinery `admin_settings.email` uses — kanban t_a794cb09) and
//!   configured by an admin in the panel. No env-var fallback, and nothing about it ever
//!   reaches n8n: the tenant's generated graph calls an app route, never a provider.
//! * NOT CONFIGURED IS REFUSED, never silently skipped. `is_configured` is the single predicate
//!   the write path and this sender share, so an `sms` Notify step cannot even be stored until a
//!   provider is set.
//! * the destination is a phone number that already belongs to the account's own people
//!   (`users.phone`, E.164). This module never takes a number out of step config — the caller
//!   resolves the recipient first; see [`crate::notify`].

use serde_json::json;
use sqlx::PgPool;

use crate::state::AppState;

/// The provider vocabulary of this app's SMS sender. ONE list: the Admin > Settings > SMS picker
/// (www-admin/index.html) offers exactly it, and `send_sms` has an arm for each. An unknown value
/// is refused rather than falling through to some default transport — a mail sender can fall back
/// to its only HTTP shape, an SMS sender must not invent one.
pub const SMS_PROVIDERS: &[&str] = &["twilio", "vonage"];

/// The credential fields carried inside the `admin_settings.sms` object. Sealed with the same
/// `enc:v1:` envelope as every other stored credential in this app.
pub const CONFIG_SECRET_FIELDS: [&str; 2] = ["api_key", "api_secret"];

/// Default endpoints, used only when the admin left `api_url` blank.
const TWILIO_API: &str = "https://api.twilio.com";
const VONAGE_API: &str = "https://rest.nexmo.com";

#[derive(Debug, Clone, Default)]
pub struct SmsConfig {
    /// `twilio` | `vonage`
    pub provider: String,
    /// twilio: the account's auth token; vonage: the API key.
    pub api_key: String,
    /// vonage only.
    pub api_secret: String,
    /// twilio only — the Account SID that owns the sender.
    pub account_sid: String,
    /// The number messages are sent FROM (E.164).
    pub from_number: String,
    /// Optional endpoint override (a tenant-independent, admin-set constant).
    pub api_url: String,
}

fn cfg_str(cfg: &serde_json::Value, key: &str) -> String {
    cfg.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

impl SmsConfig {
    /// Can this configuration actually deliver a message? This is the ONE predicate behind the
    /// fail-closed gate: `assert_notify_channel_ok` accepts an `sms` step only when it is true.
    ///
    /// It requires the fields the chosen provider's arm really needs — a half-filled Twilio
    /// config (no Account SID) is NOT configured, because it would 404 at send time.
    pub fn is_configured(&self) -> bool {
        if !SMS_PROVIDERS.contains(&self.provider.as_str()) || self.from_number.is_empty() {
            return false;
        }
        match self.provider.as_str() {
            "twilio" => !self.api_key.is_empty() && !self.account_sid.is_empty(),
            "vonage" => !self.api_key.is_empty() && !self.api_secret.is_empty(),
            _ => false,
        }
    }
}

/// Read `admin_settings.sms`, opening the sealed credential, exactly like
/// `email::get_email_config` does for the mail leg.
///
/// Returns `None` when the row is missing — that is "no SMS sender on this install", which every
/// caller treats as CONFIGURED=false (fail-closed), never as "send anyway".
pub async fn get_sms_config(state: &AppState) -> Option<SmsConfig> {
    let raw = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT value FROM admin_settings WHERE key = 'sms'",
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()?;

    let mut cfg = raw;
    if let Err(e) = open_config_secrets(&state.db, &mut cfg).await {
        // A credential we cannot open is a credential we do not have: report it and stay
        // fail-closed rather than sending with an empty key.
        eprintln!(
            "[sms] could not open admin_settings.sms credentials: {e} — treating SMS as \
             not configured (Admin > Settings > SMS)"
        );
        return None;
    }

    Some(SmsConfig {
        provider: cfg_str(&cfg, "provider").to_ascii_lowercase(),
        api_key: cfg_str(&cfg, "api_key"),
        api_secret: cfg_str(&cfg, "api_secret"),
        account_sid: cfg_str(&cfg, "account_sid"),
        from_number: cfg_str(&cfg, "from_number"),
        api_url: cfg_str(&cfg, "api_url"),
    })
}

/// Is there an SMS sender on this install? The gate's question, answered without holding the
/// credential any longer than the query itself.
pub async fn is_configured(state: &AppState) -> bool {
    get_sms_config(state)
        .await
        .map(|c| c.is_configured())
        .unwrap_or(false)
}

/// E.164 normalisation. The ONE destination shape this product accepts, applied wherever a phone
/// number is written (the admin's own user records) and re-applied before a send.
///
/// Deliberately does NOT guess a country: a bare 10-digit number is refused with the reason,
/// because guessing would send an account's notification to a stranger in another country.
pub fn normalize_phone(raw: &str) -> Result<String, String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '(' | ')' | '.'))
        .collect();
    let candidate = if let Some(rest) = cleaned.strip_prefix("00") {
        format!("+{rest}")
    } else {
        cleaned
    };
    let digits = candidate.strip_prefix('+').unwrap_or("");
    let ok = candidate.starts_with('+')
        && (8..=15).contains(&digits.len())
        && digits.chars().all(|c| c.is_ascii_digit())
        && !digits.starts_with('0');
    if ok {
        Ok(candidate)
    } else {
        Err("phone must be in E.164 form with a country code, e.g. +15551234567".to_string())
    }
}

/// Send one SMS through whichever provider the admin selected in Admin > Settings > SMS.
///
/// Every outcome is the provider's own: a non-2xx is an `Err` carrying its status and body, so a
/// step result says what actually happened instead of a fabricated "sent".
pub async fn send_sms(state: &AppState, to: &str, body: &str) -> Result<(), String> {
    let Some(cfg) = get_sms_config(state).await else {
        return Err("SMS not configured: set the provider in Admin > Settings > SMS".to_string());
    };
    if !cfg.is_configured() {
        return Err("SMS not configured: set the provider in Admin > Settings > SMS".to_string());
    }
    let to = normalize_phone(to).map_err(|e| format!("refusing to send: {e}"))?;
    match cfg.provider.as_str() {
        "twilio" => send_via_twilio(&cfg, &to, body).await,
        "vonage" => send_via_vonage(&cfg, &to, body).await,
        other => Err(format!(
            "SMS provider '{}' has no sender in this app. Valid providers: {}",
            other,
            SMS_PROVIDERS.join(", ")
        )),
    }
}

async fn send_via_twilio(cfg: &SmsConfig, to: &str, body: &str) -> Result<(), String> {
    let base = if cfg.api_url.is_empty() {
        TWILIO_API.to_string()
    } else {
        cfg.api_url.clone()
    };
    let url = format!(
        "{}/2010-04-01/Accounts/{}/Messages.json",
        base.trim_end_matches('/'),
        cfg.account_sid
    );

    let resp = reqwest::Client::new()
        .post(&url)
        .basic_auth(&cfg.account_sid, Some(&cfg.api_key))
        .form(&[
            ("From", cfg.from_number.as_str()),
            ("To", to),
            ("Body", body),
        ])
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("Failed to reach twilio: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("Twilio API returned {}: {}", status, text));
    }
    // Twilio answers 200 for a request it *accepted*; an invalid number shows up as
    // `{"status":"queued"|"failed", "error_code":…}`. Report the failure it names.
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some("failed") = parsed.get("status").and_then(|s| s.as_str()) {
            return Err(format!(
                "Twilio refused the message: {}",
                parsed
                    .get("error_message")
                    .and_then(|m| m.as_str())
                    .unwrap_or(&text)
            ));
        }
    }
    Ok(())
}

async fn send_via_vonage(cfg: &SmsConfig, to: &str, body: &str) -> Result<(), String> {
    let base = if cfg.api_url.is_empty() {
        VONAGE_API.to_string()
    } else {
        cfg.api_url.clone()
    };
    let url = format!("{}/sms/json", base.trim_end_matches('/'));

    let resp = reqwest::Client::new()
        .post(&url)
        .form(&[
            ("api_key", cfg.api_key.as_str()),
            ("api_secret", cfg.api_secret.as_str()),
            ("from", cfg.from_number.as_str()),
            ("to", to),
            ("text", body),
        ])
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("Failed to reach vonage: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("Vonage API returned {}: {}", status, text));
    }
    // Vonage also answers 200 with a per-message status: "0" is accepted, anything else is the
    // error it names. A 200 is not a send.
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(msg) = parsed
            .get("messages")
            .and_then(|m| m.as_array())
            .and_then(|a| a.first())
        {
            let st = msg.get("status").and_then(|s| s.as_str()).unwrap_or("");
            if !st.is_empty() && st != "0" {
                return Err(format!(
                    "Vonage refused the message: {}",
                    msg.get("error-text")
                        .and_then(|m| m.as_str())
                        .unwrap_or(&text)
                ));
            }
        }
    }
    Ok(())
}

/// Seal the credential fields of the `sms`-config object IN PLACE, before it is stored.
/// Same contract as `email::seal_config_secrets`: empty stays empty, an already-sealed value is
/// left alone, and a missing master key FAILS rather than storing plaintext.
pub async fn seal_config_secrets(
    pool: &PgPool,
    cfg: &mut serde_json::Value,
) -> Result<(), crate::security::provider_key_crypto::CryptoError> {
    let Some(obj) = cfg.as_object_mut() else {
        return Ok(());
    };
    for field in CONFIG_SECRET_FIELDS {
        let current = obj
            .get(field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if current.is_empty() || crate::security::provider_key_crypto::is_encrypted(&current) {
            continue;
        }
        let sealed =
            crate::security::provider_key_crypto::encrypt_for_storage(pool, &current).await?;
        obj.insert(field.to_string(), serde_json::Value::String(sealed));
    }
    Ok(())
}

/// Open the credential fields of the `sms`-config object IN PLACE after a DB read.
pub async fn open_config_secrets(
    pool: &PgPool,
    cfg: &mut serde_json::Value,
) -> Result<(), crate::security::provider_key_crypto::CryptoError> {
    let Some(obj) = cfg.as_object_mut() else {
        return Ok(());
    };
    for field in CONFIG_SECRET_FIELDS {
        let current = obj
            .get(field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if current.is_empty() || !crate::security::provider_key_crypto::is_encrypted(&current) {
            continue;
        }
        let opened =
            crate::security::provider_key_crypto::decrypt_from_storage(pool, &current).await?;
        obj.insert(field.to_string(), serde_json::Value::String(opened));
    }
    Ok(())
}

/// JSON shape the Admin > Settings > SMS panel reads, so it never has to know the envelope.
pub fn panel_view(cfg: &SmsConfig) -> serde_json::Value {
    json!({
        "provider": cfg.provider,
        "from_number": cfg.from_number,
        "api_url": cfg.api_url,
        "account_sid": cfg.account_sid,
        "configured": cfg.is_configured(),
        "providers": SMS_PROVIDERS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_phone_accepts_e164_and_refuses_guesses() {
        assert_eq!(normalize_phone("+15551234567").unwrap(), "+15551234567");
        assert_eq!(
            normalize_phone("+1 (555) 123-4567").unwrap(),
            "+15551234567"
        );
        assert_eq!(
            normalize_phone("0044 20 7946 0958").unwrap(),
            "+442079460958"
        );
        // A bare national number is REFUSED: guessing a country code sends an account's
        // notification to a stranger.
        assert!(normalize_phone("5551234567").is_err());
        assert!(normalize_phone("+0123456789").is_err());
        assert!(normalize_phone("+123").is_err());
        assert!(normalize_phone("").is_err());
    }

    #[test]
    fn is_configured_requires_the_fields_the_provider_arm_needs() {
        let mut c = SmsConfig {
            provider: "twilio".into(),
            api_key: "token".into(),
            account_sid: "AC123".into(),
            from_number: "+15550000000".into(),
            ..Default::default()
        };
        assert!(c.is_configured());
        // Half-filled Twilio (no Account SID) is NOT configured — it would 404 at send time.
        c.account_sid.clear();
        assert!(!c.is_configured());
        // An unknown provider has no arm at all.
        let unknown = SmsConfig {
            provider: "carrier-pigeon".into(),
            api_key: "k".into(),
            from_number: "+15550000000".into(),
            ..Default::default()
        };
        assert!(!unknown.is_configured());
        // No From number is not configured, whatever else is set.
        let no_from = SmsConfig {
            provider: "vonage".into(),
            api_key: "k".into(),
            api_secret: "s".into(),
            from_number: String::new(),
            ..Default::default()
        };
        assert!(!no_from.is_configured());
    }
}
