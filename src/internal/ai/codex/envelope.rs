//! Codex websocket → runtime `AgentEvent` envelope normalization (W3-04).
//!
//! Managed Codex speaks an app-server JSON-RPC notification schema over
//! WebSocket. This module is the **only** place that classifies those
//! notifications into the shared runtime [`AgentEventKind`] envelope used by
//! non-Codex providers. Callers must record the normalized kinds before
//! projecting Code UI state so Codex cannot author a parallel event path.
//!
//! Unknown / future method strings classify as [`MethodKind::Unknown`] and
//! emit a diagnosable [`AgentEventKind::ProviderNotification`] fallback —
//! never silent drop and never panic.

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::protocol::MethodKind;
use crate::internal::ai::{
    runtime::{AgentEventKind, InteractionState},
    web::code_ui::{
        CodeUiApplyToFuture, CodeUiInteractionKind, CodeUiInteractionOption,
        CodeUiInteractionRequest, CodeUiInteractionResponse, CodeUiInteractionStatus,
    },
};

/// Provider label stamped on every Codex-normalized envelope kind.
pub const CODEX_PROVIDER: &str = "codex";

/// Result of mapping one Codex websocket notification into runtime envelope
/// kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedCodexEnvelope {
    /// Raw JSON-RPC `method` string from the notification.
    pub method: String,
    /// Stable classification name (MethodKind discriminant or `"unknown"`).
    pub classification: String,
    /// One or more runtime envelope kinds. Lifecycle methods emit the
    /// first-class variants; everything else (including unknown) emits
    /// [`AgentEventKind::ProviderNotification`].
    kinds: Vec<AgentEventKind>,
    /// `true` when the method was not recognized (`MethodKind::Unknown`).
    pub used_fallback: bool,
}

impl NormalizedCodexEnvelope {
    fn new(
        method: String,
        classification: String,
        kinds: Vec<AgentEventKind>,
        used_fallback: bool,
    ) -> Self {
        // INVARIANT: map_method_kind always returns a non-empty vec; keep the
        // public type unconstructable with an empty kinds list.
        debug_assert!(!kinds.is_empty());
        Self {
            method,
            classification,
            kinds,
            used_fallback,
        }
    }

    /// Envelope kinds produced for this notification (always non-empty when
    /// constructed via [`normalize_codex_notification`]).
    pub fn kinds(&self) -> &[AgentEventKind] {
        &self.kinds
    }

    /// Primary (first) envelope kind.
    pub fn primary_kind(&self) -> Option<&AgentEventKind> {
        self.kinds.first()
    }
}

/// Classify a Codex app-server notification into runtime [`AgentEventKind`]s.
///
/// Exhaustive over [`MethodKind`]: adding a new variant forces an update here.
/// Unknown raw methods always take the diagnosable fallback path.
pub fn normalize_codex_notification(method: &str, params: &Value) -> NormalizedCodexEnvelope {
    let mk = MethodKind::from(method);
    let classification = method_kind_classification(mk).to_string();
    let used_fallback = matches!(mk, MethodKind::Unknown);
    let kinds = map_method_kind(mk, method, params);
    // INVARIANT: map_method_kind always returns a non-empty vec.
    debug_assert!(!kinds.is_empty());
    NormalizedCodexEnvelope::new(method.to_string(), classification, kinds, used_fallback)
}

/// Stable snake_case name for each [`MethodKind`] discriminant.
pub fn method_kind_classification(kind: MethodKind) -> &'static str {
    match kind {
        MethodKind::ThreadStarted => "thread_started",
        MethodKind::ThreadStatusChanged => "thread_status_changed",
        MethodKind::ThreadNameUpdated => "thread_name_updated",
        MethodKind::ThreadArchived => "thread_archived",
        MethodKind::ThreadCompacted => "thread_compacted",
        MethodKind::ThreadClosed => "thread_closed",
        MethodKind::TurnStarted => "turn_started",
        MethodKind::TurnCompleted => "turn_completed",
        MethodKind::TokenUsageUpdated => "token_usage_updated",
        MethodKind::PlanUpdated => "plan_updated",
        MethodKind::PlanDelta => "plan_delta",
        MethodKind::AgentMessageDelta => "agent_message_delta",
        MethodKind::CommandExecutionOutputDelta => "command_execution_output_delta",
        MethodKind::FileChangeOutputDelta => "file_change_output_delta",
        MethodKind::TaskStarted => "task_started",
        MethodKind::TaskCompleted => "task_completed",
        MethodKind::ItemStarted => "item_started",
        MethodKind::ItemCompleted => "item_completed",
        MethodKind::RequestApproval => "request_approval",
        MethodKind::RequestApprovalCommandExecution => "request_approval_command_execution",
        MethodKind::RequestApprovalFileChange => "request_approval_file_change",
        MethodKind::RequestApprovalApplyPatch => "request_approval_apply_patch",
        MethodKind::RequestApprovalExec => "request_approval_exec",
        MethodKind::Initialized => "initialized",
        MethodKind::Unknown => "unknown",
    }
}

/// Every known [`MethodKind`] discriminant except [`MethodKind::Unknown`].
/// Used by regression tables so new variants fail the W3-04 coverage test.
pub fn all_known_method_kinds() -> &'static [MethodKind] {
    &[
        MethodKind::ThreadStarted,
        MethodKind::ThreadStatusChanged,
        MethodKind::ThreadNameUpdated,
        MethodKind::ThreadArchived,
        MethodKind::ThreadCompacted,
        MethodKind::ThreadClosed,
        MethodKind::TurnStarted,
        MethodKind::TurnCompleted,
        MethodKind::TokenUsageUpdated,
        MethodKind::PlanUpdated,
        MethodKind::PlanDelta,
        MethodKind::AgentMessageDelta,
        MethodKind::CommandExecutionOutputDelta,
        MethodKind::FileChangeOutputDelta,
        MethodKind::TaskStarted,
        MethodKind::TaskCompleted,
        MethodKind::ItemStarted,
        MethodKind::ItemCompleted,
        MethodKind::RequestApproval,
        MethodKind::RequestApprovalCommandExecution,
        MethodKind::RequestApprovalFileChange,
        MethodKind::RequestApprovalApplyPatch,
        MethodKind::RequestApprovalExec,
        MethodKind::Initialized,
    ]
}

/// Canonical raw method string for a known kind (for table-driven tests).
pub fn sample_method_for_kind(kind: MethodKind) -> &'static str {
    match kind {
        MethodKind::ThreadStarted => "thread/started",
        MethodKind::ThreadStatusChanged => "thread/status/changed",
        MethodKind::ThreadNameUpdated => "thread/name/updated",
        MethodKind::ThreadArchived => "thread/archived",
        MethodKind::ThreadCompacted => "thread/compacted",
        MethodKind::ThreadClosed => "thread/closed",
        MethodKind::TurnStarted => "turn/started",
        MethodKind::TurnCompleted => "turn/completed",
        MethodKind::TokenUsageUpdated => "thread/tokenUsage/updated",
        MethodKind::PlanUpdated => "turn/plan/updated",
        MethodKind::PlanDelta => "item/plan/delta",
        MethodKind::AgentMessageDelta => "item/agentMessage/delta",
        MethodKind::CommandExecutionOutputDelta => "item/commandExecution/outputDelta",
        MethodKind::FileChangeOutputDelta => "item/fileChange/outputDelta",
        MethodKind::TaskStarted => "codex/event/task_started",
        MethodKind::TaskCompleted => "codex/event/task_complete",
        MethodKind::ItemStarted => "item/agentMessage/started",
        MethodKind::ItemCompleted => "item/agentMessage/completed",
        MethodKind::RequestApproval => "item/tool/requestApproval",
        MethodKind::RequestApprovalCommandExecution => "item/commandExecution/requestApproval",
        MethodKind::RequestApprovalFileChange => "item/fileChange/requestApproval",
        MethodKind::RequestApprovalApplyPatch => "apply_patch_approval_request",
        MethodKind::RequestApprovalExec => "exec_approval_request",
        MethodKind::Initialized => "initialized",
        MethodKind::Unknown => "codex/future/unknownMethod",
    }
}

fn map_method_kind(kind: MethodKind, method: &str, params: &Value) -> Vec<AgentEventKind> {
    match kind {
        MethodKind::TurnStarted => vec![AgentEventKind::TurnStarted],
        MethodKind::TurnCompleted => vec![map_turn_completed(params)],
        MethodKind::RequestApproval
        | MethodKind::RequestApprovalCommandExecution
        | MethodKind::RequestApprovalFileChange
        | MethodKind::RequestApprovalApplyPatch
        | MethodKind::RequestApprovalExec => {
            vec![AgentEventKind::InteractionRequested {
                state: InteractionState::AwaitingToolApproval {
                    interaction_id: extract_interaction_id(params),
                    tool_name: approval_tool_name(kind),
                },
            }]
        }
        MethodKind::ThreadStarted
        | MethodKind::ThreadStatusChanged
        | MethodKind::ThreadNameUpdated
        | MethodKind::ThreadArchived
        | MethodKind::ThreadCompacted
        | MethodKind::ThreadClosed
        | MethodKind::TokenUsageUpdated
        | MethodKind::PlanUpdated
        | MethodKind::PlanDelta
        | MethodKind::AgentMessageDelta
        | MethodKind::CommandExecutionOutputDelta
        | MethodKind::FileChangeOutputDelta
        | MethodKind::TaskStarted
        | MethodKind::TaskCompleted
        | MethodKind::ItemStarted
        | MethodKind::ItemCompleted
        | MethodKind::Initialized
        | MethodKind::Unknown => {
            vec![provider_notification(kind, method, params)]
        }
    }
}

fn map_turn_completed(params: &Value) -> AgentEventKind {
    if params.get("error").is_some() {
        return AgentEventKind::TurnFailed {
            reason: "codex turn completed with error".to_string(),
        };
    }
    let status = params
        .get("status")
        .or_else(|| params.pointer("/turn/status"))
        .and_then(Value::as_str)
        .unwrap_or("completed");
    let status_l = status.to_ascii_lowercase();
    if matches!(
        status_l.as_str(),
        "failed" | "error" | "errored" | "cancelled" | "canceled" | "aborted" | "interrupted"
    ) {
        if matches!(
            status_l.as_str(),
            "cancelled" | "canceled" | "aborted" | "interrupted"
        ) {
            AgentEventKind::TurnCancelled
        } else {
            AgentEventKind::TurnFailed {
                reason: format!("codex turn ended with status '{status}'"),
            }
        }
    } else {
        AgentEventKind::TurnCompleted {
            summary: format!("codex turn completed ({status})"),
        }
    }
}

/// Managed Codex [`super::types::RunStatus`] to persist for a `turn/completed`
/// notification. Kept in lockstep with [`map_turn_completed`] so hydration
/// cannot rewrite a failed/cancelled turn as Completed (W3-04).
pub fn run_status_for_turn_completed(params: &Value) -> super::types::RunStatus {
    match map_turn_completed(params) {
        AgentEventKind::TurnCancelled => super::types::RunStatus::Cancelled,
        AgentEventKind::TurnFailed { .. } => super::types::RunStatus::Failed,
        _ => super::types::RunStatus::Completed,
    }
}

fn approval_tool_name(kind: MethodKind) -> String {
    match kind {
        MethodKind::RequestApprovalCommandExecution | MethodKind::RequestApprovalExec => {
            "command_execution".to_string()
        }
        MethodKind::RequestApprovalFileChange | MethodKind::RequestApprovalApplyPatch => {
            "file_change".to_string()
        }
        _ => "tool".to_string(),
    }
}

/// App-server JSON-RPC method used to forward one approval decision.
pub fn approval_resolve_method(kind: MethodKind) -> &'static str {
    match kind {
        MethodKind::RequestApprovalCommandExecution => {
            "item/commandExecution/requestApproval/resolve"
        }
        MethodKind::RequestApprovalFileChange => "item/fileChange/requestApproval/resolve",
        MethodKind::RequestApprovalExec => "exec_approval_request/resolve",
        MethodKind::RequestApprovalApplyPatch => "apply_patch_approval_request/resolve",
        _ => "requestApproval/resolve",
    }
}

/// Outward Code UI approval row for managed Codex (W3-07).
///
/// Option ids match the non-Codex headless exec-approval wire (`approve` /
/// `deny` / `abort`) so the browser does not branch on provider. Codex
/// app-server still owns the approval loop; this is projection only.
pub fn codex_tool_approval_interaction(
    interaction_id: impl Into<String>,
    tool_name: &str,
    description: Option<String>,
    prompt: Option<String>,
    metadata: Value,
    requested_at: DateTime<Utc>,
) -> CodeUiInteractionRequest {
    let title = match tool_name {
        "command_execution" => "Approve command execution",
        "file_change" => "Approve file change",
        _ => "Approval required",
    };
    CodeUiInteractionRequest {
        id: interaction_id.into(),
        kind: CodeUiInteractionKind::Approval,
        title: Some(title.to_string()),
        description,
        prompt,
        options: vec![
            CodeUiInteractionOption {
                id: "approve".to_string(),
                label: "Approve".to_string(),
                description: Some("Allow this request once".to_string()),
            },
            CodeUiInteractionOption {
                id: "deny".to_string(),
                label: "Deny".to_string(),
                description: Some("Reject this request".to_string()),
            },
            CodeUiInteractionOption {
                id: "abort".to_string(),
                label: "Abort".to_string(),
                description: Some("Cancel this tool run immediately".to_string()),
            },
        ],
        status: CodeUiInteractionStatus::Pending,
        metadata,
        requested_at,
        resolved_at: None,
    }
}

/// Parse a browser/automation interaction response into an app-server
/// `approved` flag plus whether the user asked to abort the turn.
/// `deny` declines once; `abort` declines and should interrupt the turn.
/// `applyToFuture` is returned separately so the adapter can update
/// ask/accept/decline mode.
pub fn decision_from_code_ui_interaction_response(
    response: &CodeUiInteractionResponse,
) -> Result<(bool, Option<CodeUiApplyToFuture>, bool), String> {
    let abort_selected = response
        .selected_option
        .as_deref()
        .is_some_and(|option| option.eq_ignore_ascii_case("abort"));
    let from_option = match response.selected_option.as_deref() {
        Some(option) if option.eq_ignore_ascii_case("approve") => Some(true),
        Some(option) if option.eq_ignore_ascii_case("allow") => Some(true),
        Some(option) if option.eq_ignore_ascii_case("approve_all") => Some(true),
        Some(option) if option.eq_ignore_ascii_case("yes") => Some(true),
        Some(option) if option.eq_ignore_ascii_case("deny") => Some(false),
        Some(option) if option.eq_ignore_ascii_case("decline") => Some(false),
        Some(option) if option.eq_ignore_ascii_case("decline_all") => Some(false),
        Some(option) if option.eq_ignore_ascii_case("no") => Some(false),
        Some(option) if option.eq_ignore_ascii_case("abort") => Some(false),
        _ => None,
    };
    if let (Some(approved), Some(from_option)) = (response.approved, from_option)
        && approved != from_option
    {
        return Err(
            "Codex approval response has conflicting approved and selectedOption fields"
                .to_string(),
        );
    }
    let Some(approved) = response.approved.or(from_option) else {
        return Err("Codex approvals require an explicit decision".to_string());
    };
    let apply_to_future =
        response
            .apply_to_future
            .clone()
            .or(match response.selected_option.as_deref() {
                Some(option) if option.eq_ignore_ascii_case("approve_all") => {
                    Some(CodeUiApplyToFuture::AcceptAll)
                }
                Some(option) if option.eq_ignore_ascii_case("decline_all") => {
                    Some(CodeUiApplyToFuture::DeclineAll)
                }
                _ => None,
            });
    if let Some(apply_to_future) = apply_to_future.as_ref() {
        let matches_decision = match apply_to_future {
            CodeUiApplyToFuture::AcceptAll => approved,
            CodeUiApplyToFuture::DeclineAll => !approved,
            CodeUiApplyToFuture::No => true,
        };
        if !matches_decision {
            return Err(
                "Codex approval applyToFuture conflicts with the resolved decision".to_string(),
            );
        }
    }
    if abort_selected && approved {
        return Err("Codex abort cannot be combined with an approve decision".to_string());
    }
    Ok((approved, apply_to_future, abort_selected))
}

fn extract_interaction_id(params: &Value) -> String {
    params
        .get("requestId")
        .or_else(|| params.get("request_id"))
        .or_else(|| params.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| "codex-approval".to_string())
}

fn provider_notification(kind: MethodKind, method: &str, params: &Value) -> AgentEventKind {
    let classification = method_kind_classification(kind);
    let detail = match kind {
        MethodKind::Unknown => {
            format!(
                "unrecognized Codex notification method '{}'; recorded as fallback",
                truncate_for_envelope(method, 128)
            )
        }
        MethodKind::ThreadStatusChanged => {
            let status = params
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            format!("thread status → {}", truncate_for_envelope(status, 64))
        }
        MethodKind::TokenUsageUpdated => "token usage updated".to_string(),
        MethodKind::PlanUpdated | MethodKind::PlanDelta => "plan updated".to_string(),
        MethodKind::AgentMessageDelta => "agent message delta".to_string(),
        MethodKind::CommandExecutionOutputDelta => "command output delta".to_string(),
        MethodKind::FileChangeOutputDelta => "file change delta".to_string(),
        MethodKind::ItemStarted => {
            let item_type = params
                .pointer("/item/type")
                .and_then(Value::as_str)
                .unwrap_or("item");
            format!("item started ({})", truncate_for_envelope(item_type, 64))
        }
        MethodKind::ItemCompleted => {
            let item_type = params
                .pointer("/item/type")
                .and_then(Value::as_str)
                .unwrap_or("item");
            format!("item completed ({})", truncate_for_envelope(item_type, 64))
        }
        MethodKind::TaskStarted => "task started".to_string(),
        MethodKind::TaskCompleted => "task completed".to_string(),
        MethodKind::Initialized => "app-server initialized".to_string(),
        MethodKind::ThreadStarted => "thread started".to_string(),
        MethodKind::ThreadNameUpdated => "thread name updated".to_string(),
        MethodKind::ThreadArchived => "thread archived".to_string(),
        MethodKind::ThreadCompacted => "thread compacted".to_string(),
        MethodKind::ThreadClosed => "thread closed".to_string(),
        _ => classification.to_string(),
    };
    AgentEventKind::ProviderNotification {
        provider: CODEX_PROVIDER.to_string(),
        method: truncate_for_envelope(method, 256),
        classification: classification.to_string(),
        detail: truncate_for_envelope(&detail, 512),
    }
}

fn truncate_for_envelope(input: &str, max_chars: usize) -> String {
    let mut iter = input.char_indices();
    match iter.nth(max_chars) {
        Some((idx, _)) => format!("{}…(truncated)", &input[..idx]),
        None => input.to_string(),
    }
}

/// Overlay lifecycle fields from normalized runtime envelope kinds onto a
/// Code UI snapshot. Content (transcript/plan/tools) remains hydrated from the
/// managed Codex session mirror; this function is the shared lifecycle
/// authority so Codex does not project status through a private path.
///
/// `InteractionRequested` only forces `CodeUiSessionStatus::AwaitingInteraction`
/// when the hydrated snapshot still has a pending interaction. Approvals that
/// were resolved in the same notification batch must not leave the UI stuck
/// awaiting an action that is no longer available.
pub fn apply_agent_event_kinds_to_code_ui_status(
    snapshot: &mut crate::internal::ai::web::code_ui::CodeUiSessionSnapshot,
    kinds: &[AgentEventKind],
) {
    use crate::internal::ai::web::code_ui::{CodeUiInteractionStatus, CodeUiSessionStatus};
    let has_pending_interaction = snapshot
        .interactions
        .iter()
        .any(|interaction| matches!(interaction.status, CodeUiInteractionStatus::Pending));
    for kind in kinds {
        match kind {
            AgentEventKind::TurnQueued | AgentEventKind::TurnStarted => {
                snapshot.status = CodeUiSessionStatus::Thinking;
            }
            AgentEventKind::TurnCompleted { .. } => {
                snapshot.status = CodeUiSessionStatus::Completed;
            }
            AgentEventKind::TurnFailed { .. } => {
                snapshot.status = CodeUiSessionStatus::Error;
            }
            AgentEventKind::TurnCancelled => {
                snapshot.status = CodeUiSessionStatus::Idle;
            }
            AgentEventKind::TurnIndeterminateSideEffect { .. } => {
                snapshot.status = CodeUiSessionStatus::IndeterminateSideEffect;
            }
            AgentEventKind::CancelRequested => {
                // Code UI has no dedicated Cancelling status; surface as
                // Thinking until the terminal cancel/complete event arrives.
                snapshot.status = CodeUiSessionStatus::Thinking;
            }
            AgentEventKind::InteractionRequested { .. } => {
                if has_pending_interaction {
                    snapshot.status = CodeUiSessionStatus::AwaitingInteraction;
                }
            }
            AgentEventKind::InteractionResponded { .. } => {
                if !has_pending_interaction
                    && matches!(snapshot.status, CodeUiSessionStatus::AwaitingInteraction)
                {
                    snapshot.status = CodeUiSessionStatus::Thinking;
                }
            }
            AgentEventKind::ProviderNotification { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn unknown_method_uses_diagnosable_fallback() {
        let envelope = normalize_codex_notification("codex/future/brandNew", &json!({}));
        assert!(envelope.used_fallback);
        assert_eq!(envelope.classification, "unknown");
        match envelope.primary_kind() {
            Some(AgentEventKind::ProviderNotification {
                provider,
                method,
                classification,
                detail,
            }) => {
                assert_eq!(provider, CODEX_PROVIDER);
                assert_eq!(method, "codex/future/brandNew");
                assert_eq!(classification, "unknown");
                assert!(detail.contains("unrecognized"));
            }
            other => panic!("expected ProviderNotification fallback, got {other:?}"),
        }
    }

    #[test]
    fn turn_and_approval_map_to_first_class_kinds() {
        let started = normalize_codex_notification("turn/started", &json!({}));
        assert!(matches!(
            started.primary_kind(),
            Some(AgentEventKind::TurnStarted)
        ));

        let completed =
            normalize_codex_notification("turn/completed", &json!({"status": "completed"}));
        assert!(matches!(
            completed.primary_kind(),
            Some(AgentEventKind::TurnCompleted { .. })
        ));

        let failed = normalize_codex_notification("turn/completed", &json!({"status": "failed"}));
        assert!(matches!(
            failed.primary_kind(),
            Some(AgentEventKind::TurnFailed { .. })
        ));

        let approval = normalize_codex_notification(
            "item/commandExecution/requestApproval",
            &json!({"requestId": "req-1"}),
        );
        match approval.primary_kind() {
            Some(AgentEventKind::InteractionRequested {
                state:
                    InteractionState::AwaitingToolApproval {
                        interaction_id,
                        tool_name,
                    },
            }) => {
                assert_eq!(interaction_id, "req-1");
                assert_eq!(tool_name, "command_execution");
                let projected = super::codex_tool_approval_interaction(
                    interaction_id,
                    tool_name,
                    None,
                    None,
                    json!({}),
                    chrono::Utc::now(),
                );
                assert_eq!(projected.id, "req-1");
                assert_eq!(
                    projected
                        .options
                        .iter()
                        .map(|option| option.id.as_str())
                        .collect::<Vec<_>>(),
                    vec!["approve", "deny", "abort"]
                );
            }
            other => panic!("expected InteractionRequested, got {other:?}"),
        }
    }

    #[test]
    fn turn_completed_terminal_statuses_map_to_persisted_run_status() {
        assert_eq!(
            run_status_for_turn_completed(&json!({"status": "failed"})),
            super::super::types::RunStatus::Failed
        );
        assert_eq!(
            run_status_for_turn_completed(&json!({"status": "cancelled"})),
            super::super::types::RunStatus::Cancelled
        );
        assert_eq!(
            run_status_for_turn_completed(&json!({"status": "completed"})),
            super::super::types::RunStatus::Completed
        );
        assert_eq!(
            run_status_for_turn_completed(&json!({"error": {"message": "boom"}})),
            super::super::types::RunStatus::Failed
        );
        assert_eq!(
            run_status_for_turn_completed(&json!({"status": "interrupted"})),
            super::super::types::RunStatus::Cancelled
        );
    }

    #[test]
    fn approval_response_rejects_conflicting_decision_fields() {
        use crate::internal::ai::web::code_ui::CodeUiInteractionResponse;

        let conflict = decision_from_code_ui_interaction_response(&CodeUiInteractionResponse {
            approved: Some(false),
            selected_option: Some("approve".to_string()),
            apply_to_future: None,
            answers: Default::default(),
            max_attempts: None,
            note: None,
        });
        assert!(
            conflict.is_err(),
            "approved vs selectedOption must conflict"
        );

        let future_conflict =
            decision_from_code_ui_interaction_response(&CodeUiInteractionResponse {
                approved: Some(false),
                selected_option: None,
                apply_to_future: Some(CodeUiApplyToFuture::AcceptAll),
                answers: Default::default(),
                max_attempts: None,
                note: None,
            });
        assert!(
            future_conflict.is_err(),
            "deny + accept_all must not switch future mode"
        );

        let ok = decision_from_code_ui_interaction_response(&CodeUiInteractionResponse {
            approved: Some(true),
            selected_option: Some("approve_all".to_string()),
            apply_to_future: None,
            answers: Default::default(),
            max_attempts: None,
            note: None,
        })
        .expect("matching approve_all must parse");
        assert!(ok.0);
        assert_eq!(ok.1, Some(CodeUiApplyToFuture::AcceptAll));
        assert!(!ok.2);
    }
}
