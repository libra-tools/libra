//! Runtime-owned failure classification and repair-loop state for plan execution.
//!
//! Adapters build an [`ExecutionFailureEvidence`] from their execution result,
//! then consume the resulting [`PlanExecutionRepairState`].  In particular,
//! adapters must not re-classify failures or calculate retry limits themselves.

use std::{
    io,
    sync::{Arc, atomic::AtomicBool},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::internal::ai::{
    orchestrator::types::{DecisionOutcome, OrchestratorResult, TaskNodeStatus},
    runtime::worker::{
        AgentRuntimeHandle, InteractionResponse, InteractionState, RuntimeExecutionContext,
        RuntimeInteractionDelivery, RuntimeTurnExecution, RuntimeWorkerError, TurnRequest,
    },
    session::{CodeWorkflowEventKind, jsonl::SessionJsonlStore},
};

/// The maximum number of automatic repair attempts a user may authorize.
pub const MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS: u8 = 10;
/// Automatic repair is opt-in by default.
pub const DEFAULT_AUTOMATIC_PLAN_REPAIR_ATTEMPTS: u8 = 0;
/// Evidence is sent to remote Code UI clients, so individual summaries must
/// remain useful without becoming an unbounded data-export surface.
const FAILURE_EVIDENCE_SUMMARY_LIMIT: usize = 512;

/// The single runtime classification for a failed plan execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionFailureRevision {
    PlanRevision,
    IntentSpecRevision,
    ManualAction,
}

/// Safe failure evidence projected through runtime/wire read models.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionFailureEvidence {
    pub output: String,
    #[serde(default)]
    pub diagnostics: Vec<String>,
    #[serde(default)]
    pub attempt: u8,
    #[serde(default)]
    pub max_attempts: u8,
}

/// Runtime-owned repair-loop status. This is serializable so Web Code UI and
/// remote adapters observe the same decision and evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum PlanExecutionRepairState {
    AutomaticRepair {
        route: ExecutionFailureRevision,
        evidence: ExecutionFailureEvidence,
    },
    AwaitingUser {
        interaction_id: String,
        route: ExecutionFailureRevision,
        evidence: ExecutionFailureEvidence,
    },
    IntentSpecRevision {
        evidence: ExecutionFailureEvidence,
    },
    ManualAction {
        evidence: ExecutionFailureEvidence,
    },
    Cancelled {
        evidence: ExecutionFailureEvidence,
    },
}

impl PlanExecutionRepairState {
    /// Map a pending repair to the worker-owned interaction surface.
    pub fn interaction_state(&self) -> Option<InteractionState> {
        match self {
            Self::AwaitingUser { interaction_id, .. } => {
                Some(InteractionState::AwaitingPlanRepair {
                    interaction_id: interaction_id.clone(),
                })
            }
            _ => None,
        }
    }

    pub fn evidence(&self) -> &ExecutionFailureEvidence {
        match self {
            Self::AutomaticRepair { evidence, .. }
            | Self::AwaitingUser { evidence, .. }
            | Self::IntentSpecRevision { evidence }
            | Self::ManualAction { evidence }
            | Self::Cancelled { evidence } => evidence,
        }
    }
}

/// Scan durable Code workflow events for an unresolved plan-execution repair
/// gate. Used on resume so Continue/Cancel remains mandatory after the worker
/// and its live snapshot have been recreated.
///
/// Returns the unresolved `(repair, turn_id)` pair after following any durable
/// supersession chain. Markers written before gate-turn recovery may have an
/// empty turn id; callers must allocate and persist a replacement before
/// re-parking the gate.
pub fn open_plan_execution_repair_from_workflow<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
) -> Option<(PlanExecutionRepairState, String)> {
    selected_plan_execution_repair_marker(unresolved_plan_execution_repair_markers(events))
        .map(|(repair, turn_id, _, _)| (repair, turn_id))
}

/// Find speculative repair continuations written after the oldest unresolved
/// repair gate. A continuation records its predecessor interaction id and is
/// persisted before that predecessor's acknowledgement. If a process fails
/// before that acknowledgement, restoring the predecessor must retire these
/// orphaned copies; otherwise resolving the predecessor would leave a stale
/// continuation to block the next restart.
pub fn speculative_plan_execution_repair_continuations_from_workflow<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
) -> Vec<String> {
    let unresolved = unresolved_plan_execution_repair_markers(events);
    let Some((predecessor, _, _, _)) = selected_plan_execution_repair_marker(unresolved.clone())
    else {
        return Vec::new();
    };
    let PlanExecutionRepairState::AwaitingUser {
        interaction_id: predecessor_interaction_id,
        ..
    } = &predecessor
    else {
        unreachable!("unresolved repair markers are awaiting user input");
    };
    unresolved
        .into_iter()
        .skip(1)
        .filter_map(
            |(candidate, _, continuation_predecessor_interaction_id, supersedes_predecessor)| {
                (!supersedes_predecessor
                    && (continuation_predecessor_interaction_id == *predecessor_interaction_id
                        || (continuation_predecessor_interaction_id.is_empty()
                            && same_repair_lineage(&predecessor, &candidate))))
                .then(|| match candidate {
                    PlanExecutionRepairState::AwaitingUser { interaction_id, .. } => interaction_id,
                    _ => unreachable!("unresolved repair markers are awaiting user input"),
                })
            },
        )
        .collect()
}

fn selected_plan_execution_repair_marker(
    unresolved: Vec<(PlanExecutionRepairState, String, String, bool)>,
) -> Option<(PlanExecutionRepairState, String, String, bool)> {
    let mut selected = unresolved.first()?.clone();
    loop {
        let PlanExecutionRepairState::AwaitingUser {
            interaction_id: selected_interaction_id,
            ..
        } = &selected.0
        else {
            unreachable!("unresolved repair markers are awaiting user input");
        };
        let Some(successor) = unresolved
            .iter()
            .rev()
            .find(|(_, _, predecessor, supersedes)| {
                *supersedes && predecessor == selected_interaction_id
            })
        else {
            return Some(selected);
        };
        selected = successor.clone();
    }
}

fn unresolved_plan_execution_repair_markers<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
) -> Vec<(PlanExecutionRepairState, String, String, bool)> {
    use std::collections::HashMap;

    use crate::internal::ai::session::CodeWorkflowEventKind;

    let mut open: HashMap<String, (PlanExecutionRepairState, String, String, bool)> =
        HashMap::new();
    let mut order = Vec::new();
    for event in events {
        match event {
            CodeWorkflowEventKind::PlanExecutionRepairRequested {
                interaction_id,
                turn_id,
                predecessor_interaction_id,
                supersedes_predecessor,
                repair: repair @ PlanExecutionRepairState::AwaitingUser { .. },
            } => {
                if open
                    .insert(
                        interaction_id.clone(),
                        (
                            repair.clone(),
                            turn_id.clone(),
                            predecessor_interaction_id.clone(),
                            *supersedes_predecessor,
                        ),
                    )
                    .is_none()
                {
                    order.push(interaction_id.clone());
                }
            }
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                intent_revision_consumption: None,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                ..
            } => {
                open.remove(interaction_id);
                order.retain(|id| id != interaction_id);
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .filter_map(|interaction_id| open.remove(&interaction_id))
        .collect()
}

fn same_repair_lineage(
    predecessor: &PlanExecutionRepairState,
    candidate: &PlanExecutionRepairState,
) -> bool {
    matches!(
        (predecessor, candidate),
        (
            PlanExecutionRepairState::AwaitingUser {
                route: predecessor_route,
                evidence: predecessor_evidence,
                ..
            },
            PlanExecutionRepairState::AwaitingUser {
                route: candidate_route,
                evidence: candidate_evidence,
                ..
            },
        ) if predecessor_route == candidate_route && predecessor_evidence == candidate_evidence
    )
}

/// Persist the repair marker before the execution handoff is cleared. A crash
/// after that clear must still restore the mandatory Continue/Cancel gate.
pub fn persist_plan_execution_repair_gate(
    store: &SessionJsonlStore,
    repair: &PlanExecutionRepairState,
    gate_turn_id: &str,
) -> io::Result<()> {
    persist_plan_execution_repair_gate_with_predecessor(store, repair, gate_turn_id, None)
}

/// Persist a repair gate, recording its unresolved predecessor when this is a
/// speculative continuation written before the predecessor is acknowledged.
pub fn persist_plan_execution_repair_gate_with_predecessor(
    store: &SessionJsonlStore,
    repair: &PlanExecutionRepairState,
    gate_turn_id: &str,
    predecessor_interaction_id: Option<&str>,
) -> io::Result<()> {
    persist_plan_execution_repair_gate_with_lineage(
        store,
        repair,
        gate_turn_id,
        predecessor_interaction_id,
        false,
    )
}

/// Persist a replacement repair gate that makes its unresolved predecessor
/// obsolete. Recovery follows this durable supersession link if a crash occurs
/// before the predecessor's resolution marker is appended.
pub fn persist_plan_execution_repair_gate_superseding(
    store: &SessionJsonlStore,
    repair: &PlanExecutionRepairState,
    gate_turn_id: &str,
    predecessor_interaction_id: &str,
) -> io::Result<()> {
    persist_plan_execution_repair_gate_with_lineage(
        store,
        repair,
        gate_turn_id,
        Some(predecessor_interaction_id),
        true,
    )
}

fn persist_plan_execution_repair_gate_with_lineage(
    store: &SessionJsonlStore,
    repair: &PlanExecutionRepairState,
    gate_turn_id: &str,
    predecessor_interaction_id: Option<&str>,
    supersedes_predecessor: bool,
) -> io::Result<()> {
    let PlanExecutionRepairState::AwaitingUser { interaction_id, .. } = repair else {
        return Ok(());
    };
    store
        .append_code_workflow_durable(CodeWorkflowEventKind::PlanExecutionRepairRequested {
            interaction_id: interaction_id.clone(),
            turn_id: gate_turn_id.to_string(),
            predecessor_interaction_id: predecessor_interaction_id.unwrap_or_default().to_string(),
            supersedes_predecessor,
            repair: repair.clone(),
        })
        .map(|_| ())
}

/// Park the worker-owned Continue/Cancel gate for a persisted repair marker.
///
/// Adapters must persist the marker first with
/// [`persist_plan_execution_repair_gate`] so a process failure between those
/// operations remains recoverable.
pub async fn park_plan_execution_repair_gate(
    runtime: &AgentRuntimeHandle,
    session_id: String,
    interaction_id: &str,
    runtime_turn_id: String,
) -> Result<(), RuntimeWorkerError> {
    runtime
        .track_external_turn(
            TurnRequest::new(
                session_id.clone(),
                runtime_turn_id.clone(),
                "Plan repair",
                false,
            ),
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .await?;
    if let Err(error) = runtime
        .register_interaction_with_delivery(
            session_id.clone(),
            runtime_turn_id.clone(),
            InteractionState::AwaitingPlanRepair {
                interaction_id: interaction_id.to_string(),
            },
            Box::new(PlanExecutionRepairAckDelivery),
        )
        .await
    {
        let _ = runtime
            .finish_external_turn(
                session_id,
                runtime_turn_id,
                Ok(RuntimeTurnExecution::CompletedDiscardQueued {
                    summary: "Plan repair gate registration failed".to_string(),
                }),
            )
            .await;
        return Err(error);
    }
    Ok(())
}

/// Persist and then park an awaiting repair gate using the ordering required by
/// the historical `ExecuteWorkflowComplete` adapter. A crash after persistence can
/// be restored; a crash before persistence leaves the prior execution handoff
/// intact for reconciliation.
pub async fn persist_and_park_plan_execution_repair_gate(
    store: &SessionJsonlStore,
    runtime: &AgentRuntimeHandle,
    session_id: String,
    repair: &PlanExecutionRepairState,
    gate_turn_id: String,
) -> Result<(), RuntimeWorkerError> {
    persist_plan_execution_repair_gate(store, repair, &gate_turn_id).map_err(|error| {
        RuntimeWorkerError::DurabilityFailure(format!(
            "failed to persist plan-execution repair gate: {error}"
        ))
    })?;
    let Some(interaction_id) =
        repair
            .interaction_state()
            .and_then(|interaction| match interaction {
                InteractionState::AwaitingPlanRepair { interaction_id } => Some(interaction_id),
                _ => None,
            })
    else {
        return Ok(());
    };
    park_plan_execution_repair_gate(runtime, session_id, &interaction_id, gate_turn_id).await
}

/// Worker-owned acknowledgement for an `AwaitingPlanRepair` interaction.
///
/// The Web Code UI owns the subsequent re-planning UI transition, while the
/// runtime owns validation and session fencing until the developer chooses
/// continue/cancel.
#[derive(Clone, Debug, Default)]
pub struct PlanExecutionRepairAckDelivery;

#[async_trait]
impl RuntimeInteractionDelivery for PlanExecutionRepairAckDelivery {
    fn validate(&self, interaction: &InteractionResponse) -> Result<(), RuntimeWorkerError> {
        matches!(
            interaction.response.trim().to_ascii_lowercase().as_str(),
            "continue" | "cancel"
        )
        .then_some(())
        .ok_or_else(|| {
            RuntimeWorkerError::ExecutionFailed(
                "unrecognized plan repair response; expected continue or cancel".to_string(),
            )
        })
    }

    fn persist_interaction_resolved_after_terminal(&self) -> bool {
        true
    }

    async fn deliver(
        self: Box<Self>,
        _request: TurnRequest,
        interaction: InteractionResponse,
        _context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        match interaction.response.trim().to_ascii_lowercase().as_str() {
            "continue" => Ok(RuntimeTurnExecution::Completed {
                summary: "Plan repair continuation accepted".to_string(),
            }),
            "cancel" => Ok(RuntimeTurnExecution::CompletedDiscardQueued {
                summary: "Plan repair cancelled".to_string(),
            }),
            _ => Err(RuntimeWorkerError::ExecutionFailed(
                "unrecognized plan repair response; expected continue or cancel".to_string(),
            )),
        }
    }
}

/// Runtime policy for classifying failure and advancing repair interactions.
#[derive(Clone, Debug, Default)]
pub struct PlanExecutionRepairService;

impl PlanExecutionRepairService {
    pub fn classify_failure_signals(
        abandoned: bool,
        missing_artifacts: bool,
        intentspec_policy_violation: bool,
    ) -> ExecutionFailureRevision {
        if missing_artifacts || intentspec_policy_violation {
            ExecutionFailureRevision::IntentSpecRevision
        } else if abandoned {
            ExecutionFailureRevision::PlanRevision
        } else {
            ExecutionFailureRevision::ManualAction
        }
    }

    pub fn should_auto_repair(
        route: ExecutionFailureRevision,
        attempts: u8,
        max_attempts: u8,
    ) -> bool {
        route == ExecutionFailureRevision::PlanRevision && attempts < max_attempts
    }

    /// Classify an orchestrator result exactly once for every adapter.
    pub fn classify_execution_failure(
        result: Option<&OrchestratorResult>,
        execution_summary: Option<&str>,
    ) -> ExecutionFailureRevision {
        if let Some(result) = result {
            let requires_intentspec = !result.system_report.missing_artifacts.is_empty()
                || result.task_results.iter().any(|task| {
                    task.policy_violations.iter().any(|violation| {
                        matches!(
                            violation.code.as_str(),
                            "scope-creep"
                                | "network-policy-deny"
                                | "tool-acl-deny"
                                | "sandbox-escalation-deny"
                                | "git-version-control-deny"
                        )
                    })
                });
            return Self::classify_failure_signals(
                result.decision == DecisionOutcome::Abandon,
                !result.system_report.missing_artifacts.is_empty(),
                requires_intentspec,
            );
        }

        let manual_blocker = execution_summary
            .and_then(orchestrator_failure_detail)
            .is_some_and(|detail| {
                [
                    "config error",
                    "configuration",
                    "mcp",
                    "persisted plan",
                    "persistence",
                    "database",
                    "sqlite",
                    "store",
                ]
                .iter()
                .any(|needle| detail.to_ascii_lowercase().contains(needle))
            });
        let _ = manual_blocker;
        ExecutionFailureRevision::ManualAction
    }

    /// Produce wire-safe evidence from the execution result.
    pub fn failure_evidence(
        result: Option<&OrchestratorResult>,
        execution_summary: Option<&str>,
        attempt: u8,
        max_attempts: u8,
    ) -> ExecutionFailureEvidence {
        let mut diagnostics = Vec::new();
        let output = if let Some(result) = result {
            for task in result
                .task_results
                .iter()
                .filter(|task| task.status == TaskNodeStatus::Failed)
                .take(3)
            {
                if let Some(output) = task
                    .agent_output
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    diagnostics.push(redacted_failure_summary(output));
                }
                diagnostics.extend(
                    task.policy_violations
                        .iter()
                        .map(|violation| {
                            redacted_failure_summary(&format!(
                                "{}: {}",
                                violation.code, violation.message
                            ))
                        })
                        .take(3),
                );
            }
            format!("Decision: {:?}.", result.decision)
        } else {
            redacted_failure_summary(
                execution_summary
                    .map(str::trim)
                    .filter(|summary| !summary.is_empty())
                    .unwrap_or("The orchestrator failed before producing a final decision."),
            )
        };
        ExecutionFailureEvidence {
            output,
            diagnostics,
            attempt,
            max_attempts,
        }
    }

    /// Enter the repair loop after a failed execution.
    pub fn after_failure(
        &self,
        interaction_id: impl Into<String>,
        result: Option<&OrchestratorResult>,
        execution_summary: Option<&str>,
        attempts: u8,
        max_attempts: u8,
    ) -> PlanExecutionRepairState {
        let route = Self::classify_execution_failure(result, execution_summary);
        let mut evidence =
            Self::failure_evidence(result, execution_summary, attempts, max_attempts);
        match route {
            ExecutionFailureRevision::PlanRevision
                if Self::should_auto_repair(route, attempts, max_attempts) =>
            {
                evidence.attempt = evidence.attempt.saturating_add(1);
                PlanExecutionRepairState::AutomaticRepair { route, evidence }
            }
            ExecutionFailureRevision::PlanRevision => PlanExecutionRepairState::AwaitingUser {
                interaction_id: interaction_id.into(),
                route,
                evidence,
            },
            ExecutionFailureRevision::IntentSpecRevision => {
                PlanExecutionRepairState::IntentSpecRevision { evidence }
            }
            ExecutionFailureRevision::ManualAction => {
                PlanExecutionRepairState::ManualAction { evidence }
            }
        }
    }

    /// Process the runtime interaction response. `continue` enters an automatic
    /// repair only within the hard cap; `cancel` is terminal. Other responses
    /// leave the adapter to open an explicit plan-modification interaction.
    pub fn respond(
        &self,
        state: PlanExecutionRepairState,
        response: &str,
        requested_max_attempts: Option<u8>,
    ) -> PlanExecutionRepairState {
        let PlanExecutionRepairState::AwaitingUser {
            interaction_id,
            route,
            mut evidence,
        } = state
        else {
            return state;
        };
        match response.trim().to_ascii_lowercase().as_str() {
            "cancel" | "/plan cancel" => PlanExecutionRepairState::Cancelled { evidence },
            "continue" | "/plan continue" if route == ExecutionFailureRevision::PlanRevision => {
                let next_attempt = evidence.attempt.saturating_add(1);
                let max_attempts = requested_max_attempts
                    .filter(|requested| {
                        *requested > evidence.max_attempts
                            && *requested <= MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS
                    })
                    .unwrap_or(evidence.max_attempts);
                if next_attempt <= max_attempts {
                    evidence.attempt = next_attempt;
                    evidence.max_attempts = max_attempts;
                    PlanExecutionRepairState::AutomaticRepair { route, evidence }
                } else {
                    PlanExecutionRepairState::AwaitingUser {
                        interaction_id,
                        route,
                        evidence,
                    }
                }
            }
            _ => PlanExecutionRepairState::AwaitingUser {
                interaction_id,
                route,
                evidence,
            },
        }
    }
}

/// Redact and bound a string before it becomes runtime repair evidence.
///
/// Repair adapters use this for all supplemental evidence, including failures
/// that happen while re-planning rather than during orchestration.
pub fn redacted_failure_summary(value: &str) -> String {
    let redacted = crate::internal::ai::runtime::SecretRedactor::default_runtime().redact(value);
    let mut end = redacted
        .char_indices()
        .nth(FAILURE_EVIDENCE_SUMMARY_LIMIT)
        .map_or(redacted.len(), |(index, _)| index);
    if end < redacted.len() {
        while !redacted.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &redacted[..end])
    } else {
        redacted
    }
}

fn orchestrator_failure_detail(summary: &str) -> Option<&str> {
    summary
        .trim()
        .strip_prefix("Orchestrator failed:")
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
}
