//! Append-only JSONL session event storage.

#[cfg(any(test, feature = "test-provider"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::state::SessionState;
use crate::internal::ai::{
    agent_run::{AgentRunEvent, AgentRunEventEnvelope, AgentRunId},
    context_budget::{CompactionEvent, ContextFrameEvent, MemoryAnchorEvent, MemoryAnchorReplay},
    goal::GoalEventEnvelope,
    runtime::event::Event,
};

pub const SESSION_EVENTS_FILE: &str = "events.jsonl";

/// Maximum UTF-8 size of the user-authored note retained in the local
/// IntentSpec revision sidecar. Raw notes must never be copied into the Code
/// workflow stream because those events are also projected over SSE.
pub const MAX_INTENT_REVISION_NOTE_BYTES: usize = 16 * 1024;

/// Non-sensitive binding committed atomically with an IntentSpec `modify`
/// terminal.
///
/// This payload is not workflow authority: the enclosing primary interaction
/// resolution remains the sole gate-closing event. The HMAC commits to a
/// private session-local sidecar containing the raw note and IntentSpec. The
/// HMAC key and raw content never enter this workflow terminal/receipt payload
/// or the SSE v2 consumption payload; the ordinary transcript/session snapshot
/// keeps its existing user-content persistence boundary. This is a
/// durable-lineage check, not a defense against same-user local tampering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntentRevisionRecovery {
    pub interaction_id: String,
    pub sidecar_digest: String,
}

pub const INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION: u32 = 1;
pub const INTENT_REVISION_CONSUMER_COMMAND_KIND: &str = "headless_direct_turn";
pub const INTENT_REVISION_CANCEL_COMMAND_INPUT: &str = "/intent cancel";
pub(crate) const INTENT_REVISION_CONSUMER_RECOVERY_FAILURE_REASON: &str = "IntentSpec revision consumer stopped before its durable consumption receipt; the revision remains available for retry";
pub(crate) const INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_EFFECT: &str = "mutating_runtime_turn";
pub(crate) const INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_REASON: &str = "IntentSpec revision consumption stopped before its durable receipt; the revision remains available for retry";
pub(crate) const INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON: &str = "IntentSpec revision produced a durable replacement review; startup must restore that review before accepting another turn";
pub(crate) const PRE_MUTATION_CANCELLED_COMMAND_REASON: &str =
    "runtime turn cancelled before a mutating side effect began";
const RECOVERED_MUTATING_COMMAND_EFFECT: &str = "unknown_mutating_dispatch";
const RECOVERED_MUTATING_COMMAND_REASON: &str =
    "runtime stopped after durable intent; manual reconciliation is required";

/// Exact lineage record that a later durable command consumes one durably
/// bound IntentSpec revision sidecar. Claiming/Consuming persists the full
/// attribution before provider execution. The receipt is committed only once
/// a canonical no-provider Cancel effect or a durable replacement-review
/// marker proves the effect, and always before the sidecar is unlinked. Raw
/// revision text is never copied into JSONL/SSE.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntentRevisionConsumptionClaim {
    pub schema_version: u32,
    pub interaction_id: String,
    pub source_command: CodeCommandIdentity,
    pub consumer_intent: CodeCommandIntent,
    pub terminal_event_id: Uuid,
    pub terminal_sequence: u64,
    pub intent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntentRevisionConsumption {
    pub claim: IntentRevisionConsumptionClaim,
    pub consumer_intent_event_id: Uuid,
    pub consumer_intent_sequence: u64,
}

pub(crate) fn is_canonical_intent_revision_digest(value: &str) -> bool {
    value.len() == "hmac-sha256:".len() + 64
        && value.starts_with("hmac-sha256:")
        && value["hmac-sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn exact_intent_revision_lineage_intent_id<'a>(
    events: &'a [CodeWorkflowEvent],
    interaction_id: &str,
    identity: &CodeCommandIdentity,
) -> Result<Option<&'a str>, ()> {
    let mut marker_count = 0usize;
    let mut turn_match_count = 0usize;
    let mut phase0_match_count = 0usize;
    let mut lineage_intent_id = None::<&str>;
    let mut lineage_phase0_turn_id = None::<&str>;
    let mut latest_turn_id = None::<&str>;
    let mut conflicting_intent_id = false;
    let mut conflicting_phase0_turn_id = false;
    for event in events {
        let CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: candidate_interaction_id,
            intent_id,
            turn_id,
            phase0_turn_id,
        } = &event.event
        else {
            continue;
        };
        if interaction_id != candidate_interaction_id {
            continue;
        }
        marker_count += 1;
        match lineage_intent_id {
            Some(expected) if expected != intent_id => conflicting_intent_id = true,
            None => lineage_intent_id = Some(intent_id),
            Some(_) => {}
        }
        match lineage_phase0_turn_id {
            Some(expected) if expected != phase0_turn_id => conflicting_phase0_turn_id = true,
            None => lineage_phase0_turn_id = Some(phase0_turn_id),
            Some(_) => {}
        }
        latest_turn_id = Some(turn_id);
        turn_match_count += usize::from(turn_id == &identity.command_id);
        phase0_match_count += usize::from(phase0_turn_id == &identity.command_id);
    }
    // A restored review can durably append replacement turn bindings for the
    // same interaction. All generations must retain one IntentSpec id, while
    // only the latest replacement (or the sole original Phase 0 owner) may
    // terminalize it.
    if marker_count == 0 {
        return Ok(None);
    }
    let exact_current_owner = if marker_count == 1 {
        turn_match_count + phase0_match_count == 1
    } else {
        latest_turn_id == Some(identity.command_id.as_str())
            && turn_match_count == 1
            && phase0_match_count == 0
    };
    if conflicting_intent_id || conflicting_phase0_turn_id || !exact_current_owner {
        return Err(());
    }
    Ok(lineage_intent_id)
}

/// Retryable IntentSpec review authority published atomically with a failed
/// Phase 1 command terminal. Older readers ignore this additive payload while
/// still observing the enclosing failure, so they fail closed instead of
/// admitting a duplicate provider attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Phase1RetryIntentReview {
    pub interaction_id: String,
    pub intent_id: String,
    pub intent_spec_id: String,
    pub source_interaction_id: String,
    pub source_resolution: String,
    pub source_phase1_turn_id: String,
    pub start_seed_digest: String,
}

thread_local! {
    static CODE_WORKFLOW_REPLAY_PARSE_VISITS: Cell<usize> = const { Cell::new(0) };
    #[cfg(test)]
    static INTENT_REVISION_REPLAY_INDEX_VISITS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn reset_intent_revision_replay_index_visits() {
    INTENT_REVISION_REPLAY_INDEX_VISITS.with(|cell| cell.set(0));
}

#[cfg(test)]
fn intent_revision_replay_index_visits() -> usize {
    INTENT_REVISION_REPLAY_INDEX_VISITS.with(Cell::get)
}

#[inline]
fn record_intent_revision_replay_index_visits(visits: usize) {
    #[cfg(test)]
    INTENT_REVISION_REPLAY_INDEX_VISITS.with(|cell| cell.set(cell.get().saturating_add(visits)));
    #[cfg(not(test))]
    let _ = visits;
}

/// Reset the per-thread JSONL parse-visit counter used by bounded workflow
/// replay (W3-14 access evidence).
pub fn reset_code_workflow_replay_parse_visits() {
    CODE_WORKFLOW_REPLAY_PARSE_VISITS.with(|cell| cell.set(0));
}

/// Number of JSONL records parsed by [`SessionJsonlStore::load_code_workflow_replay_since`]
/// since the last [`reset_code_workflow_replay_parse_visits`] on this thread.
pub fn code_workflow_replay_parse_visits() -> usize {
    CODE_WORKFLOW_REPLAY_PARSE_VISITS.with(Cell::get)
}
const CODE_WORKFLOW_APPEND_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const CODE_WORKFLOW_APPEND_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(not(unix))]
const STALE_PROCESS_OWNER_RECORD_AGE: Duration = Duration::from_secs(30);

/// Event persisted in a session JSONL stream.
///
/// The wire form follows the runtime `Event` envelope contract:
/// `{"kind":"session_snapshot","payload":{...}}`. Readers inspect the
/// envelope before deserializing so future event kinds can be skipped without
/// breaking older binaries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
// Keep persisted event payloads inline: boxing a public variant would change
// the Rust API solely to optimize a cold JSONL serialization boundary.
#[allow(clippy::large_enum_variant)]
pub enum SessionEvent {
    SessionSnapshot(SessionSnapshotEvent),
    ContextFrame(ContextFrameEvent),
    CompactionEvent(CompactionEvent),
    MemoryAnchor(MemoryAnchorEvent),
    /// OC-Phase 3 sub-agent lifecycle event. These do not mutate the
    /// legacy `SessionState`; they are replayed by agent-run specific
    /// projections and skipped by older binaries through the unknown
    /// event branch.
    AgentRun(AgentRunEventEnvelope),
    /// Dedicated child tool-call transcript event. The child session
    /// stream also carries `SessionSnapshot` rows for legacy resume,
    /// but this event keeps tool arguments queryable without parsing
    /// snapshot message strings.
    ToolCall(SessionToolCallEvent),
    /// Dedicated child tool-result transcript event. Mirrors
    /// [`Self::ToolCall`] and does not mutate legacy `SessionState`.
    ToolResult(SessionToolResultEvent),
    /// OC-Phase 6 Goal mode envelope. Goal supervisor wiring emits these
    /// alongside normal session events; older binaries still skip unknown
    /// `goal_event` payloads via the `parse_session_event_value` `unknown`
    /// branch.
    Goal(GoalEventEnvelope),
    /// OC-Phase 4 ArtifactLedger JSONL projection. The
    /// `ValidationReportStore::write_latest_with_session_mirror` and
    /// `DecisionProposalStore::write_latest_with_session_mirror` paths
    /// persist artefacts to `ai_validation_report` /
    /// `ai_decision_proposal` / `ai_risk_score_breakdown` SQLite
    /// tables; this variant projects the same write into the
    /// session JSONL stream so a single tail of the session log
    /// gives an operator the artefact lifecycle without an
    /// SQLite join.
    ///
    /// Forward-compat: older binaries that don't know this kind
    /// skip the row via the `parse_session_event_value` unknown
    /// branch. New schema additions ride additively under
    /// `payload.payload: serde_json::Value` so a future kind
    /// extension does not break older readers.
    AiArtifact(AiArtifactEvent),
    /// Code runtime workflow event. This is deliberately one additive top-
    /// level kind: binaries released before W1-03 see `code_workflow` as an
    /// unknown event and safely skip the whole row. The typed payload holds
    /// the cursor/deduplication identity needed by later resume and SSE
    /// projections without changing the wire shape of legacy session rows.
    CodeWorkflow(CodeWorkflowEvent),
}

/// A cursor into one session's Code workflow stream.
///
/// The session id scopes `sequence`; consumers must never compare sequence
/// values from different sessions. Runtime adapters use this cursor for SSE
/// and resume, while W1-06 will add the projection that consumes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeWorkflowCursor {
    pub session_id: String,
    pub sequence: u64,
}

impl CodeWorkflowCursor {
    pub fn new(session_id: impl Into<String>, sequence: u64) -> Self {
        Self {
            session_id: session_id.into(),
            sequence,
        }
    }
}

/// One additive Code workflow event in a session JSONL stream.
///
/// `event_id` is the de-duplication identity and `sequence` is the ordered
/// per-session cursor. UUID ordering is explicitly not an event-ordering
/// mechanism. This card serializes allocation plus append; W1-05 owns the
/// fsync/mutation replay contract that must surround it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeWorkflowEvent {
    pub event_id: Uuid,
    pub sequence: u64,
    pub recorded_at: DateTime<Utc>,
    #[serde(flatten)]
    pub event: CodeWorkflowEventKind,
}

impl CodeWorkflowEvent {
    pub fn new(sequence: u64, event: CodeWorkflowEventKind) -> Self {
        Self {
            event_id: Uuid::new_v4(),
            sequence,
            recorded_at: Utc::now(),
            event,
        }
    }
}

/// Code-specific payload variants frozen by W1-03.
///
/// All strings are identifiers or redacted summaries. Raw user prompts,
/// approval responses, tool arguments and environment values belong to their
/// existing redacted stores and must not be copied into this event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
// This public persisted vocabulary intentionally owns each payload inline;
// boxing one variant would be an avoidable source-level compatibility break.
#[allow(clippy::large_enum_variant)]
pub enum CodeWorkflowEventKind {
    CommandAccepted {
        command_id: String,
        workflow: String,
    },
    IntentReviewRequested {
        interaction_id: String,
        intent_id: String,
        /// Non-mutating runtime turn that owns the parked review gate.
        /// Empty on rows written before W2-02 turn-id recovery; resume then
        /// allocates a replacement turn and must terminalize any orphan.
        #[serde(default)]
        turn_id: String,
        /// Mutating Phase 0 turn that wrote the IntentSpec draft. Startup
        /// recovery may complete this identity as success when the marker is
        /// open; other pending mutations stay fenced/cancelled.
        #[serde(default)]
        phase0_turn_id: String,
    },
    PlanReviewRequested {
        interaction_id: String,
        plan_id: String,
        /// Non-mutating runtime turn that owns the parked plan-review gate.
        /// Empty on rows written before W2-03 turn-id recovery; resume then
        /// allocates a replacement turn and must terminalize any orphan.
        #[serde(default)]
        turn_id: String,
        /// Mutating Phase 1 turn that wrote the plan draft. Startup recovery
        /// may complete this identity as success when the marker is open;
        /// other pending mutations stay fenced/cancelled.
        #[serde(default)]
        phase1_turn_id: String,
        /// Immutable context sidecar id. Empty legacy rows use interaction_id.
        #[serde(default)]
        context_id: String,
        /// When present, this same durable row atomically consumes the prior
        /// Modify decision and opens its replacement gate. Older readers
        /// ignore this additive field but still recognize the replacement.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision_of: Option<String>,
        /// Network gate whose durable `back` resolution activates this Plan
        /// marker. Before that resolution the marker is provisional and must
        /// not replace/demote the current Network authority.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prepared_from_network: Option<String>,
    },
    /// Durable boundary immediately before the formal Phase 1 plan write.
    /// Pending mutating commands with this marker but no PlanReviewRequested
    /// must be fenced on recovery; only exact seed-backed Pending commands
    /// without this marker are eligible for pre-write reattachment.
    Phase1FormalWriteStarted {
        phase1_turn_id: String,
        source_interaction_id: String,
        seed_digest: String,
    },
    /// Post-plan network-policy gate marker (W2-03 recovery).
    ///
    /// Written **before** the Plan review resolves so a crash in the window
    /// between "plan approved" and "network policy answered" still leaves a
    /// durable record of the required human decision — the resolved plan
    /// marker alone would make plan-review recovery a no-op and silently drop
    /// the gate.
    NetworkPolicyRequested {
        interaction_id: String,
        plan_id: String,
        /// Non-mutating runtime turn that owns the parked network-policy gate.
        /// Empty on rows written before durable gate-turn recovery; resume
        /// then allocates a replacement turn and records it in a fresh marker.
        #[serde(default)]
        turn_id: String,
        /// Default selection hint (IntentSpec network allow already true).
        #[serde(default)]
        default_allow: bool,
    },
    /// Plan-execution repair gate marker (W2-11 recovery).
    ///
    /// Written before the runtime parks Continue/Cancel so a process restart
    /// cannot discard the human repair decision and re-admit plan execution.
    PlanExecutionRepairRequested {
        interaction_id: String,
        /// Non-mutating runtime turn that owns the parked repair gate.
        #[serde(default)]
        turn_id: String,
        /// The unresolved repair marker this speculative continuation replaces.
        ///
        /// Empty for an initial repair gate and markers written before
        /// continuation lineage was recorded. Recovery uses this relationship
        /// to retire a pre-ack continuation after restoring its predecessor.
        #[serde(default)]
        predecessor_interaction_id: String,
        /// Whether this marker is a replacement that makes its predecessor
        /// obsolete, rather than a speculative pre-ack continuation.
        #[serde(default)]
        supersedes_predecessor: bool,
        repair: crate::internal::ai::runtime::PlanExecutionRepairState,
    },
    InteractionResolved {
        interaction_id: String,
        resolution: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<CodeCommandIdentity>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        prior_interaction_resolutions: Vec<(String, String)>,
        /// Durable consume receipt for the active IntentSpec revision. The
        /// full Claiming/Consuming attribution precedes provider work; this row
        /// follows either a canonical no-provider Cancel effect or a durable
        /// replacement-review marker and always precedes sidecar unlink.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent_revision_consumption: Option<IntentRevisionConsumption>,
    },
    CodeUiProjectionDelta {
        projection: String,
        summary: String,
        /// Typed projection payload.  W1-06 consumers deserialize this only
        /// for projection names they understand; older rows that predate this
        /// additive field decode as `Null`, and future projection names remain
        /// skippable without changing the top-level session-event envelope.
        #[serde(default)]
        payload: serde_json::Value,
    },
    TerminalSuccess {
        command_id: String,
        summary: String,
    },
    TerminalFailure {
        command_id: String,
        reason: String,
    },
    IndeterminateSideEffect {
        command_id: String,
        effect: String,
        reason: String,
    },
    /// Recovery-critical intent. This is written and fsynced by W1-05 before
    /// the runtime dispatches any mutating command.
    CommandIntentPersisted {
        command: CodeCommandIntent,
    },
    CommandTerminalSuccess {
        command: CodeCommandIdentity,
        summary: String,
    },
    /// Crash-atomic terminal success + interaction resolution for review gates
    /// (W2-02). A single JSONL row so a torn write cannot leave the command
    /// succeeded while the review marker stays open.
    CommandTerminalSuccessWithInteractionResolved {
        command: CodeCommandIdentity,
        summary: String,
        interaction_id: String,
        resolution: String,
        /// Earlier interactions delivered by the same command. The last/current
        /// gate remains in the legacy primary fields so older readers still
        /// close the workflow authority; newer readers retain the full audit
        /// without a prefix-visible multi-row batch.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        prior_interaction_resolutions: Vec<(String, String)>,
        /// Bounded recovery data for an IntentSpec Modify response. This is
        /// valid only for the primary canonical `modify` resolution and does
        /// not itself open, close, or replace a workflow gate.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent_revision: Option<IntentRevisionRecovery>,
    },
    CommandTerminalFailure {
        command: CodeCommandIdentity,
        reason: String,
        /// Interaction responses that were durably delivered before the turn
        /// later terminated as cancelled/failed. Keeping them on the same
        /// terminal row avoids both losing audit evidence and introducing a
        /// prefix-visible multi-row crash window. Older readers ignore this
        /// additive field and still recover the terminal failure.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        interaction_resolutions: Vec<(String, String)>,
        /// A pre-formal-write Phase 1 failure may atomically re-arm the
        /// IntentSpec review. Keeping the authority in this terminal row
        /// prevents a crash between a Failed fsync and a later gate marker.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_intent_review: Option<Phase1RetryIntentReview>,
    },
    CommandIndeterminateSideEffect {
        command: CodeCommandIdentity,
        effect: String,
        reason: String,
    },
}

/// Stable runtime-command de-duplication key. The session id remains in the
/// key even though the file path is session-scoped so a copied or misrouted
/// JSONL row cannot silently match a command from another session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CodeCommandIdentity {
    pub repo_id: String,
    pub session_id: String,
    pub principal_id: String,
    pub command_id: String,
}

impl CodeCommandIdentity {
    pub fn new(
        repo_id: impl Into<String>,
        session_id: impl Into<String>,
        principal_id: impl Into<String>,
        command_id: impl Into<String>,
    ) -> Self {
        Self {
            repo_id: repo_id.into(),
            session_id: session_id.into(),
            principal_id: principal_id.into(),
            command_id: command_id.into(),
        }
    }

    fn is_complete(&self) -> bool {
        !self.repo_id.is_empty()
            && !self.session_id.is_empty()
            && !self.principal_id.is_empty()
            && !self.command_id.is_empty()
    }
}

/// Canonical request metadata persisted before runtime execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeCommandIntent {
    pub identity: CodeCommandIdentity,
    pub command_kind: String,
    pub canonical_request_hash: String,
    pub mutating: bool,
}

impl CodeCommandIntent {
    pub fn new(
        identity: CodeCommandIdentity,
        command_kind: impl Into<String>,
        canonical_request_hash: impl Into<String>,
        mutating: bool,
    ) -> Self {
        Self {
            identity,
            command_kind: command_kind.into(),
            canonical_request_hash: canonical_request_hash.into(),
            mutating,
        }
    }

    fn is_valid(&self) -> bool {
        self.identity.is_complete()
            && !self.command_kind.is_empty()
            && !self.canonical_request_hash.is_empty()
    }
}

pub(crate) fn intent_revision_cancel_consumer_is_canonical(intent: &CodeCommandIntent) -> bool {
    use sha2::Digest as _;

    intent.is_valid()
        && intent.command_kind == INTENT_REVISION_CONSUMER_COMMAND_KIND
        && intent.mutating
        && intent.canonical_request_hash
            == format!(
                "sha256:{}",
                hex::encode(sha2::Sha256::digest(
                    INTENT_REVISION_CANCEL_COMMAND_INPUT.as_bytes()
                ))
            )
}

fn intent_revision_consumption_claims_overlap(
    left: &IntentRevisionConsumptionClaim,
    right: &IntentRevisionConsumptionClaim,
) -> bool {
    left.terminal_event_id == right.terminal_event_id
        || left.terminal_sequence == right.terminal_sequence
        || left.interaction_id == right.interaction_id
        || left.source_command == right.source_command
        || left.consumer_intent.identity == right.consumer_intent.identity
}

pub(crate) fn intent_revision_consumption_claim_is_valid(
    consumption: &IntentRevisionConsumptionClaim,
) -> bool {
    consumption.schema_version == INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION
        && !consumption.interaction_id.trim().is_empty()
        && consumption.source_command.is_complete()
        && consumption.consumer_intent.is_valid()
        && consumption.consumer_intent.mutating
        && consumption.consumer_intent.command_kind == INTENT_REVISION_CONSUMER_COMMAND_KIND
        && consumption.terminal_sequence > 0
        && !consumption.intent_id.trim().is_empty()
        && consumption
            .sidecar_digest
            .as_deref()
            .is_some_and(is_canonical_intent_revision_digest)
        && consumption.source_command.repo_id == consumption.consumer_intent.identity.repo_id
        && consumption.source_command.session_id == consumption.consumer_intent.identity.session_id
        && consumption.source_command.principal_id
            == consumption.consumer_intent.identity.principal_id
        && consumption.source_command.command_id != consumption.consumer_intent.identity.command_id
}

fn exact_intent_revision_consumption_terminal_index(
    replay: &CodeWorkflowReplay,
    consumption: &IntentRevisionConsumptionClaim,
) -> Option<usize> {
    if !intent_revision_replay_is_complete(replay)
        || !intent_revision_consumption_claim_is_valid(consumption)
    {
        return None;
    }
    let mut terminal_index = None;
    let mut source_terminal_count = 0usize;
    for (event_index, event) in replay.events.iter().enumerate() {
        let terminal_command = match &event.event {
            CodeWorkflowEventKind::CommandTerminalSuccess { command, .. }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalFailure { command, .. }
            | CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. } => {
                Some(command)
            }
            _ => None,
        };
        if terminal_command == Some(&consumption.source_command) {
            source_terminal_count = source_terminal_count.saturating_add(1);
        }
        if event.event_id != consumption.terminal_event_id
            && event.sequence != consumption.terminal_sequence
        {
            continue;
        }
        if event.event_id != consumption.terminal_event_id
            || event.sequence != consumption.terminal_sequence
        {
            return None;
        }
        let CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            command,
            interaction_id,
            resolution,
            intent_revision,
            ..
        } = &event.event
        else {
            return None;
        };
        if command != &consumption.source_command
            || interaction_id != &consumption.interaction_id
            || resolution != "modify"
            || terminal_index.replace(event_index).is_some()
        {
            return None;
        }
        match (intent_revision, consumption.sidecar_digest.as_deref()) {
            (Some(recovery), Some(digest))
                if recovery.interaction_id == consumption.interaction_id
                    && recovery.sidecar_digest == digest => {}
            (None, Some(digest)) if is_canonical_intent_revision_digest(digest) => {}
            _ => return None,
        }
    }
    let terminal_index = terminal_index?;
    if source_terminal_count != 1 {
        return None;
    }
    if exact_intent_revision_lineage_intent_id(
        &replay.events[..terminal_index],
        &consumption.interaction_id,
        &consumption.source_command,
    ) != Ok(Some(consumption.intent_id.as_str()))
        || !has_exact_intent_revision_source_intent(
            replay,
            terminal_index,
            &consumption.source_command,
            &consumption.interaction_id,
        )
    {
        return None;
    }
    Some(terminal_index)
}

pub(crate) fn intent_revision_replay_is_complete(replay: &CodeWorkflowReplay) -> bool {
    replay.gaps.is_empty() && !replay.window_cut_mid_record
}

pub(crate) fn has_exact_intent_revision_source_intent(
    replay: &CodeWorkflowReplay,
    terminal_index: usize,
    identity: &CodeCommandIdentity,
    interaction_id: &str,
) -> bool {
    if !intent_revision_replay_is_complete(replay) || terminal_index > replay.events.len() {
        return false;
    }
    let mut exact_count = 0usize;
    let mut source_mutating = None;
    for (event_index, event) in replay.events.iter().enumerate() {
        let CodeWorkflowEventKind::CommandIntentPersisted { command } = &event.event else {
            continue;
        };
        if command.identity != *identity {
            continue;
        }
        if event_index >= terminal_index
            || !command.is_valid()
            || command.command_kind != INTENT_REVISION_CONSUMER_COMMAND_KIND
        {
            return false;
        }
        exact_count += 1;
        source_mutating = Some(command.mutating);
    }
    if exact_count != 1 {
        return false;
    }

    let Some(source_mutating) = source_mutating else {
        return false;
    };
    let mut marker_count = 0usize;
    let mut exact_turn_count = 0usize;
    let mut exact_phase0_count = 0usize;
    let mut latest_turn_id = None::<&str>;
    for event in &replay.events[..terminal_index] {
        let CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: candidate,
            turn_id,
            phase0_turn_id,
            ..
        } = &event.event
        else {
            continue;
        };
        if candidate != interaction_id {
            continue;
        }
        marker_count += 1;
        latest_turn_id = Some(turn_id);
        exact_turn_count += usize::from(turn_id == &identity.command_id);
        exact_phase0_count += usize::from(phase0_turn_id == &identity.command_id);
    }
    if source_mutating {
        marker_count == 1 && exact_phase0_count == 1 && exact_turn_count == 0
    } else {
        marker_count >= 1
            && latest_turn_id == Some(identity.command_id.as_str())
            && exact_turn_count == 1
            && exact_phase0_count == 0
    }
}

/// Current durable state for one runtime command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeCommandStatus {
    Pending,
    Succeeded { summary: String },
    Failed { reason: String },
    Indeterminate { effect: String, reason: String },
}

pub(crate) fn claimed_intent_revision_consumer_status(
    replay: &CodeWorkflowReplay,
    consumption: &IntentRevisionConsumption,
) -> Result<CodeCommandStatus, CodeCommandStoreError> {
    validated_intent_revision_consumption_receipts(replay)?
        .claimed_intent_revision_consumer_status(consumption)
}

fn intent_revision_consumption_from_claim(
    replay: &CodeWorkflowReplay,
    consumer_intent: &CodeCommandIntent,
    claim: &IntentRevisionConsumptionClaim,
) -> Result<IntentRevisionConsumption, CodeCommandStoreError> {
    let Some(terminal_index) = exact_intent_revision_consumption_terminal_index(replay, claim)
    else {
        return Err(CodeCommandStoreError::InvalidIntent);
    };
    for event in &replay.events {
        if let CodeWorkflowEventKind::InteractionResolved {
            intent_revision_consumption: Some(existing),
            ..
        } = &event.event
            && intent_revision_consumption_claims_overlap(&existing.claim, claim)
        {
            // A source revision and an ordinary command are each first-writer
            // identities. Neither may be rebound even when a conflicting
            // receipt precedes this source terminal.
            return Err(CodeCommandStoreError::InvalidIntent);
        }
    }
    let mut consumer_event = None;
    for (event_index, event) in replay.events.iter().enumerate() {
        if let CodeWorkflowEventKind::CommandIntentPersisted { command } = &event.event
            && command.identity == consumer_intent.identity
            && (event_index <= terminal_index
                || command != consumer_intent
                || consumer_event.replace(event).is_some())
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
    }
    let consumer_event = consumer_event.ok_or(CodeCommandStoreError::InvalidIntent)?;
    for event in &replay.events {
        if let CodeWorkflowEventKind::InteractionResolved {
            intent_revision_consumption: Some(existing),
            ..
        } = &event.event
            && (existing.consumer_intent_event_id == consumer_event.event_id
                || existing.consumer_intent_sequence == consumer_event.sequence)
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
    }
    Ok(IntentRevisionConsumption {
        claim: claim.clone(),
        consumer_intent_event_id: consumer_event.event_id,
        consumer_intent_sequence: consumer_event.sequence,
    })
}

fn exact_intent_revision_consumption_receipt_indices(
    replay: &CodeWorkflowReplay,
    consumption: &IntentRevisionConsumption,
) -> Result<(usize, Option<usize>), CodeCommandStoreError> {
    let claim = &consumption.claim;
    let Some(terminal_index) = exact_intent_revision_consumption_terminal_index(replay, claim)
    else {
        return Err(CodeCommandStoreError::InvalidIntent);
    };
    let mut exact_consumer_index = None;
    let mut exact_receipt_index = None;
    let mut consumer_terminal_index = None;
    for (event_index, event) in replay.events.iter().enumerate() {
        match &event.event {
            CodeWorkflowEventKind::CommandIntentPersisted { command } => {
                if event.event_id == consumption.consumer_intent_event_id
                    || event.sequence == consumption.consumer_intent_sequence
                    || command.identity == claim.consumer_intent.identity
                {
                    if event.event_id != consumption.consumer_intent_event_id
                        || event.sequence != consumption.consumer_intent_sequence
                        || command != &claim.consumer_intent
                        || event_index <= terminal_index
                    {
                        return Err(CodeCommandStoreError::InvalidIntent);
                    }
                    if exact_consumer_index.replace(event_index).is_some() {
                        return Err(CodeCommandStoreError::InvalidIntent);
                    }
                }
            }
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                command,
                prior_interaction_resolutions,
                intent_revision_consumption: Some(existing),
            } if intent_revision_consumption_claims_overlap(&existing.claim, claim)
                || existing.consumer_intent_event_id == consumption.consumer_intent_event_id
                || existing.consumer_intent_sequence == consumption.consumer_intent_sequence =>
            {
                if interaction_id != &claim.interaction_id
                    || resolution != "modify"
                    || command.is_some()
                    || !prior_interaction_resolutions.is_empty()
                    || existing != consumption
                {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                if exact_receipt_index.replace(event_index).is_some() {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
            }
            CodeWorkflowEventKind::CommandTerminalSuccess { command, .. }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalFailure { command, .. }
            | CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
                if command == &claim.consumer_intent.identity
                    && consumer_terminal_index.replace(event_index).is_some() =>
            {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
            _ => {}
        }
    }
    let exact_consumer_index = exact_consumer_index.ok_or(CodeCommandStoreError::InvalidIntent)?;
    if exact_receipt_index.is_some_and(|receipt_index| receipt_index <= exact_consumer_index) {
        return Err(CodeCommandStoreError::InvalidIntent);
    }
    if let (Some(receipt_index), Some(terminal_index)) =
        (exact_receipt_index, consumer_terminal_index)
    {
        if terminal_index <= exact_consumer_index {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        if receipt_index >= terminal_index {
            let recoverable_replacement_terminal = matches!(
                &replay.events[terminal_index].event,
                CodeWorkflowEventKind::CommandIndeterminateSideEffect {
                    command,
                    effect,
                    reason,
                } if command == &claim.consumer_intent.identity
                    && effect == INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_EFFECT
                    && reason == INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON
            )
                && intent_revision_consumer_replacement_review_after_index(
                    replay,
                    consumption,
                    exact_consumer_index,
                )?
                .is_some_and(|proof| proof.first_marker_index < terminal_index);
            if !recoverable_replacement_terminal {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
        }
    }
    Ok((exact_consumer_index, exact_receipt_index))
}

/// Apply the authoritative source/consumer/receipt ordering validation used
/// by both JSONL recovery and the web sidecar reconciler. Keeping this in one
/// place is important because the replacement-marker ACK-loss exception
/// permits one narrowly proven receipt to follow its Indeterminate terminal.
#[cfg(test)]
pub(crate) fn validate_intent_revision_consumption_receipt(
    replay: &CodeWorkflowReplay,
    consumption: &IntentRevisionConsumption,
) -> Result<(), CodeCommandStoreError> {
    let validated = validated_intent_revision_consumption_receipts(replay)?;
    if validated.receipts().all(|receipt| {
        receipt.consumption != consumption
            || validated
                .exact_receipt_for_consumption(consumption)
                .is_none()
    }) {
        return Err(CodeCommandStoreError::InvalidIntent);
    }
    Ok(())
}

/// Prove that a revision consumer durably produced one still-open replacement
/// IntentSpec review. The marker is written only after the replacement
/// IntentSpec itself is durable, so recovery may finish the old revision
/// receipt without replaying the provider. Repeated restore markers for the
/// same interaction are allowed; a conflicting interaction or a resolved gate
/// is not.
pub(crate) fn intent_revision_consumer_has_open_replacement_review(
    replay: &CodeWorkflowReplay,
    consumption: &IntentRevisionConsumption,
) -> Result<bool, CodeCommandStoreError> {
    if !intent_revision_consumer_has_replacement_review(replay, consumption)? {
        return Ok(false);
    }
    let (consumer_index, _) =
        exact_intent_revision_consumption_receipt_indices(replay, consumption)?;
    Ok(intent_revision_consumer_replacement_review_after_index(
        replay,
        consumption,
        consumer_index,
    )?
    .is_some_and(|proof| !proof.resolved))
}

/// Prove that a revision consumer produced an exact replacement review even
/// after that review is resolved. This permanent effect proof keeps a
/// recovered post-terminal receipt authoritative on every later startup;
/// callers that need to restore a gate must use the open-only helper above.
pub(crate) fn intent_revision_consumer_has_replacement_review(
    replay: &CodeWorkflowReplay,
    consumption: &IntentRevisionConsumption,
) -> Result<bool, CodeCommandStoreError> {
    let (consumer_index, receipt_index) =
        exact_intent_revision_consumption_receipt_indices(replay, consumption)?;
    let Some(proof) = intent_revision_consumer_replacement_review_after_index(
        replay,
        consumption,
        consumer_index,
    )?
    else {
        return Ok(false);
    };
    if let Some(receipt_index) = receipt_index {
        return Ok(proof.first_marker_index < receipt_index);
    }

    let mut consumer_terminal = None;
    for (event_index, event) in replay.events.iter().enumerate() {
        let identity = &consumption.claim.consumer_intent.identity;
        let is_consumer_terminal = matches!(
            &event.event,
            CodeWorkflowEventKind::CommandTerminalSuccess { command, .. }
                | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    command,
                    ..
                }
                | CodeWorkflowEventKind::CommandTerminalFailure { command, .. }
                | CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. }
                    if command == identity
        );
        if is_consumer_terminal && consumer_terminal.replace(event_index).is_some() {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
    }
    let Some(terminal_index) = consumer_terminal else {
        return Ok(true);
    };
    Ok(proof.first_marker_index < terminal_index
        && matches!(
            &replay.events[terminal_index].event,
            CodeWorkflowEventKind::CommandIndeterminateSideEffect {
                command,
                effect,
                reason,
            } if command == &consumption.claim.consumer_intent.identity
                && effect == INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_EFFECT
                && reason == INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON
        ))
}

fn intent_revision_consumer_has_any_replacement_review(
    replay: &CodeWorkflowReplay,
    consumption: &IntentRevisionConsumption,
) -> Result<bool, CodeCommandStoreError> {
    let (consumer_index, _) =
        exact_intent_revision_consumption_receipt_indices(replay, consumption)?;
    Ok(intent_revision_consumer_replacement_review_after_index(
        replay,
        consumption,
        consumer_index,
    )?
    .is_some())
}

#[derive(Debug, Clone, Copy)]
struct IntentRevisionReplacementReviewProof {
    first_marker_index: usize,
    resolved: bool,
}

fn intent_revision_consumer_replacement_review_after_index(
    replay: &CodeWorkflowReplay,
    consumption: &IntentRevisionConsumption,
    consumer_index: usize,
) -> Result<Option<IntentRevisionReplacementReviewProof>, CodeCommandStoreError> {
    let command_id = &consumption.claim.consumer_intent.identity.command_id;
    let mut replacement = None::<(String, String)>;
    let mut first_marker_index = None;
    let mut resolved = false;
    for (event_index, event) in replay.events.iter().enumerate() {
        match &event.event {
            CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id,
                intent_id,
                turn_id,
                phase0_turn_id,
            } if phase0_turn_id == command_id => {
                if event_index <= consumer_index
                    || interaction_id.is_empty()
                    || intent_id.is_empty()
                    || turn_id.is_empty()
                {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                let candidate = (interaction_id.clone(), intent_id.clone());
                if replacement
                    .as_ref()
                    .is_some_and(|existing| existing != &candidate)
                {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                replacement = Some(candidate);
                first_marker_index.get_or_insert(event_index);
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
            } if replacement.as_ref().is_some_and(|(candidate, _)| {
                candidate == interaction_id
                    || prior_interaction_resolutions
                        .iter()
                        .any(|(resolved_id, _)| resolved_id == candidate)
            }) =>
            {
                resolved = true;
            }
            _ => {}
        }
    }
    Ok(
        first_marker_index.map(|first_marker_index| IntentRevisionReplacementReviewProof {
            first_marker_index,
            resolved,
        }),
    )
}

fn intent_revision_consumption_receipt_event(
    consumption: &IntentRevisionConsumption,
) -> CodeWorkflowEventKind {
    CodeWorkflowEventKind::InteractionResolved {
        interaction_id: consumption.claim.interaction_id.clone(),
        resolution: "modify".to_string(),
        command: None,
        prior_interaction_resolutions: Vec::new(),
        intent_revision_consumption: Some(consumption.clone()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IntentRevisionCommandScope {
    repo_id: String,
    session_id: String,
    principal_id: String,
}

impl From<&CodeCommandIdentity> for IntentRevisionCommandScope {
    fn from(identity: &CodeCommandIdentity) -> Self {
        Self {
            repo_id: identity.repo_id.clone(),
            session_id: identity.session_id.clone(),
            principal_id: identity.principal_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct IndexedIntentRevisionIntent<'a> {
    event_index: usize,
    event_id: Uuid,
    sequence: u64,
    command: &'a CodeCommandIntent,
}

#[derive(Debug, Clone, Copy)]
struct IndexedIntentRevisionTerminal<'a> {
    event_index: usize,
    event: &'a CodeWorkflowEventKind,
}

#[derive(Debug, Clone, Copy)]
struct IndexedIntentRevisionMarker<'a> {
    event_index: usize,
    interaction_id: &'a str,
    intent_id: &'a str,
    turn_id: &'a str,
    phase0_turn_id: &'a str,
}

#[derive(Debug, Clone, Copy)]
struct IndexedIntentRevisionReceipt<'a> {
    event_index: usize,
    consumption: &'a IntentRevisionConsumption,
}

#[derive(Debug, Clone, Copy)]
struct IndexedIntentRevisionSourceTerminal<'a> {
    event_index: usize,
    lineage_start: usize,
    event_id: Uuid,
    sequence: u64,
    interaction_id: &'a str,
    command: &'a CodeCommandIdentity,
    intent_id: &'a str,
    sidecar_digest: Option<&'a str>,
}

#[derive(Debug, Default)]
struct IndexedIntentRevisionAttemptScope<'a> {
    attempts: Vec<IndexedIntentRevisionIntent<'a>>,
    positions_by_event_index: HashMap<usize, usize>,
    current_statuses: Vec<Option<CodeCommandStatus>>,
    prior_invalid_prefix: Vec<usize>,
    last_invalid_intent_position: Option<usize>,
}

impl IndexedIntentRevisionAttemptScope<'_> {
    fn finalize(
        &mut self,
        terminals: &HashMap<CodeCommandIdentity, IndexedIntentRevisionTerminal<'_>>,
    ) {
        self.positions_by_event_index.reserve(self.attempts.len());
        self.current_statuses.reserve(self.attempts.len());
        self.prior_invalid_prefix = Vec::with_capacity(self.attempts.len().saturating_add(1));
        self.prior_invalid_prefix.push(0);
        for (position, attempt) in self.attempts.iter().enumerate() {
            record_intent_revision_replay_index_visits(1);
            self.positions_by_event_index
                .insert(attempt.event_index, position);
            if !attempt.command.is_valid() {
                self.last_invalid_intent_position = Some(position);
            }
            let next_intent_index = self
                .attempts
                .get(position.saturating_add(1))
                .map_or(usize::MAX, |next| next.event_index);
            let terminal = terminals.get(&attempt.command.identity).copied();
            let current_status =
                indexed_intent_revision_attempt_status(attempt, next_intent_index, terminal, true);
            let prior_status =
                indexed_intent_revision_attempt_status(attempt, next_intent_index, terminal, false);
            let prior_invalid = !attempt.command.is_valid()
                || prior_status
                    .as_ref()
                    .is_none_or(|status| matches!(status, CodeCommandStatus::Pending));
            let prior_invalid_count = self
                .prior_invalid_prefix
                .last()
                .copied()
                .unwrap_or_default()
                .saturating_add(usize::from(prior_invalid));
            self.prior_invalid_prefix.push(prior_invalid_count);
            self.current_statuses.push(current_status);
        }
    }

    fn validate_committed_lineage(
        &self,
        lineage_start: usize,
        consumer_intent_index: usize,
        receipt_index: usize,
    ) -> Result<(usize, usize, CodeCommandStatus), CodeCommandStoreError> {
        let current_position = self
            .positions_by_event_index
            .get(&consumer_intent_index)
            .copied()
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        if current_position < lineage_start
            || receipt_index <= consumer_intent_index
            || self
                .attempts
                .get(current_position.saturating_add(1))
                .is_some_and(|next| next.event_index < receipt_index)
            || self
                .last_invalid_intent_position
                .is_some_and(|position| position >= lineage_start)
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let invalid_prior_attempts = self.prior_invalid_prefix[current_position]
            .saturating_sub(self.prior_invalid_prefix[lineage_start]);
        if invalid_prior_attempts != 0 {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let status = self
            .current_statuses
            .get(current_position)
            .and_then(Clone::clone)
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        Ok((lineage_start, current_position, status))
    }

    fn validate_uncommitted_lineage(
        &self,
        terminals: &HashMap<CodeCommandIdentity, IndexedIntentRevisionTerminal<'_>>,
        lineage_start: usize,
        consumer_intent_index: usize,
        receipt_index: Option<usize>,
        allow_current_pre_mutation_cancel: bool,
    ) -> Result<Vec<(CodeCommandIdentity, CodeCommandStatus)>, CodeCommandStoreError> {
        let current_position = self
            .positions_by_event_index
            .get(&consumer_intent_index)
            .copied()
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        if current_position < lineage_start
            || self
                .last_invalid_intent_position
                .is_some_and(|position| position >= lineage_start)
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        match receipt_index {
            Some(receipt_index) => {
                if receipt_index <= consumer_intent_index
                    || self
                        .attempts
                        .get(current_position.saturating_add(1))
                        .is_some_and(|next| next.event_index < receipt_index)
                {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
            }
            None if current_position.saturating_add(1) != self.attempts.len() => {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
            None => {}
        }

        let mut statuses = Vec::with_capacity(
            current_position
                .saturating_sub(lineage_start)
                .saturating_add(1),
        );
        for position in lineage_start..=current_position {
            record_intent_revision_replay_index_visits(1);
            let attempt = self
                .attempts
                .get(position)
                .ok_or(CodeCommandStoreError::InvalidIntent)?;
            let next_intent_index = self
                .attempts
                .get(position.saturating_add(1))
                .map_or(usize::MAX, |next| next.event_index);
            let current_attempt = position == current_position;
            let status = indexed_recoverable_intent_revision_consumer_status(
                attempt,
                next_intent_index,
                terminals.get(&attempt.command.identity).copied(),
                current_attempt,
                allow_current_pre_mutation_cancel,
                current_attempt && receipt_index.is_some(),
            )?;
            if !current_attempt && matches!(status, CodeCommandStatus::Pending) {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
            statuses.push((attempt.command.identity.clone(), status));
        }
        Ok(statuses)
    }
}

fn indexed_intent_revision_attempt_status(
    attempt: &IndexedIntentRevisionIntent<'_>,
    next_intent_index: usize,
    terminal: Option<IndexedIntentRevisionTerminal<'_>>,
    current_committed_attempt: bool,
) -> Option<CodeCommandStatus> {
    let Some(terminal) = terminal else {
        return Some(CodeCommandStatus::Pending);
    };
    if terminal.event_index <= attempt.event_index || terminal.event_index >= next_intent_index {
        return None;
    }
    match terminal.event {
        CodeWorkflowEventKind::CommandTerminalFailure {
            command,
            reason,
            interaction_resolutions,
            retry_intent_review,
        } if command == &attempt.command.identity
            && (reason == INTENT_REVISION_CONSUMER_RECOVERY_FAILURE_REASON
                || reason == PRE_MUTATION_CANCELLED_COMMAND_REASON)
            && interaction_resolutions.is_empty()
            && retry_intent_review.is_none() =>
        {
            Some(CodeCommandStatus::Failed {
                reason: reason.clone(),
            })
        }
        CodeWorkflowEventKind::CommandIndeterminateSideEffect {
            command,
            effect,
            reason,
        } if command == &attempt.command.identity
            && (current_committed_attempt
                || (effect == RECOVERED_MUTATING_COMMAND_EFFECT
                    && reason == RECOVERED_MUTATING_COMMAND_REASON)
                || (effect == INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_EFFECT
                    && (reason == INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_REASON
                        || reason == INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON))) =>
        {
            Some(CodeCommandStatus::Indeterminate {
                effect: effect.clone(),
                reason: reason.clone(),
            })
        }
        CodeWorkflowEventKind::CommandTerminalSuccess { command, summary }
            if current_committed_attempt && command == &attempt.command.identity =>
        {
            Some(CodeCommandStatus::Succeeded {
                summary: summary.clone(),
            })
        }
        CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            command,
            summary,
            ..
        } if current_committed_attempt && command == &attempt.command.identity => {
            Some(CodeCommandStatus::Succeeded {
                summary: summary.clone(),
            })
        }
        _ => None,
    }
}

fn indexed_recoverable_intent_revision_consumer_status(
    attempt: &IndexedIntentRevisionIntent<'_>,
    next_intent_index: usize,
    terminal: Option<IndexedIntentRevisionTerminal<'_>>,
    current_attempt: bool,
    allow_current_pre_mutation_cancel: bool,
    allow_committed_effect_terminal: bool,
) -> Result<CodeCommandStatus, CodeCommandStoreError> {
    let Some(terminal) = terminal else {
        return Ok(CodeCommandStatus::Pending);
    };
    if terminal.event_index <= attempt.event_index || terminal.event_index >= next_intent_index {
        return Err(CodeCommandStoreError::InvalidIntent);
    }
    let identity = &attempt.command.identity;
    match terminal.event {
        CodeWorkflowEventKind::CommandTerminalFailure {
            command,
            reason,
            interaction_resolutions,
            retry_intent_review,
        } if command == identity => {
            let accepted_reason = reason == INTENT_REVISION_CONSUMER_RECOVERY_FAILURE_REASON
                || ((!current_attempt || allow_current_pre_mutation_cancel)
                    && reason == PRE_MUTATION_CANCELLED_COMMAND_REASON);
            if !accepted_reason
                || !interaction_resolutions.is_empty()
                || retry_intent_review.is_some()
            {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
            Ok(CodeCommandStatus::Failed {
                reason: reason.clone(),
            })
        }
        CodeWorkflowEventKind::CommandIndeterminateSideEffect {
            command,
            effect,
            reason,
        } if command == identity => {
            let accepted = allow_committed_effect_terminal
                || (effect == RECOVERED_MUTATING_COMMAND_EFFECT
                    && reason == RECOVERED_MUTATING_COMMAND_REASON)
                || (effect == INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_EFFECT
                    && (reason == INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_REASON
                        || reason == INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON));
            if !accepted {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
            Ok(CodeCommandStatus::Indeterminate {
                effect: effect.clone(),
                reason: reason.clone(),
            })
        }
        CodeWorkflowEventKind::CommandTerminalSuccess { command, summary }
        | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            command,
            summary,
            ..
        } if command == identity && allow_committed_effect_terminal => {
            Ok(CodeCommandStatus::Succeeded {
                summary: summary.clone(),
            })
        }
        CodeWorkflowEventKind::CommandTerminalSuccess { command, .. }
        | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            command, ..
        } if command == identity => Err(CodeCommandStoreError::TerminalConflict {
            command_id: identity.command_id.clone(),
        }),
        _ => Err(CodeCommandStoreError::InvalidIntent),
    }
}

#[derive(Debug)]
struct IntentRevisionReplayIndex<'a> {
    event_indices_by_id: HashMap<Uuid, usize>,
    event_indices_by_sequence: HashMap<u64, usize>,
    intents_by_identity: HashMap<CodeCommandIdentity, Vec<IndexedIntentRevisionIntent<'a>>>,
    web_intent_identity_by_command_id: HashMap<&'a str, &'a CodeCommandIdentity>,
    terminals_by_identity: HashMap<CodeCommandIdentity, IndexedIntentRevisionTerminal<'a>>,
    markers_by_interaction: HashMap<&'a str, Vec<IndexedIntentRevisionMarker<'a>>>,
    markers_by_phase0_turn: HashMap<&'a str, Vec<IndexedIntentRevisionMarker<'a>>>,
    marker_lineage_by_interaction: HashMap<&'a str, (&'a str, &'a str)>,
    marker_interaction_by_phase0_turn: HashMap<&'a str, &'a str>,
    marker_interaction_by_turn: HashMap<&'a str, &'a str>,
    latest_resolution_index_by_interaction: HashMap<&'a str, usize>,
    attempt_scopes: HashMap<IntentRevisionCommandScope, IndexedIntentRevisionAttemptScope<'a>>,
    latest_web_intent_sequence_by_scope: HashMap<IntentRevisionCommandScope, u64>,
    latest_nonmutating_web_intent_index_by_scope: HashMap<IntentRevisionCommandScope, usize>,
    source_terminals: Vec<IndexedIntentRevisionSourceTerminal<'a>>,
    source_terminals_by_event_id: HashMap<Uuid, usize>,
    source_terminals_by_sequence: HashMap<u64, usize>,
    source_terminals_by_interaction: HashMap<&'a str, usize>,
    source_terminals_by_command: HashMap<CodeCommandIdentity, usize>,
    receipts: Vec<IndexedIntentRevisionReceipt<'a>>,
    receipts_by_source_event_id: HashMap<Uuid, usize>,
    receipts_by_source_sequence: HashMap<u64, usize>,
    receipts_by_source_command: HashMap<CodeCommandIdentity, usize>,
    receipts_by_source_interaction: HashMap<&'a str, usize>,
    receipts_by_consumer_event_id: HashMap<Uuid, usize>,
    receipts_by_consumer_sequence: HashMap<u64, usize>,
    receipts_by_consumer_command: HashMap<CodeCommandIdentity, usize>,
}

impl<'a> IntentRevisionReplayIndex<'a> {
    fn build(replay: &'a CodeWorkflowReplay) -> Result<Self, CodeCommandStoreError> {
        if !intent_revision_replay_is_complete(replay) {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let mut index = Self {
            event_indices_by_id: HashMap::new(),
            event_indices_by_sequence: HashMap::new(),
            intents_by_identity: HashMap::new(),
            web_intent_identity_by_command_id: HashMap::new(),
            terminals_by_identity: HashMap::new(),
            markers_by_interaction: HashMap::new(),
            markers_by_phase0_turn: HashMap::new(),
            marker_lineage_by_interaction: HashMap::new(),
            marker_interaction_by_phase0_turn: HashMap::new(),
            marker_interaction_by_turn: HashMap::new(),
            latest_resolution_index_by_interaction: HashMap::new(),
            attempt_scopes: HashMap::new(),
            latest_web_intent_sequence_by_scope: HashMap::new(),
            latest_nonmutating_web_intent_index_by_scope: HashMap::new(),
            source_terminals: Vec::new(),
            source_terminals_by_event_id: HashMap::new(),
            source_terminals_by_sequence: HashMap::new(),
            source_terminals_by_interaction: HashMap::new(),
            source_terminals_by_command: HashMap::new(),
            receipts: Vec::new(),
            receipts_by_source_event_id: HashMap::new(),
            receipts_by_source_sequence: HashMap::new(),
            receipts_by_source_command: HashMap::new(),
            receipts_by_source_interaction: HashMap::new(),
            receipts_by_consumer_event_id: HashMap::new(),
            receipts_by_consumer_sequence: HashMap::new(),
            receipts_by_consumer_command: HashMap::new(),
        };
        for (event_index, event) in replay.events.iter().enumerate() {
            record_intent_revision_replay_index_visits(1);
            if index
                .event_indices_by_id
                .insert(event.event_id, event_index)
                .is_some()
                || index
                    .event_indices_by_sequence
                    .insert(event.sequence, event_index)
                    .is_some()
            {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
            match &event.event {
                CodeWorkflowEventKind::CommandIntentPersisted { command } => {
                    let indexed_intent = IndexedIntentRevisionIntent {
                        event_index,
                        event_id: event.event_id,
                        sequence: event.sequence,
                        command,
                    };
                    let identity_intents = index
                        .intents_by_identity
                        .entry(command.identity.clone())
                        .or_default();
                    if command.command_kind == INTENT_REVISION_CONSUMER_COMMAND_KIND
                        && !identity_intents.is_empty()
                    {
                        return Err(CodeCommandStoreError::InvalidIntent);
                    }
                    identity_intents.push(indexed_intent);
                    if command.command_kind == INTENT_REVISION_CONSUMER_COMMAND_KIND {
                        if !command.is_valid() {
                            return Err(CodeCommandStoreError::InvalidIntent);
                        }
                        if index
                            .web_intent_identity_by_command_id
                            .insert(&command.identity.command_id, &command.identity)
                            .is_some()
                        {
                            return Err(CodeCommandStoreError::InvalidIntent);
                        }
                        let scope = IntentRevisionCommandScope::from(&command.identity);
                        index
                            .latest_web_intent_sequence_by_scope
                            .entry(scope.clone())
                            .and_modify(|latest| *latest = (*latest).max(event.sequence))
                            .or_insert(event.sequence);
                        if command.mutating {
                            index
                                .attempt_scopes
                                .entry(scope)
                                .or_default()
                                .attempts
                                .push(indexed_intent);
                        } else {
                            index
                                .latest_nonmutating_web_intent_index_by_scope
                                .entry(scope)
                                .and_modify(|latest| *latest = (*latest).max(event_index))
                                .or_insert(event_index);
                        }
                    }
                }
                CodeWorkflowEventKind::IntentReviewRequested {
                    interaction_id,
                    intent_id,
                    turn_id,
                    phase0_turn_id,
                } => {
                    let lineage = (intent_id.as_str(), phase0_turn_id.as_str());
                    if index
                        .marker_lineage_by_interaction
                        .get(interaction_id.as_str())
                        .is_some_and(|existing| *existing != lineage)
                        || (!phase0_turn_id.is_empty()
                            && index
                                .marker_interaction_by_phase0_turn
                                .get(phase0_turn_id.as_str())
                                .is_some_and(|existing| *existing != interaction_id))
                        || (!turn_id.is_empty()
                            && index
                                .marker_interaction_by_turn
                                .insert(turn_id, interaction_id)
                                .is_some())
                    {
                        return Err(CodeCommandStoreError::InvalidIntent);
                    }
                    index
                        .marker_lineage_by_interaction
                        .entry(interaction_id)
                        .or_insert(lineage);
                    if !phase0_turn_id.is_empty() {
                        index
                            .marker_interaction_by_phase0_turn
                            .entry(phase0_turn_id)
                            .or_insert(interaction_id);
                    }
                    let marker = IndexedIntentRevisionMarker {
                        event_index,
                        interaction_id,
                        intent_id,
                        turn_id,
                        phase0_turn_id,
                    };
                    index
                        .markers_by_interaction
                        .entry(interaction_id)
                        .or_default()
                        .push(marker);
                    index
                        .markers_by_phase0_turn
                        .entry(phase0_turn_id)
                        .or_default()
                        .push(marker);
                }
                CodeWorkflowEventKind::InteractionResolved {
                    interaction_id,
                    prior_interaction_resolutions,
                    intent_revision_consumption,
                    ..
                } => {
                    if intent_revision_consumption.is_none() {
                        index
                            .latest_resolution_index_by_interaction
                            .entry(interaction_id)
                            .and_modify(|latest| *latest = (*latest).max(event_index))
                            .or_insert(event_index);
                        for (resolved_id, _) in prior_interaction_resolutions {
                            record_intent_revision_replay_index_visits(1);
                            index
                                .latest_resolution_index_by_interaction
                                .entry(resolved_id)
                                .and_modify(|latest| *latest = (*latest).max(event_index))
                                .or_insert(event_index);
                        }
                    }
                    if let Some(consumption) = intent_revision_consumption {
                        index.insert_receipt(
                            event_index,
                            interaction_id,
                            &event.event,
                            consumption,
                        )?;
                    }
                }
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    command,
                    interaction_id,
                    prior_interaction_resolutions,
                    ..
                } => {
                    index.insert_terminal(event_index, command, &event.event)?;
                    index
                        .latest_resolution_index_by_interaction
                        .entry(interaction_id)
                        .and_modify(|latest| *latest = (*latest).max(event_index))
                        .or_insert(event_index);
                    for (resolved_id, _) in prior_interaction_resolutions {
                        record_intent_revision_replay_index_visits(1);
                        index
                            .latest_resolution_index_by_interaction
                            .entry(resolved_id)
                            .and_modify(|latest| *latest = (*latest).max(event_index))
                            .or_insert(event_index);
                    }
                }
                CodeWorkflowEventKind::CommandTerminalSuccess { command, .. }
                | CodeWorkflowEventKind::CommandTerminalFailure { command, .. }
                | CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. } => {
                    index.insert_terminal(event_index, command, &event.event)?;
                }
                _ => {}
            }
        }
        let terminals = &index.terminals_by_identity;
        for scope in index.attempt_scopes.values_mut() {
            scope.finalize(terminals);
        }
        index.build_source_terminals(replay)?;
        Ok(index)
    }

    fn insert_terminal(
        &mut self,
        event_index: usize,
        command: &CodeCommandIdentity,
        event: &'a CodeWorkflowEventKind,
    ) -> Result<(), CodeCommandStoreError> {
        if self
            .terminals_by_identity
            .insert(
                command.clone(),
                IndexedIntentRevisionTerminal { event_index, event },
            )
            .is_some()
        {
            return Err(CodeCommandStoreError::TerminalConflict {
                command_id: command.command_id.clone(),
            });
        }
        Ok(())
    }

    fn insert_receipt(
        &mut self,
        event_index: usize,
        interaction_id: &'a str,
        event: &'a CodeWorkflowEventKind,
        consumption: &'a IntentRevisionConsumption,
    ) -> Result<(), CodeCommandStoreError> {
        let CodeWorkflowEventKind::InteractionResolved {
            resolution,
            command,
            prior_interaction_resolutions,
            ..
        } = event
        else {
            return Err(CodeCommandStoreError::InvalidIntent);
        };
        let claim = &consumption.claim;
        if interaction_id != claim.interaction_id
            || resolution != "modify"
            || command.is_some()
            || !prior_interaction_resolutions.is_empty()
            || !intent_revision_consumption_claim_is_valid(claim)
            || consumption.consumer_intent_sequence == 0
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let receipt_index = self.receipts.len();
        if self
            .receipts_by_source_event_id
            .insert(claim.terminal_event_id, receipt_index)
            .is_some()
            || self
                .receipts_by_source_sequence
                .insert(claim.terminal_sequence, receipt_index)
                .is_some()
            || self
                .receipts_by_source_command
                .insert(claim.source_command.clone(), receipt_index)
                .is_some()
            || self
                .receipts_by_source_interaction
                .insert(&claim.interaction_id, receipt_index)
                .is_some()
            || self
                .receipts_by_consumer_event_id
                .insert(consumption.consumer_intent_event_id, receipt_index)
                .is_some()
            || self
                .receipts_by_consumer_sequence
                .insert(consumption.consumer_intent_sequence, receipt_index)
                .is_some()
            || self
                .receipts_by_consumer_command
                .insert(claim.consumer_intent.identity.clone(), receipt_index)
                .is_some()
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        self.receipts.push(IndexedIntentRevisionReceipt {
            event_index,
            consumption,
        });
        Ok(())
    }

    fn exact_source_lineage(
        &self,
        terminal_index: usize,
        command: &CodeCommandIdentity,
        interaction_id: &str,
    ) -> Result<&'a str, CodeCommandStoreError> {
        let source_intents = self
            .intents_by_identity
            .get(command)
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        let [source_intent] = source_intents.as_slice() else {
            return Err(CodeCommandStoreError::InvalidIntent);
        };
        record_intent_revision_replay_index_visits(source_intents.len());
        if source_intent.event_index >= terminal_index
            || !source_intent.command.is_valid()
            || source_intent.command.command_kind != INTENT_REVISION_CONSUMER_COMMAND_KIND
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let markers = self
            .markers_by_interaction
            .get(interaction_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut marker_count = 0usize;
        let mut turn_match_count = 0usize;
        let mut phase0_match_count = 0usize;
        let mut lineage_intent_id = None::<&str>;
        let mut lineage_phase0_turn_id = None::<&str>;
        let mut latest_turn_id = None::<&str>;
        for marker in markers
            .iter()
            .filter(|marker| marker.event_index < terminal_index)
        {
            record_intent_revision_replay_index_visits(1);
            marker_count = marker_count.saturating_add(1);
            match lineage_intent_id {
                Some(expected) if expected != marker.intent_id => {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                None => lineage_intent_id = Some(marker.intent_id),
                Some(_) => {}
            }
            match lineage_phase0_turn_id {
                Some(expected) if expected != marker.phase0_turn_id => {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                None => lineage_phase0_turn_id = Some(marker.phase0_turn_id),
                Some(_) => {}
            }
            latest_turn_id = Some(marker.turn_id);
            turn_match_count =
                turn_match_count.saturating_add(usize::from(marker.turn_id == command.command_id));
            phase0_match_count = phase0_match_count
                .saturating_add(usize::from(marker.phase0_turn_id == command.command_id));
        }
        let exact_current_owner = if marker_count == 1 {
            turn_match_count.saturating_add(phase0_match_count) == 1
        } else {
            marker_count > 1
                && latest_turn_id == Some(command.command_id.as_str())
                && turn_match_count == 1
                && phase0_match_count == 0
        };
        let exact_source_shape = if source_intent.command.mutating {
            marker_count == 1 && phase0_match_count == 1 && turn_match_count == 0
        } else {
            marker_count >= 1
                && latest_turn_id == Some(command.command_id.as_str())
                && turn_match_count == 1
                && phase0_match_count == 0
        };
        if !exact_current_owner || !exact_source_shape {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        lineage_intent_id.ok_or(CodeCommandStoreError::InvalidIntent)
    }

    fn build_source_terminals(
        &mut self,
        replay: &'a CodeWorkflowReplay,
    ) -> Result<(), CodeCommandStoreError> {
        let mut attempt_cursor_by_scope = HashMap::<IntentRevisionCommandScope, usize>::new();
        for (event_index, event) in replay.events.iter().enumerate() {
            record_intent_revision_replay_index_visits(1);
            let CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command,
                interaction_id,
                resolution,
                intent_revision,
                ..
            } = &event.event
            else {
                continue;
            };
            if resolution != "modify" {
                continue;
            }
            if self
                .terminals_by_identity
                .get(command)
                .is_none_or(|terminal| terminal.event_index != event_index)
            {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
            let intent_id = self.exact_source_lineage(event_index, command, interaction_id)?;
            let sidecar_digest = match intent_revision {
                Some(recovery)
                    if recovery.interaction_id == *interaction_id
                        && is_canonical_intent_revision_digest(&recovery.sidecar_digest) =>
                {
                    Some(recovery.sidecar_digest.as_str())
                }
                Some(_) => return Err(CodeCommandStoreError::InvalidIntent),
                None => None,
            };
            let source_index = self.source_terminals.len();
            let scope_key = IntentRevisionCommandScope::from(command);
            let lineage_start = if let Some(scope) = self.attempt_scopes.get(&scope_key) {
                let cursor = attempt_cursor_by_scope.entry(scope_key).or_default();
                while scope
                    .attempts
                    .get(*cursor)
                    .is_some_and(|attempt| attempt.event_index <= event_index)
                {
                    record_intent_revision_replay_index_visits(1);
                    *cursor = cursor.saturating_add(1);
                }
                *cursor
            } else {
                0
            };
            if self
                .source_terminals_by_event_id
                .insert(event.event_id, source_index)
                .is_some()
                || self
                    .source_terminals_by_sequence
                    .insert(event.sequence, source_index)
                    .is_some()
                || self
                    .source_terminals_by_interaction
                    .insert(interaction_id, source_index)
                    .is_some()
                || self
                    .source_terminals_by_command
                    .insert(command.clone(), source_index)
                    .is_some()
            {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
            self.source_terminals
                .push(IndexedIntentRevisionSourceTerminal {
                    event_index,
                    lineage_start,
                    event_id: event.event_id,
                    sequence: event.sequence,
                    interaction_id,
                    command,
                    intent_id,
                    sidecar_digest,
                });
        }
        Ok(())
    }

    fn exact_source_terminal(
        &self,
        consumption: &IntentRevisionConsumption,
    ) -> Result<IndexedIntentRevisionSourceTerminal<'a>, CodeCommandStoreError> {
        self.exact_source_terminal_for_claim(&consumption.claim)
    }

    fn exact_source_terminal_for_claim(
        &self,
        claim: &IntentRevisionConsumptionClaim,
    ) -> Result<IndexedIntentRevisionSourceTerminal<'a>, CodeCommandStoreError> {
        let by_event_id = self
            .source_terminals_by_event_id
            .get(&claim.terminal_event_id)
            .copied()
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        let by_sequence = self
            .source_terminals_by_sequence
            .get(&claim.terminal_sequence)
            .copied()
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        if by_event_id != by_sequence {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let source = self
            .source_terminals
            .get(by_event_id)
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        if source.interaction_id != claim.interaction_id
            || source.command != &claim.source_command
            || source.intent_id != claim.intent_id
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        match (source.sidecar_digest, claim.sidecar_digest.as_deref()) {
            (Some(expected), Some(actual)) if expected == actual => {}
            (None, Some(actual)) if is_canonical_intent_revision_digest(actual) => {}
            _ => return Err(CodeCommandStoreError::InvalidIntent),
        }
        Ok(*source)
    }

    fn exact_receipt_event_index(
        &self,
        consumption: &IntentRevisionConsumption,
    ) -> Result<Option<usize>, CodeCommandStoreError> {
        let claim = &consumption.claim;
        let candidates = [
            self.receipts_by_source_event_id
                .get(&claim.terminal_event_id)
                .copied(),
            self.receipts_by_source_sequence
                .get(&claim.terminal_sequence)
                .copied(),
            self.receipts_by_source_command
                .get(&claim.source_command)
                .copied(),
            self.receipts_by_source_interaction
                .get(claim.interaction_id.as_str())
                .copied(),
            self.receipts_by_consumer_event_id
                .get(&consumption.consumer_intent_event_id)
                .copied(),
            self.receipts_by_consumer_sequence
                .get(&consumption.consumer_intent_sequence)
                .copied(),
            self.receipts_by_consumer_command
                .get(&claim.consumer_intent.identity)
                .copied(),
        ];
        let mut exact_position = None;
        for candidate in candidates.into_iter().flatten() {
            if exact_position
                .replace(candidate)
                .is_some_and(|existing| existing != candidate)
            {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
        }
        let Some(exact_position) = exact_position else {
            return Ok(None);
        };
        let indexed = self
            .receipts
            .get(exact_position)
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        if indexed.consumption != consumption {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        Ok(Some(indexed.event_index))
    }

    fn exact_consumer_intent_index(
        &self,
        consumption: &IntentRevisionConsumption,
        source_terminal_index: usize,
    ) -> Result<usize, CodeCommandStoreError> {
        let event_id_index = self
            .event_indices_by_id
            .get(&consumption.consumer_intent_event_id)
            .copied()
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        let sequence_index = self
            .event_indices_by_sequence
            .get(&consumption.consumer_intent_sequence)
            .copied()
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        if event_id_index != sequence_index || event_id_index <= source_terminal_index {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let consumer_intents = self
            .intents_by_identity
            .get(&consumption.claim.consumer_intent.identity)
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        let [consumer_intent] = consumer_intents.as_slice() else {
            return Err(CodeCommandStoreError::InvalidIntent);
        };
        record_intent_revision_replay_index_visits(consumer_intents.len());
        if consumer_intent.event_index != event_id_index
            || consumer_intent.event_id != consumption.consumer_intent_event_id
            || consumer_intent.sequence != consumption.consumer_intent_sequence
            || consumer_intent.command != &consumption.claim.consumer_intent
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        Ok(event_id_index)
    }

    fn replacement_review_proof(
        &self,
        consumption: &IntentRevisionConsumption,
        consumer_intent_index: usize,
    ) -> Result<Option<IntentRevisionReplacementReviewProof>, CodeCommandStoreError> {
        let markers = self
            .markers_by_phase0_turn
            .get(
                consumption
                    .claim
                    .consumer_intent
                    .identity
                    .command_id
                    .as_str(),
            )
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut replacement = None::<(&str, &str)>;
        let mut first_marker_index = None;
        for marker in markers {
            record_intent_revision_replay_index_visits(1);
            if marker.event_index <= consumer_intent_index
                || marker.interaction_id.is_empty()
                || marker.intent_id.is_empty()
                || marker.turn_id.is_empty()
            {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
            let candidate = (marker.interaction_id, marker.intent_id);
            if replacement.is_some_and(|existing| existing != candidate) {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
            replacement = Some(candidate);
            first_marker_index.get_or_insert(marker.event_index);
        }
        let Some((interaction_id, _)) = replacement else {
            return Ok(None);
        };
        let first_marker_index = first_marker_index.ok_or(CodeCommandStoreError::InvalidIntent)?;
        let resolved = self
            .latest_resolution_index_by_interaction
            .get(interaction_id)
            .is_some_and(|latest| *latest > first_marker_index);
        Ok(Some(IntentRevisionReplacementReviewProof {
            first_marker_index,
            resolved,
        }))
    }

    fn consumer_terminal(
        &self,
        consumption: &IntentRevisionConsumption,
    ) -> Option<IndexedIntentRevisionTerminal<'a>> {
        self.terminals_by_identity
            .get(&consumption.claim.consumer_intent.identity)
            .copied()
    }

    fn intent_revision_consumer_attempt_statuses(
        &self,
        consumption: &IntentRevisionConsumption,
        allow_current_pre_mutation_cancel: bool,
        allow_exact_consumption_receipt: bool,
    ) -> Result<Vec<(CodeCommandIdentity, CodeCommandStatus)>, CodeCommandStoreError> {
        if !intent_revision_consumption_claim_is_valid(&consumption.claim)
            || consumption.consumer_intent_sequence == 0
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let source = self.exact_source_terminal(consumption)?;
        let consumer_intent_index =
            self.exact_consumer_intent_index(consumption, source.event_index)?;
        let receipt_index = self.exact_receipt_event_index(consumption)?;
        if receipt_index.is_some() != allow_exact_consumption_receipt {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let scope_key = IntentRevisionCommandScope::from(&consumption.claim.source_command);
        let scope = self
            .attempt_scopes
            .get(&scope_key)
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        scope.validate_uncommitted_lineage(
            &self.terminals_by_identity,
            source.lineage_start,
            consumer_intent_index,
            receipt_index,
            allow_current_pre_mutation_cancel,
        )
    }

    fn latest_recoverable_intent_revision_attempt_before_claim(
        &self,
        claim: &IntentRevisionConsumptionClaim,
    ) -> Result<Option<IntentRevisionConsumption>, CodeCommandStoreError> {
        if !intent_revision_consumption_claim_is_valid(claim) {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let source = self.exact_source_terminal_for_claim(claim)?;
        let scope_key = IntentRevisionCommandScope::from(&claim.source_command);
        if self
            .latest_nonmutating_web_intent_index_by_scope
            .get(&scope_key)
            .is_some_and(|event_index| *event_index > source.event_index)
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let Some(scope) = self.attempt_scopes.get(&scope_key) else {
            return Ok(None);
        };
        if self
            .intents_by_identity
            .contains_key(&claim.consumer_intent.identity)
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let later_attempts = scope
            .attempts
            .get(source.lineage_start..)
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        let Some(latest) = later_attempts.last() else {
            return Ok(None);
        };
        let mut prior_claim = claim.clone();
        prior_claim.consumer_intent = latest.command.clone();
        let consumption = IntentRevisionConsumption {
            claim: prior_claim,
            consumer_intent_event_id: latest.event_id,
            consumer_intent_sequence: latest.sequence,
        };
        let statuses = self.intent_revision_consumer_attempt_statuses(&consumption, true, false)?;
        if statuses.iter().any(|(_, status)| {
            record_intent_revision_replay_index_visits(1);
            !matches!(
                status,
                CodeCommandStatus::Failed { .. } | CodeCommandStatus::Indeterminate { .. }
            )
        }) {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        Ok(Some(consumption))
    }
}

/// One Modify terminal whose command, interaction, review lineage and optional
/// HMAC commitment were validated against the shared replay index.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct ValidatedIntentRevisionSourceTerminal<'a> {
    pub(crate) interaction_id: &'a str,
    pub(crate) command: &'a CodeCommandIdentity,
    pub(crate) terminal_event_id: Uuid,
    pub(crate) terminal_sequence: u64,
    pub(crate) intent_id: &'a str,
    pub(crate) sidecar_digest: Option<&'a str>,
    pub(crate) legacy_terminal: bool,
    pub(crate) later_web_intent: bool,
}

/// One receipt whose source, consumer, ordering, uniqueness and effect proof
/// were validated against a shared replay index.
#[derive(Debug)]
// The sibling Web startup reconciler consumes this complete projection through
// the batch API; individual fields are intentionally available before that
// caller is migrated away from its legacy per-terminal scans.
#[allow(dead_code)]
pub(crate) struct ValidatedIntentRevisionReceipt<'a> {
    pub(crate) consumption: &'a IntentRevisionConsumption,
    pub(crate) source_terminal_index: usize,
    pub(crate) consumer_intent_index: usize,
    pub(crate) receipt_index: usize,
    pub(crate) consumer_status: Option<CodeCommandStatus>,
    pub(crate) canonical_cancel: bool,
    pub(crate) replacement_review: bool,
    pub(crate) replacement_review_open: bool,
    pub(crate) later_web_intent: bool,
}

/// Batch authority projection for all durable IntentSpec revision receipts.
///
/// Construct this once per replay and reuse its exact source/consumer lookups
/// in startup callers. Building and validating the projection is linear in
/// events plus indexed marker/attempt relationships; it never rescans the
/// complete event log per receipt or per retry attempt.
#[derive(Debug)]
pub(crate) struct ValidatedIntentRevisionReceiptIndex<'a> {
    replay_index: IntentRevisionReplayIndex<'a>,
    source_terminals: Vec<ValidatedIntentRevisionSourceTerminal<'a>>,
    receipts: Vec<ValidatedIntentRevisionReceipt<'a>>,
    committed_consumer_statuses: HashMap<CodeCommandIdentity, CodeCommandStatus>,
}

impl<'a> ValidatedIntentRevisionReceiptIndex<'a> {
    #[allow(dead_code)]
    pub(crate) fn source_terminals(
        &self,
    ) -> impl ExactSizeIterator<Item = &ValidatedIntentRevisionSourceTerminal<'a>> {
        self.source_terminals.iter()
    }

    #[allow(dead_code)]
    pub(crate) fn source_terminal_for_interaction(
        &self,
        interaction_id: &str,
    ) -> Option<&ValidatedIntentRevisionSourceTerminal<'a>> {
        self.replay_index
            .source_terminals_by_interaction
            .get(interaction_id)
            .and_then(|index| self.source_terminals.get(*index))
    }

    pub(crate) fn exact_source_terminal(
        &self,
        terminal_event_id: Uuid,
        terminal_sequence: u64,
        interaction_id: &str,
        source_command: &CodeCommandIdentity,
    ) -> Option<&ValidatedIntentRevisionSourceTerminal<'a>> {
        let by_event_id = self
            .replay_index
            .source_terminals_by_event_id
            .get(&terminal_event_id)
            .copied()?;
        let by_sequence = self
            .replay_index
            .source_terminals_by_sequence
            .get(&terminal_sequence)
            .copied()?;
        if by_event_id != by_sequence {
            return None;
        }
        self.source_terminals.get(by_event_id).filter(|terminal| {
            terminal.interaction_id == interaction_id && terminal.command == source_command
        })
    }

    pub(crate) fn receipts(
        &self,
    ) -> impl ExactSizeIterator<Item = &ValidatedIntentRevisionReceipt<'a>> {
        self.receipts.iter()
    }

    pub(crate) fn exact_receipt_for_source(
        &self,
        terminal_event_id: Uuid,
        terminal_sequence: u64,
        interaction_id: &str,
        source_command: &CodeCommandIdentity,
    ) -> Option<&ValidatedIntentRevisionReceipt<'a>> {
        self.exact_source_terminal(
            terminal_event_id,
            terminal_sequence,
            interaction_id,
            source_command,
        )?;
        let by_event_id = self
            .replay_index
            .receipts_by_source_event_id
            .get(&terminal_event_id)
            .copied()?;
        let by_sequence = self
            .replay_index
            .receipts_by_source_sequence
            .get(&terminal_sequence)
            .copied()?;
        if by_event_id != by_sequence {
            return None;
        }
        self.receipts.get(by_event_id).filter(|receipt| {
            receipt.consumption.claim.interaction_id == interaction_id
                && receipt.consumption.claim.source_command == *source_command
        })
    }

    #[cfg(test)]
    pub(crate) fn exact_receipt_for_consumption(
        &self,
        consumption: &IntentRevisionConsumption,
    ) -> Option<&ValidatedIntentRevisionReceipt<'a>> {
        self.exact_receipt_for_source(
            consumption.claim.terminal_event_id,
            consumption.claim.terminal_sequence,
            &consumption.claim.interaction_id,
            &consumption.claim.source_command,
        )
        .filter(|receipt| receipt.consumption == consumption)
    }

    #[allow(dead_code)]
    pub(crate) fn committed_consumer_status(
        &self,
        identity: &CodeCommandIdentity,
    ) -> Option<&CodeCommandStatus> {
        self.committed_consumer_statuses.get(identity)
    }

    pub(crate) fn claimed_intent_revision_consumer_status(
        &self,
        consumption: &IntentRevisionConsumption,
    ) -> Result<CodeCommandStatus, CodeCommandStoreError> {
        self.replay_index
            .intent_revision_consumer_attempt_statuses(consumption, true, false)?
            .pop()
            .map(|(_, status)| status)
            .ok_or(CodeCommandStoreError::InvalidIntent)
    }

    pub(crate) fn latest_recoverable_intent_revision_attempt_before_claim(
        &self,
        claim: &IntentRevisionConsumptionClaim,
    ) -> Result<Option<IntentRevisionConsumption>, CodeCommandStoreError> {
        self.replay_index
            .latest_recoverable_intent_revision_attempt_before_claim(claim)
    }

    fn intent_revision_consumer_attempt_statuses(
        &self,
        consumption: &IntentRevisionConsumption,
        allow_current_pre_mutation_cancel: bool,
        allow_exact_consumption_receipt: bool,
    ) -> Result<Vec<(CodeCommandIdentity, CodeCommandStatus)>, CodeCommandStoreError> {
        self.replay_index.intent_revision_consumer_attempt_statuses(
            consumption,
            allow_current_pre_mutation_cancel,
            allow_exact_consumption_receipt,
        )
    }
}

/// Validate all durable revision receipts with one replay index. This is the
/// shared batch API for JSONL startup recovery and the Web sidecar reconciler.
pub(crate) fn validated_intent_revision_consumption_receipts(
    replay: &CodeWorkflowReplay,
) -> Result<ValidatedIntentRevisionReceiptIndex<'_>, CodeCommandStoreError> {
    let index = IntentRevisionReplayIndex::build(replay)?;
    let source_terminals = index
        .source_terminals
        .iter()
        .map(|source| {
            record_intent_revision_replay_index_visits(1);
            let scope = IntentRevisionCommandScope::from(source.command);
            ValidatedIntentRevisionSourceTerminal {
                interaction_id: source.interaction_id,
                command: source.command,
                terminal_event_id: source.event_id,
                terminal_sequence: source.sequence,
                intent_id: source.intent_id,
                sidecar_digest: source.sidecar_digest,
                legacy_terminal: source.sidecar_digest.is_none(),
                later_web_intent: index
                    .latest_web_intent_sequence_by_scope
                    .get(&scope)
                    .is_some_and(|latest| *latest > source.sequence),
            }
        })
        .collect::<Vec<_>>();
    let mut receipts = Vec::with_capacity(index.receipts.len());
    let mut committed_intervals = HashMap::<IntentRevisionCommandScope, Vec<(usize, usize)>>::new();
    for indexed_receipt in &index.receipts {
        record_intent_revision_replay_index_visits(1);
        let consumption = indexed_receipt.consumption;
        let source_terminal = index.exact_source_terminal(consumption)?;
        let source_terminal_index = source_terminal.event_index;
        let consumer_intent_index =
            index.exact_consumer_intent_index(consumption, source_terminal_index)?;
        if indexed_receipt.event_index <= consumer_intent_index {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let replacement_proof =
            index.replacement_review_proof(consumption, consumer_intent_index)?;
        let replacement_review = replacement_proof
            .is_some_and(|proof| proof.first_marker_index < indexed_receipt.event_index);
        let canonical_cancel =
            intent_revision_cancel_consumer_is_canonical(&consumption.claim.consumer_intent);
        if canonical_cancel == replacement_review {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        if let Some(terminal) = index.consumer_terminal(consumption) {
            if terminal.event_index <= consumer_intent_index {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
            if indexed_receipt.event_index >= terminal.event_index {
                let recoverable_post_terminal_receipt = replacement_proof
                    .is_some_and(|proof| proof.first_marker_index < terminal.event_index)
                    && matches!(
                        terminal.event,
                        CodeWorkflowEventKind::CommandIndeterminateSideEffect {
                            command,
                            effect,
                            reason,
                        } if command == &consumption.claim.consumer_intent.identity
                            && effect == INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_EFFECT
                            && reason
                                == INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON
                    );
                if !recoverable_post_terminal_receipt {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
            }
        }

        let scope_key = IntentRevisionCommandScope::from(&consumption.claim.source_command);
        let scope = index
            .attempt_scopes
            .get(&scope_key)
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        let (lineage_start, current_position, status) = scope.validate_committed_lineage(
            source_terminal.lineage_start,
            consumer_intent_index,
            indexed_receipt.event_index,
        )?;
        committed_intervals
            .entry(scope_key)
            .or_default()
            .push((lineage_start, current_position));
        let later_web_intent = index
            .latest_web_intent_sequence_by_scope
            .get(&IntentRevisionCommandScope::from(
                &consumption.claim.consumer_intent.identity,
            ))
            .is_some_and(|latest| *latest > consumption.consumer_intent_sequence);
        if matches!(status, CodeCommandStatus::Pending) && later_web_intent {
            return Err(
                CodeCommandStoreError::PendingRevisionReceiptFollowedByWebIntent {
                    command_id: consumption
                        .claim
                        .consumer_intent
                        .identity
                        .command_id
                        .clone(),
                },
            );
        }
        let consumer_status = Some(status);
        receipts.push(ValidatedIntentRevisionReceipt {
            consumption,
            source_terminal_index,
            consumer_intent_index,
            receipt_index: indexed_receipt.event_index,
            consumer_status,
            canonical_cancel,
            replacement_review,
            replacement_review_open: replacement_proof.is_some_and(|proof| !proof.resolved),
            later_web_intent,
        });
    }

    let mut committed_consumer_statuses = HashMap::new();
    for (scope_key, intervals) in committed_intervals {
        let scope = index
            .attempt_scopes
            .get(&scope_key)
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        let mut coverage_delta = vec![0_i64; scope.attempts.len().saturating_add(1)];
        for (start, end) in intervals {
            coverage_delta[start] = coverage_delta[start].saturating_add(1);
            coverage_delta[end.saturating_add(1)] =
                coverage_delta[end.saturating_add(1)].saturating_sub(1);
        }
        let mut coverage = 0_i64;
        for (position, attempt) in scope.attempts.iter().enumerate() {
            record_intent_revision_replay_index_visits(1);
            coverage = coverage.saturating_add(coverage_delta[position]);
            if coverage <= 0 {
                continue;
            }
            let status = scope
                .current_statuses
                .get(position)
                .and_then(Clone::clone)
                .ok_or(CodeCommandStoreError::InvalidIntent)?;
            if committed_consumer_statuses
                .insert(attempt.command.identity.clone(), status.clone())
                .is_some_and(|existing| existing != status)
            {
                return Err(CodeCommandStoreError::TerminalConflict {
                    command_id: attempt.command.identity.command_id.clone(),
                });
            }
        }
    }

    Ok(ValidatedIntentRevisionReceiptIndex {
        replay_index: index,
        source_terminals,
        receipts,
        committed_consumer_statuses,
    })
}

#[derive(Debug, Default)]
pub struct PendingMutationRecoveryOutcome {
    pub fenced: Vec<CodeCommandIdentity>,
    pub phase1_prewrite_reattached: bool,
    /// An exact Consuming/no-receipt lineage proved that its latest consumer
    /// never reached the provider/tool loop. Startup may clear only that
    /// consumer's stale browser reconciliation projection before rearming the
    /// retained revision.
    pub intent_revision_consumer_healed: bool,
    /// An authenticated exact receipt plus canonical `/intent cancel`
    /// request proved that the revision cancellation completed before its
    /// Runtime success terminal was fsynced. Recovery completed that one
    /// idempotent control command rather than fencing it as an unknown tool
    /// mutation.
    pub intent_revision_cancel_healed: bool,
    /// An authenticated replacement review marker proved that the revision
    /// provider turn finished even though its Runtime terminal or browser
    /// projection may not have reached durable rest. Startup may close only
    /// the stale live rows before restoring that exact review gate.
    pub intent_revision_replacement_review_healed: bool,
}

/// Result of attempting to admit a command. A duplicate with the same
/// canonical payload never executes a second time; the caller receives the
/// previously durable state instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeCommandAdmission {
    Execute { intent: CodeCommandIntent },
    Existing { status: CodeCommandStatus },
}

/// Recovery decision for a command found in the JSONL log after a restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeCommandRecovery {
    RetryReadOnly { intent: CodeCommandIntent },
    Existing { status: CodeCommandStatus },
}

#[derive(Debug, thiserror::Error)]
pub enum CodeCommandStoreError {
    #[error("Code command identity and canonical request metadata must be non-empty")]
    InvalidIntent,
    #[error(
        "pending consumer for IntentSpec revision receipt on Code command '{command_id}' is followed by a later Web command; refusing startup recovery because the durable command order is inconsistent (start a new session or restore a consistent session log)"
    )]
    PendingRevisionReceiptFollowedByWebIntent { command_id: String },
    #[error(
        "Code command '{command_id}' for repo '{repo_id}', session '{session_id}', principal '{principal_id}' was reused with a different canonical payload"
    )]
    PayloadConflict {
        repo_id: String,
        session_id: String,
        principal_id: String,
        command_id: String,
    },
    #[error("Code command '{command_id}' has a terminal event without a durable intent")]
    TerminalWithoutIntent { command_id: String },
    #[error("Code command '{command_id}' has conflicting terminal results")]
    TerminalConflict { command_id: String },
    #[error("Code command '{command_id}' has no durable intent in this session")]
    MissingIntent { command_id: String },
    #[error("failed to access the Code command session log: {0}")]
    Storage(#[from] io::Error),
}

/// Gap observed while replaying Code workflow event cursors.
///
/// A gap is data, rather than a silently repaired sequence: a resume/SSE
/// adapter can request a snapshot or surface the loss to its caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeWorkflowSequenceGap {
    pub after: u64,
    pub before: u64,
}

/// De-duplicated, ordered view of Code workflow rows in one session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodeWorkflowReplay {
    pub events: Vec<CodeWorkflowEvent>,
    pub gaps: Vec<CodeWorkflowSequenceGap>,
    /// Bounded byte-window started after byte 0 and discarded an incomplete
    /// leading JSONL fragment (W3-08). Prefix sequence gaps in that case are
    /// transport truncation, not durable log corruption.
    pub window_cut_mid_record: bool,
}

/// OC-Phase 4 ArtifactLedger JSONL projection envelope (v0.17.810).
///
/// One row per Phase 3/Phase 4 artefact write. The payload itself
/// is a free-form `serde_json::Value` so callers can attach any
/// future shape (`ValidationReport`, `RiskScoreBreakdown`,
/// `DecisionProposal`, …) without a SessionEvent enum bump per
/// artefact kind. Replay code that wants a typed view does the
/// `serde_json::from_value::<TypedShape>(payload.payload)` deserialise
/// at the projection layer instead of in the JSONL parser.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AiArtifactEvent {
    pub event_id: Uuid,
    pub recorded_at: DateTime<Utc>,
    /// Stable thread id the artefact attaches to. Matches the
    /// `thread_id` column on each persisted artefact row so a
    /// session JSONL replay can correlate to the SeaORM rows.
    pub thread_id: Uuid,
    /// Short tag identifying the artefact kind. Free-form
    /// snake_case so a future Phase 5 artefact type can land
    /// without a SessionEvent enum bump.
    pub artifact_kind: String,
    /// Optional artefact-specific id (UUID-as-string today). None
    /// only for kinds that don't carry their own id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    /// Free-form structured payload. Required field — callers
    /// must supply a `serde_json::Value` (object preferred). An
    /// empty `Object({})` is acceptable for kinds whose
    /// `artifact_id` already carries all the signal.
    pub payload: serde_json::Value,
}

/// Dedicated child tool-call transcript event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionToolCallEvent {
    pub event_id: Uuid,
    pub recorded_at: DateTime<Utc>,
    pub agent_run_id: AgentRunId,
    pub subagent_name: String,
    pub call_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// Dedicated child tool-result transcript event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionToolResultEvent {
    pub event_id: Uuid,
    pub recorded_at: DateTime<Utc>,
    pub agent_run_id: AgentRunId,
    pub subagent_name: String,
    pub call_id: String,
    pub tool_name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Full session-state snapshot event.
///
/// Snapshots keep CEX-12 compatible with the existing `SessionState` resume
/// surface while moving the truth source from rewrite-in-place JSON blobs to
/// append-only JSONL. Later CEX cards can add finer-grained events and replay
/// them through the same reader.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSnapshotEvent {
    pub event_id: Uuid,
    pub recorded_at: DateTime<Utc>,
    pub state: SessionState,
}

impl SessionEvent {
    pub fn snapshot(state: SessionState) -> Self {
        Self::SessionSnapshot(SessionSnapshotEvent {
            event_id: Uuid::new_v4(),
            recorded_at: Utc::now(),
            state,
        })
    }

    pub fn context_frame(event: ContextFrameEvent) -> Self {
        Self::ContextFrame(event)
    }

    pub fn compaction(event: CompactionEvent) -> Self {
        Self::CompactionEvent(event)
    }

    pub fn memory_anchor(event: MemoryAnchorEvent) -> Self {
        Self::MemoryAnchor(event)
    }

    pub fn agent_run(event: AgentRunEvent) -> Self {
        Self::AgentRun(event.into())
    }

    pub fn tool_call(event: SessionToolCallEvent) -> Self {
        Self::ToolCall(event)
    }

    pub fn tool_result(event: SessionToolResultEvent) -> Self {
        Self::ToolResult(event)
    }

    pub fn goal(event: GoalEventEnvelope) -> Self {
        Self::Goal(event)
    }

    pub fn ai_artifact(event: AiArtifactEvent) -> Self {
        Self::AiArtifact(event)
    }

    pub fn code_workflow(event: CodeWorkflowEvent) -> Self {
        Self::CodeWorkflow(event)
    }

    pub fn apply_to(&self, current: &mut Option<SessionState>) {
        match self {
            Self::SessionSnapshot(event) => {
                *current = Some(event.state.clone());
            }
            // Goal envelopes do NOT mutate the legacy `SessionState`.
            // Replay into a `GoalState` lives in
            // `crate::internal::ai::goal::state::replay`. Listing the
            // variant here makes the no-op explicit so a future
            // maintainer does not assume an oversight.
            //
            // AiArtifact envelopes also do not mutate the legacy
            // `SessionState`; they're a JSONL projection of
            // Phase 3/Phase 4 SeaORM writes that the artefact
            // ledger replay reads through a separate projection
            // (similar to GoalState replay).
            Self::ContextFrame(_)
            | Self::CompactionEvent(_)
            | Self::MemoryAnchor(_)
            | Self::AgentRun(_)
            | Self::ToolCall(_)
            | Self::ToolResult(_)
            | Self::Goal(_)
            | Self::AiArtifact(_)
            | Self::CodeWorkflow(_) => {}
        }
    }
}

impl Event for SessionEvent {
    fn event_kind(&self) -> &'static str {
        match self {
            Self::SessionSnapshot(_) => "session_snapshot",
            Self::ContextFrame(event) => event.event_kind(),
            Self::CompactionEvent(event) => event.event_kind(),
            Self::MemoryAnchor(event) => event.event_kind(),
            Self::AgentRun(_) => "agent_run",
            Self::ToolCall(_) => "tool_call",
            Self::ToolResult(_) => "tool_result",
            Self::Goal(event) => event.event_kind(),
            Self::AiArtifact(_) => "ai_artifact",
            Self::CodeWorkflow(_) => "code_workflow",
        }
    }

    fn event_id(&self) -> Uuid {
        match self {
            Self::SessionSnapshot(event) => event.event_id,
            Self::ContextFrame(event) => event.event_id(),
            Self::CompactionEvent(event) => event.event_id(),
            Self::MemoryAnchor(event) => event.event_id(),
            Self::AgentRun(event) => event
                .known()
                .map(crate::internal::ai::runtime::Event::event_id)
                .unwrap_or_else(uuid::Uuid::nil),
            Self::ToolCall(event) => event.event_id,
            Self::ToolResult(event) => event.event_id,
            Self::Goal(event) => event.event_id(),
            Self::AiArtifact(event) => event.event_id,
            Self::CodeWorkflow(event) => event.event_id,
        }
    }

    fn event_summary(&self) -> String {
        match self {
            Self::SessionSnapshot(event) => format!(
                "session {} snapshot with {} message(s)",
                event.state.id,
                event.state.messages.len()
            ),
            Self::ContextFrame(event) => event.event_summary(),
            Self::CompactionEvent(event) => event.event_summary(),
            Self::MemoryAnchor(event) => event.event_summary(),
            Self::AgentRun(event) => event
                .known()
                .map(crate::internal::ai::runtime::Event::event_summary)
                .unwrap_or_else(|| "unknown agent_run event".to_string()),
            Self::ToolCall(event) => format!(
                "sub-agent {} tool_call {} ({})",
                event.subagent_name, event.call_id, event.tool_name
            ),
            Self::ToolResult(event) => format!(
                "sub-agent {} tool_result {} ({}) status={}",
                event.subagent_name, event.call_id, event.tool_name, event.status
            ),
            Self::Goal(event) => event.event_summary(),
            Self::AiArtifact(event) => format!(
                "ai_artifact {} (thread {}) {}",
                event.artifact_kind,
                event.thread_id,
                event.artifact_id.as_deref().unwrap_or("-")
            ),
            Self::CodeWorkflow(event) => format!(
                "code_workflow sequence={} {}",
                event.sequence,
                code_workflow_event_summary(&event.event)
            ),
        }
    }
}

fn code_workflow_event_summary(event: &CodeWorkflowEventKind) -> String {
    match event {
        CodeWorkflowEventKind::CommandAccepted {
            command_id,
            workflow,
        } => format!("command accepted {command_id} ({workflow})"),
        CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id,
            intent_id,
            turn_id,
            phase0_turn_id,
        } => {
            let mut summary = if turn_id.is_empty() {
                format!("intent review {interaction_id} ({intent_id})")
            } else {
                format!("intent review {interaction_id} ({intent_id}) turn={turn_id}")
            };
            if !phase0_turn_id.is_empty() {
                summary.push_str(&format!(" phase0={phase0_turn_id}"));
            }
            summary
        }
        CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id,
            plan_id,
            revision_of,
            ..
        } => revision_of.as_ref().map_or_else(
            || format!("plan review {interaction_id} ({plan_id})"),
            |source| format!("plan revision {source} -> {interaction_id} ({plan_id})"),
        ),
        CodeWorkflowEventKind::Phase1FormalWriteStarted {
            phase1_turn_id,
            source_interaction_id,
            ..
        } => format!("phase1 formal write started {phase1_turn_id} from {source_interaction_id}"),
        CodeWorkflowEventKind::NetworkPolicyRequested {
            interaction_id,
            plan_id,
            turn_id,
            default_allow,
        } => {
            let default = if *default_allow { "allow" } else { "deny" };
            if turn_id.is_empty() {
                format!("network policy {interaction_id} ({plan_id}) default={default}")
            } else {
                format!(
                    "network policy {interaction_id} ({plan_id}) default={default} turn={turn_id}"
                )
            }
        }
        CodeWorkflowEventKind::PlanExecutionRepairRequested {
            interaction_id,
            turn_id,
            ..
        } => {
            if turn_id.is_empty() {
                format!("plan execution repair {interaction_id}")
            } else {
                format!("plan execution repair {interaction_id} turn={turn_id}")
            }
        }
        CodeWorkflowEventKind::InteractionResolved {
            interaction_id,
            intent_revision_consumption: Some(_),
            ..
        } => format!("IntentSpec revision {interaction_id} consumed"),
        CodeWorkflowEventKind::InteractionResolved {
            interaction_id,
            resolution,
            intent_revision_consumption: None,
            ..
        } => format!("interaction {interaction_id} resolved ({resolution})"),
        CodeWorkflowEventKind::CodeUiProjectionDelta {
            projection,
            summary,
            ..
        } => format!("projection {projection}: {summary}"),
        CodeWorkflowEventKind::TerminalSuccess {
            command_id,
            summary,
        } => format!("command {command_id} succeeded: {summary}"),
        CodeWorkflowEventKind::TerminalFailure { command_id, reason } => {
            format!("command {command_id} failed: {reason}")
        }
        CodeWorkflowEventKind::IndeterminateSideEffect {
            command_id,
            effect,
            reason,
        } => format!("command {command_id} indeterminate {effect}: {reason}"),
        CodeWorkflowEventKind::CommandIntentPersisted { command } => format!(
            "durable command intent {} ({})",
            command.identity.command_id, command.command_kind
        ),
        CodeWorkflowEventKind::CommandTerminalSuccess { command, summary } => {
            format!(
                "durable command {} succeeded: {summary}",
                command.command_id
            )
        }
        CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            command,
            summary,
            interaction_id,
            resolution,
            ..
        } => format!(
            "durable command {} succeeded: {summary}; interaction {interaction_id} resolved ({resolution})",
            command.command_id
        ),
        CodeWorkflowEventKind::CommandTerminalFailure {
            command, reason, ..
        } => {
            format!("durable command {} failed: {reason}", command.command_id)
        }
        CodeWorkflowEventKind::CommandIndeterminateSideEffect {
            command,
            effect,
            reason,
        } => format!(
            "durable command {} indeterminate {effect}: {reason}",
            command.command_id
        ),
    }
}

fn payload_conflict(identity: &CodeCommandIdentity) -> CodeCommandStoreError {
    CodeCommandStoreError::PayloadConflict {
        repo_id: identity.repo_id.clone(),
        session_id: identity.session_id.clone(),
        principal_id: identity.principal_id.clone(),
        command_id: identity.command_id.clone(),
    }
}

fn apply_command_status_event(
    commands: &mut HashMap<CodeCommandIdentity, CachedCodeCommand>,
    event: &CodeWorkflowEventKind,
) {
    let (identity, intent, terminal) = match event {
        CodeWorkflowEventKind::CommandIntentPersisted { command } => {
            (&command.identity, Some(command), None)
        }
        CodeWorkflowEventKind::CommandTerminalSuccess { command, summary } => (
            command,
            None,
            Some(CodeCommandStatus::Succeeded {
                summary: summary.clone(),
            }),
        ),
        CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            command,
            summary,
            ..
        } => (
            command,
            None,
            Some(CodeCommandStatus::Succeeded {
                summary: summary.clone(),
            }),
        ),
        CodeWorkflowEventKind::CommandTerminalFailure {
            command, reason, ..
        } => (
            command,
            None,
            Some(CodeCommandStatus::Failed {
                reason: reason.clone(),
            }),
        ),
        CodeWorkflowEventKind::CommandIndeterminateSideEffect {
            command,
            effect,
            reason,
        } => (
            command,
            None,
            Some(CodeCommandStatus::Indeterminate {
                effect: effect.clone(),
                reason: reason.clone(),
            }),
        ),
        _ => return,
    };
    let cached = commands.entry(identity.clone()).or_default();
    if let Some(intent) = intent {
        if let Some(existing) = cached.intent.as_ref()
            && existing != intent
        {
            cached.payload_conflict = true;
        } else {
            cached.intent = Some(intent.clone());
            match &cached.status {
                None => {
                    cached.status = Some(CodeCommandStatus::Pending);
                }
                Some(CodeCommandStatus::Pending) => {}
                // Intent after a terminal row is malformed: fail closed so
                // admit/recover cannot treat the command as a fresh Pending.
                Some(_) => cached.terminal_conflict = true,
            }
        }
    }
    if let Some(target) = terminal {
        match &cached.status {
            // Keep an orphan terminal so lookups surface TerminalWithoutIntent
            // instead of silently dropping it and later re-dispatching.
            None => cached.status = Some(target),
            Some(CodeCommandStatus::Pending) => cached.status = Some(target),
            Some(existing) if *existing == target => {}
            Some(_) => cached.terminal_conflict = true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionContextReplay {
    pub frames: Vec<ContextFrameEvent>,
    pub compactions: Vec<CompactionEvent>,
}

/// Fan-out after a successful Code workflow JSONL append (SSE wire v2 hub).
pub type CodeWorkflowAppendHook = Arc<dyn Fn(&CodeWorkflowEvent) + Send + Sync>;

#[derive(Clone)]
pub struct SessionJsonlStore {
    session_root: PathBuf,
    /// Lazily populated from the workflow log, then updated by each durable
    /// command transition. This avoids replaying the entire session log for
    /// every command admission or completion in one running session.
    command_status_cache: Arc<Mutex<Option<HashMap<CodeCommandIdentity, CachedCodeCommand>>>>,
    /// Optional fan-out after a successful Code workflow append (SSE wire v2).
    /// Shared across clones so every writer of this session log publishes once.
    on_code_workflow_append: Option<CodeWorkflowAppendHook>,
    #[cfg(any(test, feature = "test-provider"))]
    test_faults: Arc<SessionJsonlTestFaults>,
}

#[cfg(any(test, feature = "test-provider"))]
#[derive(Debug, Default)]
struct SessionJsonlTestFaults {
    fail_next_combined_terminal_append: AtomicBool,
    fail_next_pending_interaction_checkpoint: AtomicBool,
    fail_next_durable_sync_after_write: AtomicBool,
    fail_next_events_log_resync: AtomicBool,
    fail_next_phase1_seed_parent_sync: AtomicBool,
    fail_next_phase1_seed_sync_after_remove: AtomicBool,
}

impl std::fmt::Debug for SessionJsonlStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionJsonlStore")
            .field("session_root", &self.session_root)
            .field(
                "on_code_workflow_append",
                &self.on_code_workflow_append.is_some(),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
struct CachedCodeCommand {
    intent: Option<CodeCommandIntent>,
    status: Option<CodeCommandStatus>,
    payload_conflict: bool,
    terminal_conflict: bool,
}

/// A narrow cross-process lock for sequence allocation plus one Code workflow
/// JSONL append. The lock deliberately covers neither tool execution nor
/// projection work. W1-05 will reuse this append boundary to add the required
/// mutation durability/fsync protocol.
#[derive(Debug)]
struct CodeWorkflowAppendLock {
    _path: PathBuf,
    /// Closing the descriptor releases the OS advisory lock. The path is
    /// deliberately persistent: unlinking lock files permits inode-split ABA.
    _file: fs::File,
}

impl SessionJsonlStore {
    pub fn new(session_root: PathBuf) -> Self {
        Self {
            session_root,
            command_status_cache: Arc::new(Mutex::new(None)),
            on_code_workflow_append: None,
            #[cfg(any(test, feature = "test-provider"))]
            test_faults: Arc::new(SessionJsonlTestFaults::default()),
        }
    }

    /// Arm one instance-scoped combined-terminal append failure. Clones of
    /// this session store share the fault, while unrelated tests/sessions do
    /// not race through a process-global flag.
    #[cfg(any(test, feature = "test-provider"))]
    pub fn fail_next_combined_terminal_append_for_test(&self) {
        self.test_faults
            .fail_next_combined_terminal_append
            .store(true, Ordering::Release);
    }

    #[cfg(any(test, feature = "test-provider"))]
    pub(crate) fn take_combined_terminal_append_failure_for_test(&self) -> bool {
        self.test_faults
            .fail_next_combined_terminal_append
            .swap(false, Ordering::AcqRel)
    }

    /// Arm one instance-scoped non-terminal response checkpoint failure.
    #[cfg(any(test, feature = "test-provider"))]
    pub fn fail_next_pending_interaction_checkpoint_for_test(&self) {
        self.test_faults
            .fail_next_pending_interaction_checkpoint
            .store(true, Ordering::Release);
    }

    #[cfg(any(test, feature = "test-provider"))]
    pub(crate) fn take_pending_interaction_checkpoint_failure_for_test(&self) -> bool {
        self.test_faults
            .fail_next_pending_interaction_checkpoint
            .swap(false, Ordering::AcqRel)
    }

    /// Arm one instance-scoped fault after a complete JSONL row is written but
    /// before its durability sync. A retry must re-sync an exact existing row
    /// before acknowledging success.
    #[cfg(any(test, feature = "test-provider"))]
    pub fn fail_next_durable_sync_after_write_for_test(&self) {
        self.test_faults
            .fail_next_durable_sync_after_write
            .store(true, Ordering::Release);
    }

    #[cfg(any(test, feature = "test-provider"))]
    fn take_durable_sync_after_write_failure_for_test(&self) -> bool {
        self.test_faults
            .fail_next_durable_sync_after_write
            .swap(false, Ordering::AcqRel)
    }

    /// Arm one instance-scoped failure for an explicit event-log re-sync.
    /// This is distinct from the post-write fault above so exact retry tests
    /// can prove that observing an existing row never bypasses fsync.
    #[cfg(any(test, feature = "test-provider"))]
    pub fn fail_next_events_log_resync_for_test(&self) {
        self.test_faults
            .fail_next_events_log_resync
            .store(true, Ordering::Release);
    }

    #[cfg(any(test, feature = "test-provider"))]
    fn take_events_log_resync_failure_for_test(&self) -> bool {
        self.test_faults
            .fail_next_events_log_resync
            .swap(false, Ordering::AcqRel)
    }

    /// Arm one instance-scoped failure before the Phase 1 seed parent sync.
    /// The initial write reaches this point after replacement; an exact retry
    /// reaches it after re-syncing the visible seed file.
    #[cfg(any(test, feature = "test-provider"))]
    pub fn fail_next_phase1_seed_parent_sync_for_test(&self) {
        self.test_faults
            .fail_next_phase1_seed_parent_sync
            .store(true, Ordering::Release);
    }

    #[cfg(any(test, feature = "test-provider"))]
    pub(crate) fn take_phase1_seed_parent_sync_failure_for_test(&self) -> bool {
        self.test_faults
            .fail_next_phase1_seed_parent_sync
            .swap(false, Ordering::AcqRel)
    }

    /// Arm one instance-scoped failure after a Phase 1 start seed has been
    /// unlinked but before the containing directory is synced.
    #[cfg(any(test, feature = "test-provider"))]
    pub fn fail_next_phase1_seed_sync_after_remove_for_test(&self) {
        self.test_faults
            .fail_next_phase1_seed_sync_after_remove
            .store(true, Ordering::Release);
    }

    #[cfg(any(test, feature = "test-provider"))]
    pub(crate) fn take_phase1_seed_sync_after_remove_failure_for_test(&self) -> bool {
        self.test_faults
            .fail_next_phase1_seed_sync_after_remove
            .swap(false, Ordering::AcqRel)
    }

    /// Register a callback invoked after each successful Code workflow append.
    /// Used by the SSE wire v2 hub so projection deltas, goal envelopes, and
    /// durable command transitions all fan out on the same sequence space.
    pub fn set_on_code_workflow_append(&mut self, hook: Option<CodeWorkflowAppendHook>) {
        self.on_code_workflow_append = hook;
    }

    pub fn session_root(&self) -> &Path {
        &self.session_root
    }

    pub fn child(&self, child_id: &str) -> Self {
        Self::new(
            self.session_root
                .join("subagents")
                .join(child_dir_name(child_id)),
        )
    }

    pub fn events_path(&self) -> PathBuf {
        self.session_root.join(SESSION_EVENTS_FILE)
    }

    /// Append the next ordered Code workflow event for this session.
    ///
    /// This takes the session-local append lock around sequence allocation and
    /// the JSONL write. W1-05 will add the recovery-critical fsync/mutation
    /// ordering around the same boundary; callers must not use this method as
    /// a substitute for that durability contract.
    pub fn append_code_workflow(
        &self,
        event: CodeWorkflowEventKind,
    ) -> io::Result<CodeWorkflowEvent> {
        self.append_code_workflow_with_durability(event, false)
    }

    /// Append and fsync a Code workflow row while holding the same
    /// session-local sequence lock as [`Self::append_code_workflow`]. This is
    /// the recovery-critical write primitive used by durable command intent
    /// and terminal-result transitions.
    pub fn append_code_workflow_durable(
        &self,
        event: CodeWorkflowEventKind,
    ) -> io::Result<CodeWorkflowEvent> {
        self.append_code_workflow_with_durability(event, true)
    }

    /// Checkpoint already-delivered interaction responses while their command
    /// intentionally remains Pending across graceful process shutdown. The
    /// gate's current unresolved choice is not included; resume will append it
    /// atomically with the eventual command terminal.
    pub fn checkpoint_pending_interaction_resolutions(
        &self,
        identity: &CodeCommandIdentity,
        resolutions: &[(String, String)],
    ) -> Result<(), CodeCommandStoreError> {
        if resolutions.is_empty() {
            return Ok(());
        }
        if !identity.is_complete()
            || resolutions.iter().any(|(interaction_id, resolution)| {
                interaction_id.trim().is_empty() || resolution.trim().is_empty()
            })
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        fs::create_dir_all(&self.session_root)?;
        let _lock = self.acquire_code_workflow_append_lock()?;
        self.invalidate_command_status_cache();
        // One replay under the append lock derives command status, the latest
        // command-bound checkpoint, and the next sequence. This avoids the
        // prior three independent scans on every response checkpoint.
        let replay = self.load_code_workflow_replay()?;
        let mut statuses = HashMap::new();
        for event in &replay.events {
            apply_command_status_event(&mut statuses, &event.event);
        }
        let cached = statuses.remove(identity).unwrap_or_default();
        if cached.payload_conflict {
            return Err(payload_conflict(identity));
        }
        if cached.terminal_conflict {
            return Err(CodeCommandStoreError::TerminalConflict {
                command_id: identity.command_id.clone(),
            });
        }
        let status = match (cached.intent, cached.status) {
            (Some(_), Some(status)) => status,
            (None, None) => {
                return Err(CodeCommandStoreError::MissingIntent {
                    command_id: identity.command_id.clone(),
                });
            }
            (None, Some(_)) | (Some(_), None) => {
                return Err(CodeCommandStoreError::TerminalWithoutIntent {
                    command_id: identity.command_id.clone(),
                });
            }
        };
        if status != CodeCommandStatus::Pending {
            return Err(CodeCommandStoreError::TerminalConflict {
                command_id: identity.command_id.clone(),
            });
        }
        let prior = replay
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.event {
                CodeWorkflowEventKind::InteractionResolved {
                    interaction_id,
                    resolution,
                    command: Some(command),
                    prior_interaction_resolutions,
                    intent_revision_consumption: None,
                    ..
                } if command == identity => Some(
                    prior_interaction_resolutions
                        .iter()
                        .cloned()
                        .chain(std::iter::once((
                            interaction_id.clone(),
                            resolution.clone(),
                        )))
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            });
        if let Some(prior) = prior.as_ref() {
            if prior == resolutions {
                // The prior append may have written a complete row and then
                // failed its sync. Exact idempotency is only durable after a
                // fresh sync under the same append lock.
                self.sync_events_log()?;
                return Ok(());
            }
            if prior.len() > resolutions.len()
                || prior
                    .iter()
                    .zip(resolutions)
                    .any(|(existing, proposed)| existing != proposed)
            {
                return Err(CodeCommandStoreError::TerminalConflict {
                    command_id: identity.command_id.clone(),
                });
            }
        }
        let (last, prior) = resolutions
            .split_last()
            .ok_or(CodeCommandStoreError::InvalidIntent)?;
        let next_sequence = match replay.events.last() {
            Some(event) => event.sequence.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "cannot append Code workflow event: sequence reached u64::MAX",
                )
            })?,
            None => 1,
        };
        self.append_code_workflow_at_sequence_while_locked(
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id: last.0.clone(),
                resolution: last.1.clone(),
                command: Some(identity.clone()),
                prior_interaction_resolutions: prior.to_vec(),
                intent_revision_consumption: None,
            },
            true,
            next_sequence,
        )?;
        Ok(())
    }

    /// Append a batch of Code workflow rows under one sequence lock.
    ///
    /// Prefer this over a loop of [`Self::append_code_workflow`] when an SSE
    /// hub is attached so fan-out can fsync once per batch.
    pub fn append_code_workflow_batch(
        &self,
        events: &[CodeWorkflowEventKind],
    ) -> io::Result<Vec<CodeWorkflowEvent>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        fs::create_dir_all(&self.session_root).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to create session directory '{}': {error}",
                    self.session_root.display()
                ),
            )
        })?;
        let _lock = self.acquire_code_workflow_append_lock()?;
        self.append_code_workflow_kinds_while_locked(events, false)
    }

    fn append_code_workflow_with_durability(
        &self,
        event: CodeWorkflowEventKind,
        durable: bool,
    ) -> io::Result<CodeWorkflowEvent> {
        fs::create_dir_all(&self.session_root).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to create session directory '{}': {error}",
                    self.session_root.display()
                ),
            )
        })?;
        let _lock = self.acquire_code_workflow_append_lock()?;
        self.append_code_workflow_while_locked(event, durable)
    }

    pub fn append(&self, event: &SessionEvent) -> io::Result<()> {
        if matches!(event, SessionEvent::CodeWorkflow(_)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "append Code workflow events through append_code_workflow so sequence allocation stays serialized",
            ));
        }
        // SessionState compatibility snapshots and Code workflow rows share
        // one JSONL file.  Taking the same lock prevents a legacy snapshot
        // writer from interleaving bytes with a projection/durability append.
        // Code workflow callers already hold this lock and use
        // `append_code_workflow_while_locked` directly.
        fs::create_dir_all(&self.session_root).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to create session directory '{}': {error}",
                    self.session_root.display()
                ),
            )
        })?;
        let _lock = self.acquire_code_workflow_append_lock()?;
        self.append_unchecked(event, false)
    }

    fn append_code_workflow_while_locked(
        &self,
        event: CodeWorkflowEventKind,
        durable: bool,
    ) -> io::Result<CodeWorkflowEvent> {
        let mut events = self.append_code_workflow_kinds_while_locked(&[event], durable)?;
        // INVARIANT: exactly one event was requested.
        Ok(events
            .pop()
            .expect("append_code_workflow_kinds_while_locked returns one event per input"))
    }

    fn append_code_workflow_at_sequence_while_locked(
        &self,
        event: CodeWorkflowEventKind,
        durable: bool,
        sequence: u64,
    ) -> io::Result<CodeWorkflowEvent> {
        let mut events = self.append_code_workflow_kinds_from_sequence_while_locked(
            &[event],
            durable,
            sequence,
        )?;
        // INVARIANT: exactly one event was requested.
        Ok(events.pop().expect(
            "append_code_workflow_kinds_from_sequence_while_locked returns one event per input",
        ))
    }

    fn append_code_workflow_kinds_while_locked(
        &self,
        events: &[CodeWorkflowEventKind],
        durable: bool,
    ) -> io::Result<Vec<CodeWorkflowEvent>> {
        // Allocate once: `next_code_workflow_sequence` reads the on-disk tail,
        // so calling it per item before the batch is written would reuse the
        // same sequence for every row.
        let sequence = self.next_code_workflow_sequence()?;
        self.append_code_workflow_kinds_from_sequence_while_locked(events, durable, sequence)
    }

    fn append_code_workflow_kinds_from_sequence_while_locked(
        &self,
        events: &[CodeWorkflowEventKind],
        durable: bool,
        mut sequence: u64,
    ) -> io::Result<Vec<CodeWorkflowEvent>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let mut workflow_events = Vec::with_capacity(events.len());
        let mut session_events = Vec::with_capacity(events.len());
        for event in events {
            let workflow_event = CodeWorkflowEvent::new(sequence, event.clone());
            session_events.push(SessionEvent::code_workflow(workflow_event.clone()));
            workflow_events.push(workflow_event);
            sequence = sequence.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "cannot append Code workflow event: sequence reached u64::MAX",
                )
            })?;
        }
        self.append_unchecked_batch(&session_events, durable)?;
        for workflow_event in &workflow_events {
            self.update_command_status_cache(&workflow_event.event);
        }
        if let Some(hook) = self.on_code_workflow_append.as_ref() {
            // One fsync for the whole batch before fan-out so SSE cursors cannot
            // outrun crash-safe durability, without forcing every non-hub
            // append onto the durable path.
            if !durable {
                self.sync_events_log()?;
            }
            for workflow_event in &workflow_events {
                hook(workflow_event);
            }
        }
        Ok(workflow_events)
    }

    fn sync_events_log(&self) -> io::Result<()> {
        let path = self.events_path();
        let file = OpenOptions::new().write(true).open(&path).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "failed to open session event log '{}' for fsync: {err}",
                    path.display()
                ),
            )
        })?;
        #[cfg(any(test, feature = "test-provider"))]
        if self.take_events_log_resync_failure_for_test() {
            return Err(io::Error::other(
                "injected failure while re-syncing the durable session event log",
            ));
        }
        file.sync_data().map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "failed to fsync session event log '{}': {err}",
                    path.display()
                ),
            )
        })
    }

    fn append_unchecked(&self, event: &SessionEvent, durable: bool) -> io::Result<()> {
        fs::create_dir_all(&self.session_root).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "failed to create session directory '{}': {err}",
                    self.session_root.display()
                ),
            )
        })?;

        let path = self.events_path();
        let needs_separator = recover_truncated_tail_for_append(&path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!(
                        "failed to open session event log '{}': {err}",
                        path.display()
                    ),
                )
            })?;

        if needs_separator {
            file.write_all(b"\n").map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!(
                        "failed to restore JSONL line boundary in '{}': {err}",
                        path.display()
                    ),
                )
            })?;
        }
        serde_json::to_writer(&mut file, event)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        file.write_all(b"\n").map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "failed to append session event log '{}': {err}",
                    path.display()
                ),
            )
        })?;
        if durable {
            #[cfg(any(test, feature = "test-provider"))]
            if self.take_durable_sync_after_write_failure_for_test() {
                return Err(io::Error::other(
                    "injected failure after durable JSONL row write and before sync",
                ));
            }
            file.sync_data().map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!(
                        "failed to fsync durable session event log '{}': {err}",
                        path.display()
                    ),
                )
            })?;
        }
        Ok(())
    }

    fn append_unchecked_batch(&self, events: &[SessionEvent], durable: bool) -> io::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        if events.len() == 1 {
            return self.append_unchecked(&events[0], durable);
        }
        fs::create_dir_all(&self.session_root).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "failed to create session directory '{}': {err}",
                    self.session_root.display()
                ),
            )
        })?;
        let path = self.events_path();
        let needs_separator = recover_truncated_tail_for_append(&path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!(
                        "failed to open session event log '{}': {err}",
                        path.display()
                    ),
                )
            })?;
        if needs_separator {
            file.write_all(b"\n").map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!(
                        "failed to restore JSONL line boundary in '{}': {err}",
                        path.display()
                    ),
                )
            })?;
        }
        let mut buffer = Vec::new();
        for event in events {
            serde_json::to_writer(&mut buffer, event)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            buffer.push(b'\n');
        }
        file.write_all(&buffer).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "failed to append session event log '{}': {err}",
                    path.display()
                ),
            )
        })?;
        if durable {
            #[cfg(any(test, feature = "test-provider"))]
            if self.take_durable_sync_after_write_failure_for_test() {
                return Err(io::Error::other(
                    "injected failure after durable JSONL batch write and before sync",
                ));
            }
            file.sync_data().map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!(
                        "failed to fsync durable session event log '{}': {err}",
                        path.display()
                    ),
                )
            })?;
        }
        Ok(())
    }

    /// Load Code workflow rows with duplicate event ids removed and sequence
    /// gaps surfaced explicitly. Resume/SSE consumers should use this view
    /// rather than assuming a UUID or line number is an ordering cursor.
    pub fn load_code_workflow_replay(&self) -> io::Result<CodeWorkflowReplay> {
        code_workflow_replay_from_events(self.load_events()?, 0)
    }

    fn load_intent_revision_workflow_replay(&self) -> io::Result<CodeWorkflowReplay> {
        let path = self.events_path();
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "failed to read IntentSpec revision workflow log '{}': {error}",
                        path.display()
                    ),
                ));
            }
        };
        let ends_with_newline = content.ends_with('\n');
        let lines = content.lines().collect::<Vec<_>>();
        let mut events = Vec::new();
        for (line_index, line) in lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let line_number = line_index + 1;
            let value = match serde_json::from_str::<Value>(line) {
                Ok(value) => value,
                Err(error) if line_index + 1 == lines.len() && !ends_with_newline => {
                    tracing::warn!(
                        path = %path.display(),
                        line = line_number,
                        error = %error,
                        "stopping IntentSpec revision replay at malformed trailing line"
                    );
                    break;
                }
                Err(error) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "malformed complete line in IntentSpec revision workflow log '{}' line {line_number}: {error}",
                            path.display()
                        ),
                    ));
                }
            };
            if value.get("kind").and_then(Value::as_str) != Some("code_workflow") {
                continue;
            }
            match parse_code_workflow_event_value(value) {
                Ok(Some(SessionEvent::CodeWorkflow(event))) => {
                    events.push(SessionEvent::CodeWorkflow(event));
                }
                Ok(Some(_)) | Ok(None) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "IntentSpec revision authority cannot skip an unknown Code workflow row in '{}' line {line_number}",
                            path.display()
                        ),
                    ));
                }
                Err(error) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "failed to decode IntentSpec revision workflow log '{}' line {line_number}: {error}",
                            path.display()
                        ),
                    ));
                }
            }
        }
        let mut seen = HashMap::<Uuid, &CodeWorkflowEvent>::new();
        for event in &events {
            let SessionEvent::CodeWorkflow(event) = event else {
                continue;
            };
            if let Some(previous) = seen.insert(event.event_id, event)
                && previous != event
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Code workflow event id '{}' has conflicting durable payloads",
                        event.event_id
                    ),
                ));
            }
        }
        let replay = code_workflow_replay_from_events(events, 0)?;
        if !intent_revision_replay_is_complete(&replay) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "IntentSpec revision authority requires a complete Code workflow replay",
            ));
        }
        Ok(replay)
    }

    /// Sync the shared event log under its append lock before returning a full
    /// replay. Decision/ACK paths use this after a prior writer may have
    /// written a complete row but reported a sync failure; observing the row
    /// alone is not proof that it is crash durable.
    pub fn load_code_workflow_replay_committed(&self) -> io::Result<CodeWorkflowReplay> {
        let _lock = self.acquire_code_workflow_append_lock()?;
        match fs::metadata(self.events_path()) {
            Ok(_) => self.sync_events_log()?,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(error),
        }
        self.load_code_workflow_replay()
    }

    pub(crate) fn load_intent_revision_workflow_replay_committed(
        &self,
    ) -> io::Result<CodeWorkflowReplay> {
        let _lock = self.acquire_code_workflow_append_lock()?;
        match fs::metadata(self.events_path()) {
            Ok(_) => self.sync_events_log()?,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(error),
        }
        self.load_intent_revision_workflow_replay()
    }

    /// Read the bounded workflow suffix after a durable projection cursor.
    ///
    /// `max_bytes` puts an upper bound on disk access and `max_events` bounds
    /// projection work.  If the requested suffix begins before the retained
    /// tail window, the returned replay reports a sequence gap so callers fail
    /// closed rather than rebuilding a plausible partial UI snapshot.
    pub fn load_code_workflow_replay_since(
        &self,
        after_sequence: u64,
        max_events: usize,
        max_bytes: u64,
    ) -> io::Result<CodeWorkflowReplay> {
        self.load_code_workflow_replay_since_unlocked(after_sequence, max_events, max_bytes)
    }

    /// Same as [`Self::load_code_workflow_replay_since`], but holds the Code
    /// workflow append lock so readers cannot observe a trailing JSONL record
    /// that has been written but not yet fsynced/published.
    pub fn load_code_workflow_replay_since_committed(
        &self,
        after_sequence: u64,
        max_events: usize,
        max_bytes: u64,
    ) -> io::Result<CodeWorkflowReplay> {
        let _lock = self.acquire_code_workflow_append_lock()?;
        self.load_code_workflow_replay_since_unlocked(after_sequence, max_events, max_bytes)
    }

    fn load_code_workflow_replay_since_unlocked(
        &self,
        after_sequence: u64,
        max_events: usize,
        max_bytes: u64,
    ) -> io::Result<CodeWorkflowReplay> {
        if max_events == 0 || max_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Code workflow replay bounds must both be greater than zero",
            ));
        }
        let path = self.events_path();
        let mut file = match OpenOptions::new().read(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(CodeWorkflowReplay::default());
            }
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "failed to open session event log '{}' for bounded replay: {error}",
                        path.display()
                    ),
                ));
            }
        };
        let file_len = file.metadata()?.len();
        let start = file_len.saturating_sub(max_bytes);
        file.seek(SeekFrom::Start(start))?;
        let tail_capacity = usize::try_from(file_len - start).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "bounded Code workflow replay window for '{}' is too large for this platform",
                    path.display()
                ),
            )
        })?;
        let mut bytes = Vec::with_capacity(tail_capacity);
        file.read_to_end(&mut bytes)?;
        let mut content = String::from_utf8_lossy(&bytes).into_owned();
        if start > 0 {
            let Some(first_newline) = content.find('\n') else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "bounded Code workflow replay window for '{}' contains no complete JSONL record",
                        path.display()
                    ),
                ));
            };
            content.drain(..=first_newline);
        }

        let (events, window_tip_sequence) =
            parse_code_workflow_suffix_from_window(&path, &content, after_sequence, max_events)?;
        let mut replay = code_workflow_replay_from_events(events, after_sequence)?;
        replay.window_cut_mid_record = start > 0;
        if start > 0 && replay.events.is_empty() {
            match window_tip_sequence {
                // Client is already at the tip of the retained suffix: idle
                // reconnect with no new durable rows is success, not a gap.
                Some(tip) if tip == after_sequence => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "bounded Code workflow replay after sequence {after_sequence} cannot prove the retained tail of '{}' contains no omitted workflow events; create a projection checkpoint before resuming",
                            path.display()
                        ),
                    ));
                }
            }
        }
        if replay.events.len() > max_events {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Code workflow replay after sequence {after_sequence} has {} events, exceeding the bounded limit of {max_events}; create a projection checkpoint before resuming",
                    replay.events.len()
                ),
            ));
        }
        Ok(replay)
    }

    pub fn next_code_workflow_sequence(&self) -> io::Result<u64> {
        let replay = self.load_code_workflow_replay()?;
        match replay.events.last() {
            Some(event) => event.sequence.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "cannot append Code workflow event: sequence reached u64::MAX",
                )
            }),
            None => Ok(1),
        }
    }

    /// Durably admit a runtime command before it is dispatched. Reusing the
    /// same `(repo, session, principal, command)` identity with another kind
    /// or canonical request hash fails closed instead of risking a replay of
    /// an unintended mutation.
    pub fn admit_code_command(
        &self,
        intent: CodeCommandIntent,
    ) -> Result<CodeCommandAdmission, CodeCommandStoreError> {
        if !intent.is_valid() {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        fs::create_dir_all(&self.session_root)?;
        let _lock = self.acquire_code_workflow_append_lock()?;
        // Rebuild after the lock so another process's terminal append is visible
        // before we decide whether this identity may dispatch again.
        self.invalidate_command_status_cache();
        if let Some((existing_intent, status)) = self.code_command_status(&intent.identity)? {
            if existing_intent != intent {
                return Err(payload_conflict(&intent.identity));
            }
            self.sync_events_log()?;
            return Ok(CodeCommandAdmission::Existing { status });
        }
        if intent.mutating {
            self.claim_mutating_owner_lease()?;
        }
        match self.append_code_workflow_while_locked(
            CodeWorkflowEventKind::CommandIntentPersisted {
                command: intent.clone(),
            },
            true,
        ) {
            Ok(_) => Ok(CodeCommandAdmission::Execute { intent }),
            Err(error) => {
                if intent.mutating {
                    self.release_mutating_owner_lease();
                }
                Err(error.into())
            }
        }
    }

    /// Resolve a caller-validated source revision to the exact ordinary
    /// consumer intent row. This is read-only apart from re-syncing the event
    /// log; it lets the executor persist a downgrade-safe Consuming envelope
    /// before appending the irreversible receipt.
    pub fn prepare_intent_revision_consumption(
        &self,
        consumer_intent: &CodeCommandIntent,
        claim: &IntentRevisionConsumptionClaim,
    ) -> Result<IntentRevisionConsumption, CodeCommandStoreError> {
        if !consumer_intent.is_valid()
            || claim.consumer_intent != *consumer_intent
            || !intent_revision_consumption_claim_is_valid(claim)
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let _lock = self.acquire_code_workflow_append_lock()?;
        self.invalidate_command_status_cache();
        let replay = self.load_intent_revision_workflow_replay()?;
        let consumption = intent_revision_consumption_from_claim(&replay, consumer_intent, claim)?;
        let Some((consumer, status)) = self.code_command_status(&consumer_intent.identity)? else {
            return Err(CodeCommandStoreError::MissingIntent {
                command_id: consumer_intent.identity.command_id.clone(),
            });
        };
        if status != CodeCommandStatus::Pending || consumer != *consumer_intent {
            return Err(CodeCommandStoreError::TerminalConflict {
                command_id: consumer_intent.identity.command_id.clone(),
            });
        }
        self.sync_events_log()?;
        Ok(consumption)
    }

    /// Resolve a pre-admission Claiming sidecar to its exact durable command
    /// row without requiring that command to remain Pending. Startup uses the
    /// returned status to rearm only a canonical pre-mutation cancellation;
    /// all other terminal-without-receipt states remain fail-closed.
    pub(crate) fn resolve_claimed_intent_revision_consumption(
        &self,
        claim: &IntentRevisionConsumptionClaim,
    ) -> Result<(IntentRevisionConsumption, CodeCommandStatus), CodeCommandStoreError> {
        let consumer_intent = &claim.consumer_intent;
        if !consumer_intent.is_valid() || !intent_revision_consumption_claim_is_valid(claim) {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let _lock = self.acquire_code_workflow_append_lock()?;
        self.invalidate_command_status_cache();
        let replay = self.load_intent_revision_workflow_replay()?;
        let consumption = intent_revision_consumption_from_claim(&replay, consumer_intent, claim)?;
        let Some((consumer, status)) = self.code_command_status(&consumer_intent.identity)? else {
            return Err(CodeCommandStoreError::MissingIntent {
                command_id: consumer_intent.identity.command_id.clone(),
            });
        };
        if consumer != *consumer_intent {
            return Err(payload_conflict(&consumer_intent.identity));
        }
        let exact_status = claimed_intent_revision_consumer_status(&replay, &consumption)?;
        if exact_status != status {
            return Err(CodeCommandStoreError::TerminalConflict {
                command_id: consumer_intent.identity.command_id.clone(),
            });
        }
        self.sync_events_log()?;
        Ok((consumption, status))
    }

    /// Commit the irreversible IntentSpec revision consume boundary. The
    /// consumer intent, browser message, and same-path Consuming envelope are
    /// already durable; the executor may unlink the sidecar only after this
    /// receipt fsync (or an exact prior receipt re-sync) succeeds.
    pub fn record_intent_revision_consumption(
        &self,
        consumption: &IntentRevisionConsumption,
    ) -> Result<(), CodeCommandStoreError> {
        let claim = &consumption.claim;
        if !intent_revision_consumption_claim_is_valid(claim)
            || consumption.consumer_intent_sequence == 0
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let _lock = self.acquire_code_workflow_append_lock()?;
        self.invalidate_command_status_cache();
        let replay = self.load_intent_revision_workflow_replay()?;
        let (_, exact_receipt_index) =
            exact_intent_revision_consumption_receipt_indices(&replay, consumption)?;
        if exact_receipt_index.is_some() {
            self.sync_events_log()?;
            return Ok(());
        }
        let Some((consumer, status)) = self.code_command_status(&claim.consumer_intent.identity)?
        else {
            return Err(CodeCommandStoreError::MissingIntent {
                command_id: claim.consumer_intent.identity.command_id.clone(),
            });
        };
        if status != CodeCommandStatus::Pending || consumer != claim.consumer_intent {
            return Err(CodeCommandStoreError::TerminalConflict {
                command_id: claim.consumer_intent.identity.command_id.clone(),
            });
        }
        let receipt = intent_revision_consumption_receipt_event(consumption);
        if let Err(append_error) = self.append_code_workflow_while_locked(receipt, true) {
            // A complete receipt row can be visible after a post-write sync
            // failure. Rebuild under the same append lock and re-sync only an
            // exact full-payload receipt; a pre-write failure remains an error.
            let replay = self.load_intent_revision_workflow_replay()?;
            let (_, exact_receipt_index) =
                exact_intent_revision_consumption_receipt_indices(&replay, consumption)?;
            if exact_receipt_index.is_some() {
                self.sync_events_log()?;
                return Ok(());
            }
            return Err(append_error.into());
        }
        Ok(())
    }

    /// Recover a command after an interrupted runtime. Read-only pending
    /// commands are explicitly eligible for a caller-controlled retry;
    /// mutating pending commands become durable `Indeterminate` state and are
    /// never replayed automatically.
    pub fn recover_code_command(
        &self,
        identity: &CodeCommandIdentity,
    ) -> Result<CodeCommandRecovery, CodeCommandStoreError> {
        if !identity.is_complete() {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        fs::create_dir_all(&self.session_root)?;
        let _lock = self.acquire_code_workflow_append_lock()?;
        self.invalidate_command_status_cache();
        let Some((intent, status)) = self.code_command_status(identity)? else {
            return Err(CodeCommandStoreError::MissingIntent {
                command_id: identity.command_id.clone(),
            });
        };
        self.sync_events_log()?;

        match status {
            CodeCommandStatus::Pending if intent.mutating => {
                if self.live_mutating_owner_exists() {
                    return Ok(CodeCommandRecovery::Existing {
                        status: CodeCommandStatus::Pending,
                    });
                }
                let status = CodeCommandStatus::Indeterminate {
                    effect: "unknown_mutating_dispatch".to_string(),
                    reason:
                        "runtime stopped after durable intent; manual reconciliation is required"
                            .to_string(),
                };
                self.append_code_workflow_while_locked(
                    CodeWorkflowEventKind::CommandIndeterminateSideEffect {
                        command: identity.clone(),
                        effect: "unknown_mutating_dispatch".to_string(),
                        reason: "runtime stopped after durable intent; manual reconciliation is required"
                            .to_string(),
                    },
                    true,
                )?;
                self.release_mutating_owner_lease_if_no_pending_mutations()?;
                Ok(CodeCommandRecovery::Existing { status })
            }
            CodeCommandStatus::Pending => Ok(CodeCommandRecovery::RetryReadOnly { intent }),
            status => Ok(CodeCommandRecovery::Existing { status }),
        }
    }

    /// Recover every mutating command that still requires reconciliation before
    /// a new runtime accepts turns. Pending mutations are durably fenced as
    /// `Indeterminate`; already-indeterminate mutating commands are returned so
    /// a later restart keeps the same fence instead of silently accepting turns.
    pub fn recover_pending_mutating_code_commands(
        &self,
    ) -> Result<Vec<CodeCommandIdentity>, CodeCommandStoreError> {
        // A brand-new session has no command log to recover. Do not create
        // storage here: ordinary turn admission owns that precondition and
        // reports a repairable persistence failure to its caller.
        match fs::metadata(self.events_path()) {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error.into()),
        }
        let _lock = self.acquire_code_workflow_append_lock()?;
        // Cross-process writers may have appended while this store held a stale
        // in-memory cache; rebuild under the lock before deciding recovery.
        self.invalidate_command_status_cache();
        let mut fence_mutations = Vec::new();
        let mut pending_to_fence = Vec::new();
        let statuses = self.all_code_command_statuses()?;
        self.sync_events_log()?;
        for (identity, cached) in statuses {
            if cached.payload_conflict {
                return Err(payload_conflict(&identity));
            }
            if cached.terminal_conflict {
                return Err(CodeCommandStoreError::TerminalConflict {
                    command_id: identity.command_id.clone(),
                });
            }
            match (&cached.intent, &cached.status) {
                (None, None) => {}
                (None, Some(_)) | (Some(_), None) => {
                    return Err(CodeCommandStoreError::TerminalWithoutIntent {
                        command_id: identity.command_id.clone(),
                    });
                }
                (Some(intent), Some(status)) if intent.mutating => match status {
                    CodeCommandStatus::Pending => {
                        // A live owner may still be executing this mutation in
                        // another process. Fencing it here would race the
                        // owner's terminal append into TerminalConflict.
                        if self.live_mutating_owner_exists() {
                            tracing::info!(
                                command_id = %identity.command_id,
                                "skipping pending mutation fence because another live runtime still owns this session"
                            );
                        } else {
                            pending_to_fence.push(identity.clone());
                            fence_mutations.push(identity);
                        }
                    }
                    CodeCommandStatus::Indeterminate { .. } => {
                        fence_mutations.push(identity);
                    }
                    _ => {}
                },
                (Some(_), Some(_)) => {}
            }
        }

        for identity in &pending_to_fence {
            self.append_code_workflow_while_locked(
                CodeWorkflowEventKind::CommandIndeterminateSideEffect {
                    command: identity.clone(),
                    effect: "unknown_mutating_dispatch".to_string(),
                    reason:
                        "runtime stopped after durable intent; manual reconciliation is required"
                            .to_string(),
                },
                true,
            )?;
        }
        if !pending_to_fence.is_empty() {
            self.release_mutating_owner_lease_if_no_pending_mutations()?;
        }
        Ok(fence_mutations)
    }

    /// Read-only inspection for surfaces such as `libra graph`: true when a
    /// mutating command is still Pending with no live owner, or already
    /// Indeterminate. Does not append a fence — callers surface
    /// `indeterminate_side_effect` so operators cannot miss reconciliation.
    ///
    /// The workflow log must fit in `max_bytes`; larger logs fail closed so
    /// graph cannot hide reconciliation behind a partial scan.
    pub fn has_unresolved_mutating_reconciliation_bounded(
        &self,
        max_bytes: u64,
    ) -> Result<bool, CodeCommandStoreError> {
        let path = self.events_path();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.len() > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Code workflow log '{}' is {} bytes, exceeding the bounded reconciliation inspection limit of {max_bytes}; create a projection checkpoint or fence pending mutations before inspecting graph overlays",
                    path.display(),
                    metadata.len()
                ),
            )
            .into());
        }
        // Prefer a fresh rebuild; inspection must not trust a stale cache from
        // another long-lived reader in the same process.
        self.invalidate_command_status_cache();
        let live_owner = self.live_mutating_owner_exists();
        for (identity, cached) in self.all_code_command_statuses()? {
            if cached.payload_conflict {
                return Err(payload_conflict(&identity));
            }
            if cached.terminal_conflict {
                return Err(CodeCommandStoreError::TerminalConflict {
                    command_id: identity.command_id.clone(),
                });
            }
            match (&cached.intent, &cached.status) {
                (None, None) => {}
                (None, Some(_)) | (Some(_), None) => {
                    return Err(CodeCommandStoreError::TerminalWithoutIntent {
                        command_id: identity.command_id.clone(),
                    });
                }
                (Some(intent), Some(status)) if intent.mutating => match status {
                    CodeCommandStatus::Pending if !live_owner => return Ok(true),
                    CodeCommandStatus::Indeterminate { .. } => return Ok(true),
                    _ => {}
                },
                (Some(_), Some(_)) => {}
            }
        }
        Ok(false)
    }

    pub fn complete_code_command_success(
        &self,
        identity: &CodeCommandIdentity,
        summary: impl Into<String>,
    ) -> Result<CodeCommandStatus, CodeCommandStoreError> {
        let summary = summary.into();
        self.finish_code_command(
            identity,
            CodeCommandStatus::Succeeded {
                summary: summary.clone(),
            },
            CodeWorkflowEventKind::CommandTerminalSuccess {
                command: identity.clone(),
                summary,
            },
        )
    }

    /// Terminalize a command and record an interaction resolution under one
    /// append lock / fsync so a crash cannot leave `Succeeded` without
    /// `InteractionResolved` (or the reverse).
    pub fn complete_code_command_success_with_interaction_resolved(
        &self,
        identity: &CodeCommandIdentity,
        summary: impl Into<String>,
        interaction_id: impl Into<String>,
        resolution: impl Into<String>,
    ) -> Result<CodeCommandStatus, CodeCommandStoreError> {
        self.complete_code_command_success_with_interaction_resolutions(
            identity,
            summary,
            &[(interaction_id.into(), resolution.into())],
        )
    }

    /// Terminalize a command and record every interaction resolution in one
    /// `CommandTerminalSuccessWithInteractionResolved` row. The final/current
    /// gate remains in the legacy primary fields and earlier responses use the
    /// additive vector, making command terminal + full audit crash-atomic.
    pub fn complete_code_command_success_with_interaction_resolutions(
        &self,
        identity: &CodeCommandIdentity,
        summary: impl Into<String>,
        resolutions: &[(String, String)],
    ) -> Result<CodeCommandStatus, CodeCommandStoreError> {
        self.complete_code_command_success_with_interaction_resolutions_and_intent_revision(
            identity,
            summary,
            resolutions,
            None,
        )
    }

    /// Terminalize a command with its ordered interaction audit and optional
    /// crash-safe IntentSpec Modify sidecar binding in one row/fsync.
    pub fn complete_code_command_success_with_interaction_resolutions_and_intent_revision(
        &self,
        identity: &CodeCommandIdentity,
        summary: impl Into<String>,
        resolutions: &[(String, String)],
        intent_revision: Option<&IntentRevisionRecovery>,
    ) -> Result<CodeCommandStatus, CodeCommandStoreError> {
        if resolutions.is_empty() {
            if intent_revision.is_some() {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
            return self.complete_code_command_success(identity, summary);
        }
        if let Some(intent_revision) = intent_revision {
            let (primary_interaction_id, primary_resolution) = &resolutions[resolutions.len() - 1];
            if primary_resolution != "modify"
                || intent_revision.interaction_id != *primary_interaction_id
                || intent_revision.interaction_id.trim().is_empty()
                || !is_canonical_intent_revision_digest(&intent_revision.sidecar_digest)
            {
                return Err(CodeCommandStoreError::InvalidIntent);
            }
        }
        let summary = summary.into();
        let target = CodeCommandStatus::Succeeded {
            summary: summary.clone(),
        };
        if !identity.is_complete() {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        fs::create_dir_all(&self.session_root)?;
        let _lock = self.acquire_code_workflow_append_lock()?;
        self.invalidate_command_status_cache();
        let Some((intent, status)) = self.code_command_status(identity)? else {
            return Err(CodeCommandStoreError::MissingIntent {
                command_id: identity.command_id.clone(),
            });
        };
        let (last_id, last_resolution) = &resolutions[resolutions.len() - 1];
        let terminal_event = CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            command: identity.clone(),
            summary,
            interaction_id: last_id.clone(),
            resolution: last_resolution.clone(),
            prior_interaction_resolutions: resolutions[..resolutions.len() - 1].to_vec(),
            intent_revision: intent_revision.cloned(),
        };
        match status {
            CodeCommandStatus::Pending => {
                let replay = if intent_revision.is_some() {
                    self.load_intent_revision_workflow_replay()?
                } else {
                    self.load_code_workflow_replay()?
                };
                let lineage =
                    exact_intent_revision_lineage_intent_id(&replay.events, last_id, identity)
                        .map_err(|()| CodeCommandStoreError::InvalidIntent)?;
                if lineage.is_some()
                    && !has_exact_intent_revision_source_intent(
                        &replay,
                        replay.events.len(),
                        identity,
                        last_id,
                    )
                {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                if last_resolution == "modify" && lineage.is_some() && intent_revision.is_none() {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                if intent_revision.is_some() && lineage.is_none() {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                self.append_code_workflow_while_locked(terminal_event, true)?;
                if intent.mutating {
                    self.release_mutating_owner_lease_if_no_pending_mutations()?;
                }
                Ok(target)
            }
            existing if existing == target => {
                let replay = if intent_revision.is_some() {
                    self.load_intent_revision_workflow_replay()?
                } else {
                    self.load_code_workflow_replay()?
                };
                // Pair membership elsewhere in the workflow log cannot prove
                // this command's exact terminal payload. Match the complete
                // combined row, including ordered prior resolutions.
                let terminal_index = Self::require_exact_terminal_event_in_replay(
                    &replay,
                    identity,
                    &terminal_event,
                )?;
                let lineage = exact_intent_revision_lineage_intent_id(
                    &replay.events[..terminal_index],
                    last_id,
                    identity,
                )
                .map_err(|()| CodeCommandStoreError::InvalidIntent)?;
                if lineage.is_some()
                    && !has_exact_intent_revision_source_intent(
                        &replay,
                        terminal_index,
                        identity,
                        last_id,
                    )
                {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                if let Some(intent_revision) = intent_revision
                    && (lineage.is_none() || intent_revision.interaction_id != *last_id)
                {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                // A previous terminal append can be visible after a
                // post-write/pre-sync failure. Re-sync the exact terminal row
                // before acknowledging the retry as durable success.
                self.sync_events_log()?;
                Ok(existing)
            }
            _ => Err(CodeCommandStoreError::TerminalConflict {
                command_id: identity.command_id.clone(),
            }),
        }
    }

    /// When an IntentSpec review marker identifies a Phase 0 turn, that
    /// pending mutating command is completed as success (draft at rest) while
    /// every other pending mutation is fenced as indeterminate. Without a
    /// Phase 0 turn id, falls back to ordinary recovery fencing.
    pub fn recover_pending_mutating_code_commands_for_intent_review(
        &self,
        phase0_turn_id: Option<&str>,
    ) -> Result<Vec<CodeCommandIdentity>, CodeCommandStoreError> {
        Ok(self
            .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
                phase0_turn_id,
                None,
                None,
            )?
            .fenced)
    }

    /// Recover review-owned mutations while preserving one exact, seed-backed
    /// Phase 1 command that is proven not to have crossed its formal-write
    /// boundary. The expected intent is compared under the append lock and
    /// its mutating-owner lease is re-claimed before the caller can reattach.
    pub fn recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
        &self,
        phase0_turn_id: Option<&str>,
        phase1_prewrite_intent: Option<&CodeCommandIntent>,
        intent_revision_consumer: Option<&IntentRevisionConsumption>,
    ) -> Result<PendingMutationRecoveryOutcome, CodeCommandStoreError> {
        let phase0_turn_id = phase0_turn_id.filter(|id| !id.is_empty());
        if let Some(intent) = phase1_prewrite_intent
            && (!intent.is_valid() || !intent.mutating)
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        if let Some(consumption) = intent_revision_consumer
            && (!intent_revision_consumption_claim_is_valid(&consumption.claim)
                || consumption.consumer_intent_sequence == 0)
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        if intent_revision_consumer.is_some() && phase1_prewrite_intent.is_some() {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        match fs::metadata(self.events_path()) {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                if intent_revision_consumer.is_some() {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                return Ok(PendingMutationRecoveryOutcome::default());
            }
            Err(error) => return Err(error.into()),
        }
        let _lock = self.acquire_code_workflow_append_lock()?;
        self.invalidate_command_status_cache();
        let mut fence_mutations = Vec::new();
        let mut pending_to_complete = Vec::new();
        let mut pending_revision_consumers_to_fail = Vec::new();
        let mut pending_revision_cancels_to_complete = Vec::new();
        let mut pending_to_fence = Vec::new();
        let mut phase1_prewrite_reattached = false;
        let live_owner = self.live_mutating_owner_exists();
        let statuses = self.all_code_command_statuses()?;
        self.sync_events_log()?;
        let revision_replay = self.load_intent_revision_workflow_replay()?;
        let revision_index = validated_intent_revision_consumption_receipts(&revision_replay)?;
        let exact_revision_consumer_recovery = intent_revision_consumer
            .map(|consumption| {
                let (_, receipt_index) = exact_intent_revision_consumption_receipt_indices(
                    &revision_replay,
                    consumption,
                )?;
                let receipt_committed = receipt_index.is_some();
                let replacement_review_committed =
                    intent_revision_consumer_has_replacement_review(&revision_replay, consumption)?;
                if !replacement_review_committed
                    && intent_revision_consumer_has_any_replacement_review(
                        &revision_replay,
                        consumption,
                    )?
                {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                let replacement_review_open = intent_revision_consumer_has_open_replacement_review(
                    &revision_replay,
                    consumption,
                )?;
                if let Some(phase0_turn_id) = phase0_turn_id
                    && (!replacement_review_open
                        || phase0_turn_id != consumption.claim.consumer_intent.identity.command_id)
                {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                let canonical_cancel = intent_revision_cancel_consumer_is_canonical(
                    &consumption.claim.consumer_intent,
                );
                if receipt_committed && !canonical_cancel && !replacement_review_committed {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                if canonical_cancel && replacement_review_committed {
                    return Err(CodeCommandStoreError::InvalidIntent);
                }
                // A Consuming/Claiming envelope proves the exact command was
                // selected before its start gate opened. Therefore the latest
                // canonical pre-mutation cancellation is as recoverable as an
                // older retry attempt and must not fence the revision. An
                // exact receipt is accepted only for the fixed no-provider
                // `/intent cancel` control, whose entire effect is already
                // proven by that receipt.
                let attempts = revision_index.intent_revision_consumer_attempt_statuses(
                    consumption,
                    true,
                    receipt_committed,
                )?;
                if replacement_review_committed && !receipt_committed {
                    self.append_code_workflow_while_locked(
                        intent_revision_consumption_receipt_event(consumption),
                        true,
                    )?;
                }
                Ok((
                    attempts,
                    receipt_committed || replacement_review_committed,
                    canonical_cancel,
                    replacement_review_committed,
                    replacement_review_open,
                ))
            })
            .transpose()?;
        let exact_revision_consumer =
            intent_revision_consumer.map(|consumption| &consumption.claim.consumer_intent);
        let exact_revision_consumer_statuses = exact_revision_consumer_recovery
            .as_ref()
            .map(|(attempts, ..)| attempts.iter().cloned().collect::<HashMap<_, _>>())
            .unwrap_or_default();
        let exact_revision_cancel_receipt_committed = exact_revision_consumer_recovery
            .as_ref()
            .is_some_and(|(_, receipt_committed, canonical_cancel, ..)| {
                *receipt_committed && *canonical_cancel
            });
        let exact_revision_replacement_review_committed =
            exact_revision_consumer_recovery.as_ref().is_some_and(
                |(_, _, _, replacement_review_committed, _)| *replacement_review_committed,
            );
        let exact_revision_replacement_review_open = exact_revision_consumer_recovery
            .as_ref()
            .is_some_and(|(_, _, _, _, replacement_review_open)| *replacement_review_open);
        let mut saw_exact_revision_consumer = false;
        for (identity, cached) in statuses {
            if cached.payload_conflict {
                return Err(payload_conflict(&identity));
            }
            if cached.terminal_conflict {
                return Err(CodeCommandStoreError::TerminalConflict {
                    command_id: identity.command_id.clone(),
                });
            }
            match (&cached.intent, &cached.status) {
                (Some(intent), Some(status)) if intent.mutating => {
                    if exact_revision_consumer.is_some_and(|expected| expected == intent) {
                        saw_exact_revision_consumer = true;
                        let Some(expected_status) =
                            exact_revision_consumer_statuses.get(&intent.identity)
                        else {
                            return Err(CodeCommandStoreError::InvalidIntent);
                        };
                        if status != expected_status {
                            return Err(CodeCommandStoreError::TerminalConflict {
                                command_id: identity.command_id.clone(),
                            });
                        }
                        if exact_revision_replacement_review_committed {
                            match status {
                                CodeCommandStatus::Pending if !live_owner => {
                                    pending_to_complete.push(identity.clone());
                                }
                                CodeCommandStatus::Succeeded { .. }
                                | CodeCommandStatus::Indeterminate { .. } => {}
                                _ => return Err(CodeCommandStoreError::InvalidIntent),
                            }
                            continue;
                        }
                        if exact_revision_cancel_receipt_committed {
                            match status {
                                CodeCommandStatus::Pending if !live_owner => {
                                    pending_revision_cancels_to_complete.push(identity.clone());
                                }
                                CodeCommandStatus::Succeeded { .. }
                                | CodeCommandStatus::Indeterminate { .. } => {}
                                _ => return Err(CodeCommandStoreError::InvalidIntent),
                            }
                            continue;
                        }
                        if matches!(status, CodeCommandStatus::Pending) {
                            if live_owner {
                                return Err(CodeCommandStoreError::InvalidIntent);
                            }
                            pending_revision_consumers_to_fail.push(identity.clone());
                        }
                        continue;
                    }
                    if let Some(expected_status) =
                        exact_revision_consumer_statuses.get(&intent.identity)
                    {
                        if status != expected_status {
                            return Err(CodeCommandStoreError::TerminalConflict {
                                command_id: identity.command_id.clone(),
                            });
                        }
                        continue;
                    }
                    if let Some(expected_status) =
                        revision_index.committed_consumer_status(&intent.identity)
                    {
                        if status != expected_status {
                            return Err(CodeCommandStoreError::TerminalConflict {
                                command_id: identity.command_id.clone(),
                            });
                        }
                        match status {
                            CodeCommandStatus::Pending if !live_owner => {
                                pending_to_complete.push(identity.clone());
                            }
                            CodeCommandStatus::Succeeded { .. }
                            | CodeCommandStatus::Failed { .. }
                            | CodeCommandStatus::Indeterminate { .. } => {}
                            CodeCommandStatus::Pending => {
                                return Err(CodeCommandStoreError::InvalidIntent);
                            }
                        }
                        continue;
                    }
                    match status {
                        CodeCommandStatus::Pending if !live_owner => {
                            if phase0_turn_id
                                .is_some_and(|phase0_turn_id| identity.command_id == phase0_turn_id)
                            {
                                pending_to_complete.push(identity.clone());
                            } else if phase1_prewrite_intent
                                .is_some_and(|expected| expected == intent)
                            {
                                phase1_prewrite_reattached = true;
                            } else {
                                pending_to_fence.push(identity.clone());
                                fence_mutations.push(identity);
                            }
                        }
                        CodeCommandStatus::Indeterminate { .. } => {
                            fence_mutations.push(identity);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if exact_revision_consumer.is_some() && !saw_exact_revision_consumer {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        for identity in &pending_to_complete {
            self.append_code_workflow_while_locked(
                CodeWorkflowEventKind::CommandTerminalSuccess {
                    command: identity.clone(),
                    summary: "IntentSpec draft durable; awaiting review confirmation".to_string(),
                },
                true,
            )?;
        }
        for identity in &pending_revision_consumers_to_fail {
            self.append_code_workflow_while_locked(
                CodeWorkflowEventKind::CommandTerminalFailure {
                    command: identity.clone(),
                    reason: INTENT_REVISION_CONSUMER_RECOVERY_FAILURE_REASON.to_string(),
                    interaction_resolutions: Vec::new(),
                    retry_intent_review: None,
                },
                true,
            )?;
        }
        for identity in &pending_revision_cancels_to_complete {
            self.append_code_workflow_while_locked(
                CodeWorkflowEventKind::CommandTerminalSuccess {
                    command: identity.clone(),
                    summary: "IntentSpec revision mode cancelled".to_string(),
                },
                true,
            )?;
        }
        for identity in &pending_to_fence {
            self.append_code_workflow_while_locked(
                CodeWorkflowEventKind::CommandIndeterminateSideEffect {
                    command: identity.clone(),
                    effect: "unknown_mutating_dispatch".to_string(),
                    reason:
                        "runtime stopped after durable intent; manual reconciliation is required"
                            .to_string(),
                },
                true,
            )?;
        }
        if !pending_to_complete.is_empty()
            || !pending_revision_consumers_to_fail.is_empty()
            || !pending_revision_cancels_to_complete.is_empty()
            || !pending_to_fence.is_empty()
        {
            self.release_mutating_owner_lease_if_no_pending_mutations()?;
        }
        if phase1_prewrite_reattached {
            self.claim_mutating_owner_lease()?;
        }
        Ok(PendingMutationRecoveryOutcome {
            fenced: fence_mutations,
            phase1_prewrite_reattached,
            intent_revision_consumer_healed: exact_revision_consumer.is_some()
                && !exact_revision_cancel_receipt_committed
                && !exact_revision_replacement_review_committed,
            intent_revision_cancel_healed: exact_revision_cancel_receipt_committed,
            intent_revision_replacement_review_healed: exact_revision_replacement_review_committed
                && exact_revision_replacement_review_open,
        })
    }

    pub fn complete_code_command_failure(
        &self,
        identity: &CodeCommandIdentity,
        reason: impl Into<String>,
    ) -> Result<CodeCommandStatus, CodeCommandStoreError> {
        let reason = reason.into();
        self.finish_code_command(
            identity,
            CodeCommandStatus::Failed {
                reason: reason.clone(),
            },
            CodeWorkflowEventKind::CommandTerminalFailure {
                command: identity.clone(),
                reason,
                interaction_resolutions: Vec::new(),
                retry_intent_review: None,
            },
        )
    }

    /// Terminalize a failed/cancelled command together with every interaction
    /// response already delivered during that command. One additive terminal
    /// row makes the audit evidence crash-atomic with the failure status.
    pub fn complete_code_command_failure_with_interaction_resolutions(
        &self,
        identity: &CodeCommandIdentity,
        reason: impl Into<String>,
        interaction_resolutions: &[(String, String)],
    ) -> Result<CodeCommandStatus, CodeCommandStoreError> {
        self.complete_code_command_failure_with_interaction_resolutions_and_retry_intent_review(
            identity,
            reason,
            interaction_resolutions,
            None,
        )
    }

    /// Terminalize a failed command together with its delivered interaction
    /// audit and an optional Phase 1 retry gate in one append/fsync.
    pub fn complete_code_command_failure_with_interaction_resolutions_and_retry_intent_review(
        &self,
        identity: &CodeCommandIdentity,
        reason: impl Into<String>,
        interaction_resolutions: &[(String, String)],
        retry_intent_review: Option<&Phase1RetryIntentReview>,
    ) -> Result<CodeCommandStatus, CodeCommandStoreError> {
        let reason = reason.into();
        if interaction_resolutions
            .iter()
            .any(|(interaction_id, resolution)| {
                interaction_id.trim().is_empty() || resolution.trim().is_empty()
            })
        {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        if retry_intent_review.is_some_and(|retry| {
            retry.interaction_id.trim().is_empty()
                || retry.intent_id.trim().is_empty()
                || retry.intent_spec_id.trim().is_empty()
                || retry.source_interaction_id.trim().is_empty()
                || retry.interaction_id == retry.source_interaction_id
                || !retry.source_resolution.eq_ignore_ascii_case("confirm")
                || retry.source_phase1_turn_id != identity.command_id
                || retry.start_seed_digest.len() != 64
                || !retry
                    .start_seed_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        self.finish_code_command(
            identity,
            CodeCommandStatus::Failed {
                reason: reason.clone(),
            },
            CodeWorkflowEventKind::CommandTerminalFailure {
                command: identity.clone(),
                reason,
                interaction_resolutions: interaction_resolutions.to_vec(),
                retry_intent_review: retry_intent_review.cloned(),
            },
        )
    }

    pub fn mark_code_command_indeterminate(
        &self,
        identity: &CodeCommandIdentity,
        effect: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<CodeCommandStatus, CodeCommandStoreError> {
        let effect = effect.into();
        let reason = reason.into();
        self.finish_code_command(
            identity,
            CodeCommandStatus::Indeterminate {
                effect: effect.clone(),
                reason: reason.clone(),
            },
            CodeWorkflowEventKind::CommandIndeterminateSideEffect {
                command: identity.clone(),
                effect,
                reason,
            },
        )
    }

    fn finish_code_command(
        &self,
        identity: &CodeCommandIdentity,
        target: CodeCommandStatus,
        event: CodeWorkflowEventKind,
    ) -> Result<CodeCommandStatus, CodeCommandStoreError> {
        if !identity.is_complete() {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        fs::create_dir_all(&self.session_root)?;
        let _lock = self.acquire_code_workflow_append_lock()?;
        // Another SessionJsonlStore may have recorded a terminal state while this
        // instance still believed the command was Pending; refresh under the lock.
        self.invalidate_command_status_cache();
        let Some((intent, status)) = self.code_command_status(identity)? else {
            return Err(CodeCommandStoreError::MissingIntent {
                command_id: identity.command_id.clone(),
            });
        };
        match status {
            CodeCommandStatus::Pending => {
                self.append_code_workflow_while_locked(event, true)?;
                if intent.mutating {
                    self.release_mutating_owner_lease_if_no_pending_mutations()?;
                }
                Ok(target)
            }
            existing if existing == target => {
                // Status equality alone is insufficient: failure rows also
                // carry the ordered interaction audit, and a plain success is
                // not interchangeable with a combined success. Require the
                // complete proposed terminal event to match every terminal row
                // for this command before treating the retry as idempotent.
                self.require_exact_terminal_event(identity, &event)?;
                // Seeing the exact terminal row does not prove its earlier
                // sync succeeded. Re-sync before returning an idempotent ACK.
                self.sync_events_log()?;
                Ok(existing)
            }
            _ => Err(CodeCommandStoreError::TerminalConflict {
                command_id: identity.command_id.clone(),
            }),
        }
    }

    fn require_exact_terminal_event(
        &self,
        identity: &CodeCommandIdentity,
        expected: &CodeWorkflowEventKind,
    ) -> Result<(), CodeCommandStoreError> {
        let replay = self.load_code_workflow_replay()?;
        Self::require_exact_terminal_event_in_replay(&replay, identity, expected).map(|_| ())
    }

    fn require_exact_terminal_event_in_replay(
        replay: &CodeWorkflowReplay,
        identity: &CodeCommandIdentity,
        expected: &CodeWorkflowEventKind,
    ) -> Result<usize, CodeCommandStoreError> {
        let mut exact_index = None;
        let mut saw_conflict = false;
        for (event_index, durable) in replay.events.iter().enumerate() {
            let command = match &durable.event {
                CodeWorkflowEventKind::CommandTerminalSuccess { command, .. }
                | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    command,
                    ..
                }
                | CodeWorkflowEventKind::CommandTerminalFailure { command, .. }
                | CodeWorkflowEventKind::CommandIndeterminateSideEffect { command, .. } => {
                    Some(command)
                }
                _ => None,
            };
            if command == Some(identity) {
                if &durable.event == expected {
                    if exact_index.replace(event_index).is_some() {
                        saw_conflict = true;
                    }
                } else {
                    saw_conflict = true;
                }
            }
        }
        if exact_index.is_none() || saw_conflict {
            return Err(CodeCommandStoreError::TerminalConflict {
                command_id: identity.command_id.clone(),
            });
        }
        Ok(exact_index.unwrap_or_default())
    }

    fn code_command_status(
        &self,
        identity: &CodeCommandIdentity,
    ) -> Result<Option<(CodeCommandIntent, CodeCommandStatus)>, CodeCommandStoreError> {
        let cached = self
            .all_code_command_statuses()?
            .remove(identity)
            .unwrap_or_default();
        if cached.payload_conflict {
            return Err(payload_conflict(identity));
        }
        if cached.terminal_conflict {
            return Err(CodeCommandStoreError::TerminalConflict {
                command_id: identity.command_id.clone(),
            });
        }
        match (cached.intent, cached.status) {
            (Some(intent), Some(status)) => Ok(Some((intent, status))),
            (None, None) => Ok(None),
            (None, Some(_)) => Err(CodeCommandStoreError::TerminalWithoutIntent {
                command_id: identity.command_id.clone(),
            }),
            (Some(_), None) => Err(CodeCommandStoreError::TerminalWithoutIntent {
                command_id: identity.command_id.clone(),
            }),
        }
    }

    pub fn code_command_intent_status(
        &self,
        identity: &CodeCommandIdentity,
    ) -> Result<Option<(CodeCommandIntent, CodeCommandStatus)>, CodeCommandStoreError> {
        if !identity.is_complete() {
            return Err(CodeCommandStoreError::InvalidIntent);
        }
        let _lock = self.acquire_code_workflow_append_lock()?;
        self.invalidate_command_status_cache();
        let status = self.code_command_status(identity)?;
        if status.is_some() {
            self.sync_events_log()?;
        }
        Ok(status)
    }

    fn all_code_command_statuses(
        &self,
    ) -> Result<HashMap<CodeCommandIdentity, CachedCodeCommand>, CodeCommandStoreError> {
        let mut cache = self
            .command_status_cache
            .lock()
            .map_err(|_| io::Error::other("session command status cache lock was poisoned"))?;
        if cache.is_none() {
            let replay = self.load_code_workflow_replay()?;
            let mut rebuilt = HashMap::new();
            for event in replay.events {
                apply_command_status_event(&mut rebuilt, &event.event);
            }
            *cache = Some(rebuilt);
        }
        Ok(cache.clone().unwrap_or_default())
    }

    /// Drop the in-memory command-status cache. Call after acquiring the
    /// cross-process append lock so subsequent lookups rebuild from the durable
    /// log instead of a stale same-process snapshot.
    fn invalidate_command_status_cache(&self) {
        let Ok(mut cache) = self.command_status_cache.lock() else {
            tracing::warn!(
                "session command status cache lock was poisoned; leaving None for rebuild"
            );
            return;
        };
        *cache = None;
    }

    fn mutating_owner_lock_path(&self) -> PathBuf {
        self.session_root.join("mutating_owner.lock")
    }

    fn claim_mutating_owner_lease(&self) -> io::Result<()> {
        if self.foreign_live_mutating_owner_exists() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "session '{}' still has a live mutating runtime owner; refuse concurrent mutating admission",
                    self.session_root.display()
                ),
            ));
        }
        let path = self.mutating_owner_lock_path();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        let owner = current_process_owner_identity();
        writeln!(file, "pid={}", owner.pid)?;
        if let Some(starttime) = owner.starttime {
            writeln!(file, "starttime={starttime}")?;
        }
        if let Some(boot_id) = owner.boot_id.as_deref() {
            writeln!(file, "boot_id={boot_id}")?;
        }
        Ok(())
    }

    fn release_mutating_owner_lease_if_no_pending_mutations(
        &self,
    ) -> Result<(), CodeCommandStoreError> {
        for (_identity, cached) in self.all_code_command_statuses()? {
            if cached.intent.as_ref().is_some_and(|intent| intent.mutating)
                && matches!(cached.status, Some(CodeCommandStatus::Pending))
            {
                return Ok(());
            }
        }
        self.release_mutating_owner_lease();
        Ok(())
    }

    fn release_mutating_owner_lease(&self) {
        let path = self.mutating_owner_lock_path();
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "failed to release mutating owner lease"
            );
        }
    }

    fn foreign_live_mutating_owner_exists(&self) -> bool {
        let path = self.mutating_owner_lock_path();
        let Ok(content) = fs::read_to_string(&path) else {
            return false;
        };
        let Some(pid) = content.lines().find_map(|line| {
            line.strip_prefix("pid=")
                .and_then(|value| value.trim().parse::<u32>().ok())
        }) else {
            return false;
        };
        if pid == std::process::id() {
            return false;
        }
        let recorded_starttime = content.lines().find_map(|line| {
            line.strip_prefix("starttime=")
                .and_then(|value| value.trim().parse::<u64>().ok())
        });
        let recorded_boot_id = content.lines().find_map(|line| {
            line.strip_prefix("boot_id=")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        });
        process_appears_alive(pid, &path, recorded_starttime, recorded_boot_id.as_deref())
    }

    fn live_mutating_owner_exists(&self) -> bool {
        self.foreign_live_mutating_owner_exists()
    }

    fn update_command_status_cache(&self, event: &CodeWorkflowEventKind) {
        let Ok(mut cache) = self.command_status_cache.lock() else {
            tracing::warn!(
                "session command status cache lock was poisoned; rebuilding on next lookup"
            );
            return;
        };
        if let Some(cache) = cache.as_mut() {
            apply_command_status_event(cache, event);
        }
    }

    fn acquire_code_workflow_append_lock(&self) -> io::Result<CodeWorkflowAppendLock> {
        self.acquire_code_workflow_append_lock_with_timeout(CODE_WORKFLOW_APPEND_LOCK_TIMEOUT)
    }

    fn acquire_code_workflow_append_lock_with_timeout(
        &self,
        timeout: Duration,
    ) -> io::Result<CodeWorkflowAppendLock> {
        let path = self.session_root.join("events.code-workflow.append.lock");
        let started = Instant::now();
        'open_current_inode: loop {
            let file = open_code_workflow_append_lock_file(&path)?;
            loop {
                match file.try_lock() {
                    Ok(()) => {
                        if !code_workflow_append_lock_path_matches_file(&path, &file)? {
                            drop(file);
                            continue 'open_current_inode;
                        }
                        return Ok(CodeWorkflowAppendLock {
                            _path: path,
                            _file: file,
                        });
                    }
                    Err(std::fs::TryLockError::WouldBlock) => {
                        if started.elapsed() >= timeout {
                            return Err(io::Error::new(
                                io::ErrorKind::WouldBlock,
                                format!(
                                    "timed out waiting for Code workflow append lock '{}'",
                                    path.display()
                                ),
                            ));
                        }
                        thread::sleep(CODE_WORKFLOW_APPEND_LOCK_POLL_INTERVAL);
                    }
                    Err(std::fs::TryLockError::Error(error)) => {
                        return Err(io::Error::new(
                            error.kind(),
                            format!(
                                "failed to acquire Code workflow append lock '{}': {error}",
                                path.display()
                            ),
                        ));
                    }
                }
            }
        }
    }

    pub fn load_state(&self) -> io::Result<Option<SessionState>> {
        let mut state = None;
        for event in self.load_events()? {
            event.apply_to(&mut state);
        }
        Ok(state)
    }

    pub fn load_events(&self) -> io::Result<Vec<SessionEvent>> {
        let path = self.events_path();
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(io::Error::new(
                    err.kind(),
                    format!(
                        "failed to read session event log '{}': {err}",
                        path.display()
                    ),
                ));
            }
        };

        parse_session_events_content(&path, &content)
    }

    pub fn load_context_replay(&self) -> io::Result<SessionContextReplay> {
        let mut replay = SessionContextReplay::default();
        for event in self.load_events()? {
            match event {
                SessionEvent::ContextFrame(frame) => replay.frames.push(frame),
                SessionEvent::CompactionEvent(compaction) => {
                    replay.compactions.push(compaction);
                }
                SessionEvent::SessionSnapshot(_) => {}
                SessionEvent::MemoryAnchor(_) => {}
                SessionEvent::AgentRun(_) => {}
                SessionEvent::ToolCall(_) => {}
                SessionEvent::ToolResult(_) => {}
                // OC-Phase 6 P6.1: Goal envelopes do not contribute to
                // `SessionContextReplay`. Goal state is replayed by
                // `crate::internal::ai::goal::state::replay`, called by
                // the supervisor (P6.3). Listed explicitly so an
                // exhaustiveness regression surfaces here.
                SessionEvent::Goal(_) => {}
                // OC-Phase 4 ArtifactLedger (v0.17.810): AiArtifact
                // envelopes do not contribute to context replay —
                // they're a Phase 3/4 SeaORM-write projection that
                // a future artefact-ledger replay reads through a
                // separate projection.
                SessionEvent::AiArtifact(_) => {}
                SessionEvent::CodeWorkflow(_) => {}
            }
        }
        Ok(replay)
    }

    pub fn load_memory_anchors(&self) -> io::Result<MemoryAnchorReplay> {
        let mut replay = MemoryAnchorReplay::default();
        for event in self.load_events()? {
            if let SessionEvent::MemoryAnchor(anchor) = event {
                replay.apply_event(anchor);
            }
        }
        Ok(replay)
    }

    pub fn load_ai_artifacts(&self) -> io::Result<Vec<AiArtifactEvent>> {
        let mut artifacts = Vec::new();
        for event in self.load_events()? {
            if let SessionEvent::AiArtifact(artifact) = event {
                artifacts.push(artifact);
            }
        }
        Ok(artifacts)
    }

    pub fn has_events(&self) -> io::Result<bool> {
        let path = self.events_path();
        match fs::metadata(&path) {
            Ok(metadata) => Ok(metadata.len() > 0),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(io::Error::new(
                err.kind(),
                format!(
                    "failed to inspect session event log '{}': {err}",
                    path.display()
                ),
            )),
        }
    }
}

fn open_code_workflow_append_lock_file(path: &Path) -> io::Result<fs::File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Code workflow append lock '{}' must be a regular file, not a symlink or special entry",
                    path.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "failed to open regular Code workflow append lock '{}': {error}",
                path.display()
            ),
        )
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Code workflow append lock '{}' is not a regular file",
                path.display()
            ),
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(current) if !current.file_type().is_symlink() && current.is_file() => Ok(file),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Code workflow append lock '{}' changed into a symlink or special entry while it was opened",
                path.display()
            ),
        )),
        Err(error) => Err(error),
    }
}

fn code_workflow_replay_from_events(
    events: impl IntoIterator<Item = SessionEvent>,
    after_sequence: u64,
) -> io::Result<CodeWorkflowReplay> {
    let mut replay = CodeWorkflowReplay::default();
    let mut seen_event_ids = HashSet::new();
    let mut previous_sequence = (after_sequence > 0).then_some(after_sequence);

    for event in events {
        let SessionEvent::CodeWorkflow(workflow_event) = event else {
            continue;
        };
        if workflow_event.sequence <= after_sequence {
            continue;
        }
        if !seen_event_ids.insert(workflow_event.event_id) {
            tracing::warn!(
                event_id = %workflow_event.event_id,
                sequence = workflow_event.sequence,
                "skipping duplicate Code workflow event id"
            );
            continue;
        }
        if workflow_event.sequence == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Code workflow event '{}' has invalid zero sequence",
                    workflow_event.event_id
                ),
            ));
        }

        if let Some(previous) = previous_sequence {
            let expected = previous.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Code workflow sequence overflowed u64",
                )
            })?;
            if workflow_event.sequence < expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Code workflow event '{}' sequence {} is not after prior sequence {}",
                        workflow_event.event_id, workflow_event.sequence, previous
                    ),
                ));
            }
            if workflow_event.sequence > expected {
                replay.gaps.push(CodeWorkflowSequenceGap {
                    after: previous,
                    before: workflow_event.sequence,
                });
            }
        } else if workflow_event.sequence > 1 {
            replay.gaps.push(CodeWorkflowSequenceGap {
                after: 0,
                before: workflow_event.sequence,
            });
        }

        previous_sequence = Some(workflow_event.sequence);
        replay.events.push(workflow_event);
    }
    Ok(replay)
}

/// Parse a bounded workflow suffix by walking JSONL records from the end of
/// the already-read byte window. Stops at the durable cursor, an overflow of
/// `max_events + 1`, or the start of the window — so 10k small rows inside
/// 8 MiB are not fully deserialized for a one-event resume.
fn parse_code_workflow_suffix_from_window(
    path: &Path,
    content: &str,
    after_sequence: u64,
    max_events: usize,
) -> io::Result<(Vec<SessionEvent>, Option<u64>)> {
    let ends_with_newline = content.ends_with('\n');
    let bytes = content.as_bytes();
    let overflow_at = max_events.saturating_add(1);
    let mut end = bytes.len();
    let mut newest_first = Vec::new();
    let mut window_tip_sequence = None;
    let mut first = true;
    let mut records_from_end = 0usize;

    while end > 0 {
        if bytes[end - 1] == b'\n' {
            end -= 1;
            if end == 0 {
                break;
            }
        }
        let start = content[..end].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line = &content[start..end];
        let is_trailing_incomplete = first && !ends_with_newline;
        first = false;
        end = start;
        if line.trim().is_empty() {
            continue;
        }
        records_from_end = records_from_end.saturating_add(1);
        CODE_WORKFLOW_REPLAY_PARSE_VISITS.with(|cell| cell.set(cell.get().saturating_add(1)));
        let value = match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(error) if is_trailing_incomplete => {
                tracing::warn!(
                    path = %path.display(),
                    tail_record = records_from_end,
                    error = %error,
                    "stopping session JSONL replay at malformed trailing line"
                );
                continue;
            }
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "malformed complete line in session event log '{}' near tail record {records_from_end}: {error}",
                        path.display()
                    ),
                ));
            }
        };
        match parse_session_event_value(value) {
            Ok(Some(SessionEvent::CodeWorkflow(workflow_event))) => {
                if window_tip_sequence.is_none() {
                    window_tip_sequence = Some(workflow_event.sequence);
                }
                if workflow_event.sequence <= after_sequence {
                    break;
                }
                newest_first.push(SessionEvent::CodeWorkflow(workflow_event));
                if newest_first.len() >= overflow_at {
                    break;
                }
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "failed to decode session event log '{}' near tail record {records_from_end}: {error}",
                        path.display()
                    ),
                ));
            }
        }
    }

    newest_first.reverse();
    Ok((newest_first, window_tip_sequence))
}

fn parse_session_events_content(path: &Path, content: &str) -> io::Result<Vec<SessionEvent>> {
    let lines: Vec<&str> = content.lines().collect();
    let ends_with_newline = content.ends_with('\n');
    let mut events = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let line_number = line_index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let value = match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(error) if line_index + 1 == lines.len() && !ends_with_newline => {
                tracing::warn!(
                    path = %path.display(),
                    line = line_number,
                    error = %error,
                    "stopping session JSONL replay at malformed trailing line"
                );
                break;
            }
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "malformed complete line in session event log '{}' line {line_number}: {error}",
                        path.display()
                    ),
                ));
            }
        };
        match parse_session_event_value(value) {
            Ok(Some(event)) => events.push(event),
            Ok(None) => {}
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "failed to decode session event log '{}' line {line_number}: {error}",
                        path.display()
                    ),
                ));
            }
        }
    }
    Ok(events)
}

fn child_dir_name(child_id: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, child_id.as_bytes());
    format!("task-{}", hex::encode(digest.as_ref()))
}

fn parse_session_event_value(value: Value) -> Result<Option<SessionEvent>, serde_json::Error> {
    let Some(kind) = value.get("kind").and_then(Value::as_str) else {
        return Ok(None);
    };

    match kind {
        "session_snapshot" => serde_json::from_value(value).map(Some),
        "context_frame" => serde_json::from_value(value).map(Some),
        "compaction_event" => serde_json::from_value(value).map(Some),
        "memory_anchor" => serde_json::from_value(value).map(Some),
        "agent_run" => serde_json::from_value(value).map(Some),
        "tool_call" => serde_json::from_value(value).map(Some),
        "tool_result" => serde_json::from_value(value).map(Some),
        // OC-Phase 6 P6.1: Goal envelope. Old binaries that predate
        // P6.1 fall through to the `unknown` branch below and skip
        // the event without surfacing an error; this branch lets a
        // P6.1-aware binary parse the envelope into the `Goal` variant.
        "goal" => serde_json::from_value(value).map(Some),
        // OC-Phase 4 ArtifactLedger (v0.17.810): same
        // forward-compat shape as `goal` — older binaries skip
        // the row via the unknown branch.
        "ai_artifact" => serde_json::from_value(value).map(Some),
        "code_workflow" => parse_code_workflow_event_value(value),
        unknown => {
            tracing::warn!(event_kind = unknown, "skipping unknown session event");
            Ok(None)
        }
    }
}

/// Decode a Code workflow envelope while preserving additive nested variants.
/// A binary that predates the whole `code_workflow` top-level kind skips it in
/// `parse_session_event_value`; a binary that knows the envelope but not a
/// future nested event likewise skips only that row instead of rejecting the
/// complete session log.
fn parse_code_workflow_event_value(
    value: Value,
) -> Result<Option<SessionEvent>, serde_json::Error> {
    let nested_event = value
        .get("payload")
        .and_then(Value::as_object)
        .and_then(|payload| payload.get("event"))
        .and_then(Value::as_str);
    let Some(nested_event) = nested_event else {
        return serde_json::from_value(value).map(Some);
    };

    match nested_event {
        "command_accepted"
        | "intent_review_requested"
        | "plan_review_requested"
        | "phase1_formal_write_started"
        | "network_policy_requested"
        | "plan_execution_repair_requested"
        | "interaction_resolved"
        | "code_ui_projection_delta"
        | "terminal_success"
        | "terminal_failure"
        | "indeterminate_side_effect"
        | "command_intent_persisted"
        | "command_terminal_success"
        | "command_terminal_success_with_interaction_resolved"
        | "command_terminal_failure"
        | "command_indeterminate_side_effect" => serde_json::from_value(value).map(Some),
        unknown => {
            tracing::warn!(
                event_kind = "code_workflow",
                nested_event = unknown,
                "skipping unknown Code workflow event"
            );
            Ok(None)
        }
    }
}

/// Repair the only recoverable JSONL corruption before an append.
///
/// A malformed final non-newline line is an interrupted write. Replay already
/// ignores it, but writing directly after it would concatenate two JSON values
/// and turn a recoverable tail into a malformed complete line. Drop only that
/// incomplete suffix, preserving every newline-terminated record. A complete
/// last JSON value lacking a newline remains valid and gets a separator before
/// the next append.
fn recover_truncated_tail_for_append(path: &Path) -> io::Result<bool> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "failed to inspect session event log '{}' before append: {error}",
                    path.display()
                ),
            ));
        }
    };
    if bytes.is_empty() || bytes.ends_with(b"\n") {
        return Ok(false);
    }

    let last_line_start = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let tail = &bytes[last_line_start..];
    if serde_json::from_slice::<Value>(tail).is_ok() {
        return Ok(true);
    }

    let file = OpenOptions::new().write(true).open(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "failed to open session event log '{}' for tail recovery: {error}",
                path.display()
            ),
        )
    })?;
    file.set_len(last_line_start as u64).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "failed to discard malformed trailing JSONL data in '{}': {error}",
                path.display()
            ),
        )
    })?;
    tracing::warn!(
        path = %path.display(),
        "discarded malformed trailing session JSONL data before append"
    );
    Ok(false)
}

#[cfg(unix)]
fn code_workflow_append_lock_path_matches_file(path: &Path, file: &fs::File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;

    let opened = file.metadata()?;
    match fs::metadata(path) {
        Ok(current) => Ok(opened.dev() == current.dev() && opened.ino() == current.ino()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn code_workflow_append_lock_path_matches_file(_path: &Path, _file: &fs::File) -> io::Result<bool> {
    // Libra never unlinks this persistent lock file. Platforms without a
    // portable file identity still get kernel-held exclusion; external lock
    // path replacement is outside the supported same-user storage contract.
    Ok(true)
}

pub fn session_events_path(session_root: &Path) -> PathBuf {
    session_root.join(SESSION_EVENTS_FILE)
}

fn process_appears_alive(
    pid: u32,
    lock_path: &Path,
    recorded_starttime: Option<u64>,
    recorded_boot_id: Option<&str>,
) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let _ = lock_path;
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return false;
        }
        let current = process_owner_identity_for_pid(pid);
        if let (Some(expected), Some(actual)) = (recorded_starttime, current.starttime)
            && expected != actual
        {
            return false;
        }
        if let (Some(expected), Some(actual)) = (recorded_boot_id, current.boot_id.as_deref())
            && expected != actual
        {
            return false;
        }
        true
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let _ = (lock_path, recorded_starttime, recorded_boot_id);
        // SAFETY: kill(pid, 0) is a existence/permission probe and does not
        // deliver a signal.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, recorded_starttime, recorded_boot_id);
        // Without a portable PID probe, treat a recent lock as live and a lock
        // older than the session-lock stale age as dead so crash recovery can
        // fence Pending mutations.
        let Ok(metadata) = fs::metadata(lock_path) else {
            return false;
        };
        let Ok(modified_at) = metadata.modified() else {
            return true;
        };
        let Ok(elapsed) = modified_at.elapsed() else {
            return true;
        };
        elapsed < STALE_PROCESS_OWNER_RECORD_AGE
    }
}

#[derive(Debug, Clone, Default)]
struct ProcessOwnerIdentity {
    pid: u32,
    starttime: Option<u64>,
    boot_id: Option<String>,
}

static CURRENT_PROCESS_OWNER_IDENTITY: OnceLock<ProcessOwnerIdentity> = OnceLock::new();

fn current_process_owner_identity() -> &'static ProcessOwnerIdentity {
    CURRENT_PROCESS_OWNER_IDENTITY
        .get_or_init(|| process_owner_identity_for_pid(std::process::id()))
}

fn process_owner_identity_for_pid(pid: u32) -> ProcessOwnerIdentity {
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let starttime = fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            // /proc/<pid>/stat: fields after the command) — starttime is field 22.
            let after_comm = stat.rsplit_once(')')?.1;
            after_comm
                .split_whitespace()
                .nth(19)
                .and_then(|value| value.parse::<u64>().ok())
        });
    ProcessOwnerIdentity {
        pid,
        starttime,
        boot_id,
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn pending_revision_receipt_followed_by_web_intent_pins_display() {
        let error = CodeCommandStoreError::PendingRevisionReceiptFollowedByWebIntent {
            command_id: "cancel-command-1".to_string(),
        };
        assert_eq!(
            error.to_string(),
            "pending consumer for IntentSpec revision receipt on Code command 'cancel-command-1' is followed by a later Web command; refusing startup recovery because the durable command order is inconsistent (start a new session or restore a consistent session log)"
        );
    }

    #[test]
    fn code_workflow_append_lock_uses_os_liveness_without_aba() {
        let tmp = TempDir::new().expect("tmp dir");
        let lock_path = tmp.path().join("events.code-workflow.append.lock");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let first = store
            .acquire_code_workflow_append_lock()
            .expect("acquire first append lock");
        fs::File::options()
            .write(true)
            .open(&lock_path)
            .expect("open append lock timestamp")
            .set_times(
                fs::FileTimes::new()
                    .set_modified(std::time::SystemTime::UNIX_EPOCH)
                    .set_accessed(std::time::SystemTime::UNIX_EPOCH),
            )
            .expect("age live append lock");
        let live_error = store
            .acquire_code_workflow_append_lock_with_timeout(Duration::ZERO)
            .expect_err("a live OS lock must not be reclaimed based on age");
        assert_eq!(live_error.kind(), io::ErrorKind::WouldBlock);

        drop(first);
        assert!(
            lock_path.exists(),
            "guard release must retain the persistent lock inode"
        );
        let replacement = store
            .acquire_code_workflow_append_lock_with_timeout(Duration::ZERO)
            .expect("a closed owner descriptor must release the OS lock immediately");
        drop(replacement);
        assert!(
            lock_path.exists(),
            "no guard may unlink a replacement lock path and create an ABA window"
        );
        store
            .acquire_code_workflow_append_lock_with_timeout(Duration::ZERO)
            .expect("persistent lock inode remains reusable after release");
    }

    #[cfg(unix)]
    #[test]
    fn code_workflow_append_lock_rejects_symlink_without_touching_event_log() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().expect("tmp dir");
        let events_path = tmp.path().join("events.code-workflow.jsonl");
        let original = b"{\"sequence\":1}\n";
        fs::write(&events_path, original).expect("write protected event log");
        symlink(
            events_path.file_name().expect("event log has a file name"),
            tmp.path().join("events.code-workflow.append.lock"),
        )
        .expect("install malicious lock symlink");

        let error = SessionJsonlStore::new(tmp.path().to_path_buf())
            .acquire_code_workflow_append_lock_with_timeout(Duration::ZERO)
            .expect_err("append lock symlink must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("regular file"));
        assert_eq!(
            fs::read(&events_path).expect("read protected event log"),
            original,
            "lock acquisition must never truncate or rewrite a symlink target"
        );
    }

    /// OC-Phase 4 ArtifactLedger JSONL projection (v0.17.810):
    /// `SessionEvent::AiArtifact` round-trips through append +
    /// load_events without losing its payload. Pins the
    /// kind/payload serde tag/content shape so a future schema
    /// extension can't accidentally break older readers'
    /// unknown-event handling.
    #[test]
    fn session_event_ai_artifact_round_trips_through_jsonl() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let event = SessionEvent::ai_artifact(AiArtifactEvent {
            event_id: Uuid::new_v4(),
            recorded_at: Utc::now(),
            thread_id: Uuid::new_v4(),
            artifact_kind: "validation_report".to_string(),
            artifact_id: Some("report-abc".to_string()),
            payload: serde_json::json!({
                "policy_version": "v0.17.810",
                "stale": false,
                "is_latest": true,
            }),
        });
        store.append(&event).expect("append must succeed");

        let loaded = store.load_events().expect("load must succeed");
        assert_eq!(loaded.len(), 1);
        match &loaded[0] {
            SessionEvent::AiArtifact(actual) => {
                let SessionEvent::AiArtifact(expected) = &event else {
                    panic!("test setup broke")
                };
                assert_eq!(actual.event_id, expected.event_id);
                assert_eq!(actual.thread_id, expected.thread_id);
                assert_eq!(actual.artifact_kind, "validation_report");
                assert_eq!(actual.artifact_id.as_deref(), Some("report-abc"));
                assert_eq!(
                    actual
                        .payload
                        .get("policy_version")
                        .and_then(|v| v.as_str()),
                    Some("v0.17.810"),
                );
            }
            other => panic!("expected AiArtifact, got: {other:?}"),
        }

        // The Event trait surface (event_kind / event_summary)
        // returns the new "ai_artifact" tag so observability
        // tooling can filter at the kind level without
        // deserialising the payload.
        use crate::internal::ai::runtime::event::Event;
        assert_eq!(event.event_kind(), "ai_artifact");
        assert!(event.event_summary().starts_with("ai_artifact "));
    }

    /// Child tool transcript events round-trip as first-class JSONL
    /// envelopes. They intentionally do not mutate legacy
    /// `SessionState`, but replay consumers can query arguments and
    /// results without parsing snapshot message strings.
    #[test]
    fn session_tool_events_round_trip_without_mutating_session_state() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let agent_run_id = AgentRunId::new();
        let tool_call = SessionEvent::tool_call(SessionToolCallEvent {
            event_id: Uuid::new_v4(),
            recorded_at: Utc::now(),
            agent_run_id,
            subagent_name: "explore".to_string(),
            call_id: "call_1".to_string(),
            tool_name: "grep_files".to_string(),
            arguments: serde_json::json!({"pattern": "TODO"}),
        });
        let tool_result = SessionEvent::tool_result(SessionToolResultEvent {
            event_id: Uuid::new_v4(),
            recorded_at: Utc::now(),
            agent_run_id,
            subagent_name: "explore".to_string(),
            call_id: "call_1".to_string(),
            tool_name: "grep_files".to_string(),
            status: "success".to_string(),
            result: Some(serde_json::json!({"matches": 3})),
            error: None,
        });
        store.append(&tool_call).expect("append tool_call");
        store.append(&tool_result).expect("append tool_result");

        let loaded = store.load_events().expect("load events");
        assert_eq!(loaded.len(), 2);
        assert!(matches!(loaded[0], SessionEvent::ToolCall(_)));
        assert!(matches!(loaded[1], SessionEvent::ToolResult(_)));
        assert!(
            store
                .load_state()
                .expect("load state should ignore tool events")
                .is_none(),
            "tool transcript events must not mutate legacy SessionState",
        );

        use crate::internal::ai::runtime::event::Event;
        assert_eq!(tool_call.event_kind(), "tool_call");
        assert_eq!(tool_result.event_kind(), "tool_result");
        assert!(tool_call.event_summary().contains("grep_files"));
        assert!(tool_result.event_summary().contains("status=success"));
    }

    /// `session_events_path` + `events_path()` must produce
    /// `<root>/events.jsonl`. Pin the layout — the migrator and
    /// `code resume` rely on it.
    #[test]
    fn events_path_appends_constant_filename() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let expected = tmp.path().join(SESSION_EVENTS_FILE);
        assert_eq!(store.events_path(), expected);
        assert_eq!(session_events_path(tmp.path()), expected);
        assert_eq!(SESSION_EVENTS_FILE, "events.jsonl");
    }

    /// Child session ids are untrusted (`task_id` can come from a model
    /// tool call), so they must never become raw path segments. The
    /// child store hashes the id into one fixed directory name under
    /// `<parent>/subagents/`.
    #[test]
    fn child_store_hashes_untrusted_id_into_single_path_segment() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let child = store.child("../outside/../../secret");
        let relative = child
            .session_root()
            .strip_prefix(store.session_root())
            .expect("child must stay below parent");
        let components: Vec<_> = relative.components().collect();

        assert_eq!(components.len(), 2);
        assert_eq!(components[0].as_os_str().to_string_lossy(), "subagents");
        let child_dir = components[1].as_os_str().to_string_lossy();
        assert!(child_dir.starts_with("task-"));
        assert_eq!(child_dir.len(), "task-".len() + 64);
        assert!(!child.session_root().ends_with("secret"));
    }

    /// `has_events()` returns `false` for a missing JSONL file (no
    /// directory created yet).
    #[test]
    fn has_events_returns_false_for_missing_file() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().join("never-exists"));
        assert!(!store.has_events().expect("has_events ok"));
    }

    /// `has_events()` returns `false` for an empty existing file
    /// (metadata.len() == 0).
    #[test]
    fn has_events_returns_false_for_empty_file() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        std::fs::write(store.events_path(), b"").expect("write empty");
        assert!(!store.has_events().expect("has_events ok"));
    }

    /// `has_events()` returns `true` after an `append`.
    #[test]
    fn append_then_has_events_returns_true() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let state = SessionState::new("/tmp/work");
        store
            .append(&SessionEvent::snapshot(state))
            .expect("append ok");
        assert!(store.has_events().expect("has_events ok"));
    }

    /// `append` + `load_events` round-trip: one snapshot in, one
    /// snapshot out, equal state.
    #[test]
    fn append_load_events_roundtrips_single_snapshot() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let state = SessionState::new("/tmp/work");
        let event = SessionEvent::snapshot(state.clone());
        store.append(&event).expect("append ok");

        let loaded = store.load_events().expect("load ok");
        assert_eq!(loaded.len(), 1);
        match &loaded[0] {
            SessionEvent::SessionSnapshot(snap) => {
                assert_eq!(snap.state, state);
            }
            other => panic!("expected SessionSnapshot, got {other:?}"),
        }
    }

    /// `load_state()` returns the latest snapshot when multiple are
    /// appended. The replay semantics are last-write-wins for
    /// snapshot events.
    #[test]
    fn load_state_returns_latest_snapshot_after_multiple_appends() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());

        let first = SessionState::new("/first/work");
        store
            .append(&SessionEvent::snapshot(first))
            .expect("first append");

        let second = SessionState::new("/second/work");
        store
            .append(&SessionEvent::snapshot(second.clone()))
            .expect("second append");

        let loaded = store.load_state().expect("load_state ok").expect("present");
        assert_eq!(loaded, second);
    }

    /// `load_state()` returns `None` when the JSONL file is missing.
    #[test]
    fn load_state_returns_none_when_no_events_file() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().join("missing-dir"));
        let loaded = store.load_state().expect("load ok");
        assert!(loaded.is_none());
    }

    /// `apply_to`: snapshot variant replaces the current state;
    /// non-snapshot variants (context_frame / compaction / memory
    /// anchor / goal) are explicit no-ops in the legacy state replay.
    #[test]
    fn apply_to_snapshot_replaces_state_other_variants_are_noops() {
        let mut state: Option<SessionState> = None;
        SessionEvent::snapshot(SessionState::new("/tmp/from-snapshot")).apply_to(&mut state);
        assert!(state.is_some(), "snapshot must populate state");
        let snapshot_state = state.clone().expect("state populated");

        // A second snapshot must replace.
        SessionEvent::snapshot(SessionState::new("/tmp/from-snapshot-2")).apply_to(&mut state);
        let after_second = state.as_ref().expect("present");
        assert_ne!(after_second, &snapshot_state);
    }

    /// `parse_session_event_value`: missing `kind` field → Ok(None)
    /// (the value is silently skipped, not an error).
    #[test]
    fn parse_session_event_value_missing_kind_returns_none() {
        let value: Value = serde_json::json!({"payload": {}});
        let result = parse_session_event_value(value).expect("call ok");
        assert!(result.is_none());
    }

    /// `parse_session_event_value`: unknown `kind` string → Ok(None)
    /// (forward-compat skip-and-warn rule from the doc).
    #[test]
    fn parse_session_event_value_unknown_kind_returns_none() {
        let value: Value =
            serde_json::json!({"kind": "future_event_type", "payload": {"any": "shape"}});
        let result = parse_session_event_value(value).expect("call ok");
        assert!(result.is_none());
    }

    /// `parse_session_event_value`: `session_snapshot` round-trips
    /// through the envelope wire format.
    #[test]
    fn parse_session_event_value_session_snapshot_parses_envelope() {
        let event = SessionEvent::snapshot(SessionState::new("/tmp/work"));
        let value = serde_json::to_value(&event).expect("serialize");
        let parsed = parse_session_event_value(value)
            .expect("parse ok")
            .expect("Some");
        assert!(matches!(parsed, SessionEvent::SessionSnapshot(_)));
    }

    /// `SessionEvent::event_kind` for SessionSnapshot returns the
    /// canonical `"session_snapshot"` discriminator — pins the
    /// Event-trait surface used by audit log emitters.
    #[test]
    fn session_event_kind_pins_session_snapshot_string() {
        let event = SessionEvent::snapshot(SessionState::new("/tmp/work"));
        assert_eq!(event.event_kind(), "session_snapshot");
    }

    /// `SessionEvent::event_summary` for SessionSnapshot includes the
    /// session id and message count so audit consumers can correlate.
    #[test]
    fn session_event_summary_includes_session_id_and_message_count() {
        let state = SessionState::new("/tmp/work");
        let session_id = state.id.clone();
        let event = SessionEvent::snapshot(state);
        let summary = event.event_summary();
        assert!(
            summary.contains(&session_id),
            "summary must include session id; got {summary}",
        );
        assert!(
            summary.contains("0 message(s)"),
            "fresh session has 0 messages; got {summary}",
        );
    }

    fn durable_test_intent(command_id: &str) -> CodeCommandIntent {
        CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", command_id),
            "test-command",
            format!("hash-{command_id}"),
            false,
        )
    }

    fn durable_revision_source(
        store: &SessionJsonlStore,
        command_id: &str,
        interaction_id: &str,
        intent_id: &str,
        digest_byte: char,
    ) -> (CodeCommandIntent, IntentRevisionRecovery, CodeWorkflowEvent) {
        let intent = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", command_id),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            format!("hash-{command_id}"),
            true,
        );
        store
            .admit_code_command(intent.clone())
            .expect("revision source intent admitted");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id: interaction_id.to_string(),
                intent_id: intent_id.to_string(),
                turn_id: format!("{interaction_id}-gate"),
                phase0_turn_id: command_id.to_string(),
            })
            .expect("revision source marker persisted");
        let recovery = IntentRevisionRecovery {
            interaction_id: interaction_id.to_string(),
            sidecar_digest: format!("hmac-sha256:{}", digest_byte.to_string().repeat(64)),
        };
        store
            .complete_code_command_success_with_interaction_resolutions_and_intent_revision(
                &intent.identity,
                "revision requested",
                &[(interaction_id.to_string(), "modify".to_string())],
                Some(&recovery),
            )
            .expect("revision source terminal persisted");
        let terminal = store
            .load_code_workflow_replay()
            .expect("revision source replay")
            .events
            .into_iter()
            .find(|event| {
                matches!(
                    &event.event,
                    CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                        command,
                        interaction_id: candidate,
                        ..
                    } if command == &intent.identity && candidate == interaction_id
                )
            })
            .expect("revision source terminal event");
        (intent, recovery, terminal)
    }

    fn durable_revision_consumer(command_id: &str) -> CodeCommandIntent {
        CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", command_id),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            format!("hash-{command_id}"),
            true,
        )
    }

    fn durable_revision_cancel_consumer(command_id: &str) -> CodeCommandIntent {
        use sha2::Digest as _;

        CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", command_id),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            format!(
                "sha256:{}",
                hex::encode(sha2::Sha256::digest(
                    INTENT_REVISION_CANCEL_COMMAND_INPUT.as_bytes()
                ))
            ),
            true,
        )
    }

    fn revision_consumption_claim(
        source: &CodeCommandIntent,
        recovery: &IntentRevisionRecovery,
        terminal: &CodeWorkflowEvent,
        intent_id: &str,
        consumer: CodeCommandIntent,
    ) -> IntentRevisionConsumptionClaim {
        IntentRevisionConsumptionClaim {
            schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
            interaction_id: recovery.interaction_id.clone(),
            source_command: source.identity.clone(),
            consumer_intent: consumer,
            terminal_event_id: terminal.event_id,
            terminal_sequence: terminal.sequence,
            intent_id: intent_id.to_string(),
            sidecar_digest: Some(recovery.sidecar_digest.clone()),
        }
    }

    #[test]
    fn intent_revision_recovery_is_atomic_bounded_and_exactly_idempotent() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let intent = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", "phase0-turn"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            "hash-phase0-turn",
            true,
        );
        store
            .admit_code_command(intent.clone())
            .expect("command intent admitted");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id: "intent-review-1".to_string(),
                intent_id: "intent-1".to_string(),
                turn_id: "intent-review-gate-1".to_string(),
                phase0_turn_id: intent.identity.command_id.clone(),
            })
            .expect("review marker persisted");
        let resolutions = [
            ("risk-1".to_string(), "answered".to_string()),
            ("intent-review-1".to_string(), "modify".to_string()),
        ];
        let recovery = IntentRevisionRecovery {
            interaction_id: "intent-review-1".to_string(),
            sidecar_digest: format!("hmac-sha256:{}", "a".repeat(64)),
        };

        let complete = || {
            store.complete_code_command_success_with_interaction_resolutions_and_intent_revision(
                &intent.identity,
                "revision requested",
                &resolutions,
                Some(&recovery),
            )
        };
        assert_eq!(
            complete().expect("combined terminal persisted"),
            CodeCommandStatus::Succeeded {
                summary: "revision requested".to_string(),
            }
        );
        assert_eq!(
            complete().expect("exact retry re-syncs the same terminal"),
            CodeCommandStatus::Succeeded {
                summary: "revision requested".to_string(),
            }
        );
        let replay = store.load_code_workflow_replay().expect("workflow replay");
        assert!(matches!(
            replay.events.last().map(|event| &event.event),
            Some(CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                resolution,
                intent_revision: Some(actual),
                ..
            }) if interaction_id == "intent-review-1"
                && resolution == "modify"
                && actual == &recovery
        ));

        let conflicting = IntentRevisionRecovery {
            sidecar_digest: format!("hmac-sha256:{}", "b".repeat(64)),
            ..recovery.clone()
        };
        assert!(matches!(
            store.complete_code_command_success_with_interaction_resolutions_and_intent_revision(
                &intent.identity,
                "revision requested",
                &resolutions,
                Some(&conflicting),
            ),
            Err(CodeCommandStoreError::TerminalConflict { .. })
        ));
    }

    #[test]
    fn intent_revision_recovery_accepts_only_the_current_restored_review_owner() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        for turn_id in ["review-owner-old", "review-owner-current"] {
            store
                .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                    interaction_id: "intent-review-restored".to_string(),
                    intent_id: "intent-restored".to_string(),
                    turn_id: turn_id.to_string(),
                    phase0_turn_id: "phase0-original".to_string(),
                })
                .expect("review owner binding persisted");
        }
        let restored = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", "review-owner-current"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            "hash-review-owner-current",
            false,
        );
        store
            .admit_code_command(restored.clone())
            .expect("restored nonmutating review owner admitted");
        let recovery = IntentRevisionRecovery {
            interaction_id: "intent-review-restored".to_string(),
            sidecar_digest: format!("hmac-sha256:{}", "c".repeat(64)),
        };
        assert_eq!(
            store
                .complete_code_command_success_with_interaction_resolutions_and_intent_revision(
                    &restored.identity,
                    "revision requested",
                    &[("intent-review-restored".to_string(), "modify".to_string())],
                    Some(&recovery),
                )
                .expect("current restored review owner terminalized"),
            CodeCommandStatus::Succeeded {
                summary: "revision requested".to_string(),
            }
        );

        let stale_tmp = TempDir::new().expect("stale tmp dir");
        let stale_store = SessionJsonlStore::new(stale_tmp.path().to_path_buf());
        let stale = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", "phase0-original"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            "hash-phase0-original",
            true,
        );
        stale_store
            .admit_code_command(stale.clone())
            .expect("original Phase 0 owner admitted");
        for turn_id in ["review-owner-old", "review-owner-current"] {
            stale_store
                .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                    interaction_id: "intent-review-restored".to_string(),
                    intent_id: "intent-restored".to_string(),
                    turn_id: turn_id.to_string(),
                    phase0_turn_id: "phase0-original".to_string(),
                })
                .expect("replacement review owner binding persisted");
        }
        assert!(matches!(
            stale_store.complete_code_command_success_with_interaction_resolved(
                &stale.identity,
                "cancelled",
                "intent-review-restored",
                "cancel",
            ),
            Err(CodeCommandStoreError::InvalidIntent)
        ));
    }

    #[test]
    fn intent_revision_recovery_rejects_noncanonical_or_mismatched_payloads() {
        fn pending_store(command_id: &str) -> (TempDir, SessionJsonlStore, CodeCommandIntent) {
            let tmp = TempDir::new().expect("tmp dir");
            let store = SessionJsonlStore::new(tmp.path().to_path_buf());
            let intent = durable_test_intent(command_id);
            store
                .admit_code_command(intent.clone())
                .expect("command intent admitted");
            store
                .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                    interaction_id: "intent-review-1".to_string(),
                    intent_id: "intent-1".to_string(),
                    turn_id: "intent-review-gate-1".to_string(),
                    phase0_turn_id: intent.identity.command_id.clone(),
                })
                .expect("review marker persisted");
            (tmp, store, intent)
        }

        let (_tmp, store, intent) = pending_store("wrong-resolution");
        let recovery = IntentRevisionRecovery {
            interaction_id: "intent-review-1".to_string(),
            sidecar_digest: format!("hmac-sha256:{}", "a".repeat(64)),
        };
        assert!(matches!(
            store.complete_code_command_success_with_interaction_resolutions_and_intent_revision(
                &intent.identity,
                "done",
                &[("intent-review-1".to_string(), "revise".to_string())],
                Some(&recovery),
            ),
            Err(CodeCommandStoreError::InvalidIntent)
        ));

        let (_tmp, store, intent) = pending_store("wrong-interaction");
        let wrong_interaction = IntentRevisionRecovery {
            interaction_id: "other-review".to_string(),
            sidecar_digest: format!("hmac-sha256:{}", "a".repeat(64)),
        };
        assert!(matches!(
            store.complete_code_command_success_with_interaction_resolutions_and_intent_revision(
                &intent.identity,
                "done",
                &[("intent-review-1".to_string(), "modify".to_string())],
                Some(&wrong_interaction),
            ),
            Err(CodeCommandStoreError::InvalidIntent)
        ));

        let (_tmp, store, intent) = pending_store("invalid-digest");
        let invalid_digest = IntentRevisionRecovery {
            interaction_id: "intent-review-1".to_string(),
            sidecar_digest: format!("hmac-sha256:{}", "A".repeat(64)),
        };
        assert!(matches!(
            store.complete_code_command_success_with_interaction_resolutions_and_intent_revision(
                &intent.identity,
                "done",
                &[("intent-review-1".to_string(), "modify".to_string())],
                Some(&invalid_digest),
            ),
            Err(CodeCommandStoreError::InvalidIntent)
        ));
    }

    #[test]
    fn intent_revision_terminal_retry_rejects_marker_appended_after_terminal() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let intent = durable_test_intent("terminal-before-marker");
        store
            .admit_code_command(intent.clone())
            .expect("command intent admitted");
        let recovery = IntentRevisionRecovery {
            interaction_id: "late-marker-review".to_string(),
            sidecar_digest: format!("hmac-sha256:{}", "a".repeat(64)),
        };
        let terminal = CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
            command: intent.identity.clone(),
            summary: "revision requested".to_string(),
            interaction_id: recovery.interaction_id.clone(),
            resolution: "modify".to_string(),
            prior_interaction_resolutions: Vec::new(),
            intent_revision: Some(recovery.clone()),
        };
        store
            .append_code_workflow_durable(terminal)
            .expect("synthetic terminal persisted");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id: recovery.interaction_id.clone(),
                intent_id: "intent-late".to_string(),
                turn_id: "late-gate".to_string(),
                phase0_turn_id: intent.identity.command_id.clone(),
            })
            .expect("synthetic late marker persisted");

        assert!(matches!(
            store.complete_code_command_success_with_interaction_resolutions_and_intent_revision(
                &intent.identity,
                "revision requested",
                &[(recovery.interaction_id.clone(), "modify".to_string())],
                Some(&recovery),
            ),
            Err(CodeCommandStoreError::InvalidIntent)
        ));
    }

    #[test]
    fn intent_revision_consumption_requires_digest_and_exact_retry_resyncs_after_terminal() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let (source, recovery, terminal) = durable_revision_source(
            &store,
            "revision-source",
            "revision-review",
            "intent-revision",
            'a',
        );
        let consumer = durable_revision_consumer("revision-consumer");
        store
            .admit_code_command(consumer.clone())
            .expect("consumer intent admitted");
        let claim = revision_consumption_claim(
            &source,
            &recovery,
            &terminal,
            "intent-revision",
            consumer.clone(),
        );
        let mut missing_digest = claim.clone();
        missing_digest.sidecar_digest = None;
        assert!(matches!(
            store.prepare_intent_revision_consumption(&consumer, &missing_digest),
            Err(CodeCommandStoreError::InvalidIntent)
        ));

        let consumption = store
            .prepare_intent_revision_consumption(&consumer, &claim)
            .expect("exact consumer intent resolved");
        store
            .record_intent_revision_consumption(&consumption)
            .expect("consumption receipt persisted");
        store
            .complete_code_command_success(&consumer.identity, "consumer complete")
            .expect("consumer terminal persisted");
        store.fail_next_events_log_resync_for_test();
        store
            .record_intent_revision_consumption(&consumption)
            .expect_err("exact terminal retry must still attempt a durable resync");
        store
            .record_intent_revision_consumption(&consumption)
            .expect("exact receipt retry re-syncs after consumer terminal");

        let replay = store.load_code_workflow_replay().expect("workflow replay");
        assert_eq!(
            replay
                .events
                .iter()
                .filter(|event| matches!(
                    &event.event,
                    CodeWorkflowEventKind::InteractionResolved {
                        intent_revision_consumption: Some(actual),
                        ..
                    } if actual == &consumption
                ))
                .count(),
            1
        );
    }

    #[test]
    fn effectless_intent_revision_receipt_fails_closed_in_batch_validation() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let (source, recovery, terminal) = durable_revision_source(
            &store,
            "effectless-source",
            "effectless-source-review",
            "effectless-source-intent",
            'c',
        );
        let consumer = durable_revision_consumer("effectless-consumer");
        store
            .admit_code_command(consumer.clone())
            .expect("consumer intent admitted");
        let claim = revision_consumption_claim(
            &source,
            &recovery,
            &terminal,
            "effectless-source-intent",
            consumer.clone(),
        );
        let consumption = store
            .prepare_intent_revision_consumption(&consumer, &claim)
            .expect("consumer lineage prepared");
        store
            .record_intent_revision_consumption(&consumption)
            .expect("synthetic legacy receipt persisted");
        let replay = store
            .load_intent_revision_workflow_replay()
            .expect("complete replay");

        assert!(matches!(
            validated_intent_revision_consumption_receipts(&replay),
            Err(CodeCommandStoreError::InvalidIntent)
        ));
    }

    #[test]
    fn duplicate_source_intent_after_its_terminal_fails_closed_in_batch_validation() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let (source, recovery, terminal) = durable_revision_source(
            &store,
            "duplicate-source",
            "duplicate-source-review",
            "duplicate-source-intent",
            'd',
        );
        let consumer = durable_revision_cancel_consumer("duplicate-source-cancel");
        store
            .admit_code_command(consumer.clone())
            .expect("cancel consumer admitted");
        let claim = revision_consumption_claim(
            &source,
            &recovery,
            &terminal,
            "duplicate-source-intent",
            consumer.clone(),
        );
        let consumption = store
            .prepare_intent_revision_consumption(&consumer, &claim)
            .expect("cancel lineage prepared");
        store
            .record_intent_revision_consumption(&consumption)
            .expect("cancel receipt persisted");
        store
            .complete_code_command_success(&consumer.identity, "revision cancelled")
            .expect("cancel terminal persisted");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::CommandIntentPersisted {
                command: source,
            })
            .expect("synthetic duplicate source intent persisted after its terminal");
        let replay = store
            .load_intent_revision_workflow_replay()
            .expect("complete replay");

        assert!(matches!(
            validated_intent_revision_consumption_receipts(&replay),
            Err(CodeCommandStoreError::InvalidIntent)
        ));
    }

    #[test]
    fn invalid_nonmutating_web_intent_after_receipt_fails_closed() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let (source, recovery, terminal) = durable_revision_source(
            &store,
            "nonmutating-source",
            "nonmutating-review",
            "nonmutating-intent",
            'e',
        );
        let consumer = durable_revision_cancel_consumer("nonmutating-cancel");
        store
            .admit_code_command(consumer.clone())
            .expect("cancel consumer admitted");
        let claim = revision_consumption_claim(
            &source,
            &recovery,
            &terminal,
            "nonmutating-intent",
            consumer.clone(),
        );
        let consumption = store
            .prepare_intent_revision_consumption(&consumer, &claim)
            .expect("cancel lineage prepared");
        store
            .record_intent_revision_consumption(&consumption)
            .expect("cancel receipt persisted");
        store
            .complete_code_command_success(&consumer.identity, "revision cancelled")
            .expect("cancel terminal persisted");

        let valid_gate_owner = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", "valid-nonmutating-owner"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            "valid-nonmutating-hash",
            false,
        );
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::CommandIntentPersisted {
                command: valid_gate_owner,
            })
            .expect("valid nonmutating owner persisted after the receipt");
        let replay = store
            .load_intent_revision_workflow_replay()
            .expect("valid nonmutating replay");
        validated_intent_revision_consumption_receipts(&replay)
            .expect("valid nonmutating owners remain outside the committed lineage");

        let invalid_gate_owner = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", "invalid-nonmutating-owner"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            "",
            false,
        );
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::CommandIntentPersisted {
                command: invalid_gate_owner,
            })
            .expect("synthetic invalid nonmutating owner persisted");
        let replay = store
            .load_intent_revision_workflow_replay()
            .expect("invalid nonmutating replay");
        assert!(matches!(
            validated_intent_revision_consumption_receipts(&replay),
            Err(CodeCommandStoreError::InvalidIntent)
        ));
    }

    #[test]
    fn reused_web_command_id_across_scopes_fails_closed_before_marker_attribution() {
        let first = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal-a", "shared-command"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            "hash-a",
            true,
        );
        let second = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal-b", "shared-command"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            "hash-b",
            true,
        );
        let replay = CodeWorkflowReplay {
            events: vec![
                CodeWorkflowEvent::new(
                    1,
                    CodeWorkflowEventKind::CommandIntentPersisted { command: first },
                ),
                CodeWorkflowEvent::new(
                    2,
                    CodeWorkflowEventKind::CommandIntentPersisted { command: second },
                ),
            ],
            gaps: Vec::new(),
            window_cut_mid_record: false,
        };

        assert!(matches!(
            validated_intent_revision_consumption_receipts(&replay),
            Err(CodeCommandStoreError::InvalidIntent)
        ));
    }

    #[test]
    fn conflicting_or_duplicate_intent_review_marker_ownership_fails_closed() {
        let marker =
            |interaction_id: &str, intent_id: &str, turn_id: &str, phase0_turn_id: &str| {
                CodeWorkflowEventKind::IntentReviewRequested {
                    interaction_id: interaction_id.to_string(),
                    intent_id: intent_id.to_string(),
                    turn_id: turn_id.to_string(),
                    phase0_turn_id: phase0_turn_id.to_string(),
                }
            };
        let cases = [
            [
                marker("review-a", "intent-a", "turn-a", "phase0-a"),
                marker("review-a", "intent-a", "turn-b", "phase0-b"),
            ],
            [
                marker("review-a", "intent-a", "turn-a", "phase0-a"),
                marker("review-b", "intent-b", "turn-b", "phase0-a"),
            ],
            [
                marker("review-a", "intent-a", "shared-turn", "phase0-a"),
                marker("review-b", "intent-b", "shared-turn", "phase0-b"),
            ],
        ];
        for (case_index, events) in cases.into_iter().enumerate() {
            let replay = CodeWorkflowReplay {
                events: events
                    .into_iter()
                    .enumerate()
                    .map(|(index, event)| CodeWorkflowEvent::new(index as u64 + 1, event))
                    .collect(),
                gaps: Vec::new(),
                window_cut_mid_record: false,
            };
            assert!(
                matches!(
                    validated_intent_revision_consumption_receipts(&replay),
                    Err(CodeCommandStoreError::InvalidIntent)
                ),
                "marker conflict case {case_index} must fail closed"
            );
        }

        let replay = CodeWorkflowReplay {
            events: [
                marker("review", "intent", "turn-a", "phase0"),
                marker("review", "intent", "turn-b", "phase0"),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, event)| CodeWorkflowEvent::new(index as u64 + 1, event))
            .collect(),
            gaps: Vec::new(),
            window_cut_mid_record: false,
        };
        validated_intent_revision_consumption_receipts(&replay)
            .expect("restored markers may retain one exact interaction lineage");
    }

    #[test]
    fn legacy_source_terminal_with_hmac_claim_remains_batch_authoritative() {
        use sha2::Digest as _;

        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let source = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "legacy-principal", "legacy-hmac-source"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            "legacy-source-hash",
            true,
        );
        store
            .admit_code_command(source.clone())
            .expect("legacy source admitted");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id: "legacy-hmac-review".to_string(),
                intent_id: "legacy-hmac-intent".to_string(),
                turn_id: "legacy-hmac-gate".to_string(),
                phase0_turn_id: source.identity.command_id.clone(),
            })
            .expect("legacy source marker persisted");
        let terminal = store
            .append_code_workflow_durable(
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    command: source.identity.clone(),
                    summary: "legacy revision requested".to_string(),
                    interaction_id: "legacy-hmac-review".to_string(),
                    resolution: "modify".to_string(),
                    prior_interaction_resolutions: Vec::new(),
                    intent_revision: None,
                },
            )
            .expect("legacy source terminal persisted without additive binding");
        let recovery = IntentRevisionRecovery {
            interaction_id: "legacy-hmac-review".to_string(),
            sidecar_digest: format!("hmac-sha256:{}", "b".repeat(64)),
        };
        let consumer = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "legacy-principal", "legacy-hmac-cancel"),
            INTENT_REVISION_CONSUMER_COMMAND_KIND,
            format!(
                "sha256:{}",
                hex::encode(sha2::Sha256::digest(
                    INTENT_REVISION_CANCEL_COMMAND_INPUT.as_bytes()
                ))
            ),
            true,
        );
        store
            .admit_code_command(consumer.clone())
            .expect("legacy cancel admitted");
        let claim = revision_consumption_claim(
            &source,
            &recovery,
            &terminal,
            "legacy-hmac-intent",
            consumer.clone(),
        );
        let consumption = store
            .prepare_intent_revision_consumption(&consumer, &claim)
            .expect("legacy HMAC claim prepared");
        store
            .record_intent_revision_consumption(&consumption)
            .expect("legacy HMAC receipt persisted");
        store
            .complete_code_command_success(&consumer.identity, "legacy revision cancelled")
            .expect("legacy cancel terminal persisted");

        let replay = store
            .load_intent_revision_workflow_replay()
            .expect("legacy receipt replay");
        let validated = validated_intent_revision_consumption_receipts(&replay)
            .expect("canonical HMAC claim preserves legacy terminal compatibility");
        let legacy_source = validated
            .source_terminal_for_interaction("legacy-hmac-review")
            .expect("legacy source terminal is indexed");
        assert!(legacy_source.legacy_terminal);
        assert_eq!(legacy_source.intent_id, "legacy-hmac-intent");
        assert_eq!(validated.receipts().len(), 1);
        assert!(
            validated
                .exact_receipt_for_consumption(&consumption)
                .is_some()
        );
    }

    #[test]
    fn resolved_replacement_receipt_permanently_closes_its_retry_lineage() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let (source, recovery, terminal) = durable_revision_source(
            &store,
            "replacement-source",
            "replacement-source-review",
            "replacement-source-intent",
            'd',
        );

        let attempt_a = durable_revision_consumer("replacement-attempt-a");
        store
            .admit_code_command(attempt_a.clone())
            .expect("first revision attempt admitted");
        store
            .mark_code_command_indeterminate(
                &attempt_a.identity,
                INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_EFFECT,
                INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_REASON,
            )
            .expect("first retryable attempt terminal persisted");

        let attempt_b = durable_revision_consumer("replacement-attempt-b");
        store
            .admit_code_command(attempt_b.clone())
            .expect("replacement-producing attempt admitted");
        let claim = revision_consumption_claim(
            &source,
            &recovery,
            &terminal,
            "replacement-source-intent",
            attempt_b.clone(),
        );
        let consumption = store
            .prepare_intent_revision_consumption(&attempt_b, &claim)
            .expect("replacement consumption prepared");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id: "replacement-review".to_string(),
                intent_id: "replacement-intent".to_string(),
                turn_id: "replacement-gate".to_string(),
                phase0_turn_id: attempt_b.identity.command_id.clone(),
            })
            .expect("replacement marker persisted before the ACK-loss terminal");
        store
            .mark_code_command_indeterminate(
                &attempt_b.identity,
                INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_EFFECT,
                INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON,
            )
            .expect("replacement handoff ACK-loss terminal persisted");
        let first_recovery = store
            .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
                Some(&attempt_b.identity.command_id),
                None,
                Some(&consumption),
            )
            .expect("startup recovery appends the post-terminal receipt");
        assert!(first_recovery.fenced.is_empty());
        assert!(first_recovery.intent_revision_replacement_review_healed);
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
                interaction_id: "replacement-review".to_string(),
                resolution: "cancel".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .expect("replacement review resolved");

        // A later Web mutation is outside the closed receipt lineage. It must
        // neither invalidate the receipt nor make attempt B look non-current.
        let later = durable_revision_consumer("later-web-turn");
        store
            .admit_code_command(later.clone())
            .expect("later Web turn admitted");
        store
            .complete_code_command_success(&later.identity, "later turn complete")
            .expect("later Web turn completed");

        let replay = store
            .load_intent_revision_workflow_replay()
            .expect("resolved replacement replay remains valid");
        validate_intent_revision_consumption_receipt(&replay, &consumption)
            .expect("resolved replacement keeps the post-terminal receipt authoritative");
        let validated = validated_intent_revision_consumption_receipts(&replay)
            .expect("resolved replacement remains in the batch authority projection");
        let source_projection = validated
            .exact_source_terminal(
                terminal.event_id,
                terminal.sequence,
                &recovery.interaction_id,
                &source.identity,
            )
            .expect("source terminal has one exact indexed authority");
        assert!(source_projection.later_web_intent);
        let receipt_projection = validated
            .exact_receipt_for_consumption(&consumption)
            .expect("replacement receipt has one exact indexed authority");
        assert!(receipt_projection.replacement_review);
        assert!(!receipt_projection.replacement_review_open);
        assert!(receipt_projection.later_web_intent);
        assert!(
            intent_revision_consumer_has_replacement_review(&replay, &consumption)
                .expect("replacement proof is valid")
        );
        assert!(
            !intent_revision_consumer_has_open_replacement_review(&replay, &consumption)
                .expect("resolved replacement proof is valid")
        );

        for recovery_run in 1..=2 {
            let outcome = store
                .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
                    None, None, None,
                )
                .unwrap_or_else(|error| {
                    panic!("recovery run {recovery_run} must accept the closed lineage: {error}")
                });
            assert!(outcome.fenced.is_empty());
        }
    }

    #[test]
    fn committed_revision_receipt_batch_index_visits_five_thousand_events_linearly() {
        use sha2::Digest as _;

        const EVENT_COUNT: usize = 5_000;
        const RECEIPT_COUNT: usize = 700;

        let mut sequence = 0_u64;
        let mut events = Vec::with_capacity(EVENT_COUNT);
        let mut append = |event: CodeWorkflowEventKind| {
            sequence = sequence.saturating_add(1);
            let event = CodeWorkflowEvent::new(sequence, event);
            events.push(event.clone());
            event
        };
        let cancel_hash = format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(
                INTENT_REVISION_CANCEL_COMMAND_INPUT.as_bytes()
            ))
        );
        let mut consumers = Vec::with_capacity(RECEIPT_COUNT);
        for lineage in 0..RECEIPT_COUNT {
            let source = CodeCommandIntent::new(
                CodeCommandIdentity::new(
                    "repo",
                    "session",
                    "principal",
                    format!("source-{lineage}"),
                ),
                INTENT_REVISION_CONSUMER_COMMAND_KIND,
                format!("source-hash-{lineage}"),
                true,
            );
            append(CodeWorkflowEventKind::CommandIntentPersisted {
                command: source.clone(),
            });
            let interaction_id = format!("source-review-{lineage}");
            let intent_id = format!("source-intent-{lineage}");
            append(CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id: interaction_id.clone(),
                intent_id: intent_id.clone(),
                turn_id: format!("source-gate-{lineage}"),
                phase0_turn_id: source.identity.command_id.clone(),
            });
            let recovery = IntentRevisionRecovery {
                interaction_id: interaction_id.clone(),
                sidecar_digest: format!("hmac-sha256:{}", "a".repeat(64)),
            };
            let terminal = append(
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    command: source.identity.clone(),
                    summary: "revision requested".to_string(),
                    interaction_id: interaction_id.clone(),
                    resolution: "modify".to_string(),
                    prior_interaction_resolutions: Vec::new(),
                    intent_revision: Some(recovery.clone()),
                },
            );
            let consumer = CodeCommandIntent::new(
                CodeCommandIdentity::new(
                    "repo",
                    "session",
                    "principal",
                    format!("consumer-{lineage}"),
                ),
                INTENT_REVISION_CONSUMER_COMMAND_KIND,
                cancel_hash.clone(),
                true,
            );
            let consumer_event = append(CodeWorkflowEventKind::CommandIntentPersisted {
                command: consumer.clone(),
            });
            let consumption = IntentRevisionConsumption {
                claim: revision_consumption_claim(
                    &source,
                    &recovery,
                    &terminal,
                    &intent_id,
                    consumer.clone(),
                ),
                consumer_intent_event_id: consumer_event.event_id,
                consumer_intent_sequence: consumer_event.sequence,
            };
            append(intent_revision_consumption_receipt_event(&consumption));
            append(CodeWorkflowEventKind::CommandTerminalSuccess {
                command: consumer.identity.clone(),
                summary: "revision cancelled".to_string(),
            });
            consumers.push(consumer.identity);
        }
        for _ in 0..EVENT_COUNT.saturating_sub(RECEIPT_COUNT.saturating_mul(6)) {
            append(CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection: "linear-index-filler".to_string(),
                summary: "irrelevant event".to_string(),
                payload: Value::Null,
            });
        }
        let _ = append;
        let replay = CodeWorkflowReplay {
            events,
            gaps: Vec::new(),
            window_cut_mid_record: false,
        };

        reset_intent_revision_replay_index_visits();
        let validated = validated_intent_revision_consumption_receipts(&replay)
            .expect("all independent cancellation receipts validate");
        assert_eq!(validated.source_terminals().len(), RECEIPT_COUNT);
        assert_eq!(validated.receipts().len(), RECEIPT_COUNT);
        for consumer in consumers {
            assert!(matches!(
                validated.committed_consumer_status(&consumer),
                Some(CodeCommandStatus::Succeeded { .. })
            ));
        }
        let visits = intent_revision_replay_index_visits();
        assert!(
            visits <= EVENT_COUNT.saturating_mul(4),
            "batch validation visited {visits} indexed relationships for {EVENT_COUNT} events"
        );
    }

    #[test]
    fn claiming_and_consuming_revision_retry_index_visits_five_thousand_events_linearly() {
        const EVENT_COUNT: usize = 5_000;
        const ATTEMPT_COUNT: usize = 2_000;

        let mut sequence = 0_u64;
        let mut events = Vec::with_capacity(EVENT_COUNT);
        let mut append = |event: CodeWorkflowEventKind| {
            sequence = sequence.saturating_add(1);
            let event = CodeWorkflowEvent::new(sequence, event);
            events.push(event.clone());
            event
        };
        let source = durable_revision_consumer("linear-retry-source");
        append(CodeWorkflowEventKind::CommandIntentPersisted {
            command: source.clone(),
        });
        let interaction_id = "linear-retry-review";
        let intent_id = "linear-retry-intent";
        append(CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: interaction_id.to_string(),
            intent_id: intent_id.to_string(),
            turn_id: "linear-retry-gate".to_string(),
            phase0_turn_id: source.identity.command_id.clone(),
        });
        let recovery = IntentRevisionRecovery {
            interaction_id: interaction_id.to_string(),
            sidecar_digest: format!("hmac-sha256:{}", "a".repeat(64)),
        };
        let terminal = append(
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                command: source.identity.clone(),
                summary: "revision requested".to_string(),
                interaction_id: interaction_id.to_string(),
                resolution: "modify".to_string(),
                prior_interaction_resolutions: Vec::new(),
                intent_revision: Some(recovery.clone()),
            },
        );
        let mut latest_attempt = None;
        let mut latest_attempt_event = None;
        for attempt in 0..ATTEMPT_COUNT {
            let consumer = durable_revision_consumer(&format!("linear-retry-{attempt}"));
            let consumer_event = append(CodeWorkflowEventKind::CommandIntentPersisted {
                command: consumer.clone(),
            });
            append(CodeWorkflowEventKind::CommandTerminalFailure {
                command: consumer.identity.clone(),
                reason: INTENT_REVISION_CONSUMER_RECOVERY_FAILURE_REASON.to_string(),
                interaction_resolutions: Vec::new(),
                retry_intent_review: None,
            });
            latest_attempt = Some(consumer);
            latest_attempt_event = Some(consumer_event);
        }
        for _ in 0..EVENT_COUNT.saturating_sub(3 + ATTEMPT_COUNT.saturating_mul(2)) {
            append(CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection: "linear-retry-index-filler".to_string(),
                summary: "irrelevant event".to_string(),
                payload: Value::Null,
            });
        }
        let _ = append;
        assert_eq!(events.len(), EVENT_COUNT);
        let latest_attempt = latest_attempt.expect("at least one retry attempt");
        let latest_attempt_event = latest_attempt_event.expect("latest retry event");
        let claiming_consumer = durable_revision_consumer("linear-retry-not-yet-admitted");
        let claim =
            revision_consumption_claim(&source, &recovery, &terminal, intent_id, claiming_consumer);
        let replay = CodeWorkflowReplay {
            events,
            gaps: Vec::new(),
            window_cut_mid_record: false,
        };

        reset_intent_revision_replay_index_visits();
        let validated = validated_intent_revision_consumption_receipts(&replay)
            .expect("one shared replay index validates the long retry lineage");
        let consumption = validated
            .latest_recoverable_intent_revision_attempt_before_claim(&claim)
            .expect("Claiming recovery validates the complete retry lineage")
            .expect("the latest failed retry retains exact durable attribution");
        assert_eq!(consumption.claim.consumer_intent, latest_attempt);
        assert_eq!(
            consumption.consumer_intent_event_id,
            latest_attempt_event.event_id
        );
        assert_eq!(
            consumption.consumer_intent_sequence,
            latest_attempt_event.sequence
        );
        assert!(matches!(
            validated
                .claimed_intent_revision_consumer_status(&consumption)
                .expect("Consuming recovery reuses the same indexed lineage"),
            CodeCommandStatus::Failed { reason }
                if reason == INTENT_REVISION_CONSUMER_RECOVERY_FAILURE_REASON
        ));
        let visits = intent_revision_replay_index_visits();
        assert!(
            visits <= EVENT_COUNT.saturating_mul(4),
            "Claiming and Consuming recovery visited {visits} indexed relationships for \
             {EVENT_COUNT} events and {ATTEMPT_COUNT} same-scope retries"
        );
    }

    #[test]
    fn replacement_marker_after_consumer_terminal_never_authorizes_a_receipt() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let (source, recovery, terminal) = durable_revision_source(
            &store,
            "late-replacement-source",
            "late-replacement-source-review",
            "late-replacement-source-intent",
            'e',
        );
        let consumer = durable_revision_consumer("late-replacement-consumer");
        store
            .admit_code_command(consumer.clone())
            .expect("revision consumer admitted");
        let claim = revision_consumption_claim(
            &source,
            &recovery,
            &terminal,
            "late-replacement-source-intent",
            consumer.clone(),
        );
        let consumption = store
            .prepare_intent_revision_consumption(&consumer, &claim)
            .expect("revision consumption prepared");
        store
            .mark_code_command_indeterminate(
                &consumer.identity,
                INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_EFFECT,
                INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON,
            )
            .expect("synthetic consumer terminal persisted first");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id: "late-replacement-review".to_string(),
                intent_id: "late-replacement-intent".to_string(),
                turn_id: "late-replacement-gate".to_string(),
                phase0_turn_id: consumer.identity.command_id.clone(),
            })
            .expect("synthetic out-of-order replacement marker persisted");
        let before = std::fs::read(store.events_path()).expect("read malformed workflow baseline");

        assert!(matches!(
            store.recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
                Some(&consumer.identity.command_id),
                None,
                Some(&consumption),
            ),
            Err(CodeCommandStoreError::InvalidIntent)
        ));
        assert_eq!(
            std::fs::read(store.events_path()).expect("read workflow after rejected recovery"),
            before,
            "fail-closed recovery must not append a receipt or terminal"
        );
    }

    #[test]
    fn intent_revision_consumption_is_first_writer_for_source_consumer_and_event() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let (source_a, recovery_a, terminal_a) =
            durable_revision_source(&store, "source-a", "review-a", "intent-a", 'a');
        let (source_b, recovery_b, terminal_b) =
            durable_revision_source(&store, "source-b", "review-b", "intent-b", 'b');
        let consumer_a = durable_revision_consumer("consumer-a");
        store
            .admit_code_command(consumer_a.clone())
            .expect("first consumer admitted");
        let claim_a = revision_consumption_claim(
            &source_a,
            &recovery_a,
            &terminal_a,
            "intent-a",
            consumer_a.clone(),
        );
        let consumption_a = store
            .prepare_intent_revision_consumption(&consumer_a, &claim_a)
            .expect("first consumption prepared");
        store
            .record_intent_revision_consumption(&consumption_a)
            .expect("first consumption persisted");

        let claim_same_consumer = revision_consumption_claim(
            &source_b,
            &recovery_b,
            &terminal_b,
            "intent-b",
            consumer_a.clone(),
        );
        assert!(matches!(
            store.prepare_intent_revision_consumption(&consumer_a, &claim_same_consumer),
            Err(CodeCommandStoreError::InvalidIntent)
        ));

        store
            .complete_code_command_success(&consumer_a.identity, "first consumer complete")
            .expect("first consumer terminal persisted");
        let consumer_b = durable_revision_consumer("consumer-b");
        store
            .admit_code_command(consumer_b.clone())
            .expect("second consumer admitted");
        let claim_b =
            revision_consumption_claim(&source_b, &recovery_b, &terminal_b, "intent-b", consumer_b);
        let reused_event = IntentRevisionConsumption {
            claim: claim_b,
            consumer_intent_event_id: consumption_a.consumer_intent_event_id,
            consumer_intent_sequence: consumption_a.consumer_intent_sequence,
        };
        assert!(matches!(
            store.record_intent_revision_consumption(&reused_event),
            Err(CodeCommandStoreError::InvalidIntent)
        ));
    }

    #[test]
    fn exact_checkpoint_retry_resyncs_post_write_failure_without_duplicate_row() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let intent = durable_test_intent("checkpoint-retry");
        assert!(matches!(
            store.admit_code_command(intent.clone()),
            Ok(CodeCommandAdmission::Execute { .. })
        ));
        let resolutions = [("input-1".to_string(), "answered".to_string())];

        store.fail_next_durable_sync_after_write_for_test();
        store
            .checkpoint_pending_interaction_resolutions(&intent.identity, &resolutions)
            .expect_err("first checkpoint reports the injected post-write sync failure");
        store.fail_next_events_log_resync_for_test();
        store
            .checkpoint_pending_interaction_resolutions(&intent.identity, &resolutions)
            .expect_err("exact checkpoint retry must attempt a fresh event-log sync");
        store
            .checkpoint_pending_interaction_resolutions(&intent.identity, &resolutions)
            .expect("exact retry re-syncs the visible row before acknowledging");

        let replay = store.load_code_workflow_replay().expect("workflow replay");
        assert_eq!(
            replay
                .events
                .iter()
                .filter(|event| matches!(
                    &event.event,
                    CodeWorkflowEventKind::InteractionResolved {
                        command: Some(command),
                        ..
                    } if command == &intent.identity
                ))
                .count(),
            1,
            "the exact retry must sync rather than append a duplicate checkpoint"
        );
    }

    #[test]
    fn exact_admission_retry_resyncs_post_write_failure_without_redispatch() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let intent = durable_test_intent("admission-retry");

        store.fail_next_durable_sync_after_write_for_test();
        store
            .admit_code_command(intent.clone())
            .expect_err("first admission reports the injected post-write sync failure");
        store.fail_next_events_log_resync_for_test();
        store
            .admit_code_command(intent.clone())
            .expect_err("exact admission retry must attempt a fresh event-log sync");
        assert_eq!(
            store
                .admit_code_command(intent.clone())
                .expect("exact admission retry re-syncs before returning existing"),
            CodeCommandAdmission::Existing {
                status: CodeCommandStatus::Pending
            }
        );

        let replay = store.load_code_workflow_replay().expect("workflow replay");
        assert_eq!(
            replay
                .events
                .iter()
                .filter(|event| matches!(
                    &event.event,
                    CodeWorkflowEventKind::CommandIntentPersisted { command }
                        if command == &intent
                ))
                .count(),
            1,
            "the exact retry must not append a second admission"
        );
    }

    #[test]
    fn exact_combined_terminal_retry_resyncs_post_write_failure() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let intent = durable_test_intent("combined-retry");
        store
            .admit_code_command(intent.clone())
            .expect("command intent admitted");
        let resolutions = [
            ("input-1".to_string(), "answered".to_string()),
            ("review-1".to_string(), "confirm".to_string()),
        ];

        store.fail_next_durable_sync_after_write_for_test();
        store
            .complete_code_command_success_with_interaction_resolutions(
                &intent.identity,
                "done",
                &resolutions,
            )
            .expect_err("first terminal write reports the injected sync failure");
        store.fail_next_events_log_resync_for_test();
        store
            .complete_code_command_success_with_interaction_resolutions(
                &intent.identity,
                "done",
                &resolutions,
            )
            .expect_err("exact combined terminal retry must attempt a fresh event-log sync");
        assert_eq!(
            store
                .complete_code_command_success_with_interaction_resolutions(
                    &intent.identity,
                    "done",
                    &resolutions,
                )
                .expect("exact terminal retry re-syncs before ACK"),
            CodeCommandStatus::Succeeded {
                summary: "done".to_string()
            }
        );

        let replay = store.load_code_workflow_replay().expect("workflow replay");
        assert_eq!(
            replay
                .events
                .iter()
                .filter(|event| matches!(
                    &event.event,
                    CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                        command,
                        ..
                    } if command == &intent.identity
                ))
                .count(),
            1
        );
        assert!(matches!(
            store.complete_code_command_success_with_interaction_resolutions(
                &intent.identity,
                "done",
                &resolutions[..1],
            ),
            Err(CodeCommandStoreError::TerminalConflict { .. })
        ));
        let reversed = [resolutions[1].clone(), resolutions[0].clone()];
        assert!(matches!(
            store.complete_code_command_success_with_interaction_resolutions(
                &intent.identity,
                "done",
                &reversed,
            ),
            Err(CodeCommandStoreError::TerminalConflict { .. })
        ));
        let with_unrelated = [
            resolutions[0].clone(),
            ("unrelated-review".to_string(), "confirm".to_string()),
            resolutions[1].clone(),
        ];
        assert!(matches!(
            store.complete_code_command_success_with_interaction_resolutions(
                &intent.identity,
                "done",
                &with_unrelated,
            ),
            Err(CodeCommandStoreError::TerminalConflict { .. })
        ));
        assert!(matches!(
            store.complete_code_command_success(&intent.identity, "done"),
            Err(CodeCommandStoreError::TerminalConflict { .. })
        ));
    }

    #[test]
    fn combined_terminal_retry_rejects_plain_success_plus_unrelated_resolution() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let intent = durable_test_intent("plain-success-with-unrelated-resolution");
        store
            .admit_code_command(intent.clone())
            .expect("command intent admitted");
        store
            .complete_code_command_success(&intent.identity, "done")
            .expect("plain terminal success");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
                interaction_id: "review-1".to_string(),
                resolution: "confirm".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .expect("unrelated workflow resolution");

        assert!(matches!(
            store.complete_code_command_success_with_interaction_resolved(
                &intent.identity,
                "done",
                "review-1",
                "confirm",
            ),
            Err(CodeCommandStoreError::TerminalConflict { .. })
        ));
    }

    #[test]
    fn exact_failure_terminal_retry_resyncs_post_write_failure() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let intent = durable_test_intent("failure-retry");
        store
            .admit_code_command(intent.clone())
            .expect("command intent admitted");
        let resolutions = [
            ("input-1".to_string(), "first".to_string()),
            ("input-2".to_string(), "second".to_string()),
        ];

        store.fail_next_durable_sync_after_write_for_test();
        store
            .complete_code_command_failure_with_interaction_resolutions(
                &intent.identity,
                "failed",
                &resolutions,
            )
            .expect_err("first terminal write reports the injected sync failure");
        store.fail_next_events_log_resync_for_test();
        store
            .complete_code_command_failure_with_interaction_resolutions(
                &intent.identity,
                "failed",
                &resolutions,
            )
            .expect_err("exact failure retry must attempt a fresh event-log sync");
        assert_eq!(
            store
                .complete_code_command_failure_with_interaction_resolutions(
                    &intent.identity,
                    "failed",
                    &resolutions,
                )
                .expect("exact failure retry re-syncs before ACK"),
            CodeCommandStatus::Failed {
                reason: "failed".to_string()
            }
        );
        assert!(matches!(
            store.complete_code_command_failure(&intent.identity, "failed"),
            Err(CodeCommandStoreError::TerminalConflict { .. })
        ));
        let reversed = [resolutions[1].clone(), resolutions[0].clone()];
        assert!(matches!(
            store.complete_code_command_failure_with_interaction_resolutions(
                &intent.identity,
                "failed",
                &reversed,
            ),
            Err(CodeCommandStoreError::TerminalConflict { .. })
        ));
    }

    #[test]
    fn failure_terminal_atomically_carries_retry_gate_and_legacy_rows_default_closed() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let intent = durable_test_intent("phase1-retry-terminal");
        store
            .admit_code_command(intent.clone())
            .expect("command intent admitted");
        let retry = Phase1RetryIntentReview {
            interaction_id: "intent-review-retry-1".to_string(),
            intent_id: "intent-1".to_string(),
            intent_spec_id: "intent-spec-1".to_string(),
            source_interaction_id: "intent-review-source".to_string(),
            source_resolution: "confirm".to_string(),
            source_phase1_turn_id: intent.identity.command_id.clone(),
            start_seed_digest: "ab".repeat(32),
        };
        store
            .complete_code_command_failure_with_interaction_resolutions_and_retry_intent_review(
                &intent.identity,
                "planner failed before formal write",
                &[],
                Some(&retry),
            )
            .expect("failure and retry gate share one durable terminal");

        let replay = store
            .load_code_workflow_replay_committed()
            .expect("committed workflow replay");
        let failure = replay
            .events
            .iter()
            .find_map(|event| match &event.event {
                CodeWorkflowEventKind::CommandTerminalFailure {
                    command,
                    retry_intent_review,
                    ..
                } if command == &intent.identity => Some(retry_intent_review.clone()),
                _ => None,
            })
            .expect("one failure terminal");
        assert_eq!(failure, Some(retry.clone()));
        assert!(replay.events.iter().all(|event| !matches!(
            &event.event,
            CodeWorkflowEventKind::IntentReviewRequested { .. }
        )));

        let event = CodeWorkflowEventKind::CommandTerminalFailure {
            command: intent.identity,
            reason: "legacy failure".to_string(),
            interaction_resolutions: Vec::new(),
            retry_intent_review: Some(retry),
        };
        let mut legacy = serde_json::to_value(event).expect("serialize terminal");
        legacy
            .as_object_mut()
            .expect("terminal JSON object")
            .remove("retry_intent_review");
        assert!(matches!(
            serde_json::from_value::<CodeWorkflowEventKind>(legacy)
                .expect("legacy terminal without additive field"),
            CodeWorkflowEventKind::CommandTerminalFailure {
                retry_intent_review: None,
                ..
            }
        ));
    }

    #[test]
    fn command_status_ack_resyncs_visible_post_write_intent() {
        let tmp = TempDir::new().expect("tmp dir");
        let store = SessionJsonlStore::new(tmp.path().to_path_buf());
        let intent = durable_test_intent("status-retry");

        store.fail_next_durable_sync_after_write_for_test();
        store
            .admit_code_command(intent.clone())
            .expect_err("first admission reports the injected post-write sync failure");
        store.fail_next_events_log_resync_for_test();
        store
            .code_command_intent_status(&intent.identity)
            .expect_err("status ACK must attempt a fresh event-log sync");
        assert_eq!(
            store
                .code_command_intent_status(&intent.identity)
                .expect("status retry re-syncs the visible intent"),
            Some((intent, CodeCommandStatus::Pending))
        );
    }
}
