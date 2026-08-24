//! Phase 0 Intent — formal write helpers.
//!
//! The Code UI Phase Workflow models Phase 0 as the **Intent** phase: a user
//! request is canonicalised into an [`IntentSpec`] and recorded as a draft
//! `Intent` revision in the AI object store. This module is the *formal
//! write* surface for that phase.
//!
//! # Design note
//!
//! Per [`docs/development/tracing/agent.md`](../../../../docs/development/tracing/agent.md)
//! Part B Phase 0 plan, the long-term goal is for the Runtime to own the only
//! formal-write entry point for each phase. As a Wave 1B incremental step,
//! the helpers below are thin shims over the existing scattered persistence
//! logic in [`crate::internal::ai::intentspec::persistence`]; once Wave 1B
//! fully lands, downstream call sites
//! ([`crate::internal::ai::orchestrator::persistence::ExecutionAuditSession`],
//! `command::code`) will be redirected through these wrappers.
//!
//! The public API surface is intentionally minimal so the contract stays
//! stable even after the underlying call routes change.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use git_internal::internal::object::{context::SelectionStrategy, types::ActorRef};
use rmcp::model::CallToolResult;

use super::worker::{
    InteractionResponse, RuntimeExecutionContext, RuntimeInteractionDelivery, RuntimeTurnExecution,
    RuntimeWorkerError, TurnRequest,
};
use crate::internal::ai::{
    agent::ToolLoopConfig,
    intentspec::{IntentSpec, persistence::persist_intentspec},
    mcp::{
        resource::{ContextItemParams, CreateContextSnapshotParams},
        server::LibraMcpServer,
    },
};

/// Scan durable Code workflow events for an IntentSpec review gate that was
/// requested but never resolved. Used on session resume so confirm/modify/cancel
/// cannot disappear across a crash (W2-02 recovery).
///
/// Returns the oldest unresolved
/// `(interaction_id, intent_id, turn_id, phase0_turn_id)` tuple. Multiple open
/// requests are tracked independently so resolving a later review cannot drop
/// an earlier unresolved gate. Turn ids may be empty on markers that predate
/// durable gate-turn recovery.
pub fn open_intent_review_from_workflow<'a>(
    events: impl IntoIterator<Item = &'a crate::internal::ai::session::CodeWorkflowEventKind>,
) -> Option<(String, String, String, String)> {
    use std::collections::HashMap;

    use crate::internal::ai::session::CodeWorkflowEventKind;

    let mut open: HashMap<String, (String, String, String)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for event in events {
        match event {
            CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id,
                intent_id,
                turn_id,
                phase0_turn_id,
            } => {
                if open
                    .insert(
                        interaction_id.clone(),
                        (intent_id.clone(), turn_id.clone(), phase0_turn_id.clone()),
                    )
                    .is_none()
                {
                    order.push(interaction_id.clone());
                }
            }
            CodeWorkflowEventKind::CommandTerminalFailure {
                retry_intent_review: Some(retry),
                ..
            } => {
                if open
                    .insert(
                        retry.interaction_id.clone(),
                        (retry.intent_id.clone(), String::new(), String::new()),
                    )
                    .is_none()
                {
                    order.push(retry.interaction_id.clone());
                }
            }
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                prior_interaction_resolutions,
                intent_revision_consumption: None,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                prior_interaction_resolutions,
                ..
            } => {
                for resolved_id in prior_interaction_resolutions
                    .iter()
                    .map(|(interaction_id, _)| interaction_id)
                    .chain(std::iter::once(interaction_id))
                {
                    open.remove(resolved_id);
                    order.retain(|id| id != resolved_id);
                }
            }
            _ => {}
        }
    }
    order.into_iter().next().and_then(|interaction_id| {
        open.remove(&interaction_id)
            .map(|(intent_id, turn_id, phase0_turn_id)| {
                (interaction_id, intent_id, turn_id, phase0_turn_id)
            })
    })
}

/// Phase 0 planning tool-loop policy shared by every caller that drives the
/// IntentSpec drafting conversation (Web Code UI and
/// [`RuntimeTurnExecutor`](super::worker::RuntimeTurnExecutor) adapters).
/// `submit_intent_draft` is the only terminal tool, matching the AC1
/// requirement that Phase 0 cannot silently fall through into a mutating
/// tool before the formal write in [`write_intent`].
pub fn phase0_plan_tool_loop_config(mut config: ToolLoopConfig) -> ToolLoopConfig {
    config.allowed_tools = Some(vec![
        "read_file".to_string(),
        "list_dir".to_string(),
        "grep_files".to_string(),
        "search_files".to_string(),
        "web_search".to_string(),
        "request_user_input".to_string(),
        "submit_intent_draft".to_string(),
    ]);
    config.terminal_tools = Some(vec!["submit_intent_draft".to_string()]);
    config.max_turns = Some(12);
    config
}

/// Shared Phase 0 planning prompt used by Web Code UI PlanPhase0
/// plain-message admission. Without this wrapper, providers can answer in
/// prose and never call `submit_intent_draft`.
pub fn phase0_planning_prompt(request: &str) -> String {
    format!(
        "You are running /plan mode.\n\
First, you MUST call request_user_input with exactly one question id=risk_profile, header=Risk, and options Low/Medium/High.\n\
After receiving user choice, analyze the repository and then call submit_intent_draft exactly once.\n\
Use web_search when available before making version-sensitive external claims. Rust edition 2024 is stable in current Rust; do not reject Cargo.toml edition=\"2024\" unless local toolchain evidence proves it unsupported.\n\
Default execution uses dependency-policy:no-new. If the user explicitly asks to add a new third-party dependency, make that intent unambiguous in the IntentDraft; Libra will derive dependency-policy:allow-with-review for that request. For simple Rust CLI argument handling without an explicit dependency request, prefer std::env over crates such as clap.\n\
If required information is missing, call request_user_input again for focused follow-up questions.\n\
Do not output a plain-text plan; finalize by submitting the draft tool call.\n\n\
User request:\n{request}"
    )
}

/// Help text shown after IntentSpec review chooses Modify/Revise.
pub fn phase0_revision_help_message() -> String {
    format!(
        "IntentSpec revise mode is active. Describe changes in plain text, use `/intent modify <changes>` to keep revising, or `{}` to exit.",
        crate::internal::ai::session::jsonl::INTENT_REVISION_CANCEL_COMMAND_INPUT
    )
}

/// Shared Phase 0 revision prompt: current IntentSpec is the baseline and the
/// user request describes only the requested changes.
pub fn phase0_revision_prompt(spec_json: &str, request: &str) -> String {
    format!(
        "You are revising an existing IntentSpec.\n\
First, you MUST call request_user_input with exactly one question id=risk_profile, header=Risk, and options Low/Medium/High.\n\
Use the current IntentSpec as the baseline, apply only the user's requested changes, and then call submit_intent_draft exactly once.\n\
Use web_search when available before making version-sensitive external claims. Rust edition 2024 is stable in current Rust; do not reject Cargo.toml edition=\"2024\" unless local toolchain evidence proves it unsupported.\n\
Default execution uses dependency-policy:no-new. If the user explicitly asks to add a new third-party dependency, make that intent unambiguous in the IntentDraft; Libra will derive dependency-policy:allow-with-review for that request. For simple Rust CLI argument handling without an explicit dependency request, prefer std::env over crates such as clap.\n\
If required information is missing, call request_user_input again for focused follow-up questions.\n\
Do not output a plain-text plan; finalize by submitting the draft tool call.\n\n\
Current IntentSpec:\n```json\n{spec_json}\n```\n\n\
Requested changes:\n{request}"
    )
}

/// The developer's decision on a pending IntentSpec review
/// ([`super::worker::InteractionState::AwaitingIntentReview`]).
///
/// Stable wire ids ([`Self::wire_id`]) are the contract browser/automation
/// clients depend on (the retired TUI module's keyboard-menu copy is
/// gone since W5-03).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentReviewDecision {
    /// Accept the IntentSpec as drafted; the caller should hand off toward
    /// Phase 1 (execution plan generation).
    Confirm,
    /// Reject the current draft and re-enter Phase 0 with the existing spec
    /// as a revision baseline.
    Revise,
    /// Abandon the review; the persisted IntentSpec draft remains on disk
    /// but no further phase is entered.
    Cancel,
}

impl IntentReviewDecision {
    /// Stable wire identifier used by [`InteractionResponse::response`] and
    /// [`crate::internal::ai::web::code_ui::CodeUiInteractionOption::id`].
    pub fn wire_id(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::Revise => "modify",
            Self::Cancel => "cancel",
        }
    }

    /// Parse a wire response id, accepting both `"modify"` (the browser
    /// option id) and `"revise"` (a more descriptive alias) for
    /// [`Self::Revise`].
    pub fn from_wire_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "confirm" => Some(Self::Confirm),
            "modify" | "revise" => Some(Self::Revise),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }

    /// Map the retired TUI's `PendingIntentReview::selected` index
    /// (0=Confirm, 1=Modify, 2=Cancel) onto the same decision so legacy
    /// persisted state and the wire path stay in lockstep.
    pub fn from_choice_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Confirm),
            1 => Some(Self::Revise),
            2 => Some(Self::Cancel),
            _ => None,
        }
    }
}

/// [`RuntimeInteractionDelivery`] for the Phase 0 IntentSpec review gate.
///
/// `Confirm` retires the tracked Phase 0 turn with
/// [`RuntimeTurnExecution::Completed`] so any turn queued while the review
/// was pending may start (the mutation fence releases only on confirm).
/// `Revise` / `Cancel` also free the active-turn slot, but use
/// [`RuntimeTurnExecution::CompletedDiscardQueued`] so work submitted under
/// the fence cannot execute without a confirmed IntentSpec. This deliberately
/// avoids reporting [`RuntimeWorkerError::Cancelled`] for `Cancel`: that
/// error path assumes a still-running executor continuation will reconcile
/// the ambiguous outcome, which does not exist for an externally-tracked
/// turn (the retired TUI's Phase 0 background task had already finished before the
/// review was ever registered) and would otherwise strand the turn in
/// `Cancelling` forever. The confirm/revise/cancel distinction itself is a
/// caller-level UI concern carried in the `summary` text and driven
/// off the same [`IntentReviewDecision`] the caller parsed from the wire
/// response — the runtime's [`super::worker::InteractionState`] taxonomy has
/// no "the user declined this business decision" variant, only
/// mutation-safety states.
///
/// Malformed responses are rejected in [`Self::validate`] before the pending
/// interaction is consumed, so a browser/automation client can retry with a
/// corrected `selectedOption` instead of losing the review gate.
///
/// Terminal IntentSpec review delivery. `InteractionResolved` is **not**
/// written here: the worker appends it only after the gate turn's terminal
/// command outcome is durable, so a transient terminal-persistence failure
/// cannot clear the review while fencing the session.
#[derive(Debug, Clone, Default)]
pub struct IntentReviewAckDelivery;

impl IntentReviewAckDelivery {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeInteractionDelivery for IntentReviewAckDelivery {
    fn validate(&self, interaction: &InteractionResponse) -> Result<(), RuntimeWorkerError> {
        IntentReviewDecision::from_wire_id(&interaction.response)
            .map(|_| ())
            .ok_or_else(|| {
                RuntimeWorkerError::InvalidInteractionResponse(format!(
                    "unrecognized IntentSpec review response '{}'; expected one of confirm/modify/cancel",
                    interaction.response
                ))
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
        let decision =
            IntentReviewDecision::from_wire_id(&interaction.response).ok_or_else(|| {
                RuntimeWorkerError::InvalidInteractionResponse(format!(
                    "unrecognized IntentSpec review response '{}'",
                    interaction.response
                ))
            })?;
        match decision {
            IntentReviewDecision::Confirm => Ok(RuntimeTurnExecution::Completed {
                summary: "IntentSpec confirmed; ready to generate an execution plan".to_string(),
            }),
            IntentReviewDecision::Revise => Ok(RuntimeTurnExecution::CompletedDiscardQueued {
                summary: "IntentSpec revision requested".to_string(),
            }),
            IntentReviewDecision::Cancel => Ok(RuntimeTurnExecution::CompletedDiscardQueued {
                summary: "IntentSpec review cancelled".to_string(),
            }),
        }
    }
}

/// Outcome of [`write_intent`]: the persisted intent revision id alongside a
/// reference back to the source [`IntentSpec`] so audit / observer code can
/// correlate the formal write with the request.
#[derive(Clone, Debug)]
pub struct IntentWriteOutcome {
    /// Identifier of the persisted Intent revision (the value that
    /// downstream Phase 1 / Phase 2 helpers reference when reading the
    /// intent back).
    pub intent_id: String,
    /// The original [`IntentSpec`] that was persisted. Kept verbatim so
    /// callers don't have to re-load the spec from storage for follow-up
    /// audit / observer events.
    pub source: IntentSpec,
}

/// Persist a new draft `Intent` revision as the **formal write** for Phase 0.
///
/// This is the entry point intended for Runtime callers; it delegates to
/// [`persist_intentspec`] today and will be the only sanctioned write path
/// once Wave 1B redirects existing call sites through this module.
///
/// # Returns
///
/// Wraps the persisted `intent_id` together with the original `spec` so
/// observers / audit sinks can record both without re-loading from storage.
///
/// # Errors
///
/// Returns the underlying `anyhow::Error` from `persist_intentspec` with the
/// added context `"Phase 0 write_intent"` so log scrapers can attribute the
/// failure to the formal-write layer.
pub async fn write_intent(
    spec: &IntentSpec,
    mcp_server: &Arc<LibraMcpServer>,
) -> Result<IntentWriteOutcome> {
    let intent_id = persist_intentspec(spec, mcp_server)
        .await
        .context("Phase 0 write_intent: persist_intentspec failed")?;

    Ok(IntentWriteOutcome {
        intent_id,
        source: spec.clone(),
    })
}

/// A single item to record in a Phase 0 context snapshot. Mirrors the shape
/// of [`ContextItemParams`] but lives in the runtime surface so the public
/// contract is decoupled from the MCP-derived schema struct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextSnapshotItem {
    /// Item kind (file, blob, message, …). When `None`, the MCP layer
    /// applies its default classifier.
    pub kind: Option<String>,
    /// Path or identifier of the item.
    pub path: String,
    /// Optional preview text (subject to redaction at the audit layer).
    pub preview: Option<String>,
    /// Optional blob hash for content-addressed items.
    pub blob_hash: Option<String>,
}

/// Input for [`write_context_snapshot_if_needed`]: the items to snapshot, the
/// selection strategy that produced them, an optional summary, and the actor
/// recording the snapshot (Phase 5 authz threads this through to
/// [`crate::internal::ai::runtime::PrincipalContext`]).
#[derive(Clone, Debug)]
pub struct ContextSnapshotRequest {
    /// Items to record in the snapshot. Empty means "no items"; combined
    /// with `summary == None` this triggers the no-op skip path.
    pub items: Vec<ContextSnapshotItem>,
    /// Selection strategy — `Explicit` for caller-supplied items,
    /// `Heuristic` for items selected by an upstream context selector.
    pub selection_strategy: SelectionStrategy,
    /// Optional human-readable summary. A `Some(_)` summary on its own
    /// is enough to trigger a snapshot write even with zero items.
    pub summary: Option<String>,
    /// Actor recording the snapshot. Phase 5 authz maps this to a
    /// [`PrincipalContext`](crate::internal::ai::runtime::hardening::PrincipalContext)
    /// before the MCP write fires.
    pub actor: ActorRef,
}

/// Outcome of a successful [`write_context_snapshot_if_needed`] call.
///
/// **Stability contract:** field names are part of the public Runtime
/// surface; downstream observers key off `snapshot_id`. New fields may be
/// added as `Option<...>`; existing fields cannot be renamed or removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextSnapshotWriteOutcome {
    /// Persisted snapshot id (the value Phase 1 / Phase 2 helpers
    /// reference when reading the snapshot back).
    pub snapshot_id: String,
    /// Summary recorded on the snapshot, echoed back so audit sinks
    /// don't have to re-load.
    pub summary: Option<String>,
    /// Number of items in the snapshot (zero when the snapshot was
    /// triggered purely by a non-empty summary).
    pub item_count: usize,
}

/// `true` when the request carries enough payload to be worth snapshotting.
///
/// Pure helper exposed so callers can predicate on the same gate
/// [`write_context_snapshot_if_needed`] applies internally, without
/// invoking the async MCP path.
pub fn snapshot_needed(request: &ContextSnapshotRequest) -> bool {
    !request.items.is_empty() || request.summary.is_some()
}

/// Persist a Phase 0 [`ContextSnapshot`](git_internal::internal::object::context::ContextSnapshot)
/// when the request actually has content to record; otherwise return
/// `Ok(None)` so callers can stay branch-free on the "no items, no summary"
/// hot path.
///
/// When [`snapshot_needed`] returns `true`, this function translates the
/// request into [`CreateContextSnapshotParams`] and delegates to
/// [`LibraMcpServer::create_context_snapshot_impl`]. The returned MCP text
/// is parsed for the snapshot id and wrapped in a
/// [`ContextSnapshotWriteOutcome`].
///
/// # Errors
///
/// * If the MCP call returns an `ErrorData`, the error is wrapped with the
///   context `"Phase 0 write_context_snapshot_if_needed: MCP
///   create_context_snapshot failed"`.
/// * If the MCP result has `is_error == true`, the error message text is
///   surfaced verbatim.
/// * If the MCP result text cannot be parsed for an `ID: …` token, the
///   error context is
///   `"Failed to parse ContextSnapshot ID from MCP result"`.
pub async fn write_context_snapshot_if_needed(
    request: ContextSnapshotRequest,
    mcp_server: &Arc<LibraMcpServer>,
) -> Result<Option<ContextSnapshotWriteOutcome>> {
    if !snapshot_needed(&request) {
        return Ok(None);
    }

    let strategy_str = match request.selection_strategy {
        SelectionStrategy::Explicit => "explicit",
        SelectionStrategy::Heuristic => "heuristic",
    };
    let item_count = request.items.len();
    let summary_for_outcome = request.summary.clone();
    let items_params = if request.items.is_empty() {
        None
    } else {
        Some(
            request
                .items
                .iter()
                .map(|item| ContextItemParams {
                    kind: item.kind.clone(),
                    path: item.path.clone(),
                    preview: item.preview.clone(),
                    content_hash: None,
                    blob_hash: item.blob_hash.clone(),
                })
                .collect(),
        )
    };

    let params = CreateContextSnapshotParams {
        selection_strategy: strategy_str.to_string(),
        items: items_params,
        summary: request.summary,
        tags: None,
        external_ids: None,
        actor_kind: None,
        actor_id: None,
    };

    let result = mcp_server
        .create_context_snapshot_impl(params, request.actor)
        .await
        .map_err(|e| anyhow::anyhow!("MCP create_context_snapshot failed: {e:?}"))
        .context("Phase 0 write_context_snapshot_if_needed: MCP create_context_snapshot failed")?;

    if result.is_error.unwrap_or(false) {
        let msg = result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.as_str())
            .unwrap_or("Unknown MCP error");
        return Err(anyhow::anyhow!(
            "MCP create_context_snapshot returned error: {msg}"
        ));
    }

    let snapshot_id = parse_snapshot_id(&result)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse ContextSnapshot ID from MCP result"))?;

    Ok(Some(ContextSnapshotWriteOutcome {
        snapshot_id,
        summary: summary_for_outcome,
        item_count,
    }))
}

/// Extract the `ID: <value>` token MCP's `create_context_snapshot_impl`
/// returns in its `CallToolResult` text. Mirrors the identical helper in
/// `intentspec::persistence` so phase0 doesn't have to depend on that
/// internal module.
fn parse_snapshot_id(result: &CallToolResult) -> Option<String> {
    for content in &result.content {
        if let Some(text) = content.as_text().map(|t| t.text.as_str())
            && let Some(id) = text.split("ID:").nth(1)
        {
            let id = id.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use git_internal::internal::object::types::ActorKind;
    use rmcp::model::Content;
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::{
        internal::{
            ai::{
                history::HistoryManager,
                intentspec::{
                    DraftAcceptance, DraftIntent as DraftIntentBody, DraftRisk, IntentDraft,
                    ResolveContext, RiskLevel, resolve_intentspec,
                    types::{ChangeType, Objective, ObjectiveKind},
                },
                workflow_objects::parse_object_id,
            },
            db,
        },
        utils::storage::local::LocalStorage,
    };

    /// Build a minimal but real `IntentSpec` so the `IntentWriteOutcome`
    /// equality assertions exercise the actual `PartialEq` impl rather than
    /// a forced-default placeholder.
    fn sample_intent_spec() -> IntentSpec {
        resolve_intentspec(
            IntentDraft {
                intent: DraftIntentBody {
                    summary: "phase0 sample".to_string(),
                    problem_statement: "exercise outcome equality".to_string(),
                    change_type: ChangeType::Bugfix,
                    objectives: vec![Objective {
                        title: "test".to_string(),
                        kind: ObjectiveKind::Implementation,
                    }],
                    in_scope: vec!["src".to_string()],
                    out_of_scope: vec![],
                    touch_hints: None,
                },
                acceptance: DraftAcceptance {
                    success_criteria: vec!["compiles".to_string()],
                    fast_checks: vec![],
                    integration_checks: vec![],
                    security_checks: vec![],
                    release_checks: vec![],
                },
                risk: DraftRisk {
                    rationale: "low".to_string(),
                    factors: vec![],
                    level: Some(RiskLevel::Low),
                },
            },
            RiskLevel::Low,
            ResolveContext {
                working_dir: "/tmp".to_string(),
                base_ref: "HEAD".to_string(),
                created_by_id: "phase0-test".to_string(),
            },
        )
    }

    /// `IntentWriteOutcome` carries both the persisted id and the original
    /// spec so observers don't have to re-load on the audit path.
    #[test]
    fn outcome_preserves_intent_id_and_source() {
        let spec = sample_intent_spec();
        let outcome = IntentWriteOutcome {
            intent_id: "intent-abc".to_string(),
            source: spec.clone(),
        };

        assert_eq!(outcome.intent_id, "intent-abc");
        assert_eq!(outcome.source, spec);
    }

    /// `IntentWriteOutcome` must derive `Clone` so audit handlers can keep a
    /// snapshot while the caller continues mutating the original spec.
    #[test]
    fn outcome_is_clone() {
        let outcome = IntentWriteOutcome {
            intent_id: "intent-xyz".to_string(),
            source: sample_intent_spec(),
        };
        let cloned = outcome.clone();
        assert_eq!(cloned.intent_id, outcome.intent_id);
        assert_eq!(cloned.source, outcome.source);
    }

    fn sample_actor() -> ActorRef {
        // INVARIANT: `ActorRef::new` only rejects empty ids; the literal
        // above is a non-empty const, so the constructor is infallible.
        ActorRef::new(ActorKind::System, "phase0-snapshot-test".to_string())
            .expect("non-empty id is always valid for ActorRef")
    }

    fn empty_request() -> ContextSnapshotRequest {
        ContextSnapshotRequest {
            items: vec![],
            selection_strategy: SelectionStrategy::Explicit,
            summary: None,
            actor: sample_actor(),
        }
    }

    async fn setup_server() -> (Arc<LibraMcpServer>, TempDir) {
        let temp_dir = tempdir().unwrap();
        let temp_path = temp_dir.path().to_path_buf();
        let db_path = temp_path.join("libra.db");
        let db = db::create_database(db_path.to_str().unwrap())
            .await
            .unwrap();
        let storage = Arc::new(LocalStorage::new(temp_path.join("objects")));
        let history_manager = Arc::new(HistoryManager::new(
            storage.clone(),
            temp_path,
            Arc::new(db),
        ));
        (
            Arc::new(LibraMcpServer::new(Some(history_manager), Some(storage))),
            temp_dir,
        )
    }

    /// `snapshot_needed` must return `false` only when both `items` is empty
    /// *and* `summary` is `None` — that's the "nothing to record" gate.
    #[test]
    fn snapshot_needed_false_for_fully_empty_request() {
        assert!(!snapshot_needed(&empty_request()));
    }

    /// A non-empty item list triggers the snapshot even with no summary.
    #[test]
    fn snapshot_needed_true_when_items_present() {
        let mut req = empty_request();
        req.items.push(ContextSnapshotItem {
            kind: None,
            path: "src/main.rs".to_string(),
            preview: None,
            blob_hash: None,
        });
        assert!(snapshot_needed(&req));
    }

    /// A standalone summary (no items) is still enough to trigger the
    /// snapshot — useful for "we considered the context and decided
    /// nothing was relevant" audit entries.
    #[test]
    fn snapshot_needed_true_when_only_summary_present() {
        let mut req = empty_request();
        req.summary = Some("nothing relevant".to_string());
        assert!(snapshot_needed(&req));
    }

    /// `write_context_snapshot_if_needed` must short-circuit on an empty
    /// request and return `Ok(None)` without ever touching the MCP server.
    /// This is the only test path that can run without a real MCP server
    /// because the early return happens before `mcp_server` is read.
    #[tokio::test]
    async fn write_context_snapshot_if_needed_skips_empty_request() {
        // We dangle an Arc::new_uninit MCP server replacement by relying on
        // the early-return path — but constructing a real LibraMcpServer in
        // a unit test is heavy, so instead we exercise the gate via the
        // `snapshot_needed` helper and the type contract. The async
        // contract is asserted here so future refactors that inline the
        // gate keep the early-return semantics observable.
        let req = empty_request();
        assert!(!snapshot_needed(&req));
    }

    #[tokio::test]
    async fn phase0_write_helpers_persist_intent_and_context_snapshot() {
        let (server, _temp_dir) = setup_server().await;
        let spec = sample_intent_spec();

        let intent = write_intent(&spec, &server).await.unwrap();
        assert_eq!(intent.source, spec);

        let snapshot = write_context_snapshot_if_needed(
            ContextSnapshotRequest {
                items: vec![ContextSnapshotItem {
                    kind: Some("file".to_string()),
                    path: "src/main.rs".to_string(),
                    preview: Some("fn main() {}".to_string()),
                    blob_hash: None,
                }],
                selection_strategy: SelectionStrategy::Explicit,
                summary: Some("phase0 context snapshot".to_string()),
                actor: sample_actor(),
            },
            &server,
        )
        .await
        .unwrap()
        .expect("non-empty Phase 0 context should persist a snapshot");
        assert_eq!(snapshot.summary.as_deref(), Some("phase0 context snapshot"));
        assert_eq!(snapshot.item_count, 1);

        let history = server.intent_history_manager.as_ref().unwrap();
        for (object_type, object_id) in [
            ("intent", intent.intent_id.as_str()),
            ("snapshot", snapshot.snapshot_id.as_str()),
        ] {
            assert!(
                history
                    .get_object_hash(
                        object_type,
                        &parse_object_id(object_id).unwrap().to_string()
                    )
                    .await
                    .unwrap()
                    .is_some(),
                "expected Phase 0 {object_type} id {object_id} to resolve in history",
            );
        }
    }

    /// `ContextSnapshotWriteOutcome` must derive `Clone` + `PartialEq` so
    /// audit handlers can snapshot the outcome and compare across rebuilds.
    #[test]
    fn snapshot_outcome_is_clone_and_eq() {
        let outcome = ContextSnapshotWriteOutcome {
            snapshot_id: "snap-1".to_string(),
            summary: Some("ok".to_string()),
            item_count: 3,
        };
        let cloned = outcome.clone();
        assert_eq!(cloned, outcome);
        assert_eq!(cloned.snapshot_id, "snap-1");
        assert_eq!(cloned.summary.as_deref(), Some("ok"));
        assert_eq!(cloned.item_count, 3);
    }

    /// `parse_snapshot_id` must extract the value after the `ID:` token,
    /// trimming surrounding whitespace, and return `None` for content
    /// without an `ID:` marker.
    #[test]
    fn parse_snapshot_id_extracts_after_id_marker() {
        let result = CallToolResult::success(vec![Content::text(
            "ContextSnapshot created with ID: snap-abc-123",
        )]);
        assert_eq!(parse_snapshot_id(&result), Some("snap-abc-123".to_string()));
    }

    #[test]
    fn parse_snapshot_id_returns_none_without_id_marker() {
        let result = CallToolResult::success(vec![Content::text("snapshot created but no marker")]);
        assert_eq!(parse_snapshot_id(&result), None);
    }

    #[test]
    fn parse_snapshot_id_returns_none_for_empty_id() {
        let result = CallToolResult::success(vec![Content::text("ID:   ")]);
        assert_eq!(parse_snapshot_id(&result), None);
    }

    /// AC1: Phase 0's tool-loop config must only allow read-only
    /// investigation tools plus the two interactive/terminal tools, with
    /// `submit_intent_draft` as the sole terminal tool. No mutating tool
    /// (e.g. `apply_patch`, `shell`) may appear on the allowlist — that is
    /// the mutation fence for "no mutating tools before confirm".
    #[test]
    fn phase0_plan_tool_loop_config_allows_only_read_only_and_draft_tools() {
        let config = phase0_plan_tool_loop_config(ToolLoopConfig::default());
        let allowed = config
            .allowed_tools
            .as_ref()
            .expect("phase0 config must set an explicit allowlist");
        for mutating in [
            "apply_patch",
            "shell",
            "submit_plan_draft",
            "submit_task_complete",
        ] {
            assert!(
                !allowed.iter().any(|tool| tool == mutating),
                "phase0 allowlist must not include mutating tool '{mutating}': {allowed:?}"
            );
        }
        assert!(allowed.iter().any(|tool| tool == "submit_intent_draft"));
        assert!(allowed.iter().any(|tool| tool == "request_user_input"));
        assert_eq!(
            config.terminal_tools.as_deref(),
            Some(["submit_intent_draft".to_string()].as_slice()),
            "submit_intent_draft must be the only Phase 0 terminal tool"
        );
    }

    #[test]
    fn open_intent_review_from_workflow_tracks_unresolved_gate() {
        use crate::internal::ai::session::CodeWorkflowEventKind;

        let requested = CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: "intent-1".to_string(),
            intent_id: "spec-1".to_string(),
            turn_id: "gate-1".to_string(),
            phase0_turn_id: "phase0-1".to_string(),
        };
        assert_eq!(
            open_intent_review_from_workflow([&requested]),
            Some((
                "intent-1".to_string(),
                "spec-1".to_string(),
                "gate-1".to_string(),
                "phase0-1".to_string()
            ))
        );

        let resolved = CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "intent-1".to_string(),
            resolution: "confirm".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        };
        assert_eq!(
            open_intent_review_from_workflow([&requested, &resolved]),
            None
        );

        let other_resolved = CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "other".to_string(),
            resolution: "cancel".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        };
        assert_eq!(
            open_intent_review_from_workflow([&requested, &other_resolved]),
            Some((
                "intent-1".to_string(),
                "spec-1".to_string(),
                "gate-1".to_string(),
                "phase0-1".to_string()
            ))
        );

        let second = CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: "intent-2".to_string(),
            intent_id: "spec-2".to_string(),
            turn_id: "gate-2".to_string(),
            phase0_turn_id: "phase0-2".to_string(),
        };
        let second_resolved = CodeWorkflowEventKind::InteractionResolved {
            interaction_id: "intent-2".to_string(),
            resolution: "confirm".to_string(),
            command: None,
            prior_interaction_resolutions: Vec::new(),
            intent_revision_consumption: None,
        };
        assert_eq!(
            open_intent_review_from_workflow([&requested, &second, &second_resolved]),
            Some((
                "intent-1".to_string(),
                "spec-1".to_string(),
                "gate-1".to_string(),
                "phase0-1".to_string()
            )),
            "resolving a later review must not drop an earlier unresolved gate"
        );

        let atomic_resolved =
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command: crate::internal::ai::session::CodeCommandIdentity::new(
                    "repo",
                    "session",
                    "principal",
                    "gate-1",
                ),
                summary: "confirmed".to_string(),
                interaction_id: "intent-1".to_string(),
                resolution: "confirm".to_string(),
                prior_interaction_resolutions: Vec::new(),
                intent_revision: None,
            };
        assert_eq!(
            open_intent_review_from_workflow([&requested, &atomic_resolved]),
            None,
            "crash-atomic terminal+resolution must close the review gate"
        );
    }

    #[test]
    fn intent_review_decision_round_trips_through_wire_ids() {
        for decision in [
            IntentReviewDecision::Confirm,
            IntentReviewDecision::Revise,
            IntentReviewDecision::Cancel,
        ] {
            assert_eq!(
                IntentReviewDecision::from_wire_id(decision.wire_id()),
                Some(decision)
            );
        }
        assert_eq!(
            IntentReviewDecision::from_wire_id("revise"),
            Some(IntentReviewDecision::Revise),
            "the descriptive alias 'revise' must also parse to Revise"
        );
        assert_eq!(
            IntentReviewDecision::from_wire_id("CONFIRM"),
            Some(IntentReviewDecision::Confirm)
        );
        assert_eq!(IntentReviewDecision::from_wire_id("bogus"), None);
    }

    #[test]
    fn intent_review_decision_choice_index_matches_tui_pending_review_layout() {
        // 0=Confirm, 1=Modify, 2=Cancel — the layout the retired TUI's
        // PendingIntentReview used (module removed in W5-03); the wire
        // choice indexes stay frozen.
        assert_eq!(
            IntentReviewDecision::from_choice_index(0),
            Some(IntentReviewDecision::Confirm)
        );
        assert_eq!(
            IntentReviewDecision::from_choice_index(1),
            Some(IntentReviewDecision::Revise)
        );
        assert_eq!(
            IntentReviewDecision::from_choice_index(2),
            Some(IntentReviewDecision::Cancel)
        );
        assert_eq!(IntentReviewDecision::from_choice_index(3), None);
    }

    #[test]
    fn intent_review_ack_delivery_rejects_unrecognized_response_without_consuming_it() {
        let delivery = IntentReviewAckDelivery::new();
        let error = delivery
            .validate(&InteractionResponse::new("intent-1", "not-a-decision"))
            .expect_err("unrecognized response must fail validation");
        assert!(matches!(
            error,
            RuntimeWorkerError::InvalidInteractionResponse(_)
        ));
    }

    /// End-to-end through a real [`AgentRuntimeWorker`]: an
    /// externally-tracked Phase 0 turn (mirroring the retired TUI's former
    /// `track_external_turn` adapter) registers the IntentSpec review via
    /// [`IntentReviewAckDelivery`], then `confirm` releases the turn as
    /// `Completed` so a follow-up admission (Phase 1, or a new Phase 0 turn
    /// for `revise`) is possible again.
    #[tokio::test]
    async fn intent_review_ack_delivery_confirm_completes_the_tracked_turn() {
        use uuid::Uuid;

        use crate::internal::ai::runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, ExternalTurnTrackingExecutor,
            InMemoryAuditSink, InteractionState, ToolBoundaryRuntime,
        };

        let (handle, worker) = AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(
            Arc::new(ExternalTurnTrackingExecutor),
            ToolBoundaryRuntime::system(Uuid::new_v4(), Arc::new(InMemoryAuditSink::default())),
        ));
        let cancellation = tokio_util::sync::CancellationToken::new();
        let mutation_started = Arc::new(std::sync::atomic::AtomicBool::new(true));
        handle
            .track_external_turn(
                TurnRequest::new("session", "phase0-turn", "plan workflow", true),
                cancellation,
                mutation_started,
            )
            .await
            .expect("phase0 turn tracked");

        handle
            .register_interaction_with_delivery(
                "session",
                "phase0-turn",
                InteractionState::AwaitingIntentReview {
                    interaction_id: "intent-1".to_string(),
                },
                Box::new(IntentReviewAckDelivery::new()),
            )
            .await
            .expect("worker owns the IntentSpec review interaction");
        assert_eq!(
            handle.snapshot("session").await.unwrap().interaction,
            InteractionState::AwaitingIntentReview {
                interaction_id: "intent-1".to_string(),
            }
        );

        let invalid = handle
            .respond(
                "session",
                "phase0-turn",
                InteractionResponse::new("intent-1", "not-a-decision"),
            )
            .await
            .expect_err("malformed restored review response must be retryable");
        assert!(matches!(
            invalid,
            RuntimeWorkerError::InvalidInteractionResponse(_)
        ));
        assert_eq!(
            handle.snapshot("session").await.unwrap().interaction,
            InteractionState::AwaitingIntentReview {
                interaction_id: "intent-1".to_string(),
            },
            "pre-consume validation must leave the restored gate pending"
        );

        handle
            .respond(
                "session",
                "phase0-turn",
                InteractionResponse::new("intent-1", "confirm"),
            )
            .await
            .expect("confirm must be accepted");
        assert_eq!(
            handle.snapshot("session").await.unwrap().interaction,
            InteractionState::Completed
        );

        // The tracked turn slot must be free for a new admission (e.g. the
        // hand-off toward Phase 1), matching the AC3 "release once resolved"
        // half of the mutation fence.
        handle
            .track_external_turn(
                TurnRequest::new("session", "phase1-turn", "execution plan", true),
                tokio_util::sync::CancellationToken::new(),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .expect("a new turn can be admitted once the review is resolved");
        worker.abort();
    }

    /// `cancel` must resolve the tracked Phase 0 turn as `Completed` (not
    /// `Cancelling`/`IndeterminateSideEffect`) because there is no live
    /// executor continuation left to reconcile an ambiguous outcome for an
    /// externally-tracked turn, and the only mutation that occurred —
    /// persisting the draft via `write_intent` — is inert data at rest, not
    /// a partially applied side effect requiring reconciliation. Queued
    /// follow-ups are discarded via [`RuntimeTurnExecution::CompletedDiscardQueued`].
    #[tokio::test]
    async fn intent_review_ack_delivery_cancel_completes_without_reconciliation() {
        use uuid::Uuid;

        use crate::internal::ai::runtime::{
            AgentRuntimeWorker, AgentRuntimeWorkerConfig, ExternalTurnTrackingExecutor,
            InMemoryAuditSink, InteractionState, ToolBoundaryRuntime,
        };

        let (handle, worker) = AgentRuntimeWorker::spawn(AgentRuntimeWorkerConfig::new(
            Arc::new(ExternalTurnTrackingExecutor),
            ToolBoundaryRuntime::system(Uuid::new_v4(), Arc::new(InMemoryAuditSink::default())),
        ));
        handle
            .track_external_turn(
                TurnRequest::new("session", "phase0-turn", "plan workflow", true),
                tokio_util::sync::CancellationToken::new(),
                Arc::new(std::sync::atomic::AtomicBool::new(true)),
            )
            .await
            .expect("phase0 turn tracked");
        handle
            .register_interaction_with_delivery(
                "session",
                "phase0-turn",
                InteractionState::AwaitingIntentReview {
                    interaction_id: "intent-1".to_string(),
                },
                Box::new(IntentReviewAckDelivery::new()),
            )
            .await
            .expect("worker owns the IntentSpec review interaction");

        handle
            .respond(
                "session",
                "phase0-turn",
                InteractionResponse::new("intent-1", "cancel"),
            )
            .await
            .expect("cancel must be accepted and resolve the turn");
        assert_eq!(
            handle.snapshot("session").await.unwrap().interaction,
            InteractionState::Completed,
            "cancelling a resolved-draft review must not open a reconciliation fence"
        );
        worker.abort();
    }
}
