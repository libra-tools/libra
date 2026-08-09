//! Headless web-only runtime for non-Codex providers.
//!
//! `--web-only --provider <X>` (X != codex) used to fall back to a read-only
//! placeholder snapshot, leaving the browser unable to drive the agent. This
//! module provides the minimum-viable replacement: a [`HeadlessCodeRuntime`]
//! that owns a [`CodeUiSession`], submits each browser turn to the shared
//! [`crate::internal::ai::runtime::AgentRuntimeWorker`], and streams the
//! model's output back into the session transcript through that worker's
//! executor boundary.
//!
//! # v0 scope (Phase 3 minimum)
//!
//! - `submitMessage` queues a user message through `AgentRuntimeHandle` — the
//!   worker's executor runs the standard `run_tool_loop_with_history_and_observer`
//!   and the assistant reply lands in the live snapshot, streamed delta-by-delta.
//! - `cancelTurn` cooperatively stops model or read-only work and marks the
//!   assistant entry as cancelled. A started mutation is never hard-aborted:
//!   cancellation returns an actionable error until its determinate result is
//!   available.
//! - The runtime reuses the caller-provided [`ToolRegistry`] and
//!   [`ToolLoopConfig`], so the same allow-list / hooks / sandbox boundaries
//!   that protect the TUI agent also apply here.
//!
//! # Phase 3 follow-up target
//!
//! - IntentSpec / Plan workflow integration. The TUI's Phase 0/1 review loop
//!   is deeply coupled to the ratatui [`crate::internal::tui::app::App`]; this
//!   runtime treats every browser submit as a single direct turn instead.
//! - Full IntentSpec plan approval remains future work; direct `update_plan`
//!   and `apply_patch` tool projections are surfaced in the shared Code UI
//!   snapshot.
//!
//! These follow-ups are explicitly called out in
//! `docs/development/commands/_general.md` and will land in subsequent phases.

use std::{
    collections::HashMap,
    io,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::anyhow;
use async_trait::async_trait;
use chrono::Utc;
use tokio::{
    sync::{Mutex, mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::code_ui::{
    CodeUiApiError, CodeUiApplyToFuture, CodeUiCapabilities, CodeUiCommandAdapter, CodeUiEventType,
    CodeUiInteractionKind, CodeUiInteractionOption, CodeUiInteractionRequest,
    CodeUiInteractionResponse, CodeUiInteractionStatus, CodeUiPatchChange, CodeUiPatchsetSnapshot,
    CodeUiPlanSnapshot, CodeUiPlanStep, CodeUiReadModel, CodeUiSession, CodeUiSessionSnapshot,
    CodeUiSessionStatus, CodeUiToolCallSnapshot, CodeUiTranscriptEntry, CodeUiTranscriptEntryKind,
};
use crate::internal::ai::{
    agent::runtime::{ToolLoopCancellation, run_tool_loop_with_history_and_observer},
    completion::{
        CompletionError, CompletionModel, CompletionStreamEvent, CompletionUsage,
        CompletionUsageSummary, Message,
    },
    runtime::{
        AgentRuntimeHandle, AgentRuntimeWorker, AgentRuntimeWorkerConfig, AgentSnapshot,
        InteractionState, RuntimeCommandDurability, RuntimeExecutionContext,
        RuntimeInteractionDelivery, RuntimeTurnExecution, RuntimeTurnExecutor, RuntimeWorkerError,
        TurnRequest, runtime_worker_adapter_message,
    },
    sandbox::{ExecApprovalRequest, NetworkAccess, ReviewDecision},
    session::{CodeWorkflowEventKind, SessionJsonlStore, SessionState, SessionStore},
    tools::{
        ToolOutput, ToolRegistry,
        context::{
            StepStatus, SubmitPlanDraftArgs, UpdatePlanArgs, UserInputAnswer, UserInputQuestion,
            UserInputRequest, UserInputResponse,
        },
    },
};

/// Capabilities advertised by the headless runtime.
///
/// `messageInput`, streaming text, tool calls, direct plan updates, patchsets,
/// approval interactions, structured questions, and session resume are delivered
/// by the headless runtime. Full IntentSpec workflow approval stays gated.
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
    }
}

/// Bound graceful shutdown waits so a stuck provider cannot leave the CLI
/// indefinitely unresponsive. The timeout error is deliberately actionable;
/// the caller must surface it rather than silently treating shutdown as clean.
const HEADLESS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const HEADLESS_BROWSER_PRINCIPAL: &str = "web-headless-browser";
const HEADLESS_DIRECT_TURN_KIND: &str = "headless_direct_turn";

#[derive(Clone)]
pub struct HeadlessSessionPersistence {
    store: Arc<SessionStore>,
    state: Arc<Mutex<SessionState>>,
    projection_store: SessionJsonlStore,
    projection_checkpoint: Arc<Mutex<HeadlessProjectionCheckpoint>>,
    durability_repo_id: String,
    durability_session_id: String,
}

struct HeadlessProjectionCheckpoint {
    snapshot: CodeUiSessionSnapshot,
    sequence: u64,
}

impl HeadlessSessionPersistence {
    /// Construct persistence for callers that do not yet have a restored
    /// projection checkpoint. The first persisted snapshot becomes the
    /// checkpoint through normal fine-grained delta emission.
    pub fn new(store: Arc<SessionStore>, state: SessionState) -> Self {
        Self::with_projection_checkpoint(store, state, CodeUiSessionSnapshot::default(), 0)
    }

    /// Construct persistence from the durable legacy snapshot and its last
    /// workflow cursor. This is the resume path used by `libra code`.
    pub fn with_projection_checkpoint(
        store: Arc<SessionStore>,
        state: SessionState,
        initial_projection_snapshot: CodeUiSessionSnapshot,
        initial_projection_sequence: u64,
    ) -> Self {
        let projection_store = SessionJsonlStore::new(store.session_root(&state.id));
        Self {
            store,
            state: Arc::new(Mutex::new(state.clone())),
            projection_store,
            projection_checkpoint: Arc::new(Mutex::new(HeadlessProjectionCheckpoint {
                snapshot: initial_projection_snapshot,
                sequence: initial_projection_sequence,
            })),
            durability_repo_id: state.working_dir.clone(),
            durability_session_id: state.id.clone(),
        }
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

    async fn record_user_message(
        &self,
        snapshot: CodeUiSessionSnapshot,
        content: &str,
    ) -> io::Result<()> {
        let sequence = self.persist_projection_deltas(&snapshot).await?;
        let mut state = self.state.lock().await;
        state.add_user_message(content);
        sync_session_metadata_from_snapshot(&mut state, snapshot, sequence)?;
        self.store.save(&state)
    }

    async fn record_assistant_message(
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

    async fn persist_snapshot(&self, snapshot: CodeUiSessionSnapshot) -> io::Result<()> {
        let sequence = self.persist_projection_deltas(&snapshot).await?;
        let mut state = self.state.lock().await;
        sync_session_metadata_from_snapshot(&mut state, snapshot, sequence)?;
        self.store.save(&state)
    }

    /// Record the non-sensitive fact that a browser interaction was resolved
    /// before its continuation is released. Projection deltas make the UI
    /// resumable, while this durable workflow event is the audit fact used to
    /// distinguish a response from a merely rendered interaction state.
    fn record_interaction_resolution(
        &self,
        interaction_id: &str,
        resolution: &str,
    ) -> io::Result<()> {
        self.projection_store.append_code_workflow_durable(
            CodeWorkflowEventKind::InteractionResolved {
                interaction_id: interaction_id.to_string(),
                resolution: resolution.to_string(),
            },
        )?;
        Ok(())
    }

    /// Persist only the projection fields that changed since the last durable
    /// headless checkpoint.  `SessionSnapshot` remains the compatibility
    /// record, while these ordered deltas are the authoritative Code UI suffix
    /// replayed on resume.
    async fn persist_projection_deltas(&self, snapshot: &CodeUiSessionSnapshot) -> io::Result<u64> {
        let mut checkpoint = self.projection_checkpoint.lock().await;
        let deltas = code_ui_projection_deltas(&checkpoint.snapshot, snapshot)?;
        for delta in deltas {
            let event = self.projection_store.append_code_workflow(delta)?;
            checkpoint.sequence = event.sequence;
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

struct PendingHeadlessUserInput {
    questions: Vec<UserInputQuestion>,
    response_tx: oneshot::Sender<UserInputResponse>,
}

struct PendingHeadlessExecApproval {
    request: ExecApprovalRequest,
}

/// A live tool-loop continuation held by `AgentRuntimeWorker` while the Web
/// session is awaiting an interaction response.  The legacy pending maps below
/// are retained only for standalone, no-active-turn adapter calls; browser
/// turns must register one of these so validation, durable audit, and one-shot
/// release have a single owner.
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
}

#[async_trait]
impl RuntimeInteractionDelivery for HeadlessInteractionDelivery {
    fn validate(
        &self,
        interaction: &crate::internal::ai::runtime::InteractionResponse,
    ) -> Result<(), RuntimeWorkerError> {
        let response = decode_headless_interaction_response(interaction)?;
        match self {
            Self::UserInput { questions, .. } => {
                user_input_response_from_code_ui_request(questions, response)
                    .map(|_| ())
                    .map_err(|error| RuntimeWorkerError::ExecutionFailed(error.to_string()))
            }
            Self::ExecApproval { .. } => review_decision_from_interaction_response(response)
                .map(|_| ())
                .map_err(|error| RuntimeWorkerError::ExecutionFailed(error.to_string())),
        }
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
        let response = decode_headless_interaction_response(&interaction)?;
        match *self {
            Self::UserInput {
                session,
                interaction_persistence_failed,
                persistence,
                interaction_id,
                questions,
                response_tx,
            } => {
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
                request,
            } => {
                deliver_headless_exec_approval_response(
                    &session,
                    &interaction_persistence_failed,
                    persistence.as_ref(),
                    &interaction_id,
                    request,
                    response,
                )
                .await
            }
        }
    }
}

/// Adapter that runs an agent tool loop in response to browser-driven messages.
///
/// Generic over a [`CompletionModel`] so each provider (Ollama, OpenAI, Gemini,
/// …) can plug in its own client. The model is held inside an `Arc<Mutex<…>>`
/// so the spawned turn task can take exclusive access while the next submit
/// waits in the queue.
/// Bookkeeping for a browser turn accepted by the serialized worker.
///
/// The gate keeps the worker executor from running a tool before the browser
/// turn's durable initial projection has been written. This preserves the
/// retry-safe persistence precondition even though worker admission itself is
/// intentionally asynchronous.
struct InFlightTurn {
    runtime_turn_id: String,
    assistant_entry_id: String,
    start_gate: Arc<tokio::sync::Notify>,
    start_open: Arc<AtomicBool>,
    /// Signals once terminal UI state and the worker's active-turn slot have
    /// settled, including admission rollback after a durability failure.
    completion: Arc<tokio::sync::Notify>,
}

/// Adapter from the UI-neutral serialized runtime to the existing headless
/// provider/tool-loop stack. It deliberately owns no queueing state: ordering,
/// cancellation and shutdown belong to `AgentRuntimeWorker`.
struct HeadlessDirectTurnExecutor<M: CompletionModel + 'static> {
    session: Arc<CodeUiSession>,
    history: Arc<Mutex<Vec<Message>>>,
    model: Arc<M>,
    registry: Arc<ToolRegistry>,
    config_factory:
        Arc<dyn Fn() -> super::super::agent::runtime::tool_loop::ToolLoopConfig + Send + Sync>,
    in_flight: Arc<Mutex<Option<InFlightTurn>>>,
    active_turn_mutations: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    shutdown_timed_out: Arc<AtomicBool>,
    /// A browser interaction response or request could not be durably
    /// projected. The original tool-loop may still be unwinding, so its later
    /// terminal result must not overwrite the reconciliation requirement.
    interaction_persistence_failed: Arc<AtomicBool>,
    pending_user_inputs: Arc<Mutex<HashMap<String, PendingHeadlessUserInput>>>,
    pending_exec_approvals: Arc<Mutex<HashMap<String, PendingHeadlessExecApproval>>>,
    persistence: Option<HeadlessSessionPersistence>,
}

pub struct HeadlessCodeRuntime<M: CompletionModel + 'static> {
    // The provider model lives in the runtime executor; keep the public
    // adapter generic so callers cannot accidentally pair an executor built
    // for one provider type with a differently typed headless handle.
    model_type: PhantomData<M>,
    session: Arc<CodeUiSession>,
    capabilities: CodeUiCapabilities,
    /// Active turn slot. `submit_message` holds the lock while it spawns and
    /// stores the worker request so two concurrent submits can never both see
    /// an empty slot. `cancel_turn` and the runtime executor acquire the lock
    /// to release / finalize the slot.
    in_flight: Arc<Mutex<Option<InFlightTurn>>>,
    /// Monotonic turn id; used by spawned tasks to detect that a successor
    /// turn has claimed the slot before they cleared their own entry.
    next_turn_id: Arc<AtomicU64>,
    /// Session identity for the in-memory worker. It is intentionally opaque
    /// to the browser and never contains request text.
    runtime_session_id: String,
    /// The only path browser turns use to enter the serialized runtime.
    runtime: AgentRuntimeHandle,
    /// Retained so explicit shutdown can join the worker and report a panic
    /// rather than silently detaching the lifecycle owner.
    runtime_worker_task: Mutex<Option<JoinHandle<()>>>,
    /// Worker-owned mutation markers registered by the executor. The browser
    /// adapter reads these only to preserve its actionable "cannot safely
    /// abort" response after it has asked the runtime to reconcile a turn.
    active_turn_mutations: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    /// Once shutdown begins, no adapter command may start a replacement turn
    /// while the previous in-flight turn is being reconciled.
    shutting_down: AtomicBool,
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
    /// Pending `request_user_input` flows keyed by tool call id.
    pending_user_inputs: Arc<Mutex<HashMap<String, PendingHeadlessUserInput>>>,
    /// Pending exec approval flows keyed by tool call id.
    pending_exec_approvals: Arc<Mutex<HashMap<String, PendingHeadlessExecApproval>>>,
    /// Optional on-disk session persistence used by `libra code --web-only
    /// --resume <thread_id>` for non-Codex providers.
    persistence: Option<HeadlessSessionPersistence>,
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
            config_factory,
            Vec::new(),
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
        shutdown_timeout: Duration,
    ) -> anyhow::Result<Arc<Self>> {
        let (shutdown_result_tx, _) = watch::channel(None);
        let in_flight = Arc::new(Mutex::new(None));
        let history = Arc::new(Mutex::new(initial_history));
        let shutdown_timed_out = Arc::new(AtomicBool::new(false));
        let interaction_persistence_failed = Arc::new(AtomicBool::new(false));
        let active_turn_mutations = Arc::new(Mutex::new(HashMap::new()));
        let pending_user_inputs = Arc::new(Mutex::new(HashMap::new()));
        let pending_exec_approvals = Arc::new(Mutex::new(HashMap::new()));
        let tool_boundary = registry.hardening().cloned().ok_or_else(|| {
            anyhow!(
                "Headless Code runtime requires the registry's shared tool-boundary policy; rebuild CodeAgentServices before starting a browser turn"
            )
        })?;
        let executor = Arc::new(HeadlessDirectTurnExecutor {
            session: session.clone(),
            history: history.clone(),
            model: Arc::new(model),
            registry,
            config_factory,
            in_flight: in_flight.clone(),
            active_turn_mutations: active_turn_mutations.clone(),
            shutdown_timed_out: shutdown_timed_out.clone(),
            interaction_persistence_failed: interaction_persistence_failed.clone(),
            pending_user_inputs: pending_user_inputs.clone(),
            pending_exec_approvals: pending_exec_approvals.clone(),
            persistence: persistence.clone(),
        });
        let mut worker_config = AgentRuntimeWorkerConfig::new(executor, tool_boundary);
        worker_config.shutdown_timeout = shutdown_timeout;
        let runtime_session_id = persistence
            .as_ref()
            .map(HeadlessSessionPersistence::durability_session_id)
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut recovered_reconciliation = false;
        if let Some(persistence) = persistence.as_ref() {
            let (durability, repo_id, principal_id) = persistence.worker_durability_config();
            let recovered_mutations = durability.recover_pending_mutations().map_err(|error| {
                anyhow!(
                    "failed to recover pending durable commands for headless Code session '{}': {error}",
                    persistence.durability_session_id()
                )
            })?;
            worker_config = worker_config
                .with_durability(durability, repo_id, principal_id)
                .with_durability_command_kind(HEADLESS_DIRECT_TURN_KIND);
            if !recovered_mutations.is_empty() {
                recovered_reconciliation = true;
                worker_config = worker_config
                    .with_recovered_reconciliation_session(persistence.durability_session_id());
            }
        }
        let (runtime_handle, runtime_worker_task) = AgentRuntimeWorker::spawn(worker_config);
        let runtime = Arc::new(Self {
            model_type: PhantomData,
            session,
            capabilities,
            in_flight,
            next_turn_id: Arc::new(AtomicU64::new(1)),
            runtime_session_id,
            runtime: runtime_handle,
            runtime_worker_task: Mutex::new(Some(runtime_worker_task)),
            active_turn_mutations,
            shutting_down: AtomicBool::new(false),
            shutdown_timed_out,
            interaction_persistence_failed,
            shutdown_result_tx,
            pending_user_inputs,
            pending_exec_approvals,
            persistence,
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

        if recovered_reconciliation {
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
        }

        Ok(runtime)
    }
}

#[async_trait]
impl<M> RuntimeTurnExecutor for HeadlessDirectTurnExecutor<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    async fn execute(
        &self,
        request: TurnRequest,
        context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        let (assistant_entry_id, start_gate, start_open) = {
            let slot = self.in_flight.lock().await;
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
            )
        };

        if !wait_for_headless_turn_start(&start_gate, &start_open, context.cancellation()).await {
            release_headless_turn(&self.in_flight, &request.turn_id).await;
            return Err(RuntimeWorkerError::Cancelled);
        }

        let mutation_started = context.mutation_started_marker();
        {
            let mut active_turn_mutations = self.active_turn_mutations.lock().await;
            active_turn_mutations.insert(request.turn_id.clone(), mutation_started.clone());
        }

        let mut observer = HeadlessTurnObserver {
            session: self.session.clone(),
            assistant_entry_id: assistant_entry_id.clone(),
            tool_arguments: Arc::new(std::sync::Mutex::new(HashMap::new())),
            start_tasks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            completion_tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
        };
        let prior_history = self.history.lock().await.clone();
        let mut config = (self.config_factory)();
        config.cancellation = Some(ToolLoopCancellation::new(
            context.cancellation(),
            mutation_started,
        ));
        let cancellation = context.cancellation();
        let result = run_tool_loop_with_history_and_observer(
            self.model.as_ref(),
            prior_history,
            request.input,
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
                    {
                        let mut history = self.history.lock().await;
                        *history = turn.history;
                    }
                    finalize_assistant_entry(
                        &self.session,
                        &assistant_entry_id,
                        &turn.final_text,
                        "completed",
                    )
                    .await;
                    self.session.set_status(CodeUiSessionStatus::Idle).await;
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
                        return Err(RuntimeWorkerError::IndeterminateSideEffect(
                            "failed to persist headless web assistant message after a successful mutating turn; session requires reconciliation"
                                .to_string(),
                        ));
                    }
                    Ok(RuntimeTurnExecution::Completed {
                        summary: "headless direct turn completed".to_string(),
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
                    self.session.set_status(CodeUiSessionStatus::Idle).await;
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
                    self.session.set_status(CodeUiSessionStatus::Error).await;
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
        release_headless_turn(&self.in_flight, &request.turn_id).await;
        terminal
    }

    async fn respond(
        &self,
        _request: TurnRequest,
        interaction: crate::internal::ai::runtime::InteractionResponse,
        context: RuntimeExecutionContext,
    ) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
        if context.cancellation().is_cancelled() {
            return Err(RuntimeWorkerError::Cancelled);
        }
        let response = decode_headless_interaction_response(&interaction)?;
        deliver_headless_interaction_response(
            &self.session,
            &self.interaction_persistence_failed,
            &self.pending_user_inputs,
            &self.pending_exec_approvals,
            self.persistence.as_ref(),
            &interaction.interaction_id,
            response,
        )
        .await
    }
}

/// Deliver a validated browser response only from the worker-dispatched
/// executor path. Keeping removal, durable projection, and oneshot delivery
/// together prevents the Web adapter from racing the original tool-loop
/// continuation.
async fn deliver_headless_interaction_response(
    session: &Arc<CodeUiSession>,
    interaction_persistence_failed: &Arc<AtomicBool>,
    pending_user_inputs: &Arc<Mutex<HashMap<String, PendingHeadlessUserInput>>>,
    pending_exec_approvals: &Arc<Mutex<HashMap<String, PendingHeadlessExecApproval>>>,
    persistence: Option<&HeadlessSessionPersistence>,
    interaction_id: &str,
    response: CodeUiInteractionResponse,
) -> Result<RuntimeTurnExecution, RuntimeWorkerError> {
    let exec_response = {
        let pending = pending_exec_approvals.lock().await;
        pending
            .contains_key(interaction_id)
            .then(|| review_decision_from_interaction_response(response.clone()))
            .transpose()
            .map_err(|error| RuntimeWorkerError::ExecutionFailed(error.to_string()))?
    };
    if exec_response.is_some() {
        let pending = {
            let mut pending = pending_exec_approvals.lock().await;
            pending.remove(interaction_id).ok_or_else(|| {
                RuntimeWorkerError::ExecutionFailed(
                    "the pending execution approval closed before the response was delivered"
                        .to_string(),
                )
            })?
        };
        return deliver_headless_exec_approval_response(
            session,
            interaction_persistence_failed,
            persistence,
            interaction_id,
            pending.request,
            response,
        )
        .await;
    }

    {
        let pending = pending_user_inputs.lock().await;
        let pending = pending.get(interaction_id).ok_or_else(|| {
            RuntimeWorkerError::ExecutionFailed(format!(
                "unknown pending interaction: {interaction_id}"
            ))
        })?;
        let _ = user_input_response_from_code_ui_request(&pending.questions, response.clone())
            .map_err(|error| RuntimeWorkerError::ExecutionFailed(error.to_string()))?;
    }
    let pending = {
        let mut pending = pending_user_inputs.lock().await;
        pending.remove(interaction_id).ok_or_else(|| {
            RuntimeWorkerError::ExecutionFailed(
                "the pending user-input request closed before the response was delivered"
                    .to_string(),
            )
        })?
    };
    deliver_headless_user_input_response(
        session,
        interaction_persistence_failed,
        persistence,
        interaction_id,
        pending.questions,
        pending.response_tx,
        response,
    )
    .await
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
    if let Err(error) = persist_headless_interaction_resolution(
        persistence,
        interaction_id,
        review_decision_resolution(decision),
    ) {
        interaction_persistence_failed.store(true, Ordering::Release);
        mark_persistence_failure(
            session,
            "failed to persist resolved exec approval audit event",
            error,
        )
        .await;
        return Err(RuntimeWorkerError::ExecutionFailed(
            "unable to persist the approval audit event; no tool action was started".to_string(),
        ));
    }
    if request.response_tx.send(decision).is_err() {
        session.set_status(CodeUiSessionStatus::Error).await;
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
    if let Err(error) =
        persist_headless_interaction_resolution(persistence, interaction_id, "answered")
    {
        interaction_persistence_failed.store(true, Ordering::Release);
        mark_persistence_failure(
            session,
            "failed to persist resolved user-input audit event",
            error,
        )
        .await;
        return Err(RuntimeWorkerError::ExecutionFailed(
            "unable to persist the user-input audit event; no tool action was started".to_string(),
        ));
    }
    if response_tx.send(user_input_response).is_err() {
        session.set_status(CodeUiSessionStatus::Error).await;
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

fn persist_headless_interaction_resolution(
    persistence: Option<&HeadlessSessionPersistence>,
    interaction_id: &str,
    resolution: &str,
) -> io::Result<()> {
    if let Some(persistence) = persistence {
        persistence.record_interaction_resolution(interaction_id, resolution)?;
    }
    Ok(())
}

fn review_decision_resolution(decision: ReviewDecision) -> &'static str {
    match decision {
        ReviewDecision::Approved => "approved",
        ReviewDecision::ApprovedForSession => "approved_for_session",
        ReviewDecision::ApprovedForTtl => "approved_for_ttl",
        ReviewDecision::ApprovedForDirectoryTtl => "approved_for_directory_ttl",
        ReviewDecision::ApprovedForPatternTtl => "approved_for_pattern_ttl",
        ReviewDecision::ApprovedForAllCommands => "approved_for_all_commands",
        ReviewDecision::Denied => "denied",
        ReviewDecision::Abort => "aborted",
    }
}

fn headless_interaction_id(state: &InteractionState) -> Option<&str> {
    match state {
        InteractionState::AwaitingIntentReview { interaction_id }
        | InteractionState::AwaitingPlanReview { interaction_id }
        | InteractionState::AwaitingNetworkPolicy { interaction_id }
        | InteractionState::AwaitingUserInput { interaction_id }
        | InteractionState::AwaitingToolApproval { interaction_id, .. } => Some(interaction_id),
        InteractionState::Idle
        | InteractionState::Queued
        | InteractionState::Running
        | InteractionState::Cancelling
        | InteractionState::Completed
        | InteractionState::Failed { .. }
        | InteractionState::Cancelled
        | InteractionState::IndeterminateSideEffect { .. } => None,
    }
}

async fn wait_for_headless_turn_start(
    start_gate: &tokio::sync::Notify,
    start_open: &AtomicBool,
    cancellation: CancellationToken,
) -> bool {
    loop {
        if start_open.load(Ordering::Acquire) {
            return true;
        }
        let notified = start_gate.notified();
        if start_open.load(Ordering::Acquire) {
            return true;
        }
        tokio::select! {
            _ = notified => {}
            _ = cancellation.cancelled() => return false,
        }
    }
}

async fn release_headless_turn(in_flight: &Mutex<Option<InFlightTurn>>, runtime_turn_id: &str) {
    let completion = {
        let mut slot = in_flight.lock().await;
        if slot
            .as_ref()
            .is_some_and(|turn| turn.runtime_turn_id == runtime_turn_id)
        {
            slot.take().map(|turn| turn.completion)
        } else {
            None
        }
    };
    if let Some(completion) = completion {
        completion.notify_waiters();
    }
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

#[async_trait]
impl<M> CodeUiCommandAdapter for HeadlessCodeRuntime<M>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
    M::Response: CompletionUsage,
{
    fn capabilities(&self) -> CodeUiCapabilities {
        self.capabilities.clone()
    }

    async fn submit_message(&self, text: String) -> anyhow::Result<()> {
        if text.trim().is_empty() {
            return Err(anyhow!("Empty messages are not accepted by libra code"));
        }
        self.ensure_not_shutting_down()?;
        self.ensure_session_is_recoverable().await?;

        // Hold the in_flight lock continuously across the check + runtime
        // admission + slot assignment. Two concurrent submits cannot both
        // observe an empty slot because the second waiter blocks until the
        // first has installed its worker request.
        let mut slot = self.in_flight.lock().await;
        self.ensure_not_shutting_down()?;
        if slot.is_some() {
            return Err(anyhow!(
                "A turn is already running; cancel it or wait for the assistant to finish before sending another message"
            ));
        }

        let user_entry_id = format!("user-{}", uuid::Uuid::new_v4());
        let assistant_entry_id = format!("assistant-{}", uuid::Uuid::new_v4());
        let now = Utc::now();
        let user_entry = CodeUiTranscriptEntry {
            id: user_entry_id,
            kind: CodeUiTranscriptEntryKind::UserMessage,
            title: None,
            content: Some(text.clone()),
            status: Some("submitted".to_string()),
            streaming: false,
            metadata: serde_json::json!({}),
            created_at: now,
            updated_at: now,
        };
        let assistant_entry = CodeUiTranscriptEntry {
            id: assistant_entry_id.clone(),
            kind: CodeUiTranscriptEntryKind::AssistantMessage,
            title: None,
            content: Some(String::new()),
            status: Some("streaming".to_string()),
            streaming: true,
            metadata: serde_json::json!({}),
            created_at: now,
            updated_at: now,
        };
        // Fresh UUID per submission so resume/restart cannot reuse a prior
        // durable command identity (numeric counters restart at 1).
        let turn_id = self.next_turn_id.fetch_add(1, Ordering::Relaxed);
        let runtime_turn_id = format!("headless-{turn_id}-{}", uuid::Uuid::new_v4());
        let start_gate = Arc::new(tokio::sync::Notify::new());
        let start_open = Arc::new(AtomicBool::new(false));
        let completion = Arc::new(tokio::sync::Notify::new());
        let completion_for_rollback = completion.clone();
        *slot = Some(InFlightTurn {
            runtime_turn_id: runtime_turn_id.clone(),
            assistant_entry_id: assistant_entry_id.clone(),
            start_gate: start_gate.clone(),
            start_open: start_open.clone(),
            completion,
        });

        // Admission occurs before the browser-visible mutation, but the
        // executor cannot pass its gate until the durable projection and live
        // transcript below are both ready. This makes the worker the sole
        // execution owner without weakening the no-untracked-side-effect
        // persistence precondition above.
        if let Err(error) = self
            .runtime
            .submit(TurnRequest::new(
                self.runtime_session_id.clone(),
                runtime_turn_id.clone(),
                text.clone(),
                true,
            ))
            .await
        {
            *slot = None;
            if matches!(error, RuntimeWorkerError::ReconciliationRequired { .. }) {
                return Err(error.into());
            }
            return Err(anyhow!(
                "Unable to admit the browser turn to the AgentRuntime queue; no turn was started: {}",
                runtime_worker_adapter_message(error)
            ));
        }

        // The executor is gated, so release the local slot lock before the
        // durable admission checks. A failure below can then cancel the
        // waiting executor and let it release the slot without any tool call.
        drop(slot);
        // The worker has accepted the turn, but its executor is blocked on
        // `start_gate`. Persist the complete initial projection before opening
        // that gate or exposing the live transcript. This ordering prevents
        // both a durable ghost message on rejected admission and an
        // untracked tool dispatch when SessionStore is unavailable.
        if let Some(persistence) = self.persistence.as_ref() {
            let mut durable_snapshot = self.session.snapshot().await;
            durable_snapshot.transcript.push(user_entry.clone());
            durable_snapshot.transcript.push(assistant_entry.clone());
            durable_snapshot.status = CodeUiSessionStatus::Thinking;
            durable_snapshot.updated_at = now;
            if let Err(error) = persistence
                .record_user_message(durable_snapshot, &text)
                .await
            {
                self.cancel_gated_runtime_turn(&runtime_turn_id, completion_for_rollback.clone())
                    .await?;
                return Err(anyhow!(
                    "Unable to persist the headless web message; no turn was started. Verify session storage and retry: {error}"
                ));
            }
        }
        self.session.upsert_transcript_entry(user_entry).await;
        self.session.upsert_transcript_entry(assistant_entry).await;
        self.session.set_status(CodeUiSessionStatus::Thinking).await;
        start_open.store(true, Ordering::Release);
        start_gate.notify_waiters();
        Ok(())
    }

    async fn respond_interaction(
        &self,
        interaction_id: &str,
        response: CodeUiInteractionResponse,
    ) -> anyhow::Result<()> {
        self.ensure_not_shutting_down()?;
        self.ensure_session_is_recoverable().await?;
        let runtime_turn_id = {
            let slot = self.in_flight.lock().await;
            slot.as_ref().map(|turn| turn.runtime_turn_id.clone())
        };
        if let Some(runtime_turn_id) = runtime_turn_id {
            let response_payload = serde_json::to_string(&response).map_err(|error| {
                anyhow!(
                    "Unable to encode the interaction response for AgentRuntime delivery: {error}"
                )
            })?;
            self.runtime
                .respond(
                    self.runtime_session_id.clone(),
                    runtime_turn_id,
                    crate::internal::ai::runtime::InteractionResponse::new(
                        interaction_id,
                        response_payload,
                    ),
                )
                .await
                .map_err(|error| {
                    anyhow!("Unable to deliver the interaction response to AgentRuntime: {error}")
                })?;
            return Ok(());
        }

        let exec_decision = {
            let pending = self.pending_exec_approvals.lock().await;
            pending
                .contains_key(interaction_id)
                .then(|| review_decision_from_interaction_response(response.clone()))
                .transpose()?
        };
        if let Some(decision) = exec_decision {
            let pending = {
                let mut pending = self.pending_exec_approvals.lock().await;
                pending.remove(interaction_id).ok_or_else(|| {
                    anyhow!(
                        "The pending execution approval closed before the response was delivered"
                    )
                })?
            };
            self.session.resolve_interaction(interaction_id).await;
            self.session
                .set_status(CodeUiSessionStatus::ExecutingTool)
                .await;
            if let Err(error) = self.persist_current_snapshot().await {
                self.interaction_persistence_failed
                    .store(true, Ordering::Release);
                mark_persistence_failure(
                    &self.session,
                    "failed to persist resolved exec approval interaction",
                    error,
                )
                .await;
                return Err(anyhow!(
                    "Unable to persist the approval response; no tool action was started. Verify session storage before retrying"
                ));
            }
            if let Err(error) = self.persist_interaction_resolution(
                interaction_id,
                review_decision_resolution(decision),
            ) {
                self.interaction_persistence_failed
                    .store(true, Ordering::Release);
                mark_persistence_failure(
                    &self.session,
                    "failed to persist resolved exec approval audit event",
                    error,
                )
                .await;
                return Err(anyhow!(
                    "Unable to persist the approval audit event; no tool action was started. Verify session storage before retrying"
                ));
            }
            if pending.request.response_tx.send(decision).is_err() {
                self.session.set_status(CodeUiSessionStatus::Error).await;
                if let Err(error) = self.persist_current_snapshot().await {
                    self.interaction_persistence_failed
                        .store(true, Ordering::Release);
                    mark_persistence_failure(
                        &self.session,
                        "failed to persist closed execution approval request",
                        error,
                    )
                    .await;
                }
                return Err(anyhow!(
                    "The pending execution approval request closed before the response was delivered; no tool action was started"
                ));
            }
            return Ok(());
        }

        let user_input_response = {
            let pending = self.pending_user_inputs.lock().await;
            let pending = pending
                .get(interaction_id)
                .ok_or_else(|| anyhow!("Unknown pending interaction: {interaction_id}"))?;
            user_input_response_from_code_ui_request(&pending.questions, response)?
        };
        let pending = {
            let mut pending = self.pending_user_inputs.lock().await;
            pending.remove(interaction_id).ok_or_else(|| {
                anyhow!("The pending user-input request closed before the response was delivered")
            })?
        };
        self.session.resolve_interaction(interaction_id).await;
        self.session
            .set_status(CodeUiSessionStatus::ExecutingTool)
            .await;
        if let Err(error) = self.persist_current_snapshot().await {
            self.interaction_persistence_failed
                .store(true, Ordering::Release);
            mark_persistence_failure(
                &self.session,
                "failed to persist resolved user input interaction",
                error,
            )
            .await;
            return Err(anyhow!(
                "Unable to persist the user-input response; no tool action was started. Verify session storage before retrying"
            ));
        }
        if let Err(error) = self.persist_interaction_resolution(interaction_id, "answered") {
            self.interaction_persistence_failed
                .store(true, Ordering::Release);
            mark_persistence_failure(
                &self.session,
                "failed to persist resolved user-input audit event",
                error,
            )
            .await;
            return Err(anyhow!(
                "Unable to persist the user-input audit event; no tool action was started. Verify session storage before retrying"
            ));
        }
        if pending.response_tx.send(user_input_response).is_err() {
            self.session.set_status(CodeUiSessionStatus::Error).await;
            if let Err(error) = self.persist_current_snapshot().await {
                self.interaction_persistence_failed
                    .store(true, Ordering::Release);
                mark_persistence_failure(
                    &self.session,
                    "failed to persist closed user-input request",
                    error,
                )
                .await;
            }
            return Err(anyhow!(
                "The pending user-input request closed before the response was delivered; no tool action was started"
            ));
        }
        Ok(())
    }

    async fn cancel_turn(&self) -> anyhow::Result<()> {
        self.ensure_not_shutting_down()?;
        let runtime_turn_id = {
            let slot = self.in_flight.lock().await;
            slot.as_ref().map(|turn| turn.runtime_turn_id.clone())
        };
        let Some(runtime_turn_id) = runtime_turn_id else {
            self.clear_pending_user_inputs().await;
            return Ok(());
        };
        let runtime_interaction_id = self
            .runtime
            .snapshot(self.runtime_session_id.clone())
            .await
            .ok()
            .and_then(|snapshot| {
                headless_interaction_id(&snapshot.interaction).map(ToOwned::to_owned)
            });

        let mutation_in_progress = self
            .active_turn_mutations
            .lock()
            .await
            .get(&runtime_turn_id)
            .is_some_and(|marker| marker.load(Ordering::Acquire));
        match self
            .runtime
            .cancel(self.runtime_session_id.clone(), runtime_turn_id)
            .await
        {
            Ok(()) | Err(RuntimeWorkerError::UnknownTurn { .. }) => {}
            Err(error @ RuntimeWorkerError::ReconciliationRequired { .. }) => {
                return Err(error.into());
            }
            Err(error) => {
                return Err(anyhow!(
                    "Unable to request cancellation from the AgentRuntime: {}",
                    runtime_worker_adapter_message(error)
                ));
            }
        }
        if let Some(interaction_id) = runtime_interaction_id {
            self.session.clear_interaction(&interaction_id).await;
        }
        self.clear_pending_user_inputs().await;
        if mutation_in_progress {
            return Err(anyhow!(
                "A mutating tool is already running; cancellation waits for its determinate result and cannot safely abort it"
            ));
        }
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
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

    /// Cancel an accepted turn whose executor is still blocked on its durable
    /// admission gate. The cancellation token wakes that executor before the
    /// tool loop can run, then the executor releases the local slot.
    async fn cancel_gated_runtime_turn(
        &self,
        runtime_turn_id: &str,
        completion: Arc<tokio::sync::Notify>,
    ) -> anyhow::Result<()> {
        let cancellation_finished = completion.notified();
        match self
            .runtime
            .cancel(self.runtime_session_id.clone(), runtime_turn_id.to_string())
            .await
        {
            Ok(()) => cancellation_finished.await,
            Err(RuntimeWorkerError::UnknownTurn { .. }) => {
                // The worker cannot have crossed the closed gate. If it has
                // already discarded this request, remove the adapter-side
                // reservation as well so a storage repair can be retried.
                release_headless_turn(&self.in_flight, runtime_turn_id).await;
            }
            Err(error @ RuntimeWorkerError::ReconciliationRequired { .. }) => {
                return Err(error.into());
            }
            Err(error) => {
                return Err(anyhow!(
                    "The gated AgentRuntime turn could not be cancelled; no tool gate was opened. Verify runtime/session storage before retrying: {}",
                    runtime_worker_adapter_message(error)
                ));
            }
        }
        Ok(())
    }

    async fn shutdown_once(&self) -> anyhow::Result<()> {
        self.clear_pending_user_inputs().await;
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
        if let Err(error) = self.persist_current_snapshot().await {
            self.interaction_persistence_failed
                .store(true, Ordering::Release);
            mark_persistence_failure(
                &self.session,
                "failed to persist pending user input interaction",
                error,
            )
            .await;
            self.clear_pending_user_inputs().await;
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

        self.pending_user_inputs.lock().await.insert(
            interaction_id,
            PendingHeadlessUserInput {
                questions: request.questions,
                response_tx: request.response_tx,
            },
        );
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
        if let Err(error) = self.persist_current_snapshot().await {
            self.interaction_persistence_failed
                .store(true, Ordering::Release);
            mark_persistence_failure(
                &self.session,
                "failed to persist pending exec approval interaction",
                error,
            )
            .await;
            self.clear_pending_user_inputs().await;
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

        self.pending_exec_approvals
            .lock()
            .await
            .insert(interaction_id, PendingHeadlessExecApproval { request });
    }

    async fn active_runtime_turn_id(&self) -> Option<String> {
        let slot = self.in_flight.lock().await;
        slot.as_ref().map(|turn| turn.runtime_turn_id.clone())
    }

    /// Transfer a live tool-loop continuation into the serialized worker.
    /// Standalone adapter calls have no active runtime turn and deliberately
    /// use the narrow compatibility map above instead.
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
            self.clear_pending_user_inputs().await;
            self.session.clear_interaction(interaction_id).await;
            self.session.set_status(CodeUiSessionStatus::Error).await;
        }
    }

    async fn clear_pending_user_inputs(&self) {
        let pending_ids = {
            let mut pending = self.pending_user_inputs.lock().await;
            let ids = pending.keys().cloned().collect::<Vec<_>>();
            pending.clear();
            ids
        };

        for interaction_id in pending_ids {
            self.session.clear_interaction(&interaction_id).await;
        }

        let pending_ids = {
            let mut pending = self.pending_exec_approvals.lock().await;
            let ids = pending.keys().cloned().collect::<Vec<_>>();
            pending.clear();
            ids
        };

        for interaction_id in pending_ids {
            self.session.clear_interaction(&interaction_id).await;
        }
    }

    async fn ensure_session_is_recoverable(&self) -> anyhow::Result<()> {
        if self.session.snapshot().await.status == CodeUiSessionStatus::IndeterminateSideEffect {
            return Err(CodeUiApiError::reconciliation_required(
                "this headless web session has an indeterminate persistence state; restart and inspect its durable session data before sending another request",
            )
            .into());
        }
        Ok(())
    }

    fn ensure_not_shutting_down(&self) -> anyhow::Result<()> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(anyhow!(
                "This headless web runtime is shutting down and cannot accept new commands"
            ));
        }
        Ok(())
    }

    async fn persist_current_snapshot(&self) -> io::Result<()> {
        if let Some(persistence) = self.persistence.as_ref() {
            persistence
                .persist_snapshot(self.session.snapshot().await)
                .await?;
        }
        Ok(())
    }

    fn persist_interaction_resolution(
        &self,
        interaction_id: &str,
        resolution: &str,
    ) -> io::Result<()> {
        persist_headless_interaction_resolution(
            self.persistence.as_ref(),
            interaction_id,
            resolution,
        )
    }
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
    let has_options = question
        .options
        .as_ref()
        .is_some_and(|options| !options.is_empty());

    let options = question
        .options
        .as_ref()
        .map(|options| {
            options
                .iter()
                .map(|option| serde_json::json!({ "id": option.label, "label": option.label }))
                .collect::<Vec<_>>()
        })
        .filter(|options| !options.is_empty())
        .unwrap_or_default();

    let metadata = serde_json::json!({
        "id": question.id,
        "prompt": question.question,
        "kind": if has_options { "single" } else { "text" },
        "options": options,
    });

    metadata
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
}

impl HeadlessTurnObserver {
    /// Wait for all projection tasks belonging to the current turn. Callback
    /// invocation is single-threaded inside the tool loop, so by the time the
    /// loop returns no new handles can be added; the loop only handles the
    /// handoff where an end task has taken a start task between the two drains.
    async fn flush_projection_tasks(&self) {
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
            let session = self.session.clone();
            let entry_id = self.assistant_entry_id.clone();
            let delta = delta.clone();
            tokio::spawn(async move {
                session.append_assistant_delta(&entry_id, &delta).await;
            });
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
        if let Ok(mut arguments_by_call) = self.tool_arguments.lock() {
            arguments_by_call.insert(call_id.to_string(), arguments.clone());
        }

        let session = self.session.clone();
        let call_id = call_id.to_string();
        let start_key = call_id.clone();
        let tool_name = tool_name.to_string();
        let arguments = arguments.clone();
        let handle = tokio::spawn(async move {
            let summary = headless_tool_call_summary(&tool_name, &arguments);
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
                && let Some(plan) =
                    plan_snapshot_from_update_plan_arguments(&call_id, "running", &arguments)
            {
                session.upsert_plan(plan).await;
            }
            if tool_name == "submit_plan_draft"
                && let Some(plan) =
                    plan_snapshot_from_submit_plan_draft_arguments(&call_id, "running", &arguments)
            {
                session.upsert_plan(plan).await;
            }
            session.set_status(CodeUiSessionStatus::ExecutingTool).await;
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
}
