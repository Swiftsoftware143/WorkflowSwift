//! `credential_watch` — tell the OPERATOR when a customer's first password never reached them.
//!
//! WHY THIS EXISTS: `auth::register` mints the workspace, the owner and the mail carrying the first
//! password, and latches `users.password_undelivered` when that send does not go. The admin console
//! badges the row ("password never delivered") and its "Send login" button is the door that mints a
//! new password and mails it again
//! (`POST /api/v1/admin/accounts/:id/resend-credentials`). But a badge is only read by somebody who
//! happens to open the Accounts screen, and NOBODY WAS EVER TOLD. The measured incident that produced
//! this worker (ADASwift, 2026-11-15) is exactly this shape: a real customer signed up, the mail
//! carrying the first password never left the box, and the account sat unusable for two days.
//!
//! Ported from `missedcallrespondr` (kanban t_177ac7ce, 2026-11-16) and adapted to THIS app:
//!
//! * Sends go through the one funnel — `email::send_email(state, None, to, TEMPLATE, &vars)` — as
//!   system mail (`None`), so the alert arrives even when the affected workspace's own mail path is
//!   the broken thing, and the operator's mail history records it like every other system send.
//! * The once-per-account dedupe keys on `email_send_log.template` + `to_email` + `status = 'sent'`
//!   and requires the account's id in `detail`, because this app's log has NO subject column. The
//!   id is written onto the funnel's own row by
//!   `email::send_undelivered_credential_operator_alert`, and only after that row reads back `sent`.
//! * No `tenants` table: `users.aid` names the workspace. The name the operator recognises is
//!   `accounts.name`; `users.company` is the customer's own field and `users.email` is the last
//!   resort, so the alert never invents a label.
//!
//! Two rules decide the design, both learned the hard way on the sibling apps:
//!
//! * **"Already reported" means the operator was actually MAILED**, not that a row exists. A send
//!   the transport withholds (a reserved test address, an unconfigured provider) writes a row too,
//!   and treating that as a report retires the account for ever. So the dedupe requires status
//!   `sent`, and a pass whose own row comes back `withheld` leaves the account unreported so the
//!   next pass retries.
//! * **Machinery must never ring a human.** A harness signup always lands with the latch set — its
//!   credentials mail is suppressed by construction — so without a gate this watch would mail the
//!   operator once per probe signup. The gate is on the REQUESTER (the customer identity and the
//!   workspace name), never on a route: the same door serves real customers.

use crate::state::AppState;
use uuid::Uuid;

/// How often the watch looks. Fifteen minutes bounds how long a locked-out customer waits for a
/// human to be told, and costs one cheap indexed read when there is nothing to report.
pub const WATCH_INTERVAL_SECS: u64 = 15 * 60;

/// Grace period. A signup whose mail is still in flight, or a mail layer that is momentarily slow,
/// must not page the operator for a customer who is about to receive their password anyway.
pub const MIN_AGE_MINUTES: i64 = 30;

/// How far back the watch looks. A latch set longer ago than this belongs to an account the
/// operator has already dealt with by other means; two weeks is the window in which the arrival is
/// still news.
pub const LOOKBACK_DAYS: i64 = 14;

/// Accounts per pass, so one pass never sends an unbounded burst of mail.
pub const BATCH: i64 = 5;

/// The `email_templates.template_type` of the alert (migration 116) — and therefore the key the
/// once-per-account dedupe reads back out of `email_send_log`. ONE definition: the sender in
/// `email::send_undelivered_credential_operator_alert` and the selector below both use it.
pub const TEMPLATE: &str = "undelivered_credential_operator_alert";

/// The selector, as ONE literal (gate rule 5d: a query is never BUILT at run time).
///
/// The `'{TEMPLATE}'` key below is spelled out; `template_and_selector_agree` makes drift between
/// this statement and [`TEMPLATE`] — the value the sender logs — a failing test rather than a watch
/// that reports the same customer every fifteen minutes.
///
/// Binds: $1 grace minutes, $2 lookback days, $3 batch size, $4 the operator address.
const SELECTOR: &str = r#"
        SELECT u.id,
               u.email,
               COALESCE(NULLIF(btrim(a.name), ''), NULLIF(btrim(u.company), ''), u.email),
               COALESCE(NULLIF(btrim(u.name), ''), ''),
               to_char(u.created_at, 'YYYY-MM-DD')
          FROM users u
          JOIN accounts a ON a.id = u.aid
         WHERE u.password_undelivered
           AND u.closed_at IS NULL
           AND u.created_at < now() - make_interval(mins => $1::int)
           AND u.created_at > now() - make_interval(days => $2::int)
           -- already reported: a row for THIS account that actually reached the operator we are
           -- about to mail. A withheld row (reserved recipient, provider not configured) is not a
           -- report, and neither is a row addressed to anyone else.
           AND NOT EXISTS (
                 SELECT 1 FROM email_send_log l
                  WHERE l.template = 'undelivered_credential_operator_alert'
                    AND l.to_email = $4
                    AND l.status = 'sent'
                    AND l.detail LIKE '%' || u.id::text || '%')
         ORDER BY u.created_at ASC
         LIMIT $3
        "#;

/// Boot-time worker, started from `main` next to the probe sweeper and the execution worker. A
/// failure inside one pass is logged and the loop continues — an alerter must never take the app
/// down.
pub fn spawn_credential_watch(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(WATCH_INTERVAL_SECS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // The first tick fires immediately, so a deploy reports the existing backlog instead of
            // waiting a quarter of an hour.
            ticker.tick().await;
            match alert_once(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(reported = n, "undelivered-credential alert"),
                Err(e) => tracing::warn!(error = %e, "undelivered-credential watch failed"),
            }
        }
    });
}

/// Report at most [`BATCH`] unreported accounts whose first password never reached them.
///
/// Returns how many alerts actually reached the operator. A withheld send does not count, so the
/// number is "a human was told", not "a function ran".
pub async fn alert_once(state: &AppState) -> Result<usize, String> {
    // WHO the alert goes to is resolved BEFORE the selector, because "already reported" has to mean
    // "delivered to THIS operator". No configured address means nothing to send to, and this code
    // never invents a mailbox.
    let Some(to) = crate::email::operator_alert_email(state).await else {
        return Ok(0);
    };

    let rows: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(SELECTOR)
        .bind(MIN_AGE_MINUTES)
        .bind(LOOKBACK_DAYS)
        .bind(BATCH)
        .bind(&to)
        .fetch_all(&state.db)
        .await
        .map_err(|e| format!("select undelivered credentials: {e}"))?;

    if rows.is_empty() {
        return Ok(0);
    }

    let mut reported = 0usize;
    for (user_id, customer_email, workspace, owner_name, signed_up) in rows {
        // A probe signup always carries the latch (its mail cannot be delivered by construction),
        // so this gate is what stops the watch mailing a human once per harness run. Requester, not
        // route: the same door serves real customers. Both the customer identity and the workspace
        // name are judged, because a harness workspace can carry a clean-looking address.
        let synthetic =
            crate::security::probe_addr::synthetic_requester(&customer_email, &owner_name)
                .or_else(|| crate::security::probe_addr::synthetic_requester("", &workspace));
        if let Some(reason) = synthetic {
            tracing::info!(
                user = %user_id,
                reason = %reason,
                "undelivered-credential alert WITHHELD — the account is a synthetic identity"
            );
            continue;
        }

        match crate::email::send_undelivered_credential_operator_alert(
            state,
            user_id,
            &customer_email,
            &workspace,
            &owner_name,
            &signed_up,
            &to,
        )
        .await
        {
            Ok(status) if status == "withheld" => {
                // The funnel answered Ok for a recipient the transport withholds. The account stays
                // UNREPORTED so a later pass retries once the operator address is a real mailbox,
                // instead of being silently retired for ever.
                tracing::warn!(
                    user = %user_id,
                    to = %to,
                    "undelivered-credential alert WITHHELD — the operator address cannot receive \
                     mail, so this account stays unreported and the next pass retries"
                );
            }
            Ok(status) if status == "sent" => {
                tracing::info!(user = %user_id, "reported an undelivered credential to the operator");
                reported += 1;
            }
            Ok(status) => {
                tracing::warn!(
                    user = %user_id,
                    status = %status,
                    "undelivered-credential alert did not reach the operator; the next pass retries"
                );
            }
            Err(e) => {
                tracing::warn!(user = %user_id, error = %e, "undelivered-credential alert failed");
            }
        }
    }
    Ok(reported)
}

#[cfg(test)]
mod selector_tests {
    use super::*;

    /// The statement keys the dedupe on the SAME template value the sender logs. Drift either way —
    /// a renamed const, a hand-edited SQL — would either re-report one customer every fifteen
    /// minutes or never report at all, and neither shows up as a compile error.
    #[test]
    fn template_and_selector_agree() {
        assert_eq!(TEMPLATE, "undelivered_credential_operator_alert");
        assert!(SELECTOR.contains("'undelivered_credential_operator_alert'"));
    }

    /// The four values the caller binds, in the order the statement names them: grace minutes,
    /// lookback days, batch, operator address. A fifth hole would be a bind the caller never makes.
    #[test]
    fn binds_are_named_in_order() {
        for hole in ["$1", "$2", "$3", "$4"] {
            assert!(SELECTOR.contains(hole), "{hole} missing from the selector");
        }
        assert!(!SELECTOR.contains("$5"));
    }
}
