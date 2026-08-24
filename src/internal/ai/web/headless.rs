//! Headless web-only **lifecycle** host for non-Codex providers.
//!
//! The default Web launch with `--provider <X>` (X != codex) builds a [`HeadlessCodeRuntime`] that
//! owns session construction, worker spawn, approval listeners, persistence
//! helpers, and shutdown. The production browser write path is
//! [`super::agent_runtime_adapter::AgentRuntimeCodeUiAdapter`] (see W3-03):
//! plain messages route through Phase 0 (`phase0_plan_tool_loop_config`) so
//! direct chat cannot bypass the default mutating gate; slash/`/`-prefixed
//! messages remain an explicit direct tool loop.
//!
//! Confirmed plan execution still goes through
//! [`crate::internal::ai::runtime::plan_execution`] /
//! `ensure_plan_execution_mutating_gate`. Full IntentSpec → Phase 1 → repair
//! parity is pinned by the completed GATE-WEB-PLAN regression gate.

#[cfg(feature = "test-provider")]
use std::sync::atomic::AtomicUsize;
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{self, Read},
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, anyhow};
use async_trait::async_trait;
use chrono::Utc;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::{
    sync::{Mutex, mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{
    agent_runtime_adapter::{AgentRuntimeCodeUiAdapter, CodeUiLifecycleShutdown},
    code_ui::{
        CodeUiApiError, CodeUiApplyToFuture, CodeUiCapabilities, CodeUiCommandAdapter,
        CodeUiEventType, CodeUiInteractionKind, CodeUiInteractionOption, CodeUiInteractionRequest,
        CodeUiInteractionResponse, CodeUiInteractionStatus, CodeUiPatchChange,
        CodeUiPatchsetSnapshot, CodeUiPlanSnapshot, CodeUiPlanStep, CodeUiReadModel, CodeUiSession,
        CodeUiSessionSnapshot, CodeUiSessionStatus, CodeUiToolCallSnapshot, CodeUiTranscriptEntry,
        CodeUiTranscriptEntryKind,
    },
    sse_wire::CodeUiWorkflowHub,
    web_admission::{
        CODE_UI_WEB_TURN_KIND, InFlightTurn, PHASE1_ATTEMPT_ADMITTING, PHASE1_ATTEMPT_CANCELLED,
        PHASE1_ATTEMPT_MUTATING, PHASE1_ATTEMPT_PLANNING, PHASE1_ATTEMPT_SETTLED,
        PRE_START_CANCELLED, PRE_START_STARTED, PRE_START_UNSTARTED, PreStartTurn,
        WebCodeUiAdmission, WebCodeUiAdmissionInit, WebTurnMode, consumes_intent_revision,
        intent_revision_modify_note, release_web_turn, wait_for_web_turn_start,
    },
};
use crate::internal::ai::{
    agent::runtime::{ToolLoopCancellation, run_tool_loop_with_history_and_observer},
    completion::{
        CompletionError, CompletionModel, CompletionStreamEvent, CompletionUsage,
        CompletionUsageSummary, Message,
    },
    runtime::{
        AgentRuntimeHandle, AgentRuntimeWorker, AgentRuntimeWorkerConfig, AgentSnapshot,
        DeferredPlanExecutionExecutor, ExecutionControlService, InteractionResponse,
        InteractionState, RuntimeCommandDurability, RuntimeExecutionContext,
        RuntimeInteractionDelivery, RuntimeTurnExecution, RuntimeTurnExecutor, RuntimeWorkerError,
        TurnRequest, is_plan_execution_turn,
        phase0::{
            IntentReviewDecision, open_intent_review_from_workflow, phase0_plan_tool_loop_config,
            phase0_planning_prompt, phase0_revision_help_message, phase0_revision_prompt,
        },
        phase1::{
            NetworkPolicyAckDelivery, Phase1CheckoutBinding, Phase1PersistedPlan,
            Phase1RetryIntentReviewState, Phase1ReviewContext, PlanReviewAckDelivery,
            clear_phase1_review_context, clear_phase1_start_seed, compile_submitted_plan,
            load_phase1_review_context, load_phase1_start_seed, network_policy_interaction_id,
            open_network_policy_from_workflow, open_plan_review_from_workflow,
            open_review_gate_phase_turn_id, pending_plan_revision_from_workflow,
            persist_phase1_review_context, phase1_plan_tool_loop_config, phase1_planning_prompt,
            phase1_retry_intent_review_state, phase1_source_resolution_matches_seed,
            validate_phase1_context_session_budget, validate_phase1_retry_intent_review_for_seed,
            validate_phase1_review_context_preflight,
        },
        runtime_worker_adapter_message, submit_confirmed_plan_execution,
    },
    sandbox::{ExecApprovalRequest, NetworkAccess, ReviewDecision},
    session::{
        CodeCommandIdentity, CodeCommandIntent, CodeCommandStatus, CodeWorkflowEventKind,
        INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION, IntentRevisionConsumption,
        IntentRevisionConsumptionClaim, IntentRevisionRecovery, MAX_INTENT_REVISION_NOTE_BYTES,
        Phase1RetryIntentReview, SessionJsonlStore, SessionState, SessionStore,
        jsonl::{
            INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_REASON, ValidatedIntentRevisionReceiptIndex,
            ValidatedIntentRevisionSourceTerminal, validated_intent_revision_consumption_receipts,
        },
    },
    tools::{
        ToolOutput, ToolRegistry,
        context::{
            StepStatus, SubmitPlanDraftArgs, UpdatePlanArgs, UserInputAnswer, UserInputQuestion,
            UserInputRequest, UserInputResponse, validate_submit_plan_draft_value_bounds,
        },
    },
};

fn stable_phase1_gate_id(prefix: &str, source_interaction_id: &str) -> String {
    use sha2::Digest as _;

    let mut identity = sha2::Sha256::new();
    identity.update(prefix.as_bytes());
    identity.update(b"\0");
    identity.update(source_interaction_id.as_bytes());
    format!("{prefix}-{}", hex::encode(identity.finalize()))
}

fn phase1_durable_input(confirmed: &ConfirmedIntentForPhase1) -> String {
    // Browser command idempotency is intentionally keyed by the caller's raw
    // text contract: the same command id plus the same text is a retry, while
    // different text conflicts. Checkout/source lineage remains validated by
    // the durable seed and Phase 1 context, not by changing that public hash.
    confirmed
        .revision_note
        .clone()
        .unwrap_or_else(|| "Phase 1 plan generation".to_string())
}

fn phase1_retry_intent_review(
    confirmed: &ConfirmedIntentForPhase1,
    phase1_turn_id: &str,
) -> Option<Phase1RetryIntentReview> {
    confirmed
        .revision_source_interaction_id
        .is_none()
        .then(|| Phase1RetryIntentReview {
            interaction_id: format!("intent-review-retry-{}", uuid::Uuid::new_v4()),
            intent_id: confirmed.intent_id.clone(),
            intent_spec_id: confirmed.intent_spec_id.clone(),
            source_interaction_id: confirmed.source_interaction_id.clone(),
            source_resolution: "confirm".to_string(),
            source_phase1_turn_id: phase1_turn_id.to_string(),
            start_seed_digest: confirmed.seed_digest.clone(),
        })
}

/// Return the exact cancelled pre-formal Phase 1 retry whose stale browser
/// projection may still show `Thinking` after a crash. The candidate must be
/// the latest admitted command, must have a later durable cancel resolution,
/// and must not cross the formal-write boundary. Those constraints keep this
/// recovery from clearing an unrelated ordinary turn merely because the
/// session contains an older cancelled retry.
fn cancelled_phase1_retry_projection_lineage<'a>(
    events: impl IntoIterator<Item = &'a CodeWorkflowEventKind>,
    snapshot: &CodeUiSessionSnapshot,
) -> io::Result<Option<Phase1RetryIntentReview>> {
    if !matches!(
        snapshot.status,
        CodeUiSessionStatus::Thinking | CodeUiSessionStatus::ExecutingTool
    ) {
        return Ok(None);
    }

    let events = events.into_iter().collect::<Vec<_>>();
    let Some((intent_index, phase1_turn_id)) =
        events
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, event)| match event {
                CodeWorkflowEventKind::CommandIntentPersisted { command } => {
                    Some((index, command.identity.command_id.as_str()))
                }
                _ => None,
            })
    else {
        return Ok(None);
    };
    let Some((terminal_index, retry)) =
        events
            .iter()
            .enumerate()
            .find_map(|(index, event)| match event {
                CodeWorkflowEventKind::CommandTerminalFailure {
                    command,
                    retry_intent_review: Some(retry),
                    ..
                } if command.command_id == phase1_turn_id => Some((index, retry.clone())),
                _ => None,
            })
    else {
        return Ok(None);
    };
    if terminal_index <= intent_index {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Phase 1 retry command '{phase1_turn_id}' has a terminal row before its durable intent"
            ),
        ));
    }

    let state = phase1_retry_intent_review_state(events.iter().copied(), phase1_turn_id)?;
    let Phase1RetryIntentReviewState::Resolved { review, resolution } = state else {
        return Ok(None);
    };
    if review != retry
        || IntentReviewDecision::from_wire_id(&resolution) != Some(IntentReviewDecision::Cancel)
    {
        return Ok(None);
    }

    let resolved_after_terminal = events.iter().skip(terminal_index + 1).any(|event| {
        matches!(
            event,
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                intent_revision_consumption: None,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                resolution,
                ..
            } if interaction_id == &retry.interaction_id
                && IntentReviewDecision::from_wire_id(resolution)
                    == Some(IntentReviewDecision::Cancel)
        ) || matches!(
            event,
            CodeWorkflowEventKind::InteractionResolved {
                prior_interaction_resolutions,
                intent_revision_consumption: None,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                prior_interaction_resolutions,
                ..
            } if prior_interaction_resolutions.iter().any(|(interaction_id, resolution)| {
                interaction_id == &retry.interaction_id
                    && IntentReviewDecision::from_wire_id(resolution)
                        == Some(IntentReviewDecision::Cancel)
            })
        )
    });
    if !resolved_after_terminal {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Phase 1 retry interaction '{}' was resolved before its terminal authority existed",
                retry.interaction_id
            ),
        ));
    }

    let source_resolution = events[..terminal_index]
        .iter()
        .rev()
        .find_map(|event| match event {
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id,
                resolution,
                intent_revision_consumption: None,
                ..
            }
            | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                interaction_id,
                resolution,
                ..
            } if interaction_id == &retry.source_interaction_id => Some(resolution.as_str()),
            _ => None,
        });
    if !source_resolution
        .is_some_and(|resolution| resolution.eq_ignore_ascii_case(&retry.source_resolution))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Phase 1 retry interaction '{}' has no matching earlier source resolution",
                retry.interaction_id
            ),
        ));
    }
    if events.iter().any(|event| {
        matches!(
            event,
            CodeWorkflowEventKind::Phase1FormalWriteStarted {
                phase1_turn_id: event_turn_id,
                ..
            } if event_turn_id == phase1_turn_id
        )
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Phase 1 retry command '{phase1_turn_id}' also crossed the formal-write boundary"
            ),
        ));
    }

    let source_is_resolved = snapshot.interactions.iter().any(|interaction| {
        interaction.id == retry.source_interaction_id
            && interaction.status == CodeUiInteractionStatus::Resolved
    });
    Ok(source_is_resolved.then_some(retry))
}

/// Capabilities advertised by the headless lifecycle / web adapter mount.
///
/// `messageInput`, streaming text, tool calls, plan updates, patchsets,
/// approval interactions, structured questions, and session resume are
/// delivered. Plain chat enters Phase 0 plan routing; full IntentSpec →
/// Phase 1 → repair parity remains GATE-WEB-PLAN.
pub fn headless_capabilities() -> CodeUiCapabilities {
    CodeUiCapabilities {
        message_input: true,
        streaming_text: true,
        plan_updates: true,
        tool_calls: true,
        patchsets: true,
        interactive_approvals: true,
        structured_questions: true,
        provider_session_resume: true,
        command_idempotency: true,
    }
}

/// Bound graceful shutdown waits so a stuck provider cannot leave the CLI
/// indefinitely unresponsive. The timeout error is deliberately actionable;
/// the caller must surface it rather than silently treating shutdown as clean.
const HEADLESS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const HEADLESS_BROWSER_PRINCIPAL: &str = "web-headless-browser";
const PHASE1_WRITER_LOCK_FILE: &str = "phase1-writer.lock";
const PENDING_INTENT_REVISION_FILE: &str = "pending_revision.json";
const INTENT_REVISION_HMAC_KEY_FILE: &str = "revision_hmac.key";
// The sidecar JSON-encodes an already-serialized IntentSpec string. Quotes,
// backslashes, and a bounded note can therefore expand beyond the 8 MiB spec
// limit even though every individual input is valid. Keep the envelope bounded
// while leaving enough room for the worst practical JSON escaping overhead.
const MAX_PENDING_INTENT_REVISION_BYTES: u64 = 24 * 1024 * 1024;
const INTENT_REVISION_SIDECAR_DIGEST_DOMAIN: &[u8] = b"libra.intent-revision-sidecar.v1";
const INTENT_REVISION_SIDECAR_SCHEMA_VERSION: u32 = 1;
const MAX_DURABLE_INTENT_SPEC_BYTES: u64 = 8 * 1024 * 1024;
const MAX_DURABLE_INTENT_ID_BYTES: usize = 200;

#[derive(Clone, Debug)]
pub(crate) struct ConfirmedIntentForPhase1 {
    pub(crate) source_interaction_id: String,
    pub(crate) seed_digest: String,
    pub(crate) intent_id: String,
    pub(crate) intent_spec_id: String,
    pub(crate) intent_spec_json: String,
    pub(crate) revision_note: Option<String>,
    pub(crate) checkout: Option<Phase1CheckoutBinding>,
    pub(crate) revision_source_interaction_id: Option<String>,
    pub(crate) prior_plan: Option<crate::internal::ai::orchestrator::types::ExecutionPlanSpec>,
    pub(crate) prior_plan_id: Option<String>,
    pub(crate) prior_persisted_plan: Phase1PersistedPlan,
    pub(crate) phase1_turn_id_override: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedNetworkGate {
    pub(crate) plan_interaction_id: String,
    pub(crate) network_interaction_id: String,
    pub(crate) gate_turn_id: String,
    pub(crate) context: Phase1ReviewContext,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedPlanGate {
    pub(crate) network_interaction_id: String,
    pub(crate) plan_interaction_id: String,
    pub(crate) gate_turn_id: String,
    pub(crate) context: Phase1ReviewContext,
    pub(crate) workspace_warning: Option<String>,
}

pub(crate) enum HeadlessPhase1Command {
    Start {
        confirmed: ConfirmedIntentForPhase1,
        admitted: Option<oneshot::Sender<Result<Phase1StartAdmission, RuntimeWorkerError>>>,
        start: Option<oneshot::Receiver<Result<(), String>>>,
    },
    PrepareNetwork {
        plan_interaction_id: String,
        reply: oneshot::Sender<Result<PreparedNetworkGate, CodeUiApiError>>,
    },
    ParkNetwork {
        prepared: PreparedNetworkGate,
        reply: oneshot::Sender<Result<(), String>>,
    },
    PreparePlanBack {
        network_interaction_id: String,
        reply: oneshot::Sender<Result<PreparedPlanGate, String>>,
    },
    ParkPlanBack {
        prepared: PreparedPlanGate,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// FIFO barrier behind every Start already enqueued for one Phase 1
    /// generation. Until this command is observed, its terminal attempt state
    /// remains a tombstone so an aborted duplicate cannot recreate Planning.
    CleanupAttempt { phase1_turn_id: String },
    /// W2-04: Network Allow has closed the human gate; admit confirmed plan
    /// execution onto the serialized runtime queue.
    StartPlanExecution {
        network_interaction_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase1StartAdmission {
    Execute,
    Existing,
}

/// In-memory + durable baseline for IntentSpec Modify → next plain message.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingIntentRevision {
    intent_spec: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authority: Option<PendingIntentRevisionAuthority>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingIntentRevisionAuthority {
    schema_version: u32,
    #[serde(default)]
    legacy_terminal: bool,
    interaction_id: String,
    command: CodeCommandIdentity,
    terminal_event_id: uuid::Uuid,
    terminal_sequence: u64,
    intent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sidecar_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreparedIntentRevision {
    schema_version: u32,
    interaction_id: String,
    command: CodeCommandIdentity,
    intent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    sidecar_digest: String,
}

/// Durable pre-admission binding for the one Web command allowed to consume
/// an Active revision. The ordinary command event id/sequence do not exist
/// until Runtime admission fsyncs them, so this state commits the full intent
/// first and lets restart distinguish "not admitted" from "admitted but not
/// yet promoted to Consuming" without weakening first-writer validation.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaimingIntentRevision {
    schema_version: u32,
    active: PendingIntentRevision,
    claim: IntentRevisionConsumptionClaim,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsumingIntentRevision {
    schema_version: u32,
    active: PendingIntentRevision,
    consumption: IntentRevisionConsumption,
}

struct PreparedWebIntentRevisionConsumption {
    pending: PendingIntentRevision,
    consumption: Option<IntentRevisionConsumption>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IntentRevisionTerminalAuthority {
    interaction_id: String,
    command: CodeCommandIdentity,
    terminal_event_id: uuid::Uuid,
    terminal_sequence: u64,
    intent_id: String,
    sidecar_digest: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntentRevisionSidecarEnvelope {
    #[serde(default)]
    intent_spec: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authority: Option<PendingIntentRevisionAuthority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prepared: Option<PreparedIntentRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claiming: Option<ClaimingIntentRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    consuming: Option<ConsumingIntentRevision>,
}

// These authenticated states are handled as owned values throughout recovery.
// Keep them inline so pattern matching remains explicit at the protocol boundary.
#[allow(clippy::large_enum_variant)]
enum LoadedIntentRevisionSidecar {
    Prepared(PreparedIntentRevision),
    Active(PendingIntentRevision),
    Claiming(ClaimingIntentRevision),
    Consuming(ConsumingIntentRevision),
}

struct Phase1GenerationControl {
    mutation_started: Arc<AtomicBool>,
    attempt_state: Arc<AtomicU8>,
    cancellation: CancellationToken,
}

impl PendingIntentRevision {
    fn revision_request(&self, follow_up: &str) -> String {
        let follow_up = follow_up.trim();
        match (
            self.note
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty()),
            follow_up.is_empty(),
        ) {
            (Some(note), true) => note.to_string(),
            (Some(note), false) => format!("{note}\n\nAdditional follow-up:\n{follow_up}"),
            (None, _) => follow_up.to_string(),
        }
    }
}

#[derive(Clone)]
pub struct HeadlessSessionPersistence {
    store: Arc<SessionStore>,
    state: Arc<Mutex<SessionState>>,
    projection_store: SessionJsonlStore,
    projection_checkpoint: Arc<Mutex<HeadlessProjectionCheckpoint>>,
    durability_repo_id: String,
    durability_session_id: String,
    /// Fan-out for SSE wire v2 (same durable sequence as projection appends).
    workflow_hub: Arc<CodeUiWorkflowHub>,
    _phase1_writer_lock: Arc<Phase1WriterLock>,
    #[cfg(feature = "test-provider")]
    record_user_message_hook: Option<HeadlessRecordUserMessageHook>,
    #[cfg(feature = "test-provider")]
    phase1_start_enqueued_hook: Option<HeadlessRecordUserMessageHook>,
    #[cfg(feature = "test-provider")]
    interaction_registration_hook: Option<HeadlessRecordUserMessageHook>,
    #[cfg(feature = "test-provider")]
    interaction_checkpoint_hook: Option<HeadlessRecordUserMessageHook>,
    #[cfg(feature = "test-provider")]
    phase1_formal_write_hook: Option<HeadlessRecordUserMessageHook>,
    #[cfg(feature = "test-provider")]
    snapshot_persist_failure_countdown: Arc<AtomicUsize>,
}

struct Phase1WriterLock {
    file: File,
    lock_path: PathBuf,
    session_id: String,
    claimed: AtomicBool,
}

/// Process-lifetime exclusive writer lease for a durable headless session.
///
/// Resume callers acquire this before reloading and folding the session so a
/// contender can never carry a projection cursor read before the prior writer
/// released the lease into a new writable runtime. Clones share a one-shot
/// claim: they can authorize one independently constructed persistence graph,
/// whose own clones then retain the private guard.
#[derive(Clone)]
pub struct HeadlessSessionLease {
    inner: Arc<Phase1WriterLock>,
}

impl Drop for Phase1WriterLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl HeadlessSessionLease {
    fn claim_for_persistence(&self, store: &SessionStore, session_id: &str) -> io::Result<()> {
        let expected_lock_path = store.session_root(session_id).join(PHASE1_WRITER_LOCK_FILE);
        if self.inner.session_id != session_id || self.inner.lock_path != expected_lock_path {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Code session writer lease for '{}' cannot be attached to session '{session_id}' at '{}'",
                    self.inner.session_id,
                    expected_lock_path.display()
                ),
            ));
        }
        if !phase1_writer_lock_path_matches_file(&self.inner.lock_path, &self.inner.file)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Code session writer lease '{}' was replaced after it was opened; repair the session lock path before resuming",
                    self.inner.lock_path.display()
                ),
            ));
        }
        self.inner
            .claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "Code session writer lease for '{session_id}' already authorized a persistence runtime"
                    ),
                )
            })?;
        Ok(())
    }
}

fn open_phase1_writer_lock_file(path: &Path) -> io::Result<File> {
    #[cfg(not(any(unix, windows)))]
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "Code session writer lease '{}' cannot be acquired on this platform; Phase 1 writer leases require Unix or Windows identity checks",
                path.display()
            ),
        ));
    }

    #[cfg(any(unix, windows))]
    {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if phase1_writer_lock_metadata_is_unsafe(&metadata) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Code session writer lease '{}' must be a regular non-link file",
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
            use std::os::unix::fs::OpenOptionsExt as _;

            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;

            use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = options.open(path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to open regular Code session writer lease '{}': {error}",
                    path.display()
                ),
            )
        })?;
        let metadata = file.metadata().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to inspect opened Code session writer lease '{}': {error}",
                    path.display()
                ),
            )
        })?;
        if phase1_writer_lock_metadata_is_unsafe(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Code session writer lease '{}' is not a regular non-link file",
                    path.display()
                ),
            ));
        }
        if !phase1_writer_lock_path_matches_file(path, &file)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Code session writer lease '{}' changed while it was opened",
                    path.display()
                ),
            ));
        }
        Ok(file)
    }
}

#[cfg(unix)]
fn phase1_writer_lock_metadata_is_unsafe(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink() || !metadata.is_file()
}

#[cfg(windows)]
fn phase1_writer_lock_metadata_is_unsafe(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_file()
}

#[cfg(not(any(unix, windows)))]
fn phase1_writer_lock_metadata_is_unsafe(_metadata: &std::fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn phase1_writer_lock_path_matches_file(path: &Path, file: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let opened = file.metadata()?;
    match std::fs::symlink_metadata(path) {
        Ok(current) if !phase1_writer_lock_metadata_is_unsafe(&current) => {
            Ok(opened.dev() == current.dev() && opened.ino() == current.ino())
        }
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn phase1_writer_lock_path_matches_file(path: &Path, file: &File) -> io::Result<bool> {
    use std::os::windows::fs::OpenOptionsExt as _;

    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    let current = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path);
    let current = match current {
        Ok(current) => current,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let metadata = current.metadata()?;
    if phase1_writer_lock_metadata_is_unsafe(&metadata) {
        return Ok(false);
    }
    Ok(windows_file_identity(file)? == windows_file_identity(&current)?)
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> io::Result<(u32, u64)> {
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle},
    };

    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut information) };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    let index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok((information.dwVolumeSerialNumber, index))
}

#[cfg(not(any(unix, windows)))]
fn phase1_writer_lock_path_matches_file(_path: &Path, _file: &File) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Code session writer lease identity checks are unsupported on this platform",
    ))
}

struct HeadlessProjectionCheckpoint {
    snapshot: CodeUiSessionSnapshot,
    sequence: u64,
}

/// Deterministic test-provider hook for pausing the durable admission write.
///
/// This is feature-gated so production code has no test timing surface. It
/// lets the Code UI regression suite prove that shutdown cannot finalize a
/// turn between durable admission and opening the executor start gate.
#[cfg(feature = "test-provider")]
#[derive(Clone)]
pub struct HeadlessRecordUserMessageHook {
    entered: Arc<AtomicBool>,
    released: Arc<AtomicBool>,
    entered_notify: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[cfg(feature = "test-provider")]
impl HeadlessRecordUserMessageHook {
    pub fn new() -> Self {
        Self {
            entered: Arc::new(AtomicBool::new(false)),
            released: Arc::new(AtomicBool::new(false)),
            entered_notify: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub async fn wait_until_entered(&self) {
        loop {
            if self.entered.load(Ordering::Acquire) {
                return;
            }
            let notified = self.entered_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.entered.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    pub fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.release.notify_waiters();
    }

    async fn wait(&self) {
        self.entered.store(true, Ordering::Release);
        self.entered_notify.notify_waiters();
        loop {
            if self.released.load(Ordering::Acquire) {
                return;
            }
            let notified = self.release.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.released.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(feature = "test-provider")]
impl Default for HeadlessRecordUserMessageHook {
    fn default() -> Self {
        Self::new()
    }
}

impl HeadlessSessionPersistence {
    pub fn acquire_session_lease(
        store: &SessionStore,
        session_id: &str,
    ) -> io::Result<HeadlessSessionLease> {
        let mut components = std::path::Path::new(session_id).components();
        let safe_component = matches!(
            (components.next(), components.next()),
            (Some(std::path::Component::Normal(_)), None)
        );
        if session_id.is_empty() || !safe_component {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to acquire a Code session lease for an unsafe session id",
            ));
        }
        let session_root = store.session_root(session_id);
        std::fs::create_dir_all(&session_root)?;
        let writer_lock_path = session_root.join(PHASE1_WRITER_LOCK_FILE);
        let writer_lock_file = open_phase1_writer_lock_file(&writer_lock_path)?;
        writer_lock_file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "Code session '{session_id}' already has a writable Phase 1 runtime; close it before resuming this session elsewhere"
                ),
            ),
            std::fs::TryLockError::Error(error) => error,
        })?;
        if !phase1_writer_lock_path_matches_file(&writer_lock_path, &writer_lock_file)? {
            let _ = writer_lock_file.unlock();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Code session writer lease '{}' was replaced while it was being acquired",
                    writer_lock_path.display()
                ),
            ));
        }
        Ok(HeadlessSessionLease {
            inner: Arc::new(Phase1WriterLock {
                file: writer_lock_file,
                lock_path: writer_lock_path,
                session_id: session_id.to_string(),
                claimed: AtomicBool::new(false),
            }),
        })
    }

    /// Construct persistence for callers that do not yet have a restored
    /// projection checkpoint. The first persisted snapshot becomes the
    /// checkpoint through normal fine-grained delta emission.
    pub fn new(store: Arc<SessionStore>, state: SessionState) -> io::Result<Self> {
        Self::with_projection_checkpoint(store, state, CodeUiSessionSnapshot::default(), 0)
    }

    /// Construct persistence from the durable legacy snapshot and its last
    /// workflow cursor. This is the resume path used by `libra code`.
    pub fn with_projection_checkpoint(
        store: Arc<SessionStore>,
        state: SessionState,
        initial_projection_snapshot: CodeUiSessionSnapshot,
        initial_projection_sequence: u64,
    ) -> io::Result<Self> {
        let lease = Self::acquire_session_lease(&store, &state.id)?;
        Self::with_projection_checkpoint_and_lease(
            store,
            state,
            initial_projection_snapshot,
            initial_projection_sequence,
            lease,
        )
    }

    pub fn with_projection_checkpoint_and_lease(
        store: Arc<SessionStore>,
        state: SessionState,
        initial_projection_snapshot: CodeUiSessionSnapshot,
        initial_projection_sequence: u64,
        lease: HeadlessSessionLease,
    ) -> io::Result<Self> {
        lease.claim_for_persistence(&store, &state.id)?;
        let HeadlessSessionLease { inner: lease } = lease;
        let mut projection_store = SessionJsonlStore::new(store.session_root(&state.id));
        let workflow_hub = Arc::new(CodeUiWorkflowHub::attach(&mut projection_store)?);
        Ok(Self {
            store,
            state: Arc::new(Mutex::new(state.clone())),
            projection_store,
            projection_checkpoint: Arc::new(Mutex::new(HeadlessProjectionCheckpoint {
                snapshot: initial_projection_snapshot,
                sequence: initial_projection_sequence,
            })),
            durability_repo_id: state.working_dir.clone(),
            durability_session_id: state.id.clone(),
            workflow_hub,
            _phase1_writer_lock: lease,
            #[cfg(feature = "test-provider")]
            record_user_message_hook: None,
            #[cfg(feature = "test-provider")]
            phase1_start_enqueued_hook: None,
            #[cfg(feature = "test-provider")]
            interaction_registration_hook: None,
            #[cfg(feature = "test-provider")]
            interaction_checkpoint_hook: None,
            #[cfg(feature = "test-provider")]
            phase1_formal_write_hook: None,
            #[cfg(feature = "test-provider")]
            snapshot_persist_failure_countdown: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Add a deterministic durable-admission pause for test-provider tests.
    #[cfg(feature = "test-provider")]
    pub fn with_record_user_message_hook(mut self, hook: HeadlessRecordUserMessageHook) -> Self {
        self.record_user_message_hook = Some(hook);
        self
    }

    /// Pause initial Confirm after its Start command is durably seeded and
    /// enqueued, but before the HTTP handler observes Runtime admission.
    #[cfg(feature = "test-provider")]
    pub fn with_phase1_start_enqueued_hook(mut self, hook: HeadlessRecordUserMessageHook) -> Self {
        self.phase1_start_enqueued_hook = Some(hook);
        self
    }

    /// Pause after a tool interaction is projected into the Web snapshot but
    /// before its snapshot persistence and runtime registration. This exposes
    /// the response-before-registration boundary to deterministic tests.
    #[cfg(feature = "test-provider")]
    pub fn with_interaction_registration_hook(
        mut self,
        hook: HeadlessRecordUserMessageHook,
    ) -> Self {
        self.interaction_registration_hook = Some(hook);
        self
    }

    /// Pause after a user-input / approval response checkpoint is durable and
    /// before its continuation is released to the tool loop.
    #[cfg(feature = "test-provider")]
    pub fn with_interaction_checkpoint_hook(mut self, hook: HeadlessRecordUserMessageHook) -> Self {
        self.interaction_checkpoint_hook = Some(hook);
        self
    }

    /// Pause after the durable Phase1FormalWriteStarted boundary and before
    /// the formal plan write, allowing shutdown ordering to be tested without
    /// timing races.
    #[cfg(feature = "test-provider")]
    pub fn with_phase1_formal_write_hook(mut self, hook: HeadlessRecordUserMessageHook) -> Self {
        self.phase1_formal_write_hook = Some(hook);
        self
    }

    /// Fail one snapshot persistence after `successful_persists` successful
    /// calls. A zero countdown disables the fault. This lets test-provider
    /// regressions pin acknowledgement boundaries without exposing a
    /// production fault-injection surface.
    #[cfg(feature = "test-provider")]
    pub fn fail_snapshot_persist_after_successes_for_test(&self, successful_persists: usize) {
        self.snapshot_persist_failure_countdown
            .store(successful_persists.saturating_add(1), Ordering::Release);
    }

    #[cfg(feature = "test-provider")]
    pub fn snapshot_persist_failure_countdown_for_test(&self) -> usize {
        self.snapshot_persist_failure_countdown
            .load(Ordering::Acquire)
    }

    #[cfg(feature = "test-provider")]
    pub fn clear_snapshot_persist_failure_for_test(&self) {
        self.snapshot_persist_failure_countdown
            .store(0, Ordering::Release);
    }

    #[cfg(feature = "test-provider")]
    pub(crate) async fn wait_after_phase1_start_enqueued(&self) {
        if let Some(hook) = self.phase1_start_enqueued_hook.as_ref() {
            hook.wait().await;
        }
    }

    #[cfg(feature = "test-provider")]
    async fn wait_before_interaction_registration(&self) {
        if let Some(hook) = self.interaction_registration_hook.as_ref() {
            hook.wait().await;
        }
    }

    #[cfg(feature = "test-provider")]
    async fn wait_after_interaction_checkpoint(&self) {
        if let Some(hook) = self.interaction_checkpoint_hook.as_ref() {
            hook.wait().await;
        }
    }

    #[cfg(feature = "test-provider")]
    async fn wait_after_phase1_formal_write_started(&self) {
        if let Some(hook) = self.phase1_formal_write_hook.as_ref() {
            hook.wait().await;
        }
    }

    /// SSE wire v2 durable fan-out for this session.
    pub fn workflow_hub(&self) -> Arc<CodeUiWorkflowHub> {
        self.workflow_hub.clone()
    }

    /// Adopt the durable projection checkpoint into the live session before
    /// startup recovery inspects it. The production `--resume` caller already
    /// folds this same checkpoint into the session it builds, making adoption
    /// a no-op there; direct-runtime callers rely on it so stale browser rows
    /// can be repaired in place. A fresh persistence (cursor 0) is skipped.
    /// Any projection deltas durable after the checkpoint cursor are folded in
    /// and the checkpoint is advanced so later delta emission stays exact.
    pub(crate) async fn adopt_projection_checkpoint(
        &self,
        session: &Arc<CodeUiSession>,
    ) -> io::Result<()> {
        let (bootstrap, after_sequence) = {
            let checkpoint = self.projection_checkpoint.lock().await;
            (checkpoint.snapshot.clone(), checkpoint.sequence)
        };
        if after_sequence == 0 {
            return Ok(());
        }
        let replay = self
            .projection_store
            .load_code_workflow_replay_since_committed(
                after_sequence,
                super::code_ui_projection::MAX_CODE_UI_PROJECTION_EVENTS,
                super::code_ui_projection::MAX_CODE_UI_PROJECTION_REPLAY_BYTES,
            )?;
        let folded =
            super::code_ui_projection::rebuild_code_ui_read_model_from_events(bootstrap, &replay)
                .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("failed to fold the durable Code UI projection checkpoint: {error}"),
                )
            })?;
        {
            let mut checkpoint = self.projection_checkpoint.lock().await;
            checkpoint.snapshot = folded.snapshot.clone();
            if let Some(last_sequence) = folded.last_sequence {
                checkpoint.sequence = last_sequence;
            }
        }
        session
            .replace_snapshot(CodeUiEventType::SessionUpdated, folded.snapshot)
            .await;
        Ok(())
    }

    /// Stable durable identity fields used by the runtime worker.
    pub fn worker_durability_config(&self) -> (RuntimeCommandDurability, String, String) {
        (
            RuntimeCommandDurability::new(self.projection_store.clone()),
            self.durability_repo_id.clone(),
            HEADLESS_BROWSER_PRINCIPAL.to_string(),
        )
    }

    pub fn durability_session_id(&self) -> &str {
        &self.durability_session_id
    }

    /// The shared execution-control service appends replayable Goal envelopes
    /// to this same per-session JSONL stream.
    pub fn goal_event_store(&self) -> SessionJsonlStore {
        self.projection_store.clone()
    }

    pub(crate) async fn record_user_message(
        &self,
        snapshot: CodeUiSessionSnapshot,
        content: &str,
    ) -> io::Result<()> {
        #[cfg(feature = "test-provider")]
        if let Some(hook) = self.record_user_message_hook.as_ref() {
            hook.wait().await;
        }
        let sequence = self.persist_projection_deltas(&snapshot).await?;
        let mut state = self.state.lock().await;
        state.add_user_message(content);
        sync_session_metadata_from_snapshot(&mut state, snapshot, sequence)?;
        self.store.save(&state)
    }

    pub(crate) async fn record_assistant_message(
        &self,
        snapshot: CodeUiSessionSnapshot,
        content: &str,
    ) -> io::Result<()> {
        let sequence = self.persist_projection_deltas(&snapshot).await?;
        let mut state = self.state.lock().await;
        state.add_assistant_message(content);
        sync_session_metadata_from_snapshot(&mut state, snapshot, sequence)?;
        self.store.save(&state)
    }

    pub(crate) async fn persist_snapshot(&self, snapshot: CodeUiSessionSnapshot) -> io::Result<()> {
        #[cfg(feature = "test-provider")]
        if self.take_snapshot_persist_failure_for_test() {
            return Err(io::Error::other(
                "injected Code UI snapshot persistence failure",
            ));
        }
        let sequence = self.persist_projection_deltas(&snapshot).await?;
        let mut state = self.state.lock().await;
        sync_session_metadata_from_snapshot(&mut state, snapshot, sequence)?;
        self.store.save(&state)
    }

    #[cfg(feature = "test-provider")]
    fn take_snapshot_persist_failure_for_test(&self) -> bool {
        let mut remaining = self
            .snapshot_persist_failure_countdown
            .load(Ordering::Acquire);
        loop {
            if remaining == 0 {
                return false;
            }
            match self
                .snapshot_persist_failure_countdown
                .compare_exchange_weak(
                    remaining,
                    remaining - 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                Ok(previous) => return previous == 1,
                Err(actual) => remaining = actual,
            }
        }
    }

    /// Persist only the projection fields that changed since the last durable
    /// headless checkpoint.  `SessionSnapshot` remains the compatibility
    /// record, while these ordered deltas are the authoritative Code UI suffix
    /// replayed on resume.
    async fn persist_projection_deltas(&self, snapshot: &CodeUiSessionSnapshot) -> io::Result<u64> {
        let mut checkpoint = self.projection_checkpoint.lock().await;
        let deltas = code_ui_projection_deltas(&checkpoint.snapshot, snapshot)?;
        if !deltas.is_empty() {
            let events = self.projection_store.append_code_workflow_batch(&deltas)?;
            if let Some(last) = events.last() {
                checkpoint.sequence = last.sequence;
            }
        }
        checkpoint.snapshot = snapshot.clone();
        Ok(checkpoint.sequence)
    }
}

fn code_ui_projection_deltas(
    previous: &CodeUiSessionSnapshot,
    current: &CodeUiSessionSnapshot,
) -> io::Result<Vec<CodeWorkflowEventKind>> {
    let mut deltas = Vec::new();
    if previous.status != current.status {
        deltas.push(projection_delta(
            "status",
            "session status changed",
            &current.status,
        )?);
    }
    if previous.controller != current.controller {
        deltas.push(projection_delta(
            "controller",
            "controller state changed",
            &current.controller,
        )?);
    }
    if previous.plan_execution_repair != current.plan_execution_repair {
        deltas.push(projection_delta(
            "plan_execution_repair",
            "plan execution repair changed",
            &current.plan_execution_repair,
        )?);
    }
    append_changed_projection_items(
        &mut deltas,
        "transcript_upsert",
        "transcript entry changed",
        &previous.transcript,
        &current.transcript,
        |entry| entry.id.as_str(),
    )?;
    append_changed_projection_items(
        &mut deltas,
        "interaction_upsert",
        "interaction changed",
        &previous.interactions,
        &current.interactions,
        |interaction| interaction.id.as_str(),
    )?;
    for interaction in &previous.interactions {
        if !current
            .interactions
            .iter()
            .any(|candidate| candidate.id == interaction.id)
        {
            deltas.push(projection_delta(
                "interaction_cleared",
                "interaction cleared",
                &serde_json::json!({ "interactionId": interaction.id }),
            )?);
        }
    }
    append_changed_projection_items(
        &mut deltas,
        "plan_upsert",
        "plan changed",
        &previous.plans,
        &current.plans,
        |plan| plan.id.as_str(),
    )?;
    append_changed_projection_items(
        &mut deltas,
        "task_upsert",
        "task changed",
        &previous.tasks,
        &current.tasks,
        |task| task.id.as_str(),
    )?;
    append_changed_projection_items(
        &mut deltas,
        "tool_call_upsert",
        "tool call changed",
        &previous.tool_calls,
        &current.tool_calls,
        |tool_call| tool_call.id.as_str(),
    )?;
    append_changed_projection_items(
        &mut deltas,
        "patchset_upsert",
        "patchset changed",
        &previous.patchsets,
        &current.patchsets,
        |patchset| patchset.id.as_str(),
    )?;
    if previous.thread_graph != current.thread_graph {
        deltas.push(projection_delta(
            "thread_graph",
            if current.thread_graph.is_some() {
                "thread graph changed"
            } else {
                "thread graph cleared"
            },
            &current.thread_graph,
        )?);
    }
    Ok(deltas)
}

fn append_changed_projection_items<T, F>(
    deltas: &mut Vec<CodeWorkflowEventKind>,
    projection: &str,
    summary: &str,
    previous: &[T],
    current: &[T],
    id: F,
) -> io::Result<()>
where
    T: serde::Serialize,
    F: Fn(&T) -> &str,
{
    let previous_by_id = previous
        .iter()
        .map(|item| Ok((id(item).to_string(), serde_json::to_value(item)?)))
        .collect::<Result<HashMap<_, _>, serde_json::Error>>()
        .map_err(json_projection_error)?;
    for item in current {
        let payload = serde_json::to_value(item).map_err(json_projection_error)?;
        if previous_by_id.get(id(item)) != Some(&payload) {
            deltas.push(CodeWorkflowEventKind::CodeUiProjectionDelta {
                projection: projection.to_string(),
                summary: summary.to_string(),
                payload,
            });
        }
    }
    Ok(())
}

fn projection_delta<T: serde::Serialize>(
    projection: &str,
    summary: &str,
    payload: &T,
) -> io::Result<CodeWorkflowEventKind> {
    Ok(CodeWorkflowEventKind::CodeUiProjectionDelta {
        projection: projection.to_string(),
        summary: summary.to_string(),
        payload: serde_json::to_value(payload).map_err(json_projection_error)?,
    })
}

fn json_projection_error(error: serde_json::Error) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("failed to serialize Code UI projection event: {error}"),
    )
}

/// A live tool-loop continuation held by `AgentRuntimeWorker` while the Web
/// session is awaiting an interaction response. Browser turns register one of
/// these so validation, durable audit, and one-shot release have a single
/// owner.
enum HeadlessInteractionDelivery {
    UserInput {
        session: Arc<CodeUiSession>,
        interaction_persistence_failed: Arc<AtomicBool>,
        persistence: Option<HeadlessSessionPersistence>,
        interaction_id: String,
        questions: Vec<UserInputQuestion>,
        response_tx: oneshot::Sender<UserInputResponse>,
    },
    ExecApproval {
        session: Arc<CodeUiSession>,
        interaction_persistence_failed: Arc<AtomicBool>,
        persistence: Option<HeadlessSessionPersistence>,
        interaction_id: String,
        request: ExecApprovalRequest,
    },
    /// Phase 0 IntentSpec review after `submit_intent_draft`. Durable
    /// `InteractionResolved` is deferred until the worker terminal succeeds
    /// ([`RuntimeInteractionDelivery::persist_interaction_resolved_after_terminal`]).
    IntentReview {
        expected_interaction_id: String,
        persistence: Option<HeadlessSessionPersistence>,
    },
}

#[async_trait]
impl RuntimeInteractionDelivery for HeadlessInteractionDelivery {
    fn validate(
        &self,
        interaction: &crate::internal::ai::runtime::InteractionResponse,
    ) -> Result<(), RuntimeWorkerError> {
        match self {
            Self::UserInput { questions, .. } => {
                let response =
                    decode_headless_interaction_response(interaction).map_err(|error| {
                        RuntimeWorkerError::InvalidInteractionResponse(error.to_string())
                    })?;
                user_input_response_from_code_ui_request(questions, response)
                    .map(|_| ())
                    .map_err(|error| {
                        RuntimeWorkerError::InvalidInteractionResponse(error.to_string())
                    })
            }
            Self::ExecApproval { .. } => {
                let response =
                    decode_headless_interaction_response(interaction).map_err(|error| {
                        RuntimeWorkerError::InvalidInteractionResponse(error.to_string())
                    })?;
                review_decision_from_interaction_response(response)
                    .map(|_| ())
                    .map_err(|error| {
                        RuntimeWorkerError::InvalidInteractionResponse(error.to_string())
                    })
            }
            Self::IntentReview {
                expected_interaction_id,
                persistence,
            } => {
                if interaction.interaction_id != *expected_interaction_id {
                    return Err(RuntimeWorkerError::InvalidInteractionResponse(format!(
                        "IntentSpec review response targeted '{}' but pending gate is '{expected_interaction_id}'",
                        interaction.interaction_id
                    )));
                }
                let decision =
                    intent_review_decision_from_response(interaction).map_err(|error| {
                        RuntimeWorkerError::InvalidInteractionResponse(error.to_string())
                    })?;
                if decision == IntentReviewDecision::Revise {
                    let response = decode_headless_interaction_response(interaction)?;
                    canonical_intent_revision_note(&response)?;
                    if persistence.is_some()
                        && !interaction.intent_revision_sidecar_digest().is_some_and(
                            crate::internal::ai::session::jsonl::is_canonical_intent_revision_digest,
                        )
                    {
                        return Err(RuntimeWorkerError::InvalidInteractionResponse(
                            "IntentSpec Modify is missing its durable prepared-sidecar binding"
                                .to_string(),
                        ));
                    }
                }
                Ok(())
            }
        }
    }

    fn persist_interaction_resolved_after_terminal(&self) -> bool {
        match self {
            Self::UserInput { .. } | Self::ExecApproval { .. } => false,
            // Intent review resolution must be atomic with the worker-owned
            // terminal command. Otherwise a lost `respond` acknowledgement
            // leaves no durable evidence that Confirm was consumed, and a
            // retry could revoke the accepted Phase 1 start seed.
            Self::IntentReview { persistence, .. } => persistence.is_some(),
        }
    }

    fn checkpoint_interaction_resolved_before_delivery(&self) -> bool {
        match self {
            Self::UserInput { persistence, .. } | Self::ExecApproval { persistence, .. } => {
                persistence.is_some()
            }
            Self::IntentReview { .. } => false,
        }
    }

    async fn after_pre_delivery_checkpoint(&mut self) {
        #[cfg(feature = "test-provider")]
        match self {
            Self::UserInput {
                persistence: Some(persistence),
                ..
            }
            | Self::ExecApproval {
                persistence: Some(persistence),
                ..
            } => persistence.wait_after_interaction_checkpoint().await,
            Self::UserInput {
                persistence: None, ..
            }
            | Self::ExecApproval {
                persistence: None, ..
            }
            | Self::IntentReview { .. } => {}
        }
    }

    fn interaction_resolution(
        &self,
        interaction: &crate::internal::ai::runtime::InteractionResponse,
    ) -> String {
        match self {
            Self::UserInput { .. } => "answered".to_string(),
            Self::ExecApproval { .. } => decode_headless_interaction_response(interaction)
                .ok()
                .and_then(|response| review_decision_from_interaction_response(response).ok())
                .map(|decision| match decision {
                    ReviewDecision::Approved => "approved",
                    ReviewDecision::ApprovedForSession => "approved_for_session",
                    ReviewDecision::ApprovedForTtl => "approved_for_ttl",
                    ReviewDecision::ApprovedForDirectoryTtl => "approved_for_directory_ttl",
                    ReviewDecision::ApprovedForPatternTtl => "approved_for_pattern_ttl",
                    ReviewDecision::ApprovedForAllCommands => "approved_for_all_commands",
                    ReviewDecision::Denied => "denied",
                    ReviewDecision::Abort => "aborted",
                })
                .unwrap_or("approval_resolved")
                .to_string(),
            Self::IntentReview { .. } => intent_review_decision_from_response(interaction)
                .map(|decision| decision.wire_id().to_string())
                .unwrap_or_else(|_| "intent_review_resolved".to_string()),
        }
    }

    fn intent_revision_recovery(
        &self,
        interaction: &crate::internal::ai::runtime::InteractionResponse,
    ) -> Option<IntentRevisionRecovery> {
        let Self::IntentReview {
            expected_interaction_id,
            ..
        } = self
        else {
            return None;
        };
        intent_revision_recovery_for_response(expected_interaction_id, interaction)
    }

    fn preserve_pending_on_shutdown(&self) -> bool {
        matches!(self, Self::IntentReview { .. })
    }

    async fn deliver(
        self: Box<Self>,
        _request: TurnRequest,
        interaction: crate::internal::ai::runtime::InteractionResponse,
        context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        if context.cancellation().is_cancelled() {
            return Err(RuntimeWorkerError::Cancelled);
        }
        match *self {
            Self::UserInput {
                session,
                interaction_persistence_failed,
                persistence,
                interaction_id,
                questions,
                response_tx,
            } => {
                let response = decode_headless_interaction_response(&interaction)?;
                deliver_headless_user_input_response(
                    &session,
                    &interaction_persistence_failed,
                    persistence.as_ref(),
                    &interaction_id,
                    questions,
                    response_tx,
                    response,
                )
                .await
            }
            Self::ExecApproval {
                session,
                interaction_persistence_failed,
                persistence,
                interaction_id,
                request: approval_request,
            } => {
                let response = decode_headless_interaction_response(&interaction)?;
                deliver_headless_exec_approval_response(
                    &session,
                    &interaction_persistence_failed,
                    persistence.as_ref(),
                    &interaction_id,
                    approval_request,
                    response,
                )
                .await
            }
            Self::IntentReview {
                expected_interaction_id,
                persistence: _,
            } => {
                let decision = intent_review_decision_from_response(&interaction)?;
                if interaction.interaction_id != expected_interaction_id {
                    return Err(RuntimeWorkerError::ExecutionFailed(format!(
                        "IntentSpec review response targeted '{}' but pending gate is '{expected_interaction_id}'",
                        interaction.interaction_id
                    )));
                }
                match decision {
                    IntentReviewDecision::Confirm => {
                        Ok(RuntimeTurnExecution::CompletedHoldQueued {
                            summary: "IntentSpec confirmed; Phase 1 planning queued".to_string(),
                        })
                    }
                    IntentReviewDecision::Revise => {
                        Ok(RuntimeTurnExecution::CompletedDiscardQueued {
                            summary: "IntentSpec revision mode armed; send a plain message with requested changes".to_string(),
                        })
                    }
                    IntentReviewDecision::Cancel => {
                        Ok(RuntimeTurnExecution::CompletedDiscardQueued {
                            summary: "IntentSpec review cancelled".to_string(),
                        })
                    }
                }
            }
        }
    }
}

/// Adapter from the UI-neutral serialized runtime to the headless
/// provider/tool-loop stack. It deliberately owns no queueing state: ordering,
/// cancellation and shutdown belong to `AgentRuntimeWorker`. Plain messages
/// run Phase 0 allowlists; slash/`/` messages keep an explicit direct loop.
struct HeadlessTurnExecutor<M: CompletionModel + 'static> {
    session: Arc<CodeUiSession>,
    history: Arc<Mutex<Vec<Message>>>,
    model: Arc<M>,
    registry: Arc<ToolRegistry>,
    config_factory:
        Arc<dyn Fn() -> super::super::agent::runtime::tool_loop::ToolLoopConfig + Send + Sync>,
    in_flight: Arc<Mutex<Option<InFlightTurn>>>,
    pre_start_turn: PreStartTurn,
    active_turn_mutations: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    shutdown_timed_out: Arc<AtomicBool>,
    /// A browser interaction response or request could not be durably
    /// projected. The original tool-loop may still be unwinding, so its later
    /// terminal result must not overwrite the reconciliation requirement.
    interaction_persistence_failed: Arc<AtomicBool>,
    persistence: Option<HeadlessSessionPersistence>,
    /// PlanPhase0 turns that parked an IntentSpec review, keyed by runtime
    /// turn id → browser interaction id. Cleared when the review settles.
    pending_intent_reviews: Arc<Mutex<HashMap<String, String>>>,
    /// After Modify/Revise, the current IntentSpec JSON awaits the next plain
    /// Phase 0 message (legacy `pending_plan_revision` parity).
    pending_intent_revision: Arc<Mutex<Option<PendingIntentRevision>>>,
    phase1_attempt_states: Arc<Mutex<HashMap<String, Arc<AtomicU8>>>>,
    interaction_transition: Arc<Mutex<()>>,
    /// Optional MCP server for formal Phase 0 `write_intent` persistence.
    mcp_server: Option<Arc<crate::internal::ai::mcp::server::LibraMcpServer>>,
}

impl<M: CompletionModel + 'static> HeadlessTurnExecutor<M> {
    /// A late terminal result must not erase an earlier reconciliation
    /// boundary merely because the worker eventually returned.
    async fn preserve_reconciliation(&self) -> bool {
        self.shutdown_timed_out.load(Ordering::Acquire)
            || self.interaction_persistence_failed.load(Ordering::Acquire)
            || matches!(
                self.session.snapshot().await.status,
                CodeUiSessionStatus::IndeterminateSideEffect
            )
    }

    async fn set_terminal_status_if_recoverable(&self, status: CodeUiSessionStatus) {
        if !self.preserve_reconciliation().await {
            set_status_if_recoverable(&self.session, status).await;
        }
    }

    /// Return whether cancellation won before admission committed the start
    /// gate. This state is independent of `in_flight`, so shutdown does not
    /// need to wait for a slow durable admission write merely to learn it.
    async fn cancellation_precedes_start(&self, turn_id: &str) -> bool {
        let signal = self
            .pre_start_turn
            .lock()
            .await
            .as_ref()
            .filter(|(candidate_turn_id, _)| candidate_turn_id == turn_id)
            .map(|(_, signal)| signal.clone());
        let Some(signal) = signal else {
            return false;
        };
        match signal.compare_exchange(
            PRE_START_UNSTARTED,
            PRE_START_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) | Err(PRE_START_CANCELLED) => true,
            Err(PRE_START_STARTED) => false,
            Err(_) => true,
        }
    }
}

/// A delayed terminal callback must never turn an existing reconciliation
/// boundary back into a superficially healthy terminal state. Callers that
/// have additional in-flight failure markers use
/// `HeadlessTurnExecutor::set_terminal_status_if_recoverable`; standalone
/// interaction callbacks use this snapshot guard.
async fn set_status_if_recoverable(session: &CodeUiSession, status: CodeUiSessionStatus) {
    if !matches!(
        session.snapshot().await.status,
        CodeUiSessionStatus::IndeterminateSideEffect
    ) {
        session.set_status(status).await;
    }
}

pub struct HeadlessCodeRuntime<M: CompletionModel + 'static> {
    // The provider model lives in the runtime executor; keep the public
    // adapter generic so callers cannot accidentally pair an executor built
    // for one provider type with a differently typed headless handle.
    model_type: PhantomData<M>,
    session: Arc<CodeUiSession>,
    /// Active turn slot shared with [`WebCodeUiAdmission`] / the executor.
    in_flight: Arc<Mutex<Option<InFlightTurn>>>,
    /// Session identity for the in-memory worker. It is intentionally opaque
    /// to the browser and never contains request text.
    runtime_session_id: String,
    /// Command transport only; durable workflow/context files are the Phase 1
    /// source of truth across process restarts.
    phase1_tx: mpsc::Sender<HeadlessPhase1Command>,
    /// The only path browser turns use to enter the serialized runtime.
    runtime: AgentRuntimeHandle,
    /// Stateless Phase 1 driver. Workflow authority remains in Runtime + the
    /// session JSONL/context files; retaining the executor adds no Web plan
    /// cursor or pending-plan state.
    turn_executor: Arc<HeadlessTurnExecutor<M>>,
    /// Stages confirmed-plan bodies so the worker owns Orchestrator::run
    /// (W2-04). Distinct from the chat/Phase-0/1 executor.
    plan_execution_executor: Arc<DeferredPlanExecutionExecutor>,
    /// Retained so explicit shutdown can join the worker and report a panic
    /// rather than silently detaching the lifecycle owner.
    runtime_worker_task: Mutex<Option<JoinHandle<()>>>,
    /// Once shutdown begins, no adapter command may start a replacement turn
    /// while the previous in-flight turn is being reconciled.
    shutting_down: Arc<AtomicBool>,
    /// A bounded shutdown timed out before its active turn reported a
    /// determinate result. The turn task must not later overwrite the durable
    /// indeterminate state if it happens to finish before process exit.
    shutdown_timed_out: Arc<AtomicBool>,
    /// Shared with the executor so a persistence failure in the interaction
    /// listener remains authoritative through the original turn's completion.
    interaction_persistence_failed: Arc<AtomicBool>,
    /// Every repeated shutdown caller observes this same terminal result,
    /// rather than racing to independently cancel or detach the active turn.
    shutdown_result_tx: watch::Sender<Option<Result<(), String>>>,
    /// Optional on-disk session persistence used by default Web `libra code
    /// --resume <thread_id>` for non-Codex providers.
    persistence: Option<HeadlessSessionPersistence>,
    /// Production Code UI write-path owner (submit/cancel/respond/goal/task).
    runtime_bridge: Arc<AgentRuntimeCodeUiAdapter>,
    /// Shared with the executor so resume can rehydrate a parked IntentSpec
    /// review gate after process restart.
    pending_intent_reviews: Arc<Mutex<HashMap<String, String>>>,
    /// Shared with the executor for Modify → next plain-message revision.
    pending_intent_revision: Arc<Mutex<Option<PendingIntentRevision>>>,
    /// Startup-only handoff for a durable `Consuming` sidecar whose consumer
    /// never crossed the receipt boundary. Keep the on-disk envelope intact so
    /// a second restart still retains the aborted consumer attribution.
    uncommitted_consuming_intent_revision: Mutex<Option<PendingIntentRevision>>,
}

impl<M> HeadlessCodeRuntime<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    /// Build a new headless runtime around an existing [`CodeUiSession`].
    ///
    /// `config_factory` is invoked once per turn so per-call `usage_context`
    /// fields (turn id, etc.) can be refreshed without mutating the original
    /// config in place.
    pub async fn new(
        session: Arc<CodeUiSession>,
        capabilities: CodeUiCapabilities,
        model: M,
        registry: Arc<ToolRegistry>,
        user_input_rx: mpsc::UnboundedReceiver<UserInputRequest>,
        exec_approval_rx: mpsc::UnboundedReceiver<ExecApprovalRequest>,
        config_factory: Arc<
            dyn Fn() -> super::super::agent::runtime::tool_loop::ToolLoopConfig + Send + Sync,
        >,
    ) -> anyhow::Result<Arc<Self>> {
        Self::new_with_persistence(
            session,
            capabilities,
            model,
            registry,
            user_input_rx,
            exec_approval_rx,
            config_factory.clone(),
            Vec::new(),
            None,
            None,
        )
        .await
    }

    /// Build a headless runtime with restored model history and optional
    /// SessionStore persistence.
    #[allow(clippy::too_many_arguments)]
    pub async fn new_with_persistence(
        session: Arc<CodeUiSession>,
        capabilities: CodeUiCapabilities,
        model: M,
        registry: Arc<ToolRegistry>,
        user_input_rx: mpsc::UnboundedReceiver<UserInputRequest>,
        exec_approval_rx: mpsc::UnboundedReceiver<ExecApprovalRequest>,
        config_factory: Arc<
            dyn Fn() -> super::super::agent::runtime::tool_loop::ToolLoopConfig + Send + Sync,
        >,
        initial_history: Vec<Message>,
        persistence: Option<HeadlessSessionPersistence>,
        mcp_server: Option<Arc<crate::internal::ai::mcp::server::LibraMcpServer>>,
    ) -> anyhow::Result<Arc<Self>> {
        Self::new_with_persistence_and_shutdown_timeout(
            session,
            capabilities,
            model,
            registry,
            user_input_rx,
            exec_approval_rx,
            config_factory,
            initial_history,
            persistence,
            mcp_server,
            HEADLESS_SHUTDOWN_TIMEOUT,
        )
        .await
    }

    /// Build a headless runtime with an explicit graceful-shutdown bound.
    ///
    /// Production callers use [`Self::new_with_persistence`]'s fixed default;
    /// this constructor exists for runtime integrations and deterministic
    /// timeout-injection tests that need to verify the indeterminate recovery
    /// path without waiting for the production deadline.
    #[allow(clippy::too_many_arguments)]
    pub async fn new_with_persistence_and_shutdown_timeout(
        session: Arc<CodeUiSession>,
        capabilities: CodeUiCapabilities,
        model: M,
        registry: Arc<ToolRegistry>,
        user_input_rx: mpsc::UnboundedReceiver<UserInputRequest>,
        exec_approval_rx: mpsc::UnboundedReceiver<ExecApprovalRequest>,
        config_factory: Arc<
            dyn Fn() -> super::super::agent::runtime::tool_loop::ToolLoopConfig + Send + Sync,
        >,
        initial_history: Vec<Message>,
        persistence: Option<HeadlessSessionPersistence>,
        mcp_server: Option<Arc<crate::internal::ai::mcp::server::LibraMcpServer>>,
        shutdown_timeout: Duration,
    ) -> anyhow::Result<Arc<Self>> {
        let (shutdown_result_tx, _) = watch::channel(None);
        let in_flight = Arc::new(Mutex::new(None));
        let pre_start_turn = Arc::new(Mutex::new(None));
        let history = Arc::new(Mutex::new(initial_history));
        let shutdown_timed_out = Arc::new(AtomicBool::new(false));
        let interaction_persistence_failed = Arc::new(AtomicBool::new(false));
        let active_turn_mutations = Arc::new(Mutex::new(HashMap::new()));
        let phase1_attempt_states = Arc::new(Mutex::new(HashMap::new()));
        let interaction_transition = Arc::new(Mutex::new(()));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let tool_boundary = registry.hardening().cloned().ok_or_else(|| {
            anyhow!(
                "Headless Code runtime requires the registry's shared tool-boundary policy; rebuild CodeAgentServices before starting a browser turn"
            )
        })?;
        // Durable commandId idempotency requires the SessionStore-backed
        // command log. Without it, refuse to advertise the capability so
        // browsers omit commandId rather than getting a best-effort cache.
        let mut capabilities = capabilities;
        if persistence.is_none() {
            capabilities.command_idempotency = false;
            session.set_capabilities(capabilities.clone()).await;
        }
        // Capture the shared task runtime before moving the config factory
        // into the turn executor below.
        let subagent_runtime = (config_factory)().subagent_runtime;
        let pending_intent_reviews = Arc::new(Mutex::new(HashMap::new()));
        let pending_intent_revision = Arc::new(Mutex::new(None));
        let runtime_session_id = persistence
            .as_ref()
            .map(HeadlessSessionPersistence::durability_session_id)
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let executor = Arc::new(HeadlessTurnExecutor {
            session: session.clone(),
            history: history.clone(),
            model: Arc::new(model),
            registry,
            config_factory,
            in_flight: in_flight.clone(),
            pre_start_turn: pre_start_turn.clone(),
            active_turn_mutations: active_turn_mutations.clone(),
            phase1_attempt_states: phase1_attempt_states.clone(),
            interaction_transition: interaction_transition.clone(),
            shutdown_timed_out: shutdown_timed_out.clone(),
            interaction_persistence_failed: interaction_persistence_failed.clone(),
            persistence: persistence.clone(),
            pending_intent_reviews: pending_intent_reviews.clone(),
            pending_intent_revision: pending_intent_revision.clone(),
            mcp_server,
        });
        let plan_execution_executor = Arc::new(DeferredPlanExecutionExecutor::new());
        let worker_executor = Arc::new(WebRuntimeTurnExecutor {
            chat: executor.clone(),
            plan: plan_execution_executor.clone(),
        });
        let mut worker_config = AgentRuntimeWorkerConfig::new(worker_executor, tool_boundary);
        worker_config.shutdown_timeout = shutdown_timeout;
        // Goal JSONL store also supplies session_root so task.dispatch can
        // attach file-history batches (S2-INV-06), matching the historical `/task` path.
        let execution_control = Arc::new(
            ExecutionControlService::new(
                runtime_session_id.clone(),
                persistence
                    .as_ref()
                    .map(HeadlessSessionPersistence::goal_event_store),
                subagent_runtime,
            )
            .map_err(|error| anyhow!("failed to restore runtime execution controls: {error}"))?,
        );
        let mut recovered_reconciliation = false;
        let mut intent_revision_consumer_healed = false;
        let mut intent_revision_cancel_healed = false;
        let mut intent_revision_replacement_review_healed = false;
        let mut durability_for_adapter = None;
        if let Some(persistence) = persistence.as_ref() {
            let (durability, repo_id, principal_id) = persistence.worker_durability_config();
            durability_for_adapter = Some(durability.clone());
            // Adopt the durable projection checkpoint before any recovery pass
            // reads the live session projection; on the production resume path
            // the caller already folded it into this session (no-op here).
            persistence.adopt_projection_checkpoint(&session).await?;
            // An unresolved IntentSpec review proves the Phase 0 draft mutation
            // finished; complete that one command id and still fence others.
            let goal_store = persistence.goal_event_store();
            // Revision authority must be proven before Phase 1 recovery is
            // allowed to garbage-collect seeds/contexts or rewrite a snapshot.
            // The locked recovery call below repeats the strict replay checks
            // before it appends anything.
            let persisted_snapshot = session.snapshot().await;
            let recover_succeeded_cancel_projection =
                intent_revision_cancel_projection_requires_healing(&persisted_snapshot);
            let recover_succeeded_replacement_projection =
                intent_revision_replacement_projection_requires_healing(&persisted_snapshot);
            let intent_revision_consumer =
                authenticated_uncommitted_intent_revision_consumer_with_projection_recovery(
                    persistence,
                    recover_succeeded_cancel_projection,
                    recover_succeeded_replacement_projection,
                )
                .with_context(|| {
                    format!(
                        "failed to authenticate an interrupted IntentSpec revision consumer for session '{}'",
                        persistence.durability_session_id()
                    )
                })?;
            crate::internal::ai::runtime::phase1::prepare_phase1_recovery_authority(&goal_store)
            .map_err(|error| {
                anyhow!(
                    "failed to establish committed review-gate authority and garbage collect unreachable Phase 1 contexts for headless Code session '{}': {error}",
                    persistence.durability_session_id()
                )
            })?;
            let review_gate_turn_id = open_review_gate_phase_turn_id(&goal_store);
            let mut prewrite_turn_id = None;
            let mut prewrite_intent = None;
            if let Some(seed) = load_phase1_start_seed(&goal_store)? {
                let turn_id =
                    crate::internal::ai::runtime::phase1::phase1_turn_id_from_seed(&seed)?;
                let source_replay = goal_store.load_code_workflow_replay_committed()?;
                let source_resolution_matches = phase1_source_resolution_matches_seed(
                    source_replay.events.iter().map(|event| &event.event),
                    &seed,
                );
                use sha2::Digest as _;
                let input = seed
                    .revision_note
                    .clone()
                    .unwrap_or_else(|| "Phase 1 plan generation".to_string());
                let expected_intent = CodeCommandIntent::new(
                    CodeCommandIdentity::new(
                        repo_id.clone(),
                        persistence.durability_session_id(),
                        principal_id.clone(),
                        turn_id.clone(),
                    ),
                    CODE_UI_WEB_TURN_KIND,
                    format!(
                        "sha256:{}",
                        hex::encode(sha2::Sha256::digest(input.as_bytes()))
                    ),
                    true,
                );
                match goal_store.code_command_intent_status(&expected_intent.identity)? {
                    Some((actual_intent, _)) if actual_intent != expected_intent => {
                        return Err(anyhow!(
                            "Phase 1 start seed command '{}' conflicts with its durable request payload",
                            expected_intent.identity.command_id
                        ));
                    }
                    Some((_, CodeCommandStatus::Pending)) => {
                        let crossed_formal_write = crate::internal::ai::runtime::phase1::phase1_formal_write_started_for_seed(
                            &goal_store,
                            &turn_id,
                            &seed.durable_digest()?,
                        )?;
                        if !crossed_formal_write
                            && source_resolution_matches
                            && review_gate_turn_id.as_deref() != Some(turn_id.as_str())
                        {
                            prewrite_intent = Some(expected_intent);
                            prewrite_turn_id = Some(turn_id);
                        }
                    }
                    Some((_, CodeCommandStatus::Succeeded { .. })) => {
                        clear_phase1_start_seed(&goal_store)?;
                        crate::internal::ai::runtime::phase1::gc_unreachable_phase1_review_contexts(
                            &goal_store,
                        )?;
                    }
                    Some((_, CodeCommandStatus::Failed { .. })) => {
                        let replay = goal_store.load_code_workflow_replay_committed()?;
                        let crossed_formal_write = crate::internal::ai::runtime::phase1::phase1_formal_write_started_for_seed(
                            &goal_store,
                            &turn_id,
                            &seed.durable_digest()?,
                        )?;
                        let revision_prewrite_failure = match phase1_retry_intent_review_state(
                            replay.events.iter().map(|event| &event.event),
                            &turn_id,
                        )? {
                            Phase1RetryIntentReviewState::Open(review)
                            | Phase1RetryIntentReviewState::Resolved { review, .. } => {
                                if !source_resolution_matches {
                                    return Err(anyhow!(
                                        "Phase 1 retry gate '{}' has no matching durable source resolution",
                                        review.interaction_id
                                    ));
                                }
                                if crossed_formal_write {
                                    return Err(anyhow!(
                                        "Phase 1 retry gate '{}' conflicts with a durable formal-write marker",
                                        review.interaction_id
                                    ));
                                }
                                validate_phase1_retry_intent_review_for_seed(
                                    &review,
                                    &turn_id,
                                    &seed,
                                )?;
                                false
                            }
                            Phase1RetryIntentReviewState::TerminalWithoutRetry => {
                                !crossed_formal_write
                                    && source_resolution_matches
                                    && seed.source_resolution.eq_ignore_ascii_case("modify")
                                    && seed.revision_note.as_deref().is_some_and(|note| {
                                        !note.trim().is_empty()
                                    })
                                    && seed.prior_plan.is_some()
                                    && pending_plan_revision_from_workflow(
                                        replay.events.iter().map(|event| &event.event),
                                    )
                                    .as_deref()
                                        == Some(seed.source_interaction_id.as_str())
                            }
                            Phase1RetryIntentReviewState::NoIntent
                            | Phase1RetryIntentReviewState::PendingTerminal => {
                                return Err(anyhow!(
                                    "Phase 1 command '{}' reports Failed without an exact durable terminal row",
                                    turn_id
                                ));
                            }
                        };
                        if revision_prewrite_failure {
                            // The durable revision source remains authoritative,
                            // but its failed Phase 1 attempt can leave the last
                            // persisted browser snapshot in Thinking. Finalize
                            // that projection before deleting the only seed that
                            // binds this terminal command to the pending revision.
                            session
                                .resolve_interaction(&seed.source_interaction_id)
                                .await;
                            let snapshot = session.snapshot().await;
                            let has_pending_interaction = snapshot.interactions.iter().any(
                                |interaction| {
                                    interaction.status == CodeUiInteractionStatus::Pending
                                },
                            );
                            if has_pending_interaction {
                                return Err(anyhow!(
                                    "recoverable Phase 1 plan revision '{}' conflicts with another pending browser interaction",
                                    turn_id
                                ));
                            }
                            if matches!(
                                    snapshot.status,
                                    CodeUiSessionStatus::Thinking
                                        | CodeUiSessionStatus::ExecutingTool
                                ) {
                                finalize_terminal_phase1_projection(
                                    &session,
                                    "Phase 1 plan revision failed before any formal write",
                                    "error",
                                )
                                .await;
                                persistence
                                    .persist_snapshot(session.snapshot().await)
                                    .await
                                    .map_err(|error| {
                                        anyhow!(
                                            "failed to durably finalize the recoverable Phase 1 plan revision projection before clearing its start seed: {error}"
                                        )
                                    })?;
                            }
                        }
                        clear_phase1_start_seed(&goal_store)?;
                        crate::internal::ai::runtime::phase1::gc_unreachable_phase1_review_contexts(
                            &goal_store,
                        )?;
                    }
                    Some((_, CodeCommandStatus::Indeterminate { .. })) => {}
                    None => {
                        if crate::internal::ai::runtime::phase1::phase1_formal_write_started_for_seed(
                            &goal_store,
                            &turn_id,
                            &seed.durable_digest()?,
                        )? {
                            return Err(anyhow!(
                                "Phase 1 formal-write marker '{}' has no durable command intent",
                                turn_id
                            ));
                        }
                    }
                }
            }
            let recovery = durability
                .recover_pending_mutations_for_review_and_phase1_prewrite(
                    review_gate_turn_id.as_deref(),
                    prewrite_intent.as_ref(),
                    intent_revision_consumer.as_ref(),
                )
                .map_err(|error| {
                    anyhow!(
                        "failed to recover pending durable commands for headless Code session '{}': {error}",
                        persistence.durability_session_id()
                    )
                })?;
            intent_revision_consumer_healed = recovery.intent_revision_consumer_healed;
            intent_revision_cancel_healed = recovery.intent_revision_cancel_healed;
            intent_revision_replacement_review_healed =
                recovery.intent_revision_replacement_review_healed;
            worker_config = worker_config
                .with_durability(durability, repo_id, principal_id)
                .with_durability_command_kind(CODE_UI_WEB_TURN_KIND);
            if recovery.phase1_prewrite_reattached
                && let Some(turn_id) = prewrite_turn_id
            {
                worker_config = worker_config.with_phase1_prewrite_reattach_turn(turn_id);
            }
            if !recovery.fenced.is_empty() {
                recovered_reconciliation = true;
                worker_config = worker_config
                    .with_recovered_reconciliation_session(persistence.durability_session_id());
            }
        }
        let (runtime_handle, runtime_worker_task) = AgentRuntimeWorker::spawn(worker_config);
        let (phase1_tx, phase1_rx) = mpsc::channel(4);
        let web_admission = WebCodeUiAdmission::new(WebCodeUiAdmissionInit {
            runtime_session_id: runtime_session_id.clone(),
            persistence: persistence.clone(),
            in_flight: in_flight.clone(),
            pre_start_turn,
            active_turn_mutations: active_turn_mutations.clone(),
            phase1_attempt_states,
            interaction_transition,
            pending_intent_reviews: pending_intent_reviews.clone(),
            pending_intent_revision: pending_intent_revision.clone(),
            shutting_down: shutting_down.clone(),
            interaction_persistence_failed: interaction_persistence_failed.clone(),
            phase1_tx: phase1_tx.clone(),
            working_dir: executor.registry.working_dir().to_path_buf(),
        });
        let runtime_bridge = AgentRuntimeCodeUiAdapter::new_with_web_admission(
            session.clone(),
            capabilities.clone(),
            runtime_handle.clone(),
            runtime_session_id.clone(),
            execution_control.clone(),
            None,
            durability_for_adapter,
            Some(web_admission),
        );
        let runtime = Arc::new(Self {
            model_type: PhantomData,
            session,
            in_flight,
            runtime_session_id,
            phase1_tx,
            runtime: runtime_handle,
            turn_executor: executor,
            plan_execution_executor,
            runtime_worker_task: Mutex::new(Some(runtime_worker_task)),
            shutting_down,
            shutdown_timed_out,
            interaction_persistence_failed,
            shutdown_result_tx,
            persistence,
            runtime_bridge: runtime_bridge.clone(),
            pending_intent_reviews,
            pending_intent_revision,
            uncommitted_consuming_intent_revision: Mutex::new(None),
        });
        runtime_bridge
            .attach_lifecycle_shutdown(runtime.clone() as Arc<dyn CodeUiLifecycleShutdown>)
            .await;

        let weak_phase1 = Arc::downgrade(&runtime);
        tokio::spawn(async move {
            Self::run_phase1_command_listener(weak_phase1, phase1_rx).await;
        });

        let weak_listener = Arc::downgrade(&runtime);
        let user_input_rx = user_input_rx;
        let exec_approval_rx = exec_approval_rx;
        tokio::spawn(async move {
            Self::run_user_and_exec_approval_request_listener(
                weak_listener,
                user_input_rx,
                exec_approval_rx,
            )
            .await;
        });

        if let Err(error) = runtime.reconcile_intent_revision_sidecar().await {
            tracing::error!(
                error = %error,
                "failed to reconcile durable IntentSpec revision handoff; fencing"
            );
            runtime.fence_phase1_failure(error).await;
            recovered_reconciliation = true;
        }
        if (intent_revision_consumer_healed
            || intent_revision_cancel_healed
            || intent_revision_replacement_review_healed)
            && !recovered_reconciliation
        {
            if intent_revision_cancel_healed {
                if !finalize_recovered_intent_revision_cancel_projection(&runtime.session).await {
                    tracing::error!(
                        "authenticated IntentSpec cancel receipt did not own the latest user projection; fencing"
                    );
                    recovered_reconciliation = true;
                }
            } else if intent_revision_replacement_review_healed {
                finalize_recovered_intent_revision_replacement_projection(&runtime.session).await;
            } else {
                finalize_recovered_intent_revision_consumer_projection(&runtime.session).await;
            }
            if let Some(persistence) = runtime.persistence.as_ref()
                && let Err(error) = persistence
                    .persist_snapshot(runtime.session.snapshot().await)
                    .await
            {
                tracing::error!(
                    error = %error,
                    "failed to persist the authenticated IntentSpec revision consumer recovery projection"
                );
                runtime
                    .session
                    .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                    .await;
                recovered_reconciliation = true;
            }
        }

        if recovered_reconciliation
            || runtime.session.snapshot().await.status
                == CodeUiSessionStatus::IndeterminateSideEffect
        {
            // Keep the browser-visible snapshot aligned with the worker fence
            // so SSE/snapshot clients see reconciliation before the first 409.
            runtime
                .session
                .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                .await;
            if let Some(persistence) = runtime.persistence.as_ref()
                && let Err(error) = persistence
                    .persist_snapshot(runtime.session.snapshot().await)
                    .await
            {
                tracing::warn!(
                    error = %error,
                    "failed to persist recovered reconciliation fence for headless Code session"
                );
            }
        } else if let Err(error) = runtime.reconcile_resolved_phase1_projection().await {
            runtime.fence_phase1_failure(error).await;
        } else if let Err(error) = runtime.restore_pending_intent_review_gate().await {
            tracing::error!(
                error = %error,
                "failed to restore pending IntentSpec review gate for headless Code session; fencing"
            );
            runtime
                .session
                .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                .await;
            if let Some(persistence) = runtime.persistence.as_ref()
                && let Err(persist_error) = persistence
                    .persist_snapshot(runtime.session.snapshot().await)
                    .await
            {
                tracing::warn!(
                    error = %persist_error,
                    "failed to persist unrestorable IntentSpec review fence"
                );
            }
        } else if runtime.pending_intent_reviews.lock().await.is_empty() {
            match runtime.restore_pending_phase1_gate().await {
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        "failed to restore pending Phase 1 review gate; fencing"
                    );
                    runtime.fence_phase1_failure(error).await;
                }
                Ok(true) => {}
                Ok(false) => match runtime.restore_phase1_start_seed().await {
                    Err(error) => runtime.fence_phase1_failure(error).await,
                    Ok(true) => {}
                    Ok(false) => {
                        if let Err(error) = runtime.restore_pending_intent_revision_mode().await {
                            tracing::error!(
                                error = %error,
                                "failed to restore pending IntentSpec revision mode for headless Code session; fencing"
                            );
                            runtime.fence_phase1_failure(error).await;
                        }
                    }
                },
            }
        } else if let Err(error) = runtime.restore_pending_intent_revision_mode().await {
            tracing::error!(
                error = %error,
                "failed to restore pending IntentSpec revision mode for headless Code session; fencing"
            );
            runtime
                .session
                .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                .await;
            if let Some(persistence) = runtime.persistence.as_ref()
                && let Err(persist_error) = persistence
                    .persist_snapshot(runtime.session.snapshot().await)
                    .await
            {
                tracing::warn!(
                    error = %persist_error,
                    "failed to persist unrestorable IntentSpec revision fence"
                );
            }
        }

        Ok(runtime)
    }

    async fn run_phase1_command_listener(
        weak: std::sync::Weak<Self>,
        mut rx: mpsc::Receiver<HeadlessPhase1Command>,
    ) {
        while let Some(command) = rx.recv().await {
            let Some(runtime) = weak.upgrade() else {
                break;
            };
            match command {
                HeadlessPhase1Command::Start {
                    confirmed,
                    admitted,
                    start,
                } => {
                    #[cfg(feature = "test-provider")]
                    if confirmed.revision_source_interaction_id.is_none()
                        && admitted.is_some()
                        && let Some(persistence) = runtime.persistence.as_ref()
                    {
                        // Pause in the coordinator, not the HTTP task: once
                        // the command is enqueued the listener is the sole
                        // owner that can close the source gate or admit the
                        // Phase 1 attempt. This makes abort/retry ordering
                        // deterministic without changing production flow.
                        persistence.wait_after_phase1_start_enqueued().await;
                    }
                    if let Err(error) = runtime
                        .start_confirmed_phase1(confirmed, admitted, start)
                        .await
                    {
                        runtime.fence_phase1_failure(error).await;
                    }
                }
                HeadlessPhase1Command::PrepareNetwork {
                    plan_interaction_id,
                    reply,
                } => {
                    let result = runtime.prepare_network_gate(&plan_interaction_id).await;
                    let _ = reply.send(result);
                }
                HeadlessPhase1Command::ParkNetwork { prepared, reply } => {
                    let result = runtime
                        .park_network_gate(prepared)
                        .await
                        .map_err(|error| error.to_string());
                    if let Err(error) = &result {
                        runtime.fence_phase1_failure(anyhow!(error.clone())).await;
                    }
                    let _ = reply.send(result);
                }
                HeadlessPhase1Command::PreparePlanBack {
                    network_interaction_id,
                    reply,
                } => {
                    let result = runtime
                        .prepare_plan_gate_from_network(&network_interaction_id)
                        .await
                        .map_err(|error| error.to_string());
                    let _ = reply.send(result);
                }
                HeadlessPhase1Command::ParkPlanBack { prepared, reply } => {
                    let result = runtime
                        .park_plan_gate_from_network(prepared)
                        .await
                        .map_err(|error| error.to_string());
                    if let Err(error) = &result {
                        runtime.fence_phase1_failure(anyhow!(error.clone())).await;
                    }
                    let _ = reply.send(result);
                }
                HeadlessPhase1Command::StartPlanExecution {
                    network_interaction_id,
                    reply,
                } => {
                    let result = runtime
                        .start_confirmed_plan_execution(&network_interaction_id)
                        .await
                        .map_err(|error| error.to_string());
                    if let Err(error) = &result {
                        runtime.fence_phase1_failure(anyhow!(error.clone())).await;
                    }
                    let _ = reply.send(result);
                }
                HeadlessPhase1Command::CleanupAttempt { phase1_turn_id } => {
                    let removed = {
                        let mut attempts = runtime.turn_executor.phase1_attempt_states.lock().await;
                        let removable = attempts.get(&phase1_turn_id).is_some_and(|state| {
                            matches!(
                                state.load(Ordering::Acquire),
                                PHASE1_ATTEMPT_CANCELLED | PHASE1_ATTEMPT_SETTLED
                            )
                        });
                        removable && attempts.remove(&phase1_turn_id).is_some()
                    };
                    if removed {
                        runtime
                            .turn_executor
                            .active_turn_mutations
                            .lock()
                            .await
                            .remove(&phase1_turn_id);
                        release_web_turn(&runtime.in_flight, &phase1_turn_id).await;
                    }
                }
            }
        }
    }

    async fn fence_phase1_failure(&self, _error: anyhow::Error) {
        let reason = "Phase 1 coordinator outcome is indeterminate; restart and reconcile the durable session before retrying".to_string();
        let runtime_turn_id = self
            .in_flight
            .lock()
            .await
            .as_ref()
            .map(|turn| turn.runtime_turn_id.clone())
            .unwrap_or_else(|| "phase1-coordinator".to_string());
        let mut failed_stages = Vec::new();
        if self
            .runtime
            .fence_session(self.runtime_session_id.clone(), reason.clone())
            .await
            .is_err()
        {
            failed_stages.push("runtime fence");
        }
        self.session
            .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
            .await;
        finalize_phase1_streaming_entries(&self.session, &reason, "error").await;
        if let Some(persistence) = self.persistence.as_ref() {
            if persistence
                .goal_event_store()
                .append_code_workflow_durable(CodeWorkflowEventKind::IndeterminateSideEffect {
                    command_id: runtime_turn_id.clone(),
                    effect: "phase1_coordinator".to_string(),
                    reason: reason.clone(),
                })
                .is_err()
            {
                failed_stages.push("workflow fence marker");
            }
            if persistence
                .persist_snapshot(self.session.snapshot().await)
                .await
                .is_err()
            {
                failed_stages.push("session snapshot");
            }
        }
        if !failed_stages.is_empty() {
            self.interaction_persistence_failed
                .store(true, Ordering::Release);
            tracing::error!(
                failed_stages = %failed_stages.join(", "),
                "Phase 1 coordinator could not establish every durable reconciliation fence"
            );
        }
        release_web_turn(&self.in_flight, &runtime_turn_id).await;
    }

    async fn start_confirmed_phase1(
        self: &Arc<Self>,
        confirmed: ConfirmedIntentForPhase1,
        admitted: Option<oneshot::Sender<Result<Phase1StartAdmission, RuntimeWorkerError>>>,
        start: Option<oneshot::Receiver<Result<(), String>>>,
    ) -> anyhow::Result<()> {
        let mut identity = sha2::Sha256::new();
        use sha2::Digest as _;
        identity.update(confirmed.intent_id.as_bytes());
        identity.update(b"\0");
        identity.update(
            confirmed
                .revision_source_interaction_id
                .as_deref()
                .unwrap_or("confirmed-intent")
                .as_bytes(),
        );
        identity.update(b"\0");
        identity.update(confirmed.revision_note.as_deref().unwrap_or("").as_bytes());
        identity.update(b"\0");
        if let Some(checkout) = confirmed.checkout.as_ref() {
            identity.update(serde_json::to_vec(checkout)?);
        }
        let identity = hex::encode(identity.finalize());
        let phase1_turn_id = confirmed
            .phase1_turn_id_override
            .clone()
            .unwrap_or_else(|| format!("phase1-web-{identity}"));
        let review_turn_id = format!("plan-review-turn-{identity}");
        let plan_interaction_id = format!("plan-review-{identity}");
        let cancellation = CancellationToken::new();
        let mutation_started = {
            let mut mutations = self.turn_executor.active_turn_mutations.lock().await;
            mutations
                .entry(phase1_turn_id.clone())
                .or_insert_with(|| Arc::new(AtomicBool::new(false)))
                .clone()
        };
        let attempt_state = {
            let mut attempts = self.turn_executor.phase1_attempt_states.lock().await;
            attempts
                .entry(phase1_turn_id.clone())
                .or_insert_with(|| Arc::new(AtomicU8::new(PHASE1_ATTEMPT_PLANNING)))
                .clone()
        };
        let attempt_phase = match attempt_state.compare_exchange(
            PHASE1_ATTEMPT_PLANNING,
            PHASE1_ATTEMPT_ADMITTING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => PHASE1_ATTEMPT_ADMITTING,
            Err(phase) => phase,
        };
        if matches!(
            attempt_phase,
            PHASE1_ATTEMPT_CANCELLED | PHASE1_ATTEMPT_SETTLED
        ) {
            // Web admission installs this marker before enqueueing Confirm's
            // Start command. An aborted HTTP request can therefore be followed
            // by Cancel/shutdown while the coordinator is paused; never admit
            // provider work after either transition has claimed the marker.
            // Keep both terminal tombstones until the FIFO CleanupAttempt.
            // Another already-queued duplicate Start must observe the same
            // state instead of recreating Planning after this command exits.
            release_web_turn(&self.in_flight, &phase1_turn_id).await;
            if let Some(admitted) = admitted {
                let _ = admitted.send(Ok(Phase1StartAdmission::Existing));
            }
            return Ok(());
        }
        if !matches!(
            attempt_phase,
            PHASE1_ATTEMPT_ADMITTING | PHASE1_ATTEMPT_MUTATING
        ) {
            return Err(anyhow!(
                "Phase 1 attempt '{phase1_turn_id}' has an invalid admission state {attempt_phase}"
            ));
        }
        let durable_input = phase1_durable_input(&confirmed);
        // Register the shared Planning state before awaiting Runtime admission.
        // Shutdown and Web Cancel can now win Planning -> Cancelled even in the
        // actor-reply handoff window; a later formal write must win the same
        // state transition before it can mutate.
        if let Err(error) = self
            .runtime
            .track_external_turn(
                TurnRequest::new(
                    self.runtime_session_id.clone(),
                    phase1_turn_id.clone(),
                    durable_input,
                    true,
                ),
                cancellation.clone(),
                mutation_started.clone(),
            )
            .await
        {
            let existing_success = matches!(
                &error,
                RuntimeWorkerError::IdempotentCommand {
                    ack_ok: true,
                    status,
                    ..
                } if status.starts_with("Succeeded")
            );
            if existing_success {
                self.turn_executor
                    .active_turn_mutations
                    .lock()
                    .await
                    .remove(&phase1_turn_id);
                self.turn_executor
                    .phase1_attempt_states
                    .lock()
                    .await
                    .remove(&phase1_turn_id);
                if let Some(persistence) = self.persistence.as_ref() {
                    clear_phase1_start_seed(&persistence.goal_event_store())?;
                }
                self.session
                    .resolve_interaction(&confirmed.source_interaction_id)
                    .await;
                if let Some(persistence) = self.persistence.as_ref() {
                    persistence
                        .persist_snapshot(self.session.snapshot().await)
                        .await?;
                }
                let has_open_plan = self
                    .persistence
                    .as_ref()
                    .and_then(|persistence| {
                        let store = persistence.goal_event_store();
                        store.load_code_workflow_replay().ok().and_then(|replay| {
                            open_plan_review_from_workflow(
                                replay.events.iter().map(|event| &event.event),
                            )
                        })
                    })
                    .is_some();
                let current_turn_id = self
                    .in_flight
                    .lock()
                    .await
                    .as_ref()
                    .map(|turn| turn.runtime_turn_id.clone());
                if current_turn_id.as_deref() == Some(phase1_turn_id.as_str()) && !has_open_plan {
                    release_web_turn(&self.in_flight, &phase1_turn_id).await;
                }
                if let Some(admitted) = admitted {
                    let _ = admitted.send(Ok(Phase1StartAdmission::Existing));
                }
                return Ok(());
            }
            let rearmed_after_determinate_failure =
                matches!(
                    &error,
                    RuntimeWorkerError::IdempotentCommand { ack_ok: false, .. }
                ) && confirmed.revision_source_interaction_id.is_none()
                    && self
                        .persistence
                        .as_ref()
                        .and_then(|persistence| {
                            persistence
                                .goal_event_store()
                                .load_code_workflow_replay()
                                .ok()
                        })
                        .and_then(|replay| {
                            open_intent_review_from_workflow(
                                replay.events.iter().map(|event| &event.event),
                            )
                        })
                        .is_some_and(|(interaction_id, intent_id, _, _)| {
                            interaction_id != confirmed.source_interaction_id
                                && intent_id == confirmed.intent_id
                        });
            if rearmed_after_determinate_failure {
                // A duplicate HTTP Confirm may already be queued behind the
                // first Phase 1 attempt. If that attempt failed before formal
                // mutation and durably rearmed a fresh Intent gate, the old
                // command's Failed status is an idempotent acknowledgement of
                // that close-out—not grounds to fence or release the fresh
                // generation.
                if let Some(admitted) = admitted {
                    let _ = admitted.send(Ok(Phase1StartAdmission::Existing));
                }
                return Ok(());
            }
            self.turn_executor
                .active_turn_mutations
                .lock()
                .await
                .remove(&phase1_turn_id);
            self.turn_executor
                .phase1_attempt_states
                .lock()
                .await
                .remove(&phase1_turn_id);
            let determinate_rejection = matches!(
                &error,
                RuntimeWorkerError::CommandPayloadConflict { .. }
                    | RuntimeWorkerError::IdempotentCommand { ack_ok: false, .. }
                    | RuntimeWorkerError::ShuttingDown
            );
            let shutdown_rejection = matches!(&error, RuntimeWorkerError::ShuttingDown)
                && self.shutting_down.load(Ordering::Acquire);
            if determinate_rejection {
                if !shutdown_rejection && let Some(persistence) = self.persistence.as_ref() {
                    clear_phase1_start_seed(&persistence.goal_event_store())?;
                }
                let current_turn_id = self
                    .in_flight
                    .lock()
                    .await
                    .as_ref()
                    .map(|turn| turn.runtime_turn_id.clone());
                if current_turn_id.as_deref() == Some(phase1_turn_id.as_str()) {
                    release_web_turn(&self.in_flight, &phase1_turn_id).await;
                }
            }
            if let Some(admitted) = admitted {
                let _ = admitted.send(Err(error.clone()));
            }
            if determinate_rejection {
                return Ok(());
            }
            return Err(anyhow!(runtime_worker_adapter_message(error)));
        }
        if matches!(
            attempt_state.load(Ordering::Acquire),
            PHASE1_ATTEMPT_CANCELLED | PHASE1_ATTEMPT_SETTLED
        ) {
            // Cancel may claim Admitting while Runtime admission is in flight.
            // Once the intent exists, publish the typed retry authority in its
            // terminal row without ever starting the provider; Cancel waits
            // for this row and resolves the exact embedded generation.
            let retry_intent_review = phase1_retry_intent_review(&confirmed, &phase1_turn_id);
            let retry_interaction_id = retry_intent_review
                .as_ref()
                .map(|retry| retry.interaction_id.clone());
            let reason = "Phase 1 planning cancelled before provider start".to_string();
            if let Err(finish_error) = self
                .runtime
                .finish_external_turn_with_retry_intent_review(
                    self.runtime_session_id.clone(),
                    phase1_turn_id.clone(),
                    Err(RuntimeWorkerError::ExecutionFailed(reason.clone())),
                    retry_intent_review,
                )
                .await
            {
                self.turn_executor
                    .active_turn_mutations
                    .lock()
                    .await
                    .remove(&phase1_turn_id);
                self.turn_executor
                    .phase1_attempt_states
                    .lock()
                    .await
                    .remove(&phase1_turn_id);
                return Err(anyhow!(
                    "Phase 1 cancellation reached durable admission, but Runtime could not terminalize the attempt ({finish_error}); the start seed was retained for reconciliation"
                ));
            }
            let source_interaction_id = confirmed.source_interaction_id.clone();
            let runtime = Arc::clone(self);
            tokio::spawn(async move {
                if let Err(error) = runtime
                    .settle_failed_phase1_prewrite(
                        phase1_turn_id,
                        attempt_state,
                        retry_interaction_id,
                        source_interaction_id,
                        reason,
                    )
                    .await
                {
                    runtime.fence_phase1_failure(error).await;
                }
            });
            if let Some(admitted) = admitted {
                let _ = admitted.send(Ok(Phase1StartAdmission::Existing));
            }
            return Ok(());
        }
        self.session
            .resolve_interaction(&confirmed.source_interaction_id)
            .await;
        self.session.set_status(CodeUiSessionStatus::Thinking).await;
        if let Some(persistence) = self.persistence.as_ref() {
            persistence
                .persist_snapshot(self.session.snapshot().await)
                .await?;
        }
        self.replace_in_flight_runtime_turn(&phase1_turn_id, "Phase 1 plan generation")
            .await?;
        if let Some(admitted) = admitted {
            let _ = admitted.send(Ok(Phase1StartAdmission::Execute));
        }
        if let Some(start) = start {
            let start_result = start.await.unwrap_or_else(|_| {
                Err("Phase 1 admission owner stopped before provider start".to_string())
            });
            if let Err(reason) = start_result {
                let retry_intent_review = phase1_retry_intent_review(&confirmed, &phase1_turn_id);
                let retry_interaction_id = retry_intent_review
                    .as_ref()
                    .map(|retry| retry.interaction_id.clone());
                let finish_result = self
                    .runtime
                    .finish_external_turn_with_retry_intent_review(
                        self.runtime_session_id.clone(),
                        phase1_turn_id.clone(),
                        Err(RuntimeWorkerError::ExecutionFailed(reason.clone())),
                        retry_intent_review,
                    )
                    .await;
                if let Err(finish_error) = finish_result {
                    self.turn_executor
                        .active_turn_mutations
                        .lock()
                        .await
                        .remove(&phase1_turn_id);
                    self.turn_executor
                        .phase1_attempt_states
                        .lock()
                        .await
                        .remove(&phase1_turn_id);
                    return Err(anyhow!(
                        "Phase 1 admission failed before provider start ({reason}), and Runtime could not durably terminalize the attempt ({finish_error}); the start seed was retained for reconciliation"
                    ));
                }
                let source_interaction_id = confirmed.source_interaction_id.clone();
                let runtime = Arc::clone(self);
                tokio::spawn(async move {
                    if let Err(error) = runtime
                        .settle_failed_phase1_prewrite(
                            phase1_turn_id,
                            attempt_state,
                            retry_interaction_id,
                            source_interaction_id,
                            reason,
                        )
                        .await
                    {
                        runtime.fence_phase1_failure(error).await;
                    }
                });
                return Ok(());
            }
        }

        let context = match self
            .turn_executor
            .generate_web_phase1(
                &confirmed,
                &phase1_turn_id,
                &review_turn_id,
                &plan_interaction_id,
                Phase1GenerationControl {
                    mutation_started: mutation_started.clone(),
                    attempt_state: attempt_state.clone(),
                    cancellation,
                },
            )
            .await
        {
            Ok(context) => context,
            Err(error) => {
                let attempt_phase = attempt_state.load(Ordering::Acquire);
                let failed_before_formal_write = attempt_phase != PHASE1_ATTEMPT_MUTATING;
                let cancelled_before_formal_write = matches!(
                    attempt_phase,
                    PHASE1_ATTEMPT_CANCELLED | PHASE1_ATTEMPT_SETTLED
                );
                let retry_intent_review = failed_before_formal_write
                    .then(|| phase1_retry_intent_review(&confirmed, &phase1_turn_id))
                    .flatten();
                let retry_interaction_id = retry_intent_review
                    .as_ref()
                    .map(|retry| retry.interaction_id.clone());
                let finish_result = self
                    .runtime
                    .finish_external_turn_with_retry_intent_review(
                        self.runtime_session_id.clone(),
                        phase1_turn_id.clone(),
                        Err(error.clone()),
                        retry_intent_review,
                    )
                    .await;

                if let Err(finish_error) = finish_result {
                    self.turn_executor
                        .active_turn_mutations
                        .lock()
                        .await
                        .remove(&phase1_turn_id);
                    self.turn_executor
                        .phase1_attempt_states
                        .lock()
                        .await
                        .remove(&phase1_turn_id);
                    return Err(anyhow!(
                        "Phase 1 generation failed ({error}), and Runtime could not durably terminalize the attempt ({finish_error}); the start seed was retained for reconciliation"
                    ));
                }
                if failed_before_formal_write {
                    let failure_message = if cancelled_before_formal_write {
                        "Phase 1 planning cancelled before any formal write".to_string()
                    } else {
                        format!("Phase 1 planning failed before any formal write: {error}")
                    };
                    let source_interaction_id = confirmed.source_interaction_id.clone();
                    let runtime = Arc::clone(self);
                    tokio::spawn(async move {
                        if let Err(error) = runtime
                            .settle_failed_phase1_prewrite(
                                phase1_turn_id,
                                attempt_state,
                                retry_interaction_id,
                                source_interaction_id,
                                failure_message,
                            )
                            .await
                        {
                            runtime.fence_phase1_failure(error).await;
                        }
                    });
                    return Ok(());
                }
                // Formal-write failures still serialize their terminal outcome
                // with Web cancellation and shutdown. Only the pre-write retry
                // path above atomically embeds retry authority in the Failed
                // terminal and defers its live-gate restore, because a queued
                // exact Start retry may hold this guard while awaiting us.
                let _transition = self.turn_executor.interaction_transition.lock().await;
                self.turn_executor
                    .active_turn_mutations
                    .lock()
                    .await
                    .remove(&phase1_turn_id);
                self.turn_executor
                    .phase1_attempt_states
                    .lock()
                    .await
                    .remove(&phase1_turn_id);
                return Err(anyhow!(runtime_worker_adapter_message(error)));
            }
        };
        // Keep the Phase 1 terminal -> Plan gate handoff serialized with Web
        // cancel/respond. The active attempt and in-flight owner remain live
        // until the replacement gate has been registered and projected.
        let _transition = self.turn_executor.interaction_transition.lock().await;
        self.runtime
            .finish_external_turn(
                self.runtime_session_id.clone(),
                phase1_turn_id.clone(),
                Ok(RuntimeTurnExecution::CompletedHoldQueued {
                    summary: "Execution plan persisted; awaiting review".to_string(),
                }),
            )
            .await
            .map_err(|error| anyhow!(runtime_worker_adapter_message(error)))?;
        // The Plan marker/context is already durable. Drop the crash seed
        // before the live gate is projected so observers that see PostPlanChoice
        // cannot still load the pre-write seed.
        if let Some(persistence) = self.persistence.as_ref() {
            clear_phase1_start_seed(&persistence.goal_event_store())?;
        }

        if self.shutting_down.load(Ordering::Acquire) {
            // The durable Plan marker/context already exists and the runtime
            // terminal was acknowledged by the same actor command. Shutdown
            // may now stop the worker before a live review turn can be parked;
            // leave that projection to startup recovery instead of fencing a
            // successful formal write as a response-drop failure.
            if let Some(persistence) = self.persistence.as_ref()
                && let Err(error) = clear_phase1_start_seed(&persistence.goal_event_store())
            {
                tracing::warn!(
                    %error,
                    "failed to clear a terminal Phase 1 seed during shutdown; the open Plan marker remains recovery authority"
                );
            }
            self.turn_executor
                .active_turn_mutations
                .lock()
                .await
                .remove(&phase1_turn_id);
            self.turn_executor
                .phase1_attempt_states
                .lock()
                .await
                .remove(&phase1_turn_id);
            release_web_turn(&self.in_flight, &phase1_turn_id).await;
            return Ok(());
        }

        self.runtime
            .track_external_turn(
                TurnRequest::new(
                    self.runtime_session_id.clone(),
                    review_turn_id.clone(),
                    "Plan review",
                    false,
                ),
                CancellationToken::new(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .map_err(|error| anyhow!(runtime_worker_adapter_message(error)))?;
        self.runtime
            .register_interaction_with_delivery(
                self.runtime_session_id.clone(),
                review_turn_id.clone(),
                InteractionState::AwaitingPlanReview {
                    interaction_id: plan_interaction_id.clone(),
                },
                Box::new(PlanReviewAckDelivery::new()),
            )
            .await
            .map_err(|error| anyhow!(runtime_worker_adapter_message(error)))?;
        self.replace_in_flight_runtime_turn(&review_turn_id, "Plan review")
            .await?;
        self.project_plan_review(&context, false, None).await;
        if let Some(persistence) = self.persistence.as_ref() {
            persistence
                .persist_snapshot(self.session.snapshot().await)
                .await?;
            let store = persistence.goal_event_store();
            clear_phase1_start_seed(&store)?;
        }
        self.turn_executor
            .active_turn_mutations
            .lock()
            .await
            .remove(&phase1_turn_id);
        self.turn_executor
            .phase1_attempt_states
            .lock()
            .await
            .remove(&phase1_turn_id);
        Ok(())
    }

    async fn settle_failed_phase1_prewrite(
        &self,
        phase1_turn_id: String,
        attempt_state: Arc<AtomicU8>,
        retry_interaction_id: Option<String>,
        source_interaction_id: String,
        mut failure_message: String,
    ) -> anyhow::Result<()> {
        // A same-source HTTP retry may hold this guard while its duplicate
        // Start waits in the coordinator queue. This settlement runs outside
        // that listener, so the duplicate can first observe the durable Failed
        // command plus fresh Intent authority and ACK Existing. Once the Web
        // request releases the guard, this task either parks that fresh gate or
        // consumes it if cancellation/shutdown won in the meantime.
        let _transition = self.turn_executor.interaction_transition.lock().await;
        let attempt_phase = attempt_state.load(Ordering::Acquire);
        let web_cancel_settled = attempt_phase == PHASE1_ATTEMPT_SETTLED;
        let cancelled_before_formal_write = matches!(
            attempt_phase,
            PHASE1_ATTEMPT_CANCELLED | PHASE1_ATTEMPT_SETTLED
        );
        let shutting_down = self.shutting_down.load(Ordering::Acquire);

        if web_cancel_settled {
            // Web Cancel already fsynced the terminal projection and deleted
            // the seed before marking this shared state Settled. Repeating
            // either write here could turn a later I/O fault into a false
            // reconciliation fence after the user already received 2xx. A
            // FIFO CleanupAttempt also owns both tombstones: deleting either
            // here could let an older queued duplicate recreate Planning.
            release_web_turn(&self.in_flight, &phase1_turn_id).await;
            return Ok(());
        }

        if let Some(persistence) = self.persistence.as_ref() {
            let store = persistence.goal_event_store();
            if cancelled_before_formal_write
                && !shutting_down
                && let Some(expected_interaction_id) = retry_interaction_id.as_ref()
            {
                let replay = store.load_code_workflow_replay_committed()?;
                match phase1_retry_intent_review_state(
                    replay.events.iter().map(|event| &event.event),
                    &phase1_turn_id,
                )? {
                    Phase1RetryIntentReviewState::Open(review) => {
                        if review.interaction_id != *expected_interaction_id {
                            return Err(anyhow!(
                                "Phase 1 retry terminal published interaction '{}', expected '{}'",
                                review.interaction_id,
                                expected_interaction_id
                            ));
                        }
                        if let Some(seed) = load_phase1_start_seed(&store)? {
                            validate_phase1_retry_intent_review_for_seed(
                                &review,
                                &phase1_turn_id,
                                &seed,
                            )?;
                        }
                        store.append_code_workflow_durable(
                            CodeWorkflowEventKind::InteractionResolved {
                                interaction_id: review.interaction_id,
                                resolution: "cancel".to_string(),
                                command: None,
                                prior_interaction_resolutions: Vec::new(),
                                intent_revision_consumption: None,
                            },
                        )?;
                    }
                    Phase1RetryIntentReviewState::Resolved { review, resolution }
                        if review.interaction_id == *expected_interaction_id
                            && IntentReviewDecision::from_wire_id(&resolution)
                                == Some(IntentReviewDecision::Cancel) => {}
                    state => {
                        return Err(anyhow!(
                            "Phase 1 retry cancellation found incompatible durable terminal state: {state:?}"
                        ));
                    }
                }
            }
        }

        self.session
            .resolve_interaction(&source_interaction_id)
            .await;
        if cancelled_before_formal_write {
            failure_message = "Phase 1 planning cancelled before any formal write".to_string();
        }
        finalize_terminal_phase1_projection(
            &self.session,
            &failure_message,
            if cancelled_before_formal_write {
                "cancelled"
            } else {
                "error"
            },
        )
        .await;
        if let Some(persistence) = self.persistence.as_ref() {
            persistence
                .persist_snapshot(self.session.snapshot().await)
                .await?;
            clear_phase1_start_seed(&persistence.goal_event_store())?;
        }

        self.turn_executor
            .active_turn_mutations
            .lock()
            .await
            .remove(&phase1_turn_id);
        self.turn_executor
            .phase1_attempt_states
            .lock()
            .await
            .remove(&phase1_turn_id);
        release_web_turn(&self.in_flight, &phase1_turn_id).await;

        if !shutting_down && !cancelled_before_formal_write && retry_interaction_id.is_some() {
            self.restore_pending_intent_review_gate().await?;
        }
        Ok(())
    }

    async fn prepare_network_gate(
        &self,
        plan_interaction_id: &str,
    ) -> Result<PreparedNetworkGate, CodeUiApiError> {
        let persistence = self.persistence.as_ref().ok_or_else(|| {
            CodeUiApiError::unsupported_from_error(anyhow!(
                "Phase 1 network gate requires session persistence"
            ))
        })?;
        let store = persistence.goal_event_store();
        let replay = store
            .load_code_workflow_replay()
            .map_err(|error| CodeUiApiError::unsupported_from_error(error.into()))?;
        let context_id = crate::internal::ai::runtime::phase1::phase1_context_id_for_interaction(
            replay.events.iter().map(|event| &event.event),
            plan_interaction_id,
        )
        .ok_or_else(|| {
            CodeUiApiError::unsupported_from_error(anyhow!(
                "Plan review has no durable context binding"
            ))
        })?;
        let mut context = load_phase1_review_context(&store, &context_id)
            .map_err(|error| CodeUiApiError::unsupported_from_error(error.into()))?;
        context.interaction_id = plan_interaction_id.to_string();
        context
            .checkout
            .validate_same_intent_repository(
                self.turn_executor.registry.working_dir(),
                &context.intent_spec,
            )
            .await
            .map_err(|error| {
                CodeUiApiError::conflict(
                    "PHASE1_WORKSPACE_CHANGED",
                    format!(
                        "The Intent repository changed since this plan was generated ({error}); the Plan gate remains pending. Choose Cancel and start a new request so the current repository receives a new IntentSpec review."
                    ),
                )
            })?;
        context
            .checkout
            .validate_exact(
                self.turn_executor.registry.working_dir(),
                &context.intent_spec,
            )
            .await
            .map_err(|error| {
                CodeUiApiError::conflict(
                    "PHASE1_WORKSPACE_CHANGED",
                    format!(
                        "Libra could not verify the exact checkout and workspace still match this plan ({error}); the Plan gate remains pending. Retry after filesystem activity settles, choose Modify to regenerate against the current checkout, or Cancel."
                    ),
                )
            })?;
        let network_interaction_id = stable_phase1_gate_id(
            &network_policy_interaction_id(context.plan_id()),
            plan_interaction_id,
        );
        let gate_turn_id = stable_phase1_gate_id("network-policy-turn", plan_interaction_id);
        if let Err(error) =
            store.append_code_workflow_durable(CodeWorkflowEventKind::NetworkPolicyRequested {
                interaction_id: network_interaction_id.clone(),
                plan_id: context.plan_id().unwrap_or_default().to_string(),
                turn_id: gate_turn_id.clone(),
                default_allow: context.default_allow_network,
            })
        {
            let ambiguity = anyhow!(
                "Network gate marker append could not be durably confirmed; session requires reconciliation: {error}"
            );
            self.fence_phase1_failure(anyhow!(ambiguity.to_string()))
                .await;
            return Err(CodeUiApiError::unsupported_from_error(ambiguity));
        }
        Ok(PreparedNetworkGate {
            plan_interaction_id: plan_interaction_id.to_string(),
            network_interaction_id,
            gate_turn_id,
            context,
        })
    }

    async fn park_network_gate(&self, prepared: PreparedNetworkGate) -> anyhow::Result<()> {
        let owns_gate_turn = self
            .in_flight
            .lock()
            .await
            .as_ref()
            .is_some_and(|turn| turn.runtime_turn_id == prepared.gate_turn_id);
        let already_parked = owns_gate_turn
            && self
                .session
                .snapshot()
                .await
                .interactions
                .iter()
                .any(|item| {
                    item.id == prepared.network_interaction_id
                        && item.status == CodeUiInteractionStatus::Pending
                });
        if already_parked {
            if let Some(persistence) = self.persistence.as_ref() {
                persistence
                    .persist_snapshot(self.session.snapshot().await)
                    .await?;
            }
            return Ok(());
        }
        self.runtime
            .track_external_turn(
                TurnRequest::new(
                    self.runtime_session_id.clone(),
                    prepared.gate_turn_id.clone(),
                    if prepared.context.default_allow_network {
                        "network policy (default: allow)"
                    } else {
                        "network policy (default: deny)"
                    },
                    false,
                ),
                CancellationToken::new(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .map_err(|error| anyhow!(runtime_worker_adapter_message(error)))?;
        self.runtime
            .register_interaction_with_delivery(
                self.runtime_session_id.clone(),
                prepared.gate_turn_id.clone(),
                InteractionState::AwaitingNetworkPolicy {
                    interaction_id: prepared.network_interaction_id.clone(),
                },
                Box::new(NetworkPolicyAckDelivery::new()),
            )
            .await
            .map_err(|error| anyhow!(runtime_worker_adapter_message(error)))?;
        self.replace_in_flight_runtime_turn(&prepared.gate_turn_id, "Network policy")
            .await?;
        self.session
            .resolve_interaction(&prepared.plan_interaction_id)
            .await;
        self.project_network_policy(
            &prepared.context,
            &prepared.network_interaction_id,
            false,
            None,
        )
        .await;
        if let Some(persistence) = self.persistence.as_ref() {
            persistence
                .persist_snapshot(self.session.snapshot().await)
                .await?;
        }
        Ok(())
    }

    /// W2-04: after Network Allow, admit confirmed plan execution onto the
    /// serialized worker queue. Mutating tools still pass through the shared
    /// hardening / approval / sandbox / ACL boundary.
    async fn start_confirmed_plan_execution(
        self: &Arc<Self>,
        network_interaction_id: &str,
    ) -> anyhow::Result<()> {
        use crate::internal::ai::{
            agent::runtime::ToolLoopCancellation,
            intentspec::types::NetworkPolicy,
            orchestrator::{
                Orchestrator,
                types::{OrchestratorConfig, PersistedPlanReviewBundle},
            },
        };

        let persistence = self.persistence.as_ref().ok_or_else(|| {
            anyhow!("Confirmed plan execution requires durable Phase 1 session persistence")
        })?;
        let store = persistence.goal_event_store();
        let replay = store.load_code_workflow_replay()?;
        let plan_id = replay
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.event {
                CodeWorkflowEventKind::NetworkPolicyRequested {
                    interaction_id,
                    plan_id,
                    ..
                } if interaction_id == network_interaction_id => Some(plan_id.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                anyhow!(
                    "network-policy gate '{network_interaction_id}' has no durable request marker"
                )
            })?;
        let source_context_id = replay
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.event {
                CodeWorkflowEventKind::PlanReviewRequested {
                    interaction_id,
                    plan_id: candidate,
                    context_id,
                    ..
                } if candidate == &plan_id => Some(if context_id.is_empty() {
                    interaction_id.clone()
                } else {
                    context_id.clone()
                }),
                _ => None,
            })
            .ok_or_else(|| anyhow!("network-policy Allow has no matching plan review"))?;
        let context = load_phase1_review_context(&store, &source_context_id)?;
        if context.plan_id().unwrap_or_default() != plan_id {
            return Err(anyhow!(
                "network-policy Allow context does not match plan id"
            ));
        }
        context
            .checkout
            .validate_same_intent_repository(
                self.turn_executor.registry.working_dir(),
                &context.intent_spec,
            )
            .await
            .map_err(|error| {
                anyhow!(
                    "The Intent repository changed since this plan was generated ({error}); confirmed execution was not started."
                )
            })?;
        context
            .checkout
            .validate_exact(
                self.turn_executor.registry.working_dir(),
                &context.intent_spec,
            )
            .await
            .map_err(|error| {
                anyhow!(
                    "Libra could not verify the exact checkout still matches this plan ({error}); confirmed execution was not started."
                )
            })?;

        let mut spec = context.intent_spec.clone();
        spec.constraints.security.network_policy = NetworkPolicy::Allow;
        let persisted_plan_id = context.plan_id().map(str::to_owned);
        let persisted_plan_bundle = match &context.persisted_plan {
            Phase1PersistedPlan::Persisted {
                execution_plan_id,
                test_plan_id,
            } => Some(PersistedPlanReviewBundle {
                plan_id: execution_plan_id.clone(),
                test_plan_id: test_plan_id.clone(),
                step_ids: HashMap::new(),
                task_ids: HashMap::new(),
                plan_id_by_task_id: HashMap::new(),
            }),
            Phase1PersistedPlan::Unavailable => None,
        };
        let approved_plan = context.execution_plan.clone();
        let intent_id = context.intent_id.clone();
        let runtime_turn_id = format!("plan-exec-{}", uuid::Uuid::new_v4());
        let session_id = self.runtime_session_id.clone();
        let model = (*self.turn_executor.model).clone();
        let base_registry = self.turn_executor.registry.clone();
        let mut tool_loop_config = (self.turn_executor.config_factory)();
        let mcp_server = self.turn_executor.mcp_server.clone();
        let working_dir = base_registry.working_dir().to_path_buf();
        let session = self.session.clone();
        let persistence_for_repair = persistence.clone();
        let runtime_for_repair = self.runtime.clone();
        let session_id_for_repair = session_id.clone();

        self.session
            .resolve_interaction(network_interaction_id)
            .await;
        let assistant_entry_id = format!("assistant-plan-exec-{runtime_turn_id}");
        self.session
            .upsert_transcript_entry(CodeUiTranscriptEntry {
                id: assistant_entry_id.clone(),
                kind: CodeUiTranscriptEntryKind::AssistantMessage,
                title: Some("Plan execution".to_string()),
                content: Some(String::new()),
                status: Some("streaming".to_string()),
                streaming: true,
                metadata: serde_json::json!({ "phase": "plan_execution" }),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .await;
        self.session.set_status(CodeUiSessionStatus::Thinking).await;
        persistence
            .persist_snapshot(self.session.snapshot().await)
            .await?;
        self.replace_in_flight_runtime_turn(&runtime_turn_id, "Confirmed plan execution")
            .await?;

        let observer_session = session.clone();
        let observer_entry_id = assistant_entry_id.clone();
        let tokio_handle = tokio::runtime::Handle::current();
        let observer = Arc::new(WebPlanExecutionObserver {
            session: observer_session,
            assistant_entry_id: observer_entry_id,
            handle: tokio_handle,
        });

        let runner_session = session.clone();
        let runner_entry_id = assistant_entry_id.clone();
        let runner_in_flight = self.in_flight.clone();
        let runner_turn_id = runtime_turn_id.clone();
        let runner = Box::new(move |exec_context: RuntimeExecutionContext| {
            Box::pin(async move {
                tool_loop_config.cancellation = Some(ToolLoopCancellation::new(
                    exec_context.cancellation(),
                    exec_context.mutation_started_marker(),
                ));
                let hardened_boundary = exec_context.tool_boundary().with_network_access(true);
                let registry = Arc::new((*base_registry).clone().with_hardening(hardened_boundary));
                if let Some(subagent_runtime) = tool_loop_config.subagent_runtime.as_mut() {
                    subagent_runtime.tool_registry = (*registry).clone();
                }
                let config = OrchestratorConfig {
                    working_dir,
                    base_commit: None,
                    persisted_intent_id: Some(intent_id),
                    persisted_plan_bundle,
                    persisted_plan_id,
                    initial_plan: Some(approved_plan),
                    dagrs_resume_checkpoint_id: None,
                    tool_loop_config,
                    coder_preamble: None,
                    reviewer_preamble: None,
                    mcp_server,
                    observer: Some(observer),
                    phase_confirmer: None,
                };
                let orchestrator = Orchestrator::new(model, registry, config);
                let run_result = orchestrator.run(spec).await;
                let summary = match &run_result {
                    Ok(result) => format!(
                        "Confirmed plan execution finished with decision {:?}.",
                        result.decision
                    ),
                    Err(error) => format!("Confirmed plan execution failed: {error}"),
                };
                finalize_assistant_entry(
                    &runner_session,
                    &runner_entry_id,
                    &summary,
                    if run_result.is_ok() {
                        "completed"
                    } else {
                        "error"
                    },
                )
                .await;
                match run_result {
                    Ok(result) => {
                        let needs_repair = !matches!(
                            result.decision,
                            crate::internal::ai::orchestrator::types::DecisionOutcome::Commit
                        );
                        if needs_repair {
                            park_web_plan_execution_repair(
                                &persistence_for_repair,
                                &runtime_for_repair,
                                &session_id_for_repair,
                                &runner_session,
                                Some(&result),
                                Some(&summary),
                            )
                            .await?;
                        } else {
                            runner_session.set_status(CodeUiSessionStatus::Idle).await;
                            if let Err(error) = persistence_for_repair
                                .persist_snapshot(runner_session.snapshot().await)
                                .await
                            {
                                return Err(RuntimeWorkerError::DurabilityFailure(format!(
                                    "failed to persist confirmed plan execution snapshot: {error}"
                                )));
                            }
                        }
                        release_web_turn(&runner_in_flight, &runner_turn_id).await;
                        Ok(RuntimeTurnExecution::Completed { summary })
                    }
                    Err(error) => {
                        park_web_plan_execution_repair(
                            &persistence_for_repair,
                            &runtime_for_repair,
                            &session_id_for_repair,
                            &runner_session,
                            None,
                            Some(&error.to_string()),
                        )
                        .await?;
                        release_web_turn(&runner_in_flight, &runner_turn_id).await;
                        Err(orchestrator_error_to_runtime(error))
                    }
                }
            })
                as std::pin::Pin<
                    Box<
                        dyn std::future::Future<
                                Output = Result<RuntimeTurnExecution, RuntimeWorkerError>,
                            > + Send,
                    >,
                >
        });

        match submit_confirmed_plan_execution(
            &self.runtime,
            self.plan_execution_executor.as_ref(),
            session_id,
            runtime_turn_id.clone(),
            runner,
        )
        .await
        {
            Ok(_) => Ok(()),
            Err(error) => {
                release_web_turn(&self.in_flight, &runtime_turn_id).await;
                self.session.set_status(CodeUiSessionStatus::Idle).await;
                Err(anyhow!(runtime_worker_adapter_message(error)))
            }
        }
    }

    async fn prepare_plan_gate_from_network(
        &self,
        network_interaction_id: &str,
    ) -> anyhow::Result<PreparedPlanGate> {
        let persistence = self
            .persistence
            .as_ref()
            .ok_or_else(|| anyhow!("Phase 1 Back requires session persistence"))?;
        let store = persistence.goal_event_store();
        let replay = store.load_code_workflow_replay()?;
        let plan_id = replay
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.event {
                CodeWorkflowEventKind::NetworkPolicyRequested {
                    interaction_id,
                    plan_id,
                    ..
                } if interaction_id == network_interaction_id => Some(plan_id.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                anyhow!(
                    "network-policy gate '{network_interaction_id}' has no durable request marker"
                )
            })?;
        let (_source_plan_interaction_id, source_context_id) = replay
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.event {
                CodeWorkflowEventKind::PlanReviewRequested {
                    interaction_id,
                    plan_id: candidate,
                    context_id,
                    ..
                } if candidate == &plan_id => Some((
                    interaction_id.clone(),
                    if context_id.is_empty() {
                        interaction_id.clone()
                    } else {
                        context_id.clone()
                    },
                )),
                _ => None,
            })
            .ok_or_else(|| anyhow!("network-policy gate has no matching plan review"))?;
        let mut context = load_phase1_review_context(&store, &source_context_id)?;
        if context.plan_id().unwrap_or_default() != plan_id {
            return Err(anyhow!(
                "network-policy Back context does not match plan id"
            ));
        }
        let workspace_warning = self.phase1_workspace_warning(&context).await;
        let plan_interaction_id = stable_phase1_gate_id("plan-review-back", network_interaction_id);
        context.interaction_id = plan_interaction_id.clone();
        let gate_turn_id = stable_phase1_gate_id("plan-review-back-turn", network_interaction_id);
        // Crash ordering: reopening Plan review is durable before Back closes
        // the network gate, so recovery demotes network and cannot skip Plan.
        if let Err(error) =
            store.append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
                interaction_id: plan_interaction_id.clone(),
                plan_id,
                turn_id: gate_turn_id.clone(),
                phase1_turn_id: String::new(),
                context_id: source_context_id,
                revision_of: None,
                prepared_from_network: Some(network_interaction_id.to_string()),
            })
        {
            let ambiguity = anyhow!(
                "Plan Back marker append could not be durably confirmed; session requires reconciliation: {error}"
            );
            self.fence_phase1_failure(anyhow!(ambiguity.to_string()))
                .await;
            return Err(ambiguity);
        }
        Ok(PreparedPlanGate {
            network_interaction_id: network_interaction_id.to_string(),
            plan_interaction_id,
            gate_turn_id,
            context,
            workspace_warning,
        })
    }

    async fn park_plan_gate_from_network(&self, prepared: PreparedPlanGate) -> anyhow::Result<()> {
        let owns_gate_turn = self
            .in_flight
            .lock()
            .await
            .as_ref()
            .is_some_and(|turn| turn.runtime_turn_id == prepared.gate_turn_id);
        let already_parked = owns_gate_turn
            && self
                .session
                .snapshot()
                .await
                .interactions
                .iter()
                .any(|item| {
                    item.id == prepared.plan_interaction_id
                        && item.status == CodeUiInteractionStatus::Pending
                });
        if already_parked {
            if let Some(persistence) = self.persistence.as_ref() {
                persistence
                    .persist_snapshot(self.session.snapshot().await)
                    .await?;
            }
            return Ok(());
        }
        self.runtime
            .track_external_turn(
                TurnRequest::new(
                    self.runtime_session_id.clone(),
                    prepared.gate_turn_id.clone(),
                    "Plan review",
                    false,
                ),
                CancellationToken::new(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .map_err(|error| anyhow!(runtime_worker_adapter_message(error)))?;
        self.runtime
            .register_interaction_with_delivery(
                self.runtime_session_id.clone(),
                prepared.gate_turn_id.clone(),
                InteractionState::AwaitingPlanReview {
                    interaction_id: prepared.plan_interaction_id.clone(),
                },
                Box::new(PlanReviewAckDelivery::new()),
            )
            .await
            .map_err(|error| anyhow!(runtime_worker_adapter_message(error)))?;
        self.replace_in_flight_runtime_turn(&prepared.gate_turn_id, "Plan review")
            .await?;
        self.session
            .resolve_interaction(&prepared.network_interaction_id)
            .await;
        self.project_plan_review(
            &prepared.context,
            false,
            prepared.workspace_warning.as_deref(),
        )
        .await;
        if let Some(persistence) = self.persistence.as_ref() {
            persistence
                .persist_snapshot(self.session.snapshot().await)
                .await?;
        }
        Ok(())
    }

    async fn replace_in_flight_runtime_turn(
        &self,
        runtime_turn_id: &str,
        input: &str,
    ) -> anyhow::Result<()> {
        let mut slot = self.in_flight.lock().await;
        if let Some(turn) = slot.as_mut() {
            turn.runtime_turn_id = runtime_turn_id.to_string();
            turn.input = input.to_string();
        } else {
            *slot = Some(InFlightTurn {
                runtime_turn_id: runtime_turn_id.to_string(),
                input: input.to_string(),
                assistant_entry_id: format!("restored-phase1-{runtime_turn_id}"),
                mode: WebTurnMode::PlanPhase0,
                start_gate: Arc::new(tokio::sync::Notify::new()),
                start_open: Arc::new(AtomicBool::new(true)),
                completion: Arc::new(tokio::sync::Notify::new()),
            });
        }
        Ok(())
    }

    async fn restore_phase1_start_seed(&self) -> anyhow::Result<bool> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(false);
        };
        let store = persistence.goal_event_store();
        let Some(seed) = load_phase1_start_seed(&store)? else {
            return Ok(false);
        };
        let phase1_turn_id = crate::internal::ai::runtime::phase1::phase1_turn_id_from_seed(&seed)?;
        let replay = store.load_code_workflow_replay()?;
        let durable_resolution = replay
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.event {
                CodeWorkflowEventKind::InteractionResolved {
                    interaction_id,
                    resolution,
                    intent_revision_consumption: None,
                    ..
                }
                | CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    interaction_id,
                    resolution,
                    ..
                } if interaction_id == &seed.source_interaction_id => Some(resolution.clone()),
                _ => None,
            });
        let Some(durable_resolution) = durable_resolution else {
            return Err(anyhow!(
                "Phase 1 start seed '{}' has no durable source resolution",
                seed.source_interaction_id
            ));
        };
        if !durable_resolution.eq_ignore_ascii_case(&seed.source_resolution) {
            // A failed Confirm/Modify attempt may leave its pre-response seed
            // behind, then the still-open gate can later resolve another way.
            // That terminal choice invalidates the seed; it is not ambiguity.
            clear_phase1_start_seed(&store)?;
            return Ok(false);
        }
        self.session
            .resolve_interaction(&seed.source_interaction_id)
            .await;
        self.session.set_status(CodeUiSessionStatus::Thinking).await;
        persistence
            .persist_snapshot(self.session.snapshot().await)
            .await?;
        tokio::time::timeout(
            Duration::from_secs(5),
            self.phase1_tx.send(HeadlessPhase1Command::Start {
                confirmed: ConfirmedIntentForPhase1 {
                    source_interaction_id: seed.source_interaction_id.clone(),
                    seed_digest: seed.durable_digest()?,
                    intent_id: seed.intent_id,
                    intent_spec_id: seed.intent_spec_id,
                    intent_spec_json: seed.intent_spec_json,
                    revision_note: seed.revision_note,
                    checkout: Some(seed.checkout),
                    revision_source_interaction_id: seed
                        .source_resolution
                        .eq_ignore_ascii_case("modify")
                        .then_some(seed.source_interaction_id),
                    prior_plan: seed.prior_plan,
                    prior_plan_id: seed.prior_plan_id,
                    prior_persisted_plan: seed.prior_persisted_plan,
                    phase1_turn_id_override: Some(phase1_turn_id),
                },
                admitted: None,
                start: None,
            }),
        )
        .await
        .map_err(|_| anyhow!("Phase 1 coordinator queue remained full during startup recovery"))?
        .map_err(|_| anyhow!("Phase 1 coordinator stopped during startup recovery"))?;
        Ok(true)
    }

    async fn restore_pending_phase1_gate(&self) -> anyhow::Result<bool> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(false);
        };
        let store = persistence.goal_event_store();
        let replay = store.load_code_workflow_replay()?;
        if let Some((interaction_id, plan_id, stored_turn_id, default_allow)) =
            open_network_policy_from_workflow(replay.events.iter().map(|event| &event.event))
        {
            let (plan_interaction_id, context_id) = replay
                .events
                .iter()
                .rev()
                .find_map(|event| match &event.event {
                    CodeWorkflowEventKind::PlanReviewRequested {
                        interaction_id,
                        plan_id: candidate,
                        context_id,
                        ..
                    } if candidate == &plan_id => Some((
                        interaction_id.clone(),
                        if context_id.is_empty() {
                            interaction_id.clone()
                        } else {
                            context_id.clone()
                        },
                    )),
                    _ => None,
                })
                .ok_or_else(|| {
                    anyhow!(
                        "open network-policy gate '{interaction_id}' has no matching plan review"
                    )
                })?;
            let mut context = load_phase1_review_context(&store, &context_id)?;
            context.interaction_id = plan_interaction_id;
            if context.plan_id().unwrap_or_default() != plan_id
                || context.default_allow_network != default_allow
            {
                return Err(anyhow!(
                    "network-policy gate '{interaction_id}' does not match its Phase 1 context"
                ));
            }
            let workspace_warning = self.phase1_workspace_warning(&context).await;
            let mut turn_id = stored_turn_id;
            if turn_id.is_empty() {
                turn_id = format!("network-policy-restore-{}", uuid::Uuid::new_v4());
                store.append_code_workflow_durable(
                    CodeWorkflowEventKind::NetworkPolicyRequested {
                        interaction_id: interaction_id.clone(),
                        plan_id: plan_id.clone(),
                        turn_id: turn_id.clone(),
                        default_allow,
                    },
                )?;
            }
            if let Err(error) = self
                .runtime
                .track_external_turn(
                    TurnRequest::new(
                        self.runtime_session_id.clone(),
                        turn_id.clone(),
                        if default_allow {
                            "network policy (default: allow)"
                        } else {
                            "network policy (default: deny)"
                        },
                        false,
                    ),
                    CancellationToken::new(),
                    Arc::new(AtomicBool::new(false)),
                )
                .await
            {
                if !matches!(
                    error,
                    RuntimeWorkerError::IdempotentCommand { ack_ok: true, .. }
                ) {
                    return Err(anyhow!(runtime_worker_adapter_message(error)));
                }
                turn_id = format!("network-policy-restore-{}", uuid::Uuid::new_v4());
                store.append_code_workflow_durable(
                    CodeWorkflowEventKind::NetworkPolicyRequested {
                        interaction_id: interaction_id.clone(),
                        plan_id: plan_id.clone(),
                        turn_id: turn_id.clone(),
                        default_allow,
                    },
                )?;
                self.runtime
                    .track_external_turn(
                        TurnRequest::new(
                            self.runtime_session_id.clone(),
                            turn_id.clone(),
                            if default_allow {
                                "network policy (default: allow)"
                            } else {
                                "network policy (default: deny)"
                            },
                            false,
                        ),
                        CancellationToken::new(),
                        Arc::new(AtomicBool::new(false)),
                    )
                    .await
                    .map_err(|error| anyhow!(runtime_worker_adapter_message(error)))?;
            }
            self.runtime
                .register_interaction_with_delivery(
                    self.runtime_session_id.clone(),
                    turn_id.clone(),
                    InteractionState::AwaitingNetworkPolicy {
                        interaction_id: interaction_id.clone(),
                    },
                    Box::new(NetworkPolicyAckDelivery::new()),
                )
                .await
                .map_err(|error| anyhow!(runtime_worker_adapter_message(error)))?;
            self.replace_in_flight_runtime_turn(&turn_id, "Network policy")
                .await?;
            clear_phase1_start_seed(&store)?;
            self.project_network_policy(
                &context,
                &interaction_id,
                true,
                workspace_warning.as_deref(),
            )
            .await;
            persistence
                .persist_snapshot(self.session.snapshot().await)
                .await?;
            return Ok(true);
        }

        let Some((interaction_id, plan_id, stored_turn_id, phase1_turn_id)) =
            open_plan_review_from_workflow(replay.events.iter().map(|event| &event.event))
        else {
            return Ok(false);
        };
        let context_id = crate::internal::ai::runtime::phase1::phase1_context_id_for_interaction(
            replay.events.iter().map(|event| &event.event),
            &interaction_id,
        )
        .ok_or_else(|| anyhow!("open Plan review has no durable context binding"))?;
        let mut context = load_phase1_review_context(&store, &context_id)?;
        context.interaction_id = interaction_id.clone();
        if context.plan_id().unwrap_or_default() != plan_id {
            return Err(anyhow!(
                "plan review '{interaction_id}' does not match its Phase 1 context"
            ));
        }
        let workspace_warning = self.phase1_workspace_warning(&context).await;
        let mut turn_id = stored_turn_id;
        if turn_id.is_empty() {
            turn_id = format!("plan-review-restore-{}", uuid::Uuid::new_v4());
            store.append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
                interaction_id: interaction_id.clone(),
                plan_id: plan_id.clone(),
                turn_id: turn_id.clone(),
                phase1_turn_id: phase1_turn_id.clone(),
                context_id: context_id.clone(),
                revision_of: None,
                prepared_from_network: None,
            })?;
        }
        if let Err(error) = self
            .runtime
            .track_external_turn(
                TurnRequest::new(
                    self.runtime_session_id.clone(),
                    turn_id.clone(),
                    "Plan review",
                    false,
                ),
                CancellationToken::new(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
        {
            if !matches!(
                error,
                RuntimeWorkerError::IdempotentCommand { ack_ok: true, .. }
            ) {
                return Err(anyhow!(runtime_worker_adapter_message(error)));
            }
            turn_id = format!("plan-review-restore-{}", uuid::Uuid::new_v4());
            store.append_code_workflow_durable(CodeWorkflowEventKind::PlanReviewRequested {
                interaction_id: interaction_id.clone(),
                plan_id: plan_id.clone(),
                turn_id: turn_id.clone(),
                phase1_turn_id,
                // A Back generation has a fresh interaction id but continues
                // to reference the immutable source plan context. Preserve
                // that authority when replacing a terminal runtime turn so a
                // subsequent resume does not look for a nonexistent sidecar
                // keyed by the gate-generation id.
                context_id: context_id.clone(),
                revision_of: None,
                prepared_from_network: None,
            })?;
            self.runtime
                .track_external_turn(
                    TurnRequest::new(
                        self.runtime_session_id.clone(),
                        turn_id.clone(),
                        "Plan review",
                        false,
                    ),
                    CancellationToken::new(),
                    Arc::new(AtomicBool::new(false)),
                )
                .await
                .map_err(|error| anyhow!(runtime_worker_adapter_message(error)))?;
        }
        self.runtime
            .register_interaction_with_delivery(
                self.runtime_session_id.clone(),
                turn_id.clone(),
                InteractionState::AwaitingPlanReview {
                    interaction_id: interaction_id.clone(),
                },
                Box::new(PlanReviewAckDelivery::new()),
            )
            .await
            .map_err(|error| anyhow!(runtime_worker_adapter_message(error)))?;
        self.replace_in_flight_runtime_turn(&turn_id, "Plan review")
            .await?;
        clear_phase1_start_seed(&store)?;
        self.project_plan_review(&context, true, workspace_warning.as_deref())
            .await;
        persistence
            .persist_snapshot(self.session.snapshot().await)
            .await?;
        Ok(true)
    }

    async fn reconcile_intent_revision_sidecar(&self) -> anyhow::Result<()> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(());
        };
        let store = persistence.goal_event_store();
        let replay = store.load_intent_revision_workflow_replay_committed()?;
        let revision_index =
            validate_all_intent_revision_consumption_receipts(persistence, &replay)?;
        let sidecar = load_intent_revision_sidecar(persistence)?;
        let mut unconsumed_terminal = None::<IntentRevisionTerminalAuthority>;

        match sidecar {
            None => {}
            Some(LoadedIntentRevisionSidecar::Prepared(prepared)) => {
                match intent_revision_terminal_binding_from_index(
                    &revision_index,
                    &prepared.interaction_id,
                    Some(&prepared.command.command_id),
                )? {
                    None => {
                        if !prepared_matches_open_intent_review(persistence, &replay, &prepared)? {
                            return Err(anyhow!(
                                "prepared IntentSpec revision has no exact open review lineage"
                            ));
                        }
                        // Prepared is dormant until a matching combined
                        // terminal commits. With the original gate still open,
                        // discard it durably and let normal gate restoration
                        // re-register the source interaction.
                        clear_pending_intent_revision(persistence)?;
                    }
                    Some(IntentRevisionTerminalBinding::Legacy(_)) => {
                        return Err(anyhow!(
                            "prepared IntentSpec revision is bound to a legacy terminal without a sidecar commitment"
                        ));
                    }
                    Some(IntentRevisionTerminalBinding::Bound(terminal)) => {
                        if !prepared_matches_terminal(&prepared, &terminal) {
                            return Err(anyhow!(
                                "prepared IntentSpec revision conflicts with its durable terminal"
                            ));
                        }
                        if exact_intent_revision_consumer_from_index(&revision_index, &terminal)?
                            .is_some()
                        {
                            // A receipt is the irreversible consume boundary.
                            // A resurrected Prepared file is only stale state.
                            clear_pending_intent_revision(persistence)?;
                        } else {
                            if terminal_has_later_web_intent_from_index(&revision_index, &terminal)?
                            {
                                return Err(anyhow!(
                                    "prepared IntentSpec revision is ambiguous after a later durable Web command"
                                ));
                            }
                            // CLI resume folds the durable combined terminal
                            // before this reconciliation pass, so the source
                            // projection may already be Resolved/Completed.
                            // The exact Prepared HMAC/lineage above is the
                            // authority; a derived Pending projection is not.
                            let pending = promote_prepared_intent_revision(
                                persistence,
                                &terminal.interaction_id,
                                &terminal.command.command_id,
                                None,
                            )?;
                            let authenticated = authenticate_active_intent_revision_from_index(
                                persistence,
                                &revision_index,
                                pending,
                            )?;
                            unconsumed_terminal = Some(authenticated.terminal);
                        }
                    }
                }
            }
            Some(LoadedIntentRevisionSidecar::Active(active)) => {
                if active.authority.is_none()
                    && legacy_active_matches_open_intent_review(
                        persistence,
                        &replay,
                        &revision_index,
                        &active,
                    )?
                {
                    // Baseline Web wrote the legacy-valid sidecar before the
                    // combined Modify terminal. With the source marker still
                    // open, that file is dormant residue, not revision mode.
                    clear_pending_intent_revision(persistence)?;
                    // Remaining bound terminals are still checked below.
                    for terminal in bound_intent_revision_terminals_from_index(&revision_index) {
                        if exact_intent_revision_consumer_from_index(&revision_index, &terminal)?
                            .is_none()
                        {
                            return Err(anyhow!(
                                "bound IntentSpec Modify terminal is missing both its sidecar and an exact consumption receipt"
                            ));
                        }
                    }
                    return Ok(());
                }
                let original = active.clone();
                let authenticated = authenticate_active_intent_revision_from_index(
                    persistence,
                    &revision_index,
                    active,
                )?;
                if exact_intent_revision_consumer_from_index(
                    &revision_index,
                    &authenticated.terminal,
                )?
                .is_some()
                {
                    clear_pending_intent_revision(persistence)?;
                } else {
                    if terminal_has_later_web_intent_from_index(
                        &revision_index,
                        &authenticated.terminal,
                    )? {
                        return Err(anyhow!(
                            "active IntentSpec revision is ambiguous after a later durable Web command"
                        ));
                    }
                    if authenticated.pending != original {
                        // Persist a validated legacy migration before exposing
                        // it in memory. A later generic Web intent must not make
                        // the same sidecar look ambiguous on the next restart.
                        persist_pending_intent_revision(persistence, &authenticated.pending)?;
                    }
                    unconsumed_terminal = Some(authenticated.terminal);
                }
            }
            Some(LoadedIntentRevisionSidecar::Claiming(claiming)) => {
                validate_claiming_intent_revision(persistence, &claiming)?;
                return Err(anyhow!(
                    "IntentSpec revision consumer claim survived startup mutation-recovery preflight"
                ));
            }
            Some(LoadedIntentRevisionSidecar::Consuming(consuming)) => {
                let authenticated = authenticate_active_intent_revision_from_index(
                    persistence,
                    &revision_index,
                    consuming.active.clone(),
                )?;
                let expected = pending_consumption_binding(
                    &authenticated.pending,
                    consuming.consumption.claim.consumer_intent.clone(),
                )?;
                if expected != consuming.consumption.claim {
                    return Err(anyhow!(
                        "consuming IntentSpec revision conflicts with its durable source authority"
                    ));
                }
                match exact_intent_revision_consumer_from_index(
                    &revision_index,
                    &authenticated.terminal,
                )? {
                    Some(receipt) if receipt == &consuming.consumption => {
                        clear_pending_intent_revision(persistence)?;
                    }
                    Some(_) => {
                        return Err(anyhow!(
                            "consuming IntentSpec revision conflicts with its durable receipt"
                        ));
                    }
                    None => {
                        validate_uncommitted_intent_revision_consumer_from_index(
                            persistence,
                            &revision_index,
                            &authenticated.terminal,
                            &consuming.consumption,
                        )?;
                        // The consumer never crossed the receipt boundary.
                        // Keep the downgrade-safe Consuming envelope on disk:
                        // its exact consumer attribution is needed on every
                        // later restart. Only hand the authenticated Active
                        // body to this process's in-memory restore path.
                        let mut recovered = self.uncommitted_consuming_intent_revision.lock().await;
                        if recovered.replace(authenticated.pending.clone()).is_some() {
                            return Err(anyhow!(
                                "multiple uncommitted IntentSpec revision consumers were recovered"
                            ));
                        }
                        unconsumed_terminal = Some(authenticated.terminal);
                    }
                }
            }
        }

        // Every digest-bound terminal except the single Active/Prepared
        // revision must have an exact receipt. Sidecar absence alone is never
        // accepted as proof of consumption.
        for terminal in bound_intent_revision_terminals_from_index(&revision_index) {
            if unconsumed_terminal.as_ref().is_some_and(|candidate| {
                candidate.terminal_event_id == terminal.terminal_event_id
                    && candidate.terminal_sequence == terminal.terminal_sequence
            }) {
                continue;
            }
            if exact_intent_revision_consumer_from_index(&revision_index, &terminal)?.is_none() {
                return Err(anyhow!(
                    "bound IntentSpec Modify terminal is missing both its sidecar and an exact consumption receipt"
                ));
            }
        }
        Ok(())
    }

    async fn reconcile_resolved_phase1_projection(&self) -> anyhow::Result<()> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(());
        };
        let store = persistence.goal_event_store();
        let replay = store.load_intent_revision_workflow_replay_committed()?;
        let mut resolved = std::collections::HashSet::new();
        let mut resolved_intent_modifications = HashMap::<String, Option<String>>::new();
        for event in &replay.events {
            match &event.event {
                CodeWorkflowEventKind::InteractionResolved {
                    interaction_id,
                    resolution,
                    prior_interaction_resolutions,
                    intent_revision_consumption: None,
                    ..
                } => {
                    resolved.insert(interaction_id.as_str());
                    if IntentReviewDecision::from_wire_id(resolution)
                        == Some(IntentReviewDecision::Revise)
                    {
                        resolved_intent_modifications
                            .entry(interaction_id.clone())
                            .or_insert(None);
                    }
                    resolved.extend(
                        prior_interaction_resolutions
                            .iter()
                            .map(|(interaction_id, _)| interaction_id.as_str()),
                    );
                }
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    command,
                    interaction_id,
                    resolution,
                    prior_interaction_resolutions,
                    intent_revision,
                    ..
                } => {
                    resolved.insert(interaction_id.as_str());
                    if IntentReviewDecision::from_wire_id(resolution)
                        == Some(IntentReviewDecision::Revise)
                    {
                        match resolved_intent_modifications.get(interaction_id) {
                            Some(Some(existing_command))
                                if existing_command != &command.command_id =>
                            {
                                return Err(anyhow!(
                                    "durable IntentSpec Modify interaction '{}' has conflicting terminal commands",
                                    interaction_id
                                ));
                            }
                            _ => {
                                resolved_intent_modifications.insert(
                                    interaction_id.clone(),
                                    Some(command.command_id.clone()),
                                );
                            }
                        }
                    } else if intent_revision.is_some() {
                        return Err(anyhow!(
                            "durable IntentSpec revision recovery payload for interaction '{}' is not bound to canonical modify",
                            interaction_id
                        ));
                    }
                    resolved.extend(
                        prior_interaction_resolutions
                            .iter()
                            .map(|(interaction_id, _)| interaction_id.as_str()),
                    );
                }
                CodeWorkflowEventKind::CommandTerminalFailure {
                    interaction_resolutions,
                    ..
                } => {
                    resolved.extend(
                        interaction_resolutions
                            .iter()
                            .map(|(interaction_id, _)| interaction_id.as_str()),
                    );
                }
                _ => {}
            }
        }
        let stale = self
            .session
            .snapshot()
            .await
            .interactions
            .into_iter()
            .filter(|interaction| {
                interaction.status == CodeUiInteractionStatus::Pending
                    && resolved.contains(interaction.id.as_str())
            })
            .collect::<Vec<_>>();
        let had_stale_interactions = !stale.is_empty();
        if let Some(interaction) = stale.iter().find(|interaction| {
            interaction.kind == CodeUiInteractionKind::IntentReviewChoice
                && resolved_intent_modifications.contains_key(interaction.id.as_str())
        }) {
            let runtime_turn_id = resolved_intent_modifications
                .get(interaction.id.as_str())
                .cloned()
                .flatten()
                .ok_or_else(|| {
                    anyhow!(
                        "legacy IntentSpec Modify interaction '{}' has no terminal command binding",
                        interaction.id
                    )
                })?;
            let recovered_consuming = {
                let mut recovered = self.uncommitted_consuming_intent_revision.lock().await;
                if let Some(pending) = recovered.as_ref() {
                    let matches = pending.authority.as_ref().is_some_and(|authority| {
                        authority.interaction_id == interaction.id
                            && authority.command.command_id == runtime_turn_id
                    });
                    if !matches {
                        return Err(anyhow!(
                            "uncommitted IntentSpec revision consumer conflicts with the stale Modify projection"
                        ));
                    }
                    recovered.take()
                } else {
                    None
                }
            };
            let pending = match recovered_consuming {
                Some(pending) => pending,
                None => promote_prepared_intent_revision(
                    persistence,
                    &interaction.id,
                    &runtime_turn_id,
                    None,
                )?,
            };
            install_pending_intent_revision_mode(
                &self.session,
                &self.pending_intent_revision,
                pending,
                true,
            )
            .await;
        }
        for interaction in stale {
            self.session.resolve_interaction(&interaction.id).await;
        }

        let snapshot = self.session.snapshot().await;
        let open_intent_review =
            open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event));
        let cancelled_retry = cancelled_phase1_retry_projection_lineage(
            replay.events.iter().map(|event| &event.event),
            &snapshot,
        )?;
        let has_open_gate = open_intent_review.is_some()
            || open_plan_review_from_workflow(replay.events.iter().map(|event| &event.event))
                .is_some()
            || open_network_policy_from_workflow(replay.events.iter().map(|event| &event.event))
                .is_some();
        let has_pending_gate = snapshot
            .interactions
            .iter()
            .any(|interaction| interaction.status == CodeUiInteractionStatus::Pending);
        let recover_cancelled_phase1_projection = cancelled_retry.is_some()
            && !has_open_gate
            && !has_pending_gate
            && load_phase1_start_seed(&store)?.is_none()
            && load_pending_intent_revision(persistence)?.is_none();

        if recover_cancelled_phase1_projection {
            finalize_terminal_phase1_projection(
                &self.session,
                "Phase 1 planning cancelled before any formal write",
                "cancelled",
            )
            .await;
        } else if had_stale_interactions {
            self.session
                .set_status(if has_pending_gate {
                    CodeUiSessionStatus::AwaitingInteraction
                } else {
                    CodeUiSessionStatus::Idle
                })
                .await;
        } else {
            return Ok(());
        }
        persistence
            .persist_snapshot(self.session.snapshot().await)
            .await?;
        Ok(())
    }

    async fn phase1_workspace_warning(&self, context: &Phase1ReviewContext) -> Option<String> {
        if let Err(error) = context
            .checkout
            .validate_identity(
                self.turn_executor.registry.working_dir(),
                &context.intent_spec,
            )
            .await
        {
            return Some(format!(
                "The checkout identity changed since this plan was generated ({error}). Execution remains blocked. If this is the same repository with a new HEAD, choose Modify to regenerate; if the repository changed, Cancel and start a new request so its IntentSpec can be reviewed."
            ));
        }
        match context
            .checkout
            .workspace_change_matches(self.turn_executor.registry.working_dir())
            .await
        {
            Ok(true) => None,
            Ok(false) => Some(
                "Workspace metadata changed since this plan was generated. Execute will perform an exact identity and content recheck and will fail closed only if that authority differs; choose Modify if the files changed, or Cancel."
                    .to_string(),
            ),
            Err(error) => Some(format!(
                "Libra could not refresh the workspace metadata drift signal ({error}). Execute will perform an exact identity and content recheck and fail closed if verification fails; retry after filesystem activity settles, choose Modify if the files changed, or Cancel."
            )),
        }
    }

    async fn project_plan_review(
        &self,
        context: &Phase1ReviewContext,
        restored: bool,
        workspace_warning: Option<&str>,
    ) {
        self.session.upsert_plan(phase1_code_ui_plan(context)).await;
        self.session
            .upsert_interaction(phase1_plan_review_interaction(
                context,
                restored,
                workspace_warning,
            ))
            .await;
        self.session
            .set_status(CodeUiSessionStatus::AwaitingInteraction)
            .await;
    }

    async fn project_network_policy(
        &self,
        context: &Phase1ReviewContext,
        interaction_id: &str,
        restored: bool,
        workspace_warning: Option<&str>,
    ) {
        self.session.upsert_plan(phase1_code_ui_plan(context)).await;
        self.session
            .upsert_interaction(phase1_network_policy_interaction(
                context,
                interaction_id,
                restored,
                workspace_warning,
            ))
            .await;
        self.session
            .set_status(CodeUiSessionStatus::AwaitingInteraction)
            .await;
    }

    /// Production write-path adapter mounted on `CodeUiRuntimeHandle`.
    pub fn command_adapter(&self) -> Arc<AgentRuntimeCodeUiAdapter> {
        self.runtime_bridge.clone()
    }

    /// Rehydrate an unresolved IntentSpec review after process restart so
    /// confirm/modify/cancel cannot disappear while a draft remains unconfirmed.
    async fn restore_pending_intent_review_gate(&self) -> anyhow::Result<()> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(());
        };
        if !self.pending_intent_reviews.lock().await.is_empty() {
            return Ok(());
        }
        let store = persistence.goal_event_store();
        let replay = match store.load_code_workflow_replay() {
            Ok(replay) => replay,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed to load Code workflow replay while restoring IntentSpec review"
                );
                return Ok(());
            }
        };
        let Some((interaction_id, intent_id, stored_turn_id, phase0_turn_id)) =
            open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event))
        else {
            return Ok(());
        };

        // Resolve browser-facing metadata before registering any review gate so
        // a missing/corrupt intents/{intent_id}.json fences without opening a
        // blind confirm/modify/cancel interaction.
        let snapshot = self.session.snapshot().await;
        let pending = snapshot.interactions.iter().find(|interaction| {
            interaction.id == interaction_id
                && interaction.kind == CodeUiInteractionKind::IntentReviewChoice
                && interaction.status == CodeUiInteractionStatus::Pending
        });
        let projection_has_intent_spec = pending
            .map(|interaction| interaction_metadata_has_intent_spec(&interaction.metadata))
            .unwrap_or(false);
        let restored_metadata = if pending.is_none() || !projection_has_intent_spec {
            Some(restored_intent_review_metadata(
                persistence,
                &intent_id,
                &phase0_turn_id,
            )?)
        } else {
            None
        };

        let mut review_turn_id = stored_turn_id;
        if review_turn_id.is_empty() {
            review_turn_id = format!("intent-review-restore-{}", uuid::Uuid::new_v4());
            if let Err(error) =
                store.append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                    interaction_id: interaction_id.clone(),
                    intent_id: intent_id.clone(),
                    turn_id: review_turn_id.clone(),
                    phase0_turn_id: phase0_turn_id.clone(),
                })
            {
                return Err(anyhow!(
                    "An unresolved IntentSpec review could not durably bind its replacement gate turn before restore ({error}). Mutation reconciliation is required before another turn can run."
                ));
            }
        }
        if let Err(error) = self
            .runtime
            .track_external_turn(
                TurnRequest::new(
                    self.runtime_session_id.clone(),
                    review_turn_id.clone(),
                    "IntentSpec review",
                    false,
                ),
                CancellationToken::new(),
                Arc::new(AtomicBool::new(false)),
            )
            .await
        {
            let retry_with_fresh_turn = matches!(
                &error,
                RuntimeWorkerError::IdempotentCommand { ack_ok: true, .. }
            );
            if !retry_with_fresh_turn {
                return Err(anyhow!(
                    "An unresolved IntentSpec review ({interaction_id}) could not be restored ({error}). Mutation reconciliation is required before another turn can run."
                ));
            }
            review_turn_id = format!("intent-review-restore-{}", uuid::Uuid::new_v4());
            if let Err(error) =
                store.append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                    interaction_id: interaction_id.clone(),
                    // Preserve the durable IntentSpec id so a later needs_projection
                    // rebuild can reload intents/{intent_id}.json instead of opening
                    // a blind confirm/modify/cancel gate.
                    intent_id: intent_id.clone(),
                    turn_id: review_turn_id.clone(),
                    phase0_turn_id: phase0_turn_id.clone(),
                })
            {
                return Err(anyhow!(
                    "An unresolved IntentSpec review could not record a replacement gate turn ({error}). Mutation reconciliation is required before another turn can run."
                ));
            }
            if let Err(retry_error) = self
                .runtime
                .track_external_turn(
                    TurnRequest::new(
                        self.runtime_session_id.clone(),
                        review_turn_id.clone(),
                        "IntentSpec review",
                        false,
                    ),
                    CancellationToken::new(),
                    Arc::new(AtomicBool::new(false)),
                )
                .await
            {
                return Err(anyhow!(
                    "An unresolved IntentSpec review could not be restored ({retry_error}). Mutation reconciliation is required before another turn can run."
                ));
            }
        }

        if let Err(error) = self
            .runtime
            .register_interaction_with_delivery(
                self.runtime_session_id.clone(),
                review_turn_id.clone(),
                InteractionState::AwaitingIntentReview {
                    interaction_id: interaction_id.clone(),
                },
                Box::new(HeadlessInteractionDelivery::IntentReview {
                    expected_interaction_id: interaction_id.clone(),
                    persistence: self.persistence.clone(),
                }),
            )
            .await
        {
            let _ = self
                .runtime
                .finish_external_turn(
                    self.runtime_session_id.clone(),
                    review_turn_id,
                    Ok(RuntimeTurnExecution::CompletedDiscardQueued {
                        summary: "restored IntentSpec review gate registration failed".to_string(),
                    }),
                )
                .await;
            return Err(anyhow!(
                "An unresolved IntentSpec review could not be re-registered ({error}). Mutation reconciliation is required before another turn can run."
            ));
        }

        if let Some(restored_metadata) = restored_metadata {
            self.session
                .upsert_interaction(intent_review_choice_interaction(
                    interaction_id.clone(),
                    restored_metadata,
                ))
                .await;
        }
        self.session
            .set_status(CodeUiSessionStatus::AwaitingInteraction)
            .await;

        {
            let mut slot = self.in_flight.lock().await;
            *slot = Some(InFlightTurn {
                runtime_turn_id: review_turn_id.clone(),
                input: "IntentSpec review".to_string(),
                assistant_entry_id: format!("restored-intent-review-{interaction_id}"),
                mode: WebTurnMode::PlanPhase0,
                start_gate: Arc::new(tokio::sync::Notify::new()),
                start_open: Arc::new(AtomicBool::new(true)),
                completion: Arc::new(tokio::sync::Notify::new()),
            });
        }
        self.pending_intent_reviews
            .lock()
            .await
            .insert(review_turn_id, interaction_id);
        persistence
            .persist_snapshot(self.session.snapshot().await)
            .await
            .map_err(|error| {
                anyhow!(
                    "restored IntentSpec review projection could not be persisted before resume: {error}"
                )
            })?;
        Ok(())
    }

    /// Rehydrate IntentSpec Modify → next-message revision mode after restart.
    async fn restore_pending_intent_revision_mode(&self) -> anyhow::Result<()> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(());
        };
        if !self.pending_intent_reviews.lock().await.is_empty() {
            return Ok(());
        }
        if self.pending_intent_revision.lock().await.is_some() {
            return Ok(());
        }
        let recovered_consuming = self
            .uncommitted_consuming_intent_revision
            .lock()
            .await
            .take();
        let (pending, restored_from_consuming) = match recovered_consuming {
            Some(pending) => (pending, true),
            None => {
                let Some(pending) = load_pending_intent_revision(persistence)? else {
                    return Ok(());
                };
                (pending, false)
            }
        };
        let replay = persistence
            .goal_event_store()
            .load_intent_revision_workflow_replay_committed()?;
        let original = pending.clone();
        let authenticated = authenticate_active_intent_revision(persistence, &replay, pending)?;
        if exact_intent_revision_consumer(&replay, &authenticated.terminal)?.is_some() {
            return Err(anyhow!(
                "consumed IntentSpec revision sidecar survived startup reconciliation"
            ));
        }
        // A recovered Consuming envelope must retain its original consumer
        // attribution across later restarts. Only migrate a legacy Active
        // body back to disk here.
        if !restored_from_consuming && authenticated.pending != original {
            persist_pending_intent_revision(persistence, &authenticated.pending)?;
        }
        install_pending_intent_revision_mode(
            &self.session,
            &self.pending_intent_revision,
            authenticated.pending,
            true,
        )
        .await;
        set_status_if_recoverable(&self.session, CodeUiSessionStatus::Idle).await;
        Ok(())
    }
}

fn prepare_web_intent_revision_consumption(
    persistence: &HeadlessSessionPersistence,
    pending: PendingIntentRevision,
    request: &TurnRequest,
) -> Result<(PendingIntentRevision, IntentRevisionConsumption), RuntimeWorkerError> {
    let pending = ensure_legacy_intent_revision_digest_before_consumption(persistence, pending)
        .map_err(|error| {
            RuntimeWorkerError::IndeterminateSideEffect(format!(
                "failed to prepare the durable IntentSpec revision consumer: {error}"
            ))
        })?;
    let consumer_intent = super::web_admission::durable_web_turn_intent(
        persistence,
        &request.turn_id,
        &request.input,
    );
    if !request.mutating {
        return Err(RuntimeWorkerError::IndeterminateSideEffect(
            "IntentSpec revision consumer was admitted as a non-mutating runtime turn".to_string(),
        ));
    }
    let claim =
        pending_consumption_binding(&pending, consumer_intent.clone()).map_err(|error| {
            RuntimeWorkerError::IndeterminateSideEffect(format!(
                "failed to validate the durable IntentSpec revision consumer lineage: {error}"
            ))
        })?;
    let consumption = persistence
        .goal_event_store()
        .prepare_intent_revision_consumption(&consumer_intent, &claim)
        .map_err(|error| {
            RuntimeWorkerError::IndeterminateSideEffect(format!(
                "failed to validate the durable IntentSpec revision consumer intent: {error}"
            ))
        })?;
    let consuming = ConsumingIntentRevision {
        schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
        active: pending.clone(),
        consumption: consumption.clone(),
    };
    if let Err(error) = persist_consuming_intent_revision(persistence, &consuming) {
        tracing::error!(
            error = %error,
            "failed to persist the IntentSpec revision consuming handoff"
        );
        return Err(RuntimeWorkerError::IndeterminateSideEffect(
            INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_REASON.to_string(),
        ));
    }
    Ok((pending, consumption))
}

fn commit_web_intent_revision_consumption(
    persistence: &HeadlessSessionPersistence,
    consumption: &IntentRevisionConsumption,
) -> Result<(), RuntimeWorkerError> {
    if let Err(error) = persistence
        .goal_event_store()
        .record_intent_revision_consumption(consumption)
    {
        tracing::error!(
            error = %error,
            "failed to persist the IntentSpec revision consume boundary"
        );
        return Err(RuntimeWorkerError::IndeterminateSideEffect(
            INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_REASON.to_string(),
        ));
    }
    if let Err(error) = clear_pending_intent_revision(persistence) {
        tracing::error!(
            error = %error,
            "failed to clear durable IntentSpec revision mode after its consume receipt"
        );
        return Err(RuntimeWorkerError::IndeterminateSideEffect(
            "IntentSpec revision receipt was committed, but its durable sidecar could not be cleared; manual reconciliation is required"
                .to_string(),
        ));
    }
    Ok(())
}

fn durably_consume_web_intent_revision(
    persistence: &HeadlessSessionPersistence,
    pending: PendingIntentRevision,
    request: &TurnRequest,
) -> Result<PendingIntentRevision, RuntimeWorkerError> {
    let (pending, consumption) =
        prepare_web_intent_revision_consumption(persistence, pending, request)?;
    commit_web_intent_revision_consumption(persistence, &consumption)?;
    Ok(pending)
}

#[async_trait]
impl<M> RuntimeTurnExecutor for HeadlessTurnExecutor<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    async fn execute(
        &self,
        request: TurnRequest,
        context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        let cancellation = context.cancellation();
        let (assistant_entry_id, start_gate, start_open, turn_mode) = {
            let maybe_slot = {
                let slot_lock = self.in_flight.lock();
                tokio::pin!(slot_lock);
                tokio::select! {
                    slot = &mut slot_lock => Some(slot),
                    _ = cancellation.cancelled() => None,
                }
            };
            let slot = match maybe_slot {
                Some(slot) => slot,
                None if self.cancellation_precedes_start(&request.turn_id).await => {
                    // Admission owns the durable correction when shutdown
                    // wins before its start gate opens, but it cannot clear
                    // this slot while holding the admission mutex. Release
                    // asynchronously so the queued worker's cancellation is
                    // observable to the preflight rollback and a follow-up
                    // browser turn can never remain stuck behind it.
                    let in_flight = self.in_flight.clone();
                    let runtime_turn_id = request.turn_id.clone();
                    tokio::spawn(async move {
                        release_web_turn(&in_flight, &runtime_turn_id).await;
                    });
                    return Err(RuntimeWorkerError::Cancelled);
                }
                None => self.in_flight.lock().await,
            };
            let turn = slot
                .as_ref()
                .filter(|turn| turn.runtime_turn_id == request.turn_id)
                .ok_or_else(|| {
                    RuntimeWorkerError::ExecutionFailed(
                        "browser turn admission was released before runtime execution began"
                            .to_string(),
                    )
                })?;
            (
                turn.assistant_entry_id.clone(),
                turn.start_gate.clone(),
                turn.start_open.clone(),
                turn.mode,
            )
        };

        if !wait_for_web_turn_start(&start_gate, &start_open, cancellation.clone()).await {
            let assistant_is_published = self
                .session
                .snapshot()
                .await
                .transcript
                .iter()
                .any(|entry| entry.id == assistant_entry_id);
            if assistant_is_published {
                // This cancellation arrived after live admission but before
                // a tool boundary. A durability-preflight rollback has no
                // live entry and deliberately leaves persistence untouched.
                finalize_assistant_entry(
                    &self.session,
                    &assistant_entry_id,
                    "(turn cancelled before execution started)",
                    "cancelled",
                )
                .await;
                self.set_terminal_status_if_recoverable(CodeUiSessionStatus::Idle)
                    .await;
                if let Some(persistence) = self.persistence.as_ref()
                    && let Err(error) = persistence
                        .persist_snapshot(self.session.snapshot().await)
                        .await
                {
                    mark_persistence_failure(
                        &self.session,
                        "failed to persist headless web turn cancelled before execution",
                        error,
                    )
                    .await;
                }
            }
            release_web_turn(&self.in_flight, &request.turn_id).await;
            return Err(RuntimeWorkerError::Cancelled);
        }

        let mutation_started = context.mutation_started_marker();
        {
            let mut active_turn_mutations = self.active_turn_mutations.lock().await;
            active_turn_mutations.insert(request.turn_id.clone(), mutation_started.clone());
        }

        if turn_mode == WebTurnMode::IntentRevisionCancel {
            let mut pending_revision = self.pending_intent_revision.lock().await;
            let Some(mut pending) = pending_revision.as_ref().cloned() else {
                self.active_turn_mutations
                    .lock()
                    .await
                    .remove(&request.turn_id);
                release_web_turn(&self.in_flight, &request.turn_id).await;
                return Err(RuntimeWorkerError::ExecutionFailed(
                    "`/intent cancel` lost its active IntentSpec revision before execution"
                        .to_string(),
                ));
            };
            if let Some(persistence) = self.persistence.as_ref() {
                pending = match durably_consume_web_intent_revision(persistence, pending, &request)
                {
                    Ok(pending) => pending,
                    Err(error) => {
                        tracing::error!(
                            %error,
                            "failed to durably consume the cancelled IntentSpec revision"
                        );
                        drop(pending_revision);
                        finalize_assistant_entry(
                            &self.session,
                            &assistant_entry_id,
                            "IntentSpec revision cancellation could not be verified; restart and reconcile before retrying.",
                            "error",
                        )
                        .await;
                        self.session
                            .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                            .await;
                        if let Some(persistence) = self.persistence.as_ref()
                            && let Err(persist_error) = persistence
                                .persist_snapshot(self.session.snapshot().await)
                                .await
                        {
                            tracing::error!(
                                error = %persist_error,
                                "failed to persist IntentSpec revision cancellation fence"
                            );
                        }
                        self.active_turn_mutations
                            .lock()
                            .await
                            .remove(&request.turn_id);
                        release_web_turn(&self.in_flight, &request.turn_id).await;
                        return Err(error);
                    }
                };
            }
            if pending_revision.as_ref() != Some(&pending) {
                drop(pending_revision);
                finalize_assistant_entry(
                    &self.session,
                    &assistant_entry_id,
                    "IntentSpec revision authority changed while cancellation was being persisted; restart and reconcile before retrying.",
                    "error",
                )
                .await;
                self.session
                    .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                    .await;
                if let Some(persistence) = self.persistence.as_ref()
                    && let Err(persist_error) = persistence
                        .persist_snapshot(self.session.snapshot().await)
                        .await
                {
                    tracing::error!(
                        error = %persist_error,
                        "failed to persist IntentSpec revision cancellation fence"
                    );
                }
                self.active_turn_mutations
                    .lock()
                    .await
                    .remove(&request.turn_id);
                release_web_turn(&self.in_flight, &request.turn_id).await;
                return Err(RuntimeWorkerError::IndeterminateSideEffect(
                    "IntentSpec revision cancellation lost its in-memory authority after the durable consume boundary; session requires reconciliation"
                        .to_string(),
                ));
            }
            *pending_revision = None;
            drop(pending_revision);
            let message =
                "IntentSpec revision mode cancelled. Explicit direct commands are available again.";
            finalize_assistant_entry(&self.session, &assistant_entry_id, message, "completed")
                .await;
            self.set_terminal_status_if_recoverable(CodeUiSessionStatus::Idle)
                .await;
            if let Some(persistence) = self.persistence.as_ref()
                && let Err(error) = persistence
                    .record_assistant_message(self.session.snapshot().await, message)
                    .await
            {
                mark_persistence_failure(
                    &self.session,
                    "failed to persist IntentSpec revision cancellation acknowledgement",
                    error,
                )
                .await;
                self.active_turn_mutations
                    .lock()
                    .await
                    .remove(&request.turn_id);
                release_web_turn(&self.in_flight, &request.turn_id).await;
                return Err(RuntimeWorkerError::IndeterminateSideEffect(
                    "IntentSpec revision was cancelled, but its acknowledgement could not be persisted; session requires reconciliation"
                        .to_string(),
                ));
            }
            self.active_turn_mutations
                .lock()
                .await
                .remove(&request.turn_id);
            release_web_turn(&self.in_flight, &request.turn_id).await;
            return Ok(RuntimeTurnExecution::Completed {
                summary: "active IntentSpec revision cancelled".to_string(),
            });
        }

        let intent_draft_json = Arc::new(std::sync::Mutex::new(CapturedIntentDraft::default()));
        let plan_draft_json = Arc::new(std::sync::Mutex::new(None));
        let selected_risk = Arc::new(std::sync::Mutex::new(None));
        let mut observer = HeadlessTurnObserver {
            session: self.session.clone(),
            assistant_entry_id: assistant_entry_id.clone(),
            tool_arguments: Arc::new(std::sync::Mutex::new(HashMap::new())),
            start_tasks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            completion_tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
            stream_delta_pending: Arc::new(std::sync::Mutex::new(String::new())),
            stream_delta_notify: Arc::new(tokio::sync::Notify::new()),
            stream_delta_closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            stream_delta_task: None,
            intent_draft_json: intent_draft_json.clone(),
            plan_draft_json,
            selected_risk: selected_risk.clone(),
        };
        let prior_history = self.history.lock().await.clone();
        let mut config = (self.config_factory)();
        if consumes_intent_revision(turn_mode) {
            // Default browser chat uses the historical Phase 0 allowlist so apply_patch
            // / shell cannot run before IntentSpec / plan confirmation.
            config = phase0_plan_tool_loop_config(config);
            let live_revision = self.pending_intent_revision.lock().await.is_some();
            if live_revision {
                // Live revision consumption (plain follow-up or `/intent modify`)
                // must observe every successful draft in the turn. A terminal
                // `submit_intent_draft` would hide a second draft and incorrectly
                // park a replacement review. Fresh Phase 0 (no live sidecar)
                // keeps the historical terminal so the first review parks once.
                config.terminal_tools = Some(Vec::new());
            }
        }
        if let Some(usage_context) = config.usage_context.as_mut() {
            // The serialized runtime's request id is durable and replay-stable.
            // It is the single turn/event identity shared by browser retries,
            // rather than a UI-local counter.
            usage_context.run_id = Some(request.turn_id.clone());
            usage_context.turn_id = Some(request.turn_id.clone());
            usage_context.event_id = Some(format!("runtime-turn:{}", request.turn_id));
        }
        if let Some(subagent_runtime) = config.subagent_runtime.as_mut() {
            // Child usage stays on the parent's durable turn; the child run is
            // identified separately by its agent_run_id/run_id.
            subagent_runtime.parent_turn_id = Some(request.turn_id.clone());
        }
        config.cancellation = Some(ToolLoopCancellation::new(
            context.cancellation(),
            mutation_started,
        ));
        let mut revision_consumption = None::<PreparedWebIntentRevisionConsumption>;
        let request_input_result: Result<String, RuntimeWorkerError> = async {
            if !consumes_intent_revision(turn_mode) {
                return Ok(request.input.clone());
            }
            let revision_input = if turn_mode == WebTurnMode::IntentRevisionModify {
                intent_revision_modify_note(&request.input).ok_or_else(|| {
                    RuntimeWorkerError::IndeterminateSideEffect(
                        "admitted `/intent modify` command lost its canonical revision note"
                            .to_string(),
                    )
                })?
            } else {
                request.input.as_str()
            };
            let pending_revision = self.pending_intent_revision.lock().await;
            if let Some(pending) = pending_revision.as_ref().cloned() {
                let (pending, consumption) = if let Some(persistence) = self.persistence.as_ref() {
                    let (pending, consumption) =
                        prepare_web_intent_revision_consumption(persistence, pending, &request)?;
                    (pending, Some(consumption))
                } else {
                    (pending, None)
                };
                let prompt = phase0_revision_prompt(
                    &pending.intent_spec,
                    &pending.revision_request(revision_input),
                );
                revision_consumption = Some(PreparedWebIntentRevisionConsumption {
                    pending,
                    consumption,
                });
                Ok(prompt)
            } else if turn_mode == WebTurnMode::IntentRevisionModify {
                Err(RuntimeWorkerError::IndeterminateSideEffect(
                    "admitted `/intent modify` command lost its active IntentSpec revision"
                        .to_string(),
                ))
            } else {
                Ok(phase0_planning_prompt(&request.input))
            }
        }
        .await;
        let request_input = match request_input_result {
            Ok(request_input) => request_input,
            Err(error) => {
                finalize_assistant_entry(
                    &self.session,
                    &assistant_entry_id,
                    "IntentSpec revision could not establish its durable consumption boundary; restart and reconcile before retrying.",
                    "error",
                )
                .await;
                self.session
                    .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                    .await;
                if let Some(persistence) = self.persistence.as_ref()
                    && let Err(persist_error) = persistence
                        .persist_snapshot(self.session.snapshot().await)
                        .await
                {
                    tracing::error!(
                        error = %persist_error,
                        "failed to persist IntentSpec revision pre-receipt reconciliation projection"
                    );
                }
                self.active_turn_mutations
                    .lock()
                    .await
                    .remove(&request.turn_id);
                release_web_turn(&self.in_flight, &request.turn_id).await;
                return Err(error);
            }
        };
        let result = run_tool_loop_with_history_and_observer(
            self.model.as_ref(),
            prior_history,
            request_input,
            self.registry.as_ref(),
            config,
            &mut observer,
        )
        .await;

        // Tool-call projections mutate the same Code UI status as turn
        // finalization. Drain them first so a late "tool completed" task
        // cannot regress the terminal Idle/Error/Cancelled status back to
        // Thinking after this executor has made the result visible.
        observer.flush_projection_tasks().await;

        let reconciliation_required = if self.shutdown_timed_out.load(Ordering::Acquire) {
            Some((
                "runtime_shutdown_timeout",
                "runtime shutdown timed out before the active turn reached a determinate result",
                "headless turn finished after runtime shutdown had already timed out; preserving indeterminate session state",
            ))
        } else if self.interaction_persistence_failed.load(Ordering::Acquire) {
            Some((
                "interaction_persistence_failure",
                "interaction persistence failed before the active turn reached a determinate result",
                "headless turn finished after interaction persistence failed; preserving indeterminate session state",
            ))
        } else {
            None
        };
        let durable_revision_recovery = revision_consumption
            .as_ref()
            .and_then(|revision| revision.consumption.as_ref())
            .is_some();
        if revision_consumption.is_some() && (reconciliation_required.is_some() || result.is_err())
        {
            finalize_assistant_entry(
                &self.session,
                &assistant_entry_id,
                if durable_revision_recovery {
                    "The IntentSpec revision did not reach a durable replacement review. Restart this session to restore the prior revision before retrying."
                } else {
                    "The IntentSpec revision did not produce a replacement review. The prior in-memory revision remains available to retry."
                },
                "error",
            )
            .await;
            self.session
                .set_status(if durable_revision_recovery {
                    CodeUiSessionStatus::IndeterminateSideEffect
                } else {
                    CodeUiSessionStatus::Error
                })
                .await;
            if let Some(persistence) = self.persistence.as_ref()
                && let Err(error) = persistence
                    .persist_snapshot(self.session.snapshot().await)
                    .await
            {
                tracing::error!(
                    %error,
                    "failed to persist the recoverable pre-receipt IntentSpec revision failure"
                );
            }
            self.active_turn_mutations
                .lock()
                .await
                .remove(&request.turn_id);
            release_web_turn(&self.in_flight, &request.turn_id).await;
            return Err(if durable_revision_recovery {
                RuntimeWorkerError::IndeterminateSideEffect(
                    INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_REASON.to_string(),
                )
            } else {
                RuntimeWorkerError::ExecutionFailed(
                    "IntentSpec revision provider failed before producing a replacement review"
                        .to_string(),
                )
            });
        }
        let terminal = if let Some((effect, reason, log_message)) = reconciliation_required {
            tracing::error!("{log_message}");
            self.session
                .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                .await;
            // With worker durability configured, the worker is the sole
            // terminal persistence owner for this command.
            Err(RuntimeWorkerError::IndeterminateSideEffect(format!(
                "{effect}: {reason}; session requires reconciliation"
            )))
        } else {
            match result {
                Ok(turn) => {
                    let captured_intent_draft = if consumes_intent_revision(turn_mode) {
                        Some(match intent_draft_json.lock() {
                            Ok(mut slot) => std::mem::take(&mut *slot),
                            Err(_) => CapturedIntentDraft::default(),
                        })
                    } else {
                        None
                    };
                    let contract_failure = captured_intent_draft.as_ref().and_then(|captured| {
                        (captured.successful_calls != 1 || captured.value.is_none()).then(|| {
                            if captured.successful_calls == 0 {
                                "Phase 0 provider completed without the required submit_intent_draft tool call"
                                    .to_string()
                            } else {
                                format!(
                                    "Phase 0 provider submitted {} successful IntentDrafts; exactly one is required",
                                    captured.successful_calls
                                )
                            }
                        })
                    });
                    if let Some(message) = contract_failure {
                        let preserves_revision = revision_consumption.is_some();
                        let user_message = if durable_revision_recovery {
                            "The provider did not produce exactly one revised IntentSpec. The prior revision remains recoverable; restart this session before retrying."
                        } else if preserves_revision {
                            "The provider did not produce exactly one revised IntentSpec. The prior in-memory revision remains available to retry."
                        } else {
                            "The provider did not produce exactly one IntentSpec draft. No review was opened."
                        };
                        finalize_assistant_entry(
                            &self.session,
                            &assistant_entry_id,
                            user_message,
                            "error",
                        )
                        .await;
                        self.session
                            .set_status(if durable_revision_recovery {
                                CodeUiSessionStatus::IndeterminateSideEffect
                            } else {
                                CodeUiSessionStatus::Error
                            })
                            .await;
                        if let Some(persistence) = self.persistence.as_ref()
                            && let Err(error) = persistence
                                .persist_snapshot(self.session.snapshot().await)
                                .await
                        {
                            tracing::error!(
                                %error,
                                "failed to persist the Phase 0 provider-contract failure projection"
                            );
                        }
                        self.active_turn_mutations
                            .lock()
                            .await
                            .remove(&request.turn_id);
                        release_web_turn(&self.in_flight, &request.turn_id).await;
                        return Err(if durable_revision_recovery {
                            RuntimeWorkerError::IndeterminateSideEffect(
                                INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_REASON.to_string(),
                            )
                        } else {
                            RuntimeWorkerError::ExecutionFailed(message)
                        });
                    }
                    {
                        let mut history = self.history.lock().await;
                        *history = turn.history;
                    }
                    let parked_intent_review =
                        captured_intent_draft.and_then(|captured| captured.value);
                    if let Some(draft_json) = parked_intent_review {
                        let selected_risk = selected_risk.lock().ok().and_then(|slot| slot.clone());
                        match self
                            .park_plan_phase0_intent_review(
                                &request.turn_id,
                                &assistant_entry_id,
                                &turn.final_text,
                                &draft_json,
                                selected_risk,
                                revision_consumption.as_ref(),
                            )
                            .await
                        {
                            Ok(waiting) => {
                                self.active_turn_mutations
                                    .lock()
                                    .await
                                    .remove(&request.turn_id);
                                // Keep `in_flight` until the review settles via
                                // `respond` so cancel/submit fencing stays live.
                                return Ok(waiting);
                            }
                            Err(error) => {
                                let error = if durable_revision_recovery
                                    && !matches!(
                                        &error,
                                        RuntimeWorkerError::IndeterminateSideEffect(reason)
                                            if reason == crate::internal::ai::session::jsonl::INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON
                                    ) {
                                    RuntimeWorkerError::IndeterminateSideEffect(
                                        INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_REASON
                                            .to_string(),
                                    )
                                } else {
                                    error
                                };
                                if durable_revision_recovery {
                                    finalize_assistant_entry(
                                        &self.session,
                                        &assistant_entry_id,
                                        "The revised IntentSpec could not be durably handed off. Restart this session to reconcile the revision before retrying.",
                                        "error",
                                    )
                                    .await;
                                    self.session
                                        .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                                        .await;
                                    if let Some(persistence) = self.persistence.as_ref()
                                        && let Err(persist_error) = persistence
                                            .persist_snapshot(self.session.snapshot().await)
                                            .await
                                    {
                                        tracing::error!(
                                            error = %persist_error,
                                            "failed to persist the IntentSpec replacement-review handoff fence"
                                        );
                                    }
                                } else if revision_consumption.is_some() {
                                    finalize_assistant_entry(
                                        &self.session,
                                        &assistant_entry_id,
                                        "The revised IntentSpec could not be prepared. The prior in-memory revision remains available to retry.",
                                        "error",
                                    )
                                    .await;
                                    self.session.set_status(CodeUiSessionStatus::Error).await;
                                }
                                self.active_turn_mutations
                                    .lock()
                                    .await
                                    .remove(&request.turn_id);
                                release_web_turn(&self.in_flight, &request.turn_id).await;
                                return Err(error);
                            }
                        }
                    }
                    finalize_assistant_entry(
                        &self.session,
                        &assistant_entry_id,
                        &turn.final_text,
                        "completed",
                    )
                    .await;
                    self.set_terminal_status_if_recoverable(CodeUiSessionStatus::Idle)
                        .await;
                    if let Some(persistence) = self.persistence.as_ref()
                        && let Err(error) = persistence
                            .record_assistant_message(
                                self.session.snapshot().await,
                                turn.final_text.as_str(),
                            )
                            .await
                    {
                        mark_persistence_failure(
                            &self.session,
                            "failed to persist headless web assistant message",
                            error,
                        )
                        .await;
                        self.active_turn_mutations
                            .lock()
                            .await
                            .remove(&request.turn_id);
                        release_web_turn(&self.in_flight, &request.turn_id).await;
                        return Err(RuntimeWorkerError::IndeterminateSideEffect(
                            "failed to persist headless web assistant message after a successful mutating turn; session requires reconciliation"
                                .to_string(),
                        ));
                    }
                    Ok(RuntimeTurnExecution::Completed {
                        summary: match turn_mode {
                            WebTurnMode::PlanPhase0 => {
                                "web plan phase-0 turn completed".to_string()
                            }
                            WebTurnMode::IntentRevisionModify => {
                                "web IntentSpec revision command completed".to_string()
                            }
                            WebTurnMode::IntentRevisionCancel => {
                                "web IntentSpec revision cancellation completed".to_string()
                            }
                            WebTurnMode::ExplicitDirect => {
                                "web explicit direct turn completed".to_string()
                            }
                        },
                    })
                }
                Err(_error) if cancellation.is_cancelled() => {
                    finalize_assistant_entry(
                        &self.session,
                        &assistant_entry_id,
                        "(turn cancelled by user)",
                        "cancelled",
                    )
                    .await;
                    self.set_terminal_status_if_recoverable(CodeUiSessionStatus::Idle)
                        .await;
                    if let Some(persistence) = self.persistence.as_ref()
                        && let Err(error) = persistence
                            .persist_snapshot(self.session.snapshot().await)
                            .await
                    {
                        mark_persistence_failure(
                            &self.session,
                            "failed to persist cancelled headless web turn",
                            error,
                        )
                        .await;
                    }
                    Err(RuntimeWorkerError::Cancelled)
                }
                Err(error) => {
                    let message = format_completion_error(&error);
                    finalize_assistant_entry(&self.session, &assistant_entry_id, &message, "error")
                        .await;
                    self.set_terminal_status_if_recoverable(CodeUiSessionStatus::Error)
                        .await;
                    if let Some(persistence) = self.persistence.as_ref()
                        && let Err(error) = persistence
                            .persist_snapshot(self.session.snapshot().await)
                            .await
                    {
                        mark_persistence_failure(
                            &self.session,
                            "failed to persist headless web failed turn snapshot",
                            error,
                        )
                        .await;
                    }
                    Err(RuntimeWorkerError::ExecutionFailed(message))
                }
            }
        };

        self.active_turn_mutations
            .lock()
            .await
            .remove(&request.turn_id);
        release_web_turn(&self.in_flight, &request.turn_id).await;
        terminal
    }

    async fn respond(
        &self,
        request: TurnRequest,
        interaction: InteractionResponse,
        _context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        if self
            .pending_intent_reviews
            .lock()
            .await
            .contains_key(&request.turn_id)
        {
            return self
                .settle_plan_phase0_intent_review(&request, interaction)
                .await;
        }
        Err(RuntimeWorkerError::ExecutorDoesNotSupportResponses)
    }

    async fn interaction_resolution(
        &self,
        request: &TurnRequest,
        interaction: &InteractionResponse,
    ) -> Option<String> {
        let expected = self
            .pending_intent_reviews
            .lock()
            .await
            .get(&request.turn_id)
            .cloned();
        if expected.as_deref() != Some(interaction.interaction_id.as_str()) {
            return None;
        }
        intent_review_decision_from_response(interaction)
            .ok()
            .map(|decision| decision.wire_id().to_string())
    }

    async fn intent_revision_recovery(
        &self,
        request: &TurnRequest,
        interaction: &InteractionResponse,
    ) -> Option<IntentRevisionRecovery> {
        let expected = self
            .pending_intent_reviews
            .lock()
            .await
            .get(&request.turn_id)
            .cloned()?;
        intent_revision_recovery_for_response(&expected, interaction)
    }

    fn validate_interaction_response(
        &self,
        interaction: &InteractionResponse,
    ) -> Result<(), RuntimeWorkerError> {
        let decision = intent_review_decision_from_response(interaction).map_err(|_| {
            RuntimeWorkerError::InvalidInteractionResponse(
                "expected one of confirm/modify/cancel".to_string(),
            )
        })?;
        if decision == IntentReviewDecision::Revise {
            let response = decode_headless_interaction_response(interaction)?;
            canonical_intent_revision_note(&response)?;
            if self.persistence.is_some()
                && !interaction.intent_revision_sidecar_digest().is_some_and(
                    crate::internal::ai::session::jsonl::is_canonical_intent_revision_digest,
                )
            {
                return Err(RuntimeWorkerError::InvalidInteractionResponse(
                    "IntentSpec Modify is missing its durable prepared-sidecar binding".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn preserve_pending_interaction_on_shutdown(
        &self,
        request: &TurnRequest,
        interaction: &InteractionState,
    ) -> bool {
        // `HeadlessTurnExecutor::execute` returns AwaitingInteraction only for
        // the durable Phase 0 Intent review. Tool approvals and user-input
        // continuations register a RuntimeInteractionDelivery separately.
        let InteractionState::AwaitingIntentReview { interaction_id } = interaction else {
            return false;
        };
        let Some(persistence) = self.persistence.as_ref() else {
            return false;
        };
        let Ok(replay) = persistence
            .goal_event_store()
            .load_code_workflow_replay_committed()
        else {
            return false;
        };
        matches!(
            open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event)),
            Some((open_interaction_id, _, _, phase0_turn_id))
                if open_interaction_id == *interaction_id
                    && phase0_turn_id == request.turn_id
        )
    }
}

/// Dispatches chat/Phase-0/1 turns to [`HeadlessTurnExecutor`] and confirmed
/// plan-execution turns to [`DeferredPlanExecutionExecutor`] so the worker
/// remains the single serialized owner (W2-04).
struct WebRuntimeTurnExecutor<M: CompletionModel + 'static> {
    chat: Arc<HeadlessTurnExecutor<M>>,
    plan: Arc<DeferredPlanExecutionExecutor>,
}

#[async_trait]
impl<M> RuntimeTurnExecutor for WebRuntimeTurnExecutor<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    async fn execute(
        &self,
        request: TurnRequest,
        context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        if is_plan_execution_turn(&request) {
            self.plan.execute(request, context).await
        } else {
            self.chat.execute(request, context).await
        }
    }

    async fn respond(
        &self,
        request: TurnRequest,
        interaction: InteractionResponse,
        context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        self.chat.respond(request, interaction, context).await
    }

    async fn interaction_resolution(
        &self,
        request: &TurnRequest,
        interaction: &InteractionResponse,
    ) -> Option<String> {
        self.chat.interaction_resolution(request, interaction).await
    }

    async fn intent_revision_recovery(
        &self,
        request: &TurnRequest,
        interaction: &InteractionResponse,
    ) -> Option<crate::internal::ai::session::IntentRevisionRecovery> {
        self.chat
            .intent_revision_recovery(request, interaction)
            .await
    }

    fn validate_interaction_response(
        &self,
        interaction: &InteractionResponse,
    ) -> Result<(), RuntimeWorkerError> {
        self.chat.validate_interaction_response(interaction)
    }

    fn preserve_pending_interaction_on_shutdown(
        &self,
        request: &TurnRequest,
        interaction: &InteractionState,
    ) -> bool {
        self.chat
            .preserve_pending_interaction_on_shutdown(request, interaction)
    }

    fn on_admission_discarded(&self, request: &TurnRequest) {
        self.plan.on_admission_discarded(request);
        self.chat.on_admission_discarded(request);
    }
}

impl<M> HeadlessTurnExecutor<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    async fn generate_web_phase1(
        &self,
        confirmed: &ConfirmedIntentForPhase1,
        phase1_turn_id: &str,
        review_turn_id: &str,
        plan_interaction_id: &str,
        control: Phase1GenerationControl,
    ) -> Result<Phase1ReviewContext, RuntimeWorkerError> {
        let Phase1GenerationControl {
            mutation_started,
            attempt_state,
            cancellation,
        } = control;
        let spec: crate::internal::ai::intentspec::IntentSpec =
            serde_json::from_str(&confirmed.intent_spec_json).map_err(|error| {
                RuntimeWorkerError::ExecutionFailed(format!(
                    "confirmed IntentSpec cannot start Phase 1: {error}"
                ))
            })?;
        if spec.metadata.id != confirmed.intent_spec_id {
            return Err(RuntimeWorkerError::ExecutionFailed(
                "confirmed IntentSpec domain id does not match its durable review metadata"
                    .to_string(),
            ));
        }
        let checkout = confirmed.checkout.as_ref().ok_or_else(|| {
            RuntimeWorkerError::DurabilityFailure(
                "Phase 1 start is missing its confirmed checkout binding".to_string(),
            )
        })?;
        checkout
            .validate(self.registry.working_dir(), &spec)
            .await
            .map_err(|error| {
                RuntimeWorkerError::DurabilityFailure(format!(
                    "Phase 1 checkout no longer matches the confirmed review seed: {error}"
                ))
            })?;

        let assistant_entry_id = format!("assistant-phase1-{}", uuid::Uuid::new_v4());
        self.session
            .upsert_transcript_entry(CodeUiTranscriptEntry {
                id: assistant_entry_id.clone(),
                kind: CodeUiTranscriptEntryKind::AssistantMessage,
                title: Some("Execution plan".to_string()),
                content: Some(String::new()),
                status: Some("streaming".to_string()),
                streaming: true,
                metadata: serde_json::json!({ "phase": "plan" }),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .await;
        self.session.set_status(CodeUiSessionStatus::Thinking).await;

        let plan_draft_json = Arc::new(std::sync::Mutex::new(None));
        let mut observer = HeadlessTurnObserver {
            session: self.session.clone(),
            assistant_entry_id: assistant_entry_id.clone(),
            tool_arguments: Arc::new(std::sync::Mutex::new(HashMap::new())),
            start_tasks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            completion_tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
            stream_delta_pending: Arc::new(std::sync::Mutex::new(String::new())),
            stream_delta_notify: Arc::new(tokio::sync::Notify::new()),
            stream_delta_closed: Arc::new(AtomicBool::new(false)),
            stream_delta_task: None,
            intent_draft_json: Arc::new(std::sync::Mutex::new(CapturedIntentDraft::default())),
            plan_draft_json: plan_draft_json.clone(),
            selected_risk: Arc::new(std::sync::Mutex::new(None)),
        };
        let mut config = phase1_plan_tool_loop_config((self.config_factory)());
        if let Some(usage_context) = config.usage_context.as_mut() {
            usage_context.run_id = Some(phase1_turn_id.to_string());
            usage_context.turn_id = Some(phase1_turn_id.to_string());
            usage_context.event_id = Some(format!("runtime-turn:{phase1_turn_id}"));
        }
        config.cancellation = Some(ToolLoopCancellation::new(
            cancellation,
            mutation_started.clone(),
        ));
        let planning_prompt = match confirmed.revision_note.as_deref() {
            Some(note) => format!(
                "{}\n\nThe developer rejected the previous plan and supplied this mandatory revision note:\n{}\nPrevious normalized plan (id={}):\n```json\n{}\n```\nGenerate a materially revised plan that preserves unaffected work and addresses the note.",
                phase1_planning_prompt(&confirmed.intent_spec_json),
                note.trim(),
                confirmed.prior_plan_id.as_deref().unwrap_or("unpersisted"),
                serde_json::to_string_pretty(&confirmed.prior_plan).map_err(|error| {
                    RuntimeWorkerError::ExecutionFailed(format!(
                        "previous Phase 1 plan cannot be encoded for revision: {error}"
                    ))
                })?
            ),
            None => phase1_planning_prompt(&confirmed.intent_spec_json),
        };
        let turn_result = run_tool_loop_with_history_and_observer(
            self.model.as_ref(),
            self.history.lock().await.clone(),
            planning_prompt,
            self.registry.as_ref(),
            config,
            &mut observer,
        )
        .await;
        observer.flush_projection_tasks().await;
        let turn = turn_result.map_err(|error| {
            RuntimeWorkerError::ExecutionFailed(format_completion_error(&error))
        })?;
        let final_text = turn.final_text;
        *self.history.lock().await = turn.history;

        let draft_json = plan_draft_json
            .lock()
            .ok()
            .and_then(|mut value| value.take())
            .ok_or_else(|| {
                RuntimeWorkerError::ExecutionFailed(
                    "Phase 1 planner did not call submit_plan_draft".to_string(),
                )
            })?;
        let draft: SubmitPlanDraftArgs = serde_json::from_str(&draft_json).map_err(|error| {
            RuntimeWorkerError::ExecutionFailed(format!(
                "Phase 1 submit_plan_draft payload is invalid: {error}"
            ))
        })?;
        let mut execution_plan = compile_submitted_plan(&spec, &draft).map_err(|error| {
            RuntimeWorkerError::ExecutionFailed(format!(
                "Phase 1 execution plan could not be compiled: {error}"
            ))
        })?;
        if let Some(prior_plan) = confirmed.prior_plan.as_ref() {
            execution_plan.parent_revision = Some(prior_plan.revision);
            execution_plan.revision = prior_plan.revision.checked_add(1).ok_or_else(|| {
                RuntimeWorkerError::ExecutionFailed(
                    "Phase 1 plan revision counter is exhausted".to_string(),
                )
            })?;
            execution_plan.replan_reason = confirmed.revision_note.clone();
            crate::internal::ai::runtime::phase1::preserve_unchanged_revision_steps(
                &mut execution_plan,
                prior_plan,
            );
        }

        let checkout = checkout.clone();
        checkout
            .validate(self.registry.working_dir(), &spec)
            .await
            .map_err(|error| {
                RuntimeWorkerError::DurabilityFailure(format!(
                    "Phase 1 checkout changed before the formal plan write: {error}"
                ))
            })?;
        let preflight_context = Phase1ReviewContext {
            schema_version: Phase1ReviewContext::SCHEMA_VERSION,
            interaction_id: plan_interaction_id.to_string(),
            intent_id: confirmed.intent_id.clone(),
            intent_spec_id: confirmed.intent_spec_id.clone(),
            persisted_plan: Phase1PersistedPlan::Unavailable,
            intent_spec: spec.clone(),
            plan_draft: draft.clone(),
            execution_plan: execution_plan.clone(),
            default_allow_network: matches!(
                spec.constraints.security.network_policy,
                crate::internal::ai::intentspec::types::NetworkPolicy::Allow
            ),
            checkout: checkout.clone(),
        };
        validate_phase1_review_context_preflight(&preflight_context).map_err(|error| {
            RuntimeWorkerError::DurabilityFailure(format!(
                "Phase 1 review context is too large to persist safely: {error}"
            ))
        })?;
        let persistence = self.persistence.as_ref().ok_or_else(|| {
            RuntimeWorkerError::DurabilityFailure(
                "Phase 1 review requires session persistence before plan write".to_string(),
            )
        })?;
        let store = persistence.goal_event_store();
        validate_phase1_context_session_budget(&store, &preflight_context).map_err(|error| {
            RuntimeWorkerError::DurabilityFailure(format!(
                "Phase 1 session context budget check failed before plan write: {error}"
            ))
        })?;

        // The external runtime turn was durably admitted before this point.
        // Mark the exact boundary immediately before the formal plan write.
        attempt_state
            .compare_exchange(
                PHASE1_ATTEMPT_ADMITTING,
                PHASE1_ATTEMPT_MUTATING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|state| {
                RuntimeWorkerError::ExecutionFailed(if state == PHASE1_ATTEMPT_CANCELLED {
                    "Phase 1 planning was cancelled before the formal plan write".to_string()
                } else {
                    "Phase 1 attempt state changed before the formal plan write".to_string()
                })
            })?;
        mutation_started.store(true, Ordering::Release);
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::Phase1FormalWriteStarted {
                phase1_turn_id: phase1_turn_id.to_string(),
                source_interaction_id: confirmed.source_interaction_id.clone(),
                seed_digest: confirmed.seed_digest.clone(),
            })
            .map_err(|error| {
                RuntimeWorkerError::IndeterminateSideEffect(format!(
                    "Phase 1 formal-write boundary could not be persisted; session requires reconciliation: {error}"
                ))
            })?;
        #[cfg(feature = "test-provider")]
        persistence.wait_after_phase1_formal_write_started().await;
        let (parent_execution_plan_id, parent_test_plan_id) = match &confirmed.prior_persisted_plan
        {
            Phase1PersistedPlan::Persisted {
                execution_plan_id,
                test_plan_id,
            } => (
                Some(execution_plan_id.as_str()),
                Some(test_plan_id.as_str()),
            ),
            Phase1PersistedPlan::Unavailable => (None, None),
        };
        let outcome = if let Some(mcp_server) = self.mcp_server.as_ref() {
            Some(
                crate::internal::ai::runtime::phase1::write_plan_set(
                    mcp_server,
                    &confirmed.intent_id,
                    parent_execution_plan_id,
                    parent_test_plan_id,
                    &execution_plan,
                )
                .await
                .map_err(|error| {
                    RuntimeWorkerError::IndeterminateSideEffect(format!(
                        "Phase 1 formal plan write failed; session requires reconciliation: {error}"
                    ))
                })?,
            )
        } else {
            None
        };
        let default_allow_network = matches!(
            spec.constraints.security.network_policy,
            crate::internal::ai::intentspec::types::NetworkPolicy::Allow
        );
        checkout
            .validate_identity(self.registry.working_dir(), &spec)
            .await
            .map_err(|error| {
                RuntimeWorkerError::IndeterminateSideEffect(format!(
                    "Phase 1 checkout identity changed after the formal plan write; session requires reconciliation: {error}"
                ))
            })?;
        let context = Phase1ReviewContext {
            schema_version: Phase1ReviewContext::SCHEMA_VERSION,
            interaction_id: plan_interaction_id.to_string(),
            intent_id: confirmed.intent_id.clone(),
            intent_spec_id: confirmed.intent_spec_id.clone(),
            persisted_plan: outcome.map_or(Phase1PersistedPlan::Unavailable, |outcome| {
                Phase1PersistedPlan::Persisted {
                    execution_plan_id: outcome.execution_plan_id,
                    test_plan_id: outcome.test_plan_id,
                }
            }),
            intent_spec: spec,
            plan_draft: draft,
            execution_plan,
            default_allow_network,
            checkout,
        };
        persist_phase1_review_context(&store, &context).map_err(|error| {
            RuntimeWorkerError::IndeterminateSideEffect(format!(
                "Phase 1 context could not be persisted; session requires reconciliation: {error}"
            ))
        })?;
        let review_event = CodeWorkflowEventKind::PlanReviewRequested {
            interaction_id: plan_interaction_id.to_string(),
            plan_id: context.plan_id().unwrap_or_default().to_string(),
            turn_id: review_turn_id.to_string(),
            phase1_turn_id: phase1_turn_id.to_string(),
            context_id: plan_interaction_id.to_string(),
            revision_of: confirmed.revision_source_interaction_id.clone(),
            prepared_from_network: None,
        };
        store
            .append_code_workflow_durable(review_event)
            .map_err(|error| {
                RuntimeWorkerError::IndeterminateSideEffect(format!(
                    "Phase 1 review marker could not be persisted; session requires reconciliation: {error}"
                ))
            })?;
        if let Some(source_interaction_id) = confirmed.revision_source_interaction_id.as_ref()
            && let Ok(replay) = store.load_code_workflow_replay()
            && let Some(source_context_id) =
                crate::internal::ai::runtime::phase1::phase1_context_id_for_interaction(
                    replay.events.iter().map(|event| &event.event),
                    source_interaction_id,
                )
            && source_context_id != plan_interaction_id
            && let Err(error) = clear_phase1_review_context(&store, &source_context_id)
        {
            tracing::warn!(
                error = %error,
                context_id = %source_context_id,
                "failed to garbage-collect superseded Phase 1 review context"
            );
        }

        finalize_assistant_entry(
            &self.session,
            &assistant_entry_id,
            if final_text.trim().is_empty() {
                "Execution plan ready for review"
            } else {
                &final_text
            },
            "completed",
        )
        .await;
        Ok(context)
    }

    async fn park_plan_phase0_intent_review(
        &self,
        runtime_turn_id: &str,
        assistant_entry_id: &str,
        final_text: &str,
        draft_json: &str,
        selected_risk: Option<crate::internal::ai::intentspec::RiskLevel>,
        revision_consumption: Option<&PreparedWebIntentRevisionConsumption>,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        let interaction_id = format!("intent-review-{}", uuid::Uuid::new_v4());
        let review_turn_id = format!("intent-review-gate-{}", uuid::Uuid::new_v4());
        let (intent_id, spec_json, spec) =
            resolve_web_phase0_intent_draft(draft_json, self.registry.working_dir(), selected_risk)
                .map_err(|error| {
                    RuntimeWorkerError::ExecutionFailed(format!(
                        "IntentSpec draft could not be resolved before review: {error}"
                    ))
                })?;

        // Formal Phase 0 write before the review gate opens. Prefer MCP
        // `write_intent` when a server is available; otherwise persist the
        // resolved IntentSpec under the session root so resume/confirm can
        // reload a durable artifact (not only an in-memory UUID).
        let intent_id = persist_web_phase0_intent_before_review(
            self.persistence.as_ref(),
            self.mcp_server.as_ref(),
            &spec,
            intent_id,
        )
        .await
        .map_err(|error| {
            RuntimeWorkerError::IndeterminateSideEffect(format!(
                "IntentSpec draft completed but could not be persisted before review; session requires reconciliation: {error}"
            ))
        })?;

        if let Some(persistence) = self.persistence.as_ref() {
            let store = persistence.goal_event_store();
            if let Err(error) =
                store.append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                    interaction_id: interaction_id.clone(),
                    intent_id: intent_id.clone(),
                    turn_id: review_turn_id,
                    phase0_turn_id: runtime_turn_id.to_string(),
                })
            {
                mark_persistence_failure(
                    &self.session,
                    "failed to persist IntentSpec review request marker",
                    error,
                )
                .await;
                return Err(RuntimeWorkerError::IndeterminateSideEffect(
                    if revision_consumption.is_some() {
                        crate::internal::ai::session::jsonl::INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON
                            .to_string()
                    } else {
                        "IntentSpec draft completed but the review marker could not be persisted; session requires reconciliation"
                            .to_string()
                    },
                ));
            }
        }

        // For a revision consumer, the replacement IntentSpec and its open
        // review marker are the durable proof that provider work completed.
        // Only now commit the old revision receipt and unlink its Consuming
        // sidecar. Startup can finish either half of this ordered handoff
        // without replaying the provider.
        if let Some(revision_consumption) = revision_consumption {
            if let (Some(persistence), Some(consumption)) = (
                self.persistence.as_ref(),
                revision_consumption.consumption.as_ref(),
            ) && let Err(error) =
                commit_web_intent_revision_consumption(persistence, consumption)
            {
                tracing::error!(
                    %error,
                    "replacement IntentSpec review is durable but its source revision receipt needs startup recovery"
                );
                return Err(RuntimeWorkerError::IndeterminateSideEffect(
                    crate::internal::ai::session::jsonl::INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON
                        .to_string(),
                ));
            }
            let mut pending_revision = self.pending_intent_revision.lock().await;
            if pending_revision.as_ref() != Some(&revision_consumption.pending) {
                return Err(RuntimeWorkerError::IndeterminateSideEffect(
                    crate::internal::ai::session::jsonl::INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON
                        .to_string(),
                ));
            }
            *pending_revision = None;
        }

        if let Some(persistence) = self.persistence.as_ref()
            && let Err(error) = persistence
                .record_assistant_message(self.session.snapshot().await, final_text)
                .await
        {
            mark_persistence_failure(
                &self.session,
                "failed to persist IntentSpec draft before review gate",
                error,
            )
            .await;
            return Err(RuntimeWorkerError::IndeterminateSideEffect(
                if revision_consumption.is_some() {
                    crate::internal::ai::session::jsonl::INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON
                        .to_string()
                } else {
                    "failed to persist IntentSpec draft before review gate; session requires reconciliation"
                        .to_string()
                },
            ));
        }

        finalize_assistant_entry(
            &self.session,
            assistant_entry_id,
            if final_text.trim().is_empty() {
                "IntentSpec draft ready for review"
            } else {
                final_text
            },
            "completed",
        )
        .await;

        let interaction = intent_review_choice_interaction(
            interaction_id.clone(),
            serde_json::json!({
                "draft": draft_json,
                "intentId": intent_id,
                "intentSpec": spec_json,
            }),
        );
        self.session.upsert_interaction(interaction).await;
        self.session
            .set_status(CodeUiSessionStatus::AwaitingInteraction)
            .await;
        #[cfg(feature = "test-provider")]
        if let Some(persistence) = self.persistence.as_ref() {
            persistence.wait_before_interaction_registration().await;
        }
        if let Err(error) =
            persist_headless_interaction_snapshot(self.persistence.as_ref(), &self.session).await
        {
            mark_persistence_failure(
                &self.session,
                "failed to persist pending IntentSpec review interaction",
                error,
            )
            .await;
            return Err(RuntimeWorkerError::IndeterminateSideEffect(
                if revision_consumption.is_some() {
                    crate::internal::ai::session::jsonl::INTENT_REVISION_REPLACEMENT_REVIEW_INDETERMINATE_REASON
                        .to_string()
                } else {
                    "failed to persist pending IntentSpec review interaction; session requires reconciliation"
                        .to_string()
                },
            ));
        }

        // Keep the gate on the live Phase 0 turn via `AwaitingInteraction` +
        // `executor.respond`. Do not `register_interaction_with_delivery` from
        // inside `execute` — that would deadlock the single-threaded worker
        // actor waiting on this future. The executor supplies a canonical
        // resolution label so the worker commits the terminal command and
        // `InteractionResolved` under one append lock/fsync.
        self.pending_intent_reviews
            .lock()
            .await
            .insert(runtime_turn_id.to_string(), interaction_id.clone());

        Ok(RuntimeTurnExecution::AwaitingInteraction(
            InteractionState::AwaitingIntentReview { interaction_id },
        ))
    }

    async fn settle_plan_phase0_intent_review(
        &self,
        request: &TurnRequest,
        interaction: InteractionResponse,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        let expected_id = self
            .pending_intent_reviews
            .lock()
            .await
            .get(&request.turn_id)
            .cloned();
        let Some(expected_id) = expected_id else {
            return Err(RuntimeWorkerError::ExecutionFailed(
                "no pending IntentSpec review is registered for this web turn".to_string(),
            ));
        };
        if interaction.interaction_id != expected_id {
            return Err(RuntimeWorkerError::ExecutionFailed(format!(
                "IntentSpec review response targeted '{}' but pending gate is '{expected_id}'",
                interaction.interaction_id
            )));
        }

        let decision = intent_review_decision_from_response(&interaction)?;

        match decision {
            IntentReviewDecision::Confirm => Ok(RuntimeTurnExecution::CompletedHoldQueued {
                summary: "IntentSpec confirmed; Phase 1 planning queued".to_string(),
            }),
            IntentReviewDecision::Revise => Ok(RuntimeTurnExecution::CompletedDiscardQueued {
                summary:
                    "IntentSpec revision mode armed; send a plain message with requested changes"
                        .to_string(),
            }),
            IntentReviewDecision::Cancel => Ok(RuntimeTurnExecution::CompletedDiscardQueued {
                summary: "IntentSpec review cancelled".to_string(),
            }),
        }
    }
}

pub(crate) async fn enter_web_intent_revision_mode(
    session: &Arc<CodeUiSession>,
    persistence: Option<&HeadlessSessionPersistence>,
    pending_intent_revision: &Arc<Mutex<Option<PendingIntentRevision>>>,
    interaction_id: &str,
    runtime_turn_id: &str,
    note: Option<String>,
) -> Result<(), RuntimeWorkerError> {
    let note = canonical_intent_revision_note_value(note.as_deref())?;
    let pending = if let Some(persistence) = persistence {
        promote_prepared_intent_revision(
            persistence,
            interaction_id,
            runtime_turn_id,
            Some(note.as_deref()),
        )
        .map_err(|error| {
            RuntimeWorkerError::IndeterminateSideEffect(format!(
                "IntentSpec revision mode could not promote its durable lineage-bound sidecar; session requires reconciliation: {error}"
            ))
        })?
    } else {
        let snapshot = session.snapshot().await;
        let spec_json = snapshot
            .interactions
            .iter()
            .find(|interaction| interaction.id == interaction_id)
            .and_then(|interaction| {
                interaction
                    .metadata
                    .get("intentSpec")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
                    .or_else(|| {
                        interaction
                            .metadata
                            .get("intentSpec")
                            .filter(|value| value.is_object())
                            .and_then(|value| serde_json::to_string_pretty(value).ok())
                    })
            })
            .ok_or_else(|| {
                RuntimeWorkerError::ExecutionFailed(
                    "Modify was selected but the pending IntentSpec payload is missing from the review gate; cannot enter revision mode"
                        .to_string(),
                )
            })?;
        PendingIntentRevision {
            intent_spec: spec_json,
            note: note.clone(),
            authority: None,
        }
    };
    install_pending_intent_revision_mode(session, pending_intent_revision, pending, false).await;
    Ok(())
}

async fn install_pending_intent_revision_mode(
    session: &Arc<CodeUiSession>,
    pending_intent_revision: &Arc<Mutex<Option<PendingIntentRevision>>>,
    pending: PendingIntentRevision,
    restored: bool,
) {
    *pending_intent_revision.lock().await = Some(pending.clone());
    let mut help = phase0_revision_help_message();
    if pending.note.is_some() {
        // The exact note remains in the private revision sidecar and is added
        // to the next provider prompt. Do not echo it into transcript deltas,
        // which are also persisted and broadcast as CodeWorkflow/SSE events.
        help = format!(
            "{help} Your Modify note is retained privately for the next Phase 0 revision prompt."
        );
    } else if restored {
        help = format!(
            "{help} Your next plain-text message will revise the current IntentSpec (restored after resume)."
        );
    } else {
        help = format!("{help} Your next plain-text message will revise the current IntentSpec.");
    }
    let entry_id = if restored {
        format!("intent-revision-help-restore-{}", uuid::Uuid::new_v4())
    } else {
        format!("intent-revision-help-{}", uuid::Uuid::new_v4())
    };
    session
        .upsert_transcript_entry(CodeUiTranscriptEntry {
            id: entry_id,
            kind: CodeUiTranscriptEntryKind::AssistantMessage,
            title: Some("IntentSpec revision".to_string()),
            content: Some(help),
            status: Some("completed".to_string()),
            streaming: false,
            metadata: serde_json::json!({
                "intentRevisionMode": true,
                "restored": restored,
            }),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
        .await;
}

fn pending_intent_revision_path(persistence: &HeadlessSessionPersistence) -> std::path::PathBuf {
    persistence
        .goal_event_store()
        .session_root()
        .join("intents")
        .join(PENDING_INTENT_REVISION_FILE)
}

fn intent_revision_hmac_key_path(persistence: &HeadlessSessionPersistence) -> std::path::PathBuf {
    persistence
        .goal_event_store()
        .session_root()
        .join("intents")
        .join(INTENT_REVISION_HMAC_KEY_FILE)
}

fn open_intent_revision_file_no_follow(
    path: &std::path::Path,
    description: &str,
) -> anyhow::Result<Option<(File, std::fs::Metadata)>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(anyhow!(
                    "failed to open {description} at {} without following symlinks: {error}",
                    path.display()
                ));
            }
        };
        let metadata = file.metadata().map_err(|error| {
            anyhow!(
                "failed to inspect {description} at {}: {error}",
                path.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(anyhow!(
                "{description} at {} is not a regular file",
                path.display()
            ));
        }
        Ok(Some((file, metadata)))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        };

        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(anyhow!(
                    "failed to open {description} at {} without following reparse points: {error}",
                    path.display()
                ));
            }
        };
        let metadata = file.metadata().map_err(|error| {
            anyhow!(
                "failed to inspect opened {description} at {}: {error}",
                path.display()
            )
        })?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_file() {
            return Err(anyhow!(
                "{description} at {} is not a regular non-reparse file",
                path.display()
            ));
        }
        Ok(Some((file, metadata)))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, description);
        Err(anyhow!(
            "secure IntentSpec revision file loading is unsupported on this platform"
        ))
    }
}

fn sync_open_intent_revision_file_and_parent(
    file: &File,
    path: &std::path::Path,
    description: &str,
) -> anyhow::Result<()> {
    file.sync_all().map_err(|error| {
        anyhow!(
            "failed to durably re-sync {description} at {}: {error}",
            path.display()
        )
    })?;
    let parent = path.parent().ok_or_else(|| {
        anyhow!(
            "cannot durably re-sync {description} with no parent: {}",
            path.display()
        )
    })?;
    crate::utils::atomic_write::fsync_parent_dir(parent).map_err(|error| {
        anyhow!(
            "failed to durably re-sync the parent of {description} at {}: {error}",
            path.display()
        )
    })?;
    Ok(())
}

fn load_intent_revision_hmac_key(
    persistence: &HeadlessSessionPersistence,
) -> anyhow::Result<Option<[u8; 32]>> {
    let path = intent_revision_hmac_key_path(persistence);
    let Some((mut file, metadata)) =
        open_intent_revision_file_no_follow(&path, "IntentSpec revision HMAC key")?
    else {
        return Ok(None);
    };
    if metadata.len() != 32 {
        return Err(anyhow!(
            "IntentSpec revision HMAC key at {} must contain exactly 32 bytes",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(anyhow!(
                "IntentSpec revision HMAC key at {} must not be accessible to group or other users",
                path.display()
            ));
        }
    }
    let mut key = [0u8; 32];
    file.read_exact(&mut key).map_err(|error| {
        anyhow!(
            "failed to read the 32-byte IntentSpec revision HMAC key {}: {error}",
            path.display()
        )
    })?;
    let mut trailing = [0u8; 1];
    if file.read(&mut trailing).map_err(|error| {
        anyhow!(
            "failed to verify the IntentSpec revision HMAC key length at {}: {error}",
            path.display()
        )
    })? != 0
    {
        return Err(anyhow!(
            "IntentSpec revision HMAC key at {} must contain exactly 32 bytes",
            path.display()
        ));
    }
    sync_open_intent_revision_file_and_parent(&file, &path, "IntentSpec revision HMAC key")?;
    Ok(Some(key))
}

fn workflow_has_intent_revision_hmac_commitment(
    replay: &crate::internal::ai::session::CodeWorkflowReplay,
) -> bool {
    replay.events.iter().any(|event| {
        matches!(
            &event.event,
            CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                intent_revision: Some(_),
                ..
            } | CodeWorkflowEventKind::InteractionResolved {
                intent_revision_consumption: Some(_),
                ..
            }
        )
    })
}

fn load_or_create_intent_revision_hmac_key(
    persistence: &HeadlessSessionPersistence,
) -> anyhow::Result<[u8; 32]> {
    if let Some(key) = load_intent_revision_hmac_key(persistence)? {
        return Ok(key);
    }
    let mut key = [0u8; 32];
    SystemRandom::new().fill(&mut key).map_err(|_| {
        anyhow!("operating-system randomness failed while creating IntentSpec revision HMAC key")
    })?;
    let path = intent_revision_hmac_key_path(persistence);
    crate::utils::atomic_write::write_atomic(&path, &key, true).map_err(|error| {
        anyhow!(
            "failed to persist IntentSpec revision HMAC key to {}: {error}",
            path.display()
        )
    })?;
    Ok(key)
}

fn update_revision_digest_field(hasher: &mut ring::hmac::Context, label: &[u8], value: &[u8]) {
    hasher.update(&(label.len() as u64).to_be_bytes());
    hasher.update(label);
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn intent_revision_sidecar_digest(
    interaction_id: &str,
    command: &CodeCommandIdentity,
    intent_id: &str,
    intent_spec: &str,
    note: Option<&str>,
    hmac_key: &[u8],
) -> anyhow::Result<String> {
    if hmac_key.len() != 32 {
        return Err(anyhow!(
            "IntentSpec revision HMAC key must contain exactly 32 bytes"
        ));
    }
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, hmac_key);
    let mut digest = ring::hmac::Context::with_key(&key);
    digest.update(INTENT_REVISION_SIDECAR_DIGEST_DOMAIN);
    update_revision_digest_field(
        &mut digest,
        b"schema_version",
        &INTENT_REVISION_SIDECAR_SCHEMA_VERSION.to_be_bytes(),
    );
    update_revision_digest_field(&mut digest, b"interaction_id", interaction_id.as_bytes());
    update_revision_digest_field(&mut digest, b"repo_id", command.repo_id.as_bytes());
    update_revision_digest_field(&mut digest, b"session_id", command.session_id.as_bytes());
    update_revision_digest_field(
        &mut digest,
        b"principal_id",
        command.principal_id.as_bytes(),
    );
    update_revision_digest_field(&mut digest, b"command_id", command.command_id.as_bytes());
    update_revision_digest_field(&mut digest, b"intent_id", intent_id.as_bytes());
    update_revision_digest_field(&mut digest, b"intent_spec", intent_spec.as_bytes());
    match note {
        Some(note) => {
            update_revision_digest_field(&mut digest, b"note_present", b"1");
            update_revision_digest_field(&mut digest, b"note", note.as_bytes());
        }
        None => update_revision_digest_field(&mut digest, b"note_present", b"0"),
    }
    Ok(format!(
        "hmac-sha256:{}",
        hex::encode(digest.sign().as_ref())
    ))
}

fn validate_intent_revision_command(command: &CodeCommandIdentity) -> anyhow::Result<()> {
    if command.repo_id.trim().is_empty()
        || command.session_id.trim().is_empty()
        || command.principal_id.trim().is_empty()
        || command.command_id.trim().is_empty()
    {
        return Err(anyhow!(
            "IntentSpec revision sidecar has an incomplete command identity"
        ));
    }
    Ok(())
}

fn validate_prepared_intent_revision(
    persistence: &HeadlessSessionPersistence,
    prepared: &PreparedIntentRevision,
) -> anyhow::Result<String> {
    if prepared.schema_version != INTENT_REVISION_SIDECAR_SCHEMA_VERSION {
        return Err(anyhow!(
            "prepared IntentSpec revision sidecar has unsupported schema version {}",
            prepared.schema_version
        ));
    }
    validate_intent_revision_command(&prepared.command)?;
    if prepared.interaction_id.trim().is_empty() || prepared.intent_id.trim().is_empty() {
        return Err(anyhow!(
            "prepared IntentSpec revision sidecar is missing durable lineage identifiers"
        ));
    }
    let note = canonical_intent_revision_note_value(prepared.note.as_deref())?;
    if note != prepared.note {
        return Err(anyhow!(
            "prepared IntentSpec revision sidecar contains a non-canonical note"
        ));
    }
    let intent_spec = load_persisted_web_phase0_intent_spec(persistence, &prepared.intent_id)?;
    let hmac_key = load_intent_revision_hmac_key(persistence)?.ok_or_else(|| {
        anyhow!("prepared IntentSpec revision sidecar is missing its session HMAC key")
    })?;
    let expected = intent_revision_sidecar_digest(
        &prepared.interaction_id,
        &prepared.command,
        &prepared.intent_id,
        &intent_spec,
        prepared.note.as_deref(),
        &hmac_key,
    )?;
    if expected != prepared.sidecar_digest
        || !crate::internal::ai::session::jsonl::is_canonical_intent_revision_digest(
            &prepared.sidecar_digest,
        )
    {
        return Err(anyhow!(
            "prepared IntentSpec revision sidecar digest does not match its durable payload"
        ));
    }
    Ok(intent_spec)
}

fn validate_active_intent_revision(
    persistence: &HeadlessSessionPersistence,
    pending: &PendingIntentRevision,
) -> anyhow::Result<()> {
    if pending.intent_spec.trim().is_empty() {
        return Err(anyhow!("pending IntentSpec revision is missing intentSpec"));
    }
    let canonical_note = canonical_intent_revision_note_value(pending.note.as_deref())?;
    if canonical_note != pending.note {
        return Err(anyhow!(
            "pending IntentSpec revision contains a non-canonical note"
        ));
    }
    if let Some(authority) = pending.authority.as_ref() {
        if authority.schema_version != INTENT_REVISION_SIDECAR_SCHEMA_VERSION {
            return Err(anyhow!(
                "pending IntentSpec revision authority has unsupported schema version {}",
                authority.schema_version
            ));
        }
        validate_intent_revision_command(&authority.command)?;
        if authority.interaction_id.trim().is_empty() || authority.intent_id.trim().is_empty() {
            return Err(anyhow!(
                "pending IntentSpec revision authority is missing durable lineage identifiers"
            ));
        }
        let persisted_spec =
            load_persisted_web_phase0_intent_spec(persistence, &authority.intent_id)?;
        if persisted_spec != pending.intent_spec {
            return Err(anyhow!(
                "pending IntentSpec revision does not match its durable IntentSpec"
            ));
        }
        if let Some(sidecar_digest) = authority.sidecar_digest.as_deref() {
            let hmac_key = load_intent_revision_hmac_key(persistence)?.ok_or_else(|| {
                anyhow!("pending IntentSpec revision authority is missing its session HMAC key")
            })?;
            let expected = intent_revision_sidecar_digest(
                &authority.interaction_id,
                &authority.command,
                &authority.intent_id,
                &pending.intent_spec,
                pending.note.as_deref(),
                &hmac_key,
            )?;
            if expected != sidecar_digest
                || !crate::internal::ai::session::jsonl::is_canonical_intent_revision_digest(
                    sidecar_digest,
                )
            {
                return Err(anyhow!(
                    "pending IntentSpec revision authority digest does not match its body"
                ));
            }
        }
    }
    Ok(())
}

fn exact_intent_review_lineage(
    events: &[crate::internal::ai::session::CodeWorkflowEvent],
    interaction_id: &str,
    command_id: &str,
) -> anyhow::Result<String> {
    let mut marker_count = 0usize;
    let mut turn_match_count = 0usize;
    let mut phase0_match_count = 0usize;
    let mut intent_id = None::<String>;
    let mut lineage_phase0_turn_id = None::<String>;
    let mut latest_turn_id = None::<String>;
    for event in events {
        let CodeWorkflowEventKind::IntentReviewRequested {
            interaction_id: candidate,
            intent_id: candidate_intent_id,
            turn_id,
            phase0_turn_id: candidate_phase0_turn_id,
        } = &event.event
        else {
            continue;
        };
        if candidate != interaction_id {
            continue;
        }
        marker_count += 1;
        if intent_id
            .as_ref()
            .is_some_and(|expected| expected != candidate_intent_id)
        {
            return Err(anyhow!(
                "IntentSpec revision interaction '{interaction_id}' has conflicting durable intent lineage"
            ));
        }
        intent_id.get_or_insert_with(|| candidate_intent_id.clone());
        if lineage_phase0_turn_id
            .as_ref()
            .is_some_and(|expected| expected != candidate_phase0_turn_id)
        {
            return Err(anyhow!(
                "IntentSpec revision interaction '{interaction_id}' has conflicting durable Phase 0 lineage"
            ));
        }
        lineage_phase0_turn_id.get_or_insert_with(|| candidate_phase0_turn_id.clone());
        latest_turn_id = Some(turn_id.clone());
        turn_match_count += usize::from(turn_id == command_id);
        phase0_match_count += usize::from(candidate_phase0_turn_id == command_id);
    }
    let exact_current_owner = if marker_count == 1 {
        turn_match_count + phase0_match_count == 1
    } else {
        latest_turn_id.as_deref() == Some(command_id)
            && turn_match_count == 1
            && phase0_match_count == 0
    };
    if !exact_current_owner {
        return Err(anyhow!(
            "IntentSpec revision interaction '{interaction_id}' has ambiguous durable review lineage"
        ));
    }
    intent_id.ok_or_else(|| {
        anyhow!(
            "IntentSpec revision interaction '{interaction_id}' has no durable IntentReviewRequested lineage"
        )
    })
}

enum IntentRevisionTerminalBinding {
    Bound(IntentRevisionTerminalAuthority),
    Legacy(IntentRevisionTerminalAuthority),
}

fn intent_revision_terminal_authority_from_projection(
    terminal: &ValidatedIntentRevisionSourceTerminal<'_>,
) -> IntentRevisionTerminalAuthority {
    IntentRevisionTerminalAuthority {
        interaction_id: terminal.interaction_id.to_string(),
        command: terminal.command.clone(),
        terminal_event_id: terminal.terminal_event_id,
        terminal_sequence: terminal.terminal_sequence,
        intent_id: terminal.intent_id.to_string(),
        sidecar_digest: terminal.sidecar_digest.map(str::to_string),
    }
}

fn intent_revision_terminal_binding_from_index(
    index: &ValidatedIntentRevisionReceiptIndex<'_>,
    interaction_id: &str,
    expected_command_id: Option<&str>,
) -> anyhow::Result<Option<IntentRevisionTerminalBinding>> {
    let Some(projected) = index.source_terminal_for_interaction(interaction_id) else {
        return Ok(None);
    };
    if expected_command_id.is_some_and(|expected| projected.command.command_id != expected) {
        return Err(anyhow!(
            "IntentSpec revision interaction '{interaction_id}' resolved under an unexpected durable command"
        ));
    }
    let terminal = intent_revision_terminal_authority_from_projection(projected);
    Ok(Some(if projected.legacy_terminal {
        IntentRevisionTerminalBinding::Legacy(terminal)
    } else {
        IntentRevisionTerminalBinding::Bound(terminal)
    }))
}

fn intent_revision_terminal_binding(
    replay: &crate::internal::ai::session::CodeWorkflowReplay,
    interaction_id: &str,
    expected_command_id: Option<&str>,
) -> anyhow::Result<Option<IntentRevisionTerminalBinding>> {
    let index = validated_intent_revision_consumption_receipts(replay)
        .map_err(|error| anyhow!("IntentSpec revision authority replay is invalid: {error}"))?;
    intent_revision_terminal_binding_from_index(&index, interaction_id, expected_command_id)
}

fn bound_intent_revision_terminals_from_index(
    index: &ValidatedIntentRevisionReceiptIndex<'_>,
) -> Vec<IntentRevisionTerminalAuthority> {
    index
        .source_terminals()
        .filter(|terminal| !terminal.legacy_terminal)
        .map(intent_revision_terminal_authority_from_projection)
        .collect()
}

fn exact_intent_revision_source_from_index<'replay, 'index>(
    index: &'index ValidatedIntentRevisionReceiptIndex<'replay>,
    terminal: &IntentRevisionTerminalAuthority,
) -> anyhow::Result<&'index ValidatedIntentRevisionSourceTerminal<'replay>> {
    let projected = index
        .exact_source_terminal(
            terminal.terminal_event_id,
            terminal.terminal_sequence,
            &terminal.interaction_id,
            &terminal.command,
        )
        .ok_or_else(|| anyhow!("IntentSpec revision terminal lost its indexed authority"))?;
    if projected.intent_id != terminal.intent_id
        || projected.sidecar_digest != terminal.sidecar_digest.as_deref()
    {
        return Err(anyhow!(
            "IntentSpec revision terminal conflicts with its indexed intent or HMAC authority"
        ));
    }
    Ok(projected)
}

fn exact_intent_revision_consumer_from_index<'replay>(
    index: &ValidatedIntentRevisionReceiptIndex<'replay>,
    terminal: &IntentRevisionTerminalAuthority,
) -> anyhow::Result<Option<&'replay IntentRevisionConsumption>> {
    exact_intent_revision_source_from_index(index, terminal)?;
    Ok(index
        .exact_receipt_for_source(
            terminal.terminal_event_id,
            terminal.terminal_sequence,
            &terminal.interaction_id,
            &terminal.command,
        )
        .map(|receipt| receipt.consumption))
}

fn exact_intent_revision_consumer(
    replay: &crate::internal::ai::session::CodeWorkflowReplay,
    terminal: &IntentRevisionTerminalAuthority,
) -> anyhow::Result<Option<IntentRevisionConsumption>> {
    let index = validated_intent_revision_consumption_receipts(replay)
        .map_err(|error| anyhow!("IntentSpec revision authority replay is invalid: {error}"))?;
    Ok(exact_intent_revision_consumer_from_index(&index, terminal)?.cloned())
}

fn validate_all_intent_revision_consumption_receipts<'a>(
    persistence: &HeadlessSessionPersistence,
    replay: &'a crate::internal::ai::session::CodeWorkflowReplay,
) -> anyhow::Result<ValidatedIntentRevisionReceiptIndex<'a>> {
    let index = validated_intent_revision_consumption_receipts(replay)
        .context("durable IntentSpec revision authority replay is invalid")?;
    let mut saw_hmac_commitment = false;
    for receipt in index.receipts() {
        let consumption = receipt.consumption;
        let claim = &consumption.claim;
        if !intent_revision_command_is_in_session(persistence, &claim.source_command)
            || !intent_revision_command_is_in_session(persistence, &claim.consumer_intent.identity)
        {
            return Err(anyhow!(
                "durable IntentSpec revision consumption receipt has invalid session lineage"
            ));
        }
        saw_hmac_commitment |= claim.sidecar_digest.is_some();
    }
    if saw_hmac_commitment && load_intent_revision_hmac_key(persistence)?.is_none() {
        return Err(anyhow!(
            "IntentSpec revision consumption receipts are missing their session HMAC key"
        ));
    }
    Ok(index)
}

fn pending_consumption_binding(
    pending: &PendingIntentRevision,
    consumer_intent: CodeCommandIntent,
) -> anyhow::Result<IntentRevisionConsumptionClaim> {
    let authority = pending.authority.as_ref().ok_or_else(|| {
        anyhow!("pending IntentSpec revision has not been validated against durable lineage")
    })?;
    let sidecar_digest = authority.sidecar_digest.clone().ok_or_else(|| {
        anyhow!("pending IntentSpec revision has no durable HMAC consumption binding")
    })?;
    Ok(IntentRevisionConsumptionClaim {
        schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
        interaction_id: authority.interaction_id.clone(),
        source_command: authority.command.clone(),
        consumer_intent,
        terminal_event_id: authority.terminal_event_id,
        terminal_sequence: authority.terminal_sequence,
        intent_id: authority.intent_id.clone(),
        sidecar_digest: Some(sidecar_digest),
    })
}

fn ensure_legacy_intent_revision_digest_before_consumption(
    persistence: &HeadlessSessionPersistence,
    mut pending: PendingIntentRevision,
) -> anyhow::Result<PendingIntentRevision> {
    let Some(authority) = pending.authority.as_mut() else {
        return Err(anyhow!(
            "pending IntentSpec revision has not been validated against durable lineage"
        ));
    };
    if !authority.legacy_terminal || authority.sidecar_digest.is_some() {
        return Ok(pending);
    }
    let replay = persistence
        .goal_event_store()
        .load_intent_revision_workflow_replay_committed()?;
    let has_hmac_commitment = workflow_has_intent_revision_hmac_commitment(&replay);
    let hmac_key = match load_intent_revision_hmac_key(persistence)? {
        Some(key) => key,
        None if !has_hmac_commitment => load_or_create_intent_revision_hmac_key(persistence)?,
        None => {
            return Err(anyhow!(
                "IntentSpec revision HMAC key is missing after a durable revision commitment"
            ));
        }
    };
    authority.sidecar_digest = Some(intent_revision_sidecar_digest(
        &authority.interaction_id,
        &authority.command,
        &authority.intent_id,
        &pending.intent_spec,
        pending.note.as_deref(),
        &hmac_key,
    )?);
    persist_pending_intent_revision(persistence, &pending)?;
    Ok(pending)
}

fn intent_revision_command_is_in_session(
    persistence: &HeadlessSessionPersistence,
    command: &CodeCommandIdentity,
) -> bool {
    let (_, repo_id, principal_id) = persistence.worker_durability_config();
    command.repo_id == repo_id
        && command.session_id == persistence.durability_session_id()
        && command.principal_id == principal_id
}

fn prepared_matches_open_intent_review(
    persistence: &HeadlessSessionPersistence,
    replay: &crate::internal::ai::session::CodeWorkflowReplay,
    prepared: &PreparedIntentRevision,
) -> anyhow::Result<bool> {
    if !intent_revision_command_is_in_session(persistence, &prepared.command) {
        return Ok(false);
    }
    let Some((interaction_id, intent_id, stored_turn_id, phase0_turn_id)) =
        open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event))
    else {
        return Ok(false);
    };
    if interaction_id != prepared.interaction_id
        || intent_id != prepared.intent_id
        || (stored_turn_id != prepared.command.command_id
            && phase0_turn_id != prepared.command.command_id)
    {
        return Ok(false);
    }
    Ok(exact_intent_review_lineage(
        &replay.events,
        &prepared.interaction_id,
        &prepared.command.command_id,
    )? == prepared.intent_id)
}

fn legacy_active_matches_open_intent_review(
    persistence: &HeadlessSessionPersistence,
    replay: &crate::internal::ai::session::CodeWorkflowReplay,
    revision_index: &ValidatedIntentRevisionReceiptIndex<'_>,
    active: &PendingIntentRevision,
) -> anyhow::Result<bool> {
    // A resurrected baseline Active may belong to an older committed Modify.
    // Never discard it merely because a newer open gate happens to carry the
    // same IntentSpec bytes; let full legacy-terminal lineage validation handle
    // that case (and fence any later-command ambiguity).
    for terminal in revision_index.source_terminals() {
        if !terminal.legacy_terminal
            || !intent_revision_command_is_in_session(persistence, terminal.command)
        {
            continue;
        }
        if load_persisted_web_phase0_intent_spec(persistence, terminal.intent_id)?
            == active.intent_spec
        {
            return Ok(false);
        }
    }
    let Some((interaction_id, intent_id, stored_turn_id, phase0_turn_id)) =
        open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event))
    else {
        return Ok(false);
    };
    if intent_revision_terminal_binding_from_index(revision_index, &interaction_id, None)?.is_some()
    {
        return Ok(false);
    }
    let command_id = if !stored_turn_id.is_empty() {
        stored_turn_id
    } else {
        phase0_turn_id
    };
    if command_id.is_empty()
        || exact_intent_review_lineage(&replay.events, &interaction_id, &command_id)? != intent_id
    {
        return Ok(false);
    }
    let persisted = load_persisted_web_phase0_intent_spec(persistence, &intent_id)?;
    Ok(persisted == active.intent_spec)
}

fn prepared_matches_terminal(
    prepared: &PreparedIntentRevision,
    terminal: &IntentRevisionTerminalAuthority,
) -> bool {
    prepared.schema_version == INTENT_REVISION_SIDECAR_SCHEMA_VERSION
        && prepared.interaction_id == terminal.interaction_id
        && prepared.command == terminal.command
        && prepared.intent_id == terminal.intent_id
        && terminal
            .sidecar_digest
            .as_deref()
            .is_some_and(|digest| digest == prepared.sidecar_digest)
}

fn terminal_has_later_web_intent_from_index(
    index: &ValidatedIntentRevisionReceiptIndex<'_>,
    terminal: &IntentRevisionTerminalAuthority,
) -> anyhow::Result<bool> {
    Ok(exact_intent_revision_source_from_index(index, terminal)?.later_web_intent)
}

fn validate_uncommitted_intent_revision_consumer(
    persistence: &HeadlessSessionPersistence,
    replay: &crate::internal::ai::session::CodeWorkflowReplay,
    terminal: &IntentRevisionTerminalAuthority,
    consumption: &IntentRevisionConsumption,
) -> anyhow::Result<CodeCommandStatus> {
    let revision_index = validated_intent_revision_consumption_receipts(replay)
        .map_err(|error| anyhow!("IntentSpec revision authority replay is invalid: {error}"))?;
    validate_uncommitted_intent_revision_consumer_from_index(
        persistence,
        &revision_index,
        terminal,
        consumption,
    )
}

fn validate_uncommitted_intent_revision_consumer_from_index(
    persistence: &HeadlessSessionPersistence,
    revision_index: &ValidatedIntentRevisionReceiptIndex<'_>,
    terminal: &IntentRevisionTerminalAuthority,
    consumption: &IntentRevisionConsumption,
) -> anyhow::Result<CodeCommandStatus> {
    let claim = &consumption.claim;
    if !intent_revision_command_is_in_session(persistence, &claim.source_command)
        || !intent_revision_command_is_in_session(persistence, &claim.consumer_intent.identity)
        || terminal.command != claim.source_command
        || terminal.interaction_id != claim.interaction_id
        || terminal.terminal_event_id != claim.terminal_event_id
        || terminal.terminal_sequence != claim.terminal_sequence
        || terminal.intent_id != claim.intent_id
        || terminal.sidecar_digest != claim.sidecar_digest
    {
        return Err(anyhow!(
            "IntentSpec revision consuming handoff has conflicting session or source lineage"
        ));
    }
    let expected_status = revision_index.claimed_intent_revision_consumer_status(consumption)?;
    let Some((intent, status)) = persistence
        .goal_event_store()
        .code_command_intent_status(&claim.consumer_intent.identity)?
    else {
        return Err(anyhow!(
            "IntentSpec revision consuming handoff lost its durable consumer intent"
        ));
    };
    if intent != claim.consumer_intent || status != expected_status {
        return Err(anyhow!(
            "IntentSpec revision consumer reached an impossible terminal state before its receipt"
        ));
    }
    Ok(status)
}

enum ClaimingIntentRevisionRecovery {
    Rearmed(AuthenticatedActiveIntentRevision),
    Consuming {
        authenticated: AuthenticatedActiveIntentRevision,
        consumption: Box<IntentRevisionConsumption>,
    },
    Consumed,
}

fn reconcile_claiming_intent_revision(
    persistence: &HeadlessSessionPersistence,
    replay: &crate::internal::ai::session::CodeWorkflowReplay,
    claiming: ClaimingIntentRevision,
) -> anyhow::Result<ClaimingIntentRevisionRecovery> {
    let revision_index = validate_all_intent_revision_consumption_receipts(persistence, replay)?;
    reconcile_claiming_intent_revision_from_index(persistence, &revision_index, claiming)
}

fn reconcile_claiming_intent_revision_from_index(
    persistence: &HeadlessSessionPersistence,
    revision_index: &ValidatedIntentRevisionReceiptIndex<'_>,
    claiming: ClaimingIntentRevision,
) -> anyhow::Result<ClaimingIntentRevisionRecovery> {
    validate_claiming_intent_revision(persistence, &claiming)?;
    let authenticated = authenticate_active_intent_revision_from_index(
        persistence,
        revision_index,
        claiming.active.clone(),
    )?;
    let expected = pending_consumption_binding(
        &authenticated.pending,
        claiming.claim.consumer_intent.clone(),
    )?;
    if expected != claiming.claim {
        return Err(anyhow!(
            "claiming IntentSpec revision conflicts with its durable source authority"
        ));
    }
    match exact_intent_revision_consumer_from_index(revision_index, &authenticated.terminal)? {
        Some(receipt) if receipt.claim == claiming.claim => {
            clear_pending_intent_revision(persistence)?;
            return Ok(ClaimingIntentRevisionRecovery::Consumed);
        }
        Some(_) => {
            return Err(anyhow!(
                "claiming IntentSpec revision conflicts with its durable receipt"
            ));
        }
        None => {}
    }
    let store = persistence.goal_event_store();
    match store.code_command_intent_status(&claiming.claim.consumer_intent.identity)? {
        None => {
            if let Some(prior_consumption) = revision_index
                .latest_recoverable_intent_revision_attempt_before_claim(&claiming.claim)?
            {
                persist_consuming_intent_revision(
                    persistence,
                    &ConsumingIntentRevision {
                        schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
                        active: authenticated.pending.clone(),
                        consumption: prior_consumption.clone(),
                    },
                )?;
                return Ok(ClaimingIntentRevisionRecovery::Consuming {
                    authenticated,
                    consumption: Box::new(prior_consumption),
                });
            }
            persist_pending_intent_revision(persistence, &authenticated.pending)?;
            Ok(ClaimingIntentRevisionRecovery::Rearmed(authenticated))
        }
        Some((actual, _)) if actual != claiming.claim.consumer_intent => Err(anyhow!(
            "claiming IntentSpec revision consumer identity conflicts with its durable command"
        )),
        Some(_) => {
            let (consumption, status) =
                store.resolve_claimed_intent_revision_consumption(&claiming.claim)?;
            match status {
                CodeCommandStatus::Pending => {
                    // Persist the full event-id/sequence attribution before
                    // generic mutation recovery is allowed to terminalize the
                    // abandoned command.
                    persist_consuming_intent_revision(
                        persistence,
                        &ConsumingIntentRevision {
                            schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
                            active: authenticated.pending.clone(),
                            consumption: consumption.clone(),
                        },
                    )?;
                    Ok(ClaimingIntentRevisionRecovery::Consuming {
                        authenticated,
                        consumption: Box::new(consumption),
                    })
                }
                CodeCommandStatus::Failed { ref reason }
                    if reason
                        == crate::internal::ai::session::jsonl::PRE_MUTATION_CANCELLED_COMMAND_REASON =>
                {
                    // Keep the exact cancelled command attribution on disk;
                    // logical Active is restored only in memory. A later
                    // retry may replace this with a new Claiming envelope,
                    // whose replay validation still pins this prior canonical
                    // no-mutation terminal.
                    persist_consuming_intent_revision(
                        persistence,
                        &ConsumingIntentRevision {
                            schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
                            active: authenticated.pending.clone(),
                            consumption: consumption.clone(),
                        },
                    )?;
                    Ok(ClaimingIntentRevisionRecovery::Consuming {
                        authenticated,
                        consumption: Box::new(consumption),
                    })
                }
                CodeCommandStatus::Succeeded { .. }
                | CodeCommandStatus::Failed { .. }
                | CodeCommandStatus::Indeterminate { .. } => Err(anyhow!(
                    "claiming IntentSpec revision consumer reached a non-recoverable terminal state without a durable consumption receipt"
                )),
            }
        }
    }
}

#[cfg(test)]
fn authenticated_uncommitted_intent_revision_consumer(
    persistence: &HeadlessSessionPersistence,
) -> anyhow::Result<Option<IntentRevisionConsumption>> {
    authenticated_uncommitted_intent_revision_consumer_with_projection_recovery(
        persistence,
        false,
        false,
    )
}

fn authenticated_uncommitted_intent_revision_consumer_with_projection_recovery(
    persistence: &HeadlessSessionPersistence,
    recover_succeeded_cancel_projection: bool,
    recover_succeeded_replacement_projection: bool,
) -> anyhow::Result<Option<IntentRevisionConsumption>> {
    let sidecar = load_intent_revision_sidecar(persistence)?;
    let replay = persistence
        .goal_event_store()
        .load_intent_revision_workflow_replay_committed()?;
    let revision_index = validate_all_intent_revision_consumption_receipts(persistence, &replay)?;
    // Recovery exceptions are only safe after the full durable authority set
    // has passed the same source-command and interaction first-writer checks
    // used by the ordinary sidecar reconciliation path.
    let bound_terminals = bound_intent_revision_terminals_from_index(&revision_index);
    let mut unconsumed_terminal = None::<IntentRevisionTerminalAuthority>;
    let mut uncommitted_consumption = None;

    match sidecar {
        None => {}
        Some(LoadedIntentRevisionSidecar::Prepared(prepared)) => {
            match intent_revision_terminal_binding_from_index(
                &revision_index,
                &prepared.interaction_id,
                Some(&prepared.command.command_id),
            )? {
                None => {
                    if !prepared_matches_open_intent_review(persistence, &replay, &prepared)? {
                        return Err(anyhow!(
                            "prepared IntentSpec revision has no exact open review lineage"
                        ));
                    }
                }
                Some(IntentRevisionTerminalBinding::Legacy(_)) => {
                    return Err(anyhow!(
                        "prepared IntentSpec revision is bound to a legacy terminal without a sidecar commitment"
                    ));
                }
                Some(IntentRevisionTerminalBinding::Bound(terminal)) => {
                    if !prepared_matches_terminal(&prepared, &terminal) {
                        return Err(anyhow!(
                            "prepared IntentSpec revision conflicts with its durable terminal"
                        ));
                    }
                    if exact_intent_revision_consumer_from_index(&revision_index, &terminal)?
                        .is_none()
                    {
                        unconsumed_terminal = Some(terminal);
                    }
                }
            }
        }
        Some(LoadedIntentRevisionSidecar::Active(active)) => {
            if !(active.authority.is_none()
                && legacy_active_matches_open_intent_review(
                    persistence,
                    &replay,
                    &revision_index,
                    &active,
                )?)
            {
                let authenticated = authenticate_active_intent_revision_from_index(
                    persistence,
                    &revision_index,
                    active,
                )?;
                if exact_intent_revision_consumer_from_index(
                    &revision_index,
                    &authenticated.terminal,
                )?
                .is_none()
                {
                    if terminal_has_later_web_intent_from_index(
                        &revision_index,
                        &authenticated.terminal,
                    )? {
                        return Err(anyhow!(
                            "active IntentSpec revision is ambiguous after a later durable Web command"
                        ));
                    }
                    unconsumed_terminal = Some(authenticated.terminal);
                }
            }
        }
        Some(LoadedIntentRevisionSidecar::Claiming(claiming)) => {
            match reconcile_claiming_intent_revision_from_index(
                persistence,
                &revision_index,
                claiming,
            )? {
                ClaimingIntentRevisionRecovery::Rearmed(authenticated) => {
                    unconsumed_terminal = Some(authenticated.terminal);
                }
                ClaimingIntentRevisionRecovery::Consuming {
                    authenticated,
                    consumption,
                } => {
                    validate_uncommitted_intent_revision_consumer_from_index(
                        persistence,
                        &revision_index,
                        &authenticated.terminal,
                        &consumption,
                    )?;
                    unconsumed_terminal = Some(authenticated.terminal);
                    uncommitted_consumption = Some(*consumption);
                }
                ClaimingIntentRevisionRecovery::Consumed => {}
            }
        }
        Some(LoadedIntentRevisionSidecar::Consuming(consuming)) => {
            let authenticated = authenticate_active_intent_revision_from_index(
                persistence,
                &revision_index,
                consuming.active.clone(),
            )?;
            let expected = pending_consumption_binding(
                &authenticated.pending,
                consuming.consumption.claim.consumer_intent.clone(),
            )?;
            if expected != consuming.consumption.claim {
                return Err(anyhow!(
                    "consuming IntentSpec revision conflicts with its durable source authority"
                ));
            }
            match exact_intent_revision_consumer_from_index(
                &revision_index,
                &authenticated.terminal,
            )? {
                Some(receipt) if receipt == &consuming.consumption => {}
                Some(_) => {
                    return Err(anyhow!(
                        "consuming IntentSpec revision conflicts with its durable receipt"
                    ));
                }
                None => {
                    validate_uncommitted_intent_revision_consumer_from_index(
                        persistence,
                        &revision_index,
                        &authenticated.terminal,
                        &consuming.consumption,
                    )?;
                    unconsumed_terminal = Some(authenticated.terminal);
                    uncommitted_consumption = Some(consuming.consumption);
                }
            }
        }
    }

    // `/intent cancel` has no provider/tool phase: once its exact receipt is
    // durable, the whole user-requested effect is proven even if the process
    // dies before Runtime appends success. Hand that one fixed-payload Pending
    // command to generic recovery so it can complete deterministically. A
    // receipt for any other revision input may have crossed provider/tool
    // execution and remains subject to the ordinary fail-closed fence.
    let mut receipt_recovery_candidate = None::<IntentRevisionConsumption>;
    for receipt in revision_index.receipts() {
        let consumption = receipt.consumption;
        let Some(status) = receipt.consumer_status.as_ref() else {
            return Err(anyhow!(
                "durable IntentSpec revision receipt has no consumer command status"
            ));
        };
        let canonical_cancel = receipt.canonical_cancel;
        let no_later_web_intent = !receipt.later_web_intent;
        let open_replacement_review = receipt.replacement_review && receipt.replacement_review_open;
        let replacement_review = open_replacement_review
            && no_later_web_intent
            && (matches!(
                status,
                CodeCommandStatus::Pending | CodeCommandStatus::Indeterminate { .. }
            ) || (recover_succeeded_replacement_projection
                && matches!(status, CodeCommandStatus::Succeeded { .. })));
        let recover_cancel = canonical_cancel
            && no_later_web_intent
            && (matches!(
                status,
                CodeCommandStatus::Pending | CodeCommandStatus::Indeterminate { .. }
            ) || (recover_succeeded_cancel_projection
                && matches!(status, CodeCommandStatus::Succeeded { .. })));
        if replacement_review || recover_cancel {
            let replace = receipt_recovery_candidate.as_ref().is_none_or(|candidate| {
                consumption.consumer_intent_sequence > candidate.consumer_intent_sequence
            });
            if replace {
                receipt_recovery_candidate = Some(consumption.clone());
            }
        }
    }
    if let Some(candidate) = receipt_recovery_candidate {
        if uncommitted_consumption
            .as_ref()
            .is_some_and(|existing| existing != &candidate)
        {
            return Err(anyhow!(
                "multiple interrupted IntentSpec revision consumers require recovery"
            ));
        }
        uncommitted_consumption = Some(candidate);
    }

    // Before generic mutation recovery writes anything, prove that every
    // other digest-bound Modify terminal is already closed by one exact
    // receipt. A missing sidecar is never treated as consumption evidence.
    for terminal in bound_terminals {
        if unconsumed_terminal.as_ref().is_some_and(|candidate| {
            candidate.terminal_event_id == terminal.terminal_event_id
                && candidate.terminal_sequence == terminal.terminal_sequence
        }) {
            continue;
        }
        if exact_intent_revision_consumer_from_index(&revision_index, &terminal)?.is_none() {
            return Err(anyhow!(
                "bound IntentSpec Modify terminal is missing both its sidecar and an exact consumption receipt"
            ));
        }
    }
    Ok(uncommitted_consumption)
}

struct AuthenticatedActiveIntentRevision {
    pending: PendingIntentRevision,
    terminal: IntentRevisionTerminalAuthority,
}

fn authenticate_active_intent_revision(
    persistence: &HeadlessSessionPersistence,
    replay: &crate::internal::ai::session::CodeWorkflowReplay,
    pending: PendingIntentRevision,
) -> anyhow::Result<AuthenticatedActiveIntentRevision> {
    let revision_index = validated_intent_revision_consumption_receipts(replay)
        .map_err(|error| anyhow!("IntentSpec revision authority replay is invalid: {error}"))?;
    authenticate_active_intent_revision_from_index(persistence, &revision_index, pending)
}

fn authenticate_active_intent_revision_from_index(
    persistence: &HeadlessSessionPersistence,
    revision_index: &ValidatedIntentRevisionReceiptIndex<'_>,
    mut pending: PendingIntentRevision,
) -> anyhow::Result<AuthenticatedActiveIntentRevision> {
    if let Some(authority) = pending.authority.as_ref() {
        if !intent_revision_command_is_in_session(persistence, &authority.command) {
            return Err(anyhow!(
                "pending IntentSpec revision authority belongs to another durable session"
            ));
        }
        let binding = intent_revision_terminal_binding_from_index(
            revision_index,
            &authority.interaction_id,
            Some(&authority.command.command_id),
        )?
        .ok_or_else(|| {
            anyhow!("pending IntentSpec revision has no matching durable terminal authority")
        })?;
        let (terminal, legacy) = match binding {
            IntentRevisionTerminalBinding::Bound(terminal) => (terminal, false),
            IntentRevisionTerminalBinding::Legacy(terminal) => (terminal, true),
        };
        if authority.legacy_terminal != legacy
            || !intent_revision_authority_matches_terminal(authority, &terminal)
        {
            return Err(anyhow!(
                "pending IntentSpec revision conflicts with its durable terminal authority"
            ));
        }
        return Ok(AuthenticatedActiveIntentRevision { pending, terminal });
    }

    // Baseline readers persisted only {intentSpec,note}. Treat that local
    // file as legacy authority solely when one exact pre-binding Modify
    // terminal and its prefix marker prove the same durable IntentSpec.
    let mut candidate = None;
    for projected in revision_index.source_terminals() {
        if !projected.legacy_terminal
            || !intent_revision_command_is_in_session(persistence, projected.command)
        {
            continue;
        }
        let persisted_spec =
            load_persisted_web_phase0_intent_spec(persistence, projected.intent_id)?;
        if persisted_spec != pending.intent_spec {
            continue;
        }
        let terminal = intent_revision_terminal_authority_from_projection(projected);
        if candidate.replace(terminal).is_some() {
            return Err(anyhow!(
                "legacy pending IntentSpec revision has ambiguous durable Modify authority"
            ));
        }
    }
    let terminal = candidate.ok_or_else(|| {
        anyhow!("legacy pending IntentSpec revision has no exact durable Modify authority")
    })?;
    if exact_intent_revision_consumer_from_index(revision_index, &terminal)?.is_none()
        && terminal_has_later_web_intent_from_index(revision_index, &terminal)?
    {
        return Err(anyhow!(
            "legacy pending IntentSpec revision is ambiguous after a later durable Web command"
        ));
    }
    pending.authority = Some(PendingIntentRevisionAuthority {
        schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
        legacy_terminal: true,
        interaction_id: terminal.interaction_id.clone(),
        command: terminal.command.clone(),
        terminal_event_id: terminal.terminal_event_id,
        terminal_sequence: terminal.terminal_sequence,
        intent_id: terminal.intent_id.clone(),
        sidecar_digest: None,
    });
    Ok(AuthenticatedActiveIntentRevision { pending, terminal })
}

pub(crate) fn prepare_intent_revision_sidecar(
    persistence: &HeadlessSessionPersistence,
    interaction_id: &str,
    runtime_turn_id: &str,
    note: Option<String>,
) -> Result<String, RuntimeWorkerError> {
    let note = canonical_intent_revision_note_value(note.as_deref())?;
    let store = persistence.goal_event_store();
    let replay = store
        .load_intent_revision_workflow_replay_committed()
        .map_err(|error| {
            RuntimeWorkerError::IndeterminateSideEffect(format!(
                "IntentSpec revision could not verify its durable review lineage: {error}"
            ))
        })?;
    let (_, repo_id, principal_id) = persistence.worker_durability_config();
    let command = CodeCommandIdentity::new(
        repo_id,
        persistence.durability_session_id(),
        principal_id,
        runtime_turn_id,
    );
    let existing_prepared = match load_intent_revision_sidecar(persistence)
        .map_err(|error| RuntimeWorkerError::IndeterminateSideEffect(error.to_string()))?
    {
        Some(LoadedIntentRevisionSidecar::Prepared(existing)) => {
            if existing.schema_version != INTENT_REVISION_SIDECAR_SCHEMA_VERSION
                || existing.interaction_id != interaction_id
                || existing.command != command
                || existing.note != note
            {
                return Err(RuntimeWorkerError::InteractionResponseConflict {
                    turn_id: runtime_turn_id.to_string(),
                    interaction_id: interaction_id.to_string(),
                });
            }
            match intent_revision_terminal_binding(&replay, interaction_id, Some(runtime_turn_id))
                .map_err(|error| RuntimeWorkerError::IndeterminateSideEffect(error.to_string()))?
            {
                Some(IntentRevisionTerminalBinding::Bound(terminal))
                    if prepared_matches_terminal(&existing, &terminal) =>
                {
                    // The first HTTP future may have been aborted after the
                    // worker committed the combined terminal. Reuse the exact
                    // Prepared binding so the queued duplicate can join the
                    // existing response and complete promotion.
                    return Ok(existing.sidecar_digest);
                }
                Some(_) => {
                    return Err(RuntimeWorkerError::IndeterminateSideEffect(
                        "prepared IntentSpec revision conflicts with its durable terminal"
                            .to_string(),
                    ));
                }
                None => Some(existing),
            }
        }
        Some(LoadedIntentRevisionSidecar::Active(_))
        | Some(LoadedIntentRevisionSidecar::Claiming(_))
        | Some(LoadedIntentRevisionSidecar::Consuming(_)) => {
            return Err(RuntimeWorkerError::IndeterminateSideEffect(
                "IntentSpec revision preparation conflicts with an active durable revision"
                    .to_string(),
            ));
        }
        None => None,
    };
    let open = open_intent_review_from_workflow(replay.events.iter().map(|event| &event.event));
    let Some((open_interaction_id, open_intent_id, stored_turn_id, phase0_turn_id)) = open else {
        return Err(RuntimeWorkerError::InteractionResponseConflict {
            turn_id: runtime_turn_id.to_string(),
            interaction_id: interaction_id.to_string(),
        });
    };
    if open_interaction_id != interaction_id
        || (stored_turn_id != runtime_turn_id && phase0_turn_id != runtime_turn_id)
    {
        return Err(RuntimeWorkerError::InteractionResponseConflict {
            turn_id: runtime_turn_id.to_string(),
            interaction_id: interaction_id.to_string(),
        });
    }
    if !crate::internal::ai::session::jsonl::has_exact_intent_revision_source_intent(
        &replay,
        replay.events.len(),
        &command,
        interaction_id,
    ) {
        return Err(RuntimeWorkerError::IndeterminateSideEffect(
            "IntentSpec revision review has no unique current durable command owner".to_string(),
        ));
    }
    let intent_id = exact_intent_review_lineage(&replay.events, interaction_id, runtime_turn_id)
        .map_err(|error| RuntimeWorkerError::IndeterminateSideEffect(error.to_string()))?;
    if intent_id != open_intent_id {
        return Err(RuntimeWorkerError::IndeterminateSideEffect(
            "IntentSpec revision review marker has conflicting durable intent identity".to_string(),
        ));
    }
    let intent_spec = load_persisted_web_phase0_intent_spec(persistence, &intent_id)
        .map_err(|error| RuntimeWorkerError::IndeterminateSideEffect(error.to_string()))?;
    if let Some(existing) = existing_prepared {
        if existing.intent_id != intent_id {
            return Err(RuntimeWorkerError::InteractionResponseConflict {
                turn_id: runtime_turn_id.to_string(),
                interaction_id: interaction_id.to_string(),
            });
        }
        return Ok(existing.sidecar_digest);
    }

    let has_hmac_commitment = workflow_has_intent_revision_hmac_commitment(&replay);
    let hmac_key = match load_intent_revision_hmac_key(persistence)
        .map_err(|error| RuntimeWorkerError::IndeterminateSideEffect(error.to_string()))?
    {
        Some(key) => key,
        None if !has_hmac_commitment => load_or_create_intent_revision_hmac_key(persistence)
            .map_err(|error| RuntimeWorkerError::IndeterminateSideEffect(error.to_string()))?,
        None => {
            return Err(RuntimeWorkerError::IndeterminateSideEffect(
                "IntentSpec revision HMAC key is missing after a durable revision commitment"
                    .to_string(),
            ));
        }
    };
    let sidecar_digest = intent_revision_sidecar_digest(
        interaction_id,
        &command,
        &intent_id,
        &intent_spec,
        note.as_deref(),
        &hmac_key,
    )
    .map_err(|error| RuntimeWorkerError::IndeterminateSideEffect(error.to_string()))?;
    let prepared = PreparedIntentRevision {
        schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
        interaction_id: interaction_id.to_string(),
        command,
        intent_id,
        note,
        sidecar_digest: sidecar_digest.clone(),
    };
    persist_prepared_intent_revision(persistence, &prepared)
        .map_err(|error| RuntimeWorkerError::IndeterminateSideEffect(error.to_string()))?;
    Ok(sidecar_digest)
}

fn intent_revision_authority_matches_terminal(
    authority: &PendingIntentRevisionAuthority,
    terminal: &IntentRevisionTerminalAuthority,
) -> bool {
    authority.schema_version == INTENT_REVISION_SIDECAR_SCHEMA_VERSION
        && authority.interaction_id == terminal.interaction_id
        && authority.command == terminal.command
        && authority.terminal_event_id == terminal.terminal_event_id
        && authority.terminal_sequence == terminal.terminal_sequence
        && authority.intent_id == terminal.intent_id
        && if authority.legacy_terminal {
            terminal.sidecar_digest.is_none()
        } else {
            terminal.sidecar_digest.is_some() && authority.sidecar_digest == terminal.sidecar_digest
        }
}

fn requested_intent_revision_digest(
    persistence: &HeadlessSessionPersistence,
    terminal: &IntentRevisionTerminalAuthority,
    requested_note: Option<&str>,
) -> anyhow::Result<String> {
    let intent_spec = load_persisted_web_phase0_intent_spec(persistence, &terminal.intent_id)?;
    let hmac_key = load_intent_revision_hmac_key(persistence)?.ok_or_else(|| {
        anyhow!("IntentSpec revision HMAC key is missing for a bound durable Modify terminal")
    })?;
    intent_revision_sidecar_digest(
        &terminal.interaction_id,
        &terminal.command,
        &terminal.intent_id,
        &intent_spec,
        requested_note,
        &hmac_key,
    )
}

pub(crate) fn verify_resolved_intent_revision_retry(
    persistence: &HeadlessSessionPersistence,
    interaction_id: &str,
    requested_note: Option<String>,
    allow_matching_prepared: bool,
) -> anyhow::Result<bool> {
    let requested_note = canonical_intent_revision_note_value(requested_note.as_deref())?;
    let replay = persistence
        .goal_event_store()
        .load_intent_revision_workflow_replay_committed()?;
    let binding =
        intent_revision_terminal_binding(&replay, interaction_id, None)?.ok_or_else(|| {
            anyhow!(
                "resolved IntentSpec Modify interaction has no exact durable terminal authority"
            )
        })?;
    let (terminal, legacy) = match binding {
        IntentRevisionTerminalBinding::Legacy(terminal) => (terminal, true),
        IntentRevisionTerminalBinding::Bound(terminal) => (terminal, false),
    };
    let receipt = exact_intent_revision_consumer(&replay, &terminal)?;
    let mut expected_digest = terminal.sidecar_digest.clone().or_else(|| {
        receipt
            .as_ref()
            .and_then(|receipt| receipt.claim.sidecar_digest.clone())
    });
    if let Some(sidecar) = load_intent_revision_sidecar(persistence)? {
        let active = match sidecar {
            LoadedIntentRevisionSidecar::Prepared(prepared)
                if prepared.interaction_id == interaction_id =>
            {
                if !allow_matching_prepared || !prepared_matches_terminal(&prepared, &terminal) {
                    return Err(anyhow!(
                        "resolved IntentSpec Modify still has an invalid unpromoted prepared sidecar"
                    ));
                }
                None
            }
            LoadedIntentRevisionSidecar::Active(active)
                if active
                    .authority
                    .as_ref()
                    .is_some_and(|authority| authority.interaction_id == interaction_id) =>
            {
                Some(active)
            }
            LoadedIntentRevisionSidecar::Claiming(claiming)
                if claiming
                    .active
                    .authority
                    .as_ref()
                    .is_some_and(|authority| authority.interaction_id == interaction_id) =>
            {
                Some(claiming.active)
            }
            LoadedIntentRevisionSidecar::Consuming(consuming)
                if consuming
                    .active
                    .authority
                    .as_ref()
                    .is_some_and(|authority| authority.interaction_id == interaction_id) =>
            {
                Some(consuming.active)
            }
            LoadedIntentRevisionSidecar::Prepared(_)
            | LoadedIntentRevisionSidecar::Active(PendingIntentRevision {
                authority: Some(_),
                ..
            })
            | LoadedIntentRevisionSidecar::Claiming(ClaimingIntentRevision {
                active:
                    PendingIntentRevision {
                        authority: Some(_), ..
                    },
                ..
            })
            | LoadedIntentRevisionSidecar::Consuming(ConsumingIntentRevision {
                active:
                    PendingIntentRevision {
                        authority: Some(_), ..
                    },
                ..
            }) if receipt.is_some() => None,
            LoadedIntentRevisionSidecar::Prepared(_)
            | LoadedIntentRevisionSidecar::Active(_)
            | LoadedIntentRevisionSidecar::Claiming(_)
            | LoadedIntentRevisionSidecar::Consuming(_) => {
                return Err(anyhow!(
                    "resolved IntentSpec Modify cannot authenticate the durable revision sidecar against an exact consumption receipt"
                ));
            }
        };
        if let Some(active) = active {
            let authenticated = authenticate_active_intent_revision(persistence, &replay, active)?;
            if authenticated.terminal.terminal_event_id != terminal.terminal_event_id
                || authenticated.terminal.terminal_sequence != terminal.terminal_sequence
            {
                return Err(anyhow!(
                    "active IntentSpec revision conflicts with its durable terminal authority"
                ));
            }
            let active_digest = authenticated
                .pending
                .authority
                .as_ref()
                .and_then(|authority| authority.sidecar_digest.clone());
            if let (Some(expected), Some(active_digest)) =
                (expected_digest.as_deref(), active_digest.as_deref())
                && expected != active_digest
            {
                return Err(anyhow!(
                    "active IntentSpec revision conflicts with its durable HMAC binding"
                ));
            }
            expected_digest = expected_digest.or(active_digest);
            if legacy && expected_digest.is_none() {
                // Baseline sidecars predate the keyed binding. While the raw
                // local sidecar still exists, its canonical body is the only
                // exact compatibility evidence available.
                return Ok(requested_note == authenticated.pending.note);
            }
        }
    }
    let Some(expected_digest) = expected_digest else {
        // A consumed baseline Modify without a receipt cannot prove any
        // historical note, including absence. Report a typed non-match to the
        // stale caller without fencing an otherwise healthy legacy session.
        return Ok(false);
    };
    let requested_digest =
        requested_intent_revision_digest(persistence, &terminal, requested_note.as_deref())?;
    Ok(requested_digest == expected_digest)
}

fn promote_prepared_intent_revision(
    persistence: &HeadlessSessionPersistence,
    interaction_id: &str,
    runtime_turn_id: &str,
    requested_note: Option<Option<&str>>,
) -> anyhow::Result<PendingIntentRevision> {
    let replay = persistence
        .goal_event_store()
        .load_intent_revision_workflow_replay_committed()?;
    let binding = intent_revision_terminal_binding(&replay, interaction_id, Some(runtime_turn_id))?
        .ok_or_else(|| {
            anyhow!(
                "IntentSpec revision has no exact durable Modify terminal to promote its sidecar"
            )
        })?;
    match binding {
        IntentRevisionTerminalBinding::Legacy(terminal) => {
            let requested_note = requested_note
                .map(canonical_intent_revision_note_value)
                .transpose()?;
            match load_intent_revision_sidecar(persistence)? {
                Some(LoadedIntentRevisionSidecar::Active(existing)) => {
                    let authenticated =
                        authenticate_active_intent_revision(persistence, &replay, existing)?;
                    if authenticated.terminal.terminal_event_id != terminal.terminal_event_id
                        || authenticated.terminal.terminal_sequence != terminal.terminal_sequence
                    {
                        return Err(anyhow!(
                            "legacy IntentSpec revision sidecar conflicts with its durable terminal"
                        ));
                    }
                    if let Some(requested_note) = requested_note {
                        if requested_note != authenticated.pending.note {
                            return Err(anyhow!(
                                "IntentSpec Modify retry conflicts with its durable legacy sidecar"
                            ));
                        }
                        if let Some(expected) = authenticated
                            .pending
                            .authority
                            .as_ref()
                            .and_then(|authority| authority.sidecar_digest.as_deref())
                        {
                            let actual = requested_intent_revision_digest(
                                persistence,
                                &terminal,
                                requested_note.as_deref(),
                            )?;
                            if actual != expected {
                                return Err(anyhow!(
                                    "IntentSpec Modify retry conflicts with its durable legacy binding"
                                ));
                            }
                        }
                    }
                    if authenticated.pending.authority.is_none() {
                        return Err(anyhow!(
                            "legacy IntentSpec revision sidecar was not durably lineage-bound"
                        ));
                    }
                    persist_pending_intent_revision(persistence, &authenticated.pending)?;
                    Ok(authenticated.pending)
                }
                Some(LoadedIntentRevisionSidecar::Prepared(_))
                | Some(LoadedIntentRevisionSidecar::Claiming(_))
                | Some(LoadedIntentRevisionSidecar::Consuming(_)) => Err(anyhow!(
                    "legacy IntentSpec Modify terminal conflicts with a non-active revision sidecar"
                )),
                None => {
                    if requested_note.is_some_and(|note| note.is_some()) {
                        return Err(anyhow!(
                            "legacy IntentSpec Modify terminal cannot recover a missing non-empty revision note"
                        ));
                    }
                    let intent_spec =
                        load_persisted_web_phase0_intent_spec(persistence, &terminal.intent_id)?;
                    let pending = PendingIntentRevision {
                        intent_spec,
                        note: None,
                        authority: Some(PendingIntentRevisionAuthority {
                            schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
                            legacy_terminal: true,
                            interaction_id: terminal.interaction_id,
                            command: terminal.command,
                            terminal_event_id: terminal.terminal_event_id,
                            terminal_sequence: terminal.terminal_sequence,
                            intent_id: terminal.intent_id,
                            sidecar_digest: None,
                        }),
                    };
                    persist_pending_intent_revision(persistence, &pending)?;
                    Ok(pending)
                }
            }
        }
        IntentRevisionTerminalBinding::Bound(terminal) => {
            if let Some(note) = requested_note {
                let note = canonical_intent_revision_note_value(note)?;
                let requested_digest =
                    requested_intent_revision_digest(persistence, &terminal, note.as_deref())?;
                if terminal
                    .sidecar_digest
                    .as_deref()
                    .is_none_or(|expected| requested_digest != expected)
                {
                    return Err(anyhow!(
                        "IntentSpec Modify retry conflicts with its durable prepared sidecar"
                    ));
                }
            }
            match load_intent_revision_sidecar(persistence)? {
                Some(LoadedIntentRevisionSidecar::Prepared(prepared)) => {
                    if prepared.interaction_id != terminal.interaction_id
                        || prepared.command != terminal.command
                        || prepared.intent_id != terminal.intent_id
                        || terminal
                            .sidecar_digest
                            .as_deref()
                            .is_none_or(|expected| prepared.sidecar_digest != expected)
                    {
                        return Err(anyhow!(
                            "prepared IntentSpec revision conflicts with its durable terminal authority"
                        ));
                    }
                    let intent_spec = validate_prepared_intent_revision(persistence, &prepared)?;
                    let authority = PendingIntentRevisionAuthority {
                        schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
                        legacy_terminal: false,
                        interaction_id: terminal.interaction_id,
                        command: terminal.command,
                        terminal_event_id: terminal.terminal_event_id,
                        terminal_sequence: terminal.terminal_sequence,
                        intent_id: terminal.intent_id,
                        sidecar_digest: terminal.sidecar_digest,
                    };
                    let pending = PendingIntentRevision {
                        intent_spec,
                        note: prepared.note,
                        authority: Some(authority),
                    };
                    persist_pending_intent_revision(persistence, &pending)?;
                    Ok(pending)
                }
                Some(LoadedIntentRevisionSidecar::Active(existing)) => {
                    let authenticated =
                        authenticate_active_intent_revision(persistence, &replay, existing)?;
                    if authenticated.terminal.terminal_event_id != terminal.terminal_event_id
                        || authenticated.terminal.terminal_sequence != terminal.terminal_sequence
                    {
                        return Err(anyhow!(
                            "active IntentSpec revision conflicts with its durable terminal authority"
                        ));
                    }
                    Ok(authenticated.pending)
                }
                Some(LoadedIntentRevisionSidecar::Claiming(_))
                | Some(LoadedIntentRevisionSidecar::Consuming(_)) => Err(anyhow!(
                    "bound IntentSpec Modify terminal is already crossing its consumption boundary"
                )),
                None => Err(anyhow!(
                    "bound IntentSpec Modify terminal is missing its prepared revision sidecar"
                )),
            }
        }
    }
}

fn persist_pending_intent_revision(
    persistence: &HeadlessSessionPersistence,
    pending: &PendingIntentRevision,
) -> anyhow::Result<()> {
    validate_active_intent_revision(persistence, pending)?;
    let envelope = IntentRevisionSidecarEnvelope {
        intent_spec: pending.intent_spec.clone(),
        note: pending.note.clone(),
        authority: pending.authority.clone(),
        prepared: None,
        claiming: None,
        consuming: None,
    };
    persist_intent_revision_sidecar_envelope(persistence, &envelope)
}

fn persist_prepared_intent_revision(
    persistence: &HeadlessSessionPersistence,
    prepared: &PreparedIntentRevision,
) -> anyhow::Result<()> {
    validate_prepared_intent_revision(persistence, prepared)?;
    // The legacy-visible fields are intentionally invalid. Readers predating
    // the prepared envelope ignore `prepared`, reject the empty intentSpec,
    // and fail closed instead of activating an uncommitted revision.
    let envelope = IntentRevisionSidecarEnvelope {
        intent_spec: String::new(),
        note: None,
        authority: None,
        prepared: Some(prepared.clone()),
        claiming: None,
        consuming: None,
    };
    persist_intent_revision_sidecar_envelope(persistence, &envelope)
}

fn validate_claiming_intent_revision(
    persistence: &HeadlessSessionPersistence,
    claiming: &ClaimingIntentRevision,
) -> anyhow::Result<()> {
    validate_active_intent_revision(persistence, &claiming.active)?;
    if claiming.schema_version != INTENT_REVISION_SIDECAR_SCHEMA_VERSION
        || !crate::internal::ai::session::jsonl::intent_revision_consumption_claim_is_valid(
            &claiming.claim,
        )
        || pending_consumption_binding(&claiming.active, claiming.claim.consumer_intent.clone())?
            != claiming.claim
    {
        return Err(anyhow!(
            "claiming IntentSpec revision does not match its lineage-bound active sidecar"
        ));
    }
    Ok(())
}

fn persist_claiming_intent_revision(
    persistence: &HeadlessSessionPersistence,
    claiming: &ClaimingIntentRevision,
) -> anyhow::Result<()> {
    validate_claiming_intent_revision(persistence, claiming)?;
    // Claiming is private crash-recovery state. Baseline readers ignore the
    // nested field and reject the deliberately empty legacy IntentSpec rather
    // than treating a pre-admission claim as an Active revision.
    let envelope = IntentRevisionSidecarEnvelope {
        intent_spec: String::new(),
        note: None,
        authority: None,
        prepared: None,
        claiming: Some(claiming.clone()),
        consuming: None,
    };
    persist_intent_revision_sidecar_envelope(persistence, &envelope)
}

pub(crate) fn prepare_claiming_intent_revision(
    persistence: &HeadlessSessionPersistence,
    pending: PendingIntentRevision,
    consumer_intent: CodeCommandIntent,
) -> anyhow::Result<(PendingIntentRevision, IntentRevisionConsumptionClaim)> {
    let replay = persistence
        .goal_event_store()
        .load_intent_revision_workflow_replay_committed()?;
    match load_intent_revision_sidecar(persistence)? {
        Some(LoadedIntentRevisionSidecar::Active(existing)) if existing == pending => {}
        Some(LoadedIntentRevisionSidecar::Consuming(existing)) if existing.active == pending => {
            let authenticated =
                authenticate_active_intent_revision(persistence, &replay, existing.active.clone())?;
            if authenticated.pending != pending
                || exact_intent_revision_consumer(&replay, &authenticated.terminal)?.is_some()
                || pending_consumption_binding(
                    &authenticated.pending,
                    existing.consumption.claim.consumer_intent.clone(),
                )? != existing.consumption.claim
                || matches!(
                    validate_uncommitted_intent_revision_consumer(
                        persistence,
                        &replay,
                        &authenticated.terminal,
                        &existing.consumption,
                    )?,
                    CodeCommandStatus::Pending | CodeCommandStatus::Succeeded { .. }
                )
            {
                return Err(anyhow!(
                    "IntentSpec revision retry cannot replace a live or consumed durable consumer"
                ));
            }
        }
        Some(LoadedIntentRevisionSidecar::Active(_))
        | Some(LoadedIntentRevisionSidecar::Prepared(_))
        | Some(LoadedIntentRevisionSidecar::Claiming(_))
        | Some(LoadedIntentRevisionSidecar::Consuming(_))
        | None => {
            return Err(anyhow!(
                "IntentSpec revision consumer claim does not replace its exact Active or recoverable Consuming sidecar"
            ));
        }
    }
    let pending = ensure_legacy_intent_revision_digest_before_consumption(persistence, pending)?;
    let claim = pending_consumption_binding(&pending, consumer_intent)?;
    let claiming = ClaimingIntentRevision {
        schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
        active: pending.clone(),
        claim: claim.clone(),
    };
    persist_claiming_intent_revision(persistence, &claiming)?;
    Ok((pending, claim))
}

pub(crate) fn promote_claiming_intent_revision_after_admission(
    persistence: &HeadlessSessionPersistence,
    active: &PendingIntentRevision,
    claim: &IntentRevisionConsumptionClaim,
) -> anyhow::Result<IntentRevisionConsumption> {
    let claiming = ClaimingIntentRevision {
        schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
        active: active.clone(),
        claim: claim.clone(),
    };
    validate_claiming_intent_revision(persistence, &claiming)?;
    match load_intent_revision_sidecar(persistence)? {
        Some(LoadedIntentRevisionSidecar::Claiming(existing)) if existing == claiming => {}
        Some(LoadedIntentRevisionSidecar::Consuming(existing))
            if existing.active == *active && existing.consumption.claim == *claim =>
        {
            return Ok(existing.consumption);
        }
        _ => {
            return Err(anyhow!(
                "IntentSpec revision consumer claim changed before Runtime admission completed"
            ));
        }
    }
    let consumption = persistence
        .goal_event_store()
        .prepare_intent_revision_consumption(&claim.consumer_intent, claim)?;
    persist_consuming_intent_revision(
        persistence,
        &ConsumingIntentRevision {
            schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
            active: active.clone(),
            consumption: consumption.clone(),
        },
    )?;
    Ok(consumption)
}

/// Roll back only a Claiming state whose exact command intent provably never
/// reached the durable workflow. Any observed row (or a changed sidecar) is
/// left intact for startup reconciliation instead of guessing across an
/// ambiguous Runtime error.
pub(crate) fn rearm_unadmitted_claiming_intent_revision(
    persistence: &HeadlessSessionPersistence,
    active: &PendingIntentRevision,
    claim: &IntentRevisionConsumptionClaim,
) -> anyhow::Result<bool> {
    let expected = ClaimingIntentRevision {
        schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
        active: active.clone(),
        claim: claim.clone(),
    };
    if !matches!(
        load_intent_revision_sidecar(persistence)?,
        Some(LoadedIntentRevisionSidecar::Claiming(existing)) if existing == expected
    ) {
        return Ok(false);
    }
    match persistence
        .goal_event_store()
        .code_command_intent_status(&claim.consumer_intent.identity)?
    {
        None => {
            persist_pending_intent_revision(persistence, active)?;
            Ok(true)
        }
        Some((actual, _)) if actual == claim.consumer_intent => Ok(false),
        Some(_) => Err(anyhow!(
            "IntentSpec revision consumer command identity conflicts with its durable claim"
        )),
    }
}

pub(crate) fn rearm_cancelled_intent_revision_consumer(
    persistence: &HeadlessSessionPersistence,
    active: &PendingIntentRevision,
    claim: &IntentRevisionConsumptionClaim,
) -> anyhow::Result<()> {
    let replay = persistence
        .goal_event_store()
        .load_intent_revision_workflow_replay_committed()?;
    match load_intent_revision_sidecar(persistence)? {
        Some(LoadedIntentRevisionSidecar::Claiming(existing))
            if existing.active == *active && existing.claim == *claim =>
        {
            match reconcile_claiming_intent_revision(persistence, &replay, existing)? {
                ClaimingIntentRevisionRecovery::Rearmed(_)
                | ClaimingIntentRevisionRecovery::Consuming { .. } => Ok(()),
                ClaimingIntentRevisionRecovery::Consumed => Err(anyhow!(
                    "cancelled IntentSpec revision claim unexpectedly crossed its consumption boundary"
                )),
            }
        }
        Some(LoadedIntentRevisionSidecar::Consuming(existing))
            if existing.active == *active && existing.consumption.claim == *claim =>
        {
            let (resolved, status) = persistence
                .goal_event_store()
                .resolve_claimed_intent_revision_consumption(claim)?;
            let recoverable = matches!(status, CodeCommandStatus::Pending)
                || matches!(
                    status,
                    CodeCommandStatus::Failed { ref reason }
                        if reason
                            == crate::internal::ai::session::jsonl::PRE_MUTATION_CANCELLED_COMMAND_REASON
                );
            if resolved != existing.consumption || !recoverable {
                return Err(anyhow!(
                    "cancelled IntentSpec revision consumer has non-canonical durable state"
                ));
            }
            // Preserve the event-id/sequence attribution on disk. The caller
            // keeps logical Active in memory, and a later exact retry may
            // replace this safely terminal Consuming envelope with Claiming.
            Ok(())
        }
        _ => Err(anyhow!(
            "cancelled IntentSpec revision consumer lost its exact durable sidecar binding"
        )),
    }
}

fn persist_consuming_intent_revision(
    persistence: &HeadlessSessionPersistence,
    consuming: &ConsumingIntentRevision,
) -> anyhow::Result<()> {
    validate_active_intent_revision(persistence, &consuming.active)?;
    if consuming.schema_version != INTENT_REVISION_SIDECAR_SCHEMA_VERSION
        || pending_consumption_binding(
            &consuming.active,
            consuming.consumption.claim.consumer_intent.clone(),
        )? != consuming.consumption.claim
    {
        return Err(anyhow!(
            "consuming IntentSpec revision does not match its lineage-bound active sidecar"
        ));
    }
    // Like Prepared, Consuming is deliberately invalid to baseline readers.
    // They ignore the nested field, reject the empty legacy intentSpec, and
    // fail closed across the receipt-to-unlink crash window.
    let envelope = IntentRevisionSidecarEnvelope {
        intent_spec: String::new(),
        note: None,
        authority: None,
        prepared: None,
        claiming: None,
        consuming: Some(consuming.clone()),
    };
    persist_intent_revision_sidecar_envelope(persistence, &envelope)
}

fn persist_intent_revision_sidecar_envelope(
    persistence: &HeadlessSessionPersistence,
    envelope: &IntentRevisionSidecarEnvelope,
) -> anyhow::Result<()> {
    let path = pending_intent_revision_path(persistence);
    let body = serde_json::to_vec_pretty(envelope).map_err(|error| {
        anyhow!("failed to serialize pending IntentSpec revision state: {error}")
    })?;
    if body.len() as u64 > MAX_PENDING_INTENT_REVISION_BYTES {
        return Err(anyhow!(
            "pending IntentSpec revision sidecar exceeds the {MAX_PENDING_INTENT_REVISION_BYTES}-byte limit"
        ));
    }
    crate::utils::atomic_write::write_atomic(&path, &body, true).map_err(|error| {
        anyhow!(
            "failed to persist pending IntentSpec revision to {}: {error}",
            path.display()
        )
    })?;
    Ok(())
}

fn load_intent_revision_sidecar(
    persistence: &HeadlessSessionPersistence,
) -> anyhow::Result<Option<LoadedIntentRevisionSidecar>> {
    let path = pending_intent_revision_path(persistence);
    let Some((mut file, metadata)) =
        open_intent_revision_file_no_follow(&path, "pending IntentSpec revision")?
    else {
        return Ok(None);
    };
    if metadata.len() > MAX_PENDING_INTENT_REVISION_BYTES {
        return Err(anyhow!(
            "pending IntentSpec revision at {} exceeds the {MAX_PENDING_INTENT_REVISION_BYTES}-byte limit",
            path.display()
        ));
    }
    let mut body = String::new();
    file.by_ref()
        .take(MAX_PENDING_INTENT_REVISION_BYTES + 1)
        .read_to_string(&mut body)
        .map_err(|error| {
            anyhow!(
                "failed to reload pending IntentSpec revision from {}: {error}",
                path.display()
            )
        })?;
    if body.len() as u64 > MAX_PENDING_INTENT_REVISION_BYTES {
        return Err(anyhow!(
            "pending IntentSpec revision at {} exceeds the {MAX_PENDING_INTENT_REVISION_BYTES}-byte limit",
            path.display()
        ));
    }
    let envelope: IntentRevisionSidecarEnvelope = serde_json::from_str(&body).map_err(|error| {
        anyhow!(
            "pending IntentSpec revision at {} is invalid: {error}",
            path.display()
        )
    })?;
    sync_open_intent_revision_file_and_parent(&file, &path, "pending IntentSpec revision")?;
    let nested_state_count = usize::from(envelope.prepared.is_some())
        + usize::from(envelope.claiming.is_some())
        + usize::from(envelope.consuming.is_some());
    if nested_state_count > 1 {
        return Err(anyhow!(
            "IntentSpec revision sidecar mixes prepared, claiming, or consuming states"
        ));
    }
    if let Some(prepared) = envelope.prepared {
        if !envelope.intent_spec.is_empty()
            || envelope.note.is_some()
            || envelope.authority.is_some()
        {
            return Err(anyhow!(
                "prepared IntentSpec revision at {} mixes dormant and active fields",
                path.display()
            ));
        }
        validate_prepared_intent_revision(persistence, &prepared)?;
        return Ok(Some(LoadedIntentRevisionSidecar::Prepared(prepared)));
    }
    if let Some(claiming) = envelope.claiming {
        if !envelope.intent_spec.is_empty()
            || envelope.note.is_some()
            || envelope.authority.is_some()
            || claiming.schema_version != INTENT_REVISION_SIDECAR_SCHEMA_VERSION
        {
            return Err(anyhow!(
                "claiming IntentSpec revision at {} mixes invalid envelope fields",
                path.display()
            ));
        }
        validate_claiming_intent_revision(persistence, &claiming).map_err(|error| {
            anyhow!(
                "claiming IntentSpec revision at {} conflicts with its active authority: {error}",
                path.display()
            )
        })?;
        return Ok(Some(LoadedIntentRevisionSidecar::Claiming(claiming)));
    }
    if let Some(consuming) = envelope.consuming {
        if !envelope.intent_spec.is_empty()
            || envelope.note.is_some()
            || envelope.authority.is_some()
            || consuming.schema_version != INTENT_REVISION_SIDECAR_SCHEMA_VERSION
        {
            return Err(anyhow!(
                "consuming IntentSpec revision at {} mixes invalid envelope fields",
                path.display()
            ));
        }
        validate_active_intent_revision(persistence, &consuming.active)?;
        if pending_consumption_binding(
            &consuming.active,
            consuming.consumption.claim.consumer_intent.clone(),
        )? != consuming.consumption.claim
        {
            return Err(anyhow!(
                "consuming IntentSpec revision at {} conflicts with its active authority",
                path.display()
            ));
        }
        return Ok(Some(LoadedIntentRevisionSidecar::Consuming(consuming)));
    }
    let pending = PendingIntentRevision {
        intent_spec: envelope.intent_spec,
        note: envelope.note,
        authority: envelope.authority,
    };
    validate_active_intent_revision(persistence, &pending)?;
    Ok(Some(LoadedIntentRevisionSidecar::Active(pending)))
}

fn load_pending_intent_revision(
    persistence: &HeadlessSessionPersistence,
) -> anyhow::Result<Option<PendingIntentRevision>> {
    match load_intent_revision_sidecar(persistence)? {
        None => Ok(None),
        Some(LoadedIntentRevisionSidecar::Active(pending)) => Ok(Some(pending)),
        Some(LoadedIntentRevisionSidecar::Prepared(_)) => Err(anyhow!(
            "IntentSpec revision is prepared but has no lineage-validated terminal promotion"
        )),
        Some(LoadedIntentRevisionSidecar::Claiming(_)) => Err(anyhow!(
            "IntentSpec revision consumer claim requires startup reconciliation"
        )),
        Some(LoadedIntentRevisionSidecar::Consuming(_)) => Err(anyhow!(
            "IntentSpec revision consume boundary requires startup reconciliation"
        )),
    }
}

fn clear_pending_intent_revision(persistence: &HeadlessSessionPersistence) -> anyhow::Result<()> {
    let path = pending_intent_revision_path(persistence);
    crate::utils::atomic_write::remove_durably(&path).map_err(|error| {
        anyhow!(
            "failed to clear pending IntentSpec revision at {}: {error}",
            path.display()
        )
    })?;
    Ok(())
}

fn intent_review_choice_interaction(
    interaction_id: String,
    metadata: serde_json::Value,
) -> CodeUiInteractionRequest {
    CodeUiInteractionRequest {
        id: interaction_id,
        kind: CodeUiInteractionKind::IntentReviewChoice,
        title: Some("Review IntentSpec".to_string()),
        description: Some(
            "Confirm this IntentSpec before Libra generates an execution plan.".to_string(),
        ),
        prompt: None,
        options: vec![
            CodeUiInteractionOption {
                id: "confirm".to_string(),
                label: "Confirm".to_string(),
                description: Some(
                    "Accept the IntentSpec draft (Phase 1 plan generation remains GATE-WEB-PLAN)"
                        .to_string(),
                ),
            },
            CodeUiInteractionOption {
                id: "modify".to_string(),
                label: "Modify".to_string(),
                description: Some(
                    "Enter revise mode — your next plain message updates this IntentSpec"
                        .to_string(),
                ),
            },
            CodeUiInteractionOption {
                id: "cancel".to_string(),
                label: "Cancel".to_string(),
                description: Some("Leave the IntentSpec in place and stop".to_string()),
            },
        ],
        status: CodeUiInteractionStatus::Pending,
        metadata,
        requested_at: Utc::now(),
        resolved_at: None,
    }
}

fn phase1_code_ui_plan(context: &Phase1ReviewContext) -> CodeUiPlanSnapshot {
    CodeUiPlanSnapshot {
        id: context.interaction_id.clone(),
        title: Some("Execution Plan".to_string()),
        summary: context.plan_draft.explanation.clone(),
        status: "ready".to_string(),
        steps: context
            .plan_draft
            .steps
            .iter()
            .map(|step| CodeUiPlanStep {
                step: step.title.clone(),
                status: "pending".to_string(),
            })
            .collect(),
        updated_at: Utc::now(),
    }
}

fn phase1_plan_review_interaction(
    context: &Phase1ReviewContext,
    restored: bool,
    workspace_warning: Option<&str>,
) -> CodeUiInteractionRequest {
    CodeUiInteractionRequest {
        id: context.interaction_id.clone(),
        kind: CodeUiInteractionKind::PostPlanChoice,
        title: Some("Choose next step".to_string()),
        description: Some(workspace_warning.map_or_else(
            || {
                "The plan is ready. Execute it, cancel it, or choose Modify and send the requested changes as your next plain message."
                    .to_string()
            },
            str::to_string,
        )),
        prompt: None,
        options: vec![
            CodeUiInteractionOption {
                id: "execute".to_string(),
                label: "Execute Plan".to_string(),
                description: Some("Confirm the plan and choose network policy".to_string()),
            },
            CodeUiInteractionOption {
                id: "modify".to_string(),
                label: "Modify Plan".to_string(),
                description: Some("Revise the execution plan".to_string()),
            },
            CodeUiInteractionOption {
                id: "cancel".to_string(),
                label: "Cancel".to_string(),
                description: Some("Leave the plan in place and stop".to_string()),
            },
        ],
        status: CodeUiInteractionStatus::Pending,
        metadata: serde_json::json!({
            "intentId": context.intent_id,
            "planId": context.plan_id(),
            "networkAccess": context.default_allow_network,
            "modifyUsesNextMessage": true,
            "restored": restored,
            "workspaceDrifted": workspace_warning.is_some(),
            "workspaceWarning": workspace_warning,
        }),
        requested_at: Utc::now(),
        resolved_at: None,
    }
}

fn phase1_network_policy_interaction(
    context: &Phase1ReviewContext,
    interaction_id: &str,
    restored: bool,
    workspace_warning: Option<&str>,
) -> CodeUiInteractionRequest {
    CodeUiInteractionRequest {
        id: interaction_id.to_string(),
        kind: CodeUiInteractionKind::PostPlanChoice,
        title: Some("Choose network policy".to_string()),
        description: Some(workspace_warning.map_or_else(
            || "Select whether shell tools and gates may use the network.".to_string(),
            |warning| {
                format!("{warning} Choose Back to return to Plan review before regenerating.")
            },
        )),
        prompt: None,
        options: vec![
            CodeUiInteractionOption {
                id: "network-deny".to_string(),
                label: "Network: Deny".to_string(),
                description: Some("Run shell/gates offline".to_string()),
            },
            CodeUiInteractionOption {
                id: "network-allow".to_string(),
                label: "Network: Allow".to_string(),
                description: Some("Allow network for shell/gates".to_string()),
            },
            CodeUiInteractionOption {
                id: "back".to_string(),
                label: "Back".to_string(),
                description: Some("Return to plan choices".to_string()),
            },
        ],
        status: CodeUiInteractionStatus::Pending,
        metadata: serde_json::json!({
            "intentId": context.intent_id,
            "planId": context.plan_id(),
            "networkAccess": context.default_allow_network,
            "phase": "networkPolicy",
            "restored": restored,
            "workspaceDrifted": workspace_warning.is_some(),
            "workspaceWarning": workspace_warning,
        }),
        requested_at: Utc::now(),
        resolved_at: None,
    }
}

fn intent_review_decision_from_response(
    interaction: &crate::internal::ai::runtime::InteractionResponse,
) -> Result<IntentReviewDecision, RuntimeWorkerError> {
    let code_ui_response = decode_headless_interaction_response(interaction)?;
    code_ui_response
        .selected_option
        .as_deref()
        .and_then(IntentReviewDecision::from_wire_id)
        .or_else(|| IntentReviewDecision::from_wire_id(&interaction.response))
        .ok_or_else(|| {
            RuntimeWorkerError::ExecutionFailed(format!(
                "unrecognized IntentSpec review response; expected confirm/modify/cancel (got selected_option={:?})",
                code_ui_response.selected_option
            ))
        })
}

/// Canonical bounded representation retained only in the private revision
/// sidecar. The workflow terminal stores a non-sensitive HMAC binding, never
/// the raw note; one normalizer keeps live and recovered comparisons exact.
pub(crate) fn canonical_intent_revision_note(
    response: &CodeUiInteractionResponse,
) -> Result<Option<String>, RuntimeWorkerError> {
    canonical_intent_revision_note_value(response.note.as_deref())
}

fn canonical_intent_revision_note_value(
    note: Option<&str>,
) -> Result<Option<String>, RuntimeWorkerError> {
    let Some(note) = note.map(str::trim) else {
        return Ok(None);
    };
    if note.is_empty() {
        return Ok(None);
    }
    if note.len() > MAX_INTENT_REVISION_NOTE_BYTES {
        return Err(RuntimeWorkerError::InvalidInteractionResponse(format!(
            "IntentSpec Modify note exceeds the {MAX_INTENT_REVISION_NOTE_BYTES}-byte UTF-8 limit"
        )));
    }
    Ok(Some(note.to_string()))
}

fn intent_revision_recovery_for_response(
    expected_interaction_id: &str,
    interaction: &crate::internal::ai::runtime::InteractionResponse,
) -> Option<IntentRevisionRecovery> {
    if interaction.interaction_id != expected_interaction_id {
        return None;
    }
    if intent_review_decision_from_response(interaction).ok()? != IntentReviewDecision::Revise {
        return None;
    }
    let sidecar_digest = interaction.intent_revision_sidecar_digest()?.to_string();
    crate::internal::ai::session::jsonl::is_canonical_intent_revision_digest(&sidecar_digest).then(
        || IntentRevisionRecovery {
            interaction_id: interaction.interaction_id.clone(),
            sidecar_digest,
        },
    )
}

fn resolve_web_phase0_intent_draft(
    draft_json: &str,
    working_dir: &std::path::Path,
    selected_risk: Option<crate::internal::ai::intentspec::RiskLevel>,
) -> anyhow::Result<(String, String, crate::internal::ai::intentspec::IntentSpec)> {
    use crate::internal::ai::{
        intentspec::{ResolveContext, resolve_intentspec},
        tools::handlers::submit_intent_draft::parse_submit_intent_draft_value,
    };

    let draft_value: serde_json::Value = serde_json::from_str(draft_json)
        .map_err(|error| anyhow!("submitted IntentDraft JSON is invalid: {error}"))?;
    let args = parse_submit_intent_draft_value(&draft_value)
        .map_err(|error| anyhow!("submitted IntentDraft could not be parsed: {error}"))?;
    let draft_risk = args.draft.risk.level.clone();
    let risk_level = match (selected_risk, draft_risk) {
        (Some(user_risk), Some(model_risk)) if user_risk != model_risk => {
            return Err(anyhow!(
                "risk_profile selection ({user_risk:?}) does not match IntentDraft.risk.level ({model_risk:?})"
            ));
        }
        (Some(user_risk), _) => user_risk,
        (None, _) => {
            return Err(anyhow!(
                "Phase 0 requires a completed risk_profile selection before IntentSpec review"
            ));
        }
    };
    let spec = resolve_intentspec(
        args.draft,
        risk_level,
        ResolveContext {
            working_dir: working_dir.display().to_string(),
            base_ref: "HEAD".to_string(),
            created_by_id: "web-headless".to_string(),
        },
    );
    let intent_id = spec.metadata.id.clone();
    let spec_json = serde_json::to_string_pretty(&spec)
        .map_err(|error| anyhow!("resolved IntentSpec could not be serialized: {error}"))?;
    Ok((intent_id, spec_json, spec))
}

async fn persist_web_phase0_intent_before_review(
    persistence: Option<&HeadlessSessionPersistence>,
    mcp_server: Option<&Arc<crate::internal::ai::mcp::server::LibraMcpServer>>,
    spec: &crate::internal::ai::intentspec::IntentSpec,
    fallback_intent_id: String,
) -> anyhow::Result<String> {
    let mut intent_id = fallback_intent_id;
    if let Some(mcp_server) = mcp_server {
        match crate::internal::ai::runtime::phase0::write_intent(spec, mcp_server).await {
            Ok(outcome) => {
                intent_id = outcome.intent_id;
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "MCP write_intent failed for web Phase 0; falling back to session-root IntentSpec persistence"
                );
            }
        }
    }
    validate_durable_intent_id(&intent_id)?;
    // Always mirror a session-root copy when persistence is available so
    // resume can reload Confirm/Modify/Cancel after a crash even when the
    // formal MCP write succeeded (resume does not talk to MCP).
    if let Some(persistence) = persistence {
        let intents_dir = persistence
            .goal_event_store()
            .session_root()
            .join("intents");
        let path = intents_dir.join(format!("{intent_id}.json"));
        let body = serde_json::to_vec_pretty(spec).map_err(|error| {
            anyhow!("failed to serialize IntentSpec for durable web persistence: {error}")
        })?;
        if body.len() as u64 > MAX_DURABLE_INTENT_SPEC_BYTES {
            return Err(anyhow!(
                "resolved IntentSpec exceeds the {MAX_DURABLE_INTENT_SPEC_BYTES}-byte durable storage limit"
            ));
        }
        // Recovery-critical: resume reloads this file for Confirm/Modify/Cancel.
        crate::utils::atomic_write::write_atomic(&path, &body, true).map_err(|error| {
            anyhow!(
                "failed to persist IntentSpec to {}: {error}",
                path.display()
            )
        })?;
        return Ok(intent_id);
    }
    // Ephemeral unit tests without SessionStore still park an in-memory gate.
    Ok(intent_id)
}

fn validate_durable_intent_id(intent_id: &str) -> anyhow::Result<()> {
    use std::path::{Component, Path};

    let mut components = Path::new(intent_id).components();
    let one_normal_component = matches!(components.next(), Some(Component::Normal(component))
        if component == std::ffi::OsStr::new(intent_id))
        && components.next().is_none();
    if intent_id.is_empty()
        || intent_id.len() > MAX_DURABLE_INTENT_ID_BYTES
        || intent_id == "."
        || intent_id == ".."
        || intent_id.contains('/')
        || intent_id.contains('\\')
        || intent_id.contains('\0')
        || intent_id.chars().any(char::is_control)
        || !one_normal_component
    {
        return Err(anyhow!(
            "durable IntentSpec id must be one safe filename component of at most {MAX_DURABLE_INTENT_ID_BYTES} bytes"
        ));
    }
    Ok(())
}

fn interaction_metadata_has_intent_spec(metadata: &serde_json::Value) -> bool {
    metadata
        .get("intentSpec")
        .and_then(|value| value.as_str())
        .is_some_and(|spec| !spec.trim().is_empty())
        || metadata
            .get("intentSpec")
            .is_some_and(|value| value.is_object())
}

fn load_persisted_web_phase0_intent_spec(
    persistence: &HeadlessSessionPersistence,
    intent_id: &str,
) -> anyhow::Result<String> {
    validate_durable_intent_id(intent_id)?;
    let path = persistence
        .goal_event_store()
        .session_root()
        .join("intents")
        .join(format!("{intent_id}.json"));
    let file = File::open(&path).map_err(|error| {
        anyhow!(
            "failed to reload durable IntentSpec from {}: {error}",
            path.display()
        )
    })?;
    let metadata = file.metadata().map_err(|error| {
        anyhow!(
            "failed to inspect durable IntentSpec at {}: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(anyhow!(
            "durable IntentSpec at {} is not a regular file",
            path.display()
        ));
    }
    if metadata.len() > MAX_DURABLE_INTENT_SPEC_BYTES {
        return Err(anyhow!(
            "durable IntentSpec at {} exceeds the {MAX_DURABLE_INTENT_SPEC_BYTES}-byte limit",
            path.display()
        ));
    }
    let mut body = String::new();
    file.take(MAX_DURABLE_INTENT_SPEC_BYTES + 1)
        .read_to_string(&mut body)
        .map_err(|error| {
            anyhow!(
                "failed to reload durable IntentSpec from {}: {error}",
                path.display()
            )
        })?;
    if body.len() as u64 > MAX_DURABLE_INTENT_SPEC_BYTES {
        return Err(anyhow!(
            "durable IntentSpec at {} exceeds the {MAX_DURABLE_INTENT_SPEC_BYTES}-byte limit",
            path.display()
        ));
    }
    let parsed: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
        anyhow!(
            "durable IntentSpec at {} is not valid JSON: {error}",
            path.display()
        )
    })?;
    // Round-trip through IntentSpec so a corrupt/truncated file cannot open a
    // review gate with opaque JSON the user cannot meaningfully approve.
    let _spec: crate::internal::ai::intentspec::IntentSpec = serde_json::from_value(parsed)
        .map_err(|error| {
            anyhow!(
                "durable IntentSpec at {} failed schema validation: {error}",
                path.display()
            )
        })?;
    crate::utils::atomic_write::sync_file_and_parent_durably_with_pre_parent_sync_hook(
        &path,
        || Ok(()),
    )
    .map_err(|error| {
        anyhow!(
            "failed to durably re-sync IntentSpec {} before using it as revision lineage: {error}",
            path.display()
        )
    })?;
    Ok(body)
}

fn restored_intent_review_metadata(
    persistence: &HeadlessSessionPersistence,
    intent_id: &str,
    phase0_turn_id: &str,
) -> anyhow::Result<serde_json::Value> {
    let spec_json = load_persisted_web_phase0_intent_spec(persistence, intent_id)?;
    Ok(serde_json::json!({
        "restored": true,
        "phase0TurnId": phase0_turn_id,
        "intentId": intent_id,
        "intentSpec": spec_json,
    }))
}

fn extract_risk_level_from_user_input(
    resp: &UserInputResponse,
) -> Option<crate::internal::ai::intentspec::RiskLevel> {
    use crate::internal::ai::intentspec::RiskLevel;
    // Only the Phase 0 risk_profile question is authoritative. Scanning every
    // follow-up answer would let unrelated text (e.g. "medium priority")
    // overwrite the user's earlier Low/Medium/High selection.
    let answer = resp.answers.get("risk_profile")?;
    for item in &answer.answers {
        match item.trim().to_ascii_lowercase().as_str() {
            "low" => return Some(RiskLevel::Low),
            "medium" => return Some(RiskLevel::Medium),
            "high" => return Some(RiskLevel::High),
            _ => {}
        }
    }
    None
}

fn decode_headless_interaction_response(
    interaction: &crate::internal::ai::runtime::InteractionResponse,
) -> Result<CodeUiInteractionResponse, RuntimeWorkerError> {
    serde_json::from_str(&interaction.response).map_err(|error| {
        RuntimeWorkerError::ExecutionFailed(format!(
            "headless interaction response could not be decoded: {error}"
        ))
    })
}

async fn deliver_headless_exec_approval_response(
    session: &Arc<CodeUiSession>,
    interaction_persistence_failed: &Arc<AtomicBool>,
    persistence: Option<&HeadlessSessionPersistence>,
    interaction_id: &str,
    request: ExecApprovalRequest,
    response: CodeUiInteractionResponse,
) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
    let decision = review_decision_from_interaction_response(response)
        .map_err(|error| RuntimeWorkerError::ExecutionFailed(error.to_string()))?;
    session.resolve_interaction(interaction_id).await;
    session.set_status(CodeUiSessionStatus::ExecutingTool).await;
    if let Err(error) = persist_headless_interaction_snapshot(persistence, session).await {
        interaction_persistence_failed.store(true, Ordering::Release);
        mark_persistence_failure(
            session,
            "failed to persist resolved exec approval interaction",
            error,
        )
        .await;
        return Err(RuntimeWorkerError::ExecutionFailed(
            "unable to persist the approval response; no tool action was started".to_string(),
        ));
    }
    if request.response_tx.send(decision).is_err() {
        set_status_if_recoverable(session, CodeUiSessionStatus::Error).await;
        if let Err(error) = persist_headless_interaction_snapshot(persistence, session).await {
            interaction_persistence_failed.store(true, Ordering::Release);
            mark_persistence_failure(
                session,
                "failed to persist closed execution approval request",
                error,
            )
            .await;
        }
        return Err(RuntimeWorkerError::ExecutionFailed(
            "the pending execution approval request closed before the response was delivered; no tool action was started"
                .to_string(),
        ));
    }
    Ok(RuntimeTurnExecution::InteractionResponseDelivered)
}

async fn deliver_headless_user_input_response(
    session: &Arc<CodeUiSession>,
    interaction_persistence_failed: &Arc<AtomicBool>,
    persistence: Option<&HeadlessSessionPersistence>,
    interaction_id: &str,
    questions: Vec<UserInputQuestion>,
    response_tx: oneshot::Sender<UserInputResponse>,
    response: CodeUiInteractionResponse,
) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
    let user_input_response = user_input_response_from_code_ui_request(&questions, response)
        .map_err(|error| RuntimeWorkerError::ExecutionFailed(error.to_string()))?;
    session.resolve_interaction(interaction_id).await;
    session.set_status(CodeUiSessionStatus::ExecutingTool).await;
    if let Err(error) = persist_headless_interaction_snapshot(persistence, session).await {
        interaction_persistence_failed.store(true, Ordering::Release);
        mark_persistence_failure(
            session,
            "failed to persist resolved user input interaction",
            error,
        )
        .await;
        return Err(RuntimeWorkerError::ExecutionFailed(
            "unable to persist the user-input response; no tool action was started".to_string(),
        ));
    }
    if response_tx.send(user_input_response).is_err() {
        set_status_if_recoverable(session, CodeUiSessionStatus::Error).await;
        if let Err(error) = persist_headless_interaction_snapshot(persistence, session).await {
            interaction_persistence_failed.store(true, Ordering::Release);
            mark_persistence_failure(
                session,
                "failed to persist closed user-input request",
                error,
            )
            .await;
        }
        return Err(RuntimeWorkerError::ExecutionFailed(
            "the pending user-input request closed before the response was delivered; no tool action was started"
                .to_string(),
        ));
    }
    Ok(RuntimeTurnExecution::InteractionResponseDelivered)
}

async fn persist_headless_interaction_snapshot(
    persistence: Option<&HeadlessSessionPersistence>,
    session: &Arc<CodeUiSession>,
) -> io::Result<()> {
    if let Some(persistence) = persistence {
        persistence
            .persist_snapshot(session.snapshot().await)
            .await?;
    }
    Ok(())
}

#[async_trait]
impl<M> CodeUiReadModel for HeadlessCodeRuntime<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    fn session(&self) -> Arc<CodeUiSession> {
        self.session.clone()
    }
}

/// Thin test/lifecycle forwarder — production mounts [`Self::command_adapter`].
#[async_trait]
impl<M> CodeUiCommandAdapter for HeadlessCodeRuntime<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    fn capabilities(&self) -> CodeUiCapabilities {
        self.runtime_bridge.capabilities()
    }

    async fn submit_message(&self, text: String) -> anyhow::Result<()> {
        self.runtime_bridge.submit_message(text).await
    }

    async fn submit_message_with_command_id(
        &self,
        text: String,
        command_id: Option<String>,
    ) -> anyhow::Result<()> {
        self.runtime_bridge
            .submit_message_with_command_id(text, command_id)
            .await
    }

    async fn respond_interaction(
        &self,
        interaction_id: &str,
        response: CodeUiInteractionResponse,
    ) -> anyhow::Result<()> {
        self.runtime_bridge
            .respond_interaction(interaction_id, response)
            .await
    }

    async fn cancel_turn(&self) -> anyhow::Result<()> {
        self.runtime_bridge.cancel_turn().await
    }

    async fn on_controller_lease_takeover(&self) -> anyhow::Result<()> {
        self.runtime_bridge.on_controller_lease_takeover().await
    }

    async fn task_dispatch(&self, agent: String, prompt: String) -> anyhow::Result<String> {
        self.runtime_bridge.task_dispatch(agent, prompt).await
    }

    async fn goal_start(&self, objective: String) -> anyhow::Result<String> {
        self.runtime_bridge.goal_start(objective).await
    }

    async fn goal_status(&self) -> anyhow::Result<String> {
        self.runtime_bridge.goal_status().await
    }

    async fn goal_cancel(&self, reason: String) -> anyhow::Result<String> {
        self.runtime_bridge.goal_cancel(reason).await
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        CodeUiLifecycleShutdown::shutdown(self).await
    }
}

impl<M> HeadlessCodeRuntime<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    /// Read the serialized runtime's session snapshot. Web adapters continue
    /// to consume [`CodeUiSession`] for their rich projection, while this
    /// narrow accessor provides lifecycle integrations and regressions a way
    /// to verify that browser turns are owned by `AgentRuntimeWorker`.
    pub async fn runtime_snapshot(&self) -> Result<AgentSnapshot, RuntimeWorkerError> {
        self.runtime.snapshot(self.runtime_session_id.clone()).await
    }

    async fn shutdown_once(&self) -> anyhow::Result<()> {
        {
            // Confirm/revision admission holds the same transition lock until
            // Runtime admission has installed its shared attempt state.
            // Taking it here closes the TrackExternalTurn-reply handoff: once
            // shutdown has begun, every visible Planning attempt is cancelled
            // before the worker observes shutdown, and no later formal write
            // can win Planning -> Mutating.
            let _transition = self.turn_executor.interaction_transition.lock().await;
            for state in self
                .turn_executor
                .phase1_attempt_states
                .lock()
                .await
                .values()
            {
                loop {
                    let current = state.load(Ordering::Acquire);
                    if !matches!(current, PHASE1_ATTEMPT_PLANNING | PHASE1_ATTEMPT_ADMITTING) {
                        break;
                    }
                    if state
                        .compare_exchange(
                            current,
                            PHASE1_ATTEMPT_CANCELLED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        break;
                    }
                }
            }
        }
        let shutdown_result = self.runtime.shutdown().await;
        if shutdown_result.is_err() {
            self.shutdown_timed_out.store(true, Ordering::Release);
            self.session
                .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
                .await;
            if let Err(error) = self.persist_current_snapshot().await {
                tracing::error!(
                    error = %error,
                    "failed to persist indeterminate headless session after runtime shutdown failure"
                );
            }
        }

        let worker_task = self.runtime_worker_task.lock().await.take();
        if let Some(worker_task) = worker_task {
            worker_task.await.map_err(|error| {
                anyhow!(
                    "AgentRuntime worker terminated unexpectedly during headless shutdown: {error}"
                )
            })?;
        }

        shutdown_result.map_err(|error| {
            anyhow!(
                "Headless web runtime shutdown did not complete cleanly: {error}. The session is indeterminate; inspect and reconcile it before restarting"
            )
        })
    }

    async fn wait_for_shutdown_result(&self) -> anyhow::Result<()> {
        let mut result_rx = self.shutdown_result_tx.subscribe();
        loop {
            if let Some(result) = result_rx.borrow().clone() {
                return result.map_err(anyhow::Error::msg);
            }
            result_rx.changed().await.map_err(|_| {
                anyhow!("The headless web runtime stopped before it published the shutdown result")
            })?;
        }
    }
}

impl<M> CodeUiLifecycleShutdown for HeadlessCodeRuntime<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    fn shutdown(&self) -> futures_util::future::BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async move {
            if self
                .shutting_down
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return self.wait_for_shutdown_result().await;
            }

            let result = self
                .shutdown_once()
                .await
                .map_err(|error| error.to_string());
            self.shutdown_result_tx.send_replace(Some(result.clone()));
            result.map_err(anyhow::Error::msg)
        })
    }

    fn workflow_hub(&self) -> Option<Arc<CodeUiWorkflowHub>> {
        self.persistence
            .as_ref()
            .map(HeadlessSessionPersistence::workflow_hub)
    }
}

impl<M> HeadlessCodeRuntime<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    async fn run_user_and_exec_approval_request_listener(
        weak_listener: std::sync::Weak<Self>,
        mut user_input_rx: mpsc::UnboundedReceiver<UserInputRequest>,
        mut exec_approval_rx: mpsc::UnboundedReceiver<ExecApprovalRequest>,
    ) {
        let mut user_input_open = true;
        let mut exec_approval_open = true;

        while user_input_open || exec_approval_open {
            tokio::select! {
                request = user_input_rx.recv(), if user_input_open => {
                    if let Some(request) = request {
                        if let Some(listener) = weak_listener.upgrade() {
                            listener.handle_user_input_request(request).await;
                        } else {
                            break;
                        }
                    } else {
                        user_input_open = false;
                    }
                }
                request = exec_approval_rx.recv(), if exec_approval_open => {
                    if let Some(request) = request {
                        if let Some(listener) = weak_listener.upgrade() {
                            listener.handle_exec_approval_request(request).await;
                        } else {
                            break;
                        }
                    } else {
                        exec_approval_open = false;
                    }
                }
            }
        }
    }

    async fn handle_user_input_request(&self, request: UserInputRequest) {
        let interaction_id = request.call_id.clone();
        let questions_for_ui = request
            .questions
            .iter()
            .map(request_user_input_question_to_metadata)
            .collect::<Vec<_>>();

        let interaction = CodeUiInteractionRequest {
            id: interaction_id.clone(),
            kind: crate::internal::ai::web::code_ui::CodeUiInteractionKind::RequestUserInput,
            title: Some("User input required".to_string()),
            description: None,
            prompt: None,
            options: Vec::new(),
            status: crate::internal::ai::web::code_ui::CodeUiInteractionStatus::Pending,
            metadata: serde_json::json!({ "questions": questions_for_ui }),
            requested_at: Utc::now(),
            resolved_at: None,
        };

        self.session.upsert_interaction(interaction).await;
        self.session
            .set_status(CodeUiSessionStatus::AwaitingInteraction)
            .await;
        #[cfg(feature = "test-provider")]
        if let Some(persistence) = self.persistence.as_ref() {
            persistence.wait_before_interaction_registration().await;
        }
        if let Err(error) = self.persist_current_snapshot().await {
            self.interaction_persistence_failed
                .store(true, Ordering::Release);
            mark_persistence_failure(
                &self.session,
                "failed to persist pending user input interaction",
                error,
            )
            .await;
            return;
        }
        let interaction_state = InteractionState::AwaitingUserInput {
            interaction_id: interaction_id.clone(),
        };
        if let Some(runtime_turn_id) = self.active_runtime_turn_id().await {
            self.register_live_runtime_interaction(
                runtime_turn_id,
                &interaction_id,
                interaction_state,
                Box::new(HeadlessInteractionDelivery::UserInput {
                    session: self.session.clone(),
                    interaction_persistence_failed: self.interaction_persistence_failed.clone(),
                    persistence: self.persistence.clone(),
                    interaction_id: interaction_id.clone(),
                    questions: request.questions,
                    response_tx: request.response_tx,
                }),
            )
            .await;
            return;
        }

        tracing::error!(
            interaction_id,
            "headless user-input request arrived without an active runtime turn; closing fail-closed"
        );
        self.session.clear_interaction(&interaction_id).await;
        set_status_if_recoverable(&self.session, CodeUiSessionStatus::Error).await;
        drop(request.response_tx);
    }

    async fn handle_exec_approval_request(&self, request: ExecApprovalRequest) {
        let interaction_id = request.call_id.clone();
        let interaction_kind = if request.sandbox_label == "outside sandbox" {
            CodeUiInteractionKind::SandboxApproval
        } else {
            CodeUiInteractionKind::Approval
        };

        let interaction = interaction_request_for_exec_approval(
            interaction_id.clone(),
            interaction_kind,
            &request,
        );

        self.session.upsert_interaction(interaction).await;
        self.session
            .set_status(CodeUiSessionStatus::AwaitingInteraction)
            .await;
        #[cfg(feature = "test-provider")]
        if let Some(persistence) = self.persistence.as_ref() {
            persistence.wait_before_interaction_registration().await;
        }
        if let Err(error) = self.persist_current_snapshot().await {
            self.interaction_persistence_failed
                .store(true, Ordering::Release);
            mark_persistence_failure(
                &self.session,
                "failed to persist pending exec approval interaction",
                error,
            )
            .await;
            return;
        }
        let interaction_state = InteractionState::AwaitingToolApproval {
            interaction_id: interaction_id.clone(),
            tool_name: "shell".to_string(),
        };
        if let Some(runtime_turn_id) = self.active_runtime_turn_id().await {
            self.register_live_runtime_interaction(
                runtime_turn_id,
                &interaction_id,
                interaction_state,
                Box::new(HeadlessInteractionDelivery::ExecApproval {
                    session: self.session.clone(),
                    interaction_persistence_failed: self.interaction_persistence_failed.clone(),
                    persistence: self.persistence.clone(),
                    interaction_id: interaction_id.clone(),
                    request,
                }),
            )
            .await;
            return;
        }

        tracing::error!(
            interaction_id,
            "headless exec approval arrived without an active runtime turn; denying fail-closed"
        );
        self.session.clear_interaction(&interaction_id).await;
        set_status_if_recoverable(&self.session, CodeUiSessionStatus::Error).await;
        let _ = request.response_tx.send(ReviewDecision::Denied);
    }

    async fn active_runtime_turn_id(&self) -> Option<String> {
        let slot = self.in_flight.lock().await;
        slot.as_ref().map(|turn| turn.runtime_turn_id.clone())
    }

    /// Transfer a live tool-loop continuation into the serialized worker.
    async fn register_live_runtime_interaction(
        &self,
        runtime_turn_id: String,
        interaction_id: &str,
        interaction: InteractionState,
        delivery: Box<dyn RuntimeInteractionDelivery>,
    ) {
        if let Err(error) = self
            .runtime
            .register_interaction_with_delivery(
                self.runtime_session_id.clone(),
                runtime_turn_id,
                interaction,
                delivery,
            )
            .await
        {
            tracing::error!(
                interaction_id,
                error = %error,
                "failed to register a live headless interaction with AgentRuntime; closing the interaction fail-closed"
            );
            self.session.clear_interaction(interaction_id).await;
            set_status_if_recoverable(&self.session, CodeUiSessionStatus::Error).await;
        }
    }

    async fn persist_current_snapshot(&self) -> io::Result<()> {
        if let Some(persistence) = self.persistence.as_ref() {
            persistence
                .persist_snapshot(self.session.snapshot().await)
                .await?;
        }
        Ok(())
    }
}

struct WebPlanExecutionObserver {
    session: Arc<CodeUiSession>,
    assistant_entry_id: String,
    handle: tokio::runtime::Handle,
}

impl crate::internal::ai::orchestrator::types::OrchestratorObserver for WebPlanExecutionObserver {
    fn on_task_runtime_event(
        &self,
        task: &crate::internal::ai::orchestrator::types::TaskSpec,
        event: crate::internal::ai::orchestrator::types::TaskRuntimeEvent,
    ) {
        use crate::internal::ai::orchestrator::types::TaskRuntimeEvent;
        let text = match event {
            TaskRuntimeEvent::AssistantMessage(message) => message,
            TaskRuntimeEvent::Note { text, .. } => text,
            TaskRuntimeEvent::ToolCallBegin { tool_name, .. } => {
                format!("{} started `{tool_name}`", task.objective)
            }
            _ => return,
        };
        if text.trim().is_empty() {
            return;
        }
        let session = self.session.clone();
        let entry_id = self.assistant_entry_id.clone();
        self.handle.spawn(async move {
            session
                .mutate(CodeUiEventType::SessionUpdated, |snapshot| {
                    if let Some(entry) = snapshot.transcript.iter_mut().find(|e| e.id == entry_id) {
                        let mut content = entry.content.clone().unwrap_or_default();
                        if !content.is_empty() {
                            content.push('\n');
                        }
                        content.push_str(&text);
                        entry.content = Some(content);
                        entry.updated_at = Utc::now();
                    }
                })
                .await;
        });
    }
}

fn orchestrator_error_to_runtime(
    error: crate::internal::ai::orchestrator::types::OrchestratorError,
) -> RuntimeWorkerError {
    RuntimeWorkerError::ExecutionFailed(error.to_string())
}

async fn park_web_plan_execution_repair(
    persistence: &HeadlessSessionPersistence,
    runtime: &AgentRuntimeHandle,
    session_id: &str,
    session: &Arc<CodeUiSession>,
    result: Option<&crate::internal::ai::orchestrator::types::OrchestratorResult>,
    summary: Option<&str>,
) -> Result<(), RuntimeWorkerError> {
    use crate::internal::ai::runtime::{
        DEFAULT_AUTOMATIC_PLAN_REPAIR_ATTEMPTS, PlanExecutionRepairService,
        persist_and_park_plan_execution_repair_gate,
    };

    let interaction_id = format!("plan-repair-{}", uuid::Uuid::new_v4());
    let repair = PlanExecutionRepairService.after_failure(
        interaction_id.clone(),
        result,
        summary,
        0,
        DEFAULT_AUTOMATIC_PLAN_REPAIR_ATTEMPTS,
    );
    let gate_turn_id = format!("plan-repair-turn-{}", uuid::Uuid::new_v4());
    persist_and_park_plan_execution_repair_gate(
        &persistence.goal_event_store(),
        runtime,
        session_id.to_string(),
        &repair,
        gate_turn_id,
    )
    .await?;
    session
        .set_plan_execution_repair(Some(repair.clone()))
        .await;
    if let Some(InteractionState::AwaitingPlanRepair { interaction_id }) =
        repair.interaction_state()
    {
        session
            .upsert_interaction(CodeUiInteractionRequest {
                id: interaction_id,
                kind: CodeUiInteractionKind::PlanExecutionRepair,
                title: Some("Plan execution repair".to_string()),
                description: Some(repair.evidence().output.clone()),
                prompt: None,
                options: vec![
                    CodeUiInteractionOption {
                        id: "continue".to_string(),
                        label: "Continue".to_string(),
                        description: Some("Retry plan execution".to_string()),
                    },
                    CodeUiInteractionOption {
                        id: "cancel".to_string(),
                        label: "Cancel".to_string(),
                        description: Some("Abandon this plan".to_string()),
                    },
                ],
                status: CodeUiInteractionStatus::Pending,
                metadata: serde_json::json!({ "phase": "planExecutionRepair" }),
                requested_at: Utc::now(),
                resolved_at: None,
            })
            .await;
        session
            .set_status(CodeUiSessionStatus::AwaitingInteraction)
            .await;
    } else {
        session.set_status(CodeUiSessionStatus::Idle).await;
    }
    persistence
        .persist_snapshot(session.snapshot().await)
        .await
        .map_err(|error| {
            RuntimeWorkerError::DurabilityFailure(format!(
                "failed to persist plan-execution repair snapshot: {error}"
            ))
        })?;
    Ok(())
}

// `CodeUiProviderAdapter` is automatically implemented for any `T` that
// satisfies `CodeUiReadModel + CodeUiCommandAdapter` via the blanket impl in
// `code_ui.rs`. `Arc<HeadlessCodeRuntime<M>>` picks that up directly because
// `HeadlessCodeRuntime` itself implements both halves.

/// Replace the streaming assistant entry with the finalized text, mark the
/// streaming flag false, and stamp the supplied status (`completed`,
/// `error`, or `cancelled`).
async fn finalize_assistant_entry(
    session: &Arc<CodeUiSession>,
    entry_id: &str,
    text: &str,
    status: &str,
) {
    let entry_id = entry_id.to_string();
    let text = text.to_string();
    let status = status.to_string();
    session
        .mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            if let Some(entry) = snapshot.transcript.iter_mut().find(|e| e.id == entry_id) {
                entry.content = Some(text.clone());
                entry.status = Some(status.clone());
                entry.streaming = false;
                entry.updated_at = Utc::now();
            }
        })
        .await;
}

async fn finalize_phase1_streaming_entries(session: &Arc<CodeUiSession>, text: &str, status: &str) {
    let text = text.to_string();
    let status = status.to_string();
    session
        .mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            for entry in &mut snapshot.transcript {
                if entry.streaming
                    && entry
                        .metadata
                        .get("phase")
                        .and_then(serde_json::Value::as_str)
                        == Some("plan")
                {
                    entry.content = Some(text.clone());
                    entry.status = Some(status.clone());
                    entry.streaming = false;
                    entry.updated_at = Utc::now();
                }
            }
        })
        .await;
}

/// An authenticated Consuming/no-receipt lineage proves that the latest Web
/// consumer stopped before its provider/tool loop. Clear only the live
/// projection that such a turn can leave behind; durable command recovery and
/// sidecar reconciliation run before this helper and remain the authority.
async fn finalize_recovered_intent_revision_consumer_projection(session: &Arc<CodeUiSession>) {
    const REASON: &str =
        "IntentSpec revision consumer stopped before execution and remains available for retry";
    session
        .mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            let running_tool_ids = snapshot
                .tool_calls
                .iter()
                .filter(|tool_call| tool_call.status == "running")
                .map(|tool_call| tool_call.id.clone())
                .collect::<HashSet<_>>();
            for tool_call in &mut snapshot.tool_calls {
                if running_tool_ids.contains(&tool_call.id) {
                    tool_call.status = "failed".to_string();
                    if tool_call.details.is_none() {
                        tool_call.details = Some(REASON.to_string());
                    }
                    tool_call.updated_at = Utc::now();
                }
            }
            for entry in &mut snapshot.transcript {
                let was_streaming = entry.streaming;
                let running_tool = entry.kind == CodeUiTranscriptEntryKind::ToolCall
                    && running_tool_ids.contains(&entry.id)
                    && entry.status.as_deref() == Some("running");
                if was_streaming {
                    entry.content = Some(REASON.to_string());
                    entry.streaming = false;
                }
                if was_streaming || running_tool {
                    entry.status = Some("error".to_string());
                    entry.updated_at = Utc::now();
                }
            }
            for plan in &mut snapshot.plans {
                if running_tool_ids.contains(&plan.id) && plan.status == "running" {
                    plan.status = "failed".to_string();
                    plan.updated_at = Utc::now();
                }
            }
            snapshot.status = CodeUiSessionStatus::Idle;
        })
        .await;
}

/// Complete only the browser projection left by a canonical `/intent cancel`
/// whose exact consumption receipt was durable before its Runtime success
/// terminal. Startup has already authenticated the fixed payload and healed
/// that terminal; no provider/tool work is replayed.
const INTENT_REVISION_CANCEL_ACKNOWLEDGEMENT: &str =
    "IntentSpec revision mode cancelled. Explicit direct commands are available again.";

fn intent_revision_cancel_projection_requires_healing(snapshot: &CodeUiSessionSnapshot) -> bool {
    let Some((cancel_index, last_user)) = snapshot
        .transcript
        .iter()
        .enumerate()
        .rev()
        .find(|(_, entry)| entry.kind == CodeUiTranscriptEntryKind::UserMessage)
    else {
        return false;
    };
    if last_user.content.as_deref()
        != Some(crate::internal::ai::session::jsonl::INTENT_REVISION_CANCEL_COMMAND_INPUT)
    {
        return false;
    }
    snapshot
        .transcript
        .iter()
        .skip(cancel_index + 1)
        .any(|entry| {
            entry.kind == CodeUiTranscriptEntryKind::AssistantMessage
                && (entry.streaming
                    || entry.status.as_deref() != Some("completed")
                    || entry.content.as_deref() != Some(INTENT_REVISION_CANCEL_ACKNOWLEDGEMENT))
        })
}

/// A replacement review marker proves the revision provider returned one
/// successful IntentDraft, but it is fsynced before the source receipt,
/// Runtime terminal, and final browser projection. Detect only the live rows
/// that can survive a crash in that ordered suffix so a second startup can
/// finish them before restoring the exact review gate.
fn intent_revision_replacement_projection_requires_healing(
    snapshot: &CodeUiSessionSnapshot,
) -> bool {
    matches!(
        snapshot.status,
        CodeUiSessionStatus::Thinking
            | CodeUiSessionStatus::ExecutingTool
            | CodeUiSessionStatus::IndeterminateSideEffect
    ) || snapshot.transcript.iter().any(|entry| entry.streaming)
        || snapshot
            .tool_calls
            .iter()
            .any(|tool_call| tool_call.status == "running")
}

async fn finalize_recovered_intent_revision_replacement_projection(session: &Arc<CodeUiSession>) {
    const MESSAGE: &str = "Revised IntentSpec recovered and ready for review.";
    const INTERRUPTED_TOOL: &str = "IntentSpec revision review was recovered after restart; the stale live tool projection was closed.";
    session
        .mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            let consumer_index = snapshot
                .transcript
                .iter()
                .rposition(|entry| entry.kind == CodeUiTranscriptEntryKind::UserMessage);
            let mut recovered_tool_statuses = HashMap::new();
            for tool_call in &mut snapshot.tool_calls {
                if tool_call.status != "running" {
                    continue;
                }
                let status = if tool_call.tool_name == "submit_intent_draft" {
                    "completed"
                } else {
                    "failed"
                };
                tool_call.status = status.to_string();
                if tool_call.details.is_none() {
                    tool_call.details = Some(if status == "completed" {
                        MESSAGE.to_string()
                    } else {
                        INTERRUPTED_TOOL.to_string()
                    });
                }
                tool_call.updated_at = Utc::now();
                recovered_tool_statuses.insert(tool_call.id.clone(), status.to_string());
            }
            for (entry_index, entry) in snapshot.transcript.iter_mut().enumerate() {
                if consumer_index.is_some_and(|index| entry_index > index)
                    && entry.kind == CodeUiTranscriptEntryKind::AssistantMessage
                {
                    entry.content = Some(MESSAGE.to_string());
                    entry.status = Some("completed".to_string());
                    entry.streaming = false;
                    entry.updated_at = Utc::now();
                    continue;
                }
                if entry.kind == CodeUiTranscriptEntryKind::ToolCall
                    && let Some(status) = recovered_tool_statuses.get(&entry.id)
                {
                    entry.status = Some(status.clone());
                    entry.streaming = false;
                    if entry.content.is_none() {
                        entry.content = Some(if status == "completed" {
                            MESSAGE.to_string()
                        } else {
                            INTERRUPTED_TOOL.to_string()
                        });
                    }
                    entry.updated_at = Utc::now();
                }
            }
            for plan in &mut snapshot.plans {
                if recovered_tool_statuses
                    .get(&plan.id)
                    .is_some_and(|status| status == "failed")
                    && plan.status == "running"
                {
                    plan.status = "failed".to_string();
                    plan.updated_at = Utc::now();
                }
            }
            snapshot.status = CodeUiSessionStatus::Idle;
        })
        .await;
}

async fn finalize_recovered_intent_revision_cancel_projection(
    session: &Arc<CodeUiSession>,
) -> bool {
    let mut projection_owned = false;
    session
        .mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            let Some(cancel_index) = snapshot
                .transcript
                .iter()
                .rposition(|entry| entry.kind == CodeUiTranscriptEntryKind::UserMessage)
            else {
                snapshot.status = CodeUiSessionStatus::IndeterminateSideEffect;
                return;
            };
            if snapshot.transcript[cancel_index].content.as_deref()
                != Some(crate::internal::ai::session::jsonl::INTENT_REVISION_CANCEL_COMMAND_INPUT)
            {
                snapshot.status = CodeUiSessionStatus::IndeterminateSideEffect;
                return;
            }
            projection_owned = true;
            let projection_start = cancel_index + 1;
            for entry in snapshot.transcript.iter_mut().skip(projection_start) {
                if entry.kind == CodeUiTranscriptEntryKind::AssistantMessage {
                    entry.content = Some(INTENT_REVISION_CANCEL_ACKNOWLEDGEMENT.to_string());
                    entry.status = Some("completed".to_string());
                    entry.streaming = false;
                    entry.updated_at = Utc::now();
                }
            }
            snapshot.status = CodeUiSessionStatus::Idle;
        })
        .await;
    projection_owned
}

/// Finish only the projection state that a terminal pre-formal Phase 1 turn
/// can leave live across a crash. Callers first validate the exact durable
/// seed/terminal lineage. Runtime admission is single-writer, so any still-
/// running tool row in that recovery belongs to the failed turn; completed
/// rows and unrelated historical plans remain untouched.
pub(super) async fn finalize_terminal_phase1_projection(
    session: &Arc<CodeUiSession>,
    reason: &str,
    transcript_status: &str,
) {
    let reason = reason.to_string();
    let transcript_status = transcript_status.to_string();
    let tool_status = if transcript_status == "cancelled" {
        "cancelled"
    } else {
        "failed"
    }
    .to_string();
    session
        .mutate(CodeUiEventType::SessionUpdated, |snapshot| {
            let running_tool_ids = snapshot
                .tool_calls
                .iter()
                .filter(|tool_call| tool_call.status == "running")
                .map(|tool_call| tool_call.id.clone())
                .collect::<HashSet<_>>();
            for tool_call in &mut snapshot.tool_calls {
                if running_tool_ids.contains(&tool_call.id) {
                    tool_call.status = tool_status.clone();
                    if tool_call.details.is_none() {
                        tool_call.details = Some(reason.clone());
                    }
                    tool_call.updated_at = Utc::now();
                }
            }
            for entry in &mut snapshot.transcript {
                let phase1_stream = entry.streaming
                    && entry
                        .metadata
                        .get("phase")
                        .and_then(serde_json::Value::as_str)
                        == Some("plan");
                let running_tool = entry.kind == CodeUiTranscriptEntryKind::ToolCall
                    && running_tool_ids.contains(&entry.id)
                    && entry.status.as_deref() == Some("running");
                if phase1_stream {
                    entry.content = Some(reason.clone());
                    entry.streaming = false;
                }
                if phase1_stream || running_tool {
                    entry.status = Some(if running_tool {
                        tool_status.clone()
                    } else {
                        transcript_status.clone()
                    });
                    entry.updated_at = Utc::now();
                }
            }
            for plan in &mut snapshot.plans {
                if running_tool_ids.contains(&plan.id) && plan.status == "running" {
                    plan.status = "failed".to_string();
                    plan.updated_at = Utc::now();
                }
            }
            snapshot.status = CodeUiSessionStatus::Idle;
        })
        .await;
}

fn format_completion_error(error: &CompletionError) -> String {
    format!("Agent turn failed: {error}")
}

async fn mark_persistence_failure(
    session: &Arc<CodeUiSession>,
    message: &'static str,
    error: io::Error,
) {
    tracing::error!(error = %error, "{message}");
    session
        .set_status(CodeUiSessionStatus::IndeterminateSideEffect)
        .await;
}

fn sync_session_metadata_from_snapshot(
    state: &mut SessionState,
    mut snapshot: CodeUiSessionSnapshot,
    projection_sequence: u64,
) -> io::Result<()> {
    let thread_id = snapshot
        .thread_id
        .clone()
        .unwrap_or_else(|| state.id.clone());
    snapshot.thread_id = Some(thread_id.clone());
    state
        .metadata
        .insert("thread_id".to_string(), serde_json::json!(thread_id));
    state.metadata.insert(
        "code_ui_snapshot".to_string(),
        serde_json::to_value(snapshot).map_err(json_projection_error)?,
    );
    state.metadata.insert(
        "code_ui_projection_cursor".to_string(),
        serde_json::json!(projection_sequence),
    );
    state.updated_at = Utc::now();
    Ok(())
}

fn request_user_input_question_to_metadata(question: &UserInputQuestion) -> serde_json::Value {
    let mut seen_labels = std::collections::HashSet::new();
    let options = question
        .options
        .as_ref()
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    let label = option.label.trim();
                    if label.is_empty() || !seen_labels.insert(label.to_string()) {
                        return None;
                    }
                    let mut mapped = serde_json::Map::new();
                    mapped.insert(
                        "id".to_string(),
                        serde_json::Value::String(label.to_string()),
                    );
                    mapped.insert(
                        "label".to_string(),
                        serde_json::Value::String(label.to_string()),
                    );
                    if !option.description.trim().is_empty() {
                        mapped.insert(
                            "description".to_string(),
                            serde_json::Value::String(option.description.clone()),
                        );
                    }
                    Some(serde_json::Value::Object(mapped))
                })
                .collect::<Vec<_>>()
        })
        .filter(|options| !options.is_empty())
        .unwrap_or_default();
    let has_options = !options.is_empty();

    serde_json::json!({
        "id": question.id,
        "header": question.header,
        "prompt": question.question,
        "kind": if has_options { "single" } else { "text" },
        "options": options,
        "isOther": question.is_other,
        "isSecret": question.is_secret,
    })
}

fn interaction_request_for_exec_approval(
    interaction_id: String,
    kind: CodeUiInteractionKind,
    request: &ExecApprovalRequest,
) -> CodeUiInteractionRequest {
    let command = request.command.clone();
    let reason = request
        .reason
        .clone()
        .unwrap_or_else(|| String::from("Command execution"))
        .trim()
        .to_string();

    let title = match kind {
        CodeUiInteractionKind::Approval => "Approve command execution",
        CodeUiInteractionKind::SandboxApproval => "Approve sandbox-executed command",
        _ => "Approval request",
    };

    CodeUiInteractionRequest {
        id: interaction_id,
        kind,
        title: Some(title.to_string()),
        description: Some(reason),
        prompt: Some(command),
        options: vec![
            CodeUiInteractionOption {
                id: "approve".to_string(),
                label: "Approve".to_string(),
                description: Some("Allow this command once".to_string()),
            },
            CodeUiInteractionOption {
                id: "deny".to_string(),
                label: "Deny".to_string(),
                description: Some("Skip this command".to_string()),
            },
            CodeUiInteractionOption {
                id: "abort".to_string(),
                label: "Abort".to_string(),
                description: Some("Cancel this tool run immediately".to_string()),
            },
        ],
        status: CodeUiInteractionStatus::Pending,
        metadata: exec_approval_request_to_metadata(request),
        requested_at: Utc::now(),
        resolved_at: None,
    }
}

fn exec_approval_request_to_metadata(request: &ExecApprovalRequest) -> serde_json::Value {
    serde_json::json!({
        "command": request.command,
        "cwd": request.cwd.display().to_string(),
        "reason": request.reason,
        "is_retry": request.is_retry,
        "sandbox_label": request.sandbox_label,
        "network_access": network_access_label(&request.network_access),
        "writable_roots": request
            .writable_roots
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
        "cache_disabled_reason": request.cache_disabled_reason,
    })
}

fn network_access_label(network_access: &NetworkAccess) -> &'static str {
    match network_access {
        NetworkAccess::Denied => "denied",
        NetworkAccess::Allowlist { .. } => "allowlist",
        NetworkAccess::Full => "full",
    }
}

fn review_decision_from_interaction_response(
    response: CodeUiInteractionResponse,
) -> anyhow::Result<ReviewDecision> {
    let approved = response
        .approved
        .or(match response.selected_option.as_deref() {
            Some(option) if option.eq_ignore_ascii_case("approve") => Some(true),
            Some(option) if option.eq_ignore_ascii_case("allow") => Some(true),
            Some(option) if option.eq_ignore_ascii_case("approve_all") => Some(true),
            Some(option) if option.eq_ignore_ascii_case("yes") => Some(true),
            Some(option) if option.eq_ignore_ascii_case("deny") => Some(false),
            Some(option) if option.eq_ignore_ascii_case("decline") => Some(false),
            Some(option) if option.eq_ignore_ascii_case("no") => Some(false),
            Some(option) if option.eq_ignore_ascii_case("abort") => {
                return Ok(ReviewDecision::Abort);
            }
            _ => None,
        })
        .ok_or_else(|| anyhow!("Exec approvals require an explicit decision"))?;

    if !approved {
        return Ok(ReviewDecision::Denied);
    }

    match response.apply_to_future {
        Some(CodeUiApplyToFuture::AcceptAll) => Ok(ReviewDecision::ApprovedForAllCommands),
        Some(CodeUiApplyToFuture::DeclineAll) => Ok(ReviewDecision::Denied),
        Some(CodeUiApplyToFuture::No) | None => Ok(ReviewDecision::Approved),
    }
}

fn user_input_response_from_code_ui_request(
    questions: &[UserInputQuestion],
    response: CodeUiInteractionResponse,
) -> anyhow::Result<UserInputResponse> {
    if questions.is_empty() {
        return Err(anyhow!("User input request contains no questions"));
    }

    if questions
        .iter()
        .any(|question| question.id.trim().is_empty())
    {
        return Err(anyhow!(
            "User input request contains a question without a stable id"
        ));
    }
    let question_ids = questions
        .iter()
        .map(|question| question.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    if question_ids.len() != questions.len() {
        return Err(anyhow!(
            "User input request contains duplicate question ids and cannot be answered safely"
        ));
    }

    if !response.answers.is_empty() {
        let unknown_question_ids = response
            .answers
            .keys()
            .filter(|question_id| !question_ids.contains(question_id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !unknown_question_ids.is_empty() {
            return Err(anyhow!(
                "User input response contains answers for unknown question ids: {}",
                unknown_question_ids.join(", ")
            ));
        }

        let mut answers = HashMap::with_capacity(questions.len());
        for question in questions {
            let values = response.answers.get(&question.id).ok_or_else(|| {
                anyhow!(
                    "User input response is missing an answer for question '{}'",
                    question.id
                )
            })?;
            if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
                return Err(anyhow!(
                    "User input response must include a non-empty answer for question '{}'",
                    question.id
                ));
            }
            answers.insert(
                question.id.clone(),
                UserInputAnswer {
                    answers: values.clone(),
                },
            );
        }
        return Ok(UserInputResponse { answers });
    }

    if questions.len() != 1 {
        return Err(anyhow!(
            "User input response must answer each of the {} requested questions",
            questions.len()
        ));
    }
    let question = &questions[0];

    let mut values = Vec::new();
    if let Some(selected) = response.selected_option
        && !selected.is_empty()
    {
        values.push(selected);
    }
    if let Some(note) = response.note.as_deref() {
        let note = note.trim();
        if !note.is_empty() {
            values.push(format!("user_note: {note}"));
        }
    }

    if values.is_empty()
        && let Some(approved) = response.approved
    {
        values.push(if approved {
            "yes".to_string()
        } else {
            "no".to_string()
        });
    }

    if values.is_empty() {
        return Err(anyhow!("User input response must include answers"));
    }

    Ok(UserInputResponse {
        answers: [(question.id.clone(), UserInputAnswer { answers: values })]
            .into_iter()
            .collect::<HashMap<_, _>>(),
    })
}

/// Observer that streams text deltas into the live snapshot transcript so the
/// browser sees the assistant's reply build up as it arrives.
#[derive(Default)]
struct CapturedIntentDraft {
    value: Option<String>,
    successful_calls: usize,
}

struct HeadlessTurnObserver {
    session: Arc<CodeUiSession>,
    assistant_entry_id: String,
    tool_arguments: Arc<std::sync::Mutex<HashMap<String, serde_json::Value>>>,
    /// `JoinHandle`s of the per-tool-call "start" projection tasks, keyed by
    /// call id. `on_tool_call_start` and `on_tool_call_end` each `tokio::spawn`
    /// an independent task with no ordering guarantee; `on_tool_call_end`
    /// awaits the matching start handle before writing terminal state so a late
    /// "start" task can never clobber the "completed" tool_call / transcript /
    /// plan rows or regress the session status back to `ExecutingTool`.
    start_tasks: Arc<std::sync::Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// Terminal projection tasks. They must finish before the enclosing turn
    /// writes its terminal session status, or their final `Thinking` update
    /// can race with `Idle`/`Error`/`Cancelled`.
    completion_tasks: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    /// Coalescing buffer + single worker for assistant text deltas.
    /// Unordered per-delta tasks reordered appends; an unbounded mpsc of
    /// every delta retained heap proportional to delta count. Coalescing
    /// keeps O(transcript) memory with one task (W3-12 Codex r7/r9).
    stream_delta_pending: Arc<std::sync::Mutex<String>>,
    stream_delta_notify: Arc<tokio::sync::Notify>,
    stream_delta_closed: Arc<std::sync::atomic::AtomicBool>,
    stream_delta_task: Option<tokio::task::JoinHandle<()>>,
    /// Successful `submit_intent_draft` payload for PlanPhase0 review parking.
    intent_draft_json: Arc<std::sync::Mutex<CapturedIntentDraft>>,
    /// Successful `submit_plan_draft` payload for Phase 1 formal compilation.
    plan_draft_json: Arc<std::sync::Mutex<Option<String>>>,
    /// Authoritative risk_profile answer from `request_user_input` (when asked).
    selected_risk: Arc<std::sync::Mutex<Option<crate::internal::ai::intentspec::RiskLevel>>>,
}

impl HeadlessTurnObserver {
    /// Wait for all projection tasks belonging to the current turn. Callback
    /// invocation is single-threaded inside the tool loop, so by the time the
    /// loop returns no new handles can be added; the loop only handles the
    /// handoff where an end task has taken a start task between the two drains.
    async fn flush_projection_tasks(&mut self) {
        self.stream_delta_closed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.stream_delta_notify.notify_one();
        if let Some(handle) = self.stream_delta_task.take() {
            let _ = handle.await;
        }
        loop {
            let mut handles = self
                .start_tasks
                .lock()
                .map(|mut tasks| tasks.drain().map(|(_, task)| task).collect::<Vec<_>>())
                .unwrap_or_default();
            handles.extend(
                self.completion_tasks
                    .lock()
                    .map(|mut tasks| std::mem::take(&mut *tasks))
                    .unwrap_or_default(),
            );
            if handles.is_empty() {
                return;
            }
            for handle in handles {
                let _ = handle.await;
            }
        }
    }
}

impl super::super::agent::runtime::tool_loop::ToolLoopObserver for HeadlessTurnObserver {
    fn on_model_stream_event(&mut self, event: &CompletionStreamEvent) {
        if let CompletionStreamEvent::TextDelta { delta, .. } = event {
            if delta.is_empty() {
                return;
            }
            if self.stream_delta_task.is_none() {
                let session = self.session.clone();
                let entry_id = self.assistant_entry_id.clone();
                let pending = self.stream_delta_pending.clone();
                let notify = self.stream_delta_notify.clone();
                let closed = self.stream_delta_closed.clone();
                self.stream_delta_task = Some(tokio::spawn(async move {
                    loop {
                        let chunk = pending
                            .lock()
                            .map(|mut buf| std::mem::take(&mut *buf))
                            .unwrap_or_default();
                        if !chunk.is_empty() {
                            session.append_assistant_delta(&entry_id, &chunk).await;
                            continue;
                        }
                        if closed.load(std::sync::atomic::Ordering::SeqCst) {
                            break;
                        }
                        notify.notified().await;
                    }
                }));
            }
            if let Ok(mut buf) = self.stream_delta_pending.lock() {
                buf.push_str(delta);
            }
            self.stream_delta_notify.notify_one();
        }
    }

    fn on_model_usage_recorded(&mut self, _usage: &CompletionUsageSummary, _wall_clock_ms: u64) {
        // Phase 3 follow-up: persist usage rows + show them in the Settings tab.
    }

    fn on_tool_call_begin(
        &mut self,
        call_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) {
        let arguments_are_bounded = tool_name != "submit_plan_draft"
            || validate_submit_plan_draft_value_bounds(arguments).is_ok();
        if arguments_are_bounded && let Ok(mut arguments_by_call) = self.tool_arguments.lock() {
            arguments_by_call.insert(call_id.to_string(), arguments.clone());
        }

        let session = self.session.clone();
        let call_id = call_id.to_string();
        let start_key = call_id.clone();
        let tool_name = tool_name.to_string();
        let arguments = arguments_are_bounded.then(|| arguments.clone());
        let handle = tokio::spawn(async move {
            let summary = arguments.as_ref().map_or_else(
                || "Rejected oversized plan draft".to_string(),
                |arguments| headless_tool_call_summary(&tool_name, arguments),
            );
            session
                .upsert_tool_call(CodeUiToolCallSnapshot {
                    id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    status: "running".to_string(),
                    summary: Some(summary.clone()),
                    details: None,
                    updated_at: Utc::now(),
                })
                .await;
            session
                .upsert_transcript_entry(CodeUiTranscriptEntry {
                    id: call_id.clone(),
                    kind: CodeUiTranscriptEntryKind::ToolCall,
                    title: Some(tool_name.clone()),
                    content: Some(summary),
                    status: Some("running".to_string()),
                    streaming: false,
                    metadata: serde_json::json!({}),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                })
                .await;
            if tool_name == "update_plan"
                && let Some(arguments) = arguments.as_ref()
                && let Some(plan) =
                    plan_snapshot_from_update_plan_arguments(&call_id, "running", arguments)
            {
                session.upsert_plan(plan).await;
            }
            if tool_name == "submit_plan_draft"
                && let Some(arguments) = arguments.as_ref()
                && let Some(plan) =
                    plan_snapshot_from_submit_plan_draft_arguments(&call_id, "running", arguments)
            {
                session.upsert_plan(plan).await;
            }
            // Late-start barrier for the APPROVAL parking path (W5-03):
            // this task runs unordered relative to an exec-approval /
            // user-input request that parks the turn — never clobber a
            // parked AwaitingInteraction gate back to ExecutingTool.
            session
                .set_status_unless_awaiting_interaction(CodeUiSessionStatus::ExecutingTool)
                .await;
        });
        // Record the start task so `on_tool_call_end` can await it before
        // writing terminal state (the ordering barrier for this tool call).
        if let Ok(mut tasks) = self.start_tasks.lock() {
            tasks.insert(start_key, handle);
        }
    }

    fn on_tool_call_end(
        &mut self,
        call_id: &str,
        tool_name: &str,
        result: &Result<ToolOutput, String>,
    ) {
        let arguments = self
            .tool_arguments
            .lock()
            .ok()
            .and_then(|mut arguments_by_call| arguments_by_call.remove(call_id));
        if tool_name == "submit_intent_draft"
            && matches!(result, Ok(output) if output.is_success())
            && let Some(arguments) = arguments.as_ref()
            && let Ok(mut draft) = self.intent_draft_json.lock()
        {
            draft.successful_calls = draft.successful_calls.saturating_add(1);
            if draft.value.is_none() {
                draft.value = Some(arguments.to_string());
            }
        }
        if tool_name == "submit_plan_draft"
            && matches!(result, Ok(output) if output.is_success())
            && let Some(arguments) = arguments.as_ref()
            && let Ok(mut draft) = self.plan_draft_json.lock()
        {
            *draft = Some(arguments.to_string());
        }
        if tool_name == "request_user_input"
            && let Ok(output) = result
            && let Some(content) = output.as_text()
            && let Ok(resp) = serde_json::from_str::<UserInputResponse>(content)
            && let Some(level) = extract_risk_level_from_user_input(&resp)
            && let Ok(mut selected) = self.selected_risk.lock()
        {
            *selected = Some(level);
        }
        // Ordering barrier: take the matching `on_tool_call_begin` task so the
        // end task can await it before writing terminal state. Without this, a
        // late-scheduled start task would clobber "completed" back to "running"
        // (tool_call / transcript / plan rows) and regress the session status.
        let start_handle = self
            .start_tasks
            .lock()
            .ok()
            .and_then(|mut tasks| tasks.remove(call_id));
        let session = self.session.clone();
        let call_id = call_id.to_string();
        let tool_name = tool_name.to_string();
        let result = result.clone();
        let handle = tokio::spawn(async move {
            if let Some(handle) = start_handle {
                let _ = handle.await;
            }
            let (status, details) = match &result {
                Ok(output) if output.is_success() => (
                    "completed".to_string(),
                    output.as_text().map(ToString::to_string),
                ),
                Ok(output) => (
                    "failed".to_string(),
                    output.as_text().map(ToString::to_string),
                ),
                Err(error) => ("failed".to_string(), Some(error.clone())),
            };

            session
                .upsert_tool_call(CodeUiToolCallSnapshot {
                    id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    status: status.clone(),
                    summary: None,
                    details: details.clone(),
                    updated_at: Utc::now(),
                })
                .await;
            session
                .upsert_transcript_entry(CodeUiTranscriptEntry {
                    id: call_id.clone(),
                    kind: CodeUiTranscriptEntryKind::ToolCall,
                    title: Some(tool_name.clone()),
                    content: details,
                    status: Some(status.clone()),
                    streaming: false,
                    metadata: serde_json::json!({}),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                })
                .await;
            if tool_name == "apply_patch"
                && let Some(patchset) =
                    patchset_snapshot_for_tool_result(&call_id, &status, &result)
            {
                session.upsert_patchset(patchset).await;
            }
            if tool_name == "update_plan"
                && let Some(arguments) = arguments.as_ref()
                && let Some(plan) =
                    plan_snapshot_from_update_plan_arguments(&call_id, &status, arguments)
            {
                session.upsert_plan(plan).await;
            }
            if tool_name == "submit_plan_draft"
                && let Some(arguments) = arguments.as_ref()
                && let Some(plan) =
                    plan_snapshot_from_submit_plan_draft_arguments(&call_id, &status, arguments)
            {
                session.upsert_plan(plan).await;
            }
            session.set_status(CodeUiSessionStatus::Thinking).await;
        });
        if let Ok(mut tasks) = self.completion_tasks.lock() {
            tasks.push(handle);
        }
    }
}

fn headless_tool_call_summary(tool_name: &str, arguments: &serde_json::Value) -> String {
    if tool_name == "shell"
        && let Some(command) = arguments.get("command").and_then(serde_json::Value::as_str)
    {
        return format!("Run `{command}`");
    }

    if tool_name == "read_file"
        && let Some(path) = arguments.get("path").and_then(serde_json::Value::as_str)
    {
        return format!("Read {path}");
    }

    if tool_name == "web_search"
        && let Some(query) = arguments.get("query").and_then(serde_json::Value::as_str)
    {
        return format!("Search {query}");
    }

    match tool_name {
        "apply_patch" => "Apply patch".to_string(),
        "request_user_input" => "Ask for user input".to_string(),
        "submit_intent_draft" => "Submit intent draft".to_string(),
        "submit_plan_draft" => "Submit plan draft".to_string(),
        "update_plan" => "Update plan".to_string(),
        _ => tool_name.replace('_', " "),
    }
}

fn plan_snapshot_from_update_plan_arguments(
    call_id: &str,
    status: &str,
    arguments: &serde_json::Value,
) -> Option<CodeUiPlanSnapshot> {
    let args = serde_json::from_value::<UpdatePlanArgs>(arguments.clone()).ok()?;
    Some(CodeUiPlanSnapshot {
        id: call_id.to_string(),
        title: Some("Current plan".to_string()),
        summary: args.explanation,
        status: status.to_string(),
        steps: args
            .plan
            .into_iter()
            .map(|step| CodeUiPlanStep {
                step: step.step,
                status: step_status_label(&step.status).to_string(),
            })
            .collect(),
        updated_at: Utc::now(),
    })
}

fn plan_snapshot_from_submit_plan_draft_arguments(
    call_id: &str,
    status: &str,
    arguments: &serde_json::Value,
) -> Option<CodeUiPlanSnapshot> {
    let args = serde_json::from_value::<SubmitPlanDraftArgs>(arguments.clone()).ok()?;
    Some(CodeUiPlanSnapshot {
        id: call_id.to_string(),
        title: Some("Draft execution plan".to_string()),
        summary: args.explanation,
        status: status.to_string(),
        steps: args
            .steps
            .into_iter()
            .map(|step| CodeUiPlanStep {
                step: step.title,
                status: "pending".to_string(),
            })
            .collect(),
        updated_at: Utc::now(),
    })
}

fn step_status_label(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::InProgress => "in_progress",
        StepStatus::Completed => "completed",
    }
}

fn patchset_snapshot_for_tool_result(
    call_id: &str,
    status: &str,
    result: &Result<ToolOutput, String>,
) -> Option<CodeUiPatchsetSnapshot> {
    let Ok(output) = result else {
        return None;
    };
    let ToolOutput::Function {
        metadata: Some(metadata),
        ..
    } = output
    else {
        return None;
    };
    let diffs = metadata.get("diffs")?.as_array()?;
    let changes = diffs
        .iter()
        .filter_map(|entry| {
            Some(CodeUiPatchChange {
                path: entry.get("path")?.as_str()?.to_string(),
                change_type: entry
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("update")
                    .to_string(),
                diff: entry
                    .get("diff")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string),
            })
        })
        .collect::<Vec<_>>();
    if changes.is_empty() {
        return None;
    }
    Some(CodeUiPatchsetSnapshot {
        id: call_id.to_string(),
        status: status.to_string(),
        changes,
        updated_at: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn revision_test_spec(summary: &str) -> crate::internal::ai::intentspec::IntentSpec {
        use crate::internal::ai::intentspec::{
            DraftAcceptance, DraftIntent, DraftRisk, IntentDraft, ResolveContext, RiskLevel,
            resolve_intentspec,
            types::{ChangeType, Objective, ObjectiveKind},
        };

        resolve_intentspec(
            IntentDraft {
                intent: DraftIntent {
                    summary: summary.to_string(),
                    problem_statement: "pin revision retry lineage".to_string(),
                    change_type: ChangeType::Bugfix,
                    objectives: vec![Objective {
                        title: "preserve exact retry identity".to_string(),
                        kind: ObjectiveKind::Implementation,
                    }],
                    in_scope: vec!["src".to_string()],
                    out_of_scope: Vec::new(),
                    touch_hints: None,
                },
                acceptance: DraftAcceptance {
                    success_criteria: vec!["retry remains exact".to_string()],
                    fast_checks: Vec::new(),
                    integration_checks: Vec::new(),
                    security_checks: Vec::new(),
                    release_checks: Vec::new(),
                },
                risk: DraftRisk {
                    rationale: "unit-test fixture".to_string(),
                    factors: Vec::new(),
                    level: Some(RiskLevel::Low),
                },
            },
            RiskLevel::Low,
            ResolveContext {
                working_dir: "/repo".to_string(),
                base_ref: "HEAD".to_string(),
                created_by_id: "test".to_string(),
            },
        )
    }

    async fn persist_revision_test_source(
        persistence: &HeadlessSessionPersistence,
        command: CodeCommandIntent,
        interaction_id: &str,
        spec: &crate::internal::ai::intentspec::IntentSpec,
        note: &str,
    ) -> PendingIntentRevision {
        let store = persistence.goal_event_store();
        let intent_id = spec.metadata.id.clone();
        persist_web_phase0_intent_before_review(Some(persistence), None, spec, intent_id.clone())
            .await
            .expect("IntentSpec fixture persisted");
        store
            .admit_code_command(command.clone())
            .expect("source command admitted");
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id: interaction_id.to_string(),
                intent_id,
                turn_id: format!("{interaction_id}-gate"),
                phase0_turn_id: command.identity.command_id.clone(),
            })
            .expect("review marker persisted");
        let sidecar_digest = prepare_intent_revision_sidecar(
            persistence,
            interaction_id,
            &command.identity.command_id,
            Some(note.to_string()),
        )
        .expect("prepared revision sidecar persisted");
        store
            .complete_code_command_success_with_interaction_resolutions_and_intent_revision(
                &command.identity,
                "revision requested",
                &[(interaction_id.to_string(), "modify".to_string())],
                Some(&IntentRevisionRecovery {
                    interaction_id: interaction_id.to_string(),
                    sidecar_digest,
                }),
            )
            .expect("combined Modify terminal persisted");
        promote_prepared_intent_revision(
            persistence,
            interaction_id,
            &command.identity.command_id,
            Some(Some(note)),
        )
        .expect("revision sidecar promoted")
    }

    fn persist_replacement_review_marker(
        store: &crate::internal::ai::session::jsonl::SessionJsonlStore,
        consumer_command_id: &str,
        interaction_id: &str,
        intent_id: &str,
    ) {
        store
            .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id: interaction_id.to_string(),
                intent_id: intent_id.to_string(),
                turn_id: format!("{interaction_id}-gate"),
                phase0_turn_id: consumer_command_id.to_string(),
            })
            .expect("replacement review marker persisted");
    }

    #[test]
    fn intent_revision_note_is_canonical_and_bounded() {
        let response = CodeUiInteractionResponse {
            selected_option: Some("modify".to_string()),
            note: Some("  retain the safety constraint  ".to_string()),
            ..Default::default()
        };
        assert_eq!(
            canonical_intent_revision_note(&response).expect("valid note"),
            Some("retain the safety constraint".to_string())
        );

        let oversized = CodeUiInteractionResponse {
            selected_option: Some("modify".to_string()),
            note: Some("x".repeat(MAX_INTENT_REVISION_NOTE_BYTES + 1)),
            ..Default::default()
        };
        assert!(matches!(
            canonical_intent_revision_note(&oversized),
            Err(RuntimeWorkerError::InvalidInteractionResponse(message))
                if message.contains("16384-byte UTF-8 limit")
        ));
    }

    #[test]
    fn startup_receipt_validation_accepts_legacy_binding_and_rejects_reused_consumer() {
        let storage = tempfile::tempdir().expect("temporary session storage");
        let store = Arc::new(SessionStore::from_storage_path(storage.path()));
        let state = SessionState::new("/repo");
        let persistence =
            HeadlessSessionPersistence::new(store, state).expect("attach headless persistence");
        let goal_store = persistence.goal_event_store();
        let (_, repo_id, principal_id) = persistence.worker_durability_config();
        let session_id = persistence.durability_session_id().to_string();

        let append_legacy_source = |command_id: &str, interaction_id: &str, intent_id: &str| {
            let intent = CodeCommandIntent::new(
                CodeCommandIdentity::new(
                    repo_id.clone(),
                    session_id.clone(),
                    principal_id.clone(),
                    command_id,
                ),
                CODE_UI_WEB_TURN_KIND,
                format!("sha256:{command_id}"),
                true,
            );
            goal_store
                .admit_code_command(intent.clone())
                .expect("legacy source intent admitted");
            goal_store
                .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                    interaction_id: interaction_id.to_string(),
                    intent_id: intent_id.to_string(),
                    turn_id: format!("{interaction_id}-gate"),
                    phase0_turn_id: command_id.to_string(),
                })
                .expect("legacy source marker persisted");
            let terminal = goal_store
                .append_code_workflow_durable(
                    CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                        command: intent.identity.clone(),
                        summary: "revision requested".to_string(),
                        interaction_id: interaction_id.to_string(),
                        resolution: "modify".to_string(),
                        prior_interaction_resolutions: Vec::new(),
                        intent_revision: None,
                    },
                )
                .expect("legacy source terminal persisted");
            (intent, terminal)
        };
        let (source_a, terminal_a) = append_legacy_source("source-a", "review-a", "intent-a");
        use sha2::Digest as _;
        let consumer = CodeCommandIntent::new(
            CodeCommandIdentity::new(
                repo_id.clone(),
                session_id.clone(),
                principal_id.clone(),
                "revision-consumer",
            ),
            CODE_UI_WEB_TURN_KIND,
            format!(
                "sha256:{}",
                hex::encode(sha2::Sha256::digest(
                    crate::internal::ai::session::jsonl::INTENT_REVISION_CANCEL_COMMAND_INPUT
                        .as_bytes(),
                ))
            ),
            true,
        );
        goal_store
            .admit_code_command(consumer.clone())
            .expect("consumer intent admitted");
        load_or_create_intent_revision_hmac_key(&persistence).expect("session HMAC key persisted");

        let claim_a = IntentRevisionConsumptionClaim {
            schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
            interaction_id: "review-a".to_string(),
            source_command: source_a.identity,
            consumer_intent: consumer.clone(),
            terminal_event_id: terminal_a.event_id,
            terminal_sequence: terminal_a.sequence,
            intent_id: "intent-a".to_string(),
            sidecar_digest: Some(format!("hmac-sha256:{}", "a".repeat(64))),
        };
        let consumption_a = goal_store
            .prepare_intent_revision_consumption(&consumer, &claim_a)
            .expect("legacy consumption prepared");
        goal_store
            .record_intent_revision_consumption(&consumption_a)
            .expect("legacy consumption receipt persisted");
        goal_store
            .complete_code_command_success(&consumer.identity, "legacy revision cancelled")
            .expect("legacy cancel terminal persisted");
        let replay = goal_store
            .load_code_workflow_replay_committed()
            .expect("committed legacy receipt replay");
        validate_all_intent_revision_consumption_receipts(&persistence, &replay)
            .expect("one exact legacy receipt is valid");

        let (source_b, terminal_b) = append_legacy_source("source-b", "review-b", "intent-b");
        let claim_b = IntentRevisionConsumptionClaim {
            schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
            interaction_id: "review-b".to_string(),
            source_command: source_b.identity,
            consumer_intent: consumer,
            terminal_event_id: terminal_b.event_id,
            terminal_sequence: terminal_b.sequence,
            intent_id: "intent-b".to_string(),
            sidecar_digest: Some(format!("hmac-sha256:{}", "b".repeat(64))),
        };
        let conflicting = IntentRevisionConsumption {
            claim: claim_b,
            consumer_intent_event_id: consumption_a.consumer_intent_event_id,
            consumer_intent_sequence: consumption_a.consumer_intent_sequence,
        };
        goal_store
            .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
                interaction_id: "review-b".to_string(),
                resolution: "modify".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: Some(conflicting),
            })
            .expect("synthetic conflicting receipt persisted");
        let replay = goal_store
            .load_code_workflow_replay_committed()
            .expect("committed conflicting replay");
        assert!(validate_all_intent_revision_consumption_receipts(&persistence, &replay).is_err());
    }

    #[test]
    fn startup_receipt_validation_rejects_receipt_after_consumer_terminal() {
        let storage = tempfile::tempdir().expect("temporary session storage");
        let store = Arc::new(SessionStore::from_storage_path(storage.path()));
        let state = SessionState::new("/repo");
        let persistence =
            HeadlessSessionPersistence::new(store, state).expect("attach headless persistence");
        let goal_store = persistence.goal_event_store();
        let (_, repo_id, principal_id) = persistence.worker_durability_config();
        let session_id = persistence.durability_session_id().to_string();
        let source = CodeCommandIntent::new(
            CodeCommandIdentity::new(
                repo_id.clone(),
                session_id.clone(),
                principal_id.clone(),
                "legacy-source",
            ),
            CODE_UI_WEB_TURN_KIND,
            "sha256:legacy-source",
            true,
        );
        goal_store
            .admit_code_command(source.clone())
            .expect("legacy source intent admitted");
        goal_store
            .append_code_workflow_durable(CodeWorkflowEventKind::IntentReviewRequested {
                interaction_id: "legacy-review".to_string(),
                intent_id: "legacy-intent".to_string(),
                turn_id: "legacy-review-gate".to_string(),
                phase0_turn_id: source.identity.command_id.clone(),
            })
            .expect("legacy source marker persisted");
        let source_terminal = goal_store
            .append_code_workflow_durable(
                CodeWorkflowEventKind::CommandTerminalSuccessWithInteractionResolved {
                    command: source.identity.clone(),
                    summary: "revision requested".to_string(),
                    interaction_id: "legacy-review".to_string(),
                    resolution: "modify".to_string(),
                    prior_interaction_resolutions: Vec::new(),
                    intent_revision: None,
                },
            )
            .expect("legacy source terminal persisted");
        let consumer = CodeCommandIntent::new(
            CodeCommandIdentity::new(repo_id, session_id, principal_id, "consumer"),
            CODE_UI_WEB_TURN_KIND,
            "sha256:consumer",
            true,
        );
        goal_store
            .admit_code_command(consumer.clone())
            .expect("consumer intent admitted");
        let replay = goal_store
            .load_code_workflow_replay_committed()
            .expect("consumer intent replay");
        let consumer_event = replay
            .events
            .iter()
            .find(|event| {
                matches!(
                    &event.event,
                    CodeWorkflowEventKind::CommandIntentPersisted { command }
                        if command == &consumer
                )
            })
            .expect("consumer intent event");
        let consumption = IntentRevisionConsumption {
            claim: IntentRevisionConsumptionClaim {
                schema_version: INTENT_REVISION_CONSUMPTION_SCHEMA_VERSION,
                interaction_id: "legacy-review".to_string(),
                source_command: source.identity,
                consumer_intent: consumer.clone(),
                terminal_event_id: source_terminal.event_id,
                terminal_sequence: source_terminal.sequence,
                intent_id: "legacy-intent".to_string(),
                sidecar_digest: Some(format!("hmac-sha256:{}", "a".repeat(64))),
            },
            consumer_intent_event_id: consumer_event.event_id,
            consumer_intent_sequence: consumer_event.sequence,
        };
        goal_store
            .complete_code_command_success(&consumer.identity, "consumer finished")
            .expect("consumer terminal persisted before corrupt receipt");
        goal_store
            .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
                interaction_id: "legacy-review".to_string(),
                resolution: "modify".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: Some(consumption),
            })
            .expect("synthetic late receipt persisted");
        load_or_create_intent_revision_hmac_key(&persistence).expect("session HMAC key persisted");

        let replay = goal_store
            .load_code_workflow_replay_committed()
            .expect("corrupt late-receipt replay");
        assert!(validate_all_intent_revision_consumption_receipts(&persistence, &replay).is_err());
    }

    #[tokio::test]
    async fn consumed_modify_retry_ignores_a_different_active_revision_sidecar() {
        let storage = tempfile::tempdir().expect("temporary session storage");
        let store = Arc::new(SessionStore::from_storage_path(storage.path()));
        let state = SessionState::new("/repo");
        let persistence =
            HeadlessSessionPersistence::new(store, state).expect("attach headless persistence");
        let goal_store = persistence.goal_event_store();
        let (_, repo_id, principal_id) = persistence.worker_durability_config();
        let session_id = persistence.durability_session_id().to_string();
        let source = |command_id: &str| {
            CodeCommandIntent::new(
                CodeCommandIdentity::new(
                    repo_id.clone(),
                    session_id.clone(),
                    principal_id.clone(),
                    command_id,
                ),
                CODE_UI_WEB_TURN_KIND,
                format!("sha256:{command_id}"),
                true,
            )
        };

        let pending_a = persist_revision_test_source(
            &persistence,
            source("source-a"),
            "review-a",
            &revision_test_spec("revision A"),
            "note A",
        )
        .await;
        let consumer = source("consumer-a");
        goal_store
            .admit_code_command(consumer.clone())
            .expect("consumer command admitted");
        persist_replacement_review_marker(
            &goal_store,
            &consumer.identity.command_id,
            "replacement-a",
            "replacement-intent-a",
        );
        let claim = pending_consumption_binding(&pending_a, consumer.clone())
            .expect("consumer binding created");
        let consumption = goal_store
            .prepare_intent_revision_consumption(&consumer, &claim)
            .expect("consumer receipt prepared");
        goal_store
            .record_intent_revision_consumption(&consumption)
            .expect("consumer receipt persisted");
        goal_store
            .append_code_workflow_durable(CodeWorkflowEventKind::InteractionResolved {
                interaction_id: "replacement-a".to_string(),
                resolution: "cancel".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            })
            .expect("replacement review resolved so a later Modify can open");
        clear_pending_intent_revision(&persistence).expect("consumed sidecar removed");
        goal_store
            .complete_code_command_success(&consumer.identity, "revision consumed")
            .expect("consumer command completed");

        let pending_b = persist_revision_test_source(
            &persistence,
            source("source-b"),
            "review-b",
            &revision_test_spec("revision B"),
            "note B",
        )
        .await;
        assert_eq!(
            pending_b
                .authority
                .as_ref()
                .map(|authority| authority.interaction_id.as_str()),
            Some("review-b")
        );

        assert!(
            verify_resolved_intent_revision_retry(
                &persistence,
                "review-a",
                Some(" note A ".to_string()),
                false,
            )
            .expect("old retry validated independently of the newer sidecar")
        );
        assert!(
            !verify_resolved_intent_revision_retry(
                &persistence,
                "review-a",
                Some("different note".to_string()),
                false,
            )
            .expect("different old retry rejected without fencing the newer sidecar")
        );
        assert!(matches!(
            load_intent_revision_sidecar(&persistence).expect("new sidecar remains readable"),
            Some(LoadedIntentRevisionSidecar::Active(active))
                if active.authority.as_ref().is_some_and(|authority| authority.interaction_id == "review-b")
        ));

        clear_pending_intent_revision(&persistence).expect("replace the B fixture sidecar");
        let _pending_c = persist_revision_test_source(
            &persistence,
            source("source-c"),
            "review-c",
            &revision_test_spec("revision C"),
            "note C",
        )
        .await;
        clear_pending_intent_revision(&persistence)
            .expect("simulate a missing unconsumed C sidecar");
        let pending_d = persist_revision_test_source(
            &persistence,
            source("source-d"),
            "review-d",
            &revision_test_spec("revision D"),
            "note D",
        )
        .await;

        assert!(
            verify_resolved_intent_revision_retry(
                &persistence,
                "review-c",
                Some("note C".to_string()),
                false,
            )
            .is_err(),
            "another sidecar must not hide a missing receipt for an unconsumed revision"
        );
        assert_eq!(
            pending_d
                .authority
                .as_ref()
                .map(|authority| authority.interaction_id.as_str()),
            Some("review-d")
        );
    }

    #[tokio::test]
    async fn uncommitted_consuming_sidecar_validation_preserves_the_envelope_across_reloads() {
        fn recover_once(persistence: &HeadlessSessionPersistence) -> PendingIntentRevision {
            let replay = persistence
                .goal_event_store()
                .load_code_workflow_replay_committed()
                .expect("startup replay");
            let consuming =
                match load_intent_revision_sidecar(persistence).expect("startup sidecar load") {
                    Some(LoadedIntentRevisionSidecar::Consuming(consuming)) => consuming,
                    _ => panic!("uncommitted consumer must remain a Consuming envelope"),
                };
            let authenticated =
                authenticate_active_intent_revision(persistence, &replay, consuming.active.clone())
                    .expect("Consuming Active body has exact source authority");
            assert_eq!(
                pending_consumption_binding(
                    &authenticated.pending,
                    consuming.consumption.claim.consumer_intent.clone(),
                )
                .expect("expected consumer binding"),
                consuming.consumption.claim
            );
            assert!(
                exact_intent_revision_consumer(&replay, &authenticated.terminal)
                    .expect("receipt scan")
                    .is_none()
            );
            validate_uncommitted_intent_revision_consumer(
                persistence,
                &replay,
                &authenticated.terminal,
                &consuming.consumption,
            )
            .expect("aborted consumer remains safe to restore in memory");
            authenticated.pending
        }

        let storage = tempfile::tempdir().expect("temporary session storage");
        let store = Arc::new(SessionStore::from_storage_path(storage.path()));
        let state = SessionState::new("/repo");
        let persistence =
            HeadlessSessionPersistence::new(store, state).expect("attach headless persistence");
        let goal_store = persistence.goal_event_store();
        let (_, repo_id, principal_id) = persistence.worker_durability_config();
        let session_id = persistence.durability_session_id().to_string();
        let command = |command_id: &str| {
            CodeCommandIntent::new(
                CodeCommandIdentity::new(
                    repo_id.clone(),
                    session_id.clone(),
                    principal_id.clone(),
                    command_id,
                ),
                CODE_UI_WEB_TURN_KIND,
                format!("sha256:{command_id}"),
                true,
            )
        };
        let pending = persist_revision_test_source(
            &persistence,
            command("source"),
            "review",
            &revision_test_spec("revision"),
            "keep this note",
        )
        .await;
        let consumer = command("aborted-consumer");
        goal_store
            .admit_code_command(consumer.clone())
            .expect("consumer intent admitted");
        let claim =
            pending_consumption_binding(&pending, consumer.clone()).expect("consumer claim bound");
        let consumption = goal_store
            .prepare_intent_revision_consumption(&consumer, &claim)
            .expect("consumer handoff prepared");
        persist_consuming_intent_revision(
            &persistence,
            &ConsumingIntentRevision {
                schema_version: INTENT_REVISION_SIDECAR_SCHEMA_VERSION,
                active: pending.clone(),
                consumption,
            },
        )
        .expect("Consuming envelope persisted before the simulated crash");

        let first_restore = recover_once(&persistence);
        assert_eq!(first_restore, pending);
        let second_restore = recover_once(&persistence);
        assert_eq!(second_restore, pending);
        assert!(matches!(
            load_intent_revision_sidecar(&persistence).expect("sidecar after two restores"),
            Some(LoadedIntentRevisionSidecar::Consuming(_))
        ));
    }

    #[tokio::test]
    async fn claiming_without_command_rearms_active_before_mutation_recovery() {
        let storage = tempfile::tempdir().expect("temporary session storage");
        let store = Arc::new(SessionStore::from_storage_path(storage.path()));
        let state = SessionState::new("/repo");
        let persistence =
            HeadlessSessionPersistence::new(store, state).expect("attach headless persistence");
        let goal_store = persistence.goal_event_store();
        let (_, repo_id, principal_id) = persistence.worker_durability_config();
        let session_id = persistence.durability_session_id().to_string();
        let command = |command_id: &str| {
            CodeCommandIntent::new(
                CodeCommandIdentity::new(
                    repo_id.clone(),
                    session_id.clone(),
                    principal_id.clone(),
                    command_id,
                ),
                CODE_UI_WEB_TURN_KIND,
                format!("sha256:{command_id}"),
                true,
            )
        };
        let pending = persist_revision_test_source(
            &persistence,
            command("claim-source"),
            "claim-review",
            &revision_test_spec("claim crash"),
            "retain claim note",
        )
        .await;
        let (pending, _) = prepare_claiming_intent_revision(
            &persistence,
            pending,
            command("never-admitted-consumer"),
        )
        .expect("Claiming prewrite persisted");
        assert!(matches!(
            load_intent_revision_sidecar(&persistence).expect("Claiming sidecar readable"),
            Some(LoadedIntentRevisionSidecar::Claiming(_))
        ));

        assert!(
            authenticated_uncommitted_intent_revision_consumer(&persistence)
                .expect("missing command is safely rearmed")
                .is_none()
        );
        assert!(matches!(
            load_intent_revision_sidecar(&persistence).expect("rearmed sidecar readable"),
            Some(LoadedIntentRevisionSidecar::Active(active)) if active == pending
        ));
        assert!(goal_store.load_code_workflow_replay().is_ok());
    }

    #[tokio::test]
    async fn claiming_pending_command_promotes_before_generic_recovery() {
        let storage = tempfile::tempdir().expect("temporary session storage");
        let store = Arc::new(SessionStore::from_storage_path(storage.path()));
        let state = SessionState::new("/repo");
        let persistence =
            HeadlessSessionPersistence::new(store, state).expect("attach headless persistence");
        let goal_store = persistence.goal_event_store();
        let (_, repo_id, principal_id) = persistence.worker_durability_config();
        let session_id = persistence.durability_session_id().to_string();
        let command = |command_id: &str| {
            CodeCommandIntent::new(
                CodeCommandIdentity::new(
                    repo_id.clone(),
                    session_id.clone(),
                    principal_id.clone(),
                    command_id,
                ),
                CODE_UI_WEB_TURN_KIND,
                format!("sha256:{command_id}"),
                true,
            )
        };
        let pending = persist_revision_test_source(
            &persistence,
            command("pending-source"),
            "pending-review",
            &revision_test_spec("pending claim crash"),
            "retain pending note",
        )
        .await;
        let consumer = command("pending-claimed-consumer");
        let (_, claim) = prepare_claiming_intent_revision(&persistence, pending, consumer.clone())
            .expect("Claiming prewrite persisted");
        goal_store
            .admit_code_command(consumer)
            .expect("consumer intent fsynced before crash");

        let consumption = authenticated_uncommitted_intent_revision_consumer(&persistence)
            .expect("Claiming resolves exact pending command")
            .expect("pending command is passed to generic recovery");
        assert_eq!(consumption.claim, claim);
        assert!(matches!(
            load_intent_revision_sidecar(&persistence).expect("promoted sidecar readable"),
            Some(LoadedIntentRevisionSidecar::Consuming(actual))
                if actual.consumption == consumption
        ));
        let recovery = goal_store
            .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
                None,
                None,
                Some(&consumption),
            )
            .expect("generic recovery heals only the attributed consumer");
        assert!(recovery.intent_revision_consumer_healed);
    }

    #[tokio::test]
    async fn claiming_canonical_cancel_and_double_attempt_remain_retryable() {
        let storage = tempfile::tempdir().expect("temporary session storage");
        let store = Arc::new(SessionStore::from_storage_path(storage.path()));
        let state = SessionState::new("/repo");
        let persistence =
            HeadlessSessionPersistence::new(store, state).expect("attach headless persistence");
        let goal_store = persistence.goal_event_store();
        let (_, repo_id, principal_id) = persistence.worker_durability_config();
        let session_id = persistence.durability_session_id().to_string();
        let command = |command_id: &str| {
            CodeCommandIntent::new(
                CodeCommandIdentity::new(
                    repo_id.clone(),
                    session_id.clone(),
                    principal_id.clone(),
                    command_id,
                ),
                CODE_UI_WEB_TURN_KIND,
                format!("sha256:{command_id}"),
                true,
            )
        };
        let pending = persist_revision_test_source(
            &persistence,
            command("double-source"),
            "double-review",
            &revision_test_spec("double claim crash"),
            "retain double-attempt note",
        )
        .await;
        let consumer_a = command("cancelled-consumer-a");
        prepare_claiming_intent_revision(&persistence, pending.clone(), consumer_a.clone())
            .expect("attempt A Claiming persisted");
        goal_store
            .admit_code_command(consumer_a.clone())
            .expect("attempt A intent admitted");
        goal_store
            .complete_code_command_failure(
                &consumer_a.identity,
                crate::internal::ai::session::jsonl::PRE_MUTATION_CANCELLED_COMMAND_REASON,
            )
            .expect("attempt A canonical pre-start cancel persisted");
        let recovered_a = authenticated_uncommitted_intent_revision_consumer(&persistence)
            .expect("attempt A cancellation is recoverable")
            .expect("attempt A retains exact Consuming attribution");
        assert_eq!(recovered_a.claim.consumer_intent, consumer_a);

        let consumer_b = command("never-admitted-consumer-b");
        prepare_claiming_intent_revision(&persistence, pending.clone(), consumer_b)
            .expect("attempt B may replace safely failed attempt A");
        let recovered_b = authenticated_uncommitted_intent_revision_consumer(&persistence)
            .expect("attempt B crash validates all earlier attempts")
            .expect("attempt A attribution is restored when B has no command");
        assert_eq!(recovered_b, recovered_a);
        assert!(matches!(
            load_intent_revision_sidecar(&persistence)
                .expect("double-attempt recovery sidecar readable"),
            Some(LoadedIntentRevisionSidecar::Consuming(actual))
                if actual.consumption == recovered_a
        ));

        let consumer_c = command("indeterminate-consumer-c");
        let (pending, claim_c) =
            prepare_claiming_intent_revision(&persistence, pending, consumer_c.clone())
                .expect("attempt C may replace safely failed attempt A");
        goal_store
            .admit_code_command(consumer_c.clone())
            .expect("attempt C intent admitted");
        let consumption_c =
            promote_claiming_intent_revision_after_admission(&persistence, &pending, &claim_c)
                .expect("attempt C promoted to event-bound Consuming");
        goal_store
            .mark_code_command_indeterminate(
                &consumer_c.identity,
                crate::internal::ai::session::jsonl::INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_EFFECT,
                crate::internal::ai::session::jsonl::INTENT_REVISION_PRE_RECEIPT_INDETERMINATE_REASON,
            )
            .expect("attempt C canonical pre-receipt Indeterminate persisted");
        assert_eq!(
            authenticated_uncommitted_intent_revision_consumer(&persistence)
                .expect("attempt C remains explicitly retryable")
                .expect("attempt C keeps exact attribution"),
            consumption_c
        );

        let consumer_d = command("never-admitted-consumer-d");
        prepare_claiming_intent_revision(&persistence, pending, consumer_d)
            .expect("attempt D may replace retryable Indeterminate attempt C");
        assert_eq!(
            authenticated_uncommitted_intent_revision_consumer(&persistence)
                .expect("attempt D crash validates the earlier canonical Indeterminate")
                .expect("attempt C attribution is restored when D has no command"),
            consumption_c
        );
    }

    #[test]
    fn projection_deltas_emit_thread_graph_clears() {
        use crate::internal::ai::web::code_ui::{
            CodeUiThreadGraph, CodeUiThreadGraphNode, initial_snapshot,
        };

        let mut previous = initial_snapshot(
            "/repo",
            crate::internal::ai::web::code_ui::CodeUiProviderInfo::default(),
            crate::internal::ai::web::code_ui::CodeUiCapabilities::default(),
        );
        previous.thread_graph = Some(CodeUiThreadGraph {
            thread_id: "thread-1".to_string(),
            title: None,
            selected_plan_id: None,
            active_task_id: None,
            active_run_id: None,
            nodes: vec![CodeUiThreadGraphNode {
                depth: 1,
                kind: "plan".to_string(),
                id: "plan-1".to_string(),
                label: "Plan 1".to_string(),
                tags: Vec::new(),
            }],
            ..Default::default()
        });
        let current = initial_snapshot(
            "/repo",
            crate::internal::ai::web::code_ui::CodeUiProviderInfo::default(),
            crate::internal::ai::web::code_ui::CodeUiCapabilities::default(),
        );

        let deltas = code_ui_projection_deltas(&previous, &current).expect("deltas");
        assert!(
            deltas.iter().any(|delta| matches!(
                delta,
                CodeWorkflowEventKind::CodeUiProjectionDelta {
                    projection,
                    payload,
                    ..
                } if projection == "thread_graph" && payload.is_null()
            )),
            "clearing thread_graph must persist a null v2 projection delta; got {deltas:?}"
        );
    }

    #[test]
    fn request_user_input_question_to_metadata_projects_browser_wire_fields() {
        use crate::internal::ai::tools::context::UserInputOption;

        let metadata = request_user_input_question_to_metadata(&UserInputQuestion {
            id: "risk".to_string(),
            header: "Risk".to_string(),
            question: "Pick a profile".to_string(),
            is_other: true,
            is_secret: true,
            options: Some(vec![
                UserInputOption {
                    label: "Low".to_string(),
                    description: "Safer".to_string(),
                },
                UserInputOption {
                    label: "   ".to_string(),
                    description: "blank".to_string(),
                },
                UserInputOption {
                    label: "Low".to_string(),
                    description: "duplicate".to_string(),
                },
                UserInputOption {
                    label: "High".to_string(),
                    description: "Faster".to_string(),
                },
            ]),
        });

        assert_eq!(metadata["id"], "risk");
        assert_eq!(metadata["header"], "Risk");
        assert_eq!(metadata["prompt"], "Pick a profile");
        assert_eq!(metadata["kind"], "single");
        assert_eq!(metadata["isOther"], true);
        assert_eq!(metadata["isSecret"], true);
        assert_eq!(
            metadata["options"],
            json!([
                {"id": "Low", "label": "Low", "description": "Safer"},
                {"id": "High", "label": "High", "description": "Faster"},
            ])
        );
    }

    #[test]
    fn headless_capabilities_advertise_projected_plan_and_patchset_surfaces() {
        let capabilities = headless_capabilities();

        assert!(capabilities.plan_updates);
        assert!(capabilities.patchsets);
        assert!(capabilities.tool_calls);
        assert!(capabilities.interactive_approvals);
    }

    #[test]
    fn plan_snapshot_from_update_plan_arguments_maps_steps() {
        let plan = plan_snapshot_from_update_plan_arguments(
            "plan-call",
            "running",
            &json!({
                "explanation": "updated",
                "plan": [
                    {"step": "Inspect", "status": "completed"},
                    {"step": "Patch", "status": "in_progress"}
                ]
            }),
        )
        .expect("valid update_plan arguments should produce a plan snapshot");

        assert_eq!(plan.id, "plan-call");
        assert_eq!(plan.summary.as_deref(), Some("updated"));
        assert_eq!(plan.status, "running");
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].status, "completed");
        assert_eq!(plan.steps[1].status, "in_progress");
    }

    #[test]
    fn patchset_snapshot_for_tool_result_uses_apply_patch_metadata() {
        let result = Ok(ToolOutput::success("ok").with_metadata(json!({
            "diffs": [
                {"path": "src/lib.rs", "type": "update", "diff": "@@ -1 +1 @@"}
            ]
        })));

        let patchset = patchset_snapshot_for_tool_result("patch-call", "completed", &result)
            .expect("apply_patch diff metadata should produce a patchset");

        assert_eq!(patchset.id, "patch-call");
        assert_eq!(patchset.status, "completed");
        assert_eq!(patchset.changes.len(), 1);
        assert_eq!(patchset.changes[0].path, "src/lib.rs");
        assert_eq!(patchset.changes[0].change_type, "update");
        assert_eq!(patchset.changes[0].diff.as_deref(), Some("@@ -1 +1 @@"));
    }

    #[tokio::test]
    async fn cancelled_phase1_retry_repairs_only_its_stale_thinking_projection() {
        let now = Utc::now();
        let phase1_turn_id = "phase1-retry-turn";
        let command = CodeCommandIntent::new(
            CodeCommandIdentity::new("repo", "session", "principal", phase1_turn_id),
            CODE_UI_WEB_TURN_KIND,
            "sha256:phase1-input",
            true,
        );
        let retry = Phase1RetryIntentReview {
            interaction_id: "intent-review-retry".to_string(),
            intent_id: "intent".to_string(),
            intent_spec_id: "intent-spec".to_string(),
            source_interaction_id: "intent-review-source".to_string(),
            source_resolution: "confirm".to_string(),
            source_phase1_turn_id: phase1_turn_id.to_string(),
            start_seed_digest: "a".repeat(64),
        };
        let mut events = vec![
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id: retry.source_interaction_id.clone(),
                resolution: retry.source_resolution.clone(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            },
            CodeWorkflowEventKind::CommandIntentPersisted {
                command: command.clone(),
            },
            CodeWorkflowEventKind::CommandTerminalFailure {
                command: command.identity,
                reason: "cancelled before formal write".to_string(),
                interaction_resolutions: Vec::new(),
                retry_intent_review: Some(retry.clone()),
            },
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id: retry.interaction_id.clone(),
                resolution: "cancel".to_string(),
                command: None,
                prior_interaction_resolutions: Vec::new(),
                intent_revision_consumption: None,
            },
        ];
        let mut snapshot = CodeUiSessionSnapshot {
            status: CodeUiSessionStatus::Thinking,
            interactions: vec![CodeUiInteractionRequest {
                id: retry.source_interaction_id.clone(),
                kind: CodeUiInteractionKind::IntentReviewChoice,
                status: CodeUiInteractionStatus::Resolved,
                metadata: json!({}),
                requested_at: now,
                resolved_at: Some(now),
                ..Default::default()
            }],
            transcript: vec![
                CodeUiTranscriptEntry {
                    id: "phase1-stream".to_string(),
                    kind: CodeUiTranscriptEntryKind::AssistantMessage,
                    content: Some("partial".to_string()),
                    status: Some("streaming".to_string()),
                    streaming: true,
                    metadata: json!({"phase": "plan"}),
                    created_at: now,
                    updated_at: now,
                    ..Default::default()
                },
                CodeUiTranscriptEntry {
                    id: "phase1-tool".to_string(),
                    kind: CodeUiTranscriptEntryKind::ToolCall,
                    status: Some("running".to_string()),
                    created_at: now,
                    updated_at: now,
                    ..Default::default()
                },
            ],
            tool_calls: vec![CodeUiToolCallSnapshot {
                id: "phase1-tool".to_string(),
                tool_name: "update_plan".to_string(),
                status: "running".to_string(),
                updated_at: now,
                ..Default::default()
            }],
            plans: vec![CodeUiPlanSnapshot {
                id: "phase1-tool".to_string(),
                status: "running".to_string(),
                updated_at: now,
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(
            cancelled_phase1_retry_projection_lineage(events.iter(), &snapshot)
                .expect("validate cancelled retry lineage"),
            Some(retry)
        );
        let session = CodeUiSession::new(snapshot.clone());
        finalize_terminal_phase1_projection(
            &session,
            "Phase 1 planning cancelled before any formal write",
            "cancelled",
        )
        .await;
        let repaired = session.snapshot().await;
        assert_eq!(repaired.status, CodeUiSessionStatus::Idle);
        assert!(repaired.transcript.iter().all(|entry| !entry.streaming));
        assert_eq!(repaired.tool_calls[0].status, "cancelled");
        assert_eq!(repaired.transcript[1].status.as_deref(), Some("cancelled"));
        assert_eq!(repaired.plans[0].status, "failed");

        let revision_session = CodeUiSession::new(snapshot.clone());
        finalize_terminal_phase1_projection(
            &revision_session,
            "Phase 1 plan revision failed before any formal write",
            "error",
        )
        .await;
        let revision_repaired = revision_session.snapshot().await;
        assert_eq!(revision_repaired.status, CodeUiSessionStatus::Idle);
        assert_eq!(
            revision_repaired.transcript[0].status.as_deref(),
            Some("error")
        );
        assert_eq!(revision_repaired.tool_calls[0].status, "failed");
        assert_eq!(
            revision_repaired.transcript[1].status.as_deref(),
            Some("failed")
        );

        events.push(CodeWorkflowEventKind::CommandIntentPersisted {
            command: CodeCommandIntent::new(
                CodeCommandIdentity::new("repo", "session", "principal", "ordinary-turn"),
                CODE_UI_WEB_TURN_KIND,
                "sha256:ordinary",
                false,
            ),
        });
        snapshot.status = CodeUiSessionStatus::Thinking;
        assert_eq!(
            cancelled_phase1_retry_projection_lineage(events.iter(), &snapshot)
                .expect("ignore historical retry after a later ordinary turn"),
            None
        );
    }
}
