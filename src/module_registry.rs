//! Plan resolution for the private-inbox module (kanban t_473a6895).
//!
//! The inbox was ported from CoreSwift, where a `module_registry` table answers "does this account's
//! plan include private email, and what are its numeric limits". WorkflowSwift has no such table —
//! its catalogue is `plan_tiers.features` (JSONB), reached through the app's OWN resolver
//! [`crate::features::resolve_plan_id`] (an active `account_plans` row, else `accounts.plan_id`,
//! else the cheapest active tier). This module answers the SAME questions from the data this app
//! already sells:
//!
//! * `private_email` module: granted when the account resolves to an ACTIVE tier. With no tier at
//!   all the answer is `no_plan` and the inbox is refused exactly as CoreSwift refuses it (an inbox
//!   is a sold feature; "no plan" is not a plan).
//! * numeric limits (`email_domains`, `email_mailboxes`): read
//!   `plan_tiers.features->>'max_domains'` / `->>'max_mailboxes'`. A plan that states no number is
//!   UNLIMITED — the ceiling exists only where it was sold, and inventing a default here would
//!   refuse a customer for a limit nobody sold them. `tenant_email_limits` overrides still win,
//!   because that table is the operator's own per-account tool and it is consulted before the plan.
//!
//! Deliberately small: three call sites in `private_email::feature_gate`, one query each, no cache
//! to go stale when an operator moves a workspace between plans.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;

/// What one feature resolves to for a tenant.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// May this tenant use the feature at all?
    pub enabled: bool,
    /// Where the answer came from: `plan`, `override`, or `no_plan`. Callers treat `no_plan` as
    /// "refused" even when `enabled` is tolerant, which is what keeps a plan-less account from
    /// receiving unlimited domains.
    pub source: &'static str,
    /// The numeric ceiling, when the feature is a limit. `None` = no ceiling stated.
    pub limit_value: Option<f64>,
}

impl Resolved {
    fn refused(source: &'static str) -> Self {
        Self {
            enabled: false,
            source,
            limit_value: None,
        }
    }
}

/// The account's active plan blob, through this app's ONE plan resolver.
///
/// `None` means the install has no active tier at all (a fresh, unseeded database): the honest
/// `no_plan` answer. An account that never chose a plan resolves to the cheapest active tier, which
/// is what every other gate in this app enforces — the resolver is shared so the inbox can never
/// disagree with the rest of the product about which plan an account is on.
async fn active_plan_features(pool: &PgPool, tenant_id: Uuid) -> Result<Option<Value>, AppError> {
    let Some(plan_id) = crate::features::resolve_plan_id(pool, tenant_id).await? else {
        return Ok(None);
    };
    let features: Option<Option<Value>> =
        sqlx::query_scalar("SELECT features FROM plan_tiers WHERE id = $1 AND is_active")
            .bind(plan_id)
            .fetch_optional(pool)
            .await
            .map_err(AppError::Database)?;
    Ok(features.flatten())
}

/// A number spelled in the plan blob, tolerant of numbers-as-strings (the admin editor stores both).
fn number_at(features: &Value, key: &str) -> Option<f64> {
    match features.get(key) {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Does the plan blob say the feature is granted? (`true`, or a non-zero numeric cap.)
fn granted_at(features: &Value, key: &str) -> bool {
    match features.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|v| v > 0.0).unwrap_or(false),
        Some(Value::String(s)) => matches!(s.trim(), "true" | "1" | "yes"),
        _ => false,
    }
}

/// Resolve one feature for one tenant — the ported module's `resolve(pool, tenant, key)`.
pub async fn resolve(pool: &PgPool, tenant_id: Uuid, key: &str) -> Result<Resolved, AppError> {
    // An operator's own per-account override is the most specific answer and always wins.
    let overrides: Option<(Option<i32>, Option<i32>, Option<i32>)> = sqlx::query_as(
        "SELECT max_domains, max_mailboxes, max_aliases_per_mailbox
           FROM tenant_email_limits WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?;

    let override_limit = overrides.and_then(|(domains, boxes, _aliases)| match key {
        "email_domains" => domains,
        "email_mailboxes" => boxes,
        _ => None,
    });
    if let Some(limit) = override_limit {
        return Ok(Resolved {
            enabled: limit != 0,
            source: "override",
            limit_value: Some(limit as f64),
        });
    }

    let Some(features) = active_plan_features(pool, tenant_id).await? else {
        return Ok(Resolved::refused("no_plan"));
    };

    match key {
        "private_email" => Ok(Resolved {
            enabled: true,
            source: "plan",
            limit_value: None,
        }),
        "email_domains" => Ok(Resolved {
            enabled: true,
            source: "plan",
            limit_value: number_at(&features, "max_domains"),
        }),
        "email_mailboxes" => Ok(Resolved {
            enabled: true,
            source: "plan",
            limit_value: number_at(&features, "max_mailboxes"),
        }),
        other => Ok(Resolved {
            enabled: granted_at(&features, other),
            source: "plan",
            limit_value: number_at(&features, other),
        }),
    }
}
