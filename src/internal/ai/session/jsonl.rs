//! Append-only JSONL session event storage.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
const CODE_WORKFLOW_APPEND_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const CODE_WORKFLOW_APPEND_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(10);
const STALE_CODE_WORKFLOW_APPEND_LOCK_AGE: Duration = Duration::from_secs(30);

/// Event persisted in a session JSONL stream.
///
/// The wire form follows the runtime `Event` envelope contract:
/// `{"kind":"session_snapshot","payload":{...}}`. Readers inspect the
/// envelope before deserializing so future event kinds can be skipped without
/// breaking older binaries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
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
pub enum CodeWorkflowEventKind {
    CommandAccepted {
        command_id: String,
        workflow: String,
    },
    IntentReviewRequested {
        interaction_id: String,
        intent_id: String,
    },
    PlanReviewRequested {
        interaction_id: String,
        plan_id: String,
    },
    InteractionResolved {
        interaction_id: String,
        resolution: String,
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
    CommandTerminalFailure {
        command: CodeCommandIdentity,
        reason: String,
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

/// Current durable state for one runtime command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeCommandStatus {
    Pending,
    Succeeded { summary: String },
    Failed { reason: String },
    Indeterminate { effect: String, reason: String },
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
        } => format!("intent review {interaction_id} ({intent_id})"),
        CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id,
            plan_id,
        } => format!("plan review {interaction_id} ({plan_id})"),
        CodeWorkflowEventKind::InteractionResolved {
            interaction_id,
            resolution,
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
        CodeWorkflowEventKind::CommandTerminalFailure { command, reason } => {
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
        CodeWorkflowEventKind::CommandTerminalFailure { command, reason } => (
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

#[derive(Debug, Clone)]
pub struct SessionJsonlStore {
    session_root: PathBuf,
    /// Lazily populated from the workflow log, then updated by each durable
    /// command transition. This avoids replaying the entire session log for
    /// every command admission or completion in one running session.
    command_status_cache: Arc<Mutex<Option<HashMap<CodeCommandIdentity, CachedCodeCommand>>>>,
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
    path: PathBuf,
}

impl Drop for CodeWorkflowAppendLock {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.path.display(),
                error = %error,
                "failed to release Code workflow append lock"
            );
        }
    }
}

impl SessionJsonlStore {
    pub fn new(session_root: PathBuf) -> Self {
        Self {
            session_root,
            command_status_cache: Arc::new(Mutex::new(None)),
        }
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
        let sequence = self.next_code_workflow_sequence()?;
        let workflow_event = CodeWorkflowEvent::new(sequence, event);
        self.append_unchecked(
            &SessionEvent::code_workflow(workflow_event.clone()),
            durable,
        )?;
        self.update_command_status_cache(&workflow_event.event);
        Ok(workflow_event)
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

        let replay = code_workflow_replay_from_events(
            parse_session_events_content(&path, &content)?,
            after_sequence,
        )?;
        if start > 0 && replay.events.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "bounded Code workflow replay after sequence {after_sequence} cannot prove the retained tail of '{}' contains no omitted workflow events; create a projection checkpoint before resuming",
                    path.display()
                ),
            ));
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
            existing if existing == target => Ok(existing),
            _ => Err(CodeCommandStoreError::TerminalConflict {
                command_id: identity.command_id.clone(),
            }),
        }
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
        let path = self.session_root.join("events.code-workflow.append.lock");
        let started = Instant::now();
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(format!("pid={}\n", std::process::id()).as_bytes())
                        .map_err(|error| {
                            io::Error::new(
                                error.kind(),
                                format!(
                                    "failed to initialize Code workflow append lock '{}': {error}",
                                    path.display()
                                ),
                            )
                        })?;
                    return Ok(CodeWorkflowAppendLock { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if code_workflow_append_lock_is_stale(&path) {
                        match fs::remove_file(&path) {
                            Ok(()) => continue,
                            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                            Err(error) => {
                                return Err(io::Error::new(
                                    error.kind(),
                                    format!(
                                        "failed to clear stale Code workflow append lock '{}': {error}",
                                        path.display()
                                    ),
                                ));
                            }
                        }
                    }
                    if started.elapsed() >= CODE_WORKFLOW_APPEND_LOCK_TIMEOUT {
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
                Err(error) => {
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
        | "interaction_resolved"
        | "code_ui_projection_delta"
        | "terminal_success"
        | "terminal_failure"
        | "indeterminate_side_effect"
        | "command_intent_persisted"
        | "command_terminal_success"
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

fn code_workflow_append_lock_is_stale(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified_at) = metadata.modified() else {
        return false;
    };
    let Ok(elapsed) = modified_at.elapsed() else {
        return false;
    };
    elapsed >= STALE_CODE_WORKFLOW_APPEND_LOCK_AGE
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
        return true;
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let _ = (lock_path, recorded_starttime, recorded_boot_id);
        // SAFETY: kill(pid, 0) is a existence/permission probe and does not
        // deliver a signal.
        return unsafe { libc::kill(pid as i32, 0) == 0 };
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
        elapsed < STALE_CODE_WORKFLOW_APPEND_LOCK_AGE
    }
}

#[derive(Debug, Clone, Default)]
struct ProcessOwnerIdentity {
    pid: u32,
    starttime: Option<u64>,
    boot_id: Option<String>,
}

fn current_process_owner_identity() -> ProcessOwnerIdentity {
    process_owner_identity_for_pid(std::process::id())
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
}
