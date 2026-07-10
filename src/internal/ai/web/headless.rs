//! Headless web-only runtime for non-Codex providers.
//!
//! `--web-only --provider <X>` (X != codex) used to fall back to a read-only
//! placeholder snapshot, leaving the browser unable to drive the agent. This
//! module provides the minimum-viable replacement: a [`HeadlessCodeRuntime`]
//! that owns a [`CodeUiSession`], spawns a tokio task per submitted message
//! that runs the agent's tool loop, and streams the model's output back into
//! the session transcript.
//!
//! # v0 scope (Phase 3 minimum)
//!
//! - `submitMessage` queues a user message and starts a turn — the agent runs
//!   the standard `run_tool_loop_with_history_and_observer` and the assistant
//!   reply lands in the live snapshot, streamed delta-by-delta.
//! - `cancelTurn` aborts the in-flight turn and marks the assistant entry as
//!   cancelled.
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
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::anyhow;
use async_trait::async_trait;
use chrono::Utc;
use tokio::{
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
};

use super::code_ui::{
    CodeUiApplyToFuture, CodeUiCapabilities, CodeUiCommandAdapter, CodeUiEventType,
    CodeUiInteractionKind, CodeUiInteractionOption, CodeUiInteractionRequest,
    CodeUiInteractionResponse, CodeUiInteractionStatus, CodeUiPatchChange, CodeUiPatchsetSnapshot,
    CodeUiPlanSnapshot, CodeUiPlanStep, CodeUiReadModel, CodeUiSession, CodeUiSessionSnapshot,
    CodeUiSessionStatus, CodeUiToolCallSnapshot, CodeUiTranscriptEntry, CodeUiTranscriptEntryKind,
};
use crate::internal::ai::{
    agent::runtime::run_tool_loop_with_history_and_observer,
    completion::{
        CompletionError, CompletionModel, CompletionStreamEvent, CompletionUsage,
        CompletionUsageSummary, Message,
    },
    sandbox::{ExecApprovalRequest, NetworkAccess, ReviewDecision},
    session::{SessionState, SessionStore},
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

#[derive(Clone)]
pub struct HeadlessSessionPersistence {
    store: Arc<SessionStore>,
    state: Arc<Mutex<SessionState>>,
}

impl HeadlessSessionPersistence {
    pub fn new(store: Arc<SessionStore>, state: SessionState) -> Self {
        Self {
            store,
            state: Arc::new(Mutex::new(state)),
        }
    }

    async fn record_user_message(
        &self,
        snapshot: CodeUiSessionSnapshot,
        content: &str,
    ) -> io::Result<()> {
        let mut state = self.state.lock().await;
        state.add_user_message(content);
        sync_session_metadata_from_snapshot(&mut state, snapshot);
        self.store.save(&state)
    }

    async fn record_assistant_message(
        &self,
        snapshot: CodeUiSessionSnapshot,
        content: &str,
    ) -> io::Result<()> {
        let mut state = self.state.lock().await;
        state.add_assistant_message(content);
        sync_session_metadata_from_snapshot(&mut state, snapshot);
        self.store.save(&state)
    }

    async fn persist_snapshot(&self, snapshot: CodeUiSessionSnapshot) -> io::Result<()> {
        let mut state = self.state.lock().await;
        sync_session_metadata_from_snapshot(&mut state, snapshot);
        self.store.save(&state)
    }
}

struct PendingHeadlessUserInput {
    questions: Vec<UserInputQuestion>,
    response_tx: oneshot::Sender<UserInputResponse>,
}

struct PendingHeadlessExecApproval {
    request: ExecApprovalRequest,
}

/// Adapter that runs an agent tool loop in response to browser-driven messages.
///
/// Generic over a [`CompletionModel`] so each provider (Ollama, OpenAI, Gemini,
/// …) can plug in its own client. The model is held inside an `Arc<Mutex<…>>`
/// so the spawned turn task can take exclusive access while the next submit
/// waits in the queue.
/// Bookkeeping for the active turn so the runtime can finalize its
/// transcript entry on cancel and so the spawned task can avoid clobbering
/// a successor turn's slot when it eventually clears itself out.
struct InFlightTurn {
    /// Stable id assigned per-turn; the spawned task uses it as a generation
    /// counter when releasing its slot at the end of the turn.
    id: u64,
    /// Transcript entry that needs `streaming -> false` + `status` finalized
    /// when the turn ends (success, error, or cancellation).
    assistant_entry_id: String,
    handle: JoinHandle<()>,
}

pub struct HeadlessCodeRuntime<M: CompletionModel + 'static> {
    session: Arc<CodeUiSession>,
    capabilities: CodeUiCapabilities,
    /// Conversation history accumulated across turns.
    history: Arc<Mutex<Vec<Message>>>,
    model: Arc<M>,
    registry: Arc<ToolRegistry>,
    config_factory:
        Arc<dyn Fn() -> super::super::agent::runtime::tool_loop::ToolLoopConfig + Send + Sync>,
    /// Active turn slot. `submit_message` holds the lock while it spawns and
    /// stores the new turn so two concurrent submits can never both see an
    /// empty slot. `cancel_turn` and the spawned task itself acquire the
    /// lock to release / finalize the slot.
    in_flight: Arc<Mutex<Option<InFlightTurn>>>,
    /// Monotonic turn id; used by spawned tasks to detect that a successor
    /// turn has claimed the slot before they cleared their own entry.
    next_turn_id: Arc<AtomicU64>,
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
    pub fn new(
        session: Arc<CodeUiSession>,
        capabilities: CodeUiCapabilities,
        model: M,
        registry: Arc<ToolRegistry>,
        user_input_rx: mpsc::UnboundedReceiver<UserInputRequest>,
        exec_approval_rx: mpsc::UnboundedReceiver<ExecApprovalRequest>,
        config_factory: Arc<
            dyn Fn() -> super::super::agent::runtime::tool_loop::ToolLoopConfig + Send + Sync,
        >,
    ) -> Arc<Self> {
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
    }

    /// Build a headless runtime with restored model history and optional
    /// SessionStore persistence.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_persistence(
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
    ) -> Arc<Self> {
        let runtime = Arc::new(Self {
            session,
            capabilities,
            history: Arc::new(Mutex::new(initial_history)),
            model: Arc::new(model),
            registry,
            config_factory,
            in_flight: Arc::new(Mutex::new(None)),
            next_turn_id: Arc::new(AtomicU64::new(1)),
            pending_user_inputs: Arc::new(Mutex::new(HashMap::new())),
            pending_exec_approvals: Arc::new(Mutex::new(HashMap::new())),
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

        runtime
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

        // Hold the in_flight lock continuously across the check + spawn + slot
        // assignment. Two concurrent submits cannot both observe an empty slot
        // because the second waiter blocks on `lock().await` until the first
        // finishes installing its task.
        let mut slot = self.in_flight.lock().await;
        if slot.as_ref().is_some_and(|turn| !turn.handle.is_finished()) {
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
        self.session.upsert_transcript_entry(user_entry).await;
        self.session.upsert_transcript_entry(assistant_entry).await;
        self.session.set_status(CodeUiSessionStatus::Thinking).await;
        if let Some(persistence) = self.persistence.as_ref() {
            persist_or_warn(
                persistence
                    .record_user_message(self.session.snapshot().await, &text)
                    .await,
                "failed to persist headless web user message",
            );
        }

        let session = self.session.clone();
        let history = self.history.clone();
        let model = self.model.clone();
        let registry = self.registry.clone();
        let config = (self.config_factory)();
        let persistence = self.persistence.clone();
        let in_flight_for_task = self.in_flight.clone();
        let user_text = text;
        let task_assistant_entry_id = assistant_entry_id.clone();
        let turn_id = self.next_turn_id.fetch_add(1, Ordering::Relaxed);

        let task = tokio::spawn(async move {
            let mut observer = HeadlessTurnObserver {
                session: session.clone(),
                assistant_entry_id: task_assistant_entry_id.clone(),
                tool_arguments: Arc::new(std::sync::Mutex::new(HashMap::new())),
                start_tasks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            };

            let prior_history = {
                let guard = history.lock().await;
                guard.clone()
            };

            let result = run_tool_loop_with_history_and_observer(
                model.as_ref(),
                prior_history,
                user_text,
                registry.as_ref(),
                config,
                &mut observer,
            )
            .await;

            match result {
                Ok(turn) => {
                    {
                        let mut guard = history.lock().await;
                        *guard = turn.history;
                    }
                    finalize_assistant_entry(
                        &session,
                        &task_assistant_entry_id,
                        &turn.final_text,
                        "completed",
                    )
                    .await;
                    session.set_status(CodeUiSessionStatus::Idle).await;
                    if let Some(persistence) = persistence.as_ref() {
                        persist_or_warn(
                            persistence
                                .record_assistant_message(
                                    session.snapshot().await,
                                    turn.final_text.as_str(),
                                )
                                .await,
                            "failed to persist headless web assistant message",
                        );
                    }
                }
                Err(error) => {
                    let message = format_completion_error(&error);
                    finalize_assistant_entry(&session, &task_assistant_entry_id, &message, "error")
                        .await;
                    session.set_status(CodeUiSessionStatus::Error).await;
                    if let Some(persistence) = persistence.as_ref() {
                        persist_or_warn(
                            persistence.persist_snapshot(session.snapshot().await).await,
                            "failed to persist headless web failed turn snapshot",
                        );
                    }
                }
            }

            // Only clear the slot if it still holds *our* turn — a successor
            // submit may have already claimed the slot via cancel + resubmit
            // and we would otherwise wipe its handle out from under it.
            let mut slot = in_flight_for_task.lock().await;
            if slot.as_ref().is_some_and(|t| t.id == turn_id) {
                *slot = None;
            }
        });

        *slot = Some(InFlightTurn {
            id: turn_id,
            assistant_entry_id,
            handle: task,
        });
        Ok(())
    }

    async fn respond_interaction(
        &self,
        interaction_id: &str,
        response: CodeUiInteractionResponse,
    ) -> anyhow::Result<()> {
        if let Some(pending) = {
            let mut pending = self.pending_exec_approvals.lock().await;
            pending.remove(interaction_id)
        } {
            let decision = review_decision_from_interaction_response(response)?;
            pending.request.response_tx.send(decision).map_err(|_| {
                anyhow!("The pending execution approval request is no longer awaiting a response")
            })?;

            self.session.resolve_interaction(interaction_id).await;
            self.session
                .set_status(CodeUiSessionStatus::ExecutingTool)
                .await;
            self.persist_current_snapshot("failed to persist resolved exec approval interaction")
                .await;
            return Ok(());
        }

        let pending = {
            let mut pending = self.pending_user_inputs.lock().await;
            pending
                .remove(interaction_id)
                .ok_or_else(|| anyhow!("Unknown pending interaction: {interaction_id}"))?
        };

        let user_input_response =
            user_input_response_from_code_ui_request(&pending.questions, response)?;
        pending.response_tx.send(user_input_response).map_err(|_| {
            anyhow!("The pending user input request is no longer awaiting a response")
        })?;

        self.session.resolve_interaction(interaction_id).await;
        self.session
            .set_status(CodeUiSessionStatus::ExecutingTool)
            .await;
        self.persist_current_snapshot("failed to persist resolved user input interaction")
            .await;
        Ok(())
    }

    async fn cancel_turn(&self) -> anyhow::Result<()> {
        let active = {
            let mut slot = self.in_flight.lock().await;
            slot.take()
        };
        if let Some(turn) = active {
            if !turn.handle.is_finished() {
                turn.handle.abort();
            }
            // Finalize the streaming assistant entry so the browser sees a
            // terminal state instead of a perpetually streaming row.
            finalize_assistant_entry(
                &self.session,
                &turn.assistant_entry_id,
                "(turn cancelled by user)",
                "cancelled",
            )
            .await;
        }
        self.session.set_status(CodeUiSessionStatus::Idle).await;
        self.clear_pending_user_inputs().await;
        self.persist_current_snapshot("failed to persist cancelled headless web turn")
            .await;
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        let active = {
            let mut slot = self.in_flight.lock().await;
            slot.take()
        };
        if let Some(turn) = active {
            turn.handle.abort();
            finalize_assistant_entry(
                &self.session,
                &turn.assistant_entry_id,
                "(libra code shutting down)",
                "cancelled",
            )
            .await;
        }
        self.clear_pending_user_inputs().await;
        self.persist_current_snapshot("failed to persist headless web shutdown snapshot")
            .await;
        Ok(())
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

        {
            let mut pending = self.pending_user_inputs.lock().await;
            pending.insert(
                interaction_id.clone(),
                PendingHeadlessUserInput {
                    questions: request.questions,
                    response_tx: request.response_tx,
                },
            );
        }

        let interaction = CodeUiInteractionRequest {
            id: interaction_id,
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
        self.persist_current_snapshot("failed to persist pending user input interaction")
            .await;
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

        {
            let mut pending = self.pending_exec_approvals.lock().await;
            pending.insert(
                interaction_id.clone(),
                PendingHeadlessExecApproval { request },
            );
        }

        self.session.upsert_interaction(interaction).await;
        self.session
            .set_status(CodeUiSessionStatus::AwaitingInteraction)
            .await;
        self.persist_current_snapshot("failed to persist pending exec approval interaction")
            .await;
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

    async fn persist_current_snapshot(&self, warning: &'static str) {
        if let Some(persistence) = self.persistence.as_ref() {
            persist_or_warn(
                persistence
                    .persist_snapshot(self.session.snapshot().await)
                    .await,
                warning,
            );
        }
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

fn persist_or_warn(result: io::Result<()>, message: &'static str) {
    if let Err(error) = result {
        tracing::warn!(error = %error, "{message}");
    }
}

fn sync_session_metadata_from_snapshot(
    state: &mut SessionState,
    mut snapshot: CodeUiSessionSnapshot,
) {
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
        serde_json::to_value(snapshot).unwrap_or_else(|_| serde_json::json!({})),
    );
    state.updated_at = Utc::now();
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
    if let Some((question_id, answers)) = response
        .answers
        .into_iter()
        .find(|(_, answers)| !answers.is_empty())
    {
        return Ok(UserInputResponse {
            answers: [(question_id, UserInputAnswer { answers })]
                .into_iter()
                .collect::<HashMap<_, _>>(),
        });
    }

    let question = questions
        .first()
        .ok_or_else(|| anyhow!("User input request contains no questions"))?;

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
        tokio::spawn(async move {
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
