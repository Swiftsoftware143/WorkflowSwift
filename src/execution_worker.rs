//! The execution worker: the thing that can finally ADVANCE a waiting step.
//!
//! Until this module existed, a workflow whose step 2 was a `delay`/`wait` stopped
//! there forever. The engine (`src/execution.rs`) finishes the walk, leaves the
//! step `pending` with `due_date` set, marks the instance `pending` with
//! `completed_at = NULL` — and nothing in the process ever looked at it again
//! (`workflow_trigger_queue` from migration 039 was referenced by no code at all;
//! `main.rs` spawned no background task). Kanban t_1ff4b916.
//!
//! This module is deliberately thin: it owns the TIMER, not the execution. When a
//! delay comes due it claims the row and calls `execution::resume_instance`, so
//! every step still runs through the one engine (no second executor, and no second
//! credit charge — billing happens once, at trigger time).
//!
//! Two workers (say, the container plus a scratch instance booted against the same
//! database) cannot double-advance a step: the claim is a single conditional
//! `UPDATE ... WHERE status = 'pending'`, and only the caller that changed a row
//! proceeds.

use std::collections::HashSet;
use std::time::Duration;

use uuid::Uuid;

use crate::error::AppError;
use crate::execution::{resume_instance, AdvanceRequest, Decision};
use crate::state::AppState;

/// How many due steps one tick may advance. Bounded so a backlog cannot make a
/// single tick (or a single DB transaction) unbounded.
const BATCH: i64 = 25;

/// Spawn the worker on the current tokio runtime. Called once from `main`.
///
/// `EXEC_WORKER_DISABLED=1` turns it off for a process that must never settle
/// production rows (a read-only probe, the smoke harness's scratch boot).
pub fn spawn(state: AppState) {
    if std::env::var("EXEC_WORKER_DISABLED")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        })
        .unwrap_or(false)
    {
        tracing::info!(
            "execution worker DISABLED (EXEC_WORKER_DISABLED) — waiting steps stay pending"
        );
        return;
    }

    let interval_secs = std::env::var("EXEC_WORKER_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(5);

    tokio::spawn(async move {
        tracing::info!(
            interval_secs,
            "execution worker started — delay/wait steps advance on their due_date"
        );

        // Steps that already failed to advance are logged once, not every tick:
        // a stuck row must not turn into a log flood.
        let mut reported: HashSet<Uuid> = HashSet::new();

        loop {
            match tick(&state, &mut reported).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(advanced = n, "execution worker advanced waiting steps"),
                Err(e) => tracing::error!(error = %e, "execution worker tick failed"),
            }
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
    });
}

/// One pass: settle every `delay`/`wait` step whose due time has passed.
async fn tick(state: &AppState, reported: &mut HashSet<Uuid>) -> Result<usize, AppError> {
    let due = sqlx::query_as::<_, (Uuid, Uuid)>(
        r#"SELECT s.id, s.instance_id
           FROM workflow_instance_steps s
           JOIN workflow_instances i ON i.id = s.instance_id
           WHERE s.status = 'pending'
             AND s.step_type IN ('delay', 'wait')
             AND s.due_date IS NOT NULL
             AND s.due_date <= NOW()
             AND i.status NOT IN ('completed', 'failed')
           ORDER BY s.due_date ASC
           LIMIT $1"#,
    )
    .bind(BATCH)
    .fetch_all(&state.db)
    .await?;

    let mut advanced = 0usize;

    for (step_id, instance_id) in due {
        // Atomic claim. Exactly one worker in the fleet sees rows_affected = 1.
        let claimed = sqlx::query(
            "UPDATE workflow_instance_steps SET status = 'in_progress' WHERE id = $1 AND status = 'pending'",
        )
        .bind(step_id)
        .execute(&state.db)
        .await?
        .rows_affected();

        if claimed == 0 {
            continue;
        }

        let advance = AdvanceRequest {
            step_instance_id: step_id,
            decision: Decision::Approve,
        };

        match resume_instance(state, instance_id, Some(advance), "worker").await {
            Ok(outcome) => {
                advanced += 1;
                reported.remove(&step_id);
                tracing::info!(
                    instance_id = %instance_id,
                    step_id = %step_id,
                    instance_status = %outcome.status,
                    pending_steps = outcome.pending_steps,
                    failed_steps = outcome.failed_steps,
                    "due delay step advanced"
                );
            }
            Err(e) => {
                // Give the step back so a transient failure is retried on the next
                // tick, but report it only once per process.
                let _ = sqlx::query(
                    "UPDATE workflow_instance_steps SET status = 'pending' WHERE id = $1 AND status = 'in_progress'",
                )
                .bind(step_id)
                .execute(&state.db)
                .await;

                if reported.insert(step_id) {
                    tracing::error!(
                        instance_id = %instance_id,
                        step_id = %step_id,
                        error = %e,
                        "due delay step could not be advanced — left pending for retry"
                    );
                }
            }
        }
    }

    Ok(advanced)
}
