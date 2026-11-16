//! "How to pay" — the operator's own payment details, for an install with no card provider.
//!
//! `payment_providers` holds ZERO rows on this installation, so `POST /api/v1/checkout/create`
//! refuses every paid tier and a plan REQUEST is the buying path: the customer presses a plan, a
//! `tickets` row with `source = 'plan_request'` is filed, and the receipt says a person will reply.
//! What was missing is the last step — a customer who is ready to send money had nowhere to send it,
//! because nothing in the product ever told them how this business takes payment (a bank transfer,
//! an invoice, another rail).
//!
//! The operator writes that once, in his own words, on the Payment Providers screen. The same words
//! then reach the customer on two surfaces:
//!
//!   * `GET /api/v1/checkout/status` carries them, so the Plan screen shows them beside the plan
//!     ladder — including after a reload;
//!   * the receipt mail for a filed plan request prints them as "How to pay now:".
//!
//! Absent or empty stays exactly as before: nothing is shown and the mail is unchanged. That is the
//! feature switch, and it is the operator's — this module never invents payment details.
//!
//! Stored in `admin_settings` key `billing` as `{"payment_instructions": "..."}`. That key ALREADY
//! carries other fields on this install (`enabled`, `currency`, `tax_rate`, `grace_period_days`,
//! `operator_alert_email`), so the writer MERGES this one field into the row (jsonb `||`); a save
//! from this screen must never drop the operator alert address the plan-request queue mails.
//!
//! Fleet parity: FunnelSwift shipped this on 2026-11-16 12:07, IncentiveSwift at 12:27 and ADASwift
//! at 14:19 the same day; this is the WorkflowSwift side.

use sqlx::PgPool;

/// The longest blob the console may store. Generous enough for bank details plus a paragraph of
/// guidance, small enough that it cannot be used as a message board.
pub const MAX_LEN: usize = 4000;

/// The operator's saved words, or `None` when the feature is off (absent row, absent field, or an
/// empty/whitespace string). Callers treat `None` as "say nothing new".
pub async fn load(pool: &PgPool) -> Option<String> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT value->>'payment_instructions' FROM admin_settings WHERE key = 'billing'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Save the operator's words. An empty string is a deliberate CLEAR (the feature goes back to off),
/// never a silent no-op.
pub async fn save(pool: &PgPool, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.chars().count() > MAX_LEN {
        return Err(format!(
            "The payment instructions are too long ({} characters). Keep them under {}.",
            text.chars().count(),
            MAX_LEN
        ));
    }
    sqlx::query(
        "INSERT INTO admin_settings (key, value, description, updated_at)
         VALUES ('billing',
                 jsonb_build_object('payment_instructions', $1::text),
                 'Platform billing settings (admin-editable): how a customer pays while card payments are off',
                 NOW())
         ON CONFLICT (key) DO UPDATE
            SET value = admin_settings.value || jsonb_build_object('payment_instructions', $1::text),
                updated_at = NOW()",
    )
    .bind(text)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to save the payment instructions: {e}"))?;
    Ok(())
}
