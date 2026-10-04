//! Notify delivery — the ONE place a Notify step's outbound message is actually sent
//! (kanban t_d3ff37ef).
//!
//! Both doors into a Notify step's send go through here:
//!
//! * the **n8n mirror**, whose generated graph POSTs `POST /api/v1/notify/dispatch`
//!   (`handlers::notify_handler`), and
//! * the **in-process engine** (`execution::walk`'s `notify` arm).
//!
//! Keeping one implementation is what makes the two agree, and it is where the card's two
//! non-negotiable rules live:
//!
//! 1. **The account's own people ONLY.** An `email` step reaches the account's own users and its
//!    billing contact; an `sms` step reaches those same people's `users.phone`. A step whose
//!    stored config still names a free-text address or number is REFUSED — never sent. This
//!    product must never become an open outbound relay on its own sending domain.
//! 2. **FAIL-CLOSED.** A channel with no configured sender is refused, at the write path and
//!    again here, so nothing is ever offered that cannot be delivered.
//!
//! Plus the throttle: a per-account hourly cap so a runaway workflow cannot mail-bomb. A
//! throttled send is RECORDED (`notify_send_attempts.status = 'throttled'`) and reported as such
//! — never a silent `completed`.

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

/// Reach every active person on the account.
pub const SCOPE_ALL_USERS: &str = "all_users";
/// Reach the account's billing contact (its admin user).
pub const SCOPE_BILLING_CONTACT: &str = "billing_contact";
/// Reach ONE named person on the account — `recipient_user_id` names them.
pub const SCOPE_USER: &str = "user";

/// Every recipient scope the console offers and the write path accepts. ONE list: the picker
/// renders exactly this, and `resolve_recipients` has an arm for each.
pub const NOTIFY_SCOPES: &[&str] = &[SCOPE_ALL_USERS, SCOPE_BILLING_CONTACT, SCOPE_USER];

/// Install-wide fallback cap when neither the account's plan nor the Limits setting names one.
pub const DEFAULT_NOTIFY_PER_HOUR: i64 = 200;

/// Which sender-backed channels this install can actually deliver on right now. Computed from
/// `admin_settings.email` / `admin_settings.sms` — the SAME predicate the write path gates on, so
/// the console can only ever offer a channel that will really send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotifySenders {
    pub email: bool,
    pub sms: bool,
}

impl NotifySenders {
    pub fn available(&self, channel: &str) -> bool {
        match channel {
            "webhook" => true,
            "email" => self.email,
            "sms" => self.sms,
            _ => false,
        }
    }

    /// The channels to OFFER — exactly the ones that can be delivered. `webhook` is always
    /// available (it is an outbound call to a URL the tenant owns and needs no provider).
    pub fn offered(&self) -> Vec<&'static str> {
        let mut out = vec!["webhook"];
        if self.email {
            out.push("email");
        }
        if self.sms {
            out.push("sms");
        }
        out
    }
}

/// Read both sender configurations once per request.
pub async fn load_senders(state: &AppState) -> NotifySenders {
    let email = crate::email::is_configured(state).await;
    let sms = crate::sms::is_configured(state).await;
    NotifySenders { email, sms }
}

/// One resolved destination — always a person who belongs to the account the workflow belongs to.
#[derive(Debug, Clone)]
pub struct Recipient {
    pub user_id: Uuid,
    pub name: String,
    pub email: String,
    pub phone: String,
    pub billing_contact: bool,
}

impl Recipient {
    /// The address this channel writes to, or empty when this person has none on file.
    fn address(&self, channel: &str) -> String {
        match channel {
            "email" => self.email.clone(),
            "sms" => self.phone.clone(),
            _ => String::new(),
        }
    }
}

/// Resolve a Notify step's destination set, ENFORCING the account's-own-people rule.
///
/// A step that still carries the retired free-text `recipient` (an address or number the step
/// itself names) is REFUSED here — that is the negative case the card requires: nothing is sent,
/// the failure is named.
pub async fn resolve_recipients(
    pool: &PgPool,
    aid: Uuid,
    channel: &str,
    config: &Value,
) -> Result<Vec<Recipient>, String> {
    let scope = config
        .get("recipient_scope")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    if scope.is_empty() {
        let legacy = config
            .get("recipient")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if !legacy.is_empty() {
            return Err(format!(
                "Notify step names the free-text destination '{legacy}'. A {channel} step may \
                 only reach the account's own people — pick a recipient (all account users, the \
                 billing contact, or one named account user)."
            ));
        }
        return Err(
            "Notify step names no recipient. Pick one of the account's own people (all account \
             users, the billing contact, or one named account user)."
                .to_string(),
        );
    }

    if !NOTIFY_SCOPES.contains(&scope.as_str()) {
        return Err(format!(
            "unknown recipient scope '{scope}'. Valid scopes: {}",
            NOTIFY_SCOPES.join(", ")
        ));
    }

    // Every query below is scoped to `aid`: the only people a step may reach are the account's.
    let rows: Vec<(Uuid, String, String, Option<String>, String)> = match scope.as_str() {
        SCOPE_USER => {
            let raw = config
                .get("recipient_user_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let user_id = Uuid::parse_str(raw).map_err(|_| {
                "recipient_scope is 'user' but recipient_user_id is not a user".to_string()
            })?;
            sqlx::query_as(
                "SELECT id, name, email, phone, role FROM users \
                 WHERE aid = $1 AND id = $2 AND is_active = true",
            )
            .bind(aid)
            .bind(user_id)
            .fetch_all(pool)
            .await
            .map_err(|e| format!("could not resolve the named account user: {e}"))?
        }
        SCOPE_BILLING_CONTACT => {
            // The account's billing contact is its admin user. A second query covers the case
            // where the account has no company_admin row: the oldest active person on it.
            let admin: Vec<(Uuid, String, String, Option<String>, String)> = sqlx::query_as(
                "SELECT id, name, email, phone, role FROM users \
                 WHERE aid = $1 AND is_active = true AND role = 'company_admin' \
                 ORDER BY created_at LIMIT 1",
            )
            .bind(aid)
            .fetch_all(pool)
            .await
            .map_err(|e| format!("could not resolve the account's billing contact: {e}"))?;
            if !admin.is_empty() {
                admin
            } else {
                sqlx::query_as(
                    "SELECT id, name, email, phone, role FROM users \
                     WHERE aid = $1 AND is_active = true ORDER BY created_at LIMIT 1",
                )
                .bind(aid)
                .fetch_all(pool)
                .await
                .map_err(|e| format!("could not resolve the account's billing contact: {e}"))?
            }
        }
        // SCOPE_ALL_USERS
        _ => sqlx::query_as(
            "SELECT id, name, email, phone, role FROM users \
             WHERE aid = $1 AND is_active = true ORDER BY created_at",
        )
        .bind(aid)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("could not resolve the account's people: {e}"))?,
    };

    let mut out: Vec<Recipient> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (id, name, email, phone, role) in rows {
        let r = Recipient {
            user_id: id,
            name,
            email: email.trim().to_lowercase(),
            phone: phone.unwrap_or_default(),
            billing_contact: role == "company_admin",
        };
        let addr = r.address(channel);
        if addr.is_empty() {
            continue;
        }
        if channel == "sms" {
            if let Err(e) = crate::sms::normalize_phone(&addr) {
                return Err(format!(
                    "'{}' has a phone number this product cannot dial: {e} — fix it on the \
                     account's user record.",
                    if r.name.is_empty() { &r.email } else { &r.name }
                ));
            }
        }
        if seen.contains(&addr) {
            continue;
        }
        seen.push(addr);
        out.push(r);
    }

    if out.is_empty() {
        return Err(match channel {
            "sms" => "none of the account's people has a phone number on file — add one to the \
                      account's user records first"
                .to_string(),
            _ => "the account has no active user to reach".to_string(),
        });
    }
    Ok(out)
}

/// The hourly cap for one account: its plan's `max_notify_per_hour`, else the install-wide
/// `admin_settings.limits.notify_per_hour`, else [`DEFAULT_NOTIFY_PER_HOUR`]. `-1` means unlimited.
pub async fn per_hour_cap(pool: &PgPool, aid: Uuid) -> i64 {
    let plan: Option<i64> = sqlx::query_scalar(
        "SELECT t.max_notify_per_hour FROM account_plans p \
         JOIN plan_tiers t ON t.id = p.plan_id \
         WHERE p.aid = $1 AND p.is_active = true AND p.status = 'active' \
         ORDER BY p.created_at DESC LIMIT 1",
    )
    .bind(aid)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    if let Some(v) = plan {
        return v;
    }

    let global: Option<String> = sqlx::query_scalar(
        "SELECT value->>'notify_per_hour' FROM admin_settings WHERE key = 'limits'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    if let Some(v) = global.as_deref().and_then(|s| s.parse::<i64>().ok()) {
        return v;
    }

    DEFAULT_NOTIFY_PER_HOUR
}

/// Sends this account has made in the current rolling hour. Only `sent` rows count: a refusal or
/// a throttle must not consume the account's own budget.
pub async fn sends_last_hour(pool: &PgPool, aid: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM notify_send_attempts \
         WHERE aid = $1 AND status = 'sent' AND created_at > now() - interval '1 hour'",
    )
    .bind(aid)
    .fetch_one(pool)
    .await
    .unwrap_or(0)
}

/// One row of `notify_send_attempts` — the audit trail AND the throttle counter, in one shape.
pub struct Attempt<'a> {
    pub aid: Uuid,
    pub workflow_id: Option<Uuid>,
    pub step_index: Option<i32>,
    pub channel: &'a str,
    pub recipient: &'a str,
    /// `sent` | `failed` | `refused` | `throttled` — the SAME vocabulary `NotifyOutcome` reports.
    pub status: &'a str,
    pub detail: &'a str,
}

/// Record one dispatch attempt. Every outcome of every Notify send lands here — this table is
/// both the audit trail and the throttle counter, which is why a throttled or refused send can
/// never be reported as a silent `completed`.
pub async fn record_attempt(pool: &PgPool, a: Attempt<'_>) {
    let _ = sqlx::query(
        "INSERT INTO notify_send_attempts \
         (aid, workflow_id, step_index, channel, recipient, status, detail) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(a.aid)
    .bind(a.workflow_id)
    .bind(a.step_index)
    .bind(a.channel)
    .bind(a.recipient)
    .bind(a.status)
    .bind(a.detail)
    .execute(pool)
    .await;
}

/// The outcome of one `deliver` call. `status` is the SAME vocabulary `notify_send_attempts`
/// stores, so the record and the report cannot drift.
#[derive(Debug, Clone)]
pub struct NotifyOutcome {
    pub status: String,
    pub channel: String,
    pub detail: String,
    pub recipients: Vec<Value>,
}

impl NotifyOutcome {
    pub fn is_sent(&self) -> bool {
        self.status == "sent"
    }

    /// The step-result JSON both doors embed, so the console and the run log read the same shape.
    pub fn to_step_json(&self, step_index: usize) -> Value {
        json!({
            "step": step_index,
            "type": "notify",
            "status": self.status,
            "channel": self.channel,
            "detail": self.detail,
            "recipients": self.recipients,
        })
    }
}

/// Send a Notify step's message over `email` or `sms`, to the account's own people only.
///
/// The caller has usually already checked `senders.available(channel)`; this re-checks, because
/// fail-closed is the whole point and a route is not a reason to relax it.
pub async fn deliver(
    state: &AppState,
    aid: Uuid,
    workflow_id: Option<Uuid>,
    step_index: Option<i32>,
    channel: &str,
    config: &Value,
) -> NotifyOutcome {
    let refused = |detail: String| NotifyOutcome {
        status: "refused".to_string(),
        channel: channel.to_string(),
        detail,
        recipients: vec![],
    };

    if !matches!(channel, "email" | "sms") {
        return refused(format!(
            "notify channel '{channel}' is not a deliverable email/SMS channel"
        ));
    }

    let senders = load_senders(state).await;
    if !senders.available(channel) {
        return refused(format!(
            "no {channel} sender is configured on this install (Admin > Settings > {})",
            if channel == "sms" { "SMS" } else { "Email" }
        ));
    }

    let recipients = match resolve_recipients(&state.db, aid, channel, config).await {
        Ok(r) => r,
        Err(e) => {
            record_attempt(
                &state.db,
                Attempt {
                    aid,
                    workflow_id,
                    step_index,
                    channel,
                    recipient: "",
                    status: "refused",
                    detail: &e,
                },
            )
            .await;
            return refused(e);
        }
    };

    // ── Throttle ────────────────────────────────────────────────────────────────────────────
    let cap = per_hour_cap(&state.db, aid).await;
    if cap != -1 {
        let used = sends_last_hour(&state.db, aid).await;
        if used >= cap {
            let detail = format!(
                "account hourly Notify cap reached ({used}/{cap} in the last hour) — nothing was \
                 sent"
            );
            record_attempt(
                &state.db,
                Attempt {
                    aid,
                    workflow_id,
                    step_index,
                    channel,
                    recipient: "",
                    status: "throttled",
                    detail: &detail,
                },
            )
            .await;
            return NotifyOutcome {
                status: "throttled".to_string(),
                channel: channel.to_string(),
                detail,
                recipients: vec![],
            };
        }
    }

    // ── Send, one record per recipient ──────────────────────────────────────────────────────
    let message = config.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let subject = config
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("WorkflowSwift notification");

    let mut results: Vec<Value> = Vec::new();
    let mut failures = 0usize;
    let mut first_error = String::new();
    for r in &recipients {
        let addr = r.address(channel);
        let sent = if channel == "email" {
            let vars = json!({
                "name": r.name,
                "email": r.email,
                "message": message,
                "subject": subject,
                "app_url": "https://app.workflowswift.com",
            });
            crate::email::send_email(state, &addr, "workflow_notify", &vars).await
        } else {
            let body = if message.is_empty() {
                "WorkflowSwift notification".to_string()
            } else {
                message.to_string()
            };
            crate::sms::send_sms(state, &addr, &body).await
        };

        let (status, detail) = match sent {
            Ok(()) => ("sent".to_string(), String::new()),
            Err(e) => {
                failures += 1;
                if first_error.is_empty() {
                    first_error = e.clone();
                }
                ("failed".to_string(), e)
            }
        };
        record_attempt(
            &state.db,
            Attempt {
                aid,
                workflow_id,
                step_index,
                channel,
                recipient: &addr,
                status: &status,
                detail: &detail,
            },
        )
        .await;
        results.push(json!({
            "user_id": r.user_id.to_string(),
            "name": r.name,
            "to": addr,
            "billing_contact": r.billing_contact,
            "status": status,
            "detail": detail,
        }));
    }

    let status = if failures == 0 { "sent" } else { "failed" };
    let detail = if failures == 0 {
        format!(
            "{channel} sent to {} of the account's own people",
            results.len()
        )
    } else {
        format!(
            "{failures} of {} {channel} sends failed: {first_error}",
            results.len()
        )
    };
    NotifyOutcome {
        status: status.to_string(),
        channel: channel.to_string(),
        detail,
        recipients: results,
    }
}

/// Map an outcome to the HTTP answer the dispatch route gives. `throttled` is a 429 (the card:
/// "the route answers 429 past the cap"); a refusal is a 4xx, never a 2xx.
pub fn outcome_error(outcome: &NotifyOutcome) -> Option<AppError> {
    match outcome.status.as_str() {
        "sent" => None,
        "throttled" => Some(AppError::TooManyRequests(outcome.detail.clone())),
        "refused" => Some(AppError::BadRequest(outcome.detail.clone())),
        _ => Some(AppError::Upstream(outcome.detail.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn senders_offer_only_what_can_be_delivered() {
        let none = NotifySenders {
            email: false,
            sms: false,
        };
        // `webhook` needs no provider and stays available; the other two must not be offered.
        assert_eq!(none.offered(), vec!["webhook"]);
        assert!(none.available("webhook"));
        assert!(!none.available("email"));
        assert!(!none.available("sms"));

        let both = NotifySenders {
            email: true,
            sms: true,
        };
        assert_eq!(both.offered(), vec!["webhook", "email", "sms"]);

        let email_only = NotifySenders {
            email: true,
            sms: false,
        };
        assert_eq!(email_only.offered(), vec!["webhook", "email"]);
        assert!(!email_only.available("sms"));

        // A channel nobody has a sender for (or a retired one) is never "available".
        assert!(!both.available("slack"));
        assert!(!both.available("carrier-pigeon"));
    }

    #[test]
    fn outcome_maps_throttle_to_429_and_refusal_to_4xx() {
        let mk = |status: &str, detail: &str| NotifyOutcome {
            status: status.to_string(),
            channel: "email".to_string(),
            detail: detail.to_string(),
            recipients: vec![],
        };
        assert!(outcome_error(&mk("sent", "")).is_none());
        assert!(matches!(
            outcome_error(&mk("throttled", "cap")),
            Some(AppError::TooManyRequests(_))
        ));
        assert!(matches!(
            outcome_error(&mk("refused", "stranger")),
            Some(AppError::BadRequest(_))
        ));
        assert!(matches!(
            outcome_error(&mk("failed", "smtp 554")),
            Some(AppError::Upstream(_))
        ));
    }

    #[test]
    fn scopes_are_the_three_the_console_offers() {
        assert_eq!(NOTIFY_SCOPES.len(), 3);
        assert!(NOTIFY_SCOPES.contains(&SCOPE_ALL_USERS));
        assert!(NOTIFY_SCOPES.contains(&SCOPE_BILLING_CONTACT));
        assert!(NOTIFY_SCOPES.contains(&SCOPE_USER));
    }
}
