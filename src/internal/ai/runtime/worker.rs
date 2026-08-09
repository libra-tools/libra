//! UI-neutral serialized turn runtime for `libra code` and future adapters.
//!
//! This module owns session-local turn ordering and the observable interaction
//! state.  It deliberately does not know about TUI, Web, MCP, or a provider:
//! those callers submit typed requests through [`AgentRuntimeHandle`], while a
//! [`RuntimeTurnExecutor`] adapts the existing provider/tool-loop stack.  A
//! mutating executor receives the same [`ToolBoundaryRuntime`] that backs the
//! registry hardening policy and must dispatch through the existing
//! `ToolRuntimeContext` approval/sandbox path rather than creating a second
//! permission system.

use std::{
    collections::{HashMap, VecDeque},
    future,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;

use super::{BoundaryDecision, RuntimeCommandDurability, ToolBoundaryRuntime, ToolOperation};
use crate::internal::ai::session::{CodeCommandAdmission, CodeCommandIdentity, CodeCommandIntent};

/// A monotonically increasing event position within one runtime session.
///
/// A sequence number alone is ambiguous because one worker may serve multiple
/// sessions concurrently.  Keeping the session identity in the cursor makes
/// cross-session broadcast fan-out impossible to misinterpret as one stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventCursor {
    pub session_id: String,
    pub sequence: u64,
}

impl EventCursor {
    pub fn new(session_id: impl Into<String>, sequence: u64) -> Self {
        Self {
            session_id: session_id.into(),
            sequence,
        }
    }
}

/// Typed state exposed to UI adapters.  Waiting states intentionally carry
/// identifiers and safe summaries only; raw prompts and tool arguments remain
/// in the session/persistence layer where redaction rules already apply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum InteractionState {
    Idle,
    Queued,
    Running,
    AwaitingIntentReview {
        interaction_id: String,
    },
    AwaitingPlanReview {
        interaction_id: String,
    },
    AwaitingNetworkPolicy {
        interaction_id: String,
    },
    AwaitingToolApproval {
        interaction_id: String,
        tool_name: String,
    },
    AwaitingUserInput {
        interaction_id: String,
    },
    Cancelling,
    Completed,
    Failed {
        reason: String,
    },
    Cancelled,
    IndeterminateSideEffect {
        reason: String,
    },
}

impl InteractionState {
    /// Whether a response can advance this state back into execution.
    pub fn is_awaiting_response(&self) -> bool {
        matches!(
            self,
            Self::AwaitingIntentReview { .. }
                | Self::AwaitingPlanReview { .. }
                | Self::AwaitingNetworkPolicy { .. }
                | Self::AwaitingToolApproval { .. }
                | Self::AwaitingUserInput { .. }
        )
    }

    fn interaction_id(&self) -> Option<&str> {
        match self {
            Self::AwaitingIntentReview { interaction_id }
            | Self::AwaitingPlanReview { interaction_id }
            | Self::AwaitingNetworkPolicy { interaction_id }
            | Self::AwaitingUserInput { interaction_id } => Some(interaction_id),
            Self::AwaitingToolApproval { interaction_id, .. } => Some(interaction_id),
            Self::Idle
            | Self::Queued
            | Self::Running
            | Self::Cancelling
            | Self::Completed
            | Self::Failed { .. }
            | Self::Cancelled
            | Self::IndeterminateSideEffect { .. } => None,
        }
    }
}

/// The typed transition owner for one session's current interaction.  The
/// worker is the only production caller that advances it; adapters receive a
/// copied [`InteractionState`] in [`AgentSnapshot`] and therefore cannot
/// mutate runtime state directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnStateMachine {
    state: InteractionState,
}

impl TurnStateMachine {
    pub fn new() -> Self {
        Self {
            state: InteractionState::Idle,
        }
    }

    pub fn state(&self) -> &InteractionState {
        &self.state
    }

    fn transition(&mut self, state: InteractionState) {
        self.state = state;
    }
}

impl Default for TurnStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

/// A request accepted by the serialized queue.  `input` is passed only to the
/// executor; it is never copied into runtime events.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRequest {
    pub session_id: String,
    pub turn_id: String,
    pub input: String,
    pub mutating: bool,
}

impl TurnRequest {
    pub fn new(
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        input: impl Into<String>,
        mutating: bool,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            input: input.into(),
            mutating,
        }
    }
}

/// A response to a pending interaction.  The response remains opaque to the
/// worker so the executor can apply the appropriate approval/input semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionResponse {
    pub interaction_id: String,
    pub response: String,
}

impl InteractionResponse {
    pub fn new(interaction_id: impl Into<String>, response: impl Into<String>) -> Self {
        Self {
            interaction_id: interaction_id.into(),
            response: response.into(),
        }
    }
}

/// A stable reference to an accepted turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnReceipt {
    pub session_id: String,
    pub turn_id: String,
}

/// A snapshot is a projection of worker-owned state at [`EventCursor`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub session_id: String,
    pub cursor: EventCursor,
    pub active_turn_id: Option<String>,
    pub queued_turns: usize,
    pub interaction: InteractionState,
}

/// Normalized event payload.  No variant carries raw request text, response
/// text, environment values, or tool arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum AgentEventKind {
    TurnQueued,
    TurnStarted,
    InteractionRequested { state: InteractionState },
    InteractionResponded { interaction_id: String },
    CancelRequested,
    TurnCompleted { summary: String },
    TurnFailed { reason: String },
    TurnCancelled,
    TurnIndeterminateSideEffect { reason: String },
}

/// A single observable runtime event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub cursor: EventCursor,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub kind: AgentEventKind,
}

/// Result of one executor step.  An interaction leaves the turn active and
/// blocks later turns for that session until it is answered or cancelled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeTurnExecution {
    Completed {
        summary: String,
    },
    AwaitingInteraction(InteractionState),
    /// The executor delivered an interaction response to a continuation that
    /// is still executing (for example a tool-loop handler awaiting a
    /// oneshot). This acknowledges the response without finishing the turn.
    InteractionResponseDelivered,
}

/// Context supplied to an executor.  The worker creates this once per active
/// turn, so adapters cannot replace the boundary policy between submit and
/// tool dispatch.
#[derive(Clone)]
pub struct RuntimeExecutionContext {
    tool_boundary: ToolBoundaryRuntime,
    cancellation: CancellationToken,
    mutation_started: Arc<AtomicBool>,
}

impl RuntimeExecutionContext {
    /// Apply the shared hardening policy before routing a tool operation.
    pub fn authorize(&self, operation: &ToolOperation) -> BoundaryDecision {
        self.tool_boundary.decide(operation)
    }

    /// The cancellation token that tool-loop adapters must observe before
    /// starting another model/tool iteration or a retry.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Mark the point at which a mutating tool has actually started its side
    /// effect. A subsequent cancel is then a reconciliation request, not a
    /// hard-abort signal. Production tool-loop adapters must call this after
    /// durable intent persistence and immediately before dispatch.
    pub fn mark_mutation_started(&self) {
        self.mutation_started.store(true, Ordering::Release);
    }

    pub fn mutation_started(&self) -> bool {
        self.mutation_started.load(Ordering::Acquire)
    }

    /// Share the worker-owned mutation boundary with an existing tool-loop
    /// adapter. The adapter must only pass this marker to the shared
    /// cancellation bridge; it must not replace it with an adapter-local
    /// marker, or a post-dispatch cancellation could be misclassified as a
    /// safe cooperative cancellation.
    pub fn mutation_started_marker(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.mutation_started)
    }

    /// Exposes the shared boundary runtime for registry construction/audit.
    /// The executor must still use `ToolRuntimeContext` for sandbox and human
    /// approval; the worker intentionally does not duplicate those mechanisms.
    pub fn tool_boundary(&self) -> &ToolBoundaryRuntime {
        &self.tool_boundary
    }
}

/// Adapter from the serialized worker to the existing provider/tool-loop
/// execution path.  A production implementation must attach
/// `context.tool_boundary()` to its `ToolRegistry` and retain the existing
/// `ToolRuntimeContext` approval/sandbox configuration.
#[async_trait]
pub trait RuntimeTurnExecutor: Send + Sync + 'static {
    async fn execute(
        &self,
        request: TurnRequest,
        context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError>;

    async fn respond(
        &self,
        request: TurnRequest,
        interaction: InteractionResponse,
        context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        let _ = (request, interaction, context);
        Err(RuntimeWorkerError::ExecutorDoesNotSupportResponses)
    }
}

/// Placeholder executor for adapters that use [`AgentRuntimeHandle`] solely
/// for external-turn admission and cancellation authority.
#[derive(Default)]
pub struct ExternalTurnTrackingExecutor;

#[async_trait]
impl RuntimeTurnExecutor for ExternalTurnTrackingExecutor {
    async fn execute(
        &self,
        _request: TurnRequest,
        _context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        Err(RuntimeWorkerError::ExecutionFailed(
            "external tracking executor must not execute submitted turns".to_string(),
        ))
    }
}

/// A continuation for an interaction emitted by a long-lived executor.
///
/// The worker owns this continuation while the interaction is pending, so an
/// adapter cannot independently remove or resolve it.  Implementations must
/// validate the opaque response before making the continuation observable to
/// a tool loop.  In particular, any durable interaction audit required by an
/// adapter belongs in [`Self::deliver`] before its one-shot sender is used.
#[async_trait]
pub trait RuntimeInteractionDelivery: Send + 'static {
    /// Reject malformed input without changing the worker-owned pending
    /// interaction state, so callers can correct a browser/automation form
    /// error instead of losing the continuation.
    fn validate(&self, interaction: &InteractionResponse) -> Result<(), RuntimeWorkerError>;

    /// Persist and release the continuation.  This runs outside the actor but
    /// remains serialized by the active turn slot until it returns.
    async fn deliver(
        self: Box<Self>,
        request: TurnRequest,
        interaction: InteractionResponse,
        context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError>;
}

/// Configuration for one worker.  Queue and fan-out bounds are explicit so a
/// bad adapter cannot accumulate unbounded pending turns or observer events.
#[derive(Clone)]
pub struct AgentRuntimeWorkerConfig {
    pub executor: Arc<dyn RuntimeTurnExecutor>,
    pub tool_boundary: ToolBoundaryRuntime,
    /// Optional session command log used to make runtime cancellation outcomes
    /// recoverable across a process restart.
    pub durability: Option<RuntimeCommandDurability>,
    pub max_queued_turns_per_session: usize,
    pub command_buffer: usize,
    pub event_buffer: usize,
    /// Upper bound for cooperative worker shutdown. The timeout is deliberately
    /// owned by the runtime rather than a UI adapter so every caller receives
    /// the same diagnostic if an executor does not release its resources.
    pub shutdown_timeout: Duration,
    pub durability_repo_id: Option<String>,
    pub durability_principal_id: Option<String>,
    /// Command kind persisted for runtime-owned durable intents. Headless
    /// adapters set this to match their browser direct-turn admission record.
    pub durability_command_kind: Option<String>,
    /// Sessions with a recovered mutating command are created in a
    /// reconciliation fence before the worker reads its first turn request.
    pub recovered_reconciliation_sessions: Vec<String>,
}

impl AgentRuntimeWorkerConfig {
    pub fn new(executor: Arc<dyn RuntimeTurnExecutor>, tool_boundary: ToolBoundaryRuntime) -> Self {
        Self {
            executor,
            tool_boundary,
            durability: None,
            max_queued_turns_per_session: 16,
            command_buffer: 128,
            event_buffer: 1_024,
            shutdown_timeout: Duration::from_secs(30),
            durability_repo_id: None,
            durability_principal_id: None,
            durability_command_kind: None,
            recovered_reconciliation_sessions: Vec::new(),
        }
    }

    /// Attach the session durability owner and the stable identity fields used
    /// for each accepted turn's command intent.
    pub fn with_durability(
        mut self,
        durability: RuntimeCommandDurability,
        repo_id: impl Into<String>,
        principal_id: impl Into<String>,
    ) -> Self {
        self.durability = Some(durability);
        self.durability_repo_id = Some(repo_id.into());
        self.durability_principal_id = Some(principal_id.into());
        self
    }

    /// Override the durable command kind written by this worker. Production
    /// headless runtimes set this to their browser direct-turn kind so cancel
    /// reconciliation shares the same JSONL record as adapter admission.
    pub fn with_durability_command_kind(mut self, command_kind: impl Into<String>) -> Self {
        self.durability_command_kind = Some(command_kind.into());
        self
    }

    /// Fence recovered sessions so a restarted runtime cannot blindly accept
    /// a new turn after an interrupted mutation.
    pub fn with_recovered_reconciliation_session(mut self, session_id: impl Into<String>) -> Self {
        self.recovered_reconciliation_sessions
            .push(session_id.into());
        self
    }
}

/// Map internal runtime worker failures to stable adapter/wire messages.
///
/// Public adapters use this helper instead of exposing `RuntimeWorkerError`
/// variants directly so JSON clients can match on stable codes such as
/// `RECONCILIATION_REQUIRED`.
pub fn runtime_worker_adapter_message(error: RuntimeWorkerError) -> String {
    match error {
        RuntimeWorkerError::ReconciliationRequired { session_id } => format!(
            "RECONCILIATION_REQUIRED: session '{session_id}' requires mutation reconciliation before another turn can run"
        ),
        other => other.to_string(),
    }
}

/// Fail-closed errors returned by the worker API.  These are internal runtime
/// errors today; public adapters map them to their own stable CLI/API error.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuntimeWorkerError {
    #[error("the AgentRuntime worker stopped before the command could be processed")]
    WorkerStopped,
    #[error("the AgentRuntime worker dropped the command response")]
    ResponseDropped,
    #[error("session '{session_id}' has no active runtime state")]
    UnknownSession { session_id: String },
    #[error("turn request requires non-empty session and turn identifiers")]
    InvalidTurnIdentifier,
    #[error("turn '{turn_id}' already exists in session '{session_id}'")]
    DuplicateTurn { session_id: String, turn_id: String },
    #[error("session '{session_id}' already has {limit} queued turns")]
    QueueFull { session_id: String, limit: usize },
    #[error("session '{session_id}' requires mutation reconciliation before another turn can run")]
    ReconciliationRequired { session_id: String },
    #[error("turn '{turn_id}' is not active or queued in session '{session_id}'")]
    UnknownTurn { session_id: String, turn_id: String },
    #[error("interaction '{interaction_id}' is not pending for turn '{turn_id}'")]
    UnknownInteraction {
        turn_id: String,
        interaction_id: String,
    },
    #[error("turn '{turn_id}' cannot register a non-interactive runtime state")]
    InvalidInteractionState { turn_id: String },
    #[error("turn '{turn_id}' already has a pending interaction")]
    InteractionAlreadyPending { turn_id: String },
    #[error("turn '{turn_id}' has already stopped executing and cannot register a new interaction")]
    InteractionRegistrationClosed { turn_id: String },
    #[error("runtime executor does not support interaction responses")]
    ExecutorDoesNotSupportResponses,
    #[error("runtime executor failed: {0}")]
    ExecutionFailed(String),
    #[error("runtime turn reached an indeterminate side effect: {0}")]
    IndeterminateSideEffect(String),
    #[error("runtime turn was cancelled")]
    Cancelled,
    #[error("runtime command durability failed: {0}")]
    DurabilityFailure(String),
    #[error("the AgentRuntime worker is shutting down and cannot accept new commands")]
    ShuttingDown,
}

/// Result of a structured runtime shutdown. The resource categories avoid
/// leaking user prompts, command arguments, or environment values while still
/// telling callers what failed to release before the deadline.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuntimeShutdownError {
    #[error("the AgentRuntime worker stopped before shutdown could begin")]
    WorkerStopped,
    #[error("the AgentRuntime worker dropped the shutdown response")]
    ResponseDropped,
    #[error("AgentRuntime shutdown timed out waiting for: {unreleased_resources:?}")]
    TimedOut { unreleased_resources: Vec<String> },
    #[error(
        "AgentRuntime shutdown lifecycle state is unavailable after an internal synchronization failure"
    )]
    LifecycleStateUnavailable,
}

/// The command vocabulary processed by [`AgentRuntimeWorker`].  Reply
/// channels intentionally stay internal to the handle/worker boundary: they
/// are not persistence or wire-format types.
pub enum RuntimeCommand {
    Submit {
        request: TurnRequest,
        reply: oneshot::Sender<Result<TurnReceipt, RuntimeWorkerError>>,
    },
    /// Admit a turn executed by an existing adapter-owned loop. The runtime
    /// still owns admission, cancellation, durability, and reconciliation;
    /// the adapter reports its terminal result through `finish_external_turn`.
    TrackExternalTurn {
        request: TurnRequest,
        cancellation: CancellationToken,
        mutation_started: Arc<AtomicBool>,
        reply: oneshot::Sender<Result<TurnReceipt, RuntimeWorkerError>>,
    },
    Respond {
        session_id: String,
        turn_id: String,
        interaction: InteractionResponse,
        reply: oneshot::Sender<Result<(), RuntimeWorkerError>>,
    },
    RegisterInteraction {
        session_id: String,
        turn_id: String,
        interaction: InteractionState,
        delivery: Option<Box<dyn RuntimeInteractionDelivery>>,
        reply: oneshot::Sender<Result<(), RuntimeWorkerError>>,
    },
    Cancel {
        session_id: String,
        turn_id: String,
        reply: oneshot::Sender<Result<(), RuntimeWorkerError>>,
    },
    Snapshot {
        session_id: String,
        reply: oneshot::Sender<Result<AgentSnapshot, RuntimeWorkerError>>,
    },
    Observe {
        cursor: EventCursor,
        reply: oneshot::Sender<AgentEventStream>,
    },
    Shutdown {
        reply: Option<oneshot::Sender<Result<(), RuntimeShutdownError>>>,
    },
    ExecutionFinished {
        session_id: String,
        turn_id: String,
        result: Result<RuntimeTurnExecution, RuntimeWorkerError>,
    },
    /// Completion of an executor-side interaction response delivery.  This is
    /// separate from [`Self::ExecutionFinished`] because a response may resume
    /// a still-running tool loop rather than terminate its turn.
    ResponseFinished {
        session_id: String,
        turn_id: String,
        interaction_id: String,
        result: Result<RuntimeTurnExecution, RuntimeWorkerError>,
        reply: oneshot::Sender<Result<(), RuntimeWorkerError>>,
    },
}

/// Thin UI-neutral client handle.  It owns no session or interaction state.
pub struct AgentRuntimeHandle {
    client: Arc<RuntimeClient>,
}

struct RuntimeClient {
    command_tx: mpsc::Sender<RuntimeCommand>,
    lifecycle_tx: mpsc::UnboundedSender<RuntimeLifecycleCommand>,
    live_handles: AtomicUsize,
    shutdown_result: Arc<std::sync::Mutex<Option<Result<(), RuntimeShutdownError>>>>,
}

enum RuntimeLifecycleCommand {
    LastHandleDropped,
}

impl Clone for AgentRuntimeHandle {
    fn clone(&self) -> Self {
        self.client.live_handles.fetch_add(1, Ordering::Relaxed);
        Self {
            client: Arc::clone(&self.client),
        }
    }
}

impl Drop for AgentRuntimeHandle {
    fn drop(&mut self) {
        if self.client.live_handles.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self
                .client
                .lifecycle_tx
                .send(RuntimeLifecycleCommand::LastHandleDropped);
        }
    }
}

impl AgentRuntimeHandle {
    fn recorded_shutdown_result(
        &self,
    ) -> Result<Option<Result<(), RuntimeShutdownError>>, RuntimeShutdownError> {
        self.client
            .shutdown_result
            .lock()
            .map(|result| result.clone())
            .map_err(|_| RuntimeShutdownError::LifecycleStateUnavailable)
    }

    pub async fn submit(&self, request: TurnRequest) -> Result<TurnReceipt, RuntimeWorkerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.client
            .command_tx
            .send(RuntimeCommand::Submit {
                request,
                reply: reply_tx,
            })
            .await
            .map_err(|_| RuntimeWorkerError::WorkerStopped)?;
        reply_rx
            .await
            .map_err(|_| RuntimeWorkerError::ResponseDropped)?
    }

    /// Register an adapter-owned turn with the runtime control plane.
    ///
    /// This is for legacy loops which cannot yet execute through
    /// `RuntimeTurnExecutor`. Their cancellation token and mutation marker are
    /// shared with the runtime so `cancel` applies the same safety fence.
    pub async fn track_external_turn(
        &self,
        request: TurnRequest,
        cancellation: CancellationToken,
        mutation_started: Arc<AtomicBool>,
    ) -> Result<TurnReceipt, RuntimeWorkerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.client
            .command_tx
            .send(RuntimeCommand::TrackExternalTurn {
                request,
                cancellation,
                mutation_started,
                reply: reply_tx,
            })
            .await
            .map_err(|_| RuntimeWorkerError::WorkerStopped)?;
        reply_rx
            .await
            .map_err(|_| RuntimeWorkerError::ResponseDropped)?
    }

    /// Report the terminal outcome of an adapter-owned tracked turn.
    pub async fn finish_external_turn(
        &self,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        result: Result<RuntimeTurnExecution, RuntimeWorkerError>,
    ) -> Result<(), RuntimeWorkerError> {
        let session_id = session_id.into();
        self.client
            .command_tx
            .send(RuntimeCommand::ExecutionFinished {
                session_id: session_id.clone(),
                turn_id: turn_id.into(),
                result,
            })
            .await
            .map_err(|_| RuntimeWorkerError::WorkerStopped)?;

        // The snapshot request is ordered after ExecutionFinished on the
        // worker mailbox. It therefore acknowledges that terminal durability
        // has settled before an adapter reports the turn as finalized.
        match self.snapshot(session_id.clone()).await?.interaction {
            InteractionState::Completed | InteractionState::Cancelled => Ok(()),
            InteractionState::IndeterminateSideEffect { .. } => {
                Err(RuntimeWorkerError::ReconciliationRequired { session_id })
            }
            InteractionState::Failed { reason } => Err(RuntimeWorkerError::ExecutionFailed(reason)),
            state => Err(RuntimeWorkerError::ExecutionFailed(format!(
                "external turn finalization did not reach a terminal state: {state:?}"
            ))),
        }
    }

    pub async fn respond(
        &self,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        interaction: InteractionResponse,
    ) -> Result<(), RuntimeWorkerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.client
            .command_tx
            .send(RuntimeCommand::Respond {
                session_id: session_id.into(),
                turn_id: turn_id.into(),
                interaction,
                reply: reply_tx,
            })
            .await
            .map_err(|_| RuntimeWorkerError::WorkerStopped)?;
        reply_rx
            .await
            .map_err(|_| RuntimeWorkerError::ResponseDropped)?
    }

    /// Register a typed interaction emitted by a long-lived executor. Unlike
    /// [`RuntimeTurnExecution::AwaitingInteraction`], this keeps the original
    /// execution future active while a tool-loop handler awaits the response.
    /// The worker remains the only owner of the observable state transition.
    pub async fn register_interaction(
        &self,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        interaction: InteractionState,
    ) -> Result<(), RuntimeWorkerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.client
            .command_tx
            .send(RuntimeCommand::RegisterInteraction {
                session_id: session_id.into(),
                turn_id: turn_id.into(),
                interaction,
                delivery: None,
                reply: reply_tx,
            })
            .await
            .map_err(|_| RuntimeWorkerError::WorkerStopped)?;
        reply_rx
            .await
            .map_err(|_| RuntimeWorkerError::ResponseDropped)?
    }

    /// Register an interaction whose continuation is owned by the runtime
    /// until a validated response is durably delivered.  This is the adapter
    /// migration path for tool approvals and structured user-input requests;
    /// the legacy [`Self::register_interaction`] method remains for executors
    /// that implement response delivery themselves.
    pub async fn register_interaction_with_delivery(
        &self,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        interaction: InteractionState,
        delivery: Box<dyn RuntimeInteractionDelivery>,
    ) -> Result<(), RuntimeWorkerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.client
            .command_tx
            .send(RuntimeCommand::RegisterInteraction {
                session_id: session_id.into(),
                turn_id: turn_id.into(),
                interaction,
                delivery: Some(delivery),
                reply: reply_tx,
            })
            .await
            .map_err(|_| RuntimeWorkerError::WorkerStopped)?;
        reply_rx
            .await
            .map_err(|_| RuntimeWorkerError::ResponseDropped)?
    }

    pub async fn cancel(
        &self,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Result<(), RuntimeWorkerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.client
            .command_tx
            .send(RuntimeCommand::Cancel {
                session_id: session_id.into(),
                turn_id: turn_id.into(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| RuntimeWorkerError::WorkerStopped)?;
        reply_rx
            .await
            .map_err(|_| RuntimeWorkerError::ResponseDropped)?
    }

    pub async fn snapshot(
        &self,
        session_id: impl Into<String>,
    ) -> Result<AgentSnapshot, RuntimeWorkerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.client
            .command_tx
            .send(RuntimeCommand::Snapshot {
                session_id: session_id.into(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| RuntimeWorkerError::WorkerStopped)?;
        reply_rx
            .await
            .map_err(|_| RuntimeWorkerError::ResponseDropped)?
    }

    /// Subscribe to events strictly after `cursor`.  A lagged consumer gets a
    /// concrete error and must recover from durable session state; it is never
    /// allowed to make this bounded in-memory channel grow without limit.
    pub async fn observe(
        &self,
        cursor: EventCursor,
    ) -> Result<AgentEventStream, RuntimeWorkerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.client
            .command_tx
            .send(RuntimeCommand::Observe {
                cursor,
                reply: reply_tx,
            })
            .await
            .map_err(|_| RuntimeWorkerError::WorkerStopped)?;
        reply_rx
            .await
            .map_err(|_| RuntimeWorkerError::ResponseDropped)
    }

    /// Stop the worker through its structured lifecycle path. Repeated calls
    /// join the same in-progress shutdown and receive the same terminal
    /// outcome rather than racing to abort executor tasks.
    pub async fn shutdown(&self) -> Result<(), RuntimeShutdownError> {
        if let Some(result) = self.recorded_shutdown_result()? {
            return result;
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .client
            .command_tx
            .send(RuntimeCommand::Shutdown {
                reply: Some(reply_tx),
            })
            .await
            .is_err()
        {
            return self
                .recorded_shutdown_result()?
                .unwrap_or(Err(RuntimeShutdownError::WorkerStopped));
        }
        reply_rx
            .await
            .map_err(|_| RuntimeShutdownError::ResponseDropped)?
    }
}

/// A cursor-filtered, session-scoped bounded live event stream.
pub struct AgentEventStream {
    after_cursor: EventCursor,
    receiver: broadcast::Receiver<AgentEvent>,
}

impl AgentEventStream {
    pub async fn recv(&mut self) -> Result<AgentEvent, RuntimeObserveError> {
        loop {
            match self.receiver.recv().await {
                Ok(event)
                    if event.cursor.session_id == self.after_cursor.session_id
                        && event.cursor.sequence > self.after_cursor.sequence =>
                {
                    self.after_cursor = event.cursor.clone();
                    return Ok(event);
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    return Err(RuntimeObserveError::Lagged { skipped });
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(RuntimeObserveError::Closed);
                }
            }
        }
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuntimeObserveError {
    #[error("event consumer lagged by {skipped} events; recover from persisted session state")]
    Lagged { skipped: u64 },
    #[error("AgentRuntime event stream closed")]
    Closed,
}

enum WorkerInput {
    Deadline,
    Lifecycle(Option<RuntimeLifecycleCommand>),
    Command(Option<RuntimeCommand>),
}

/// Owns session-local queues and dispatches executor work.  The worker is
/// intentionally an actor: its state is mutated only by [`RuntimeCommand`]
/// handling, while provider/tool-loop work runs outside the actor and reports
/// its outcome back through `ExecutionFinished`.
pub struct AgentRuntimeWorker {
    config: AgentRuntimeWorkerConfig,
    command_tx: mpsc::Sender<RuntimeCommand>,
    sessions: HashMap<String, SessionQueue>,
    shutdown: Option<ShutdownState>,
    shutdown_result: Arc<std::sync::Mutex<Option<Result<(), RuntimeShutdownError>>>>,
}

impl AgentRuntimeWorker {
    /// Spawn a worker task and return the only adapter-facing handle.
    pub fn spawn(config: AgentRuntimeWorkerConfig) -> (AgentRuntimeHandle, JoinHandle<()>) {
        let command_buffer = config.command_buffer.max(1);
        let (command_tx, command_rx) = mpsc::channel(command_buffer);
        let (lifecycle_tx, lifecycle_rx) = mpsc::unbounded_channel();
        let shutdown_result = Arc::new(std::sync::Mutex::new(None));
        let handle = AgentRuntimeHandle {
            client: Arc::new(RuntimeClient {
                command_tx: command_tx.clone(),
                lifecycle_tx,
                live_handles: AtomicUsize::new(1),
                shutdown_result: Arc::clone(&shutdown_result),
            }),
        };
        let mut sessions = HashMap::new();
        for session_id in &config.recovered_reconciliation_sessions {
            let mut session = SessionQueue::new(session_id.clone(), config.event_buffer.max(1));
            session.set_state(InteractionState::IndeterminateSideEffect {
                reason: "runtime recovered an interrupted mutating command; manual reconciliation is required"
                    .to_string(),
            });
            sessions.insert(session_id.clone(), session);
        }
        let worker = Self {
            config,
            command_tx,
            sessions,
            shutdown: None,
            shutdown_result,
        };
        let task = tokio::spawn(worker.run(command_rx, lifecycle_rx));
        (handle, task)
    }

    async fn run(
        mut self,
        mut command_rx: mpsc::Receiver<RuntimeCommand>,
        mut lifecycle_rx: mpsc::UnboundedReceiver<RuntimeLifecycleCommand>,
    ) {
        let mut lifecycle_open = true;
        loop {
            let deadline = self.shutdown.as_ref().map(|shutdown| shutdown.deadline);
            let input = tokio::select! {
                _ = wait_for_shutdown_deadline(deadline) => {
                    WorkerInput::Deadline
                }
                lifecycle = lifecycle_rx.recv(), if lifecycle_open => WorkerInput::Lifecycle(lifecycle),
                command = command_rx.recv() => WorkerInput::Command(command),
            };
            match input {
                WorkerInput::Deadline => {
                    let error = RuntimeShutdownError::TimedOut {
                        unreleased_resources: self.unreleased_resource_categories(),
                    };
                    self.finish_shutdown(Err(error));
                    break;
                }
                WorkerInput::Lifecycle(Some(RuntimeLifecycleCommand::LastHandleDropped)) => {
                    self.begin_shutdown(None);
                }
                WorkerInput::Lifecycle(None) => lifecycle_open = false,
                WorkerInput::Command(None) => {
                    if self.shutdown.is_some() {
                        let error = RuntimeShutdownError::TimedOut {
                            unreleased_resources: self.unreleased_resource_categories(),
                        };
                        self.finish_shutdown(Err(error));
                    }
                    break;
                }
                WorkerInput::Command(Some(command)) => match command {
                    RuntimeCommand::Submit { request, reply } => {
                        let _ = reply.send(self.submit(request));
                    }
                    RuntimeCommand::TrackExternalTurn {
                        request,
                        cancellation,
                        mutation_started,
                        reply,
                    } => {
                        let _ = reply.send(self.track_external_turn(
                            request,
                            cancellation,
                            mutation_started,
                        ));
                    }
                    RuntimeCommand::Respond {
                        session_id,
                        turn_id,
                        interaction,
                        reply,
                    } => {
                        self.respond(session_id, turn_id, interaction, reply);
                    }
                    RuntimeCommand::RegisterInteraction {
                        session_id,
                        turn_id,
                        interaction,
                        delivery,
                        reply,
                    } => {
                        let _ = reply.send(self.register_interaction(
                            session_id,
                            turn_id,
                            interaction,
                            delivery,
                        ));
                    }
                    RuntimeCommand::Cancel {
                        session_id,
                        turn_id,
                        reply,
                    } => {
                        let _ = reply.send(self.cancel(session_id, turn_id));
                    }
                    RuntimeCommand::Snapshot { session_id, reply } => {
                        let _ = reply.send(self.snapshot(&session_id));
                    }
                    RuntimeCommand::Observe { cursor, reply } => {
                        let _ = reply.send(self.observe(cursor));
                    }
                    RuntimeCommand::Shutdown { reply } => self.begin_shutdown(reply),
                    RuntimeCommand::ExecutionFinished {
                        session_id,
                        turn_id,
                        result,
                    } => self.finish_execution(&session_id, &turn_id, result),
                    RuntimeCommand::ResponseFinished {
                        session_id,
                        turn_id,
                        interaction_id,
                        result,
                        reply,
                    } => {
                        self.finish_response(&session_id, &turn_id, &interaction_id, result, reply)
                    }
                },
            }
            if self.shutdown.is_some() && self.shutdown_is_complete() {
                self.finish_shutdown(Ok(()));
                break;
            }
        }
    }

    fn submit(&mut self, request: TurnRequest) -> Result<TurnReceipt, RuntimeWorkerError> {
        if self.shutdown.is_some() {
            return Err(RuntimeWorkerError::ShuttingDown);
        }
        if request.session_id.is_empty() || request.turn_id.is_empty() {
            return Err(RuntimeWorkerError::InvalidTurnIdentifier);
        }

        let session_id = request.session_id.clone();
        let turn_id = request.turn_id.clone();
        let queue_limit = self.config.max_queued_turns_per_session;
        let event_buffer = self.config.event_buffer.max(1);
        {
            let session = self
                .sessions
                .entry(session_id.clone())
                .or_insert_with(|| SessionQueue::new(session_id.clone(), event_buffer));
            if matches!(
                session.snapshot.interaction,
                InteractionState::IndeterminateSideEffect { .. }
            ) {
                return Err(RuntimeWorkerError::ReconciliationRequired { session_id });
            }
            let active_matches = session
                .active
                .as_ref()
                .is_some_and(|active| active.request.turn_id == turn_id);
            let queued_matches = session
                .queued
                .iter()
                .any(|queued| queued.turn_id == turn_id);
            if active_matches || queued_matches {
                return Err(RuntimeWorkerError::DuplicateTurn {
                    session_id,
                    turn_id,
                });
            }
            if session.queued.len() >= queue_limit {
                return Err(RuntimeWorkerError::QueueFull {
                    session_id,
                    limit: queue_limit,
                });
            }
        }
        self.admit_durable_turn(&request)?;
        {
            let session = self.sessions.get_mut(&session_id).ok_or_else(|| {
                RuntimeWorkerError::UnknownSession {
                    session_id: session_id.clone(),
                }
            })?;
            session.queued.push_back(request);
            session.snapshot.queued_turns = session.queued.len();
            if session.active.is_none() {
                session.set_state(InteractionState::Queued);
            }
        }

        self.emit(
            &session_id,
            Some(turn_id.clone()),
            AgentEventKind::TurnQueued,
        );
        self.start_next_if_idle(&session_id);
        Ok(TurnReceipt {
            session_id,
            turn_id,
        })
    }

    fn track_external_turn(
        &mut self,
        request: TurnRequest,
        cancellation: CancellationToken,
        mutation_started: Arc<AtomicBool>,
    ) -> Result<TurnReceipt, RuntimeWorkerError> {
        if self.shutdown.is_some() {
            return Err(RuntimeWorkerError::ShuttingDown);
        }
        if request.session_id.is_empty() || request.turn_id.is_empty() {
            return Err(RuntimeWorkerError::InvalidTurnIdentifier);
        }
        let session_id = request.session_id.clone();
        let turn_id = request.turn_id.clone();
        let event_buffer = self.config.event_buffer.max(1);
        {
            let session = self
                .sessions
                .entry(session_id.clone())
                .or_insert_with(|| SessionQueue::new(session_id.clone(), event_buffer));
            if matches!(
                session.snapshot.interaction,
                InteractionState::IndeterminateSideEffect { .. }
            ) {
                return Err(RuntimeWorkerError::ReconciliationRequired { session_id });
            }
            if session.active.is_some() || !session.queued.is_empty() {
                return Err(RuntimeWorkerError::QueueFull {
                    session_id,
                    limit: 0,
                });
            }
        }
        self.admit_durable_turn(&request)?;
        let session = self.sessions.get_mut(&request.session_id).ok_or_else(|| {
            RuntimeWorkerError::UnknownSession {
                session_id: request.session_id.clone(),
            }
        })?;
        session.snapshot.active_turn_id = Some(turn_id.clone());
        session.snapshot.queued_turns = 0;
        session.set_state(InteractionState::Running);
        session.active = Some(ActiveTurn {
            request,
            cancellation,
            mutation_started,
            cancel_requested_after_mutation: false,
            interaction: None,
            interaction_delivery: None,
            execution_in_progress: true,
            response_in_progress: false,
            deferred_execution: None,
            response_delivery_failure: None,
        });
        self.emit(
            &session_id,
            Some(turn_id.clone()),
            AgentEventKind::TurnQueued,
        );
        self.emit(
            &session_id,
            Some(turn_id.clone()),
            AgentEventKind::TurnStarted,
        );
        Ok(TurnReceipt {
            session_id,
            turn_id,
        })
    }

    fn respond(
        &mut self,
        session_id: String,
        turn_id: String,
        interaction: InteractionResponse,
        reply: oneshot::Sender<Result<(), RuntimeWorkerError>>,
    ) {
        let response = (|| {
            let (request, cancellation, mutation_started, delivery) = {
                let session = self.sessions.get_mut(&session_id).ok_or_else(|| {
                    RuntimeWorkerError::UnknownSession {
                        session_id: session_id.clone(),
                    }
                })?;
                let active = session
                    .active
                    .as_mut()
                    .filter(|active| active.request.turn_id == turn_id)
                    .ok_or_else(|| RuntimeWorkerError::UnknownTurn {
                        session_id: session_id.clone(),
                        turn_id: turn_id.clone(),
                    })?;
                let is_pending = active.interaction.as_ref().is_some_and(|state| {
                    state.is_awaiting_response()
                        && state.interaction_id() == Some(interaction.interaction_id.as_str())
                });
                if !is_pending {
                    return Err(RuntimeWorkerError::UnknownInteraction {
                        turn_id: turn_id.clone(),
                        interaction_id: interaction.interaction_id.clone(),
                    });
                }
                if let Some(delivery) = active.interaction_delivery.as_ref() {
                    delivery.validate(&interaction)?;
                }
                active.interaction = None;
                active.response_in_progress = true;
                let request = active.request.clone();
                let cancellation = active.cancellation.clone();
                let mutation_started = active.mutation_started.clone();
                let delivery = active.interaction_delivery.take();
                session.set_state(InteractionState::Running);
                (request, cancellation, mutation_started, delivery)
            };
            Ok((request, cancellation, mutation_started, delivery))
        })();

        match response {
            Ok((request, cancellation, mutation_started, delivery)) => {
                self.spawn_response(ResponseExecution {
                    session_id,
                    turn_id,
                    request,
                    interaction,
                    cancellation,
                    mutation_started,
                    delivery,
                    reply,
                });
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    fn register_interaction(
        &mut self,
        session_id: String,
        turn_id: String,
        interaction: InteractionState,
        delivery: Option<Box<dyn RuntimeInteractionDelivery>>,
    ) -> Result<(), RuntimeWorkerError> {
        if !interaction.is_awaiting_response() {
            return Err(RuntimeWorkerError::InvalidInteractionState { turn_id });
        }
        {
            let session = self.sessions.get_mut(&session_id).ok_or_else(|| {
                RuntimeWorkerError::UnknownSession {
                    session_id: session_id.clone(),
                }
            })?;
            let active = session
                .active
                .as_mut()
                .filter(|active| active.request.turn_id == turn_id)
                .ok_or_else(|| RuntimeWorkerError::UnknownTurn {
                    session_id: session_id.clone(),
                    turn_id: turn_id.clone(),
                })?;
            if !active.execution_in_progress {
                return Err(RuntimeWorkerError::InteractionRegistrationClosed { turn_id });
            }
            if active.interaction.is_some() {
                return Err(RuntimeWorkerError::InteractionAlreadyPending { turn_id });
            }
            active.interaction = Some(interaction.clone());
            active.interaction_delivery = delivery;
            session.set_state(interaction.clone());
        }
        self.emit(
            &session_id,
            Some(turn_id),
            AgentEventKind::InteractionRequested { state: interaction },
        );
        Ok(())
    }

    fn cancel(&mut self, session_id: String, turn_id: String) -> Result<(), RuntimeWorkerError> {
        enum CancelOutcome {
            Queued(TurnRequest),
            WaitingActive(TurnRequest),
            RunningActive,
            Missing,
        }

        let outcome = {
            let session = self.sessions.get_mut(&session_id).ok_or_else(|| {
                RuntimeWorkerError::UnknownSession {
                    session_id: session_id.clone(),
                }
            })?;
            if let Some(position) = session
                .queued
                .iter()
                .position(|queued| queued.turn_id == turn_id)
            {
                if let Some(request) = session.queued.remove(position) {
                    session.snapshot.queued_turns = session.queued.len();
                    CancelOutcome::Queued(request)
                } else {
                    CancelOutcome::Missing
                }
            } else if session
                .active
                .as_ref()
                .is_some_and(|active| active.request.turn_id == turn_id)
            {
                let (mutation_started, waiting_for_interaction, execution_in_progress, request) =
                    {
                        let active = session.active.as_mut().ok_or_else(|| {
                            RuntimeWorkerError::UnknownTurn {
                                session_id: session_id.clone(),
                                turn_id: turn_id.clone(),
                            }
                        })?;
                        let mutation_started = active.request.mutating
                            && active.mutation_started.load(Ordering::Acquire);
                        let waiting_for_interaction = active.interaction.is_some();
                        if waiting_for_interaction {
                            // A parked tool handler may await only this sender, not
                            // the runtime cancellation token. Drop the worker-owned
                            // continuation before waiting for the executor to exit;
                            // otherwise cancel would deadlock as each side waited
                            // for the other to release the oneshot.
                            active.interaction = None;
                            active.interaction_delivery = None;
                        }
                        if mutation_started {
                            active.cancel_requested_after_mutation = true;
                        } else {
                            active.cancellation.cancel();
                        }
                        (
                            mutation_started,
                            waiting_for_interaction,
                            active.execution_in_progress,
                            active.request.clone(),
                        )
                    };
                if mutation_started {
                    // The executor continues until it reports a determinate
                    // result. Cancelling its token here could interrupt a
                    // write after the side effect had started.
                    session.set_state(InteractionState::Cancelling);
                    CancelOutcome::RunningActive
                } else if waiting_for_interaction && !execution_in_progress {
                    // A waiting interaction has no executor future left to
                    // observe the token and report `ExecutionFinished`.
                    // Finish it here so the following turn cannot deadlock.
                    session.active = None;
                    session.snapshot.active_turn_id = None;
                    session.set_state(InteractionState::Cancelled);
                    CancelOutcome::WaitingActive(request)
                } else {
                    session.set_state(InteractionState::Cancelling);
                    CancelOutcome::RunningActive
                }
            } else {
                CancelOutcome::Missing
            }
        };

        match outcome {
            CancelOutcome::Queued(request) => {
                if let Err(error) = Self::persist_cancelled_turn(
                    self.config.durability.as_ref(),
                    self.config.durability_repo_id.as_deref(),
                    self.config.durability_principal_id.as_deref(),
                    self.config.durability_command_kind.as_deref(),
                    &request,
                ) {
                    self.fence_for_durability_failure(&session_id, &turn_id, &error);
                    return Err(error);
                }
                self.emit(&session_id, Some(turn_id), AgentEventKind::TurnCancelled);
                Ok(())
            }
            CancelOutcome::WaitingActive(request) => {
                if let Err(error) = Self::persist_cancelled_turn(
                    self.config.durability.as_ref(),
                    self.config.durability_repo_id.as_deref(),
                    self.config.durability_principal_id.as_deref(),
                    self.config.durability_command_kind.as_deref(),
                    &request,
                ) {
                    self.fence_for_durability_failure(&session_id, &turn_id, &error);
                    return Err(error);
                }
                self.emit(&session_id, Some(turn_id), AgentEventKind::TurnCancelled);
                self.start_next_if_idle(&session_id);
                Ok(())
            }
            CancelOutcome::RunningActive => {
                self.emit(&session_id, Some(turn_id), AgentEventKind::CancelRequested);
                Ok(())
            }
            CancelOutcome::Missing => Err(RuntimeWorkerError::UnknownTurn {
                session_id,
                turn_id,
            }),
        }
    }

    fn begin_shutdown(&mut self, reply: Option<oneshot::Sender<Result<(), RuntimeShutdownError>>>) {
        if let Some(shutdown) = self.shutdown.as_mut() {
            if let Some(reply) = reply {
                shutdown.waiters.push(reply);
            }
            return;
        }

        self.shutdown = Some(ShutdownState {
            deadline: Instant::now() + self.config.shutdown_timeout,
            waiters: reply.into_iter().collect(),
        });

        let mut cancelled_turns = Vec::new();
        let mut cancellation_requested_turns = Vec::new();
        for (session_id, session) in &mut self.sessions {
            for queued in session.queued.drain(..) {
                cancelled_turns.push((session_id.clone(), queued));
            }
            session.snapshot.queued_turns = 0;

            let Some(active) = session.active.as_mut() else {
                continue;
            };
            let turn_id = active.request.turn_id.clone();
            let request = active.request.clone();
            let mutation_started =
                active.request.mutating && active.mutation_started.load(Ordering::Acquire);
            let waiting_for_interaction = active.interaction.is_some();
            if waiting_for_interaction {
                // Match ordinary cancellation: a parked tool loop can be
                // waiting exclusively on the response sender, so shutdown
                // must drop it before awaiting the executor's termination.
                active.interaction = None;
                active.interaction_delivery = None;
            }
            if mutation_started {
                // Do not cancel a process after a mutation has begun. It must
                // report a determinate terminal result or surface the timeout
                // as an explicit unreleased reconciliation resource.
                active.cancel_requested_after_mutation = true;
                session.set_state(InteractionState::Cancelling);
                cancellation_requested_turns.push((session_id.clone(), turn_id));
            } else if waiting_for_interaction && !active.execution_in_progress {
                // An interaction waits for an adapter response, not an executor
                // future. Release it synchronously so shutdown cannot deadlock.
                session.active = None;
                session.snapshot.active_turn_id = None;
                session.set_state(InteractionState::Cancelled);
                cancelled_turns.push((session_id.clone(), request));
            } else {
                active.cancellation.cancel();
                session.set_state(InteractionState::Cancelling);
                cancellation_requested_turns.push((session_id.clone(), turn_id));
            }
        }

        for (session_id, request) in cancelled_turns {
            if let Err(error) = Self::persist_cancelled_turn(
                self.config.durability.as_ref(),
                self.config.durability_repo_id.as_deref(),
                self.config.durability_principal_id.as_deref(),
                self.config.durability_command_kind.as_deref(),
                &request,
            ) {
                self.fence_for_durability_failure(&session_id, &request.turn_id, &error);
                continue;
            }
            self.emit(
                &session_id,
                Some(request.turn_id),
                AgentEventKind::TurnCancelled,
            );
        }
        for (session_id, turn_id) in cancellation_requested_turns {
            self.emit(&session_id, Some(turn_id), AgentEventKind::CancelRequested);
        }
    }

    fn shutdown_is_complete(&self) -> bool {
        self.sessions
            .values()
            .all(|session| session.active.is_none())
    }

    fn finish_shutdown(&mut self, result: Result<(), RuntimeShutdownError>) {
        if result.is_err() {
            // The worker owns terminal durability. A shutdown deadline leaves
            // only active mutations that have crossed their mutation boundary
            // indeterminate. Queued turns and non-mutating active work can be
            // conclusively cancelled because they cannot have changed state.
            let active_turns: Vec<_> = self
                .sessions
                .iter()
                .filter_map(|(session_id, session)| {
                    session
                        .active
                        .as_ref()
                        .map(|active| (session_id.clone(), active.request.clone()))
                })
                .collect();
            for (session_id, request) in active_turns {
                let mutation_started = self
                    .sessions
                    .get(&session_id)
                    .and_then(|session| session.active.as_ref())
                    .is_some_and(|active| {
                        active.request.mutating
                            && (active.mutation_started.load(Ordering::Acquire)
                                || active.cancel_requested_after_mutation)
                    });
                if !mutation_started {
                    if let Err(error) = Self::persist_cancelled_turn(
                        self.config.durability.as_ref(),
                        self.config.durability_repo_id.as_deref(),
                        self.config.durability_principal_id.as_deref(),
                        self.config.durability_command_kind.as_deref(),
                        &request,
                    ) {
                        self.fence_for_durability_failure(&session_id, &request.turn_id, &error);
                        continue;
                    }
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.active = None;
                        session.snapshot.active_turn_id = None;
                        session.set_state(InteractionState::Cancelled);
                    }
                    self.emit(
                        &session_id,
                        Some(request.turn_id),
                        AgentEventKind::TurnCancelled,
                    );
                    continue;
                }
                let reason = "runtime shutdown timed out before the active turn reached a determinate result";
                match Self::persist_indeterminate_turn(
                    self.config.durability.as_ref(),
                    self.config.durability_repo_id.as_deref(),
                    self.config.durability_principal_id.as_deref(),
                    self.config.durability_command_kind.as_deref(),
                    &request,
                    reason,
                ) {
                    Ok(()) => {
                        if let Some(session) = self.sessions.get_mut(&session_id) {
                            session.active = None;
                            session.snapshot.active_turn_id = None;
                            session.set_state(InteractionState::IndeterminateSideEffect {
                                reason: reason.to_string(),
                            });
                        }
                        self.emit(
                            &session_id,
                            Some(request.turn_id),
                            AgentEventKind::TurnIndeterminateSideEffect {
                                reason: reason.to_string(),
                            },
                        );
                    }
                    Err(error) => {
                        self.fence_for_durability_failure(&session_id, &request.turn_id, &error);
                    }
                }
            }
        }
        let Some(shutdown) = self.shutdown.take() else {
            return;
        };
        let result = match self.shutdown_result.lock() {
            Ok(mut recorded) => {
                *recorded = Some(result.clone());
                result
            }
            Err(_) => Err(RuntimeShutdownError::LifecycleStateUnavailable),
        };
        for waiter in shutdown.waiters {
            let _ = waiter.send(result.clone());
        }
    }

    fn unreleased_resource_categories(&self) -> Vec<String> {
        let mut categories = Vec::new();
        for session in self.sessions.values() {
            let Some(active) = session.active.as_ref() else {
                continue;
            };
            let category =
                if active.request.mutating && active.mutation_started.load(Ordering::Acquire) {
                    "mutating_runtime_turn_reconciliation"
                } else {
                    "runtime_turn"
                };
            if !categories.iter().any(|existing| existing == category) {
                categories.push(category.to_string());
            }
        }
        categories
    }

    fn snapshot(&self, session_id: &str) -> Result<AgentSnapshot, RuntimeWorkerError> {
        self.sessions
            .get(session_id)
            .map(|session| session.snapshot.clone())
            .ok_or_else(|| RuntimeWorkerError::UnknownSession {
                session_id: session_id.to_string(),
            })
    }

    fn observe(&mut self, cursor: EventCursor) -> AgentEventStream {
        let event_buffer = self.config.event_buffer.max(1);
        let session = self
            .sessions
            .entry(cursor.session_id.clone())
            .or_insert_with(|| SessionQueue::new(cursor.session_id.clone(), event_buffer));
        AgentEventStream {
            after_cursor: cursor,
            receiver: session.event_tx.subscribe(),
        }
    }

    fn start_next_if_idle(&mut self, session_id: &str) {
        if self.shutdown.is_some() {
            return;
        }
        let next = {
            let Some(session) = self.sessions.get_mut(session_id) else {
                return;
            };
            if session.active.is_some() {
                return;
            }
            if matches!(
                session.snapshot.interaction,
                InteractionState::IndeterminateSideEffect { .. }
            ) {
                return;
            }
            let Some(request) = session.queued.pop_front() else {
                session.snapshot.queued_turns = 0;
                if !matches!(
                    session.snapshot.interaction,
                    InteractionState::Completed
                        | InteractionState::Failed { .. }
                        | InteractionState::Cancelled
                        | InteractionState::IndeterminateSideEffect { .. }
                ) {
                    session.set_state(InteractionState::Idle);
                }
                return;
            };
            let cancellation = CancellationToken::new();
            let mutation_started = Arc::new(AtomicBool::new(false));
            session.snapshot.active_turn_id = Some(request.turn_id.clone());
            session.snapshot.queued_turns = session.queued.len();
            session.set_state(InteractionState::Running);
            session.active = Some(ActiveTurn {
                request: request.clone(),
                cancellation: cancellation.clone(),
                mutation_started: mutation_started.clone(),
                cancel_requested_after_mutation: false,
                interaction: None,
                interaction_delivery: None,
                execution_in_progress: true,
                response_in_progress: false,
                deferred_execution: None,
                response_delivery_failure: None,
            });
            Some((request, cancellation, mutation_started))
        };

        if let Some((request, cancellation, mutation_started)) = next {
            self.emit(
                session_id,
                Some(request.turn_id.clone()),
                AgentEventKind::TurnStarted,
            );
            self.spawn_execution(request, cancellation, mutation_started);
        }
    }

    fn spawn_execution(
        &self,
        request: TurnRequest,
        cancellation: CancellationToken,
        mutation_started: Arc<AtomicBool>,
    ) {
        let executor = self.config.executor.clone();
        let tool_boundary = self.config.tool_boundary.clone();
        let command_tx = self.command_tx.clone();
        tokio::spawn(async move {
            let session_id = request.session_id.clone();
            let turn_id = request.turn_id.clone();
            let context = RuntimeExecutionContext {
                tool_boundary,
                cancellation,
                mutation_started,
            };
            let result = executor.execute(request, context).await;
            let _ = command_tx
                .send(RuntimeCommand::ExecutionFinished {
                    session_id,
                    turn_id,
                    result,
                })
                .await;
        });
    }

    fn spawn_response(&self, response: ResponseExecution) {
        let executor = self.config.executor.clone();
        let tool_boundary = self.config.tool_boundary.clone();
        let command_tx = self.command_tx.clone();
        let interaction_id = response.interaction.interaction_id.clone();
        tokio::spawn(async move {
            let context = RuntimeExecutionContext {
                tool_boundary,
                cancellation: response.cancellation,
                mutation_started: response.mutation_started,
            };
            let result = if let Some(delivery) = response.delivery {
                delivery
                    .deliver(response.request, response.interaction, context)
                    .await
            } else {
                executor
                    .respond(response.request, response.interaction, context)
                    .await
            };
            let _ = command_tx
                .send(RuntimeCommand::ResponseFinished {
                    session_id: response.session_id,
                    turn_id: response.turn_id,
                    interaction_id,
                    result,
                    reply: response.reply,
                })
                .await;
        });
    }

    /// Complete the caller-facing response acknowledgement only after the
    /// executor has either accepted the response or reported its failure. The
    /// prior implementation returned success as soon as the actor queued an
    /// executor task, which could tell a browser that an approval/input had
    /// been delivered even if the executor subsequently rejected it.
    fn finish_response(
        &mut self,
        session_id: &str,
        turn_id: &str,
        interaction_id: &str,
        result: Result<RuntimeTurnExecution, RuntimeWorkerError>,
        reply: oneshot::Sender<Result<(), RuntimeWorkerError>>,
    ) {
        let reply_result = match &result {
            Ok(_) => Ok(()),
            Err(error) => Err(error.clone()),
        };
        let response_failure = result.as_ref().err().map(ToString::to_string);
        let response_delivery_state = self
            .sessions
            .get_mut(session_id)
            .and_then(|session| {
                session
                    .active
                    .as_mut()
                    .filter(|active| active.request.turn_id == turn_id)
            })
            .map(|active| {
                active.response_in_progress = false;
                if let Some(reason) = response_failure.as_ref() {
                    // The original executor can still be running while its
                    // interaction response is being persisted or forwarded.
                    // Keep its serialized slot until it reports a terminal
                    // result; otherwise a follow-up turn could overlap an
                    // orphaned tool loop.
                    active.response_delivery_failure = Some(reason.clone());
                    active.interaction = None;
                    if active.request.mutating && active.mutation_started.load(Ordering::Acquire) {
                        active.cancel_requested_after_mutation = true;
                    } else {
                        active.cancellation.cancel();
                    }
                }
                (
                    active.deferred_execution.take(),
                    response_failure.is_some() && !active.execution_in_progress,
                )
            });
        let (deferred_execution, response_failure_without_live_execution) =
            response_delivery_state.unwrap_or((None, false));
        if response_failure.is_some()
            && let Some(session) = self.sessions.get_mut(session_id)
            && session
                .active
                .as_ref()
                .is_some_and(|active| active.request.turn_id == turn_id)
        {
            session.set_state(InteractionState::Cancelling);
        }
        if reply_result.is_ok() {
            self.emit(
                session_id,
                Some(turn_id.to_string()),
                AgentEventKind::InteractionResponded {
                    interaction_id: interaction_id.to_string(),
                },
            );
        }
        if reply_result.is_ok() || response_failure_without_live_execution {
            self.finish_execution(session_id, turn_id, result);
        }
        if let Some(deferred_execution) = deferred_execution {
            self.finish_execution(session_id, turn_id, deferred_execution);
        }
        let _ = reply.send(reply_result);
    }

    fn finish_execution(
        &mut self,
        session_id: &str,
        turn_id: &str,
        result: Result<RuntimeTurnExecution, RuntimeWorkerError>,
    ) {
        let durability = self.config.durability.clone();
        let durability_repo_id = self.config.durability_repo_id.clone();
        let durability_principal_id = self.config.durability_principal_id.clone();
        let durability_command_kind = self.config.durability_command_kind.clone();
        let Some(session) = self.sessions.get_mut(session_id) else {
            return;
        };
        let Some(active) = session.active.as_ref() else {
            return;
        };
        if active.request.turn_id != turn_id {
            return;
        }
        if active.response_in_progress {
            if let Some(active) = session.active.as_mut() {
                active.deferred_execution = Some(result);
            }
            return;
        }
        let cancel_requested_after_mutation = active.cancel_requested_after_mutation;
        let response_delivery_failure = active.response_delivery_failure.clone();
        let request = active.request.clone();

        if let Some(reason) = response_delivery_failure {
            session.active = None;
            session.snapshot.active_turn_id = None;
            if cancel_requested_after_mutation {
                let reason = format!(
                    "mutating turn could not reconcile a failed interaction response; reconciliation is required: {reason}"
                );
                if let Err(error) = Self::persist_indeterminate_turn(
                    durability.as_ref(),
                    durability_repo_id.as_deref(),
                    durability_principal_id.as_deref(),
                    durability_command_kind.as_deref(),
                    &request,
                    &reason,
                ) {
                    self.fence_for_durability_failure(session_id, turn_id, &error);
                    return;
                }
                session.set_state(InteractionState::IndeterminateSideEffect {
                    reason: reason.clone(),
                });
                self.emit(
                    session_id,
                    Some(turn_id.to_string()),
                    AgentEventKind::TurnIndeterminateSideEffect { reason },
                );
            } else {
                if let Err(error) = Self::persist_failed_turn(
                    durability.as_ref(),
                    durability_repo_id.as_deref(),
                    durability_principal_id.as_deref(),
                    durability_command_kind.as_deref(),
                    &request,
                    &reason,
                ) {
                    self.fence_for_durability_failure(session_id, turn_id, &error);
                    return;
                }
                session.set_state(InteractionState::Failed {
                    reason: reason.clone(),
                });
                self.emit(
                    session_id,
                    Some(turn_id.to_string()),
                    AgentEventKind::TurnFailed { reason },
                );
                self.start_next_if_idle(session_id);
            }
            return;
        }

        match result {
            Ok(RuntimeTurnExecution::InteractionResponseDelivered) => {
                // The original executor future remains responsible for the
                // eventual terminal result. `respond` already advanced the
                // observable interaction state back to Running.
            }
            Ok(RuntimeTurnExecution::AwaitingInteraction(state))
                if state.is_awaiting_response() =>
            {
                if let Some(active) = session.active.as_mut() {
                    active.execution_in_progress = false;
                    active.interaction = Some(state.clone());
                }
                session.set_state(state.clone());
                self.emit(
                    session_id,
                    Some(turn_id.to_string()),
                    AgentEventKind::InteractionRequested { state },
                );
            }
            Ok(RuntimeTurnExecution::AwaitingInteraction(_)) => {
                let reason = "executor returned a non-interactive waiting state".to_string();
                session.active = None;
                session.snapshot.active_turn_id = None;
                if let Err(error) = Self::persist_failed_turn(
                    durability.as_ref(),
                    durability_repo_id.as_deref(),
                    durability_principal_id.as_deref(),
                    durability_command_kind.as_deref(),
                    &request,
                    &reason,
                ) {
                    self.fence_for_durability_failure(session_id, turn_id, &error);
                    return;
                }
                session.set_state(InteractionState::Failed {
                    reason: reason.clone(),
                });
                self.emit(
                    session_id,
                    Some(turn_id.to_string()),
                    AgentEventKind::TurnFailed { reason },
                );
                self.start_next_if_idle(session_id);
            }
            Ok(RuntimeTurnExecution::Completed { summary }) => {
                session.active = None;
                session.snapshot.active_turn_id = None;
                if let Err(error) = Self::persist_successful_turn(
                    durability.as_ref(),
                    durability_repo_id.as_deref(),
                    durability_principal_id.as_deref(),
                    durability_command_kind.as_deref(),
                    &request,
                    &summary,
                ) {
                    self.fence_for_durability_failure(session_id, turn_id, &error);
                    return;
                }
                session.set_state(InteractionState::Completed);
                self.emit(
                    session_id,
                    Some(turn_id.to_string()),
                    AgentEventKind::TurnCompleted { summary },
                );
                self.start_next_if_idle(session_id);
            }
            Err(RuntimeWorkerError::Cancelled) if cancel_requested_after_mutation => {
                let reason = "mutating dispatch did not report a determinate result after cancellation; reconciliation is required".to_string();
                session.active = None;
                session.snapshot.active_turn_id = None;
                if let Err(error) = Self::persist_indeterminate_turn(
                    durability.as_ref(),
                    durability_repo_id.as_deref(),
                    durability_principal_id.as_deref(),
                    durability_command_kind.as_deref(),
                    &request,
                    &reason,
                ) {
                    self.fence_for_durability_failure(session_id, turn_id, &error);
                    return;
                }
                session.set_state(InteractionState::IndeterminateSideEffect {
                    reason: reason.clone(),
                });
                self.emit(
                    session_id,
                    Some(turn_id.to_string()),
                    AgentEventKind::TurnIndeterminateSideEffect { reason },
                );
            }
            Err(RuntimeWorkerError::IndeterminateSideEffect(reason)) => {
                session.active = None;
                session.snapshot.active_turn_id = None;
                if let Err(error) = Self::persist_indeterminate_turn(
                    durability.as_ref(),
                    durability_repo_id.as_deref(),
                    durability_principal_id.as_deref(),
                    durability_command_kind.as_deref(),
                    &request,
                    &reason,
                ) {
                    self.fence_for_durability_failure(session_id, turn_id, &error);
                    return;
                }
                session.set_state(InteractionState::IndeterminateSideEffect {
                    reason: reason.clone(),
                });
                self.emit(
                    session_id,
                    Some(turn_id.to_string()),
                    AgentEventKind::TurnIndeterminateSideEffect { reason },
                );
            }
            Err(RuntimeWorkerError::Cancelled) => {
                session.active = None;
                session.snapshot.active_turn_id = None;
                if let Err(error) = Self::persist_cancelled_turn(
                    durability.as_ref(),
                    durability_repo_id.as_deref(),
                    durability_principal_id.as_deref(),
                    durability_command_kind.as_deref(),
                    &request,
                ) {
                    self.fence_for_durability_failure(session_id, turn_id, &error);
                    return;
                }
                session.set_state(InteractionState::Cancelled);
                self.emit(
                    session_id,
                    Some(turn_id.to_string()),
                    AgentEventKind::TurnCancelled,
                );
                self.start_next_if_idle(session_id);
            }
            Err(error) if cancel_requested_after_mutation => {
                let reason = format!(
                    "mutating dispatch returned an error after cancellation and may require reconciliation: {error}"
                );
                session.active = None;
                session.snapshot.active_turn_id = None;
                if let Err(error) = Self::persist_indeterminate_turn(
                    durability.as_ref(),
                    durability_repo_id.as_deref(),
                    durability_principal_id.as_deref(),
                    durability_command_kind.as_deref(),
                    &request,
                    &reason,
                ) {
                    self.fence_for_durability_failure(session_id, turn_id, &error);
                    return;
                }
                session.set_state(InteractionState::IndeterminateSideEffect {
                    reason: reason.clone(),
                });
                self.emit(
                    session_id,
                    Some(turn_id.to_string()),
                    AgentEventKind::TurnIndeterminateSideEffect { reason },
                );
            }
            Err(error) => {
                let reason = error.to_string();
                session.active = None;
                session.snapshot.active_turn_id = None;
                if let Err(error) = Self::persist_failed_turn(
                    durability.as_ref(),
                    durability_repo_id.as_deref(),
                    durability_principal_id.as_deref(),
                    durability_command_kind.as_deref(),
                    &request,
                    &reason,
                ) {
                    self.fence_for_durability_failure(session_id, turn_id, &error);
                    return;
                }
                session.set_state(InteractionState::Failed {
                    reason: reason.clone(),
                });
                self.emit(
                    session_id,
                    Some(turn_id.to_string()),
                    AgentEventKind::TurnFailed { reason },
                );
                self.start_next_if_idle(session_id);
            }
        }
    }

    fn durable_intent(
        durability: Option<&RuntimeCommandDurability>,
        repo_id: Option<&str>,
        principal_id: Option<&str>,
        command_kind: Option<&str>,
        request: &TurnRequest,
    ) -> Result<Option<CodeCommandIntent>, RuntimeWorkerError> {
        let Some(_) = durability else {
            return Ok(None);
        };
        let repo_id = repo_id.ok_or_else(|| {
            RuntimeWorkerError::DurabilityFailure(
                "durability is configured without a repository identity".to_string(),
            )
        })?;
        let principal_id = principal_id.ok_or_else(|| {
            RuntimeWorkerError::DurabilityFailure(
                "durability is configured without a principal identity".to_string(),
            )
        })?;
        let request_hash = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(request.input.as_bytes()))
        );
        Ok(Some(CodeCommandIntent::new(
            CodeCommandIdentity::new(
                repo_id,
                request.session_id.clone(),
                principal_id,
                request.turn_id.clone(),
            ),
            command_kind.unwrap_or("agent_runtime_turn"),
            request_hash,
            request.mutating,
        )))
    }

    fn admit_durable_turn(&self, request: &TurnRequest) -> Result<(), RuntimeWorkerError> {
        let Some(intent) = Self::durable_intent(
            self.config.durability.as_ref(),
            self.config.durability_repo_id.as_deref(),
            self.config.durability_principal_id.as_deref(),
            self.config.durability_command_kind.as_deref(),
            request,
        )?
        else {
            return Ok(());
        };
        let durability = self.config.durability.as_ref().ok_or_else(|| {
            RuntimeWorkerError::DurabilityFailure(
                "durability disappeared while admitting a runtime turn".to_string(),
            )
        })?;
        match durability.admit(intent).map_err(|error| {
            RuntimeWorkerError::DurabilityFailure(format!(
                "could not persist intent for turn '{}': {error}",
                request.turn_id
            ))
        })? {
            CodeCommandAdmission::Execute { .. } => Ok(()),
            CodeCommandAdmission::Existing { status } => {
                Err(RuntimeWorkerError::DurabilityFailure(format!(
                    "turn '{}' already has durable command state {status:?}; refusing to dispatch it again",
                    request.turn_id
                )))
            }
        }
    }

    fn persist_cancelled_turn(
        durability: Option<&RuntimeCommandDurability>,
        repo_id: Option<&str>,
        principal_id: Option<&str>,
        command_kind: Option<&str>,
        request: &TurnRequest,
    ) -> Result<(), RuntimeWorkerError> {
        let Some(intent) =
            Self::durable_intent(durability, repo_id, principal_id, command_kind, request)?
        else {
            return Ok(());
        };
        let durability = durability.ok_or_else(|| {
            RuntimeWorkerError::DurabilityFailure(
                "durability disappeared while recording a cancellation".to_string(),
            )
        })?;
        durability
            .complete_failure(
                &intent,
                "runtime turn cancelled before a mutating side effect began",
            )
            .map_err(|error| {
                RuntimeWorkerError::DurabilityFailure(format!(
                    "could not durably record cancellation for turn '{}': {error}",
                    request.turn_id
                ))
            })?;
        Ok(())
    }

    fn persist_successful_turn(
        durability: Option<&RuntimeCommandDurability>,
        repo_id: Option<&str>,
        principal_id: Option<&str>,
        command_kind: Option<&str>,
        request: &TurnRequest,
        summary: &str,
    ) -> Result<(), RuntimeWorkerError> {
        let Some(intent) =
            Self::durable_intent(durability, repo_id, principal_id, command_kind, request)?
        else {
            return Ok(());
        };
        let durability = durability.ok_or_else(|| {
            RuntimeWorkerError::DurabilityFailure(
                "durability disappeared while recording successful completion".to_string(),
            )
        })?;
        durability
            .complete_success(&intent, summary)
            .map_err(|error| {
                RuntimeWorkerError::DurabilityFailure(format!(
                    "could not durably record successful completion for turn '{}': {error}",
                    request.turn_id
                ))
            })?;
        Ok(())
    }

    fn persist_failed_turn(
        durability: Option<&RuntimeCommandDurability>,
        repo_id: Option<&str>,
        principal_id: Option<&str>,
        command_kind: Option<&str>,
        request: &TurnRequest,
        reason: &str,
    ) -> Result<(), RuntimeWorkerError> {
        let Some(intent) =
            Self::durable_intent(durability, repo_id, principal_id, command_kind, request)?
        else {
            return Ok(());
        };
        let durability = durability.ok_or_else(|| {
            RuntimeWorkerError::DurabilityFailure(
                "durability disappeared while recording failed completion".to_string(),
            )
        })?;
        durability
            .complete_failure(&intent, reason)
            .map_err(|error| {
                RuntimeWorkerError::DurabilityFailure(format!(
                    "could not durably record failed completion for turn '{}': {error}",
                    request.turn_id
                ))
            })?;
        Ok(())
    }

    fn persist_indeterminate_turn(
        durability: Option<&RuntimeCommandDurability>,
        repo_id: Option<&str>,
        principal_id: Option<&str>,
        command_kind: Option<&str>,
        request: &TurnRequest,
        reason: &str,
    ) -> Result<(), RuntimeWorkerError> {
        let Some(intent) =
            Self::durable_intent(durability, repo_id, principal_id, command_kind, request)?
        else {
            return Ok(());
        };
        let durability = durability.ok_or_else(|| {
            RuntimeWorkerError::DurabilityFailure(
                "durability disappeared while recording an indeterminate mutation".to_string(),
            )
        })?;
        let effect = if reason.contains("runtime shutdown timed out") {
            "runtime_shutdown_timeout"
        } else {
            "mutating_runtime_turn"
        };
        durability
            .mark_indeterminate(&intent, effect, reason)
            .map_err(|error| {
                RuntimeWorkerError::DurabilityFailure(format!(
                    "could not durably record reconciliation requirement for turn '{}': {error}",
                    request.turn_id
                ))
            })?;
        Ok(())
    }

    fn fence_for_durability_failure(
        &mut self,
        session_id: &str,
        turn_id: &str,
        error: &RuntimeWorkerError,
    ) {
        let reason = format!("reconciliation is required because {error}");
        if let Some(session) = self.sessions.get_mut(session_id) {
            // Only drop the active slot when it belongs to the failed turn.
            // Queued-cancel durability failures must not orphan a still-running
            // sibling turn that remains active on the same session.
            let clears_active = session
                .active
                .as_ref()
                .is_some_and(|active| active.request.turn_id == turn_id)
                || session.snapshot.active_turn_id.as_deref() == Some(turn_id);
            if clears_active {
                session.active = None;
                session.snapshot.active_turn_id = None;
            }
            session.set_state(InteractionState::IndeterminateSideEffect {
                reason: reason.clone(),
            });
        }
        self.emit(
            session_id,
            Some(turn_id.to_string()),
            AgentEventKind::TurnIndeterminateSideEffect { reason },
        );
    }

    fn emit(&mut self, session_id: &str, turn_id: Option<String>, kind: AgentEventKind) {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return;
        };
        session.snapshot.cursor.sequence = session.snapshot.cursor.sequence.saturating_add(1);
        let _ = session.event_tx.send(AgentEvent {
            cursor: session.snapshot.cursor.clone(),
            session_id: session_id.to_string(),
            turn_id,
            kind,
        });
    }
}

struct ShutdownState {
    deadline: Instant,
    waiters: Vec<oneshot::Sender<Result<(), RuntimeShutdownError>>>,
}

async fn wait_for_shutdown_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => future::pending::<()>().await,
    }
}

struct SessionQueue {
    snapshot: AgentSnapshot,
    state_machine: TurnStateMachine,
    event_tx: broadcast::Sender<AgentEvent>,
    queued: VecDeque<TurnRequest>,
    active: Option<ActiveTurn>,
}

impl SessionQueue {
    fn new(session_id: String, event_buffer: usize) -> Self {
        let cursor = EventCursor::new(session_id.clone(), 0);
        Self {
            snapshot: AgentSnapshot {
                session_id,
                cursor,
                active_turn_id: None,
                queued_turns: 0,
                interaction: InteractionState::Idle,
            },
            state_machine: TurnStateMachine::new(),
            event_tx: broadcast::channel(event_buffer).0,
            queued: VecDeque::new(),
            active: None,
        }
    }

    fn set_state(&mut self, state: InteractionState) {
        self.state_machine.transition(state.clone());
        self.snapshot.interaction = state;
    }
}

struct ActiveTurn {
    request: TurnRequest,
    cancellation: CancellationToken,
    mutation_started: Arc<AtomicBool>,
    cancel_requested_after_mutation: bool,
    interaction: Option<InteractionState>,
    /// A runtime-owned continuation for a live interaction.  The legacy
    /// executor-response path leaves this `None`; adapters migrating to the
    /// shared owner install a delivery object here instead of retaining a
    /// private pending map.
    interaction_delivery: Option<Box<dyn RuntimeInteractionDelivery>>,
    /// A tool-loop executor can keep running while its handler awaits a UI
    /// response. Cancellation must signal that live future rather than free
    /// the serialized slot as if the executor had already returned.
    execution_in_progress: bool,
    /// While a response is being delivered, terminal completion from the
    /// original executor is deferred so observers see a linearized response
    /// acknowledgement before the turn terminal event.
    response_in_progress: bool,
    deferred_execution: Option<Result<RuntimeTurnExecution, RuntimeWorkerError>>,
    /// A response-delivery task failed while the original executor remained
    /// live. Its final completion must not be reported as a successful turn.
    response_delivery_failure: Option<String>,
}

struct ResponseExecution {
    session_id: String,
    turn_id: String,
    request: TurnRequest,
    interaction: InteractionResponse,
    cancellation: CancellationToken,
    mutation_started: Arc<AtomicBool>,
    delivery: Option<Box<dyn RuntimeInteractionDelivery>>,
    reply: oneshot::Sender<Result<(), RuntimeWorkerError>>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::{
        sync::{Mutex, Notify, oneshot},
        time::{Duration, timeout},
    };
    use uuid::Uuid;

    use super::*;
    use crate::internal::ai::runtime::{
        InMemoryAuditSink, PrincipalContext, PrincipalRole, SecretRedactor, ToolBoundaryPolicy,
    };

    fn config(executor: Arc<dyn RuntimeTurnExecutor>) -> AgentRuntimeWorkerConfig {
        let audit_sink = Arc::new(InMemoryAuditSink::default());
        let boundary = ToolBoundaryRuntime::new(
            Uuid::new_v4(),
            PrincipalContext {
                principal_id: "runtime-test".to_string(),
                role: PrincipalRole::Contributor,
            },
            ToolBoundaryPolicy::default_runtime(),
            SecretRedactor::default_runtime(),
            audit_sink,
        );
        AgentRuntimeWorkerConfig::new(executor, boundary)
    }

    struct BlockingExecutor {
        starts: Arc<Mutex<Vec<String>>>,
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for BlockingExecutor {
        async fn execute(
            &self,
            request: TurnRequest,
            context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.starts.lock().await.push(request.turn_id);
            self.started.notify_one();
            let cancellation = context.cancellation();
            tokio::select! {
                _ = self.release.notified() => Ok(RuntimeTurnExecution::Completed { summary: "done".to_string() }),
                _ = cancellation.cancelled() => Err(RuntimeWorkerError::Cancelled),
            }
        }
    }

    #[tokio::test]
    async fn agent_runtime_state_machine_serializes_turns_per_session() {
        let starts = Arc::new(Mutex::new(Vec::new()));
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let executor = Arc::new(BlockingExecutor {
            starts: starts.clone(),
            started: started.clone(),
            release: release.clone(),
        });
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));

        handle
            .submit(TurnRequest::new("session", "first", "one", true))
            .await
            .expect("first turn accepted");
        handle
            .submit(TurnRequest::new("session", "second", "two", true))
            .await
            .expect("second turn accepted");

        timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("first turn started");
        assert_eq!(starts.lock().await.as_slice(), ["first"]);
        let snapshot = handle.snapshot("session").await.expect("snapshot");
        assert_eq!(snapshot.active_turn_id.as_deref(), Some("first"));
        assert_eq!(snapshot.queued_turns, 1);
        assert_eq!(snapshot.interaction, InteractionState::Running);

        release.notify_one();
        timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("second turn started only after first completed");
        assert_eq!(starts.lock().await.as_slice(), ["first", "second"]);
        release.notify_one();

        handle
            .cancel("session", "missing")
            .await
            .expect_err("unknown turn");
        worker.abort();
    }

    #[tokio::test]
    async fn cancellation_keeps_the_next_turn_queued_until_the_active_executor_exits() {
        let starts = Arc::new(Mutex::new(Vec::new()));
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let executor = Arc::new(BlockingExecutor {
            starts: starts.clone(),
            started: started.clone(),
            release: release.clone(),
        });
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));

        handle
            .submit(TurnRequest::new("session", "first", "one", true))
            .await
            .expect("first turn accepted");
        handle
            .submit(TurnRequest::new("session", "second", "two", true))
            .await
            .expect("second turn accepted");
        timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("first turn started");

        handle
            .cancel("session", "first")
            .await
            .expect("active cancellation accepted");
        timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("second turn started after cancelled executor returned");
        assert_eq!(starts.lock().await.as_slice(), ["first", "second"]);
        let snapshot = handle.snapshot("session").await.expect("snapshot");
        assert_eq!(snapshot.active_turn_id.as_deref(), Some("second"));
        assert_eq!(snapshot.interaction, InteractionState::Running);
        release.notify_one();
        worker.abort();
    }

    struct RegisteredInteractionExecutor {
        first_started: Arc<Notify>,
        cancellation_seen: Arc<Notify>,
        release_first: Arc<Notify>,
        second_started: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for RegisteredInteractionExecutor {
        async fn execute(
            &self,
            request: TurnRequest,
            context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            if request.turn_id == "first" {
                self.first_started.notify_one();
                context.cancellation().cancelled().await;
                self.cancellation_seen.notify_one();
                self.release_first.notified().await;
                return Err(RuntimeWorkerError::Cancelled);
            }
            self.second_started.notify_one();
            Ok(RuntimeTurnExecution::Completed {
                summary: "second completed".to_string(),
            })
        }
    }

    struct ValidatingInteractionDelivery {
        delivered_responses: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl RuntimeInteractionDelivery for ValidatingInteractionDelivery {
        fn validate(&self, interaction: &InteractionResponse) -> Result<(), RuntimeWorkerError> {
            if interaction.response == "approved" {
                Ok(())
            } else {
                Err(RuntimeWorkerError::ExecutionFailed(
                    "interaction response must be approved".to_string(),
                ))
            }
        }

        async fn deliver(
            self: Box<Self>,
            _request: TurnRequest,
            interaction: InteractionResponse,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.delivered_responses
                .lock()
                .await
                .push(interaction.response);
            Ok(RuntimeTurnExecution::InteractionResponseDelivered)
        }
    }

    /// A runtime-owned delivery validates before the active interaction is
    /// consumed, so malformed Web/automation input remains retryable.  On a
    /// valid response the worker, rather than an adapter map, owns the only
    /// path that can release the continuation.
    #[tokio::test]
    async fn registered_delivery_keeps_invalid_response_pending_then_releases_once() {
        let first_started = Arc::new(Notify::new());
        let cancellation_seen = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());
        let second_started = Arc::new(Notify::new());
        let executor = Arc::new(RegisteredInteractionExecutor {
            first_started: first_started.clone(),
            cancellation_seen: cancellation_seen.clone(),
            release_first: release_first.clone(),
            second_started,
        });
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));
        let delivered_responses = Arc::new(Mutex::new(Vec::new()));

        handle
            .submit(TurnRequest::new("session", "first", "ask", false))
            .await
            .expect("first turn accepted");
        timeout(Duration::from_secs(1), first_started.notified())
            .await
            .expect("first executor started");
        handle
            .register_interaction_with_delivery(
                "session",
                "first",
                InteractionState::AwaitingUserInput {
                    interaction_id: "input-1".to_string(),
                },
                Box::new(ValidatingInteractionDelivery {
                    delivered_responses: delivered_responses.clone(),
                }),
            )
            .await
            .expect("worker owns the registered interaction continuation");

        let error = handle
            .respond(
                "session",
                "first",
                InteractionResponse::new("input-1", "not approved"),
            )
            .await
            .expect_err("invalid response must remain retryable");
        assert!(matches!(error, RuntimeWorkerError::ExecutionFailed(_)));
        assert_eq!(
            handle
                .snapshot("session")
                .await
                .expect("snapshot")
                .interaction,
            InteractionState::AwaitingUserInput {
                interaction_id: "input-1".to_string(),
            }
        );
        assert!(delivered_responses.lock().await.is_empty());

        handle
            .respond(
                "session",
                "first",
                InteractionResponse::new("input-1", "approved"),
            )
            .await
            .expect("valid response is delivered by the worker-owned continuation");
        assert_eq!(delivered_responses.lock().await.as_slice(), ["approved"]);
        assert_eq!(
            handle
                .snapshot("session")
                .await
                .expect("snapshot")
                .interaction,
            InteractionState::Running
        );

        handle
            .cancel("session", "first")
            .await
            .expect("active turn cancellation accepted");
        timeout(Duration::from_secs(1), cancellation_seen.notified())
            .await
            .expect("original live executor observes cancellation");
        release_first.notify_one();
        worker.abort();
    }

    struct CancellationDelivery {
        response_tx: oneshot::Sender<()>,
    }

    #[async_trait]
    impl RuntimeInteractionDelivery for CancellationDelivery {
        fn validate(&self, _interaction: &InteractionResponse) -> Result<(), RuntimeWorkerError> {
            Ok(())
        }

        async fn deliver(
            self: Box<Self>,
            _request: TurnRequest,
            _interaction: InteractionResponse,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.response_tx.send(()).map_err(|_| {
                RuntimeWorkerError::ExecutionFailed(
                    "test interaction receiver closed before delivery".to_string(),
                )
            })?;
            Ok(RuntimeTurnExecution::InteractionResponseDelivered)
        }
    }

    /// A live tool handler can await only its interaction sender.  Cancelling
    /// must drop that sender before the original executor exits, or the two
    /// tasks can wait on each other forever.
    #[tokio::test]
    async fn cancel_of_registered_delivery_closes_continuation_before_executor_exit() {
        let first_started = Arc::new(Notify::new());
        let cancellation_seen = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());
        let executor = Arc::new(RegisteredInteractionExecutor {
            first_started: first_started.clone(),
            cancellation_seen: cancellation_seen.clone(),
            release_first: release_first.clone(),
            second_started: Arc::new(Notify::new()),
        });
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));
        let (response_tx, response_rx) = oneshot::channel();

        handle
            .submit(TurnRequest::new("session", "first", "ask", false))
            .await
            .expect("first turn accepted");
        timeout(Duration::from_secs(1), first_started.notified())
            .await
            .expect("first executor started");
        handle
            .register_interaction_with_delivery(
                "session",
                "first",
                InteractionState::AwaitingUserInput {
                    interaction_id: "input-1".to_string(),
                },
                Box::new(CancellationDelivery { response_tx }),
            )
            .await
            .expect("worker owns the parked continuation");

        handle
            .cancel("session", "first")
            .await
            .expect("active cancellation accepted");
        assert!(
            timeout(Duration::from_secs(1), response_rx)
                .await
                .expect("cancellation must release the parked continuation")
                .is_err(),
            "the sender must close before the original executor is released",
        );
        timeout(Duration::from_secs(1), cancellation_seen.notified())
            .await
            .expect("original executor sees its cancellation token");
        assert_eq!(
            handle
                .snapshot("session")
                .await
                .expect("snapshot")
                .interaction,
            InteractionState::Cancelling,
            "the worker retains the serial slot while the original executor unwinds",
        );

        release_first.notify_one();
        worker.abort();
    }

    /// Tool-loop adapters can remain alive while a handler waits on a UI
    /// oneshot. Registering that state must not make cancel discard the active
    /// turn before the original future has observed its cancellation token.
    #[tokio::test]
    async fn cancel_of_registered_live_interaction_waits_for_executor_exit() {
        let first_started = Arc::new(Notify::new());
        let cancellation_seen = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());
        let second_started = Arc::new(Notify::new());
        let executor = Arc::new(RegisteredInteractionExecutor {
            first_started: first_started.clone(),
            cancellation_seen: cancellation_seen.clone(),
            release_first: release_first.clone(),
            second_started: second_started.clone(),
        });
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));

        handle
            .submit(TurnRequest::new("session", "first", "ask", false))
            .await
            .expect("first turn accepted");
        timeout(Duration::from_secs(1), first_started.notified())
            .await
            .expect("first executor started");
        handle
            .register_interaction(
                "session",
                "first",
                InteractionState::AwaitingUserInput {
                    interaction_id: "input-1".to_string(),
                },
            )
            .await
            .expect("live executor registers its interaction through the worker");
        handle
            .submit(TurnRequest::new("session", "second", "next", false))
            .await
            .expect("second turn queued");

        handle
            .cancel("session", "first")
            .await
            .expect("cancel request accepted");
        timeout(Duration::from_secs(1), cancellation_seen.notified())
            .await
            .expect("live executor observes the cancellation token");
        assert!(
            timeout(Duration::from_millis(50), second_started.notified())
                .await
                .is_err(),
            "worker must retain the active slot until the live executor exits"
        );

        release_first.notify_one();
        timeout(Duration::from_secs(1), second_started.notified())
            .await
            .expect("queued turn starts only after the cancelled executor exits");
        worker.abort();
    }

    struct FailingResponseExecutor {
        first_started: Arc<Notify>,
        cancellation_seen: Arc<Notify>,
        release_first: Arc<Notify>,
        second_started: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for FailingResponseExecutor {
        async fn execute(
            &self,
            request: TurnRequest,
            context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            if request.turn_id == "first" {
                self.first_started.notify_one();
                context.cancellation().cancelled().await;
                self.cancellation_seen.notify_one();
                self.release_first.notified().await;
                return Err(RuntimeWorkerError::Cancelled);
            }
            self.second_started.notify_one();
            Ok(RuntimeTurnExecution::Completed {
                summary: "second completed".to_string(),
            })
        }

        async fn respond(
            &self,
            _request: TurnRequest,
            _interaction: InteractionResponse,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            Err(RuntimeWorkerError::ExecutionFailed(
                "durable interaction response write failed".to_string(),
            ))
        }
    }

    /// A response task can fail while the original tool-loop future is still
    /// running. Keep that future as the active turn and cancel it before a
    /// queued follow-up is allowed to start.
    #[tokio::test]
    async fn failed_response_delivery_waits_for_original_executor_exit() {
        let first_started = Arc::new(Notify::new());
        let cancellation_seen = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());
        let second_started = Arc::new(Notify::new());
        let executor = Arc::new(FailingResponseExecutor {
            first_started: first_started.clone(),
            cancellation_seen: cancellation_seen.clone(),
            release_first: release_first.clone(),
            second_started: second_started.clone(),
        });
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));

        handle
            .submit(TurnRequest::new("session", "first", "ask", false))
            .await
            .expect("first turn accepted");
        timeout(Duration::from_secs(1), first_started.notified())
            .await
            .expect("first executor started");
        handle
            .register_interaction(
                "session",
                "first",
                InteractionState::AwaitingUserInput {
                    interaction_id: "input-1".to_string(),
                },
            )
            .await
            .expect("live executor interaction registered");
        handle
            .submit(TurnRequest::new("session", "second", "next", false))
            .await
            .expect("second turn queued");

        handle
            .respond(
                "session",
                "first",
                InteractionResponse::new("input-1", "continue"),
            )
            .await
            .expect_err("failing response delivery must be reported to the caller");
        timeout(Duration::from_secs(1), cancellation_seen.notified())
            .await
            .expect("response failure cancels the original live executor");
        assert!(
            timeout(Duration::from_millis(50), second_started.notified())
                .await
                .is_err(),
            "the queued turn must not overlap the failed response's original executor"
        );

        release_first.notify_one();
        timeout(Duration::from_secs(1), second_started.notified())
            .await
            .expect("queued turn starts only after the original executor exits");
        worker.abort();
    }

    struct InteractionExecutor {
        responses: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for InteractionExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            Ok(RuntimeTurnExecution::AwaitingInteraction(
                InteractionState::AwaitingToolApproval {
                    interaction_id: "approve-1".to_string(),
                    tool_name: "apply_patch".to_string(),
                },
            ))
        }

        async fn respond(
            &self,
            _request: TurnRequest,
            interaction: InteractionResponse,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.responses.lock().await.push(interaction.response);
            Ok(RuntimeTurnExecution::Completed {
                summary: "approved".to_string(),
            })
        }
    }

    #[tokio::test]
    async fn interaction_response_advances_the_same_active_turn() {
        let responses = Arc::new(Mutex::new(Vec::new()));
        let executor = Arc::new(InteractionExecutor {
            responses: responses.clone(),
        });
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));
        let mut events = handle
            .observe(EventCursor::new("session", 0))
            .await
            .expect("event stream");

        handle
            .submit(TurnRequest::new(
                "session",
                "turn",
                "apply a safe patch",
                true,
            ))
            .await
            .expect("turn accepted");
        let interaction_event = timeout(Duration::from_secs(1), async {
            loop {
                let event = events.recv().await.expect("event stream open");
                if matches!(event.kind, AgentEventKind::InteractionRequested { .. }) {
                    return event;
                }
            }
        })
        .await
        .expect("approval interaction emitted");
        assert_eq!(interaction_event.turn_id.as_deref(), Some("turn"));

        handle
            .respond(
                "session",
                "turn",
                InteractionResponse::new("approve-1", "approved"),
            )
            .await
            .expect("approved response accepted");
        timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = handle.snapshot("session").await.expect("snapshot");
                if snapshot.interaction == InteractionState::Completed {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("response completed turn");
        assert_eq!(responses.lock().await.as_slice(), ["approved"]);
        worker.abort();
    }

    struct AwaitingInteractionResponseFailureExecutor;

    #[async_trait]
    impl RuntimeTurnExecutor for AwaitingInteractionResponseFailureExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            Ok(RuntimeTurnExecution::AwaitingInteraction(
                InteractionState::AwaitingUserInput {
                    interaction_id: "input-1".to_string(),
                },
            ))
        }

        async fn respond(
            &self,
            _request: TurnRequest,
            _interaction: InteractionResponse,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            Err(RuntimeWorkerError::ExecutionFailed(
                "response validation failed after dispatch".to_string(),
            ))
        }
    }

    /// Some adapters return `AwaitingInteraction` after parking their
    /// executor. A later response failure has no live future to cancel, so
    /// it must terminalize immediately instead of leaving the turn stuck.
    #[tokio::test]
    async fn failed_response_without_live_executor_terminalizes_immediately() {
        let executor = Arc::new(AwaitingInteractionResponseFailureExecutor);
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));

        handle
            .submit(TurnRequest::new("session", "turn", "ask", false))
            .await
            .expect("turn accepted");
        timeout(Duration::from_secs(1), async {
            loop {
                if handle
                    .snapshot("session")
                    .await
                    .expect("snapshot")
                    .interaction
                    .is_awaiting_response()
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("turn reaches an interaction state");

        handle
            .respond(
                "session",
                "turn",
                InteractionResponse::new("input-1", "continue"),
            )
            .await
            .expect_err("failing response delivery must be reported");
        let snapshot = handle.snapshot("session").await.expect("terminal snapshot");
        assert!(snapshot.active_turn_id.is_none());
        assert!(matches!(
            snapshot.interaction,
            InteractionState::Failed { .. }
        ));
        worker.abort();
    }

    struct MutatingCancellationExecutor {
        started: Arc<Notify>,
        response_rx: Mutex<Option<oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for MutatingCancellationExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            context.mark_mutation_started();
            self.started.notify_one();
            let response_rx = self.response_rx.lock().await.take().ok_or_else(|| {
                RuntimeWorkerError::ExecutionFailed(
                    "mutating test executor lost its interaction receiver".to_string(),
                )
            })?;
            response_rx.await.map_err(|_| {
                RuntimeWorkerError::ExecutionFailed(
                    "mutating interaction continuation was closed during cancellation".to_string(),
                )
            })?;
            Ok(RuntimeTurnExecution::Completed {
                summary: "unexpected test response".to_string(),
            })
        }
    }

    /// Closing a continuation after a mutation may make the executor return a
    /// normal error rather than `Cancelled`. That cannot be treated as an
    /// ordinary failure: the already-started side effect needs reconciliation.
    #[tokio::test]
    async fn cancelled_mutating_delivery_error_requires_reconciliation() {
        let started = Arc::new(Notify::new());
        let (response_tx, response_rx) = oneshot::channel();
        let executor = Arc::new(MutatingCancellationExecutor {
            started: started.clone(),
            response_rx: Mutex::new(Some(response_rx)),
        });
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));

        handle
            .submit(TurnRequest::new("session", "turn", "mutate", true))
            .await
            .expect("mutating turn accepted");
        timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("mutating executor started");
        handle
            .register_interaction_with_delivery(
                "session",
                "turn",
                InteractionState::AwaitingToolApproval {
                    interaction_id: "approve-1".to_string(),
                    tool_name: "shell".to_string(),
                },
                Box::new(CancellationDelivery { response_tx }),
            )
            .await
            .expect("worker owns the mutating continuation");

        handle
            .cancel("session", "turn")
            .await
            .expect("post-mutation cancellation is recorded");
        timeout(Duration::from_secs(1), async {
            loop {
                if handle
                    .snapshot("session")
                    .await
                    .expect("snapshot")
                    .active_turn_id
                    .is_none()
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("closed continuation lets the mutating executor finish");
        assert!(matches!(
            handle
                .snapshot("session")
                .await
                .expect("snapshot")
                .interaction,
            InteractionState::IndeterminateSideEffect { .. }
        ));
        assert!(matches!(
            handle
                .submit(TurnRequest::new("session", "next", "retry", false))
                .await,
            Err(RuntimeWorkerError::ReconciliationRequired { .. })
        ));
        worker.abort();
    }

    struct MutatingResponseFailureExecutor;

    #[async_trait]
    impl RuntimeTurnExecutor for MutatingResponseFailureExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            context.mark_mutation_started();
            Ok(RuntimeTurnExecution::AwaitingInteraction(
                InteractionState::AwaitingToolApproval {
                    interaction_id: "approve-1".to_string(),
                    tool_name: "shell".to_string(),
                },
            ))
        }

        async fn respond(
            &self,
            _request: TurnRequest,
            _interaction: InteractionResponse,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            Err(RuntimeWorkerError::ExecutionFailed(
                "interaction audit persistence failed".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn failed_response_after_mutation_requires_reconciliation() {
        let executor = Arc::new(MutatingResponseFailureExecutor);
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));

        handle
            .submit(TurnRequest::new("session", "turn", "mutate", true))
            .await
            .expect("turn accepted");
        timeout(Duration::from_secs(1), async {
            loop {
                if handle
                    .snapshot("session")
                    .await
                    .expect("snapshot")
                    .interaction
                    .is_awaiting_response()
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("turn reaches approval interaction");

        handle
            .respond(
                "session",
                "turn",
                InteractionResponse::new("approve-1", "approve"),
            )
            .await
            .expect_err("failing mutating response delivery must be reported");
        let snapshot = handle.snapshot("session").await.expect("terminal snapshot");
        assert!(snapshot.active_turn_id.is_none());
        assert!(matches!(
            snapshot.interaction,
            InteractionState::IndeterminateSideEffect { .. }
        ));
        handle
            .submit(TurnRequest::new("session", "next", "retry", false))
            .await
            .expect_err("indeterminate mutation must block subsequent turns");
        worker.abort();
    }

    struct DelayedInteractionResponseExecutor {
        response_started: Arc<Notify>,
        release_response: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for DelayedInteractionResponseExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            Ok(RuntimeTurnExecution::AwaitingInteraction(
                InteractionState::AwaitingUserInput {
                    interaction_id: "input-1".to_string(),
                },
            ))
        }

        async fn respond(
            &self,
            _request: TurnRequest,
            _interaction: InteractionResponse,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.response_started.notify_one();
            self.release_response.notified().await;
            Ok(RuntimeTurnExecution::Completed {
                summary: "input delivered".to_string(),
            })
        }
    }

    /// A browser or automation caller must not be told an interaction was
    /// resolved while the adapter can still fail to deliver it to the original
    /// continuation. This also prevents a client from discarding its response
    /// and retrying a non-idempotent approval after a late executor error.
    #[tokio::test]
    async fn interaction_response_waits_for_executor_delivery() {
        let response_started = Arc::new(Notify::new());
        let release_response = Arc::new(Notify::new());
        let executor = Arc::new(DelayedInteractionResponseExecutor {
            response_started: response_started.clone(),
            release_response: release_response.clone(),
        });
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));

        handle
            .submit(TurnRequest::new("session", "turn", "ask", false))
            .await
            .expect("turn accepted");
        timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = handle.snapshot("session").await.expect("snapshot");
                if snapshot.interaction.is_awaiting_response() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("turn waits for the user input response");

        let response_handle = handle.clone();
        let mut response_task = tokio::spawn(async move {
            response_handle
                .respond(
                    "session",
                    "turn",
                    InteractionResponse::new("input-1", r#"{\"answer\":\"continue\"}"#),
                )
                .await
        });
        timeout(Duration::from_secs(1), response_started.notified())
            .await
            .expect("executor starts delivering the response");
        assert!(
            timeout(Duration::from_millis(50), &mut response_task)
                .await
                .is_err(),
            "handle.respond must wait for executor delivery rather than acknowledge enqueue"
        );

        release_response.notify_one();
        response_task
            .await
            .expect("response task does not panic")
            .expect("response succeeds after delivery");
        assert_eq!(
            handle
                .snapshot("session")
                .await
                .expect("terminal snapshot")
                .interaction,
            InteractionState::Completed,
        );
        worker.abort();
    }

    struct RacingInteractionExecutor {
        release_execution: Arc<Notify>,
        execution_finished: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for RacingInteractionExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.release_execution.notified().await;
            self.execution_finished.notify_one();
            Ok(RuntimeTurnExecution::Completed {
                summary: "continued after input".to_string(),
            })
        }

        async fn respond(
            &self,
            _request: TurnRequest,
            _interaction: InteractionResponse,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            self.release_execution.notify_one();
            self.execution_finished.notified().await;
            // Give the original executor's `ExecutionFinished` actor command
            // a chance to arrive before the response task reports success.
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(RuntimeTurnExecution::InteractionResponseDelivered)
        }
    }

    /// If the original tool-loop continuation finishes immediately after a
    /// response is sent, the worker must not publish a terminal event before
    /// the response acknowledgement. Otherwise a browser can observe a
    /// completed turn followed by a stale interaction response event.
    #[tokio::test]
    async fn response_acknowledgement_precedes_racing_terminal_event() {
        let release_execution = Arc::new(Notify::new());
        let execution_finished = Arc::new(Notify::new());
        let executor = Arc::new(RacingInteractionExecutor {
            release_execution: release_execution.clone(),
            execution_finished: execution_finished.clone(),
        });
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));
        let mut events = handle
            .observe(EventCursor::new("session", 0))
            .await
            .expect("event stream");

        handle
            .submit(TurnRequest::new("session", "turn", "ask", false))
            .await
            .expect("turn accepted");
        handle
            .register_interaction(
                "session",
                "turn",
                InteractionState::AwaitingUserInput {
                    interaction_id: "input-1".to_string(),
                },
            )
            .await
            .expect("live executor interaction registered");
        handle
            .respond(
                "session",
                "turn",
                InteractionResponse::new("input-1", "continue"),
            )
            .await
            .expect("response delivered");

        let mut relevant = Vec::new();
        timeout(Duration::from_secs(1), async {
            while relevant.len() < 2 {
                let event = events.recv().await.expect("event stream remains open");
                if matches!(
                    event.kind,
                    AgentEventKind::InteractionResponded { .. }
                        | AgentEventKind::TurnCompleted { .. }
                ) {
                    relevant.push(event.kind);
                }
            }
        })
        .await
        .expect("response and terminal events emitted");
        assert!(matches!(
            relevant.as_slice(),
            [
                AgentEventKind::InteractionResponded { .. },
                AgentEventKind::TurnCompleted { .. }
            ]
        ));
        worker.abort();
    }

    struct WaitingThenBlockingExecutor {
        second_started: Arc<Notify>,
        release_second: Arc<Notify>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for WaitingThenBlockingExecutor {
        async fn execute(
            &self,
            request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            if request.turn_id == "first" {
                return Ok(RuntimeTurnExecution::AwaitingInteraction(
                    InteractionState::AwaitingIntentReview {
                        interaction_id: "intent-review".to_string(),
                    },
                ));
            }
            self.second_started.notify_one();
            self.release_second.notified().await;
            Ok(RuntimeTurnExecution::Completed {
                summary: "second completed".to_string(),
            })
        }
    }

    #[tokio::test]
    async fn cancelling_a_waiting_interaction_releases_the_next_turn() {
        let second_started = Arc::new(Notify::new());
        let release_second = Arc::new(Notify::new());
        let executor = Arc::new(WaitingThenBlockingExecutor {
            second_started: second_started.clone(),
            release_second: release_second.clone(),
        });
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));

        handle
            .submit(TurnRequest::new("session", "first", "review intent", true))
            .await
            .expect("first turn accepted");
        timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = handle.snapshot("session").await.expect("snapshot");
                if snapshot.interaction.is_awaiting_response() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first turn waits for a response");
        handle
            .submit(TurnRequest::new("session", "second", "next", true))
            .await
            .expect("second turn accepted");

        handle
            .cancel("session", "first")
            .await
            .expect("waiting turn cancelled");
        timeout(Duration::from_secs(1), second_started.notified())
            .await
            .expect("second turn starts after waiting turn cancellation");
        let snapshot = handle.snapshot("session").await.expect("snapshot");
        assert_eq!(snapshot.active_turn_id.as_deref(), Some("second"));
        assert_eq!(snapshot.interaction, InteractionState::Running);

        release_second.notify_one();
        worker.abort();
    }

    struct ImmediateExecutor;

    #[async_trait]
    impl RuntimeTurnExecutor for ImmediateExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            _context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            Ok(RuntimeTurnExecution::Completed {
                summary: "done".to_string(),
            })
        }
    }

    #[tokio::test]
    async fn event_stream_is_not_lagged_by_other_sessions() {
        let mut worker_config = config(Arc::new(ImmediateExecutor));
        worker_config.event_buffer = 4;
        let (handle, worker) = AgentRuntimeWorker::spawn(worker_config);
        let mut session_a_events = handle
            .observe(EventCursor::new("session-a", 0))
            .await
            .expect("session-scoped event stream");

        for index in 0..8 {
            let session_id = format!("other-session-{index}");
            handle
                .submit(TurnRequest::new(session_id, "turn", "unrelated", false))
                .await
                .expect("unrelated turn accepted");
        }
        handle
            .submit(TurnRequest::new("session-a", "turn", "observed", false))
            .await
            .expect("observed turn accepted");

        let event = timeout(Duration::from_secs(1), session_a_events.recv())
            .await
            .expect("session A event available")
            .expect("other sessions must not cause a lagged stream");
        assert_eq!(event.session_id, "session-a");
        assert_eq!(event.cursor.session_id, "session-a");
        worker.abort();
    }

    struct PolicyCheckingExecutor {
        decision: Arc<Mutex<Option<BoundaryDecision>>>,
    }

    #[async_trait]
    impl RuntimeTurnExecutor for PolicyCheckingExecutor {
        async fn execute(
            &self,
            _request: TurnRequest,
            context: RuntimeExecutionContext,
        ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
            let decision = context.authorize(&ToolOperation::tool("apply_patch", true, false));
            *self.decision.lock().await = Some(decision);
            Ok(RuntimeTurnExecution::Completed {
                summary: "checked policy".to_string(),
            })
        }
    }

    #[tokio::test]
    async fn executor_receives_the_shared_hardening_boundary() {
        let decision = Arc::new(Mutex::new(None));
        let executor = Arc::new(PolicyCheckingExecutor {
            decision: decision.clone(),
        });
        let (handle, worker) = AgentRuntimeWorker::spawn(config(executor));
        handle
            .submit(TurnRequest::new("session", "turn", "patch", true))
            .await
            .expect("turn accepted");

        timeout(Duration::from_secs(1), async {
            loop {
                if decision.lock().await.is_some() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("executor received hardening context");
        let observed = decision.lock().await.clone().expect("decision captured");
        assert!(observed.allowed);
        assert!(observed.approval_required);
        worker.abort();
    }
}
