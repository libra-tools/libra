//! Replayable Code UI projection over the additive session workflow stream.
//!
//! This is deliberately a pure fold: the live Web/TUI adapters may cache the
//! resulting snapshot, but they do not become the authority for transcript,
//! interaction, plan, task, tool-call, or patchset state.  A caller supplies a
//! legacy/bootstrap snapshot for immutable session metadata, then applies the
//! bounded fine-grained event suffix.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::code_ui::{
    CodeUiInteractionRequest, CodeUiInteractionStatus, CodeUiPatchsetSnapshot, CodeUiPlanSnapshot,
    CodeUiSessionSnapshot, CodeUiSessionStatus, CodeUiTaskSnapshot, CodeUiToolCallSnapshot,
    CodeUiTranscriptEntry,
};
use crate::internal::ai::session::{
    CodeWorkflowEventKind, CodeWorkflowReplay, CodeWorkflowSequenceGap,
};

/// The fold never silently applies an unbounded historical suffix.  A later
/// compaction/checkpoint card can advance this bounded replay window without
/// changing the event schema or resurrecting a second projection store.
pub const MAX_CODE_UI_PROJECTION_EVENTS: usize = 1024;
/// Maximum number of bytes read from the end of a session JSONL file during a
/// resume. A suffix that cannot be proven complete inside this window fails
/// closed and requires compaction/checkpoint recovery.
pub const MAX_CODE_UI_PROJECTION_REPLAY_BYTES: u64 = 8 * 1024 * 1024;

/// Result of folding a session-scoped Code UI event suffix.
#[derive(Debug, Clone)]
pub struct CodeUiProjectionFold {
    pub snapshot: CodeUiSessionSnapshot,
    pub last_sequence: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeUiProjectionFoldError {
    SequenceGap(CodeWorkflowSequenceGap),
    HistoryLimitExceeded { limit: usize, observed: usize },
    InvalidPayload { projection: String, reason: String },
}

impl fmt::Display for CodeUiProjectionFoldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SequenceGap(gap) => write!(
                f,
                "Code UI projection cannot resume across missing workflow events between sequences {} and {}",
                gap.after, gap.before
            ),
            Self::HistoryLimitExceeded { limit, observed } => write!(
                f,
                "Code UI projection suffix has {observed} events, exceeding the bounded replay limit of {limit}; create or load a compaction checkpoint before resuming",
            ),
            Self::InvalidPayload { projection, reason } => write!(
                f,
                "Code UI projection event '{projection}' has an invalid payload: {reason}",
            ),
        }
    }
}

impl std::error::Error for CodeUiProjectionFoldError {}

/// Canonical workflow-event fold for Code UI read-model fields.
///
/// Resume, SSE recovery, and graph/history Code-UI-equivalent read paths
/// must use this entry (or [`fold_graph_compatible_code_ui_snapshot`]) —
/// not a separate projection implementation.
pub fn rebuild_code_ui_read_model_from_events(
    bootstrap: CodeUiSessionSnapshot,
    replay: &CodeWorkflowReplay,
) -> Result<CodeUiProjectionFold, CodeUiProjectionFoldError> {
    fold_code_ui_snapshot(bootstrap, replay)
}

/// Graph/history read paths that surface transcript, interaction, and status
/// fields equivalent to Code UI must fold through the same entry as resume.
pub fn fold_graph_compatible_code_ui_snapshot(
    bootstrap: CodeUiSessionSnapshot,
    replay: &CodeWorkflowReplay,
) -> Result<CodeUiProjectionFold, CodeUiProjectionFoldError> {
    rebuild_code_ui_read_model_from_events(bootstrap, replay)
}

/// Fold ordered Code workflow events over a bootstrap snapshot.
///
/// Unknown projection names and historical `code_ui_projection_delta` rows
/// without the additive `payload` field are skipped for forward/backward
/// compatibility. Recognized names with malformed non-null payloads fail
/// closed rather than producing a plausible but incorrect resumed UI state.
pub fn fold_code_ui_snapshot(
    bootstrap: CodeUiSessionSnapshot,
    replay: &CodeWorkflowReplay,
) -> Result<CodeUiProjectionFold, CodeUiProjectionFoldError> {
    if let Some(gap) = replay.gaps.first() {
        return Err(CodeUiProjectionFoldError::SequenceGap(gap.clone()));
    }
    if replay.events.len() > MAX_CODE_UI_PROJECTION_EVENTS {
        return Err(CodeUiProjectionFoldError::HistoryLimitExceeded {
            limit: MAX_CODE_UI_PROJECTION_EVENTS,
            observed: replay.events.len(),
        });
    }

    let mut snapshot = bootstrap;
    let mut last_sequence = None;
    for workflow_event in &replay.events {
        last_sequence = Some(workflow_event.sequence);
        match &workflow_event.event {
            CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection,
                payload,
                ..
            } if !payload.is_null() => apply_projection_delta(
                &mut snapshot,
                projection,
                payload,
                workflow_event.recorded_at,
            )?,
            CodeWorkflowEventKind::IndeterminateSideEffect { .. }
            | CodeWorkflowEventKind::CommandIndeterminateSideEffect { .. } => {
                snapshot.status = CodeUiSessionStatus::IndeterminateSideEffect;
                snapshot.updated_at = workflow_event.recorded_at;
            }
            CodeWorkflowEventKind::TerminalSuccess { .. }
            | CodeWorkflowEventKind::CommandTerminalSuccess { .. } => {
                snapshot.status = CodeUiSessionStatus::Completed;
                snapshot.updated_at = workflow_event.recorded_at;
            }
            CodeWorkflowEventKind::TerminalFailure { .. }
            | CodeWorkflowEventKind::CommandTerminalFailure { .. } => {
                snapshot.status = CodeUiSessionStatus::Error;
                snapshot.updated_at = workflow_event.recorded_at;
            }
            _ => {}
        }
    }

    Ok(CodeUiProjectionFold {
        snapshot,
        last_sequence,
    })
}

fn apply_projection_delta(
    snapshot: &mut CodeUiSessionSnapshot,
    projection: &str,
    payload: &serde_json::Value,
    recorded_at: DateTime<Utc>,
) -> Result<(), CodeUiProjectionFoldError> {
    match projection {
        "status" => snapshot.status = decode(projection, payload)?,
        "controller" => snapshot.controller = decode(projection, payload)?,
        "transcript_upsert" => {
            let entry: CodeUiTranscriptEntry = decode(projection, payload)?;
            upsert_by_id(&mut snapshot.transcript, entry, |entry| entry.id.as_str());
        }
        "assistant_delta" => {
            let delta: AssistantDelta = decode(projection, payload)?;
            if let Some(entry) = snapshot
                .transcript
                .iter_mut()
                .find(|entry| entry.id == delta.entry_id)
            {
                if entry
                    .status
                    .as_deref()
                    .is_some_and(|status| matches!(status, "completed" | "error" | "cancelled"))
                {
                    return Ok(());
                }
                entry
                    .content
                    .get_or_insert_with(String::new)
                    .push_str(&delta.delta);
                entry.streaming = true;
                entry.updated_at = delta.updated_at;
            }
        }
        "interaction_upsert" => {
            let interaction: CodeUiInteractionRequest = decode(projection, payload)?;
            upsert_by_id(&mut snapshot.interactions, interaction, |item| {
                item.id.as_str()
            });
        }
        "interaction_resolved" => {
            let resolution: InteractionResolution = decode(projection, payload)?;
            if let Some(interaction) = snapshot
                .interactions
                .iter_mut()
                .find(|item| item.id == resolution.interaction_id)
            {
                interaction.status = CodeUiInteractionStatus::Resolved;
                interaction.resolved_at = Some(resolution.resolved_at);
            }
        }
        "interaction_cleared" => {
            let clear: InteractionClear = decode(projection, payload)?;
            snapshot
                .interactions
                .retain(|interaction| interaction.id != clear.interaction_id);
        }
        "plan_upsert" => {
            let plan: CodeUiPlanSnapshot = decode(projection, payload)?;
            if let Some(existing) = snapshot.plans.iter().find(|item| item.id == plan.id)
                && is_terminal_plan_status(&existing.status)
                && !is_terminal_plan_status(&plan.status)
            {
                return Ok(());
            }
            upsert_by_id(&mut snapshot.plans, plan, |item| item.id.as_str());
        }
        "task_upsert" => {
            let task: CodeUiTaskSnapshot = decode(projection, payload)?;
            upsert_by_id(&mut snapshot.tasks, task, |item| item.id.as_str());
        }
        "tool_call_upsert" => {
            let tool_call: CodeUiToolCallSnapshot = decode(projection, payload)?;
            upsert_by_id(&mut snapshot.tool_calls, tool_call, |item| item.id.as_str());
        }
        "patchset_upsert" => {
            let patchset: CodeUiPatchsetSnapshot = decode(projection, payload)?;
            upsert_by_id(&mut snapshot.patchsets, patchset, |item| item.id.as_str());
        }
        _ => return Ok(()),
    }
    snapshot.updated_at = recorded_at;
    Ok(())
}

fn decode<T: for<'de> Deserialize<'de>>(
    projection: &str,
    payload: &serde_json::Value,
) -> Result<T, CodeUiProjectionFoldError> {
    serde_json::from_value(payload.clone()).map_err(|error| {
        CodeUiProjectionFoldError::InvalidPayload {
            projection: projection.to_string(),
            reason: error.to_string(),
        }
    })
}

fn upsert_by_id<T, F>(items: &mut Vec<T>, incoming: T, id_fn: F)
where
    F: Fn(&T) -> &str,
{
    let id = id_fn(&incoming).to_string();
    if let Some(existing) = items.iter_mut().find(|item| id_fn(item) == id) {
        *existing = incoming;
    } else {
        items.push(incoming);
    }
}

fn is_terminal_plan_status(status: &str) -> bool {
    matches!(status, "completed" | "failed")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssistantDelta {
    entry_id: String,
    delta: String,
    updated_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InteractionResolution {
    interaction_id: String,
    resolved_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InteractionClear {
    interaction_id: String,
}
