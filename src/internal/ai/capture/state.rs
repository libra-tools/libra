//! Pure lifecycle reduction for durable agent capture.
//!
//! This module turns a canonical lifecycle event plus a caller-supplied
//! durable state snapshot into a deterministic write plan. It deliberately
//! has no database, provider, filesystem, clock, or ref dependency: callers
//! inject the current state, event identity, timestamps, and any deadline.

use thiserror::Error;
use uuid::Uuid;

use crate::internal::ai::hooks::LifecycleEventKind;

/// Persisted phase values accepted from `agent_session.state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CapturePhase {
    Pending,
    Active,
    Condensed,
    Stopped,
    Quarantined,
}

impl CapturePhase {
    /// Parse the complete supported database state set without silently
    /// coercing an unknown value into a live state.
    pub(crate) fn from_db(value: &str) -> Result<Self, CaptureReducerError> {
        match value {
            "pending" => Ok(Self::Pending),
            "active" => Ok(Self::Active),
            "condensed" => Ok(Self::Condensed),
            "stopped" => Ok(Self::Stopped),
            "quarantined" => Ok(Self::Quarantined),
            other => Err(CaptureReducerError::UnknownDatabaseState(other.to_string())),
        }
    }

    /// Stable representation retained by the existing SQLite contract.
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Condensed => "condensed",
            Self::Stopped => "stopped",
            Self::Quarantined => "quarantined",
        }
    }
}

/// Durable capture state loaded by a store adapter before reducing an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DurableCaptureState {
    pub(crate) phase: CapturePhase,
    pub(crate) stopped_at: Option<i64>,
    pub(crate) sync_revision: i64,
}

/// Which checkpoint writer, if any, the coordinator must invoke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckpointWrite {
    None,
    Committed,
    SubagentBoundary,
}

/// Mutation of the nullable `stopped_at` column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StoppedAtMutation {
    /// Retain the durable value for an existing row. New rows receive NULL.
    Preserve,
    /// Establish a terminal timestamp supplied by the caller.
    Set(i64),
}

/// All information a pure lifecycle reduction is permitted to consume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LifecycleReducerInput {
    pub(crate) current: Option<DurableCaptureState>,
    pub(crate) event_kind: LifecycleEventKind,
    pub(crate) event_id: Uuid,
    /// Event time is supplied by the caller; the reducer never reads a clock.
    /// It may be sampled before awaited reads and is used only for durable
    /// event timestamps, not as a substitute for monotonic runtime checks.
    pub(crate) occurred_at: i64,
    /// Optional absolute Unix-seconds deadline supplied by a caller/finalizer
    /// policy. This coarse, advisory guard shares `occurred_at` units; callers
    /// remain responsible for enforcing the higher-resolution monotonic
    /// deadline around I/O and mutations.
    pub(crate) deadline: Option<i64>,
}

/// Deterministic state/checkpoint work generated for one canonical event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LifecycleActionPlan {
    /// Stable receipt identity for a later catalog store to deduplicate.
    pub(crate) action_key: String,
    pub(crate) next_phase: CapturePhase,
    pub(crate) stopped_at: StoppedAtMutation,
    pub(crate) checkpoint: CheckpointWrite,
    /// The durable revision observed by the caller, when a row existed.
    pub(crate) expected_sync_revision: Option<i64>,
}

/// Fail-closed outcomes from the pure reducer.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum CaptureReducerError {
    #[error("agent capture state '{0}' is not recognized")]
    UnknownDatabaseState(String),
    #[error("cannot apply a live lifecycle event to a quarantined capture session")]
    Quarantined,
    #[error("capture lifecycle event timestamp exceeds its supplied deadline")]
    DeadlineExceeded,
    #[error("agent capture sync revision cannot be advanced without overflowing")]
    RevisionOverflow,
}

/// Reduce one canonical lifecycle event to its durable action plan.
///
/// The action key includes only the version and validated event identity, so
/// equivalent inputs replay identically regardless of provider or path.
pub(crate) fn reduce_lifecycle(
    input: LifecycleReducerInput,
) -> Result<LifecycleActionPlan, CaptureReducerError> {
    // A terminal callback must still reserve a durable pending receipt when
    // the host budget has elapsed; finalization then records that original
    // deadline and remains replayable. Dropping it here would falsely
    // acknowledge a SessionEnd while stranding its prior capture state.
    if input.event_kind != LifecycleEventKind::SessionEnd
        && input
            .deadline
            .is_some_and(|deadline| input.occurred_at > deadline)
    {
        return Err(CaptureReducerError::DeadlineExceeded);
    }
    if input
        .current
        .is_some_and(|state| matches!(state.phase, CapturePhase::Quarantined))
    {
        return Err(CaptureReducerError::Quarantined);
    }
    if let Some(current) = input.current {
        current
            .sync_revision
            .checked_add(1)
            .ok_or(CaptureReducerError::RevisionOverflow)?;
    }

    let (next_phase, stopped_at, checkpoint) = match input.event_kind {
        LifecycleEventKind::SessionEnd => (
            CapturePhase::Stopped,
            StoppedAtMutation::Set(input.occurred_at),
            CheckpointWrite::Committed,
        ),
        LifecycleEventKind::Compaction => (
            CapturePhase::Condensed,
            StoppedAtMutation::Preserve,
            CheckpointWrite::None,
        ),
        LifecycleEventKind::TurnEnd => (
            CapturePhase::Active,
            StoppedAtMutation::Preserve,
            CheckpointWrite::Committed,
        ),
        LifecycleEventKind::SubagentStart | LifecycleEventKind::SubagentEnd => (
            CapturePhase::Active,
            StoppedAtMutation::Preserve,
            CheckpointWrite::SubagentBoundary,
        ),
        LifecycleEventKind::SessionStart
        | LifecycleEventKind::TurnStart
        | LifecycleEventKind::ToolUse
        | LifecycleEventKind::ModelUpdate
        | LifecycleEventKind::CompactionCompleted
        | LifecycleEventKind::PermissionRequest
        | LifecycleEventKind::SourceEnabled
        | LifecycleEventKind::SourceDisabled => (
            CapturePhase::Active,
            StoppedAtMutation::Preserve,
            CheckpointWrite::None,
        ),
    };

    Ok(LifecycleActionPlan {
        action_key: format!("capture-lifecycle-v1:{}", input.event_id),
        next_phase,
        stopped_at,
        checkpoint,
        expected_sync_revision: input.current.map(|state| state.sync_revision),
    })
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    const EVENT_ID: Uuid = Uuid::from_u128(0x29bd_9328_66f1_40e7_99ba_85a0_2f94_8e33);

    fn input(
        current: Option<DurableCaptureState>,
        event_kind: LifecycleEventKind,
    ) -> LifecycleReducerInput {
        LifecycleReducerInput {
            current,
            event_kind,
            event_id: EVENT_ID,
            occurred_at: 1_700_000_000,
            deadline: None,
        }
    }

    #[test]
    fn database_phase_mapping_is_total_and_fail_closed() {
        for (value, phase) in [
            ("pending", CapturePhase::Pending),
            ("active", CapturePhase::Active),
            ("condensed", CapturePhase::Condensed),
            ("stopped", CapturePhase::Stopped),
            ("quarantined", CapturePhase::Quarantined),
        ] {
            assert_eq!(CapturePhase::from_db(value), Ok(phase));
            assert_eq!(phase.as_db(), value);
        }
        assert_eq!(
            CapturePhase::from_db("future"),
            Err(CaptureReducerError::UnknownDatabaseState(
                "future".to_string()
            ))
        );
    }

    #[test]
    fn stopped_without_timestamp_remains_reactivatable() {
        let incomplete = DurableCaptureState {
            phase: CapturePhase::Stopped,
            stopped_at: None,
            sync_revision: 4,
        };

        let action = reduce_lifecycle(input(Some(incomplete), LifecycleEventKind::SessionStart))
            .expect("an incomplete stop may be reactivated");
        assert_eq!(action.next_phase, CapturePhase::Active);
        assert_eq!(action.stopped_at, StoppedAtMutation::Preserve);
        assert_eq!(action.expected_sync_revision, Some(4));
    }

    #[test]
    fn every_event_has_a_deterministic_action_plan() {
        let events = [
            LifecycleEventKind::SessionStart,
            LifecycleEventKind::TurnStart,
            LifecycleEventKind::ToolUse,
            LifecycleEventKind::ModelUpdate,
            LifecycleEventKind::Compaction,
            LifecycleEventKind::CompactionCompleted,
            LifecycleEventKind::PermissionRequest,
            LifecycleEventKind::SourceEnabled,
            LifecycleEventKind::SourceDisabled,
            LifecycleEventKind::TurnEnd,
            LifecycleEventKind::SessionEnd,
            LifecycleEventKind::SubagentStart,
            LifecycleEventKind::SubagentEnd,
        ];
        for event in events {
            let first = reduce_lifecycle(input(None, event)).expect("event is reducible");
            let second = reduce_lifecycle(input(None, event)).expect("event is reducible");
            assert_eq!(first, second, "{event}");
            assert_eq!(first.action_key, format!("capture-lifecycle-v1:{EVENT_ID}"));
        }
    }

    #[test]
    fn terminal_and_reactivation_semantics_are_explicit() {
        let stopped = DurableCaptureState {
            phase: CapturePhase::Stopped,
            stopped_at: Some(1_699_999_999),
            sync_revision: 8,
        };
        let terminal = reduce_lifecycle(input(Some(stopped), LifecycleEventKind::SessionEnd))
            .expect("session end reduces");
        assert_eq!(terminal.next_phase, CapturePhase::Stopped);
        assert_eq!(terminal.stopped_at, StoppedAtMutation::Set(1_700_000_000));
        assert_eq!(terminal.checkpoint, CheckpointWrite::Committed);
        assert_eq!(terminal.expected_sync_revision, Some(8));

        for event in [
            LifecycleEventKind::Compaction,
            LifecycleEventKind::TurnEnd,
            LifecycleEventKind::SubagentStart,
            LifecycleEventKind::SubagentEnd,
            LifecycleEventKind::SessionStart,
            LifecycleEventKind::TurnStart,
            LifecycleEventKind::ToolUse,
            LifecycleEventKind::ModelUpdate,
            LifecycleEventKind::CompactionCompleted,
            LifecycleEventKind::PermissionRequest,
            LifecycleEventKind::SourceEnabled,
            LifecycleEventKind::SourceDisabled,
        ] {
            let action = reduce_lifecycle(input(Some(stopped), event)).expect("reactivates");
            assert_ne!(action.next_phase, CapturePhase::Stopped, "{event}");
            assert_eq!(action.stopped_at, StoppedAtMutation::Preserve, "{event}");
        }
    }

    #[test]
    fn rejects_quarantine_deadline_and_revision_overflow() {
        let quarantined = DurableCaptureState {
            phase: CapturePhase::Quarantined,
            stopped_at: None,
            sync_revision: 1,
        };
        assert_eq!(
            reduce_lifecycle(input(Some(quarantined), LifecycleEventKind::TurnStart)),
            Err(CaptureReducerError::Quarantined)
        );

        let mut late = input(None, LifecycleEventKind::TurnStart);
        late.deadline = Some(late.occurred_at - 1);
        assert_eq!(
            reduce_lifecycle(late),
            Err(CaptureReducerError::DeadlineExceeded)
        );

        let mut late_terminal = input(None, LifecycleEventKind::SessionEnd);
        late_terminal.deadline = Some(late_terminal.occurred_at - 1);
        assert!(
            reduce_lifecycle(late_terminal).is_ok(),
            "an expired terminal event must reach durable pending finalization"
        );

        let overflowing = DurableCaptureState {
            phase: CapturePhase::Active,
            stopped_at: None,
            sync_revision: i64::MAX,
        };
        assert_eq!(
            reduce_lifecycle(input(Some(overflowing), LifecycleEventKind::TurnStart)),
            Err(CaptureReducerError::RevisionOverflow)
        );
    }
}
