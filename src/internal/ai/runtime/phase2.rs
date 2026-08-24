//! Phase 2 Execution — formal write helpers (schema-only landing).
//!
//! The Code UI Phase Workflow models Phase 2 as the **Execution** phase: a
//! `TaskExecutionContext` is dispatched to a `TaskExecutor`, runs a
//! single task attempt, and produces a `TaskExecutionResult` alongside the
//! attempt-lifecycle formal writes (start / finish / patchset / evidence).
//!
//! # Runtime-owned contract, transitional storage
//!
//! [`AttemptWriteOutcome`], [`write_attempt_start`], and
//! [`write_attempt_finish`] are the pure Runtime-owned Phase 2 contract
//! surface. [`write_attempt_start_with_session`] and
//! [`write_attempt_finish_with_session`] currently delegate into
//! [`crate::internal::ai::orchestrator::persistence::ExecutionAuditSession`]
//! so the existing Run / TaskEvent / RunEvent / PlanStepEvent plumbing stays
//! in the orchestrator persistence layer while provider/UI callers target the
//! Runtime entry points. Once that storage code is folded into this module,
//! callers keep the same outcome type and lifecycle semantics.
//!
//! The important invariant is that a start write creates or reuses exactly one
//! persisted attempt run for the logical task, and a finish write appends the
//! terminal lifecycle facts against that same run id.

use uuid::Uuid;

use crate::internal::ai::{
    orchestrator::{
        persistence::ExecutionAuditSession,
        types::{OrchestratorError, TaskSpec},
    },
    runtime::contracts::TaskExecutionStatus,
};

/// Outcome of [`write_attempt_finish`]: the attempt's terminal status plus
/// the formal-write identifiers downstream observers / audit sinks need to
/// stitch the attempt back to its task / run rows.
///
/// **Stability contract:** field names are part of the public Runtime
/// surface once `write_attempt_finish` ships; downstream observers will
/// key off `task_id` / `run_id` / `status`. New fields may be added as
/// `Option<...>`; existing fields cannot be renamed or removed without a
/// parallel deprecation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptWriteOutcome {
    /// Task identifier the attempt belongs to (UUID assigned at intent
    /// canonicalisation time, stable across retries).
    pub task_id: Uuid,
    /// Run identifier persisted at attempt-start time; the row that the
    /// terminal status will be written against.
    pub run_id: Uuid,
    /// Terminal status of the attempt (matches the contracts-level enum so
    /// the Phase 2 path doesn't introduce parallel status semantics).
    pub status: TaskExecutionStatus,
    /// Optional human-readable summary of the attempt. Audit sinks may
    /// redact this through `SecretRedactor` before persisting; the schema
    /// stores the unredacted form so the boundary between Phase 2 and the
    /// audit layer stays explicit.
    pub summary: Option<String>,
}

impl AttemptWriteOutcome {
    /// `true` when the attempt ended in a non-`Completed` status. Used by
    /// Phase 3 validation routing to decide whether to escalate to a retry
    /// or open a `MergeCandidate`.
    ///
    /// **Note:** this returns `true` for the start-state marker
    /// `Interrupted` as well as the truly-terminal failure statuses; if the
    /// caller needs to distinguish "still in flight" from "finished with
    /// failure", combine this with [`is_terminal`](Self::is_terminal):
    ///
    /// ```text
    /// is_failure() && is_terminal()   => Failed | Cancelled | TimedOut
    /// is_failure() && !is_terminal()  => Interrupted (still in flight)
    /// !is_failure() && is_terminal()  => Completed
    /// ```
    pub fn is_failure(&self) -> bool {
        !matches!(self.status, TaskExecutionStatus::Completed)
    }

    /// `true` when the attempt has reached a **terminal** status —
    /// `Completed`, `Failed`, `Cancelled`, or `TimedOut`. `Interrupted`
    /// (the in-flight marker written by [`write_attempt_start`]) is
    /// explicitly NOT terminal: it signals "the run row exists but hasn't
    /// reached a clean terminal state yet".
    ///
    /// Phase 3 routing uses this to decide whether the attempt is ready
    /// for validation; non-terminal attempts must keep waiting on the
    /// executor.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            TaskExecutionStatus::Completed
                | TaskExecutionStatus::Failed
                | TaskExecutionStatus::Cancelled
                | TaskExecutionStatus::TimedOut
        )
    }
}

/// Inputs for [`write_attempt_start`]: the identity of the attempt being
/// started plus an optional reason / preamble that audit sinks can use
/// to correlate the start event with the task's prior history.
///
/// **Stability contract:** field names match the Runtime surface once
/// `write_attempt_start` lands a fully-stateful body. New fields may be
/// added as `Option<...>`; existing fields cannot be renamed or removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptStartParams {
    /// Task identifier the attempt belongs to.
    pub task_id: Uuid,
    /// Run identifier assigned at start time; the row that the eventual
    /// terminal status will be written against. The caller is
    /// responsible for generating this id (today via
    /// `Uuid::new_v4`); a future cutover will lift run-id allocation
    /// into this helper.
    pub run_id: Uuid,
    /// Optional human-readable reason / preamble (e.g. "retry after
    /// transient policy violation", "first attempt"). Audit sinks may
    /// redact this through `SecretRedactor` before persisting; the
    /// schema stores the unredacted form so the boundary between
    /// Phase 2 and the audit layer stays explicit.
    pub summary: Option<String>,
}

/// Record the **start** of a task attempt as the Phase 2 formal write.
///
/// This is a **pure function** at this stage: it constructs an
/// [`AttemptWriteOutcome`] tagged with [`TaskExecutionStatus::Interrupted`]
/// — the "we know the attempt started but no terminal status yet"
/// marker. Downstream callers should pair this with a later
/// [`write_attempt_finish`] call that supplies the real terminal
/// status. The split exists because the orchestrator persists run rows
/// at start time (so observers can see in-flight runs) and then
/// updates them at finish time.
///
/// The eventual stateful body will:
///
/// 1. Call into the orchestrator's existing
///    `orchestrator::persistence::create_run` /
///    `create_task_run` family with `status = "patching"` (executor
///    role) or `status = "validating"` (gate role).
/// 2. Return an `AttemptWriteOutcome { status: Interrupted }`
///    indicating the row exists but hasn't reached a terminal state.
///
/// Today the pure function builds the outcome without persistence so
/// the contract is callable from unit tests; the stateful bridge
/// follows the same pattern as
/// [`super::phase1::write_plan_set`](crate::internal::ai::runtime::phase1::write_plan_set).
///
/// # Why `Interrupted` as the start-state marker
///
/// `TaskExecutionStatus` has no dedicated "in flight" variant — the
/// closest semantic is `Interrupted`, which means "the run row exists
/// but didn't reach a clean terminal status". Picking `Interrupted` at
/// start time means readers that see an `Interrupted` outcome can
/// interpret it as either (a) attempt started but never finished, or
/// (b) attempt explicitly interrupted — both cases need follow-up
/// validation and the routing is identical, so collapsing them is
/// safe.
pub fn write_attempt_start(params: AttemptStartParams) -> AttemptWriteOutcome {
    AttemptWriteOutcome {
        task_id: params.task_id,
        run_id: params.run_id,
        status: TaskExecutionStatus::Interrupted,
        summary: params.summary,
    }
}

/// Record the **terminal status** of a task attempt as the Phase 2
/// formal write.
///
/// Pure function: takes the same identity (`task_id`, `run_id`) the
/// matching `write_attempt_start` produced, plus the real terminal
/// `status` and an optional summary, and returns the
/// [`AttemptWriteOutcome`] that downstream audit / observer code keys
/// off.
///
/// The eventual stateful body will:
///
/// 1. Update the existing run row created at start time (via the
///    orchestrator's `update_run` path, which today is part of the
///    `ExecutionAuditSession` channel) with the new status + summary.
/// 2. Return the same `AttemptWriteOutcome` so callers don't have to
///    re-thread the identifiers.
///
/// The split between `write_attempt_start` and `write_attempt_finish`
/// mirrors the orchestrator's existing two-phase persistence: a run row
/// is created at start (so observers see in-flight work) and then
/// updated at finish. Keeping the runtime helpers split lets the future
/// stateful versions slot in without a second API redesign.
pub fn write_attempt_finish(
    task_id: Uuid,
    run_id: Uuid,
    status: TaskExecutionStatus,
    summary: Option<String>,
) -> AttemptWriteOutcome {
    AttemptWriteOutcome {
        task_id,
        run_id,
        status,
        summary,
    }
}

/// Stateful Phase 2 attempt-start bridge.
///
/// Delegates to the current [`ExecutionAuditSession`] storage path, which
/// persists the per-task `Run` plus the start-side TaskEvent / PlanStepEvent
/// lifecycle facts, then returns the same [`AttemptWriteOutcome`] shape as the
/// pure constructor.
pub async fn write_attempt_start_with_session(
    session: &ExecutionAuditSession,
    task: &TaskSpec,
    model_name: &str,
    summary: Option<String>,
) -> Result<AttemptWriteOutcome, OrchestratorError> {
    session
        .record_attempt_start(task, model_name, summary)
        .await
}

/// Stateful Phase 2 attempt-finish bridge.
///
/// Delegates to the current [`ExecutionAuditSession`] storage path, appending
/// terminal TaskEvent / RunEvent / PlanStepEvent facts against the run created
/// by [`write_attempt_start_with_session`], then returns the same
/// [`AttemptWriteOutcome`] shape as the pure constructor.
pub async fn write_attempt_finish_with_session(
    session: &ExecutionAuditSession,
    task: &TaskSpec,
    status: TaskExecutionStatus,
    summary: Option<String>,
) -> Result<AttemptWriteOutcome, OrchestratorError> {
    session.record_attempt_finish(task, status, summary).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `is_terminal()` must return `true` for the four terminal statuses
    /// and `false` for `Interrupted` (the in-flight marker). Combined with
    /// `is_failure()`, this lets Phase 3 routing distinguish:
    ///   - `is_failure() && is_terminal()`  → Failed | Cancelled | TimedOut
    ///   - `is_failure() && !is_terminal()` → Interrupted (still in flight)
    ///   - `!is_failure() && is_terminal()` → Completed
    #[test]
    fn is_terminal_excludes_interrupted_only() {
        let task_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();

        for terminal in [
            TaskExecutionStatus::Completed,
            TaskExecutionStatus::Failed,
            TaskExecutionStatus::Cancelled,
            TaskExecutionStatus::TimedOut,
        ] {
            let label = format!("{terminal:?}");
            let outcome = AttemptWriteOutcome {
                task_id,
                run_id,
                status: terminal,
                summary: None,
            };
            assert!(outcome.is_terminal(), "expected {label} to be terminal",);
        }

        let in_flight = AttemptWriteOutcome {
            task_id,
            run_id,
            status: TaskExecutionStatus::Interrupted,
            summary: None,
        };
        assert!(
            !in_flight.is_terminal(),
            "Interrupted is the in-flight marker; must not be terminal",
        );
    }

    /// `is_failure() && is_terminal()` must hold for all three real
    /// failure statuses, distinguishing them from the in-flight
    /// `Interrupted` marker (`is_failure() && !is_terminal()`).
    #[test]
    fn is_failure_and_is_terminal_together_partition_statuses() {
        let task_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();

        for failure in [
            TaskExecutionStatus::Failed,
            TaskExecutionStatus::Cancelled,
            TaskExecutionStatus::TimedOut,
        ] {
            let label = format!("{failure:?}");
            let outcome = AttemptWriteOutcome {
                task_id,
                run_id,
                status: failure,
                summary: None,
            };
            assert!(
                outcome.is_failure() && outcome.is_terminal(),
                "expected {label} to be both failure and terminal",
            );
        }

        let in_flight = AttemptWriteOutcome {
            task_id,
            run_id,
            status: TaskExecutionStatus::Interrupted,
            summary: None,
        };
        assert!(
            in_flight.is_failure() && !in_flight.is_terminal(),
            "Interrupted must be failure but NOT terminal",
        );

        let success = AttemptWriteOutcome {
            task_id,
            run_id,
            status: TaskExecutionStatus::Completed,
            summary: None,
        };
        assert!(
            !success.is_failure() && success.is_terminal(),
            "Completed must be NOT failure AND terminal",
        );
    }

    /// `is_failure()` must return `false` for `Completed` and `true` for
    /// every other variant so Phase 3 routing logic can fail-closed on
    /// unknown future statuses.
    #[test]
    fn is_failure_matches_completed_only() {
        let task_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();

        let completed = AttemptWriteOutcome {
            task_id,
            run_id,
            status: TaskExecutionStatus::Completed,
            summary: None,
        };
        assert!(!completed.is_failure());

        for status in [
            TaskExecutionStatus::Failed,
            TaskExecutionStatus::Cancelled,
            TaskExecutionStatus::TimedOut,
            TaskExecutionStatus::Interrupted,
        ] {
            let label = format!("{status:?}");
            let outcome = AttemptWriteOutcome {
                task_id,
                run_id,
                status,
                summary: None,
            };
            assert!(
                outcome.is_failure(),
                "expected {label} to be a failure status",
            );
        }
    }

    /// `AttemptWriteOutcome` must derive `Clone` so observer / audit
    /// handlers can keep a snapshot while the caller continues mutating
    /// the executor state.
    #[test]
    fn outcome_is_clone() {
        let outcome = AttemptWriteOutcome {
            task_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            status: TaskExecutionStatus::Completed,
            summary: Some("ok".to_string()),
        };
        let cloned = outcome.clone();
        assert_eq!(cloned, outcome);
        assert_eq!(cloned.summary.as_deref(), Some("ok"));
    }

    /// `write_attempt_start` must produce an outcome tagged
    /// `Interrupted` (the start-state marker) carrying the same
    /// identifiers and summary supplied by the caller. The outcome
    /// must also flag as a failure (`is_failure() == true`) so Phase
    /// 3 routing knows the attempt hasn't reached a clean terminal
    /// state.
    #[test]
    fn write_attempt_start_tags_outcome_as_interrupted_in_flight_marker() {
        let task_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let params = AttemptStartParams {
            task_id,
            run_id,
            summary: Some("first attempt".to_string()),
        };

        let outcome = write_attempt_start(params);

        assert_eq!(outcome.task_id, task_id);
        assert_eq!(outcome.run_id, run_id);
        assert_eq!(outcome.status, TaskExecutionStatus::Interrupted);
        assert_eq!(outcome.summary.as_deref(), Some("first attempt"));
        // The start-state is_failure() == true so Phase 3 fails closed
        // on attempts that never finished.
        assert!(outcome.is_failure());
    }

    /// `write_attempt_start` with no summary must propagate `None`
    /// through — the helper is a constructor, not an enricher.
    #[test]
    fn write_attempt_start_threads_none_summary_verbatim() {
        let params = AttemptStartParams {
            task_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            summary: None,
        };
        let outcome = write_attempt_start(params);
        assert_eq!(outcome.summary, None);
    }

    /// `write_attempt_finish` must thread `task_id`, `run_id`,
    /// `status` and `summary` verbatim into the resulting outcome.
    /// `Completed` status must flip `is_failure()` to `false`.
    #[test]
    fn write_attempt_finish_records_completed_terminal_status() {
        let task_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let outcome = write_attempt_finish(
            task_id,
            run_id,
            TaskExecutionStatus::Completed,
            Some("all tests green".to_string()),
        );

        assert_eq!(outcome.task_id, task_id);
        assert_eq!(outcome.run_id, run_id);
        assert_eq!(outcome.status, TaskExecutionStatus::Completed);
        assert_eq!(outcome.summary.as_deref(), Some("all tests green"));
        assert!(!outcome.is_failure());
    }

    /// `write_attempt_finish` with a failure status must yield an
    /// outcome where `is_failure()` is true so Phase 3 routing fires.
    #[test]
    fn write_attempt_finish_failure_status_flags_outcome() {
        let outcome = write_attempt_finish(
            Uuid::new_v4(),
            Uuid::new_v4(),
            TaskExecutionStatus::Failed,
            Some("validation error".to_string()),
        );
        assert_eq!(outcome.status, TaskExecutionStatus::Failed);
        assert!(outcome.is_failure());
    }

    /// `write_attempt_start` followed by `write_attempt_finish` on the
    /// same `task_id` / `run_id` pair must preserve identity. This is
    /// the canonical Phase 2 lifecycle: observers should be able to
    /// pair start + finish records by (task_id, run_id) alone.
    #[test]
    fn write_attempt_start_then_finish_preserves_identity() {
        let task_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();

        let start = write_attempt_start(AttemptStartParams {
            task_id,
            run_id,
            summary: Some("starting".to_string()),
        });
        let finish = write_attempt_finish(
            start.task_id,
            start.run_id,
            TaskExecutionStatus::Completed,
            Some("done".to_string()),
        );

        assert_eq!(start.task_id, finish.task_id);
        assert_eq!(start.run_id, finish.run_id);
        // Status transitions from start-marker → terminal.
        assert_eq!(start.status, TaskExecutionStatus::Interrupted);
        assert_eq!(finish.status, TaskExecutionStatus::Completed);
        // Summaries are independent.
        assert_eq!(start.summary.as_deref(), Some("starting"));
        assert_eq!(finish.summary.as_deref(), Some("done"));
    }
}
