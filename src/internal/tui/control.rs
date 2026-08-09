//! TUI automation control command channel.
//!
//! `TuiControlCommand` is deliberately independent of `AppEvent`. `AppEvent` carries
//! turn-scoped events (each variant exposes a `turn_id`), whereas automation
//! respond / cancel / reclaim are control-plane commands that may span turns or
//! occur when no turn is active. Mixing them would break the turn-scoped invariant
//! and complicate the App event bus.
//!
//! The App main loop consumes these commands through an additional branch in its
//! `tokio::select!` (`code_control_rx.recv()`). Each command carries a `oneshot`
//! ack so the HTTP handler can await acceptance or rejection (e.g. `Busy`,
//! `InteractionNotActive`) without blocking the App event loop. The default ack
//! timeout in the adapter is 30 seconds; the App must send the ack before that
//! deadline or the automation client receives a timeout error.

use std::fmt;

use tokio::sync::oneshot;

use crate::internal::ai::web::code_ui::CodeUiInteractionResponse;

pub enum TuiControlCommand {
    SubmitMessage {
        text: String,
        ack: oneshot::Sender<Result<(), TuiControlError>>,
    },
    RespondInteraction {
        interaction_id: String,
        response: CodeUiInteractionResponse,
        ack: oneshot::Sender<Result<(), TuiControlError>>,
    },
    CancelCurrentTurn {
        ack: oneshot::Sender<Result<(), TuiControlError>>,
    },
    ReclaimController {
        ack: oneshot::Sender<Result<(), TuiControlError>>,
    },
    /// `goal.start { objective }` — create an active Goal in the
    /// session, mirroring `/goal start <objective>` (OC-Phase 6
    /// P6.6). The acknowledgement carries the rendered status of
    /// the freshly created Goal so the Code Control client can
    /// surface the goal id without a follow-up `goal.status`.
    GoalStart {
        objective: String,
        ack: oneshot::Sender<Result<String, TuiControlError>>,
    },
    /// `goal.status` — render the active Goal's snapshot. The
    /// acknowledgement carries the formatted multi-line summary
    /// (or an `InteractionNotActive`-equivalent status if no Goal
    /// is in flight). Read-only; no controller-token required at
    /// the HTTP layer (loopback observer mode).
    GoalStatus {
        ack: oneshot::Sender<Result<String, TuiControlError>>,
    },
    /// `goal.cancel { reason }` — explicit cancellation. Mirrors
    /// `/goal cancel <reason>` and emits `GoalEvent::Cancelled`
    /// into the session's event log.
    GoalCancel {
        reason: String,
        ack: oneshot::Sender<Result<String, TuiControlError>>,
    },
    /// `task.dispatch { agent, prompt }` — user-initiated sub-agent
    /// dispatch through the `task` tool, mirroring `/task <agent>
    /// <prompt>`. The acknowledgement carries the rendered task
    /// message so the Code Control client can show the task id and
    /// summary without a follow-up status poll. Refuses with `Busy`
    /// when the session is mid-turn / awaiting interaction, and
    /// with `TaskInvalidRequest` when the `agent`+`prompt` pair
    /// fails the dispatcher's shape rules (unknown agent, empty
    /// prompt, oversized payload, etc.).
    TaskDispatch {
        agent: String,
        prompt: String,
        ack: oneshot::Sender<Result<String, TuiControlError>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiControlError {
    Busy,
    InteractionNotActive,
    UnsupportedInteractionKind,
    ControllerConflict,
    /// Durable mutation reconciliation is required before another turn can run.
    ReconciliationRequired(String),
    Internal(String),
    /// `goal.start` was called while a Goal is already active in
    /// this session. The user / automation must `goal.cancel` (or
    /// wait for completion, once the supervisor lands) before
    /// starting a new one (OC-Phase 6 P6.6).
    GoalAlreadyActive,
    /// `goal.status` / `goal.cancel` was called when no Goal is
    /// active. Distinct from `InteractionNotActive` (which is
    /// scoped to pending tool-interaction prompts).
    GoalNotActive,
    /// Goal objective failed `GoalSpec`'s shape rules at the
    /// HTTP boundary (empty / oversized). The wire message
    /// repeats the underlying `GoalSpecError` for client logs.
    GoalInvalidObjective(String),
    /// `task.dispatch` was called with an `agent`+`prompt` pair
    /// that failed the dispatcher's shape rules at the HTTP
    /// boundary (unknown agent, empty / oversized prompt, etc.).
    /// The wire message repeats the underlying validation error
    /// for client logs.
    TaskInvalidRequest(String),
}

impl TuiControlError {
    pub fn status(&self) -> u16 {
        match self {
            Self::Busy => 409,
            Self::InteractionNotActive => 409,
            Self::UnsupportedInteractionKind => 422,
            Self::ControllerConflict => 409,
            Self::ReconciliationRequired(_) => 409,
            Self::Internal(_) => 500,
            Self::GoalAlreadyActive => 409,
            Self::GoalNotActive => 409,
            Self::GoalInvalidObjective(_) => 422,
            Self::TaskInvalidRequest(_) => 422,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Busy => "SESSION_BUSY",
            Self::InteractionNotActive => "INTERACTION_NOT_ACTIVE",
            Self::UnsupportedInteractionKind => "UNSUPPORTED_INTERACTION_KIND",
            Self::ControllerConflict => "CONTROLLER_CONFLICT",
            Self::ReconciliationRequired(_) => "RECONCILIATION_REQUIRED",
            Self::Internal(_) => "TUI_CONTROL_INTERNAL",
            Self::GoalAlreadyActive => "GOAL_ALREADY_ACTIVE",
            Self::GoalNotActive => "GOAL_NOT_ACTIVE",
            Self::GoalInvalidObjective(_) => "GOAL_INVALID_OBJECTIVE",
            Self::TaskInvalidRequest(_) => "TASK_INVALID_REQUEST",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Busy => "The TUI session is busy and cannot accept a new message".to_string(),
            Self::InteractionNotActive => {
                "The requested interaction is not currently pending".to_string()
            }
            Self::UnsupportedInteractionKind => {
                "This interaction kind cannot be answered by automation".to_string()
            }
            Self::ControllerConflict => {
                "The local TUI controller is not reclaimable in this session".to_string()
            }
            Self::ReconciliationRequired(session_id) => format!(
                "session '{session_id}' requires mutation reconciliation before another turn can run"
            ),
            Self::Internal(message) => message.clone(),
            Self::GoalAlreadyActive => {
                "A Goal is already active in this session — cancel it first".to_string()
            }
            Self::GoalNotActive => {
                "No Goal is active in this session — call goal.start first".to_string()
            }
            Self::GoalInvalidObjective(detail) => {
                format!("Goal objective failed validation: {detail}")
            }
            Self::TaskInvalidRequest(detail) => {
                format!("Task dispatch request failed validation: {detail}")
            }
        }
    }
}

impl fmt::Display for TuiControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for TuiControlError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelSource {
    Esc,
    SlashQuit,
    Automation,
    /// The OC-Phase 5 [`BudgetTracker`] returned
    /// `BudgetExceededError` from a `check_session` / `check_agent` /
    /// `check_goal` call wired into the `AgentEvent::UsageUpdated`
    /// handler. The dispatcher abort source carries this so a future
    /// budget-vs-user disambiguation in the TUI footer can show
    /// "interrupted: budget" instead of "interrupted: user".
    ///
    /// [`BudgetTracker`]: crate::internal::ai::agent::budget::BudgetTracker
    Budget,
}

#[cfg(test)]
mod tests {
    use super::TuiControlError;

    #[test]
    fn tui_control_error_display_pins_each_variant() {
        assert_eq!(
            TuiControlError::Busy.to_string(),
            "The TUI session is busy and cannot accept a new message",
        );
        assert_eq!(
            TuiControlError::InteractionNotActive.to_string(),
            "The requested interaction is not currently pending",
        );
        assert_eq!(
            TuiControlError::UnsupportedInteractionKind.to_string(),
            "This interaction kind cannot be answered by automation",
        );
        assert_eq!(
            TuiControlError::ControllerConflict.to_string(),
            "The local TUI controller is not reclaimable in this session",
        );
        assert_eq!(
            TuiControlError::ReconciliationRequired("sess-1".to_string()).to_string(),
            "session 'sess-1' requires mutation reconciliation before another turn can run",
        );
        assert_eq!(
            TuiControlError::Internal("downstream blew up".to_string()).to_string(),
            "downstream blew up",
        );
        assert_eq!(
            TuiControlError::GoalAlreadyActive.to_string(),
            "A Goal is already active in this session — cancel it first",
        );
        assert_eq!(
            TuiControlError::GoalNotActive.to_string(),
            "No Goal is active in this session — call goal.start first",
        );
        assert_eq!(
            TuiControlError::GoalInvalidObjective("empty objective".to_string()).to_string(),
            "Goal objective failed validation: empty objective",
        );
        assert_eq!(
            TuiControlError::TaskInvalidRequest("unknown agent `nope`".to_string()).to_string(),
            "Task dispatch request failed validation: unknown agent `nope`",
        );
    }

    /// Pin every variant's HTTP status code so a future surface
    /// drift (e.g. dropping `TaskInvalidRequest` back to 409) is
    /// loud rather than silent. Mirrors the per-variant Display pin
    /// above; clients downstream of `/code-control` branch on these
    /// codes for retry / surface routing.
    #[test]
    fn tui_control_error_status_pins_each_variant() {
        assert_eq!(TuiControlError::Busy.status(), 409);
        assert_eq!(TuiControlError::InteractionNotActive.status(), 409);
        assert_eq!(TuiControlError::UnsupportedInteractionKind.status(), 422);
        assert_eq!(TuiControlError::ControllerConflict.status(), 409);
        assert_eq!(
            TuiControlError::ReconciliationRequired(String::new()).status(),
            409,
        );
        assert_eq!(TuiControlError::Internal(String::new()).status(), 500);
        assert_eq!(TuiControlError::GoalAlreadyActive.status(), 409);
        assert_eq!(TuiControlError::GoalNotActive.status(), 409);
        assert_eq!(
            TuiControlError::GoalInvalidObjective(String::new()).status(),
            422,
        );
        assert_eq!(
            TuiControlError::TaskInvalidRequest(String::new()).status(),
            422,
        );
    }

    /// Pin every variant's stable wire code so a rename or
    /// re-bucketing of the snake_case identifier (e.g. switching
    /// `TASK_INVALID_REQUEST` to `INVALID_TASK_REQUEST`) trips the
    /// guard at PR time.
    #[test]
    fn tui_control_error_code_pins_each_variant() {
        assert_eq!(TuiControlError::Busy.code(), "SESSION_BUSY");
        assert_eq!(
            TuiControlError::InteractionNotActive.code(),
            "INTERACTION_NOT_ACTIVE",
        );
        assert_eq!(
            TuiControlError::UnsupportedInteractionKind.code(),
            "UNSUPPORTED_INTERACTION_KIND",
        );
        assert_eq!(
            TuiControlError::ControllerConflict.code(),
            "CONTROLLER_CONFLICT",
        );
        assert_eq!(
            TuiControlError::ReconciliationRequired(String::new()).code(),
            "RECONCILIATION_REQUIRED",
        );
        assert_eq!(
            TuiControlError::Internal(String::new()).code(),
            "TUI_CONTROL_INTERNAL",
        );
        assert_eq!(
            TuiControlError::GoalAlreadyActive.code(),
            "GOAL_ALREADY_ACTIVE",
        );
        assert_eq!(TuiControlError::GoalNotActive.code(), "GOAL_NOT_ACTIVE");
        assert_eq!(
            TuiControlError::GoalInvalidObjective(String::new()).code(),
            "GOAL_INVALID_OBJECTIVE",
        );
        assert_eq!(
            TuiControlError::TaskInvalidRequest(String::new()).code(),
            "TASK_INVALID_REQUEST",
        );
    }
}
