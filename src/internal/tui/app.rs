//! Main application structure and event loop.
//!
//! The `App` struct manages the TUI state and coordinates between
//! user input, agent execution, and UI rendering.

use std::{
    collections::{HashMap, HashSet},
    fs,
    future::Future,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use chrono::Utc;
use crossterm::event::{KeyCode, KeyModifiers};
use git_internal::internal::object::context::SelectionStrategy;
use ring::digest;
use serde::Deserialize;
use tokio::{
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
    time::{interval, sleep, timeout},
};
use tokio_stream::StreamExt;

use super::{
    app_event::{
        AgentEvent, AgentStatus, AppEvent, ProviderPlanDraft, ProviderPlanDraftStep, TurnId,
        TurnInputSource,
    },
    chatwidget::ChatWidget,
    control::{CancelSource, TuiControlCommand, TuiControlError},
    diff::FileChange,
    history_cell::{
        AssistantHistoryCell, DiffHistoryCell, HistoryCell, OrchestratorResultHistoryCell,
        PlanSummaryHistoryCell, PlanUpdateHistoryCell, ThinkingHistoryCell, ToolCallHistoryCell,
        UserHistoryCell,
    },
    terminal::{TARGET_FRAME_INTERVAL, Tui, TuiEvent},
    welcome_shader::{self, WelcomeView},
};
#[cfg(unix)]
use crate::utils::fuse as fuse_utils;
use crate::{
    cli_error,
    internal::ai::{
        agent::{
            BudgetTracker, TaskIntent, TaskIntentClassificationRequest, TaskIntentClassifier,
            TaskIntentClassifierError, TaskIntentDecision, ToolLoopConfig, ToolLoopObserver,
            format_agents_table, format_budget_status,
            profile::{AgentProfileRouter, AgentsConfig},
            run_tool_loop_with_history_and_observer,
            runtime::{TaskEntryKind, TaskInvocation, TaskResult as SubAgentTaskResult},
        },
        commands::CommandDispatcher,
        completion::{
            CompletionModel, CompletionRetryEvent, CompletionRetryObserver, CompletionRetryPolicy,
            CompletionStreamEvent, CompletionUsage, CompletionUsageSummary, Message,
            RetryingCompletionModel,
        },
        context_budget::{
            MemoryAnchor, MemoryAnchorDraft, MemoryAnchorEvent, MemoryAnchorLookupError,
            build_memory_anchor_prompt_section,
        },
        goal::{
            DefaultGoalContinuationPromptBuilder, DeterministicGoalVerifier, GoalActor,
            GoalEventClock, GoalEventEnvelope, GoalLoopDecision, GoalStopPolicy,
            GoalSupervisedToolLoopRequest, GoalSupervisor, GoalVerifierContext, RecentToolCall,
            ToolResultStatus, run_goal_supervised_tool_loop,
        },
        intentspec::{
            IntentDraft, IntentSpec, ResolveContext, RiskLevel, build_intentspec_review,
            render_summary, repair_intentspec, resolve_intentspec,
            types::{
                ConflictResolution, DecompositionMode, LibraBinding, NetworkPolicy, Objective,
                ObjectiveKind, PlanGenerationConfig,
            },
            validate_intentspec,
        },
        mcp::{
            resource::{
                CreateContextFrameParams, CreateContextSnapshotParams, CreateDecisionParams,
                CreateRunParams, CreateTaskParams, CreateToolInvocationParams,
            },
            server::LibraMcpServer,
        },
        orchestrator::{
            planner::compile_execution_plan_spec,
            types::{
                DecisionOutcome, ExecutionPlanSpec, GateReport, OrchestratorPhaseConfirmer,
                OrchestratorResult, PersistedPlanReviewBundle, PhaseConfirmationDecision,
                PhaseConfirmationPrompt, PolicyViolation, SystemReport, TaskKind, TaskNodeStatus,
                TaskRuntimeEvent, TaskRuntimeNoteLevel, TaskRuntimePhase, TaskWorkspaceBackend,
            },
        },
        projection::ProjectionRebuilder,
        prompt::SystemPromptBuilder,
        runtime::phase0::{
            ContextSnapshotItem, ContextSnapshotRequest, write_context_snapshot_if_needed,
            write_intent,
        },
        sandbox::{
            ApprovalMemo, ExecApprovalRequest, FileHistoryRuntimeContext, NetworkAccess,
            ReviewDecision,
        },
        session::{
            SessionState, SessionStore,
            file_history::FileHistoryError,
            jsonl::{SessionEvent, SessionJsonlStore},
        },
        skills::SkillDispatcher,
        sources::{BUILTIN_MCP_SOURCE_SLUG, McpSource, SourceEnablement, SourcePool, SourceStatus},
        tools::{
            ToolOutput, ToolRegistry,
            context::{
                RequestUserInputArgs, SubmitPlanDraftArgs, UpdatePlanArgs, UserInputAnswer,
                UserInputRequest, UserInputResponse,
            },
            handlers::submit_intent_draft::parse_submit_intent_draft_value,
        },
        usage::{
            UsageDisplaySnapshot, UsageGrouping, UsageQueryFilter, format_usage_badge,
            format_usage_detail_panel,
        },
        web::code_ui::{
            CodeUiApplyToFuture, CodeUiEventEnvelope, CodeUiInteractionKind,
            CodeUiInteractionOption, CodeUiInteractionRequest, CodeUiInteractionResponse,
            CodeUiInteractionStatus, CodeUiPatchChange, CodeUiPatchsetSnapshot, CodeUiPlanSnapshot,
            CodeUiPlanStep, CodeUiRuntimeHandle, CodeUiSession, CodeUiSessionSnapshot,
            CodeUiSessionStatus, CodeUiToolCallSnapshot, CodeUiTranscriptEntry,
            CodeUiTranscriptEntryKind, snapshot_from_event,
        },
    },
    utils::client_storage::ClientStorage,
};

#[derive(Debug, Clone, Default)]
struct McpWriteTracker {
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl McpWriteTracker {
    fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let handle = ClientStorage::spawn_background_index_work(async move {
            if timeout(MCP_WRITE_TIMEOUT, fut).await.is_err() {
                tracing::warn!("MCP background write timed out");
            }
        });
        match self.tasks.lock() {
            Ok(mut tasks) => {
                tasks.retain(|task| !task.is_finished());
                tasks.push(handle);
            }
            Err(_) => handle.abort(),
        }
    }

    async fn drain(&self) {
        loop {
            let pending = match self.tasks.lock() {
                Ok(mut tasks) => std::mem::take(&mut *tasks),
                Err(_) => return,
            };
            if pending.is_empty() {
                return;
            }
            for handle in pending {
                let _ = handle.await;
            }
        }
    }
}

const LATEST_INTENTSPEC_INTENT_ID: &str = "latest_intentspec_intent_id";
const LATEST_EXECUTION_PLAN_ID: &str = "latest_execution_plan_id";
const LATEST_INTENTSPEC_JSON: &str = "latest_intentspec_json";
const LATEST_INTENTSPEC_WORKSPACE_KEY: &str = "latest_intentspec_workspace_key";
const LATEST_INTENTSPEC_BASE_REF: &str = "latest_intentspec_base_ref";
const LATEST_INTENTSPEC_BRANCH_LABEL: &str = "latest_intentspec_branch_label";
const MAX_INTENTSPEC_REPAIR_ATTEMPTS: usize = 2;
const MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS: u8 = 10;
const DEFAULT_AUTOMATIC_PLAN_REPAIR_ATTEMPTS: u8 = 0;
const MCP_WRITE_TIMEOUT: Duration = Duration::from_secs(8);
const MCP_TURN_TRACKING_TIMEOUT: Duration = Duration::from_secs(3);
const GRAPH_THREAD_ID_METADATA_KEYS: &[&str] = &["thread_id", "threadId", "canonical_thread_id"];

fn session_graph_thread_id(session: &SessionState) -> Option<String> {
    GRAPH_THREAD_ID_METADATA_KEYS
        .iter()
        .find_map(|key| session.metadata.get(*key).and_then(|value| value.as_str()))
        .filter(|thread_id| uuid::Uuid::parse_str(thread_id).is_ok())
        .map(str::to_string)
        .or_else(|| {
            session
                .metadata
                .get(LATEST_INTENTSPEC_INTENT_ID)
                .and_then(|value| value.as_str())
                .filter(|intent_id| uuid::Uuid::parse_str(intent_id).is_ok())
                .map(str::to_string)
        })
        .or_else(|| {
            uuid::Uuid::parse_str(&session.id)
                .ok()
                .map(|thread_id| thread_id.to_string())
        })
}

fn graph_thread_id_from_orchestrator_result(result: &OrchestratorResult) -> Option<String> {
    result
        .persistence
        .as_ref()
        .and_then(|persistence| persistence.thread_id.as_deref())
        .filter(|thread_id| uuid::Uuid::parse_str(thread_id).is_ok())
        .map(str::to_string)
}

fn graph_command_from_thread_id(thread_id: &str) -> Option<String> {
    uuid::Uuid::parse_str(thread_id)
        .ok()
        .map(|_| format!("libra graph {thread_id}"))
}

fn record_orchestrator_thread_metadata(session: &mut SessionState, result: &OrchestratorResult) {
    if let Some(thread_id) = graph_thread_id_from_orchestrator_result(result) {
        session.metadata.insert(
            "thread_id".to_string(),
            serde_json::Value::String(thread_id),
        );
    }
}

fn summarize_mcp_content(content: &[rmcp::model::Content]) -> Option<String> {
    let mut parts = Vec::new();
    for item in content {
        if let Some(text) = item
            .as_text()
            .map(|text| text.text.trim())
            .filter(|text| !text.is_empty())
        {
            parts.push(text.to_string());
        }
    }

    if !parts.is_empty() {
        return Some(parts.join(" | "));
    }

    serde_json::to_string(content)
        .ok()
        .filter(|text| !text.trim().is_empty())
}

fn render_mcp_error(context: &str, content: Vec<rmcp::model::Content>) {
    if let Some(content) = summarize_mcp_content(&content) {
        cli_error!("error" => format!("{context}: {content}"));
    } else {
        cli_error!("error" => context);
    }
}

/// The reason for exiting the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitReason {
    /// User requested exit.
    UserRequested,
    /// Fatal error occurred.
    Fatal(String),
}

/// Information about the app exit state.
#[derive(Debug, Clone)]
pub struct AppExitInfo {
    /// The reason for exiting.
    pub reason: ExitReason,
    /// Canonical thread id that can be inspected with `libra graph`, when known.
    pub thread_id: Option<String>,
}

/// Pending user-input state while the TUI waits for the user to answer.
struct PendingUserInput {
    /// The original request (questions, etc.).
    request: UserInputRequest,
    /// Index of the question currently being answered.
    current_question: usize,
    /// Answers collected so far, keyed by question id.
    answers: HashMap<String, UserInputAnswer>,
    /// Currently selected option index (0-based) for the active question.
    selected_option: usize,
    /// Whether the notes input is currently focused (Tab toggles).
    notes_focused: bool,
    /// Notes text being composed for the current question.
    notes_text: String,
}

/// Post-plan dialog state: stores the spec and user selection.
struct PendingPostPlan {
    spec_json: String,
    intent_id: Option<String>,
    plan_id: Option<String>,
    persisted_plan_bundle: Option<PersistedPlanReviewBundle>,
    plan: ExecutionPlanSpec,
    plan_draft: ProviderPlanDraft,
    warnings: Vec<String>,
    network_access: bool,
    automatic_repair_attempts: u8,
    automatic_repair_max_attempts: u8,
    selected: usize, // 0=Execute, 1=Modify, 2=Cancel
}

/// Network policy dialog state after the user approves the execution plan.
struct PendingNetworkPolicyChoice {
    post_plan: PendingPostPlan,
    selected: usize, // 0=Deny, 1=Allow, 2=Back
}

/// Execution-plan revision state after the user chooses Modify on the plan review.
struct PendingExecutionPlanRevision {
    spec_json: String,
    intent_id: Option<String>,
    current_plan: ProviderPlanDraft,
    warnings: Vec<String>,
    automatic_repair_attempts: u8,
    automatic_repair_max_attempts: u8,
    network_access: bool,
    failure_report: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExecutionFailureRevision {
    PlanRevision,
    IntentSpecRevision,
    ManualAction,
}

#[derive(Clone, Debug)]
struct PendingAutoPlanRepairExecution {
    attempt: u8,
    max_attempts: u8,
    network_access: bool,
}

struct ExecuteWorkflowRequest {
    spec_json: String,
    persisted_intent_id: Option<String>,
    persisted_plan_id: Option<String>,
    persisted_plan_bundle: Option<PersistedPlanReviewBundle>,
    approved_plan: Option<ExecutionPlanSpec>,
    approved_plan_draft: Option<ProviderPlanDraft>,
    plan_warnings: Vec<String>,
    network_access_override: Option<bool>,
    automatic_repair_attempts: u8,
    automatic_repair_max_attempts: u8,
}

/// IntentSpec review dialog state: stores the spec and user selection.
struct PendingIntentReview {
    spec_json: String,
    intent_id: Option<String>,
    warnings: Vec<String>,
    interaction_id: String,
    selected: usize, // 0=Confirm, 1=Modify, 2=Cancel
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReviewScrollAction {
    Top,
    Bottom,
    Up(usize),
    Down(usize),
}

fn review_scroll_action(key: crossterm::event::KeyEvent) -> Option<ReviewScrollAction> {
    let has_scroll_modifier = key.modifiers.contains(KeyModifiers::CONTROL)
        || key.modifiers.contains(KeyModifiers::ALT)
        || key.modifiers.contains(KeyModifiers::SHIFT);

    match key.code {
        KeyCode::Home => Some(ReviewScrollAction::Top),
        KeyCode::End => Some(ReviewScrollAction::Bottom),
        KeyCode::PageUp => Some(ReviewScrollAction::Up(10)),
        KeyCode::PageDown => Some(ReviewScrollAction::Down(10)),
        KeyCode::Up if has_scroll_modifier => Some(ReviewScrollAction::Up(1)),
        KeyCode::Down if has_scroll_modifier => Some(ReviewScrollAction::Down(1)),
        KeyCode::Char('j') if key.modifiers.is_empty() => Some(ReviewScrollAction::Up(1)),
        KeyCode::Char('l') if key.modifiers.is_empty() => Some(ReviewScrollAction::Down(1)),
        _ => None,
    }
}

/// Pending sandbox approval state.
struct PendingExecApproval {
    request: ExecApprovalRequest,
    selected: usize,
    allow_all_confirmation_requested: bool,
}

/// Pending managed-provider interaction mirrored into the approval dialog.
struct PendingManagedInteraction {
    interaction: CodeUiInteractionRequest,
    selected: usize,
}

/// Pending orchestrator Phase 3 / Phase 4 confirmation.
struct PendingPhaseConfirmation {
    prompt: PhaseConfirmationPrompt,
    response_tx: tokio::sync::oneshot::Sender<PhaseConfirmationDecision>,
    interaction_id: String,
    selected: usize,
}

/// Configuration for creating an App.
pub struct AppConfig {
    pub welcome_message: String,
    pub command_dispatcher: CommandDispatcher,
    pub skill_dispatcher: SkillDispatcher,
    pub agent_router: AgentProfileRouter,
    pub agents_config: AgentsConfig,
    pub session: SessionState,
    pub session_store: SessionStore,
    pub user_input_rx: UnboundedReceiver<UserInputRequest>,
    pub exec_approval_rx: UnboundedReceiver<ExecApprovalRequest>,
    /// Display name of the active model (e.g. "gemini-2.5-flash").
    pub model_name: String,
    /// Provider identifier (e.g. "gemini", "anthropic").
    pub provider_name: String,
    /// MCP server instance for workflow tracking.
    pub mcp_server: Option<Arc<LibraMcpServer>>,
    /// Optional Code UI session mirror for the browser UI.
    pub code_ui_session: Option<Arc<CodeUiSession>>,
    /// Optional Code UI runtime handle for local controller operations.
    pub code_ui_runtime: Option<Arc<CodeUiRuntimeHandle>>,
    /// Optional local automation command receiver.
    pub code_control_rx: Option<UnboundedReceiver<TuiControlCommand>>,
    /// Optional managed provider runtime controlled through the same TUI.
    pub managed_code_ui_runtime: Option<Arc<CodeUiRuntimeHandle>>,
    /// Default network access policy selected at TUI launch.
    pub default_network_access: bool,
    /// Whether the first unprofiled user message should be model-classified.
    pub auto_classify_first_user_message: bool,
    /// Goal objective supplied via `libra code --goal`, if any.
    pub initial_goal: Option<String>,
    /// Source Pool control surface backing `/source` commands.
    pub source_pool: SourcePool,
}

/// The main application struct.
pub struct App<M: CompletionModel> {
    /// The TUI instance.
    tui: Tui,
    /// The chat widget.
    widget: ChatWidget,
    /// The completion model used by the agent loop.
    model: RetryingCompletionModel<M>,
    /// The tool registry.
    registry: Arc<ToolRegistry>,
    /// Tool loop runtime config.
    config: ToolLoopConfig,
    /// Default tool allow-list for regular chat turns.
    default_allowed_tools: Vec<String>,
    /// Conversation history (model-facing).
    history: Vec<Message>,
    /// Receiver for app events.
    app_event_rx: UnboundedReceiver<AppEvent>,
    /// Sender for app events.
    app_event_tx: UnboundedSender<AppEvent>,
    /// The exit info, if any.
    exit_info: Option<AppExitInfo>,
    /// Last draw time for frame rate control.
    last_draw_time: Instant,
    /// Background agent task handle (used for interrupt).
    agent_task: Option<JoinHandle<()>>,
    /// Per-turn `AbortToken` shared with `config.subagent_runtime`'s
    /// (v0.17.786). `cancel_current_turn` cancels this so any
    /// in-flight sub-agent dispatch short-circuits via the
    /// runner's `tokio::select!` (v0.17.767). Reset at every
    /// turn start; cleared when the turn ends.
    current_turn_abort_token: Option<crate::internal::ai::agent::runtime::AbortToken>,
    /// Delayed draw task for frame coalescing inside frame interval.
    scheduled_draw_task: Option<JoinHandle<()>>,
    /// Initial welcome message.
    welcome_message: String,
    /// Whether the animated welcome screen is shown.
    welcome_active: bool,
    /// Slash command dispatcher.
    command_dispatcher: CommandDispatcher,
    /// Markdown skill dispatcher.
    skill_dispatcher: SkillDispatcher,
    /// Agent router for auto-selection.
    agent_router: AgentProfileRouter,
    /// Parsed declarative multi-agent config for `/agents`, `/budget`, and runtime gates.
    agents_config: AgentsConfig,
    /// In-memory budget totals for this TUI session.
    budget_tracker: BudgetTracker,
    /// Session state for persistence.
    session: SessionState,
    /// Session store for saving/loading.
    session_store: SessionStore,
    /// Receiver for user-input requests from the `request_user_input` tool handler.
    user_input_rx: UnboundedReceiver<UserInputRequest>,
    /// Receiver for exec-approval requests from sandbox-governed handlers.
    exec_approval_rx: UnboundedReceiver<ExecApprovalRequest>,
    /// Currently pending user-input interaction, if any.
    pending_user_input: Option<PendingUserInput>,
    /// Currently pending exec approval interaction, if any.
    pending_exec_approval: Option<PendingExecApproval>,
    /// Currently pending managed-provider approval interaction, if any.
    pending_managed_interaction: Option<PendingManagedInteraction>,
    /// Currently pending Phase 3 / Phase 4 confirmation, if any.
    pending_phase_confirmation: Option<PendingPhaseConfirmation>,
    /// Post-plan dialog state (present when user is choosing Execute/Modify/Cancel).
    pending_post_plan: Option<PendingPostPlan>,
    /// Network policy dialog state after the user has chosen to execute the plan.
    pending_network_policy: Option<PendingNetworkPolicyChoice>,
    /// IntentSpec dialog state (present when user is choosing Confirm/Modify/Cancel).
    pending_intent_review: Option<PendingIntentReview>,
    /// Base IntentSpec JSON for the next spec-revision request, if the user chose Modify.
    pending_plan_revision: Option<String>,
    /// Base execution plan for the next plan-revision request, if the user chose Modify Plan.
    pending_execution_plan_revision: Option<PendingExecutionPlanRevision>,
    /// Auto-execute the next generated plan as an execution-failure repair attempt.
    pending_auto_plan_repair_execution: Option<PendingAutoPlanRepairExecution>,
    /// Display name of the active model.
    model_name: String,
    /// Provider identifier.
    provider_name: String,
    /// Session-level model usage rendered in the compact bottom-pane line.
    usage_snapshot: UsageDisplaySnapshot,
    /// Estimated output tokens already added from streaming text deltas for
    /// the active turn. Final provider usage replaces this estimate.
    pending_stream_output_tokens: u64,
    /// Arguments used to render the currently open usage detail panel.
    usage_detail_args: Option<String>,
    /// MCP server instance for writing data.
    mcp_server: Option<Arc<LibraMcpServer>>,
    /// Latest execution plan ID for attaching new turn runs.
    mcp_plan_id: Option<String>,
    /// Active turn run ID for appending decisions and tool invocations.
    mcp_run_id: Option<String>,
    /// Pending detached MCP write operations that must finish before shutdown.
    mcp_write_tracker: McpWriteTracker,
    /// Current active async turn. Events from stale turns are ignored.
    active_turn_id: Option<TurnId>,
    /// Monotonic turn counter.
    next_turn_id: TurnId,
    /// Shared view of active turn for global retry observer callbacks.
    active_turn_signal: Arc<AtomicU64>,
    /// Number of tool calls currently running in UI.
    running_tool_calls: usize,
    /// Shared run-id slot for the active turn, backfilled by MCP tracking.
    active_turn_run_id: Option<Arc<Mutex<Option<String>>>>,
    /// Provider-agnostic web snapshot state shared with the browser UI.
    code_ui_session: Option<Arc<CodeUiSession>>,
    /// Provider-agnostic web runtime state shared with the browser UI.
    code_ui_runtime: Option<Arc<CodeUiRuntimeHandle>>,
    /// Receiver for local automation commands from the Code UI adapter.
    code_control_rx: Option<UnboundedReceiver<TuiControlCommand>>,
    /// Managed provider runtime for providers that own their own tool loop.
    managed_code_ui_runtime: Option<Arc<CodeUiRuntimeHandle>>,
    /// Default network access policy selected at TUI launch.
    default_network_access: bool,
    /// Whether the first unprofiled user message should be model-classified.
    auto_classify_first_user_message: bool,
    /// Monotonic id source for browser transcript artifacts.
    next_code_ui_item_id: u64,
    /// Active Goal session for this `libra code` invocation, if any
    /// (`/goal start <objective>` or `--goal "<objective>"`).
    /// Shared by both the TUI slash-command surface and the Code
    /// Control NDJSON `goal.*` methods (OC-Phase 6 P6.5 / P6.6).
    goal_session: Option<super::goal_session::GoalSession>,
    /// Source Pool control state for this TUI session.
    source_pool: SourcePool,
}

impl<M: CompletionModel + Clone + 'static> App<M>
where
    M::Response: CompletionUsage,
{
    /// Create a new App instance.
    pub fn new(
        tui: Tui,
        model: M,
        registry: Arc<ToolRegistry>,
        config: ToolLoopConfig,
        app_config: AppConfig,
    ) -> Self {
        let (app_event_tx, app_event_rx) = mpsc::unbounded_channel();
        let active_turn_signal = Arc::new(AtomicU64::new(0));
        struct TuiRetryObserver {
            tx: UnboundedSender<AppEvent>,
            active_turn_signal: Arc<AtomicU64>,
        }

        impl CompletionRetryObserver for TuiRetryObserver {
            fn on_retry(&self, event: &CompletionRetryEvent) {
                let turn_id = self.active_turn_signal.load(Ordering::Relaxed);
                if turn_id == 0 {
                    return;
                }
                let _ = self.tx.send(AppEvent::AgentEvent {
                    turn_id,
                    event: AgentEvent::Retrying {
                        attempt: event.next_attempt,
                        total_attempts: event.total_attempts,
                        delay_ms: event.delay.as_millis().min(u128::from(u64::MAX)) as u64,
                        error: event.error.clone(),
                    },
                });
            }
        }
        let history = app_config.session.to_history();
        let default_allowed_tools = default_chat_allowed_tools(&registry, &config);
        let mut widget = ChatWidget::new();
        widget
            .bottom_pane
            .set_cwd(registry.working_dir().to_path_buf());
        widget
            .bottom_pane
            .set_git_branch(current_git_branch_label(registry.working_dir()));
        let usage_snapshot = UsageDisplaySnapshot {
            provider: app_config.provider_name.clone(),
            model: app_config.model_name.clone(),
            prompt_tokens: 0,
            completion_tokens: 0,
            wall_clock_ms: 0,
            cost_usd: None,
        };
        let usage_badge = format_usage_badge(&usage_snapshot);
        widget.bottom_pane.set_usage_line(Some(usage_badge.clone()));
        widget.set_usage_header(Some(usage_badge));
        let mcp_plan_id = app_config
            .session
            .metadata
            .get(LATEST_EXECUTION_PLAN_ID)
            .and_then(|value| value.as_str())
            .map(ToString::to_string);
        let initial_goal_session = app_config.initial_goal.as_ref().and_then(|objective| {
            let session_id = app_config.session.id.clone();
            match super::goal_session::GoalSession::create(
                session_id.clone(),
                session_id,
                objective.clone(),
                crate::internal::ai::goal::GoalActor::User { id: None },
            ) {
                Ok(session) => Some(session),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "failed to create initial Goal session from validated CLI objective"
                    );
                    None
                }
            }
        });
        // OC-Phase 6 P6.7 — `--resume <thread>` replays the
        // session's Goal envelope stream so the resumed TUI shows
        // the same active Goal it left. We only do this when no
        // `--goal "..."` flag was supplied (the explicit-objective
        // path takes precedence so the user can start a fresh Goal
        // even mid-thread). The replay surfaces `GoalReplayOutcome`'s
        // rejection list as a single warn-level trace event so a
        // forged JSONL slice is observable without crashing the
        // resume.
        let initial_goal_session = initial_goal_session.or_else(|| {
            let session_root = app_config
                .session_store
                .session_root(&app_config.session.id);
            replay_goal_session_from_session_root(&session_root)
        });
        if let Some(session) = initial_goal_session.as_ref()
            && app_config.initial_goal.is_some()
        {
            // Only persist when we built the session from a fresh
            // `--goal` flag; a resumed session already has its
            // events on disk.
            let session_root = app_config
                .session_store
                .session_root(&app_config.session.id);
            if let Err(error) = persist_goal_events_to_session_root(session_root, session.events())
            {
                tracing::warn!(
                    error = %error,
                    "failed to persist initial Goal event"
                );
            }
        }
        Self {
            tui,
            widget,
            model: RetryingCompletionModel::new(model)
                .with_policy(CompletionRetryPolicy::default())
                .with_observer(Arc::new(TuiRetryObserver {
                    tx: app_event_tx.clone(),
                    active_turn_signal: active_turn_signal.clone(),
                })),
            registry,
            config,
            default_allowed_tools,
            history,
            app_event_rx,
            app_event_tx,
            exit_info: None,
            last_draw_time: Instant::now(),
            agent_task: None,
            current_turn_abort_token: None,
            scheduled_draw_task: None,
            welcome_message: app_config.welcome_message,
            welcome_active: true,
            command_dispatcher: app_config.command_dispatcher,
            skill_dispatcher: app_config.skill_dispatcher,
            agent_router: app_config.agent_router,
            agents_config: app_config.agents_config,
            budget_tracker: BudgetTracker::new(),
            session: app_config.session,
            session_store: app_config.session_store,
            user_input_rx: app_config.user_input_rx,
            exec_approval_rx: app_config.exec_approval_rx,
            pending_user_input: None,
            pending_exec_approval: None,
            pending_managed_interaction: None,
            pending_phase_confirmation: None,
            pending_post_plan: None,
            pending_network_policy: None,
            pending_intent_review: None,
            pending_plan_revision: None,
            pending_execution_plan_revision: None,
            pending_auto_plan_repair_execution: None,
            model_name: app_config.model_name,
            provider_name: app_config.provider_name,
            usage_snapshot,
            pending_stream_output_tokens: 0,
            usage_detail_args: None,
            mcp_server: app_config.mcp_server,
            mcp_plan_id,
            mcp_run_id: None,
            mcp_write_tracker: McpWriteTracker::default(),
            active_turn_id: None,
            next_turn_id: 1,
            active_turn_signal,
            running_tool_calls: 0,
            active_turn_run_id: None,
            code_ui_session: app_config.code_ui_session,
            code_ui_runtime: app_config.code_ui_runtime,
            code_control_rx: app_config.code_control_rx,
            managed_code_ui_runtime: app_config.managed_code_ui_runtime,
            default_network_access: app_config.default_network_access,
            auto_classify_first_user_message: app_config.auto_classify_first_user_message,
            next_code_ui_item_id: 1,
            goal_session: initial_goal_session,
            source_pool: app_config.source_pool,
        }
    }

    /// Run the main event loop.
    pub async fn run(&mut self) -> anyhow::Result<AppExitInfo> {
        // Enter alternate screen
        self.tui.enter_alt_screen()?;
        let run_result = self.run_in_alt_screen().await;
        self.interrupt_agent_task();
        let leave_result = self.tui.leave_alt_screen();
        self.sweep_fuse_task_worktrees_on_shutdown().await;

        // Save session on exit (best-effort)
        if self.session.message_count() > 0
            && let Err(e) = self.session_store.save(&self.session)
        {
            tracing::warn!("Failed to save session: {}", e);
        }

        match (run_result, leave_result) {
            (Ok(exit_info), Ok(())) => Ok(exit_info),
            (Err(run_err), Ok(())) => Err(run_err),
            (Ok(_), Err(leave_err)) => Err(leave_err.into()),
            (Err(run_err), Err(_leave_err)) => Err(run_err),
        }
    }

    #[cfg(unix)]
    async fn sweep_fuse_task_worktrees_on_shutdown(&self) {
        let repo_working_dir = self.registry.working_dir().to_path_buf();
        let display_repo = repo_working_dir.display().to_string();
        let sweep_result = tokio::task::spawn_blocking(move || {
            fuse_utils::sweep_repo_fuse_task_worktrees(&repo_working_dir)
        })
        .await;

        match sweep_result {
            Ok(Ok(report)) => {
                if !report.is_empty() {
                    tracing::info!(
                        repo = %display_repo,
                        scanned = report.scanned,
                        cleaned = report.cleaned,
                        skipped_live_owner = report.skipped_live_owner,
                        failures = report.failures.len(),
                        "swept repo-local FUSE task worktrees during TUI shutdown"
                    );
                }
                for failure in report.failures {
                    tracing::warn!(
                        path = %failure.path.display(),
                        error = %failure.message,
                        "failed to clean repo-local FUSE task worktree during TUI shutdown"
                    );
                }
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    repo = %display_repo,
                    error = %err,
                    "failed to sweep repo-local FUSE task worktrees during TUI shutdown"
                );
            }
            Err(err) => {
                tracing::warn!(
                    repo = %display_repo,
                    error = %err,
                    "repo-local FUSE task worktree shutdown sweep task failed"
                );
            }
        }
    }

    #[cfg(not(unix))]
    async fn sweep_fuse_task_worktrees_on_shutdown(&self) {}

    async fn run_in_alt_screen(&mut self) -> anyhow::Result<AppExitInfo> {
        self.tui.clear()?;

        // Set up slash-command autocomplete hints (built-in + YAML-defined).
        let mut hints: Vec<(String, String)> = super::slash_command::BuiltinCommand::all_hints();
        hints.extend(
            self.command_dispatcher
                .commands()
                .iter()
                .map(|c| (c.name.clone(), c.description.clone())),
        );
        hints.extend(self.skill_dispatcher.skills().iter().map(|skill| {
            (
                format!("skill {}", skill.name),
                format!("Skill: {}", skill.description),
            )
        }));
        self.widget.bottom_pane.set_command_hints(hints);
        // Seed the bottom-pane Goal indicator so a `libra code --goal "..."`
        // launch shows the active Goal on the very first frame (no
        // need to wait for the first slash-command refresh).
        self.refresh_bottom_pane_goal_status();

        // Initial draw - ensure UI is rendered immediately
        self.draw()?;

        // Get the event stream
        let mut event_stream = self.tui.event_stream();
        let mut animation_tick = interval(Duration::from_millis(120));
        let (managed_event_tx, mut managed_event_rx) =
            mpsc::unbounded_channel::<CodeUiEventEnvelope>();
        let mut code_control_rx = self.code_control_rx.take();
        let managed_event_task = self.managed_code_ui_runtime.as_ref().map(|runtime| {
            let mut events = runtime.subscribe();
            tokio::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            if managed_event_tx.send(event).is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            })
        });

        loop {
            // Check if we should exit
            if self.exit_info.is_some() {
                break;
            }

            tokio::select! {
                // Handle terminal events
                Some(event) = event_stream.next() => {
                    self.handle_tui_event(event).await?;
                }

                // Handle app events
                Some(event) = self.app_event_rx.recv() => {
                    self.handle_app_event(event).await?;
                }

                // Mirror managed provider snapshots into the local TUI.
                Some(event) = managed_event_rx.recv(), if self.managed_code_ui_runtime.is_some() => {
                    self.handle_managed_code_ui_event(event).await?;
                }

                Some(command) = async {
                    match code_control_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                }, if code_control_rx.is_some() => {
                    self.handle_tui_control_command(command).await;
                }

                // Handle user-input requests from the tool handler
                Some(request) = self.user_input_rx.recv() => {
                    self.drain_pending_app_events().await?;
                    self.handle_user_input_request(request);
                }

                // Handle exec-approval requests from sandbox-governed handlers.
                Some(request) = self.exec_approval_rx.recv() => {
                    self.drain_pending_app_events().await?;
                    self.handle_exec_approval_request(request);
                }

                // Drive subtle status/tool animations while the agent is active.
                _ = animation_tick.tick() => {
                    if matches!(
                        self.widget.bottom_pane.status,
                        AgentStatus::Thinking | AgentStatus::Retrying | AgentStatus::ExecutingTool
                    ) || self.welcome_active {
                        self.schedule_draw();
                    }
                }
            }
        }

        self.interrupt_agent_task();
        if let Some(task) = managed_event_task {
            task.abort();
        }
        self.mcp_write_tracker.drain().await;
        let mut exit_info = self.exit_info.clone().unwrap_or(AppExitInfo {
            reason: ExitReason::UserRequested,
            thread_id: None,
        });
        self.create_mcp_exit_decision(&exit_info.reason).await;
        if exit_info.thread_id.is_none() {
            exit_info.thread_id = self.graph_thread_id_hint().await;
        }

        Ok(exit_info)
    }

    fn begin_turn(&mut self) -> TurnId {
        let turn_id = self.next_turn_id;
        self.next_turn_id = self.next_turn_id.saturating_add(1);
        self.active_turn_id = Some(turn_id);
        self.active_turn_signal.store(turn_id, Ordering::Relaxed);
        self.active_turn_run_id = Some(Arc::new(Mutex::new(None)));
        turn_id
    }

    fn clear_active_turn(&mut self) {
        self.active_turn_id = None;
        self.active_turn_signal.store(0, Ordering::Relaxed);
        self.active_turn_run_id = None;
    }

    fn clear_turn_tracking(&mut self) {
        self.clear_active_turn();
        self.clear_mcp_run_id();
    }

    fn next_code_ui_id(&mut self, prefix: &str) -> String {
        let id = format!("{prefix}-{}", self.next_code_ui_item_id);
        self.next_code_ui_item_id = self.next_code_ui_item_id.saturating_add(1);
        id
    }

    fn code_ui_user_entry_id(turn_id: TurnId) -> String {
        format!("turn-{turn_id}-user")
    }

    fn code_ui_assistant_entry_id(turn_id: TurnId) -> String {
        format!("turn-{turn_id}-assistant")
    }

    fn is_active_turn(&self, turn_id: TurnId) -> bool {
        self.active_turn_id == Some(turn_id)
    }

    /// Handle a terminal event.
    async fn handle_tui_event(&mut self, event: TuiEvent) -> anyhow::Result<()> {
        match event {
            TuiEvent::Key(key) => {
                if key.kind == crossterm::event::KeyEventKind::Press {
                    self.handle_key_event(key).await?;
                }
            }
            TuiEvent::Paste(text) => {
                self.handle_paste_text(&text);
            }
            TuiEvent::Mouse(mouse) => {
                self.handle_mouse_event(mouse);
                self.schedule_draw();
            }
            TuiEvent::Resize => {
                self.schedule_draw();
            }
            TuiEvent::Draw => {
                self.scheduled_draw_task = None;
                self.last_draw_time = Instant::now();
                self.draw()?;
            }
        }
        Ok(())
    }

    fn handle_paste_text(&mut self, text: &str) {
        let text = normalize_terminal_paste_text(text);
        if text.is_empty() {
            return;
        }

        match self.widget.bottom_pane.status {
            AgentStatus::Idle => {
                self.widget.bottom_pane.insert_text(&text);
                self.widget.bottom_pane.sync_command_popup();
            }
            AgentStatus::AwaitingUserInput => {
                let is_freeform = self.pending_user_input.as_ref().is_some_and(|pending| {
                    let question = &pending.request.questions[pending.current_question];
                    question.options.as_ref().is_none_or(Vec::is_empty)
                });
                let notes_focused = self
                    .pending_user_input
                    .as_ref()
                    .is_some_and(|pending| pending.notes_focused);

                if notes_focused {
                    if let Some(pending) = self.pending_user_input.as_mut() {
                        pending.notes_text.push_str(&text);
                    }
                    self.sync_user_input_to_pane();
                } else if is_freeform {
                    self.widget.bottom_pane.insert_text(&text);
                } else {
                    return;
                }
            }
            AgentStatus::Thinking | AgentStatus::Retrying | AgentStatus::ExecutingTool
                if self.widget.has_task_mux() =>
            {
                self.widget.bottom_pane.insert_text(&text);
            }
            _ => return,
        }

        self.schedule_draw();
    }

    async fn handle_tui_control_command(&mut self, command: TuiControlCommand) {
        match command {
            TuiControlCommand::SubmitMessage { text, ack } => {
                let result = self.submit_message_from_code_ui(text).await;
                let _ = ack.send(result);
            }
            TuiControlCommand::RespondInteraction {
                interaction_id,
                response,
                ack,
            } => {
                let result = self
                    .respond_pending_interaction_from_code_ui(&interaction_id, response)
                    .await;
                let _ = ack.send(result);
            }
            TuiControlCommand::CancelCurrentTurn { ack } => {
                let result = self.cancel_current_turn(CancelSource::Automation).await;
                let _ = ack.send(result);
            }
            TuiControlCommand::TaskDispatch { agent, prompt, ack } => {
                let result = self.task_dispatch_from_control(agent, prompt).await;
                let _ = ack.send(result);
            }
            TuiControlCommand::ReclaimController { ack } => {
                let result = self.reclaim_local_controller().await;
                let _ = ack.send(result);
            }
            TuiControlCommand::GoalStart { objective, ack } => {
                let result = self.goal_session_start_from_control(objective);
                let _ = ack.send(result);
            }
            TuiControlCommand::GoalStatus { ack } => {
                let result = self.goal_session_status_from_control();
                let _ = ack.send(result);
            }
            TuiControlCommand::GoalCancel { reason, ack } => {
                let result = self.goal_session_cancel_from_control(reason);
                let _ = ack.send(result);
            }
        }
    }

    /// `goal.start { objective }` — create an active Goal mirroring
    /// `/goal start <objective>`. Refuses if a Goal is already
    /// active in this session. The Code Control client receives
    /// the rendered status so it can display the goal id without a
    /// follow-up `goal.status` call.
    fn goal_session_start_from_control(
        &mut self,
        objective: String,
    ) -> Result<String, TuiControlError> {
        use super::goal_session::{GoalSession, GoalSessionError, render_goal_status};
        if self.goal_session.as_ref().is_some_and(|s| !s.is_terminal()) {
            return Err(TuiControlError::GoalAlreadyActive);
        }
        // Reuse the session id for both `thread_id` and
        // `session_id` on the spec — the schema accepts any string
        // for those fields, and this keeps the in-memory Goal
        // bound to the running TUI session.
        let session_id = self.session.id.clone();
        let actor = goal_actor_for_session(self);
        match GoalSession::create(session_id.clone(), session_id, objective, actor) {
            Ok(session) => {
                if let Err(error) = self.persist_goal_events_to_jsonl(session.events()) {
                    return Err(TuiControlError::Internal(format!(
                        "failed to persist Goal start event: {error}"
                    )));
                }
                let rendered = render_goal_status(session.state());
                self.goal_session = Some(session);
                self.refresh_bottom_pane_goal_status();
                Ok(rendered)
            }
            Err(GoalSessionError::InvalidObjective { source }) => {
                Err(TuiControlError::GoalInvalidObjective(source.to_string()))
            }
            Err(other) => Err(TuiControlError::Internal(other.to_string())),
        }
    }

    /// `goal.status` — render the active Goal's snapshot, or
    /// `GoalNotActive` if none.
    fn goal_session_status_from_control(&self) -> Result<String, TuiControlError> {
        use super::goal_session::render_goal_status;
        match self.goal_session.as_ref() {
            Some(session) => Ok(render_goal_status(session.state())),
            None => Err(TuiControlError::GoalNotActive),
        }
    }

    /// `goal.cancel { reason }` — append `GoalEvent::Cancelled` to
    /// the active session and clear the slot. Refuses if no Goal is
    /// active or if the active Goal is already terminal.
    fn goal_session_cancel_from_control(
        &mut self,
        reason: String,
    ) -> Result<String, TuiControlError> {
        use super::goal_session::{GoalSessionError, render_goal_status};
        let actor = goal_actor_for_session(self);
        let session_root = self.session_store.session_root(&self.session.id);
        let Some(session) = self.goal_session.as_mut() else {
            return Err(TuiControlError::GoalNotActive);
        };
        let previous_event_count = session.events().len();
        match session.cancel(reason, actor) {
            Ok(outcome) => {
                let new_events = session.events()[previous_event_count..].to_vec();
                if let Err(error) = persist_goal_events_to_session_root(session_root, &new_events) {
                    return Err(TuiControlError::Internal(format!(
                        "failed to persist Goal cancel event: {error}"
                    )));
                }
                let rendered = render_goal_status(&outcome.state);
                // Per opencode.md:665 a Cancelled session is
                // terminal; clear the slot so a subsequent
                // `goal.start` succeeds without an explicit
                // teardown call.
                self.goal_session = None;
                self.refresh_bottom_pane_goal_status();
                Ok(rendered)
            }
            Err(GoalSessionError::NotActive) => {
                self.goal_session = None;
                self.refresh_bottom_pane_goal_status();
                Err(TuiControlError::GoalNotActive)
            }
            Err(other) => Err(TuiControlError::Internal(other.to_string())),
        }
    }

    /// `/goal criteria add <text>` — append a single user-authored
    /// acceptance criterion to the active Goal via a
    /// `CriteriaRevised` envelope. The dispatcher delegates id
    /// minting to `GoalSession::revise_criteria_add`; the user only
    /// supplies the natural-language description. Refuses with
    /// `GoalNotActive` when no Goal is in flight (mirrors the
    /// `/goal status` and `/goal cancel` shape).
    fn goal_session_criteria_add_from_control(
        &mut self,
        description: String,
    ) -> Result<String, TuiControlError> {
        use super::goal_session::{GoalSessionError, render_goal_status};
        let actor = goal_actor_for_session(self);
        let session_root = self.session_store.session_root(&self.session.id);
        let Some(session) = self.goal_session.as_mut() else {
            return Err(TuiControlError::GoalNotActive);
        };
        let previous_event_count = session.events().len();
        match session.revise_criteria_add(description, actor) {
            Ok(state) => {
                let new_events = session.events()[previous_event_count..].to_vec();
                if let Err(error) = persist_goal_events_to_session_root(session_root, &new_events) {
                    return Err(TuiControlError::Internal(format!(
                        "failed to persist Goal criteria revision: {error}"
                    )));
                }
                let rendered = render_goal_status(&state);
                self.refresh_bottom_pane_goal_status();
                Ok(rendered)
            }
            Err(GoalSessionError::NotActive) => Err(TuiControlError::GoalNotActive),
            Err(GoalSessionError::InvalidObjective { source }) => {
                Err(TuiControlError::GoalInvalidObjective(source.to_string()))
            }
            Err(other) => Err(TuiControlError::Internal(other.to_string())),
        }
    }

    fn persist_goal_events_to_jsonl(&self, events: &[GoalEventEnvelope]) -> std::io::Result<()> {
        persist_goal_events_to_session_root(
            self.session_store.session_root(&self.session.id),
            events,
        )
    }

    /// Refresh the bottom-pane Goal indicator from the current
    /// `goal_session` slot. Call after every mutation
    /// (`start` / `cancel` / `criteria add` / supervisor envelope
    /// fold) so the user sees the live state without invoking
    /// `/goal status`. Setting the line to `None` when there is no
    /// active Goal hides the row entirely so the layout shrinks
    /// back to its non-Goal height.
    fn refresh_bottom_pane_goal_status(&mut self) {
        use super::goal_session::render_goal_status_line;
        let line = self
            .goal_session
            .as_ref()
            .map(|session| render_goal_status_line(session.state()));
        self.widget.bottom_pane.set_goal_status_line(line);
    }

    async fn submit_message_from_code_ui(&mut self, text: String) -> Result<(), TuiControlError> {
        if !self.can_accept_code_ui_submit() {
            return Err(TuiControlError::Busy);
        }
        self.submit_message_from_source(text, TurnInputSource::Automation)
            .await;
        Ok(())
    }

    fn can_accept_code_ui_submit(&self) -> bool {
        matches!(self.widget.bottom_pane.status, AgentStatus::Idle)
            && self.active_turn_id.is_none()
            && self.pending_user_input.is_none()
            && self.pending_exec_approval.is_none()
            && self.pending_managed_interaction.is_none()
            && self.pending_phase_confirmation.is_none()
            && self.pending_post_plan.is_none()
            && self.pending_network_policy.is_none()
            && self.pending_intent_review.is_none()
            && self.pending_plan_revision.is_none()
            && self.pending_execution_plan_revision.is_none()
    }

    async fn task_dispatch_from_control(
        &mut self,
        agent: String,
        prompt: String,
    ) -> Result<String, TuiControlError> {
        if !self.can_accept_code_ui_submit() {
            return Err(TuiControlError::Busy);
        }
        let invocation = task_invocation_from_parts(&agent, &prompt)
            .map_err(TuiControlError::TaskInvalidRequest)?;
        let message = self.dispatch_user_task_invocation(invocation).await;
        self.widget
            .add_cell(Box::new(AssistantHistoryCell::new(message.clone())));
        self.sync_mux_input_context();
        self.schedule_draw();
        Ok(message)
    }

    async fn automation_controller_active(&self) -> bool {
        let Some(code_ui_session) = self.code_ui_session.as_ref() else {
            return false;
        };
        let snapshot = code_ui_session.snapshot().await;
        snapshot.controller.kind
            == crate::internal::ai::web::code_ui::CodeUiControllerKind::Automation
            && snapshot
                .controller
                .lease_expires_at
                .is_some_and(|expires_at| expires_at > Utc::now())
    }

    async fn reclaim_local_controller(&mut self) -> Result<(), TuiControlError> {
        let Some(runtime) = self.code_ui_runtime.clone() else {
            return Err(TuiControlError::ControllerConflict);
        };
        runtime
            .reclaim_local_tui_controller()
            .await
            .map_err(|error| {
                if error.code == "CONTROLLER_CONFLICT" {
                    TuiControlError::ControllerConflict
                } else {
                    TuiControlError::Internal(error.message)
                }
            })?;
        self.sync_mux_input_context();
        self.schedule_draw();
        Ok(())
    }

    async fn respond_pending_interaction_from_code_ui(
        &mut self,
        interaction_id: &str,
        response: CodeUiInteractionResponse,
    ) -> Result<(), TuiControlError> {
        if self
            .pending_user_input
            .as_ref()
            .is_some_and(|pending| pending.request.call_id == interaction_id)
        {
            return self.respond_pending_user_input_from_code_ui(response).await;
        }

        if self
            .pending_exec_approval
            .as_ref()
            .is_some_and(|pending| pending.request.call_id == interaction_id)
        {
            let Some(pending) = self.pending_exec_approval.as_ref() else {
                return Err(TuiControlError::UnsupportedInteractionKind);
            };
            let cache_disabled = pending.request.cache_disabled_reason.is_some();
            let confirm_allow_all = pending.allow_all_confirmation_requested;
            let option_ids = exec_approval_option_ids(cache_disabled, confirm_allow_all);
            let selected = selection_from_response(
                &option_ids,
                &response,
                Some(0),
                Some(exec_approval_deny_selection(
                    cache_disabled,
                    confirm_allow_all,
                )),
            )
            .ok_or(TuiControlError::UnsupportedInteractionKind)?;
            if let Some(pending) = self.pending_exec_approval.as_mut() {
                pending.selected = selected;
                self.widget.bottom_pane.exec_approval_selected = selected;
            }
            self.submit_exec_approval_decision();
            return Ok(());
        }

        if self
            .pending_phase_confirmation
            .as_ref()
            .is_some_and(|pending| pending.interaction_id == interaction_id)
        {
            let selected = selection_from_response(
                &["continue", "reject", "abort"],
                &response,
                Some(0),
                Some(1),
            )
            .ok_or(TuiControlError::UnsupportedInteractionKind)?;
            if let Some(pending) = self.pending_phase_confirmation.as_mut() {
                pending.selected = selected;
                self.widget.bottom_pane.exec_approval_selected = selected;
            }
            self.submit_phase_confirmation_decision();
            return Ok(());
        }

        if self
            .pending_managed_interaction
            .as_ref()
            .is_some_and(|pending| pending.interaction.id == interaction_id)
        {
            let selected = {
                let pending = self.pending_managed_interaction.as_ref().ok_or_else(|| {
                    TuiControlError::Internal("managed interaction disappeared".to_string())
                })?;
                selection_from_interaction_options(&pending.interaction.options, &response)
                    .ok_or(TuiControlError::UnsupportedInteractionKind)?
            };
            if let Some(pending) = self.pending_managed_interaction.as_mut() {
                pending.selected = selected;
                self.widget.bottom_pane.exec_approval_selected = selected;
            }
            self.submit_managed_interaction_decision();
            return Ok(());
        }

        if self
            .pending_intent_review
            .as_ref()
            .is_some_and(|pending| pending.interaction_id == interaction_id)
        {
            let selected = selection_from_response(
                &["confirm", "modify", "cancel"],
                &response,
                Some(0),
                Some(2),
            )
            .ok_or(TuiControlError::UnsupportedInteractionKind)?;
            if let Some(pending) = self.pending_intent_review.as_mut() {
                pending.selected = selected;
            }
            self.handle_intent_review_choice().await;
            return Ok(());
        }

        if self.pending_post_plan.as_ref().is_some_and(|pending| {
            pending.plan_id.as_deref().unwrap_or("post-plan-choice") == interaction_id
        }) {
            let selected = selection_from_response(
                &["execute", "modify", "cancel"],
                &response,
                Some(0),
                Some(2),
            )
            .ok_or(TuiControlError::UnsupportedInteractionKind)?;
            if let Some(pending) = self.pending_post_plan.as_mut() {
                pending.selected = selected;
                self.widget.bottom_pane.post_plan_selected = selected;
            }
            self.handle_post_plan_choice().await;
            return Ok(());
        }

        if self.pending_network_policy.as_ref().is_some_and(|pending| {
            network_policy_interaction_id(pending.post_plan.plan_id.as_deref()) == interaction_id
        }) {
            let selected = selection_from_response(
                &["network-deny", "network-allow", "back"],
                &response,
                None,
                None,
            )
            .ok_or(TuiControlError::UnsupportedInteractionKind)?;
            if let Some(pending) = self.pending_network_policy.as_mut() {
                pending.selected = selected;
                self.widget.bottom_pane.post_plan_selected = selected;
            }
            self.handle_network_policy_choice().await;
            return Ok(());
        }

        Err(TuiControlError::InteractionNotActive)
    }

    async fn respond_pending_user_input_from_code_ui(
        &mut self,
        response: CodeUiInteractionResponse,
    ) -> Result<(), TuiControlError> {
        let Some(pending) = self.pending_user_input.take() else {
            return Err(TuiControlError::InteractionNotActive);
        };
        let interaction_id = pending.request.call_id.clone();
        let answers = user_input_answers_from_code_ui(&pending, response)?;
        let _ = pending
            .request
            .response_tx
            .send(UserInputResponse { answers });
        self.widget.bottom_pane.set_user_input_questions(None);
        self.widget.bottom_pane.clear();
        self.widget
            .bottom_pane
            .set_status(AgentStatus::ExecutingTool);
        self.sync_mux_input_context();
        self.schedule_draw();
        if let Some(code_ui_session) = self.code_ui_session.clone() {
            code_ui_session.resolve_interaction(&interaction_id).await;
            code_ui_session
                .set_status(CodeUiSessionStatus::ExecutingTool)
                .await;
        }
        Ok(())
    }

    async fn cancel_current_turn(&mut self, source: CancelSource) -> Result<(), TuiControlError> {
        if !self.has_local_active_turn() && !self.has_code_ui_active_turn().await {
            return Err(TuiControlError::Busy);
        }

        let reason = match source {
            CancelSource::Esc => "Turn interrupted by user",
            CancelSource::SlashQuit => "Turn interrupted by quit command",
            CancelSource::Automation => "Turn interrupted by automation",
            CancelSource::Budget => "Turn interrupted by budget cap",
        };
        self.enqueue_mcp_turn_decision("abandon", reason.to_string());
        self.cancel_pending_user_input();
        self.cancel_pending_exec_approval();
        self.cancel_pending_phase_confirmation_for_control();
        self.clear_pending_code_ui_dialogs().await;
        // OC-Phase 5 P5.4 usage-row backfill (v0.17.797): when a
        // turn is cancelled, the tool_loop's `record_summary` /
        // `record_failure` path never fires (the future tree is
        // dropped). Record the cancellation as a failure row so
        // the `agent_usage_stats` audit reflects the abandoned
        // turn — error_kind carries the CancelSource so an
        // operator can distinguish Esc / SlashQuit / Automation /
        // Budget abandons in the rollup.
        if let (Some(recorder), Some(context)) = (
            self.config.usage_recorder.as_ref(),
            self.config.usage_context.as_ref(),
        ) {
            let error_kind = match source {
                CancelSource::Esc => "cancelled_esc",
                CancelSource::SlashQuit => "cancelled_quit",
                CancelSource::Automation => "cancelled_automation",
                CancelSource::Budget => "cancelled_budget",
            };
            if let Err(err) = recorder.record_failure(context, error_kind, None).await {
                tracing::warn!(
                    %err,
                    error_kind,
                    "failed to record cancellation as usage failure row",
                );
            }
        }
        self.interrupt_agent_task();
        self.clear_mcp_run_id();
        self.widget.bottom_pane.set_status(AgentStatus::Idle);
        self.sync_mux_input_context();
        self.complete_streaming_assistant_cell("Interrupted.".to_string());
        self.complete_running_tool_cells_with_interrupt();
        self.schedule_draw();
        if let Some(code_ui_session) = self.code_ui_session.clone() {
            code_ui_session.cancel_active_turn("Interrupted.").await;
        }
        Ok(())
    }

    fn has_local_active_turn(&self) -> bool {
        self.active_turn_id.is_some()
            || matches!(
                self.widget.bottom_pane.status,
                AgentStatus::Thinking
                    | AgentStatus::Retrying
                    | AgentStatus::ExecutingTool
                    | AgentStatus::AwaitingApproval
            )
    }

    async fn has_code_ui_active_turn(&self) -> bool {
        let Some(code_ui_session) = self.code_ui_session.as_ref() else {
            return false;
        };
        matches!(
            code_ui_session.snapshot().await.status,
            CodeUiSessionStatus::Thinking
                | CodeUiSessionStatus::ExecutingTool
                | CodeUiSessionStatus::AwaitingInteraction
        )
    }

    fn cancel_pending_phase_confirmation_for_control(&mut self) {
        if let Some(pending) = self.pending_phase_confirmation.take() {
            let _ = pending.response_tx.send(PhaseConfirmationDecision::Abort);
            self.widget.bottom_pane.set_exec_approval(None);
        }
    }

    async fn clear_pending_code_ui_dialogs(&mut self) {
        let mut ids = Vec::new();
        if let Some(pending) = self.pending_intent_review.take() {
            ids.push(pending.interaction_id);
        }
        if let Some(pending) = self.pending_post_plan.take() {
            ids.push(
                pending
                    .plan_id
                    .unwrap_or_else(|| "post-plan-choice".to_string()),
            );
        }
        if let Some(pending) = self.pending_network_policy.take() {
            ids.push(network_policy_interaction_id(
                pending.post_plan.plan_id.as_deref(),
            ));
        }
        self.pending_plan_revision = None;
        self.pending_execution_plan_revision = None;

        if let Some(code_ui_session) = self.code_ui_session.clone() {
            for id in ids {
                code_ui_session.clear_interaction(&id).await;
            }
        }
    }

    /// Handle a key press event.
    async fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) -> anyhow::Result<()> {
        // Check for Ctrl+C first (always handled)
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.request_user_exit();
            return Ok(());
        }

        // `/quit` is a local TUI command and must preempt every interaction
        // state, including a running workflow mux. Otherwise it is treated as
        // ordinary shared-input text while the orchestrator keeps running.
        if key.code == KeyCode::Enter && self.try_handle_global_quit_command() {
            return Ok(());
        }

        // Handle input based on agent status
        match self.widget.bottom_pane.status {
            AgentStatus::Idle => match key.code {
                KeyCode::Esc if self.widget.bottom_pane.is_usage_detail_panel_visible() => {
                    self.widget.bottom_pane.dismiss_usage_detail_panel();
                    self.usage_detail_args = None;
                    self.schedule_draw();
                }
                // ── Command popup intercepts (when visible) ──────────
                KeyCode::Tab if self.widget.bottom_pane.is_command_popup_visible() => {
                    self.widget.bottom_pane.complete_command();
                    self.widget.bottom_pane.sync_command_popup();
                    self.schedule_draw();
                }
                KeyCode::Up if self.widget.bottom_pane.is_command_popup_visible() => {
                    self.widget.bottom_pane.command_popup_up();
                    self.schedule_draw();
                }
                KeyCode::Down if self.widget.bottom_pane.is_command_popup_visible() => {
                    self.widget.bottom_pane.command_popup_down();
                    self.schedule_draw();
                }
                KeyCode::Esc if self.widget.bottom_pane.is_command_popup_visible() => {
                    self.widget.bottom_pane.dismiss_command_popup();
                    self.schedule_draw();
                }
                // ── Normal idle handlers ─────────────────────────────
                KeyCode::Enter if !self.widget.bottom_pane.is_empty() => {
                    let text = self.widget.bottom_pane.take_input();
                    if self.automation_controller_active().await
                        && !is_control_reclaim_command_input(&text)
                    {
                        self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                            "Automation is in control. Use `/control reclaim` to take back local input.".to_string(),
                        )));
                        self.widget.bottom_pane.sync_command_popup();
                        self.schedule_draw();
                        return Ok(());
                    }
                    if self.welcome_active {
                        self.welcome_active = false;
                        self.schedule_draw();
                    }
                    self.submit_message(text).await;
                }
                // Clear screen (Ctrl+K) - must come before generic Char handler
                KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.widget.clear();
                    self.widget.bottom_pane.clear();
                    self.widget.bottom_pane.sync_command_popup();
                    self.schedule_draw();
                }
                // Scroll to top (Home)
                KeyCode::Home => {
                    self.widget.scroll_to_top();
                    self.schedule_draw();
                }
                // Scroll to bottom (End)
                KeyCode::End => {
                    self.widget.scroll_to_bottom();
                    self.schedule_draw();
                }
                // Scroll
                KeyCode::PageUp => {
                    self.widget.scroll_up_lines(10);
                    self.schedule_draw();
                }
                KeyCode::PageDown => {
                    self.widget.scroll_down_lines(10);
                    self.schedule_draw();
                }
                KeyCode::Up => {
                    self.widget.scroll_up_lines(1);
                    self.schedule_draw();
                }
                KeyCode::Down => {
                    self.widget.scroll_down_lines(1);
                    self.schedule_draw();
                }
                // Cursor to beginning of input (Ctrl+A)
                KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.widget.bottom_pane.cursor_home();
                    self.schedule_draw();
                }
                // Cursor to end of input (Ctrl+E)
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.widget.bottom_pane.cursor_end();
                    self.schedule_draw();
                }
                KeyCode::Char(c) => {
                    self.widget.bottom_pane.insert_char(c);
                    self.widget.bottom_pane.sync_command_popup();
                    self.schedule_draw();
                }
                KeyCode::Backspace => {
                    self.widget.bottom_pane.backspace();
                    self.widget.bottom_pane.sync_command_popup();
                    self.schedule_draw();
                }
                KeyCode::Delete => {
                    self.widget.bottom_pane.delete();
                    self.widget.bottom_pane.sync_command_popup();
                    self.schedule_draw();
                }
                KeyCode::Left => {
                    self.widget.bottom_pane.cursor_left();
                    self.schedule_draw();
                }
                KeyCode::Right => {
                    self.widget.bottom_pane.cursor_right();
                    self.schedule_draw();
                }
                _ => {}
            },
            AgentStatus::AwaitingUserInput => {
                self.handle_user_input_key(key);
            }
            AgentStatus::AwaitingApproval => match key.code {
                KeyCode::Up => {
                    if let Some(ref mut pending) = self.pending_phase_confirmation {
                        pending.selected = pending.selected.saturating_sub(1);
                        self.widget.bottom_pane.exec_approval_selected = pending.selected;
                    } else if let Some(ref mut pending) = self.pending_managed_interaction {
                        pending.selected = pending.selected.saturating_sub(1);
                        self.widget.bottom_pane.exec_approval_selected = pending.selected;
                    } else if let Some(ref mut pending) = self.pending_exec_approval {
                        pending.selected = pending.selected.saturating_sub(1);
                        self.widget.bottom_pane.exec_approval_selected = pending.selected;
                    }
                    self.schedule_draw();
                }
                KeyCode::Down => {
                    if let Some(ref mut pending) = self.pending_phase_confirmation {
                        pending.selected = (pending.selected + 1).min(2);
                        self.widget.bottom_pane.exec_approval_selected = pending.selected;
                    } else if let Some(ref mut pending) = self.pending_managed_interaction {
                        let max = pending.interaction.options.len().saturating_sub(1);
                        pending.selected = (pending.selected + 1).min(max);
                        self.widget.bottom_pane.exec_approval_selected = pending.selected;
                    } else if let Some(ref mut pending) = self.pending_exec_approval {
                        pending.selected = (pending.selected + 1).min(5);
                        self.widget.bottom_pane.exec_approval_selected = pending.selected;
                    }
                    self.schedule_draw();
                }
                KeyCode::Enter => {
                    if self.pending_phase_confirmation.is_some() {
                        self.submit_phase_confirmation_decision();
                    } else {
                        self.submit_exec_approval_decision();
                    }
                }
                KeyCode::Esc => {
                    if self.pending_phase_confirmation.is_some() {
                        self.reject_pending_phase_confirmation();
                    } else {
                        self.reject_pending_exec_approval();
                    }
                }
                _ => {}
            },
            AgentStatus::AwaitingPostPlanChoice => {
                if let Some(action) = review_scroll_action(key) {
                    self.apply_review_scroll_action(action);
                    self.schedule_draw();
                    return Ok(());
                }

                match key.code {
                    KeyCode::Up => {
                        if let Some(ref mut p) = self.pending_post_plan {
                            p.selected = p.selected.saturating_sub(1);
                            self.widget.bottom_pane.post_plan_selected = p.selected;
                        }
                        self.schedule_draw();
                    }
                    KeyCode::Down => {
                        if let Some(ref mut p) = self.pending_post_plan {
                            p.selected = (p.selected + 1).min(2);
                            self.widget.bottom_pane.post_plan_selected = p.selected;
                        }
                        self.schedule_draw();
                    }
                    KeyCode::Enter => {
                        self.handle_post_plan_choice().await;
                    }
                    KeyCode::Esc => {
                        self.dismiss_post_plan_dialog();
                    }
                    _ => {}
                }
            }
            AgentStatus::AwaitingNetworkPolicyChoice => {
                if let Some(action) = review_scroll_action(key) {
                    self.apply_review_scroll_action(action);
                    self.schedule_draw();
                    return Ok(());
                }

                match key.code {
                    KeyCode::Up => {
                        if let Some(ref mut p) = self.pending_network_policy {
                            p.selected = p.selected.saturating_sub(1);
                            self.widget.bottom_pane.post_plan_selected = p.selected;
                        }
                        self.schedule_draw();
                    }
                    KeyCode::Down => {
                        if let Some(ref mut p) = self.pending_network_policy {
                            p.selected = (p.selected + 1).min(2);
                            self.widget.bottom_pane.post_plan_selected = p.selected;
                        }
                        self.schedule_draw();
                    }
                    KeyCode::Enter => {
                        self.handle_network_policy_choice().await;
                    }
                    KeyCode::Esc => {
                        self.return_to_post_plan_dialog();
                    }
                    _ => {}
                }
            }
            AgentStatus::AwaitingIntentReviewChoice => {
                if let Some(action) = review_scroll_action(key) {
                    self.apply_review_scroll_action(action);
                    self.schedule_draw();
                    return Ok(());
                }

                match key.code {
                    KeyCode::Up => {
                        if let Some(ref mut p) = self.pending_intent_review {
                            p.selected = p.selected.saturating_sub(1);
                            self.widget.bottom_pane.post_plan_selected = p.selected;
                        }
                        self.schedule_draw();
                    }
                    KeyCode::Down => {
                        if let Some(ref mut p) = self.pending_intent_review {
                            p.selected = (p.selected + 1).min(2);
                            self.widget.bottom_pane.post_plan_selected = p.selected;
                        }
                        self.schedule_draw();
                    }
                    KeyCode::Enter => {
                        self.handle_intent_review_choice().await;
                    }
                    KeyCode::Esc => {
                        self.dismiss_intent_review_dialog();
                    }
                    _ => {}
                }
            }
            AgentStatus::Thinking | AgentStatus::Retrying | AgentStatus::ExecutingTool => {
                let mux_visible = self.widget.has_task_mux();
                match key.code {
                    KeyCode::Tab if mux_visible => {
                        self.widget.task_mux_next();
                        self.sync_mux_input_context();
                        self.schedule_draw();
                    }
                    KeyCode::BackTab if mux_visible => {
                        self.widget.task_mux_prev();
                        self.sync_mux_input_context();
                        self.schedule_draw();
                    }
                    KeyCode::Char('o')
                        if mux_visible && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        self.widget.task_mux_show_overview();
                        self.sync_mux_input_context();
                        self.schedule_draw();
                    }
                    KeyCode::Char('f')
                        if mux_visible && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        self.widget.task_mux_focus_selected();
                        self.sync_mux_input_context();
                        self.schedule_draw();
                    }
                    KeyCode::Enter if mux_visible && !self.widget.bottom_pane.is_empty() => {
                        let command = self.widget.bottom_pane.take_input();
                        if let Some(args) = command.trim().strip_prefix("/mux") {
                            if !self.handle_mux_command(args) {
                                self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                                    "Unknown mux command. Use `/mux next`, `/mux prev`, `/mux focus <n>`, `/mux overview`, or `/mux toggle`.".to_string(),
                                )));
                            }
                        } else {
                            self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                                "Workflow is running. Use `/mux ...` to control the workflow view, `/quit` to exit, or Esc to interrupt the current turn.".to_string(),
                            )));
                        }
                        self.schedule_draw();
                    }
                    KeyCode::Char(c) if mux_visible => {
                        self.widget.bottom_pane.insert_char(c);
                        self.schedule_draw();
                    }
                    KeyCode::Backspace if mux_visible => {
                        self.widget.bottom_pane.backspace();
                        self.schedule_draw();
                    }
                    KeyCode::Delete if mux_visible => {
                        self.widget.bottom_pane.delete();
                        self.schedule_draw();
                    }
                    KeyCode::Left if mux_visible => {
                        self.widget.bottom_pane.cursor_left();
                        self.schedule_draw();
                    }
                    KeyCode::Right if mux_visible => {
                        self.widget.bottom_pane.cursor_right();
                        self.schedule_draw();
                    }
                    KeyCode::Esc if mux_visible && !self.widget.bottom_pane.is_empty() => {
                        self.widget.bottom_pane.clear();
                        self.schedule_draw();
                    }
                    KeyCode::Esc => {
                        let _ = self.cancel_current_turn(CancelSource::Esc).await;
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }

    fn apply_review_scroll_action(&mut self, action: ReviewScrollAction) {
        match action {
            ReviewScrollAction::Top => self.widget.scroll_to_top(),
            ReviewScrollAction::Bottom => self.widget.scroll_to_bottom(),
            ReviewScrollAction::Up(lines) => self.widget.scroll_up_lines(lines),
            ReviewScrollAction::Down(lines) => self.widget.scroll_down_lines(lines),
        }
    }

    /// Handle keyboard input while in the AwaitingUserInput state.
    fn handle_user_input_key(&mut self, key: crossterm::event::KeyEvent) {
        let is_freeform = self.pending_user_input.as_ref().is_some_and(|p| {
            let q = &p.request.questions[p.current_question];
            q.options.as_ref().is_none_or(|o| o.is_empty())
        });

        // If notes are focused, route most keys to the input field.
        let notes_focused = self
            .pending_user_input
            .as_ref()
            .is_some_and(|p| p.notes_focused);

        match key.code {
            // Tab: toggle between options and notes
            KeyCode::Tab if !is_freeform => {
                if let Some(ref mut pending) = self.pending_user_input {
                    pending.notes_focused = !pending.notes_focused;
                }
                self.sync_user_input_to_pane();
                self.schedule_draw();
            }
            // Navigate options with Up/Down (only when options focused)
            KeyCode::Up if !notes_focused => {
                if let Some(ref mut pending) = self.pending_user_input
                    && pending.selected_option > 0
                {
                    pending.selected_option -= 1;
                }
                self.sync_user_input_to_pane();
                self.schedule_draw();
            }
            KeyCode::Down if !notes_focused => {
                if let Some(ref mut pending) = self.pending_user_input {
                    let q = &pending.request.questions[pending.current_question];
                    let base = q.options.as_ref().map_or(0, |o| o.len());
                    let max = max_selectable_option(base, q.is_other);
                    if pending.selected_option < max {
                        pending.selected_option += 1;
                    }
                }
                self.sync_user_input_to_pane();
                self.schedule_draw();
            }
            // Quick-select by number key (1-9), only when options focused
            KeyCode::Char(c @ '1'..='9') if !notes_focused && !is_freeform => {
                let idx = (c as usize) - ('1' as usize);
                if let Some(ref mut pending) = self.pending_user_input {
                    let q = &pending.request.questions[pending.current_question];
                    let base = q.options.as_ref().map_or(0, |o| o.len());
                    let max = max_selectable_option(base, q.is_other);
                    if idx <= max {
                        pending.selected_option = idx;
                    }
                }
                self.sync_user_input_to_pane();
                self.schedule_draw();
            }
            // Type text (notes when notes_focused, or freeform input)
            KeyCode::Char(c) if notes_focused || is_freeform => {
                if notes_focused {
                    if let Some(ref mut pending) = self.pending_user_input {
                        pending.notes_text.push(c);
                    }
                    self.sync_user_input_to_pane();
                } else {
                    self.widget.bottom_pane.insert_char(c);
                }
                self.schedule_draw();
            }
            KeyCode::Backspace if notes_focused => {
                if let Some(ref mut pending) = self.pending_user_input {
                    pending.notes_text.pop();
                }
                self.sync_user_input_to_pane();
                self.schedule_draw();
            }
            KeyCode::Backspace if is_freeform => {
                self.widget.bottom_pane.backspace();
                self.schedule_draw();
            }
            // Submit answer
            KeyCode::Enter => {
                self.submit_user_input_answer();
            }
            // Cancel
            KeyCode::Esc => {
                self.cancel_pending_user_input();
                self.widget.bottom_pane.set_status(AgentStatus::Thinking);
                self.sync_mux_input_context();
                self.schedule_draw();
            }
            _ => {}
        }
    }

    /// Submit the currently selected answer for the active question.
    fn submit_user_input_answer(&mut self) {
        let interaction_id = self
            .pending_user_input
            .as_ref()
            .map(|pending| pending.request.call_id.clone());
        let answer = if let Some(ref pending) = self.pending_user_input {
            let q = &pending.request.questions[pending.current_question];
            let options = q.options.as_deref().unwrap_or_default();
            let mut answer_list: Vec<String> = Vec::new();

            if options.is_empty() {
                // Freeform question: take text from input field
                let text = self.widget.bottom_pane.take_input();
                if !text.is_empty() {
                    answer_list.push(text);
                }
            } else if pending.selected_option < options.len() {
                // Predefined option selected
                answer_list.push(options[pending.selected_option].label.clone());
            } else if q.is_other && pending.selected_option == options.len() {
                // "None of the above"
                answer_list.push("None of the above".to_string());
            }

            // Append notes if present
            if !pending.notes_text.is_empty() {
                answer_list.push(format!("user_note: {}", pending.notes_text));
            }

            UserInputAnswer {
                answers: answer_list,
            }
        } else {
            return;
        };

        let (done, question_number, question_count, answer_count) =
            if let Some(pending) = self.pending_user_input.as_mut() {
                let question_id = pending.request.questions[pending.current_question]
                    .id
                    .clone();
                let question_number = pending.current_question + 1;
                let question_count = pending.request.questions.len();
                let answer_count = answer.answers.len();
                pending.answers.insert(question_id, answer);
                pending.current_question += 1;
                pending.selected_option = 0;
                pending.notes_focused = false;
                pending.notes_text.clear();
                self.widget.bottom_pane.clear();
                (
                    pending.current_question >= pending.request.questions.len(),
                    question_number,
                    question_count,
                    answer_count,
                )
            } else {
                return;
            };

        tracing::debug!(
            target: "libra::internal::tui::interaction",
            question_number,
            question_count,
            answer_count,
            completed = done,
            "tui user-input answer submitted"
        );

        if done {
            // Send the response back to the handler.
            let Some(pending) = self.pending_user_input.take() else {
                return;
            };
            let response = UserInputResponse {
                answers: pending.answers,
            };
            let _ = pending.request.response_tx.send(response);
            self.widget
                .bottom_pane
                .set_status(AgentStatus::ExecutingTool);
            self.widget.bottom_pane.set_user_input_questions(None);
        } else {
            self.sync_user_input_to_pane();
        }
        self.schedule_draw();
        if let (Some(code_ui_session), Some(interaction_id)) =
            (self.code_ui_session.clone(), interaction_id)
        {
            tokio::spawn(async move {
                code_ui_session.resolve_interaction(&interaction_id).await;
                code_ui_session
                    .set_status(CodeUiSessionStatus::ExecutingTool)
                    .await;
            });
        }
    }

    /// Cancel the pending user-input interaction (drops the oneshot sender).
    fn cancel_pending_user_input(&mut self) {
        if let Some(pending) = self.pending_user_input.take() {
            let interaction_id = pending.request.call_id.clone();
            tracing::debug!(
                target: "libra::internal::tui::interaction",
                call_id = %interaction_id,
                answered_questions = pending.answers.len(),
                total_questions = pending.request.questions.len(),
                "tui user-input request cancelled"
            );
            // Dropping response_tx signals cancellation to the handler.
            drop(pending.request.response_tx);
            self.widget.bottom_pane.set_user_input_questions(None);
            if let Some(code_ui_session) = self.code_ui_session.clone() {
                tokio::spawn(async move {
                    code_ui_session.clear_interaction(&interaction_id).await;
                });
            }
        }
    }

    /// Sync the pending user-input state to the bottom pane for rendering.
    fn sync_user_input_to_pane(&mut self) {
        if let Some(ref pending) = self.pending_user_input {
            self.widget.bottom_pane.user_input_current_question = pending.current_question;
            self.widget.bottom_pane.user_input_selected_option = pending.selected_option;
            self.widget.bottom_pane.user_input_notes_focused = pending.notes_focused;
            self.widget.bottom_pane.user_input_notes_text = pending.notes_text.clone();
        }
    }

    /// Handle a user-input request from the tool handler.
    fn handle_user_input_request(&mut self, request: UserInputRequest) {
        let first_question = request.questions.first();
        tracing::debug!(
            target: "libra::internal::tui::interaction",
            call_id = %request.call_id,
            question_count = request.questions.len(),
            first_question_id = first_question.map(|question| question.id.as_str()).unwrap_or(""),
            first_question_options = first_question
                .and_then(|question| question.options.as_ref())
                .map(Vec::len)
                .unwrap_or_default(),
            "tui user-input request received"
        );

        let interaction = CodeUiInteractionRequest {
            id: request.call_id.clone(),
            kind: CodeUiInteractionKind::RequestUserInput,
            title: Some("User input required".to_string()),
            description: None,
            prompt: request
                .questions
                .first()
                .map(|question| question.question.clone()),
            options: request
                .questions
                .first()
                .and_then(|question| question.options.as_ref())
                .map(|options| {
                    options
                        .iter()
                        .enumerate()
                        .map(|(index, option)| CodeUiInteractionOption {
                            id: format!("option-{index}"),
                            label: option.label.clone(),
                            description: Some(option.description.clone()),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            status: CodeUiInteractionStatus::Pending,
            metadata: serde_json::to_value(
                request
                    .questions
                    .iter()
                    .map(|question| {
                        serde_json::json!({
                            "id": question.id,
                            "header": question.header,
                            "question": question.question,
                            "isOther": question.is_other,
                            "isSecret": question.is_secret,
                            "options": question.options,
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_else(|_| serde_json::json!([])),
            requested_at: Utc::now(),
            resolved_at: None,
        };

        // Store question info for the bottom pane to render.
        self.widget
            .bottom_pane
            .set_user_input_questions(Some(&request.questions));

        self.pending_user_input = Some(PendingUserInput {
            request,
            current_question: 0,
            answers: HashMap::new(),
            selected_option: 0,
            notes_focused: false,
            notes_text: String::new(),
        });
        self.widget
            .bottom_pane
            .set_status(AgentStatus::AwaitingUserInput);
        self.sync_mux_input_context();
        self.widget.bottom_pane.clear();
        self.sync_user_input_to_pane();
        self.schedule_draw();
        if let Some(code_ui_session) = self.code_ui_session.clone() {
            tokio::spawn(async move {
                code_ui_session.upsert_interaction(interaction).await;
                code_ui_session
                    .set_status(CodeUiSessionStatus::AwaitingInteraction)
                    .await;
            });
        }
    }

    fn handle_exec_approval_request(&mut self, request: ExecApprovalRequest) {
        if self.active_turn_id.is_none() {
            tracing::debug!(
                target: "libra::internal::tui::interaction",
                call_id = %request.call_id,
                command = %log_preview_text(&request.command),
                "tui sandbox approval denied because there is no active turn"
            );
            let _ = request.response_tx.send(ReviewDecision::Denied);
            return;
        }

        tracing::debug!(
            target: "libra::internal::tui::interaction",
            call_id = %request.call_id,
            command = %log_preview_text(&request.command),
            cwd = %request.cwd.display(),
            sandbox = %request.sandbox_label,
            network_access = ?request.network_access,
            writable_roots = request.writable_roots.len(),
            is_retry = request.is_retry,
            has_reason = request.reason.as_ref().is_some_and(|reason| !reason.trim().is_empty()),
            "tui sandbox approval requested"
        );

        let interaction = CodeUiInteractionRequest {
            id: request.call_id.clone(),
            kind: CodeUiInteractionKind::SandboxApproval,
            title: Some("Sandbox approval required".to_string()),
            description: request.reason.clone(),
            prompt: Some(request.command.clone()),
            options: exec_approval_interaction_options(request.cache_disabled_reason.is_some()),
            status: CodeUiInteractionStatus::Pending,
            metadata: serde_json::json!({
                "cwd": request.cwd,
                "sandboxLabel": request.sandbox_label,
                "networkAccess": network_access_label(&request.network_access),
                "writableRoots": request.writable_roots,
                "isRetry": request.is_retry,
                "cacheDisabledReason": request.cache_disabled_reason,
            }),
            requested_at: Utc::now(),
            resolved_at: None,
        };

        self.widget.bottom_pane.set_exec_approval(Some(&request));
        self.pending_exec_approval = Some(PendingExecApproval {
            request,
            selected: 0,
            allow_all_confirmation_requested: false,
        });
        self.widget.bottom_pane.exec_approval_selected = 0;
        self.widget
            .bottom_pane
            .set_status(AgentStatus::AwaitingApproval);
        self.sync_mux_input_context();
        self.schedule_draw();
        if let Some(code_ui_session) = self.code_ui_session.clone() {
            tokio::spawn(async move {
                code_ui_session.upsert_interaction(interaction).await;
                code_ui_session
                    .set_status(CodeUiSessionStatus::AwaitingInteraction)
                    .await;
            });
        }
    }

    fn handle_phase_confirmation_request(
        &mut self,
        prompt: PhaseConfirmationPrompt,
        response_tx: tokio::sync::oneshot::Sender<PhaseConfirmationDecision>,
    ) {
        let interaction_id = format!(
            "phase-{}-confirmation-{}",
            prompt.phase.number(),
            self.next_code_ui_item_id
        );
        self.next_code_ui_item_id = self.next_code_ui_item_id.saturating_add(1);
        let details = (!prompt.details.is_empty()).then(|| prompt.details.join(" | "));

        self.widget.bottom_pane.set_approval_dialog(
            prompt.title.clone(),
            prompt.summary.clone(),
            self.registry.working_dir().to_path_buf(),
            details.clone(),
            false,
            "orchestrator phase gate".to_string(),
            None,
            Vec::new(),
            vec![
                (
                    "Continue".to_string(),
                    format!("Enter {}", prompt.phase.label()),
                ),
                (
                    "Reject".to_string(),
                    "Stop this phase and report rejection".to_string(),
                ),
                ("Abort".to_string(), "Stop the workflow".to_string()),
            ],
        );
        self.pending_phase_confirmation = Some(PendingPhaseConfirmation {
            prompt: prompt.clone(),
            response_tx,
            interaction_id: interaction_id.clone(),
            selected: 0,
        });
        self.widget.bottom_pane.exec_approval_selected = 0;
        self.widget
            .bottom_pane
            .set_status(AgentStatus::AwaitingApproval);
        self.sync_mux_input_context();
        self.schedule_draw();

        if let Some(code_ui_session) = self.code_ui_session.clone() {
            let interaction = CodeUiInteractionRequest {
                id: interaction_id,
                kind: CodeUiInteractionKind::Approval,
                title: Some(prompt.title),
                description: details,
                prompt: Some(prompt.summary),
                options: vec![
                    CodeUiInteractionOption {
                        id: "continue".to_string(),
                        label: "Continue".to_string(),
                        description: Some("Continue to the next phase".to_string()),
                    },
                    CodeUiInteractionOption {
                        id: "reject".to_string(),
                        label: "Reject".to_string(),
                        description: Some("Reject this phase".to_string()),
                    },
                    CodeUiInteractionOption {
                        id: "abort".to_string(),
                        label: "Abort".to_string(),
                        description: Some("Stop the workflow".to_string()),
                    },
                ],
                status: CodeUiInteractionStatus::Pending,
                metadata: serde_json::json!({
                    "phase": prompt.phase.number(),
                    "phaseLabel": prompt.phase.label(),
                    "details": prompt.details,
                }),
                requested_at: Utc::now(),
                resolved_at: None,
            };
            tokio::spawn(async move {
                code_ui_session.upsert_interaction(interaction).await;
                code_ui_session
                    .set_status(CodeUiSessionStatus::AwaitingInteraction)
                    .await;
            });
        }
    }

    fn submit_phase_confirmation_decision(&mut self) {
        let Some(pending) = self.pending_phase_confirmation.take() else {
            return;
        };
        let decision = match pending.selected {
            0 => PhaseConfirmationDecision::Continue,
            1 => PhaseConfirmationDecision::Reject,
            _ => PhaseConfirmationDecision::Abort,
        };
        tracing::debug!(
            target: "libra::internal::tui::app",
            phase = pending.prompt.phase.number(),
            decision = ?decision,
            "tui phase confirmation resolved"
        );
        let interaction_id = pending.interaction_id;
        let _ = pending.response_tx.send(decision);
        self.widget.bottom_pane.set_exec_approval(None);
        self.widget.bottom_pane.set_status(AgentStatus::Thinking);
        self.sync_mux_input_context();
        self.schedule_draw();
        if let Some(code_ui_session) = self.code_ui_session.clone() {
            tokio::spawn(async move {
                code_ui_session.resolve_interaction(&interaction_id).await;
                code_ui_session
                    .set_status(CodeUiSessionStatus::Thinking)
                    .await;
            });
        }
    }

    fn reject_pending_phase_confirmation(&mut self) {
        let Some(pending) = self.pending_phase_confirmation.take() else {
            return;
        };
        let interaction_id = pending.interaction_id;
        let _ = pending.response_tx.send(PhaseConfirmationDecision::Abort);
        self.widget.bottom_pane.set_exec_approval(None);
        self.widget.bottom_pane.set_status(AgentStatus::Thinking);
        self.sync_mux_input_context();
        self.schedule_draw();
        if let Some(code_ui_session) = self.code_ui_session.clone() {
            tokio::spawn(async move {
                code_ui_session.resolve_interaction(&interaction_id).await;
                code_ui_session
                    .set_status(CodeUiSessionStatus::Thinking)
                    .await;
            });
        }
    }

    fn cancel_pending_phase_confirmation(&mut self) {
        if let Some(pending) = self.pending_phase_confirmation.take() {
            let interaction_id = pending.interaction_id;
            let _ = pending.response_tx.send(PhaseConfirmationDecision::Abort);
            if let Some(code_ui_session) = self.code_ui_session.clone() {
                tokio::spawn(async move {
                    code_ui_session.clear_interaction(&interaction_id).await;
                });
            }
        }
        self.widget.bottom_pane.set_exec_approval(None);
    }

    fn submit_exec_approval_decision(&mut self) {
        if self.pending_managed_interaction.is_some() {
            self.submit_managed_interaction_decision();
            return;
        }

        let Some(pending) = self.pending_exec_approval.take() else {
            return;
        };

        let cache_disabled = pending.request.cache_disabled_reason.is_some();
        let decision = exec_approval_decision_from_selection(
            pending.selected,
            cache_disabled,
            pending.allow_all_confirmation_requested,
        );
        if decision == ReviewDecision::ApprovedForAllCommands
            && !pending.allow_all_confirmation_requested
        {
            self.request_allow_all_confirmation(pending);
            return;
        }
        let interaction_id = pending.request.call_id.clone();
        tracing::debug!(
            target: "libra::internal::tui::interaction",
            call_id = %interaction_id,
            decision = ?decision,
            selected = pending.selected,
            command = %log_preview_text(&pending.request.command),
            "tui sandbox approval resolved"
        );
        let _ = pending.request.response_tx.send(decision);

        self.widget.bottom_pane.set_exec_approval(None);

        if decision == ReviewDecision::Abort {
            self.enqueue_mcp_turn_decision(
                "abandon",
                "Turn interrupted by approval dialog".to_string(),
            );
            self.interrupt_agent_task();
            self.clear_mcp_run_id();
            self.widget.bottom_pane.set_status(AgentStatus::Idle);
            self.complete_streaming_assistant_cell("Interrupted.".to_string());
            self.complete_running_tool_cells_with_interrupt();
            self.schedule_draw();
            if let Some(code_ui_session) = self.code_ui_session.clone() {
                tokio::spawn(async move {
                    code_ui_session.resolve_interaction(&interaction_id).await;
                    code_ui_session.set_status(CodeUiSessionStatus::Idle).await;
                });
            }
            return;
        }

        self.widget
            .bottom_pane
            .set_status(AgentStatus::ExecutingTool);
        self.sync_mux_input_context();
        self.schedule_draw();
        if let Some(code_ui_session) = self.code_ui_session.clone() {
            tokio::spawn(async move {
                code_ui_session.resolve_interaction(&interaction_id).await;
                code_ui_session
                    .set_status(CodeUiSessionStatus::ExecutingTool)
                    .await;
            });
        }
    }

    fn request_allow_all_confirmation(&mut self, mut pending: PendingExecApproval) {
        pending.allow_all_confirmation_requested = true;
        pending.selected = 0;
        let interaction_id = pending.request.call_id.clone();
        let command = pending.request.command.clone();
        let cwd = pending.request.cwd.clone();
        let sandbox_label = pending.request.sandbox_label.clone();
        let network_access = pending.request.network_access.clone();
        let writable_roots = pending.request.writable_roots.clone();
        let is_retry = pending.request.is_retry;

        self.widget.bottom_pane.set_approval_dialog(
            "Confirm allow all commands".to_string(),
            command.clone(),
            cwd.clone(),
            Some(
                "This will allow every command for this session and bypass future approval prompts."
                    .to_string(),
            ),
            is_retry,
            sandbox_label.clone(),
            Some(network_access.clone()),
            writable_roots.clone(),
            vec![
                (
                    "Confirm Allow All".to_string(),
                    "Allow every command for this session".to_string(),
                ),
                ("Deny".to_string(), "Reject this execution".to_string()),
                (
                    "Abort Turn".to_string(),
                    "Interrupt the current turn".to_string(),
                ),
            ],
        );
        self.widget.bottom_pane.exec_approval_selected = 0;
        self.pending_exec_approval = Some(pending);
        self.sync_mux_input_context();
        self.schedule_draw();

        if let Some(code_ui_session) = self.code_ui_session.clone() {
            let interaction = CodeUiInteractionRequest {
                id: interaction_id,
                kind: CodeUiInteractionKind::SandboxApproval,
                title: Some("Confirm allow all commands".to_string()),
                description: Some(
                    "This will allow every command for this session and bypass future approval prompts."
                        .to_string(),
                ),
                prompt: Some(command),
                options: exec_approval_allow_all_confirmation_options(),
                status: CodeUiInteractionStatus::Pending,
                metadata: serde_json::json!({
                    "cwd": cwd,
                    "sandboxLabel": sandbox_label,
                    "networkAccess": network_access_label(&network_access),
                    "writableRoots": writable_roots,
                    "isRetry": is_retry,
                    "confirmation": "allow_all_commands",
                }),
                requested_at: Utc::now(),
                resolved_at: None,
            };
            tokio::spawn(async move {
                code_ui_session.upsert_interaction(interaction).await;
            });
        }
    }

    fn reject_pending_exec_approval(&mut self) {
        if self.pending_managed_interaction.is_some() {
            self.reject_pending_managed_interaction();
            return;
        }

        if let Some(pending) = self.pending_exec_approval.take() {
            let interaction_id = pending.request.call_id.clone();
            tracing::debug!(
                target: "libra::internal::tui::interaction",
                call_id = %interaction_id,
                command = %log_preview_text(&pending.request.command),
                "tui sandbox approval rejected"
            );
            let _ = pending.request.response_tx.send(ReviewDecision::Denied);
            if let Some(code_ui_session) = self.code_ui_session.clone() {
                tokio::spawn(async move {
                    code_ui_session.resolve_interaction(&interaction_id).await;
                    code_ui_session
                        .set_status(CodeUiSessionStatus::ExecutingTool)
                        .await;
                });
            }
        }
        self.widget.bottom_pane.set_exec_approval(None);
        self.widget
            .bottom_pane
            .set_status(AgentStatus::ExecutingTool);
        self.sync_mux_input_context();
        self.schedule_draw();
    }

    fn cancel_pending_exec_approval(&mut self) {
        if self.pending_managed_interaction.is_some() {
            self.reject_pending_managed_interaction();
            return;
        }

        if let Some(pending) = self.pending_exec_approval.take() {
            let interaction_id = pending.request.call_id.clone();
            tracing::debug!(
                target: "libra::internal::tui::interaction",
                call_id = %interaction_id,
                command = %log_preview_text(&pending.request.command),
                "tui sandbox approval cancelled"
            );
            let _ = pending.request.response_tx.send(ReviewDecision::Denied);
            if let Some(code_ui_session) = self.code_ui_session.clone() {
                tokio::spawn(async move {
                    code_ui_session.clear_interaction(&interaction_id).await;
                });
            }
        }
        self.widget.bottom_pane.set_exec_approval(None);
    }

    fn submit_managed_interaction_decision(&mut self) {
        let Some(pending) = self.pending_managed_interaction.take() else {
            return;
        };
        self.respond_to_managed_interaction(pending, false);
    }

    fn reject_pending_managed_interaction(&mut self) {
        let Some(mut pending) = self.pending_managed_interaction.take() else {
            return;
        };
        pending.selected = pending
            .interaction
            .options
            .iter()
            .position(|option| {
                matches!(
                    option.id.as_str(),
                    "decline" | "deny" | "reject" | "decline_all"
                )
            })
            .unwrap_or(pending.selected);
        self.respond_to_managed_interaction(pending, true);
    }

    fn respond_to_managed_interaction(
        &mut self,
        pending: PendingManagedInteraction,
        rejected_by_escape: bool,
    ) {
        let Some(runtime) = self.managed_code_ui_runtime.clone() else {
            return;
        };
        let interaction_id = pending.interaction.id.clone();
        let response =
            code_ui_response_from_managed_selection(&pending.interaction, pending.selected);
        let tx = self.app_event_tx.clone();
        let turn_id = self.active_turn_id.unwrap_or(0);
        tokio::spawn(async move {
            if let Err(error) = runtime
                .adapter()
                .respond_interaction(&interaction_id, response)
                .await
            {
                let _ = tx.send(AppEvent::AgentEvent {
                    turn_id,
                    event: AgentEvent::Error {
                        message: error.to_string(),
                    },
                });
            }
        });

        self.widget.bottom_pane.set_exec_approval(None);
        self.widget
            .bottom_pane
            .set_status(AgentStatus::ExecutingTool);
        self.sync_mux_input_context();
        self.schedule_draw();
        if rejected_by_escape && let Some(code_ui_session) = self.code_ui_session.clone() {
            tokio::spawn(async move {
                code_ui_session
                    .set_status(CodeUiSessionStatus::ExecutingTool)
                    .await;
            });
        }
    }

    fn handle_mouse_event(&mut self, mouse: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.widget.scroll_up_lines(3);
            }
            MouseEventKind::ScrollDown => {
                self.widget.scroll_down_lines(3);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let x = mouse.column;
                let y = mouse.row;
                self.widget.bottom_pane.focused = self.widget.is_in_input_area(x, y);
            }
            _ => {}
        }
    }

    /// Handle an app event.
    async fn handle_app_event(&mut self, event: AppEvent) -> anyhow::Result<()> {
        if !self.is_active_turn(event.turn_id()) {
            return Ok(());
        }

        match event {
            AppEvent::SubmitUserMessage {
                turn_id,
                text,
                source,
                allowed_tools,
            } => {
                let browser_user_entry = CodeUiTranscriptEntry {
                    id: Self::code_ui_user_entry_id(turn_id),
                    kind: CodeUiTranscriptEntryKind::UserMessage,
                    title: Some("Developer".to_string()),
                    content: Some(text.clone()),
                    status: None,
                    streaming: false,
                    metadata: serde_json::json!({
                        "allowedTools": allowed_tools.clone(),
                    }),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                };
                let browser_assistant_entry = CodeUiTranscriptEntry {
                    id: Self::code_ui_assistant_entry_id(turn_id),
                    kind: CodeUiTranscriptEntryKind::AssistantMessage,
                    title: Some("Assistant".to_string()),
                    content: Some(String::new()),
                    status: Some("thinking".to_string()),
                    streaming: true,
                    metadata: serde_json::json!({}),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                };
                // Track in session
                self.running_tool_calls = 0;
                self.session.add_user_message(&text);
                self.save_session_snapshot_after_user_message();

                // Add user cell immediately
                self.widget
                    .add_cell(Box::new(UserHistoryCell::new(text.clone())));

                // Add streaming assistant placeholder (kept as the last cell).
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::streaming()));
                self.widget.bottom_pane.set_status(AgentStatus::Thinking);
                self.sync_mux_input_context();
                self.schedule_draw();
                if let Some(code_ui_session) = self.code_ui_session.clone() {
                    code_ui_session
                        .upsert_transcript_entry(browser_user_entry)
                        .await;
                    code_ui_session
                        .upsert_transcript_entry(browser_assistant_entry)
                        .await;
                    code_ui_session
                        .set_status(CodeUiSessionStatus::Thinking)
                        .await;
                }
                self.clear_mcp_run_id();
                if let Some(run_id_slot) = self.active_turn_run_id.as_ref()
                    && let Ok(mut slot) = run_id_slot.lock()
                {
                    *slot = None;
                }
                if let Some(mcp_server) = self.mcp_server.clone() {
                    let tx = self.app_event_tx.clone();
                    let working_dir = self.registry.working_dir().to_path_buf();
                    let plan_id = self.mcp_plan_id.clone();
                    let mcp_text = text.clone();
                    ClientStorage::spawn_background_index_work(async move {
                        match timeout(
                            MCP_TURN_TRACKING_TIMEOUT,
                            resolve_mcp_turn_tracking(mcp_server, plan_id, working_dir, mcp_text),
                        )
                        .await
                        {
                            Ok(result) => {
                                let _ = tx.send(AppEvent::McpTurnTrackingReady {
                                    turn_id,
                                    run_id: result.run_id,
                                });
                            }
                            Err(_) => {
                                tracing::warn!("MCP turn tracking timed out before agent start");
                            }
                        }
                    });
                }

                if self.managed_code_ui_runtime.is_some() {
                    self.start_managed_code_turn(turn_id, text).await;
                    return Ok(());
                }

                // Prepare components for background task
                let model = self.model.clone();
                let registry = self.registry.clone();
                let mut config = self.config.clone();
                if source == TurnInputSource::Automation {
                    apply_automation_approval_scope(&mut config, turn_id);
                }
                // OC-Phase 3 P3.7 per-turn abort token (v0.17.786):
                // attach a fresh `AbortToken` to the sub-agent
                // runtime via the v0.17.779 `with_abort_token`
                // builder. `cancel_current_turn` cancels this
                // token to short-circuit any in-flight sub-agent
                // dispatch independent of the session-level token,
                // while the session token survives for subsequent
                // turns. No-op when sub-agents are disabled.
                if let Some(rt) = config.subagent_runtime.clone() {
                    let turn_token = crate::internal::ai::agent::runtime::AbortToken::new();
                    self.current_turn_abort_token = Some(turn_token.clone());
                    config.subagent_runtime = Some(rt.with_abort_token(turn_token));
                }
                let session_root = self.session_store.session_root(&self.session.id);
                attach_file_history_context(&mut config, session_root.clone(), turn_id);
                config.context_frame_session_root = Some(session_root.clone());
                config.context_frame_prompt_id = Some(format!("turn-{turn_id}"));
                let active_goal_state = self
                    .goal_session
                    .as_ref()
                    .filter(|session| !session.is_terminal())
                    .map(|session| session.state().clone());
                let should_auto_classify = should_auto_classify_first_user_message(
                    self.auto_classify_first_user_message,
                    &self.history,
                    allowed_tools.as_ref(),
                );
                if self.auto_classify_first_user_message && self.history.is_empty() {
                    self.auto_classify_first_user_message = false;
                }
                let mut turn_allowed_tools = allowed_tools
                    .clone()
                    .unwrap_or_else(|| self.default_allowed_tools.clone());
                apply_goal_tool_visibility(
                    &mut turn_allowed_tools,
                    active_goal_state.is_some(),
                    &registry,
                );
                config.allowed_tools = Some(turn_allowed_tools);
                if active_goal_state.is_some() {
                    ensure_terminal_tool_name(&mut config, "submit_goal_complete");
                }
                let history = self.history.clone();
                let tx = self.app_event_tx.clone();
                let user_text = text;
                let mcp_server = self.mcp_server.clone();
                let run_id = self
                    .active_turn_run_id
                    .clone()
                    .unwrap_or_else(|| Arc::new(Mutex::new(None)));
                let mcp_write_tracker = self.mcp_write_tracker.clone();
                let working_dir = registry.working_dir().to_path_buf();

                // Execute agent call in background task
                let handle = ClientStorage::spawn_background_index_work(async move {
                    struct UiObserver {
                        tx: UnboundedSender<AppEvent>,
                        mcp_server: Option<Arc<LibraMcpServer>>,
                        run_id: Arc<Mutex<Option<String>>>,
                        mcp_write_tracker: McpWriteTracker,
                        recent_tool_results: Arc<Mutex<Vec<RecentToolCall>>>,
                        goal_cost_micro_usd: Arc<AtomicU64>,
                        turn_id: TurnId,
                    }

                    impl ToolLoopObserver for UiObserver {
                        fn on_model_stream_event(&mut self, event: &CompletionStreamEvent) {
                            match event {
                                CompletionStreamEvent::TextDelta { delta, .. }
                                    if !delta.is_empty() =>
                                {
                                    let _ = self.tx.send(AppEvent::AgentEvent {
                                        turn_id: self.turn_id,
                                        event: AgentEvent::ResponseDelta {
                                            delta: delta.clone(),
                                        },
                                    });
                                }
                                CompletionStreamEvent::ThinkingDelta { delta, .. }
                                    if !delta.is_empty() =>
                                {
                                    let _ = self.tx.send(AppEvent::AgentEvent {
                                        turn_id: self.turn_id,
                                        event: AgentEvent::ThinkingDelta {
                                            delta: delta.clone(),
                                        },
                                    });
                                }
                                CompletionStreamEvent::ToolCallPreview {
                                    call_id,
                                    tool_name,
                                    arguments,
                                    ..
                                } => {
                                    let _ = self.tx.send(AppEvent::ToolCallPreview {
                                        turn_id: self.turn_id,
                                        call_id: call_id.clone(),
                                        tool_name: tool_name.clone(),
                                        arguments: arguments.clone(),
                                    });
                                }
                                _ => {}
                            }
                        }

                        fn on_model_usage_recorded(
                            &mut self,
                            usage: &crate::internal::ai::completion::CompletionUsageSummary,
                            wall_clock_ms: u64,
                        ) {
                            let cost = usage_cost_micro_usd(usage);
                            if cost > 0 {
                                self.goal_cost_micro_usd.fetch_add(cost, Ordering::Relaxed);
                            }
                            let _ = self.tx.send(AppEvent::AgentEvent {
                                turn_id: self.turn_id,
                                event: AgentEvent::UsageUpdated {
                                    usage: usage.clone(),
                                    wall_clock_ms,
                                },
                            });
                        }

                        fn on_assistant_step_text(&mut self, text: &str) {
                            let cell = Box::new(AssistantHistoryCell::new(text.to_string()));
                            let _ = self.tx.send(AppEvent::InsertHistoryCell {
                                turn_id: self.turn_id,
                                cell,
                            });
                        }

                        fn on_sub_agent_completed(
                            &mut self,
                            agent_name: &str,
                            usage: &crate::internal::ai::completion::CompletionUsageSummary,
                        ) {
                            // OC-Phase 5 P5.3 per-agent budget
                            // attribution. The dispatcher already
                            // returned the sub-agent's accumulated
                            // usage on the TaskResult; route it
                            // through a dedicated AppEvent so the
                            // budget tracker can call
                            // `accumulate(usage, _, Some(agent))`
                            // and `check_agent` enforces per-agent
                            // caps. The trait's default impl folds
                            // this into the parent's anonymous
                            // bucket via `on_model_usage`, so a
                            // future regression that drops this
                            // override would silently degrade to
                            // session-only enforcement.
                            let _ = self.tx.send(AppEvent::AgentEvent {
                                turn_id: self.turn_id,
                                event: AgentEvent::SubAgentUsageUpdated {
                                    agent_name: agent_name.to_string(),
                                    usage: usage.clone(),
                                },
                            });
                        }

                        fn on_tool_call_begin(
                            &mut self,
                            call_id: &str,
                            tool_name: &str,
                            arguments: &serde_json::Value,
                        ) {
                            let _ = self.tx.send(AppEvent::ToolCallBegin {
                                turn_id: self.turn_id,
                                call_id: call_id.to_string(),
                                tool_name: tool_name.to_string(),
                                arguments: arguments.clone(),
                            });
                        }

                        fn on_tool_call_end(
                            &mut self,
                            call_id: &str,
                            tool_name: &str,
                            result: &Result<ToolOutput, String>,
                        ) {
                            record_recent_goal_tool_result(
                                &self.recent_tool_results,
                                call_id,
                                tool_name,
                                result,
                            );
                            let _ = self.tx.send(AppEvent::ToolCallEnd {
                                turn_id: self.turn_id,
                                call_id: call_id.to_string(),
                                tool_name: tool_name.to_string(),
                                result: result.clone(),
                            });

                            // Record tool invocation via MCP with final status.
                            let run_id = self.run_id.lock().ok().and_then(|slot| slot.clone());
                            if let (Some(mcp_server), Some(run_id)) =
                                (self.mcp_server.clone(), run_id)
                            {
                                let tool_name = tool_name.to_string();
                                let result = result.clone();
                                self.mcp_write_tracker.spawn(async move {
                                    let (status, result_summary) = match &result {
                                        Ok(output) => {
                                            ("ok".to_string(), Some(summarize_tool_output(output)))
                                        }
                                        Err(err) => ("error".to_string(), Some(err.clone())),
                                    };

                                    let invocation_params = CreateToolInvocationParams {
                                        run_id,
                                        tool_name,
                                        status: Some(status),
                                        args_json: None,
                                        io_footprint: None,
                                        result_summary,
                                        artifacts: None,
                                        tags: None,
                                        external_ids: None,
                                        actor_kind: Some("agent".to_string()),
                                        actor_id: Some("libra-agent".to_string()),
                                    };

                                    let actor = match mcp_server.resolve_actor_from_params(
                                        invocation_params.actor_kind.as_deref(),
                                        invocation_params.actor_id.as_deref(),
                                    ) {
                                        Ok(actor) => actor,
                                        Err(e) => {
                                            cli_error!(
                                                e,
                                                "error: failed to resolve actor for tool invocation"
                                            );
                                            return;
                                        }
                                    };

                                    match mcp_server
                                        .create_tool_invocation_impl(invocation_params, actor)
                                        .await
                                    {
                                        Ok(result) => {
                                            if result.is_error.unwrap_or(false) {
                                                render_mcp_error(
                                                    "failed to record tool invocation",
                                                    result.content,
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            cli_error!(
                                                e,
                                                "error: failed to record tool invocation"
                                            );
                                        }
                                    }
                                });
                            }
                        }
                    }

                    let recent_tool_results = Arc::new(Mutex::new(Vec::new()));
                    let goal_cost_micro_usd = Arc::new(AtomicU64::new(0));
                    let mut observer = UiObserver {
                        tx,
                        mcp_server,
                        run_id,
                        mcp_write_tracker,
                        recent_tool_results: recent_tool_results.clone(),
                        goal_cost_micro_usd: goal_cost_micro_usd.clone(),
                        turn_id,
                    };
                    if should_auto_classify {
                        match classify_first_turn_task_intent(
                            &model,
                            &registry,
                            &mut config,
                            &user_text,
                            FirstTurnIntentPolicyUpdate::PromptAndAllowedTools,
                        )
                        .await
                        {
                            Ok(decision) => {
                                if let Some(tools) = config.allowed_tools.as_mut() {
                                    apply_goal_tool_visibility(
                                        tools,
                                        active_goal_state.is_some(),
                                        &registry,
                                    );
                                }
                                send_task_intent_classified_event(
                                    &observer.tx,
                                    observer.turn_id,
                                    decision,
                                    &config,
                                    &registry,
                                    active_goal_state.is_some(),
                                );
                            }
                            Err(error) => {
                                tracing::warn!(
                                    error = %error,
                                    "first-turn task intent classification failed; using launch prompt and tool policy"
                                );
                            }
                        }
                    }
                    attach_memory_anchor_prompt_context(&mut config);
                    let result = if let Some(goal_state) = active_goal_state {
                        let stop_policy =
                            bind_goal_stop_policy(&mut config, goal_state.spec.goal_id);
                        let supervisor = GoalSupervisor {
                            stop_policy,
                            verifier: DeterministicGoalVerifier,
                            prompt_builder: DefaultGoalContinuationPromptBuilder,
                        };
                        let verifier_ctx = WorkspaceGoalVerifierContext {
                            working_dir,
                            recent_tool_results,
                            goal_cost_micro_usd,
                            created_at: goal_state.spec.created_at,
                        };
                        let clock = SystemGoalEventClock;
                        match run_goal_supervised_tool_loop(GoalSupervisedToolLoopRequest {
                            model: &model,
                            history,
                            initial_prompt: user_text,
                            registry: &registry,
                            config,
                            observer: &mut observer,
                            state: goal_state,
                            supervisor: &supervisor,
                            verifier_ctx: &verifier_ctx,
                            clock: &clock,
                        })
                        .await
                        {
                            Ok(run) => {
                                let _ = observer.tx.send(AppEvent::AgentEvent {
                                    turn_id: observer.turn_id,
                                    event: AgentEvent::GoalResponseComplete {
                                        text: run.final_text,
                                        new_history: run.history,
                                        goal_state: Box::new(run.state),
                                        goal_events: run.events,
                                        decision: run.decision,
                                    },
                                });
                                return;
                            }
                            Err(error) => Err(error),
                        }
                    } else {
                        run_tool_loop_with_history_and_observer(
                            &model,
                            history,
                            user_text,
                            &registry,
                            config,
                            &mut observer,
                        )
                        .await
                    };

                    match result {
                        Ok(turn) => {
                            let _ = observer.tx.send(AppEvent::AgentEvent {
                                turn_id: observer.turn_id,
                                event: AgentEvent::ResponseComplete {
                                    text: turn.final_text,
                                    new_history: turn.history,
                                },
                            });
                        }
                        Err(e) => {
                            let _ = observer.tx.send(AppEvent::AgentEvent {
                                turn_id: observer.turn_id,
                                event: AgentEvent::Error {
                                    message: e.to_string(),
                                },
                            });
                        }
                    }
                });

                self.agent_task = Some(handle);
            }
            AppEvent::TaskIntentClassified {
                turn_id: _turn_id,
                intent,
                preamble,
                allowed_tools,
            } => {
                self.config.preamble = Some(preamble);
                self.config.allowed_tools = Some(allowed_tools.clone());
                self.default_allowed_tools = allowed_tools;
                tracing::info!(
                    intent = %intent,
                    "first-turn task intent classification updated TUI prompt and tool policy"
                );
            }
            AppEvent::AgentEvent {
                turn_id: _turn_id,
                event: agent_event,
            } => {
                match agent_event {
                    AgentEvent::SubAgentUsageUpdated { agent_name, usage } => {
                        // OC-Phase 5 P5.3: attribute the sub-agent's
                        // cost to the per-agent bucket so
                        // `check_agent` enforces per-agent caps.
                        // wall_clock_ms = None (the dispatcher does
                        // not yet thread the wall-clock through;
                        // child loop turns each record their own
                        // wall-clock via the parent observer's
                        // `on_model_usage_recorded`).
                        self.budget_tracker
                            .accumulate(&usage, None, Some(agent_name.as_str()));
                        for warning in self.budget_tracker.drain_warnings(&self.agents_config) {
                            self.widget
                                .add_cell(Box::new(AssistantHistoryCell::new(warning.to_string())));
                        }
                        let agent_budget_breach = self
                            .budget_tracker
                            .check_agent(&agent_name, &self.agents_config)
                            .err();
                        if let Some(error) = agent_budget_breach {
                            self.widget
                                .add_cell(Box::new(AssistantHistoryCell::new(error.to_string())));
                            let _ = self.cancel_current_turn(CancelSource::Budget).await;
                        }
                        self.schedule_draw();
                    }
                    AgentEvent::UsageUpdated {
                        usage,
                        wall_clock_ms,
                    } => {
                        self.budget_tracker
                            .accumulate(&usage, Some(wall_clock_ms), None);
                        // OC-Phase 5 budget warnings: surface any axis
                        // that just crossed its `warn_*` threshold via
                        // a one-shot history cell so the operator sees
                        // the alert inline with the conversation. The
                        // tracker's `warnings_emitted` table guarantees
                        // each (scope, axis) fires at most once per
                        // session — back-to-back UsageUpdated events
                        // do not spam.
                        for warning in self.budget_tracker.drain_warnings(&self.agents_config) {
                            self.widget
                                .add_cell(Box::new(AssistantHistoryCell::new(warning.to_string())));
                        }
                        // OC-Phase 5 budget enforcement: any
                        // `max_*_cost_usd` / `max_session_tokens` /
                        // `max_wall_clock_minutes` breach surfaces as
                        // an inline error cell **and** interrupts the
                        // current turn so the model cannot keep
                        // burning budget. `check_session` returns the
                        // first cap that was crossed; `check_goal`
                        // covers the Goal-mode wall-clock / cost cap
                        // independently of the session-wide one.
                        let budget_breach = self
                            .budget_tracker
                            .check_session(&self.agents_config)
                            .err()
                            .or_else(|| self.budget_tracker.check_goal(&self.agents_config).err());
                        if let Some(error) = budget_breach {
                            self.widget
                                .add_cell(Box::new(AssistantHistoryCell::new(error.to_string())));
                            let _ = self.cancel_current_turn(CancelSource::Budget).await;
                        }
                        apply_final_usage_update(
                            &mut self.usage_snapshot,
                            &usage,
                            wall_clock_ms,
                            &mut self.pending_stream_output_tokens,
                        );
                        let usage_badge = format_usage_badge(&self.usage_snapshot);
                        self.widget
                            .bottom_pane
                            .set_usage_line(Some(usage_badge.clone()));
                        self.widget.set_usage_header(Some(usage_badge));
                        self.refresh_usage_detail_panel().await;
                        self.schedule_draw();
                    }
                    AgentEvent::GoalResponseComplete {
                        text,
                        new_history,
                        goal_state,
                        goal_events,
                        decision,
                    } => {
                        self.pending_stream_output_tokens = 0;
                        let rendered_text = render_goal_response_text(&text, &decision);
                        let mcp_decision = match &decision {
                            GoalLoopDecision::Completed { .. } => "Goal completed successfully",
                            GoalLoopDecision::Cancelled => "Goal cancelled",
                            GoalLoopDecision::AwaitUser { .. } => "Goal paused for user input",
                            GoalLoopDecision::Continue { .. } => {
                                "Goal supervisor returned an unexpected continuation"
                            }
                        };
                        self.enqueue_mcp_turn_decision("checkpoint", mcp_decision.to_string());
                        self.finish_turn_state();
                        self.history = new_history;

                        if let Some(session) = self.goal_session.as_mut() {
                            if let Err(error) = session.append_supervisor_events(&goal_events) {
                                let message =
                                    format!("failed to apply Goal supervisor events: {error}");
                                self.complete_streaming_assistant_cell(format!("Error: {message}"));
                                if let Some(code_ui_session) = self.code_ui_session.clone() {
                                    code_ui_session
                                        .upsert_transcript_entry(CodeUiTranscriptEntry {
                                            id: Self::code_ui_assistant_entry_id(_turn_id),
                                            kind: CodeUiTranscriptEntryKind::AssistantMessage,
                                            title: Some("Assistant".to_string()),
                                            content: Some(format!("Error: {message}")),
                                            status: Some("error".to_string()),
                                            streaming: false,
                                            metadata: serde_json::json!({}),
                                            created_at: Utc::now(),
                                            updated_at: Utc::now(),
                                        })
                                        .await;
                                    code_ui_session.set_status(CodeUiSessionStatus::Error).await;
                                }
                                self.set_idle_and_draw();
                                return Ok(());
                            }
                            // The supervisor returns its own projection so callers can
                            // verify the App-side replay stayed aligned.
                            debug_assert_eq!(session.state(), goal_state.as_ref());
                        } else {
                            tracing::warn!(
                                "Goal supervisor completed but the App no longer has an active Goal session"
                            );
                        }

                        if let Err(error) = self.persist_goal_events_to_jsonl(&goal_events) {
                            tracing::warn!(
                                error = %error,
                                "failed to persist Goal supervisor events"
                            );
                        }
                        // Supervisor envelope fold changed the active
                        // Goal's progress / blocker / completion state;
                        // refresh the bottom-pane indicator so the
                        // next frame reflects the new short-line.
                        self.refresh_bottom_pane_goal_status();

                        self.session.add_assistant_message(&rendered_text);
                        self.complete_streaming_assistant_cell(rendered_text.clone());
                        if let Some(code_ui_session) = self.code_ui_session.clone() {
                            let transcript_status = match &decision {
                                GoalLoopDecision::AwaitUser { .. } => "paused",
                                GoalLoopDecision::Completed { .. } => "completed",
                                GoalLoopDecision::Cancelled => "cancelled",
                                GoalLoopDecision::Continue { .. } => "completed",
                            };
                            code_ui_session
                                .upsert_transcript_entry(CodeUiTranscriptEntry {
                                    id: Self::code_ui_assistant_entry_id(_turn_id),
                                    kind: CodeUiTranscriptEntryKind::AssistantMessage,
                                    title: Some("Assistant".to_string()),
                                    content: Some(
                                        self.session
                                            .messages
                                            .last()
                                            .map(|message| message.content.clone())
                                            .unwrap_or_default(),
                                    ),
                                    status: Some(transcript_status.to_string()),
                                    streaming: false,
                                    metadata: serde_json::json!({
                                        "goalStatus": format!("{:?}", goal_state.status),
                                    }),
                                    created_at: Utc::now(),
                                    updated_at: Utc::now(),
                                })
                                .await;
                            code_ui_session.set_status(CodeUiSessionStatus::Idle).await;
                        }
                        self.set_idle_and_draw();
                    }
                    AgentEvent::ResponseComplete { text, new_history } => {
                        self.pending_stream_output_tokens = 0;
                        self.enqueue_mcp_turn_decision(
                            "checkpoint",
                            "Turn completed successfully".to_string(),
                        );
                        self.finish_turn_state();
                        self.history = new_history;

                        // Track in session
                        self.session.add_assistant_message(&text);
                        self.complete_streaming_assistant_cell(text);
                        if let Some(code_ui_session) = self.code_ui_session.clone() {
                            code_ui_session
                                .upsert_transcript_entry(CodeUiTranscriptEntry {
                                    id: Self::code_ui_assistant_entry_id(_turn_id),
                                    kind: CodeUiTranscriptEntryKind::AssistantMessage,
                                    title: Some("Assistant".to_string()),
                                    content: Some(
                                        self.session
                                            .messages
                                            .last()
                                            .map(|message| message.content.clone())
                                            .unwrap_or_default(),
                                    ),
                                    status: Some("completed".to_string()),
                                    streaming: false,
                                    metadata: serde_json::json!({}),
                                    created_at: Utc::now(),
                                    updated_at: Utc::now(),
                                })
                                .await;
                            code_ui_session.set_status(CodeUiSessionStatus::Idle).await;
                        }
                        self.set_idle_and_draw();
                    }
                    AgentEvent::Error { message } => {
                        self.pending_stream_output_tokens = 0;
                        self.pending_auto_plan_repair_execution = None;
                        self.enqueue_mcp_turn_decision(
                            "abandon",
                            format!("Turn failed: {message}"),
                        );
                        self.finish_turn_state();

                        self.complete_streaming_assistant_cell(format!("Error: {}", message));
                        if let Some(code_ui_session) = self.code_ui_session.clone() {
                            code_ui_session
                                .upsert_transcript_entry(CodeUiTranscriptEntry {
                                    id: Self::code_ui_assistant_entry_id(_turn_id),
                                    kind: CodeUiTranscriptEntryKind::AssistantMessage,
                                    title: Some("Assistant".to_string()),
                                    content: Some(format!("Error: {message}")),
                                    status: Some("error".to_string()),
                                    streaming: false,
                                    metadata: serde_json::json!({}),
                                    created_at: Utc::now(),
                                    updated_at: Utc::now(),
                                })
                                .await;
                            code_ui_session.set_status(CodeUiSessionStatus::Error).await;
                        }
                        self.set_idle_and_draw();
                    }
                    AgentEvent::ResponseDelta { delta } => {
                        apply_streaming_usage_delta(
                            &mut self.usage_snapshot,
                            &delta,
                            &mut self.pending_stream_output_tokens,
                        );
                        let usage_badge = format_usage_badge(&self.usage_snapshot);
                        self.widget
                            .bottom_pane
                            .set_usage_line(Some(usage_badge.clone()));
                        self.widget.set_usage_header(Some(usage_badge));
                        self.append_streaming_assistant_delta(&delta);
                        if let Some(code_ui_session) = self.code_ui_session.clone() {
                            code_ui_session
                                .append_assistant_delta(
                                    &Self::code_ui_assistant_entry_id(_turn_id),
                                    &delta,
                                )
                                .await;
                            code_ui_session
                                .set_status(CodeUiSessionStatus::Thinking)
                                .await;
                        }
                        self.refresh_usage_detail_panel().await;
                        self.schedule_draw();
                    }
                    AgentEvent::ThinkingDelta { delta } => {
                        self.append_streaming_thinking_delta(&delta);
                        self.schedule_draw();
                    }
                    AgentEvent::ManagedResponseComplete {
                        text,
                        provider_session_id: _provider_session_id,
                    } => {
                        self.pending_stream_output_tokens = 0;
                        self.enqueue_mcp_turn_decision(
                            "checkpoint",
                            "Turn completed successfully".to_string(),
                        );
                        self.finish_turn_state();
                        self.history.push(Message::assistant(text.clone()));
                        self.session.add_assistant_message(&text);
                        self.complete_streaming_assistant_cell(text);
                        if let Some(code_ui_session) = self.code_ui_session.clone() {
                            code_ui_session
                                .upsert_transcript_entry(CodeUiTranscriptEntry {
                                    id: Self::code_ui_assistant_entry_id(_turn_id),
                                    kind: CodeUiTranscriptEntryKind::AssistantMessage,
                                    title: Some("Assistant".to_string()),
                                    content: self
                                        .session
                                        .messages
                                        .last()
                                        .map(|message| Some(message.content.clone()))
                                        .unwrap_or_else(|| Some(String::new())),
                                    status: Some("completed".to_string()),
                                    streaming: false,
                                    metadata: serde_json::json!({}),
                                    created_at: Utc::now(),
                                    updated_at: Utc::now(),
                                })
                                .await;
                            code_ui_session.set_status(CodeUiSessionStatus::Idle).await;
                        }
                        self.set_idle_and_draw();
                    }
                    AgentEvent::Retrying {
                        attempt,
                        total_attempts,
                        delay_ms,
                        error,
                    } => {
                        let reason = summarize_retry_error(&error);
                        self.widget.bottom_pane.set_retry_notice(format!(
                            "● Retrying request {attempt}/{total_attempts} in {:.1}s ({reason})",
                            delay_ms as f64 / 1000.0
                        ));
                        self.schedule_draw();
                    }
                }
            }
            AppEvent::PlanWorkflowComplete {
                turn_id: _turn_id,
                text,
                llm_output,
                new_history,
                intent_id,
                plan_id,
                persisted_plan_bundle,
                spec_json,
                spec,
                plan,
                plan_draft,
                warnings,
                automatic_repair_attempts,
                automatic_repair_max_attempts,
            } => {
                self.finish_turn_state();
                self.history = new_history;
                self.session.add_assistant_message(&text);
                self.enqueue_llm_context_frame(
                    "step_summary",
                    "Phase 1 plan",
                    intent_id.clone(),
                    plan_id.clone(),
                    text.clone(),
                );
                let browser_plan_steps = plan
                    .tasks
                    .iter()
                    .map(|task| CodeUiPlanStep {
                        step: task.title().to_string(),
                        status: "pending".to_string(),
                    })
                    .collect::<Vec<_>>();
                self.session.metadata.insert(
                    LATEST_INTENTSPEC_JSON.to_string(),
                    serde_json::Value::String(spec_json.clone()),
                );
                let binding = current_intentspec_binding(self.registry.working_dir());
                self.session.metadata.insert(
                    LATEST_INTENTSPEC_WORKSPACE_KEY.to_string(),
                    serde_json::Value::String(binding.workspace_key),
                );
                self.session.metadata.insert(
                    LATEST_INTENTSPEC_BASE_REF.to_string(),
                    serde_json::Value::String(binding.base_ref),
                );
                if let Some(branch_label) = binding.branch_label {
                    self.session.metadata.insert(
                        LATEST_INTENTSPEC_BRANCH_LABEL.to_string(),
                        serde_json::Value::String(branch_label),
                    );
                } else {
                    self.session.metadata.remove(LATEST_INTENTSPEC_BRANCH_LABEL);
                }
                if let Some(ref id) = intent_id {
                    self.session.metadata.insert(
                        LATEST_INTENTSPEC_INTENT_ID.to_string(),
                        serde_json::Value::String(id.clone()),
                    );
                } else {
                    self.session.metadata.remove(LATEST_INTENTSPEC_INTENT_ID);
                }
                if let Some(id) = plan_id.clone() {
                    self.session.metadata.insert(
                        LATEST_EXECUTION_PLAN_ID.to_string(),
                        serde_json::Value::String(id.clone()),
                    );
                    self.mcp_plan_id = Some(id);
                } else {
                    self.session.metadata.remove(LATEST_EXECUTION_PLAN_ID);
                    self.mcp_plan_id = None;
                }

                let execution_plan = (*plan).clone();
                self.widget.show_dag_preview(execution_plan.clone());
                let network_access = matches!(
                    spec.constraints.security.network_policy,
                    NetworkPolicy::Allow
                );
                let plan_summary_cell = Box::new(PlanSummaryHistoryCell::new(
                    *spec,
                    execution_plan.clone(),
                    intent_id.clone(),
                    plan_id.clone(),
                    warnings.clone(),
                ));
                let _ = llm_output;
                self.replace_streaming_assistant_cell(plan_summary_cell);
                if let Some(auto_repair) = self.pending_auto_plan_repair_execution.take() {
                    let note = format!(
                        "Automatic plan repair attempt {} produced a revised plan. Executing it now.",
                        automatic_plan_repair_attempt_label(
                            auto_repair.attempt,
                            auto_repair.max_attempts
                        )
                    );
                    self.widget
                        .add_cell(Box::new(AssistantHistoryCell::new(note.clone())));
                    self.history.push(Message::assistant(note.clone()));
                    self.session.add_assistant_message(&note);
                    if let Some(code_ui_session) = self.code_ui_session.clone() {
                        let plan_snapshot = CodeUiPlanSnapshot {
                            id: plan_id
                                .clone()
                                .unwrap_or_else(|| format!("plan-{_turn_id}")),
                            title: Some("Execution Plan".to_string()),
                            summary: Some(text.clone()),
                            status: "executing".to_string(),
                            steps: browser_plan_steps,
                            updated_at: Utc::now(),
                        };
                        let transcript_entry = CodeUiTranscriptEntry {
                            id: Self::code_ui_assistant_entry_id(_turn_id),
                            kind: CodeUiTranscriptEntryKind::PlanSummary,
                            title: Some("Plan Ready".to_string()),
                            content: Some(text.clone()),
                            status: Some("completed".to_string()),
                            streaming: false,
                            metadata: serde_json::json!({
                                "intentId": intent_id.clone(),
                                "planId": plan_id.clone(),
                                "automaticRepairAttempt": auto_repair.attempt,
                            }),
                            created_at: Utc::now(),
                            updated_at: Utc::now(),
                        };
                        code_ui_session.upsert_plan(plan_snapshot).await;
                        code_ui_session
                            .upsert_transcript_entry(transcript_entry)
                            .await;
                        code_ui_session
                            .set_status(CodeUiSessionStatus::Thinking)
                            .await;
                    }
                    self.start_execute_workflow(ExecuteWorkflowRequest {
                        spec_json,
                        persisted_intent_id: intent_id.clone(),
                        persisted_plan_id: plan_id.clone(),
                        persisted_plan_bundle,
                        approved_plan: Some(execution_plan),
                        approved_plan_draft: Some(plan_draft),
                        plan_warnings: warnings,
                        network_access_override: Some(auto_repair.network_access),
                        automatic_repair_attempts: auto_repair.attempt,
                        automatic_repair_max_attempts: auto_repair.max_attempts,
                    })
                    .await;
                    return Ok(());
                }
                if let Some(code_ui_session) = self.code_ui_session.clone() {
                    let plan_snapshot = CodeUiPlanSnapshot {
                        id: plan_id
                            .clone()
                            .unwrap_or_else(|| format!("plan-{_turn_id}")),
                        title: Some("Execution Plan".to_string()),
                        summary: Some(text.clone()),
                        status: "ready".to_string(),
                        steps: browser_plan_steps,
                        updated_at: Utc::now(),
                    };
                    let transcript_entry = CodeUiTranscriptEntry {
                        id: Self::code_ui_assistant_entry_id(_turn_id),
                        kind: CodeUiTranscriptEntryKind::PlanSummary,
                        title: Some("Plan Ready".to_string()),
                        content: Some(text.clone()),
                        status: Some("ready".to_string()),
                        streaming: false,
                        metadata: serde_json::json!({
                            "intentId": intent_id,
                            "planId": plan_id,
                        }),
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    };
                    let interaction = CodeUiInteractionRequest {
                        id: plan_snapshot.id.clone(),
                        kind: CodeUiInteractionKind::PostPlanChoice,
                        title: Some("Choose next step".to_string()),
                        description: Some(
                            "The plan is ready. Execute it, revise it, or cancel.".to_string(),
                        ),
                        prompt: None,
                        options: vec![
                            CodeUiInteractionOption {
                                id: "execute".to_string(),
                                label: "Execute Plan".to_string(),
                                description: Some(
                                    "Confirm the plan and choose network policy".to_string(),
                                ),
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
                            "intentId": transcript_entry.metadata["intentId"].clone(),
                            "planId": transcript_entry.metadata["planId"].clone(),
                            "networkAccess": network_access,
                        }),
                        requested_at: Utc::now(),
                        resolved_at: None,
                    };
                    code_ui_session.upsert_plan(plan_snapshot).await;
                    code_ui_session
                        .upsert_transcript_entry(transcript_entry)
                        .await;
                    code_ui_session.upsert_interaction(interaction).await;
                    code_ui_session
                        .set_status(CodeUiSessionStatus::AwaitingInteraction)
                        .await;
                }

                // Show post-plan dialog instead of returning to Idle
                self.pending_post_plan = Some(PendingPostPlan {
                    spec_json,
                    intent_id,
                    plan_id,
                    persisted_plan_bundle,
                    plan: execution_plan,
                    plan_draft,
                    warnings,
                    network_access,
                    automatic_repair_attempts,
                    automatic_repair_max_attempts,
                    selected: 0,
                });
                self.widget.bottom_pane.reset_post_plan_selection();
                self.widget
                    .bottom_pane
                    .set_post_plan_network_access(network_access);
                self.widget
                    .bottom_pane
                    .set_status(AgentStatus::AwaitingPostPlanChoice);
                self.sync_mux_input_context();
                self.schedule_draw();
            }
            AppEvent::IntentSpecReviewReady {
                turn_id: _turn_id,
                text,
                llm_output,
                new_history,
                intent_id,
                spec_json,
                warnings,
            } => {
                self.finish_turn_state();
                self.widget.clear_task_mux();
                self.sync_mux_input_context();
                self.history = new_history;
                self.session.add_assistant_message(&text);
                self.enqueue_llm_context_frame(
                    "intent_analysis",
                    "Phase 0 IntentSpec",
                    intent_id.clone(),
                    None,
                    llm_output.clone().unwrap_or_else(|| text.clone()),
                );
                self.session.metadata.insert(
                    LATEST_INTENTSPEC_JSON.to_string(),
                    serde_json::Value::String(spec_json.clone()),
                );
                let binding = current_intentspec_binding(self.registry.working_dir());
                self.session.metadata.insert(
                    LATEST_INTENTSPEC_WORKSPACE_KEY.to_string(),
                    serde_json::Value::String(binding.workspace_key),
                );
                self.session.metadata.insert(
                    LATEST_INTENTSPEC_BASE_REF.to_string(),
                    serde_json::Value::String(binding.base_ref),
                );
                if let Some(branch_label) = binding.branch_label {
                    self.session.metadata.insert(
                        LATEST_INTENTSPEC_BRANCH_LABEL.to_string(),
                        serde_json::Value::String(branch_label),
                    );
                } else {
                    self.session.metadata.remove(LATEST_INTENTSPEC_BRANCH_LABEL);
                }
                if let Some(ref id) = intent_id {
                    self.session.metadata.insert(
                        LATEST_INTENTSPEC_INTENT_ID.to_string(),
                        serde_json::Value::String(id.clone()),
                    );
                } else {
                    self.session.metadata.remove(LATEST_INTENTSPEC_INTENT_ID);
                }
                self.session.metadata.remove(LATEST_EXECUTION_PLAN_ID);
                self.mcp_plan_id = None;

                self.widget.clear_dag_panel();
                if let Some(raw) = llm_output.filter(|text| !text.trim().is_empty()) {
                    self.complete_streaming_assistant_cell(raw);
                    self.widget
                        .add_cell(Box::new(AssistantHistoryCell::new(text.clone())));
                } else {
                    self.replace_streaming_assistant_cell(Box::new(AssistantHistoryCell::new(
                        text.clone(),
                    )));
                }
                let interaction_id = intent_id
                    .clone()
                    .unwrap_or_else(|| format!("intent-review-{_turn_id}"));
                if let Some(code_ui_session) = self.code_ui_session.clone() {
                    let transcript_entry = CodeUiTranscriptEntry {
                        id: Self::code_ui_assistant_entry_id(_turn_id),
                        kind: CodeUiTranscriptEntryKind::AssistantMessage,
                        title: Some("IntentSpec Review".to_string()),
                        content: Some(text.clone()),
                        status: Some("ready".to_string()),
                        streaming: false,
                        metadata: serde_json::json!({
                            "intentId": intent_id,
                        }),
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    };
                    let interaction = CodeUiInteractionRequest {
                        id: interaction_id.clone(),
                        kind: CodeUiInteractionKind::IntentReviewChoice,
                        title: Some("Review IntentSpec".to_string()),
                        description: Some(
                            "Confirm this IntentSpec before Libra generates an execution plan."
                                .to_string(),
                        ),
                        prompt: None,
                        options: vec![
                            CodeUiInteractionOption {
                                id: "confirm".to_string(),
                                label: "Confirm".to_string(),
                                description: Some("Generate the execution plan".to_string()),
                            },
                            CodeUiInteractionOption {
                                id: "modify".to_string(),
                                label: "Modify".to_string(),
                                description: Some("Revise the IntentSpec".to_string()),
                            },
                            CodeUiInteractionOption {
                                id: "cancel".to_string(),
                                label: "Cancel".to_string(),
                                description: Some(
                                    "Leave the IntentSpec in place and stop".to_string(),
                                ),
                            },
                        ],
                        status: CodeUiInteractionStatus::Pending,
                        metadata: serde_json::json!({
                            "intentId": transcript_entry.metadata["intentId"].clone(),
                        }),
                        requested_at: Utc::now(),
                        resolved_at: None,
                    };
                    code_ui_session
                        .upsert_transcript_entry(transcript_entry)
                        .await;
                    code_ui_session.upsert_interaction(interaction).await;
                    code_ui_session
                        .set_status(CodeUiSessionStatus::AwaitingInteraction)
                        .await;
                }

                self.pending_intent_review = Some(PendingIntentReview {
                    spec_json,
                    intent_id,
                    warnings,
                    interaction_id,
                    selected: 0,
                });
                self.widget.bottom_pane.reset_post_plan_selection();
                self.widget
                    .bottom_pane
                    .set_status(AgentStatus::AwaitingIntentReviewChoice);
                self.sync_mux_input_context();
                self.schedule_draw();
            }
            AppEvent::InsertHistoryCell {
                turn_id: _turn_id,
                cell,
            } => {
                if let Some(code_ui_session) = self.code_ui_session.clone() {
                    if let Some(assistant) = cell.as_any().downcast_ref::<AssistantHistoryCell>() {
                        code_ui_session
                            .upsert_transcript_entry(CodeUiTranscriptEntry {
                                id: self.next_code_ui_id("assistant-note"),
                                kind: CodeUiTranscriptEntryKind::AssistantMessage,
                                title: Some("Assistant".to_string()),
                                content: Some(assistant.content.clone()),
                                status: Some(if assistant.is_streaming {
                                    "streaming".to_string()
                                } else {
                                    "completed".to_string()
                                }),
                                streaming: assistant.is_streaming,
                                metadata: serde_json::json!({}),
                                created_at: Utc::now(),
                                updated_at: Utc::now(),
                            })
                            .await;
                    } else if let Some(plan_update) =
                        cell.as_any().downcast_ref::<PlanUpdateHistoryCell>()
                    {
                        code_ui_session
                            .upsert_transcript_entry(CodeUiTranscriptEntry {
                                id: plan_update.call_id.clone(),
                                kind: CodeUiTranscriptEntryKind::PlanSummary,
                                title: Some("Plan Update".to_string()),
                                content: plan_update.explanation.clone(),
                                status: Some(if plan_update.is_running {
                                    "in_progress".to_string()
                                } else {
                                    "completed".to_string()
                                }),
                                streaming: false,
                                metadata: serde_json::json!({
                                    "steps": plan_update.steps.iter().map(|step| serde_json::json!({
                                        "step": step.step,
                                        "status": format!("{:?}", step.status).to_lowercase(),
                                    })).collect::<Vec<_>>(),
                                }),
                                created_at: Utc::now(),
                                updated_at: Utc::now(),
                            })
                            .await;
                    }
                }
                self.insert_before_streaming_assistant(cell);
                self.schedule_draw();
            }
            AppEvent::ManagedInfoNote {
                turn_id: _turn_id,
                message,
            } => {
                if let Some(code_ui_session) = self.code_ui_session.clone() {
                    code_ui_session
                        .upsert_transcript_entry(CodeUiTranscriptEntry {
                            id: self.next_code_ui_id("info-note"),
                            kind: CodeUiTranscriptEntryKind::InfoNote,
                            title: Some("Info".to_string()),
                            content: Some(message.clone()),
                            status: None,
                            streaming: false,
                            metadata: serde_json::json!({}),
                            created_at: Utc::now(),
                            updated_at: Utc::now(),
                        })
                        .await;
                }
                self.insert_before_streaming_assistant(Box::new(AssistantHistoryCell::new(
                    format!("info> {message}"),
                )));
                self.schedule_draw();
            }
            AppEvent::DagGraphBegin {
                turn_id: _turn_id,
                plan,
            } => {
                self.widget.show_dag_panel(plan);
                self.sync_mux_input_context();
                self.schedule_draw();
            }
            AppEvent::DagTaskStatus {
                turn_id: _turn_id,
                task_id,
                status,
            } => {
                self.widget.update_dag_task_status(task_id, status);
                self.schedule_draw();
            }
            AppEvent::DagGraphProgress {
                turn_id: _turn_id,
                completed,
                total,
            } => {
                self.widget.update_dag_progress(completed, total);
                self.schedule_draw();
            }
            AppEvent::DagValidationStatus {
                turn_id: _turn_id,
                passed,
            } => {
                self.widget.update_dag_validation_status(passed);
                self.schedule_draw();
            }
            AppEvent::DagReleaseStatus {
                turn_id: _turn_id,
                passed,
            } => {
                self.widget.update_dag_release_status(passed);
                self.schedule_draw();
            }
            AppEvent::DagTaskMuxClear { turn_id: _turn_id } => {
                self.widget.clear_task_mux();
                self.sync_mux_input_context();
                self.schedule_draw();
            }
            AppEvent::ToolCallBegin {
                turn_id: _turn_id,
                call_id,
                tool_name,
                arguments,
            } => {
                tracing::debug!(
                    target: "libra::internal::tui::app",
                    turn_id = _turn_id,
                    call_id = %call_id,
                    tool = %tool_name,
                    arguments = %log_preview_text(&arguments.to_string()),
                    "tui tool call displayed"
                );
                let tool_summary = Self::tool_call_summary_for_browser(&tool_name, &arguments);
                let already_visible = mark_visible_tool_call_running(
                    &mut self.widget.cells,
                    &call_id,
                    &tool_name,
                    &arguments,
                );
                if !already_visible {
                    if tool_name == "update_plan" {
                        // Parse the plan arguments and render a specialised cell.
                        let (explanation, steps) = parse_update_plan_arguments(&arguments);
                        let cell = Box::new(PlanUpdateHistoryCell::new(
                            call_id.clone(),
                            explanation,
                            steps,
                        ));
                        self.insert_before_streaming_assistant(cell);
                    } else if !append_to_last_tool_group_cell(
                        &mut self.widget.cells,
                        call_id.clone(),
                        tool_name.as_str(),
                        arguments.clone(),
                    ) {
                        let cell = Box::new(ToolCallHistoryCell::new(
                            call_id.clone(),
                            tool_name.clone(),
                            arguments,
                        ));
                        self.insert_before_streaming_assistant(cell);
                    }
                }
                self.running_tool_calls = self.running_tool_calls.saturating_add(1);
                self.update_status_after_tool_progress();
                if let Some(code_ui_session) = self.code_ui_session.clone() {
                    code_ui_session
                        .upsert_tool_call(CodeUiToolCallSnapshot {
                            id: call_id.clone(),
                            tool_name: tool_name.clone(),
                            status: "running".to_string(),
                            summary: Some(tool_summary.clone()),
                            details: None,
                            updated_at: Utc::now(),
                        })
                        .await;
                    code_ui_session
                        .upsert_transcript_entry(CodeUiTranscriptEntry {
                            id: call_id,
                            kind: CodeUiTranscriptEntryKind::ToolCall,
                            title: Some(tool_name),
                            content: Some(tool_summary),
                            status: Some("running".to_string()),
                            streaming: false,
                            metadata: serde_json::json!({}),
                            created_at: Utc::now(),
                            updated_at: Utc::now(),
                        })
                        .await;
                    code_ui_session
                        .set_status(CodeUiSessionStatus::ExecutingTool)
                        .await;
                }
                self.schedule_draw();
            }
            AppEvent::ToolCallPreview {
                turn_id: _turn_id,
                call_id,
                tool_name,
                arguments,
            } => {
                let tool_summary = Self::tool_call_summary_for_browser(&tool_name, &arguments);
                if !update_visible_tool_call_preview(
                    &mut self.widget.cells,
                    &call_id,
                    &tool_name,
                    &arguments,
                ) {
                    if tool_name == "update_plan" {
                        let (explanation, steps) = parse_update_plan_arguments(&arguments);
                        let cell = Box::new(PlanUpdateHistoryCell::new(
                            call_id.clone(),
                            explanation,
                            steps,
                        ));
                        self.insert_before_streaming_assistant(cell);
                    } else if !append_to_last_tool_group_preview_cell(
                        &mut self.widget.cells,
                        call_id.clone(),
                        tool_name.as_str(),
                        arguments.clone(),
                    ) {
                        let cell = Box::new(ToolCallHistoryCell::new_preview(
                            call_id.clone(),
                            tool_name.clone(),
                            arguments,
                        ));
                        self.insert_before_streaming_assistant(cell);
                    }
                }

                if let Some(code_ui_session) = self.code_ui_session.clone() {
                    code_ui_session
                        .upsert_tool_call(CodeUiToolCallSnapshot {
                            id: call_id.clone(),
                            tool_name: tool_name.clone(),
                            status: "preview".to_string(),
                            summary: Some(tool_summary.clone()),
                            details: None,
                            updated_at: Utc::now(),
                        })
                        .await;
                    code_ui_session
                        .upsert_transcript_entry(CodeUiTranscriptEntry {
                            id: call_id,
                            kind: CodeUiTranscriptEntryKind::ToolCall,
                            title: Some(tool_name),
                            content: Some(tool_summary),
                            status: Some("preview".to_string()),
                            streaming: true,
                            metadata: serde_json::json!({}),
                            created_at: Utc::now(),
                            updated_at: Utc::now(),
                        })
                        .await;
                    code_ui_session
                        .set_status(CodeUiSessionStatus::Thinking)
                        .await;
                }
                self.schedule_draw();
            }
            AppEvent::ToolCallEnd {
                turn_id: _turn_id,
                call_id,
                tool_name,
                result,
            } => {
                let result_preview = match &result {
                    Ok(output) => output.log_preview(),
                    Err(error) => error.clone(),
                };
                tracing::debug!(
                    target: "libra::internal::tui::app",
                    turn_id = _turn_id,
                    call_id = %call_id,
                    tool = %tool_name,
                    result = %log_preview_text(&result_preview),
                    "tui tool call result displayed"
                );
                // For successful apply_patch, insert a visual diff cell.
                if tool_name == "apply_patch"
                    && let Ok(ref output) = result
                {
                    self.try_insert_diff_cell(output);
                }

                // Try to find a PlanUpdateHistoryCell first, then fall back to ToolCallHistoryCell.
                let mut found = false;
                for cell in self.widget.cells.iter_mut().rev() {
                    if let Some(plan_cell) =
                        cell.as_any_mut().downcast_mut::<PlanUpdateHistoryCell>()
                        && plan_cell.call_id == call_id
                        && plan_cell.is_running
                    {
                        plan_cell.complete();
                        found = true;
                        break;
                    }
                }
                if !found {
                    let mut pending_result = Some(result.clone());
                    for idx in (0..self.widget.cells.len()).rev() {
                        let Some(tool_cell) = self.widget.cells[idx]
                            .as_any_mut()
                            .downcast_mut::<ToolCallHistoryCell>()
                        else {
                            continue;
                        };
                        if !tool_cell.contains_call_id(&call_id) {
                            continue;
                        }
                        if let Some(result) = pending_result.take() {
                            tool_cell.complete_call(&call_id, result);
                        }
                        break;
                    }
                }
                self.running_tool_calls = self.running_tool_calls.saturating_sub(1);
                self.update_status_after_tool_progress();
                if let Some(code_ui_session) = self.code_ui_session.clone() {
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
                    code_ui_session
                        .upsert_tool_call(CodeUiToolCallSnapshot {
                            id: call_id.clone(),
                            tool_name: tool_name.clone(),
                            status: status.clone(),
                            summary: None,
                            details: details.clone(),
                            updated_at: Utc::now(),
                        })
                        .await;
                    code_ui_session
                        .upsert_transcript_entry(CodeUiTranscriptEntry {
                            id: call_id.clone(),
                            kind: CodeUiTranscriptEntryKind::ToolCall,
                            title: Some(tool_name.clone()),
                            content: details.clone(),
                            status: Some(status.clone()),
                            streaming: false,
                            metadata: serde_json::json!({}),
                            created_at: Utc::now(),
                            updated_at: Utc::now(),
                        })
                        .await;
                    if tool_name == "apply_patch"
                        && let Some(patchset) =
                            Self::patchset_snapshot_for_browser(&call_id, &status, &result)
                    {
                        code_ui_session.upsert_patchset(patchset).await;
                    }
                    code_ui_session
                        .set_status(if self.running_tool_calls > 0 {
                            CodeUiSessionStatus::ExecutingTool
                        } else {
                            CodeUiSessionStatus::Thinking
                        })
                        .await;
                }
                self.schedule_draw();
            }
            AppEvent::AgentStatusUpdate {
                turn_id: _turn_id,
                status,
            } => {
                self.widget.bottom_pane.set_status(status);
                self.sync_mux_input_context();
                if let Some(code_ui_session) = self.code_ui_session.clone() {
                    code_ui_session
                        .set_status(match status {
                            AgentStatus::Idle => CodeUiSessionStatus::Idle,
                            AgentStatus::Thinking | AgentStatus::Retrying => {
                                CodeUiSessionStatus::Thinking
                            }
                            AgentStatus::ExecutingTool => CodeUiSessionStatus::ExecutingTool,
                            AgentStatus::AwaitingUserInput
                            | AgentStatus::AwaitingApproval
                            | AgentStatus::AwaitingPostPlanChoice
                            | AgentStatus::AwaitingNetworkPolicyChoice
                            | AgentStatus::AwaitingIntentReviewChoice => {
                                CodeUiSessionStatus::AwaitingInteraction
                            }
                        })
                        .await;
                }
                self.schedule_draw();
            }
            AppEvent::PhaseConfirmationRequired {
                turn_id: _turn_id,
                prompt,
                response_tx,
            } => {
                tracing::debug!(
                    target: "libra::internal::tui::app",
                    phase = prompt.phase.number(),
                    title = %prompt.title,
                    summary = %log_preview_text(&prompt.summary),
                    "tui phase confirmation displayed"
                );
                self.handle_phase_confirmation_request(prompt, response_tx);
            }
            AppEvent::TaskRuntimeEvent {
                turn_id,
                task_id,
                event,
            } => {
                self.widget.apply_task_runtime_event(task_id, event.clone());
                match event {
                    TaskRuntimeEvent::ToolCallBegin {
                        call_id,
                        tool_name,
                        arguments,
                    } => {
                        let event = AppEvent::ToolCallBegin {
                            turn_id,
                            call_id,
                            tool_name,
                            arguments,
                        };
                        Box::pin(self.handle_app_event(event)).await?;
                        return Ok(());
                    }
                    TaskRuntimeEvent::ToolCallEnd {
                        call_id,
                        tool_name,
                        result,
                    } => {
                        let event = AppEvent::ToolCallEnd {
                            turn_id,
                            call_id,
                            tool_name,
                            result,
                        };
                        Box::pin(self.handle_app_event(event)).await?;
                        return Ok(());
                    }
                    _ => {}
                }
                self.schedule_draw();
            }
            AppEvent::McpTurnTrackingReady {
                turn_id: _turn_id,
                run_id,
            } => {
                self.mcp_run_id = run_id.clone();
                if let Some(run_id_slot) = self.active_turn_run_id.as_ref()
                    && let Ok(mut slot) = run_id_slot.lock()
                {
                    *slot = run_id;
                }
            }
            AppEvent::ExecuteWorkflowComplete {
                turn_id: _turn_id,
                text,
                new_history,
                result,
                spec_json,
                intent_id,
                plan_draft,
                warnings,
                network_access,
                automatic_repair_attempts,
                automatic_repair_max_attempts,
            } => {
                self.finish_turn_state();
                self.widget.clear_task_mux();
                self.sync_mux_input_context();
                self.history = new_history;
                self.session.add_assistant_message(&text);
                let repair_required = execution_requires_plan_repair(result.as_deref());
                let mut repair_plan = result
                    .as_deref()
                    .map(|result| provider_plan_draft_from_plan(&result.execution_plan_spec))
                    .unwrap_or_else(|| plan_draft.clone());
                if repair_plan.steps.is_empty() {
                    repair_plan = plan_draft;
                }
                let failure_report = repair_required
                    .then(|| execution_failure_report(result.as_deref(), Some(text.as_str())));
                let repair_route = repair_required.then(|| {
                    classify_execution_failure_revision(result.as_deref(), Some(text.as_str()))
                });
                let can_auto_repair = repair_route.is_some_and(|route| {
                    should_auto_repair_execution_failure(
                        route,
                        automatic_repair_attempts,
                        automatic_repair_max_attempts,
                    )
                });
                let repair_message = repair_route.map(|route| {
                    repair_message_for_execution_failure(
                        route,
                        failure_report.as_deref().unwrap_or_default(),
                        can_auto_repair,
                        automatic_repair_attempts,
                        automatic_repair_max_attempts,
                    )
                });
                if let Some(result) = result {
                    record_orchestrator_thread_metadata(&mut self.session, &result);
                    self.replace_streaming_assistant_cell(Box::new(
                        OrchestratorResultHistoryCell::new(*result),
                    ));
                } else {
                    self.complete_streaming_assistant_cell(text);
                }
                if let Some(code_ui_session) = self.code_ui_session.clone() {
                    code_ui_session
                        .upsert_transcript_entry(CodeUiTranscriptEntry {
                            id: Self::code_ui_assistant_entry_id(_turn_id),
                            kind: CodeUiTranscriptEntryKind::AssistantMessage,
                            title: Some("Assistant".to_string()),
                            content: Some(
                                self.session
                                    .messages
                                    .last()
                                    .map(|message| message.content.clone())
                                    .unwrap_or_default(),
                            ),
                            status: Some("completed".to_string()),
                            streaming: false,
                            metadata: serde_json::json!({}),
                            created_at: Utc::now(),
                            updated_at: Utc::now(),
                        })
                        .await;
                    code_ui_session.set_status(CodeUiSessionStatus::Idle).await;
                }
                if let Some(message) = repair_message {
                    self.widget
                        .add_cell(Box::new(AssistantHistoryCell::new(message.clone())));
                    self.history.push(Message::assistant(message.clone()));
                    self.session.add_assistant_message(&message);
                    match repair_route {
                        Some(ExecutionFailureRevision::PlanRevision) => {
                            let pending = PendingExecutionPlanRevision {
                                spec_json,
                                intent_id,
                                current_plan: repair_plan,
                                warnings,
                                automatic_repair_attempts,
                                automatic_repair_max_attempts,
                                network_access,
                                failure_report: failure_report.clone(),
                            };
                            if can_auto_repair {
                                let next_attempt = automatic_repair_attempts.saturating_add(1);
                                let request = automatic_plan_repair_request_from_report(
                                    failure_report.as_deref().unwrap_or_default(),
                                    next_attempt,
                                    automatic_repair_max_attempts,
                                );
                                self.begin_automatic_execution_plan_repair(
                                    pending,
                                    request,
                                    next_attempt,
                                    automatic_repair_max_attempts,
                                )
                                .await;
                                return Ok(());
                            }
                            self.pending_execution_plan_revision = Some(pending);
                        }
                        Some(ExecutionFailureRevision::IntentSpecRevision) => {
                            self.pending_plan_revision = Some(spec_json);
                        }
                        Some(ExecutionFailureRevision::ManualAction) | None => {}
                    }
                    self.widget.bottom_pane.set_status(AgentStatus::Idle);
                    self.sync_mux_input_context();
                    self.schedule_draw();
                    return Ok(());
                }
                self.set_idle_and_draw();
            }
        }

        Ok(())
    }

    async fn start_managed_code_turn(&mut self, turn_id: TurnId, text: String) {
        let Some(runtime) = self.managed_code_ui_runtime.clone() else {
            return;
        };
        let baseline = runtime.snapshot().await;
        let baseline_assistant_ids = assistant_entry_ids(&baseline);
        let adapter = runtime.adapter();
        let mut events = runtime.subscribe();
        let tx = self.app_event_tx.clone();

        let handle = ClientStorage::spawn_background_index_work(async move {
            if let Err(error) = adapter.submit_message(text).await {
                let _ = tx.send(AppEvent::AgentEvent {
                    turn_id,
                    event: AgentEvent::Error {
                        message: error.to_string(),
                    },
                });
                return;
            }

            let mut previous_text = String::new();
            let mut saw_activity = false;

            loop {
                let snapshot = match events.recv().await {
                    Ok(event) => match snapshot_from_event(&event) {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            let _ = tx.send(AppEvent::AgentEvent {
                                turn_id,
                                event: AgentEvent::Error {
                                    message: error.to_string(),
                                },
                            });
                            return;
                        }
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        runtime.snapshot().await
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        let _ = tx.send(AppEvent::AgentEvent {
                            turn_id,
                            event: AgentEvent::Error {
                                message: "managed provider session closed".to_string(),
                            },
                        });
                        return;
                    }
                };

                if matches!(
                    snapshot.status,
                    CodeUiSessionStatus::Thinking
                        | CodeUiSessionStatus::ExecutingTool
                        | CodeUiSessionStatus::AwaitingInteraction
                ) {
                    saw_activity = true;
                }

                if let Some(text) =
                    newest_managed_assistant_text(&snapshot, &baseline_assistant_ids)
                {
                    if !text.is_empty() {
                        saw_activity = true;
                    }
                    if let Some(delta) = text.strip_prefix(&previous_text)
                        && !delta.is_empty()
                    {
                        let _ = tx.send(AppEvent::AgentEvent {
                            turn_id,
                            event: AgentEvent::ResponseDelta {
                                delta: delta.to_string(),
                            },
                        });
                    }
                    previous_text = text;
                }

                if saw_activity && managed_snapshot_is_terminal(&snapshot) {
                    let event = if matches!(
                        snapshot.status,
                        CodeUiSessionStatus::Error | CodeUiSessionStatus::IndeterminateSideEffect
                    ) {
                        AgentEvent::Error {
                            message: if previous_text.trim().is_empty() {
                                if snapshot.status == CodeUiSessionStatus::IndeterminateSideEffect {
                                    "managed provider turn reached an indeterminate side-effect boundary; reconcile before retrying".to_string()
                                } else {
                                    "managed provider turn failed".to_string()
                                }
                            } else {
                                previous_text.clone()
                            },
                        }
                    } else {
                        AgentEvent::ManagedResponseComplete {
                            text: previous_text,
                            provider_session_id: snapshot.thread_id.unwrap_or_default(),
                        }
                    };
                    let _ = tx.send(AppEvent::AgentEvent { turn_id, event });
                    return;
                }
            }
        });

        self.agent_task = Some(handle);
    }

    async fn handle_managed_code_ui_event(
        &mut self,
        event: CodeUiEventEnvelope,
    ) -> anyhow::Result<()> {
        let snapshot = snapshot_from_event(&event)?;
        self.sync_managed_interaction(&snapshot);
        Ok(())
    }

    fn sync_managed_interaction(&mut self, snapshot: &CodeUiSessionSnapshot) {
        if self.managed_code_ui_runtime.is_none() {
            return;
        }

        let pending = snapshot
            .interactions
            .iter()
            .find(|interaction| interaction.status == CodeUiInteractionStatus::Pending)
            .cloned();

        if let Some(current) = self.pending_managed_interaction.as_ref() {
            let still_pending = pending
                .as_ref()
                .is_some_and(|interaction| interaction.id == current.interaction.id);
            if still_pending {
                return;
            }
            self.pending_managed_interaction = None;
            self.widget.bottom_pane.set_exec_approval(None);
        }

        if let Some(interaction) = pending {
            self.begin_managed_interaction(interaction);
        } else if matches!(
            self.widget.bottom_pane.status,
            AgentStatus::AwaitingApproval
        ) && self.pending_exec_approval.is_none()
        {
            self.widget
                .bottom_pane
                .set_status(managed_agent_status(snapshot.status));
            self.sync_mux_input_context();
            self.schedule_draw();
        }
    }

    fn begin_managed_interaction(&mut self, mut interaction: CodeUiInteractionRequest) {
        if interaction.options.is_empty() {
            interaction.options = default_managed_interaction_options();
        }

        let options = interaction
            .options
            .iter()
            .map(|option| {
                (
                    option.label.clone(),
                    option.description.clone().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        let title = interaction
            .title
            .clone()
            .unwrap_or_else(|| "Approval required".to_string());
        let command = interaction
            .prompt
            .clone()
            .or_else(|| interaction.description.clone())
            .unwrap_or_else(|| title.clone());
        let cwd = self.registry.working_dir().to_path_buf();
        self.widget.bottom_pane.set_approval_dialog(
            title,
            command,
            cwd,
            interaction.description.clone(),
            false,
            "codex managed runtime".to_string(),
            None,
            vec![self.registry.working_dir().to_path_buf()],
            options,
        );
        self.pending_managed_interaction = Some(PendingManagedInteraction {
            interaction,
            selected: 0,
        });
        self.widget.bottom_pane.exec_approval_selected = 0;
        self.widget
            .bottom_pane
            .set_status(AgentStatus::AwaitingApproval);
        self.sync_mux_input_context();
        self.schedule_draw();
    }

    /// Submit a user message, expanding slash commands and applying agent context.
    async fn submit_message(&mut self, text: String) {
        self.submit_message_from_source(text, TurnInputSource::Local)
            .await;
    }

    async fn submit_message_from_source(&mut self, text: String, source: TurnInputSource) {
        // 1. Check for built-in TUI commands first.
        if let Some((cmd, args)) = super::slash_command::parse_builtin(&text) {
            self.handle_builtin_command(cmd, args).await;
            return;
        }

        if let Some(pending) = self.pending_execution_plan_revision.take() {
            if text.trim_start().starts_with('/') {
                self.pending_execution_plan_revision = Some(pending);
                self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                    pending_execution_plan_revision_help_message(),
                )));
                self.sync_mux_input_context();
                self.schedule_draw();
                return;
            }
            if let PendingPlanRevisionCommand::ContinueAutoRepair { max_attempts } =
                parse_pending_plan_revision_command(&text)
            {
                self.continue_automatic_execution_plan_repair(pending, max_attempts)
                    .await;
                return;
            }
            self.begin_execution_plan_revision_flow(pending, &text)
                .await;
            return;
        }

        if let Some(spec_json) = self.pending_plan_revision.take() {
            if text.trim_start().starts_with('/') {
                self.pending_plan_revision = Some(spec_json);
                self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                    pending_plan_revision_help_message(),
                )));
                self.sync_mux_input_context();
                self.schedule_draw();
                return;
            }
            self.begin_plan_revision_flow(spec_json, &text).await;
            return;
        }

        if self.managed_code_ui_runtime.is_none() && should_route_plain_message_to_plan(&text) {
            self.start_plan_workflow_from_plain_message(&text).await;
            return;
        }

        // 2. Try YAML-defined slash commands (sent to model).
        let (effective_text, agent_name) =
            if let Some(result) = self.command_dispatcher.dispatch(&text) {
                (result.prompt, result.agent)
            } else {
                (text.clone(), None)
            };

        // Agent is only selected via slash command, not auto-detected
        let agent = agent_name
            .as_deref()
            .and_then(|name| self.agent_router.get(name));

        let agent_prompt = agent.map(|a| a.system_prompt.clone());
        let allowed_tools = agent.map(|a| a.tools.clone()).filter(|t| !t.is_empty());

        // If an agent was selected, prepend its system prompt to the user message
        let final_text = if let Some(prompt) = agent_prompt {
            format!("{prompt}\n\n---\n\n{effective_text}")
        } else {
            effective_text
        };

        self.widget.clear_dag_panel();
        self.sync_mux_input_context();
        self.submit_direct_agent_message(final_text, allowed_tools, source);
    }

    fn submit_direct_agent_message(
        &mut self,
        text: String,
        allowed_tools: Option<Vec<String>>,
        source: TurnInputSource,
    ) {
        let turn_id = self.begin_turn();
        let _ = self.app_event_tx.send(AppEvent::SubmitUserMessage {
            turn_id,
            text,
            source,
            allowed_tools,
        });
    }

    /// Handle a built-in TUI command (does not send to model).
    async fn handle_builtin_command(
        &mut self,
        cmd: super::slash_command::BuiltinCommand,
        args: &str,
    ) {
        use super::slash_command::BuiltinCommand;
        match cmd {
            BuiltinCommand::Help => {
                let mut lines = String::from("Available commands:\n");
                // Built-in commands
                for b in BuiltinCommand::all() {
                    lines.push_str(&format!("  /{:<14} {}\n", b.name(), b.description()));
                }
                // YAML-defined commands
                for c in self.command_dispatcher.commands() {
                    lines.push_str(&format!("  /{:<14} {}\n", c.name, c.description));
                }
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(lines)));
            }
            BuiltinCommand::Clear => {
                self.widget.clear();
                self.history.clear();
                self.session = SessionState::new(&self.registry.working_dir().to_string_lossy());
                self.mcp_plan_id = None;
                self.mcp_run_id = None;
                self.pending_plan_revision = None;
                self.pending_execution_plan_revision = None;
                self.pending_auto_plan_repair_execution = None;
                self.sync_mux_input_context();
            }
            BuiltinCommand::Chat => {
                let request = args.trim();
                if request.is_empty() {
                    self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                        "Usage: /chat <your question>".to_string(),
                    )));
                    self.schedule_draw();
                    return;
                }
                self.widget.clear_dag_panel();
                self.sync_mux_input_context();
                self.submit_direct_agent_message(
                    request.to_string(),
                    Some(vec![
                        "read_file".to_string(),
                        "list_dir".to_string(),
                        "grep_files".to_string(),
                        "search_files".to_string(),
                        "web_search".to_string(),
                    ]),
                    TurnInputSource::Local,
                );
            }
            BuiltinCommand::Run => {
                // `/run` mirrors `/chat`'s plan-bypass but defers
                // the allowed-tool decision to the launch-time
                // `--context` / `--agent` selection rather than
                // pinning a read-only set. Passing `None` lets
                // `default_chat_allowed_tools` pick up
                // `config.allowed_tools` (set from
                // `registry.filter_by_intent(...)` at startup).
                // Wave 5 generation matrix uses this so
                // `--context dev` actually exposes `apply_patch`.
                let request = args.trim();
                if request.is_empty() {
                    self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                        "Usage: /run <prompt>".to_string(),
                    )));
                    self.schedule_draw();
                    return;
                }
                self.widget.clear_dag_panel();
                self.sync_mux_input_context();
                self.submit_direct_agent_message(request.to_string(), None, TurnInputSource::Local);
            }
            BuiltinCommand::Model => {
                let info = format!(
                    "Provider: {}\nModel: {}",
                    self.provider_name, self.model_name,
                );
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(info)));
            }
            BuiltinCommand::Status => {
                let status = format!(
                    "Status: {:?}\nHistory: {} messages\nWorking dir: {}",
                    self.widget.bottom_pane.status,
                    self.history.len(),
                    self.registry.working_dir().display(),
                );
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(status)));
            }
            BuiltinCommand::Usage => {
                if usage_detail_popup_enabled(self.registry.working_dir()) {
                    self.usage_detail_args = Some(args.trim().to_string());
                    let details = self.usage_detail_panel_text(args).await;
                    self.widget
                        .bottom_pane
                        .set_usage_detail_panel(Some(details));
                } else {
                    self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                        self.usage_report_text(),
                    )));
                }
                self.schedule_draw();
            }
            BuiltinCommand::Plan => {
                if let Some(pending) = self.pending_execution_plan_revision.take() {
                    match parse_pending_plan_revision_command(args) {
                        PendingPlanRevisionCommand::ContinueAutoRepair { max_attempts } => {
                            self.continue_automatic_execution_plan_repair(pending, max_attempts)
                                .await;
                        }
                        PendingPlanRevisionCommand::Modify(request) => {
                            self.begin_execution_plan_revision_flow(pending, request)
                                .await;
                        }
                        PendingPlanRevisionCommand::Cancel => {
                            self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                                "Execution plan revision canceled.".to_string(),
                            )));
                            self.widget.bottom_pane.set_status(AgentStatus::Idle);
                            self.sync_mux_input_context();
                            self.schedule_draw();
                        }
                        PendingPlanRevisionCommand::Invalid => {
                            self.pending_execution_plan_revision = Some(pending);
                            self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                                pending_execution_plan_revision_help_message(),
                            )));
                            self.sync_mux_input_context();
                            self.schedule_draw();
                        }
                    }
                } else if let Some(spec_json) = self.pending_plan_revision.take() {
                    match parse_pending_plan_revision_command(args) {
                        PendingPlanRevisionCommand::ContinueAutoRepair { .. } => {
                            self.pending_plan_revision = Some(spec_json);
                            self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                                pending_plan_revision_help_message(),
                            )));
                            self.sync_mux_input_context();
                            self.schedule_draw();
                        }
                        PendingPlanRevisionCommand::Modify(request) => {
                            self.begin_plan_revision_flow(spec_json, request).await;
                        }
                        PendingPlanRevisionCommand::Cancel => {
                            self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                                "Spec revision canceled.".to_string(),
                            )));
                            self.widget.bottom_pane.set_status(AgentStatus::Idle);
                            self.sync_mux_input_context();
                            self.schedule_draw();
                        }
                        PendingPlanRevisionCommand::Invalid => {
                            self.pending_plan_revision = Some(spec_json);
                            self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                                pending_plan_revision_help_message(),
                            )));
                            self.sync_mux_input_context();
                            self.schedule_draw();
                        }
                    }
                } else {
                    self.start_plan_workflow(args).await;
                }
            }
            BuiltinCommand::Skill => match self.skill_dispatcher.dispatch(args) {
                Ok(result) => {
                    self.widget.clear_dag_panel();
                    self.sync_mux_input_context();
                    self.submit_direct_agent_message(
                        result.prompt,
                        Some(result.allowed_tools),
                        TurnInputSource::Local,
                    );
                }
                Err(error) => {
                    self.widget
                        .add_cell(Box::new(AssistantHistoryCell::new(error.to_string())));
                    self.schedule_draw();
                }
            },
            BuiltinCommand::Intent => {
                self.handle_intent_command(args).await;
            }
            BuiltinCommand::Mux => {
                let handled = self.handle_mux_command(args);
                if !handled {
                    self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                        "Mux unavailable. Start a parallel workflow first, or use `/mux next|prev|focus <n>|overview|toggle|list` while it is running.".to_string(),
                    )));
                }
                self.schedule_draw();
            }
            BuiltinCommand::Control => {
                if args.trim().eq_ignore_ascii_case("reclaim") {
                    match self.reclaim_local_controller().await {
                        Ok(()) => {
                            self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                                "Local TUI control reclaimed.".to_string(),
                            )));
                        }
                        Err(error) => {
                            self.widget
                                .add_cell(Box::new(AssistantHistoryCell::new(format!(
                                    "Unable to reclaim control: {}",
                                    error.message()
                                ))));
                        }
                    }
                } else {
                    self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                        "Usage: /control reclaim".to_string(),
                    )));
                }
                self.schedule_draw();
            }
            BuiltinCommand::Approvals => {
                let message = self.approvals_command_message(args).await;
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(message)));
                self.sync_mux_input_context();
                self.schedule_draw();
            }
            BuiltinCommand::Anchors => {
                let message = self.anchors_command_message(args);
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(message)));
                self.sync_mux_input_context();
                self.schedule_draw();
            }
            BuiltinCommand::Source => {
                let message = self.source_command_message(args);
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(message)));
                self.sync_mux_input_context();
                self.schedule_draw();
            }
            BuiltinCommand::Undo => {
                let working_dir = self.registry.working_dir();
                let message = if undo_should_prefer_vcs_rollback(working_dir) {
                    "Working tree is clean. Use Libra VCS rollback for committed changes; /undo only reverts uncommitted AI file edits.".to_string()
                } else {
                    let store = self.session_store.file_history(&self.session.id);
                    match store.undo_latest_batch(working_dir) {
                        Ok(report) => format!(
                            "Undid AI file edit batch `{}` ({} path(s) restored).",
                            report.batch_id, report.restored_paths
                        ),
                        Err(FileHistoryError::NoUndoBatch) => {
                            "No AI file edits are available to undo.".to_string()
                        }
                        Err(error) => format!("Unable to undo latest AI file edit batch: {error}"),
                    }
                };
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(message)));
                self.sync_mux_input_context();
                self.schedule_draw();
            }
            BuiltinCommand::Task => {
                let message = self.task_command_message(args).await;
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(message)));
                self.sync_mux_input_context();
                self.schedule_draw();
            }
            BuiltinCommand::Agents => {
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(format_agents_table(
                        &self.agents_config,
                    ))));
            }
            BuiltinCommand::Budget => {
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(format_budget_status(
                        &self.agents_config,
                        &self.budget_tracker,
                        &[],
                    ))));
            }
            BuiltinCommand::Goal => {
                let message = self.format_goal_command_response(args);
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(message)));
            }
            BuiltinCommand::Quit => {
                self.request_user_exit();
            }
        }
    }

    /// Render the response cell for a `/goal …` invocation. The
    /// parser lives in [`super::goal_command::parse_goal_subcommand`];
    /// this helper dispatches the typed result into the App's
    /// goal-session methods (the same methods the Code Control
    /// NDJSON `goal.*` handlers call), so both surfaces share one
    /// authority for state mutation (OC-Phase 6 P6.5 / P6.6). The
    /// `criteria add` arm stays parse-only because `CriteriaRevised`
    /// envelopes flow through the supervisor's apply path —
    /// integration ships once `run_tool_loop` integrates.
    fn format_goal_command_response(&mut self, args: &str) -> String {
        use super::goal_command::{GoalSubcommand, parse_goal_subcommand};
        match parse_goal_subcommand(args) {
            Ok(GoalSubcommand::Start { objective }) => {
                match self.goal_session_start_from_control(objective) {
                    Ok(rendered) => format!("Goal started.\n{rendered}"),
                    Err(err) => format!("`/goal start` failed: {}", err.message()),
                }
            }
            Ok(GoalSubcommand::Status) => match self.goal_session_status_from_control() {
                Ok(rendered) => rendered,
                Err(err) => err.message(),
            },
            Ok(GoalSubcommand::Cancel { reason }) => {
                match self.goal_session_cancel_from_control(reason) {
                    Ok(rendered) => format!("Goal cancelled.\n{rendered}"),
                    Err(err) => format!("`/goal cancel` failed: {}", err.message()),
                }
            }
            Ok(GoalSubcommand::CriteriaAdd { text }) => {
                match self.goal_session_criteria_add_from_control(text) {
                    Ok(rendered) => format!("Goal criteria revised.\n{rendered}"),
                    Err(err) => format!("`/goal criteria add` failed: {}", err.message()),
                }
            }
            Err(err) => err.to_string(),
        }
    }

    /// Dispatch a user-initiated sub-agent from `/task <agent> <prompt>`.
    ///
    /// This is the OC-Phase 3 P3.6 user entry point: it skips the
    /// LLM-initiated permission dialog because the human explicitly
    /// requested the sub-agent, while keeping all other dispatcher
    /// gates (feature flag, depth, concurrency, safety, permission
    /// escalation, model build, child loop) intact.
    async fn task_command_message(&self, args: &str) -> String {
        let invocation = match parse_task_command_args(args) {
            Ok(invocation) => invocation,
            Err(message) => return message,
        };
        self.dispatch_user_task_invocation(invocation).await
    }

    async fn dispatch_user_task_invocation(&self, invocation: TaskInvocation) -> String {
        let Some(runtime) = self.config.subagent_runtime.clone() else {
            return "`/task` requires `code.multi_agent.enabled = true` in `.libra/agents.toml`."
                .to_string();
        };

        let subagent_name = invocation.subagent_type.clone();
        // The `/task` slash command runs outside a turn loop, so there
        // is no per-turn `ToolLoopConfig` carrying a file-history batch.
        // Derive a per-invocation context from the session's base
        // runtime context (sandbox / approval authority) and attach a
        // fresh file-history batch so a child `apply_patch` records undo
        // preimages — matching the per-turn LLM-initiated path
        // (S2-INV-06). Falls back to the runtime's stored snapshot when
        // the session has no base runtime context.
        let user_task_id = uuid::Uuid::new_v4();
        let live_runtime_context = self.config.runtime_context.clone().map(|mut ctx| {
            ctx.file_history = Some(FileHistoryRuntimeContext {
                session_root: self.session_store.session_root(&self.session.id),
                batch_id: format!("user-task-{user_task_id}"),
            });
            ctx
        });
        let ctx =
            runtime.dispatch_context(format!("user-task:{user_task_id}"), live_runtime_context);
        match runtime
            .dispatcher
            .dispatch(
                ctx,
                invocation,
                TaskEntryKind::UserInitiated {
                    bypass_permission_ask: true,
                },
            )
            .await
        {
            Ok(result) => format_task_command_result(result),
            Err(error) => format!("`/task {subagent_name}` failed: {error}"),
        }
    }

    async fn refresh_usage_detail_panel(&mut self) {
        if !self.widget.bottom_pane.is_usage_detail_panel_visible() {
            return;
        }
        let args = self.usage_detail_args.clone().unwrap_or_default();
        let details = self.usage_detail_panel_text(&args).await;
        self.widget
            .bottom_pane
            .set_usage_detail_panel(Some(details));
    }

    async fn usage_detail_panel_text(&self, args: &str) -> String {
        let grouping = match parse_usage_grouping(args) {
            Ok(grouping) => grouping,
            Err(message) => return format!("{}\n\n{message}", self.usage_report_text()),
        };
        let Some(recorder) = self.config.usage_recorder.as_ref() else {
            return self.usage_report_text();
        };
        let filter = UsageQueryFilter {
            session_id: self
                .config
                .usage_context
                .as_ref()
                .and_then(|context| context.session_id.clone()),
            ..Default::default()
        };
        match recorder.query().aggregate_filtered(grouping, &filter).await {
            Ok(rows) => format_usage_detail_panel(
                &self.usage_snapshot,
                usage_grouping_label(grouping),
                &rows,
            ),
            Err(error) => format!(
                "{}\n\nFailed to load SQLite usage details: {error}",
                self.usage_report_text()
            ),
        }
    }

    fn usage_report_text(&self) -> String {
        let total_tokens = self
            .usage_snapshot
            .prompt_tokens
            .saturating_add(self.usage_snapshot.completion_tokens);
        let cost = self
            .usage_snapshot
            .cost_usd
            .map(|cost| format!("\nCost estimate: ${cost:.4}"))
            .unwrap_or_default();
        format!(
            "Usage\nProvider: {}\nModel: {}\nPrompt tokens: {}\nCompletion tokens: {}\nTotal tokens: {}\nWall clock: {:.1}s{}",
            self.usage_snapshot.provider,
            self.usage_snapshot.model,
            self.usage_snapshot.prompt_tokens,
            self.usage_snapshot.completion_tokens,
            total_tokens,
            self.usage_snapshot.wall_clock_ms as f64 / 1000.0,
            cost
        )
    }

    fn source_command_message(&mut self, args: &str) -> String {
        let args = args.trim();
        if args.is_empty() || args.eq_ignore_ascii_case("list") {
            return match self.source_pool.source_statuses() {
                Ok(statuses) => format_source_statuses(&statuses),
                Err(error) => format!("Unable to list sources: {error}"),
            };
        }

        let mut parts = args.split_whitespace();
        let subcommand = parts.next().unwrap_or_default().to_ascii_lowercase();
        let slug = parts.next();
        if parts.next().is_some() {
            return source_command_usage();
        }

        match (subcommand.as_str(), slug) {
            ("enable", Some(slug)) => match self
                .source_pool
                .enable_source(slug, SourceEnablement::SessionExplicit)
            {
                Ok(()) => format!("Enabled source `{slug}` for this session."),
                Err(error) => format!("Unable to enable source `{slug}`: {error}"),
            },
            ("disable", Some(slug)) => match self.source_pool.disable_source(slug) {
                Ok(()) => format!("Disabled source `{slug}` for this session."),
                Err(error) => format!("Unable to disable source `{slug}`: {error}"),
            },
            ("reload", Some(slug)) if slug == BUILTIN_MCP_SOURCE_SLUG => {
                let Some(server) = self.mcp_server.clone() else {
                    return format!("Unable to reload source `{slug}`: MCP server unavailable.");
                };
                match self
                    .source_pool
                    .reload_source(Arc::new(McpSource::builtin(server)))
                {
                    Ok(status) => format!(
                        "Reloaded source `{}`; next source handler build sees {} tool(s).",
                        status.slug, status.tool_count
                    ),
                    Err(error) => format!("Unable to reload source `{slug}`: {error}"),
                }
            }
            ("reload", Some(slug)) => {
                format!("Reload is not available for source `{slug}` in this session.")
            }
            _ => source_command_usage(),
        }
    }

    async fn approvals_command_message(&self, args: &str) -> String {
        let Some(approval) = self
            .config
            .runtime_context
            .as_ref()
            .and_then(|runtime| runtime.approval.as_ref())
            .cloned()
        else {
            return "Approval cache is not available for this session.".to_string();
        };

        let args = args.trim();
        if let Some(prefix) = args.strip_prefix("revoke").map(str::trim) {
            if prefix.is_empty() {
                return "Usage: /approvals revoke <key-prefix> | revoke allow-all <scope>"
                    .to_string();
            }

            // Allow-all decisions live in a separate map and were previously
            // unrevocable from the UI — once `ApprovedForAllCommands` was
            // chosen the cache stayed open for the whole session. The
            // `revoke allow-all <scope>` form clears that record.
            if let Some(scope_arg) = prefix.strip_prefix("allow-all").map(str::trim) {
                if scope_arg.is_empty() {
                    return "Usage: /approvals revoke allow-all <scope>".to_string();
                }
                let mut store = approval.store.lock().await;
                return if store.revoke_allow_all_for_scope(scope_arg) {
                    format!("Revoked allow-all approval for scope `{scope_arg}`.")
                } else {
                    format!("No allow-all approval was active for scope `{scope_arg}`.")
                };
            }

            let mut store = approval.store.lock().await;
            let matches = store
                .active_memos_at(Utc::now())
                .into_iter()
                .filter(|memo| memo.key.starts_with(prefix))
                .collect::<Vec<_>>();

            return match matches.as_slice() {
                [] => format!("No active approval memo matches `{prefix}`."),
                [memo] => {
                    let key = memo.key.clone();
                    if store.revoke(&key) {
                        format!("Revoked approval memo `{key}`.")
                    } else {
                        format!("Approval memo `{key}` was already inactive.")
                    }
                }
                _ => format!(
                    "Approval key prefix `{prefix}` is ambiguous:\n{}",
                    format_approval_memos(&matches)
                ),
            };
        }

        if !args.is_empty() && !args.eq_ignore_ascii_case("list") {
            return "Usage: /approvals [list|revoke <key-prefix>|revoke allow-all <scope>]"
                .to_string();
        }

        let store = approval.store.lock().await;
        let memos = store.active_memos_at(Utc::now());
        let allow_all = store.active_allow_all_scopes();
        match (memos.is_empty(), allow_all.is_empty()) {
            (true, true) => "No active approval memos.".to_string(),
            (false, true) => format!("Active approval memos:\n{}", format_approval_memos(&memos)),
            (true, false) => format!(
                "Active allow-all scopes (use `/approvals revoke allow-all <scope>`):\n{}",
                allow_all
                    .iter()
                    .map(|scope| format!("  {scope}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            (false, false) => format!(
                "Active approval memos:\n{}\nActive allow-all scopes (use `/approvals revoke allow-all <scope>`):\n{}",
                format_approval_memos(&memos),
                allow_all
                    .iter()
                    .map(|scope| format!("  {scope}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        }
    }

    fn anchors_command_message(&self, args: &str) -> String {
        let session_root = self.session_store.session_root(&self.session.id);
        let store = SessionJsonlStore::new(session_root);
        let args = args.trim();
        if args.is_empty() || args.eq_ignore_ascii_case("list") {
            return match store.load_memory_anchors() {
                Ok(replay) => format_memory_anchors(&replay.anchors()),
                Err(error) => format!("Unable to load memory anchors: {error}"),
            };
        }

        if let Some(content) = args.strip_prefix("draft").map(str::trim) {
            if content.is_empty() {
                return "Usage: /anchors draft <memory>".to_string();
            }
            let event = MemoryAnchorEvent::draft(MemoryAnchorDraft::session_user_constraint(
                content, "user",
            ));
            return match store.append(&SessionEvent::memory_anchor(event.clone())) {
                Ok(()) => format!(
                    "Drafted memory anchor `{}`. Use `/anchors confirm {}` to activate it.",
                    short_anchor_id(event.anchor_id),
                    short_anchor_id(event.anchor_id)
                ),
                Err(error) => format!("Unable to draft memory anchor: {error}"),
            };
        }

        if let Some(prefix) = args.strip_prefix("confirm").map(str::trim) {
            if prefix.is_empty() {
                return "Usage: /anchors confirm <id-prefix>".to_string();
            }
            return self.update_anchor_by_prefix(&store, prefix, |anchor| {
                MemoryAnchorEvent::confirm(anchor, Some("confirmed by user".to_string()))
            });
        }

        if let Some(rest) = args.strip_prefix("revoke").map(str::trim) {
            let (prefix, reason) = split_anchor_prefix_and_tail(rest);
            if prefix.is_empty() {
                return "Usage: /anchors revoke <id-prefix> [reason]".to_string();
            }
            let reason = if reason.is_empty() {
                Some("revoked by user".to_string())
            } else {
                Some(reason.to_string())
            };
            return self.update_anchor_by_prefix(&store, prefix, |anchor| {
                MemoryAnchorEvent::revoke(anchor, reason.clone())
            });
        }

        if let Some(rest) = args.strip_prefix("supersede").map(str::trim) {
            let (prefix, content) = split_anchor_prefix_and_tail(rest);
            if prefix.is_empty() || content.is_empty() {
                return "Usage: /anchors supersede <id-prefix> <replacement memory>".to_string();
            }
            let replay = match store.load_memory_anchors() {
                Ok(replay) => replay,
                Err(error) => return format!("Unable to load memory anchors: {error}"),
            };
            let anchor = match replay.find_unique_by_prefix(prefix) {
                Ok(anchor) => anchor,
                Err(error) => return anchor_lookup_message(error),
            };
            let draft = MemoryAnchorEvent::draft(MemoryAnchorDraft::session_user_constraint(
                content, "user",
            ));
            let supersede = MemoryAnchorEvent::supersede(
                &anchor,
                draft.anchor_id,
                Some("superseded by user".to_string()),
            );
            if let Err(error) = store.append(&SessionEvent::memory_anchor(draft.clone())) {
                return format!("Unable to draft replacement memory anchor: {error}");
            }
            if let Err(error) = store.append(&SessionEvent::memory_anchor(supersede)) {
                return format!("Unable to supersede memory anchor: {error}");
            }
            return format!(
                "Superseded `{}` with draft `{}`. Use `/anchors confirm {}` to activate it.",
                anchor.short_id(),
                short_anchor_id(draft.anchor_id),
                short_anchor_id(draft.anchor_id)
            );
        }

        "Usage: /anchors [list|draft <memory>|confirm <id-prefix>|revoke <id-prefix> [reason]|supersede <id-prefix> <replacement memory>]".to_string()
    }

    fn update_anchor_by_prefix(
        &self,
        store: &SessionJsonlStore,
        prefix: &str,
        build_event: impl FnOnce(&MemoryAnchor) -> MemoryAnchorEvent,
    ) -> String {
        let replay = match store.load_memory_anchors() {
            Ok(replay) => replay,
            Err(error) => return format!("Unable to load memory anchors: {error}"),
        };
        let anchor = match replay.find_unique_by_prefix(prefix) {
            Ok(anchor) => anchor,
            Err(error) => return anchor_lookup_message(error),
        };
        let event = build_event(&anchor);
        match store.append(&SessionEvent::memory_anchor(event.clone())) {
            Ok(()) => format!(
                "Memory anchor `{}` is now {}.",
                anchor.short_id(),
                event.review_state
            ),
            Err(error) => format!("Unable to update memory anchor: {error}"),
        }
    }

    fn try_handle_global_quit_command(&mut self) -> bool {
        if !is_global_quit_command_input(&self.widget.bottom_pane.input) {
            return false;
        }
        self.widget.bottom_pane.take_input();
        self.widget.bottom_pane.sync_command_popup();
        self.request_user_exit();
        true
    }

    fn request_user_exit(&mut self) {
        self.cancel_pending_user_input();
        self.cancel_pending_phase_confirmation();
        self.cancel_pending_exec_approval();
        if self.pending_post_plan.is_some() {
            self.dismiss_post_plan_dialog();
        }
        if self.pending_network_policy.is_some() {
            self.dismiss_network_policy_dialog();
        }
        if self.pending_intent_review.is_some() {
            self.dismiss_intent_review_dialog();
        }
        self.pending_execution_plan_revision = None;
        self.pending_plan_revision = None;
        self.interrupt_agent_task();
        self.clear_mcp_run_id();
        self.widget.bottom_pane.clear();
        self.widget.bottom_pane.sync_command_popup();
        self.widget.bottom_pane.set_status(AgentStatus::Idle);
        self.sync_mux_input_context();
        self.exit_info = Some(AppExitInfo {
            reason: ExitReason::UserRequested,
            thread_id: None,
        });
    }

    async fn graph_thread_id_hint(&self) -> Option<String> {
        if let Some(thread_id) = session_graph_thread_id(&self.session) {
            return Some(thread_id);
        }

        if let Some(code_ui_session) = self.code_ui_session.as_ref() {
            let snapshot = code_ui_session.snapshot().await;
            if let Some(thread_id) = snapshot
                .thread_id
                .as_deref()
                .filter(|thread_id| uuid::Uuid::parse_str(thread_id).is_ok())
            {
                return Some(thread_id.to_string());
            }
            if uuid::Uuid::parse_str(&snapshot.session_id).is_ok() {
                return Some(snapshot.session_id);
            }
        }

        let (Some(history), Some(storage)) = (
            self.mcp_server
                .as_ref()
                .and_then(|server| server.intent_history_manager.as_ref()),
            self.mcp_server
                .as_ref()
                .and_then(|server| server.storage.as_ref()),
        ) else {
            return None;
        };

        let rebuilder = ProjectionRebuilder::new(storage.as_ref(), history);
        match rebuilder.rebuild_latest_thread().await {
            Ok(Some(rebuild)) => Some(rebuild.thread.thread_id.to_string()),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(error = %error, "failed to derive graph thread id hint");
                None
            }
        }
    }

    fn handle_mux_command(&mut self, args: &str) -> bool {
        if !self.widget.has_task_mux() {
            return false;
        }

        let trimmed = args.trim();
        let mut parts = trimmed.split_whitespace();
        let command = parts.next().unwrap_or("toggle");
        let handled = match command {
            "" | "toggle" => self.widget.task_mux_toggle_mode(),
            "next" => self.widget.task_mux_next(),
            "prev" => self.widget.task_mux_prev(),
            "overview" => self.widget.task_mux_show_overview(),
            "focus" => {
                if let Some(index_text) = parts.next() {
                    index_text
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| index.checked_sub(1))
                        .is_some_and(|index| self.widget.task_mux_focus_index(index))
                } else {
                    self.widget.task_mux_focus_selected()
                }
            }
            "list" => {
                if let Some(lines) = self.widget.task_mux_list_lines() {
                    self.widget
                        .add_cell(Box::new(AssistantHistoryCell::new(format!(
                            "Task mux panes:\n{}",
                            lines.join("\n")
                        ))));
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if handled {
            self.sync_mux_input_context();
        }
        handled
    }

    async fn create_mcp_exit_decision(&self, reason: &ExitReason) {
        let (Some(mcp_server), Some(run_id)) = (self.mcp_server.clone(), self.mcp_run_id.clone())
        else {
            return;
        };

        let (decision_type, rationale) = match reason {
            ExitReason::UserRequested => ("abandon", "Session ended by user request".to_string()),
            ExitReason::Fatal(message) => (
                "abandon",
                format!("Session ended due to fatal error: {message}"),
            ),
        };

        write_mcp_decision(mcp_server, run_id, decision_type.to_string(), rationale).await;
    }

    fn enqueue_mcp_turn_decision(&self, decision_type: &str, rationale: String) {
        let (Some(mcp_server), Some(run_id)) = (self.mcp_server.clone(), self.mcp_run_id.clone())
        else {
            return;
        };
        let decision_type = decision_type.to_string();
        self.mcp_write_tracker.spawn(async move {
            write_mcp_decision(mcp_server, run_id, decision_type, rationale).await;
        });
    }

    fn enqueue_llm_context_frame(
        &self,
        kind: &'static str,
        phase: &'static str,
        intent_id: Option<String>,
        plan_id: Option<String>,
        text: String,
    ) {
        let Some(mcp_server) = self.mcp_server.clone() else {
            return;
        };
        let summary = format!(
            "{phase} LLM output: {}",
            summarize_llm_output_for_context(&text, 96)
        );
        let output_summary = summarize_llm_output_for_context(&text, 240);
        let content_chars = text.chars().count();
        self.mcp_write_tracker.spawn(async move {
            let params = CreateContextFrameParams {
                kind: kind.to_string(),
                summary,
                intent_id,
                run_id: None,
                plan_id,
                step_id: None,
                data: Some(serde_json::json!({
                    "event": "agent_message",
                    "phase": phase,
                    "summary": output_summary,
                    "contentChars": content_chars,
                    "fullTextStored": false,
                })),
                token_estimate: Some(((content_chars.max(1) as u64) / 4).max(1)),
                actor_kind: Some("agent".to_string()),
                actor_id: Some("libra-planner".to_string()),
            };
            let actor = match mcp_server
                .resolve_actor_from_params(params.actor_kind.as_deref(), params.actor_id.as_deref())
            {
                Ok(actor) => actor,
                Err(error) => {
                    cli_error!(
                        error,
                        "error: failed to resolve actor for LLM context frame"
                    );
                    return;
                }
            };
            if let Err(error) = mcp_server.create_context_frame_impl(params, actor).await {
                cli_error!(error, "error: failed to record LLM context frame");
            }
        });
    }

    fn clear_mcp_run_id(&mut self) {
        self.mcp_run_id = None;
    }

    async fn drain_pending_app_events(&mut self) -> anyhow::Result<()> {
        while let Ok(event) = self.app_event_rx.try_recv() {
            self.handle_app_event(event).await?;
        }
        Ok(())
    }

    fn finish_turn_state(&mut self) {
        self.cancel_pending_phase_confirmation();
        self.cancel_pending_exec_approval();
        self.complete_streaming_thinking_cells();
        self.agent_task = None;
        self.running_tool_calls = 0;
        self.clear_turn_tracking();
    }

    fn save_session_snapshot_after_user_message(&self) {
        if let Err(error) = self.session_store.save(&self.session) {
            tracing::warn!(
                session_id = %self.session.id,
                error = %error,
                "failed to save session snapshot before context-frame recording"
            );
        }
    }

    fn set_idle_and_draw(&mut self) {
        self.widget
            .bottom_pane
            .set_git_branch(current_git_branch_label(self.registry.working_dir()));
        self.widget.bottom_pane.set_status(AgentStatus::Idle);
        self.sync_mux_input_context();
        self.schedule_draw();
        if let Some(code_ui_session) = self.code_ui_session.clone() {
            tokio::spawn(async move {
                code_ui_session.set_status(CodeUiSessionStatus::Idle).await;
            });
        }
    }

    fn sync_mux_input_context(&mut self) {
        let show_mux_context = self.widget.has_task_mux()
            && matches!(
                self.widget.bottom_pane.status,
                AgentStatus::Thinking
                    | AgentStatus::Retrying
                    | AgentStatus::ExecutingTool
                    | AgentStatus::AwaitingApproval
            );
        if show_mux_context {
            self.widget
                .bottom_pane
                .set_input_context_label(self.widget.task_mux_context_label());
            self.widget
                .bottom_pane
                .set_input_hint(self.widget.task_mux_input_hint());
        } else if self.pending_execution_plan_revision.is_some()
            && matches!(self.widget.bottom_pane.status, AgentStatus::Idle)
        {
            self.widget
                .bottom_pane
                .set_input_context_label(Some("Revise Plan".to_string()));
            self.widget.bottom_pane.set_input_hint(Some(
                "Describe plan changes, or use /plan modify <changes> or /plan cancel".to_string(),
            ));
        } else if self.pending_plan_revision.is_some()
            && matches!(self.widget.bottom_pane.status, AgentStatus::Idle)
        {
            self.widget
                .bottom_pane
                .set_input_context_label(Some("Revise IntentSpec".to_string()));
            self.widget.bottom_pane.set_input_hint(Some(
                "Describe spec changes, or use /intent modify <changes> or /intent cancel"
                    .to_string(),
            ));
        } else {
            self.widget.bottom_pane.set_input_context_label(None);
            self.widget.bottom_pane.set_input_hint(None);
        }
    }

    // ── IntentSpec review dialog ────────────────────────────────────

    async fn handle_intent_review_choice(&mut self) {
        let pending = match self.pending_intent_review.take() {
            Some(p) => p,
            None => return,
        };
        let interaction_id = pending.interaction_id.clone();
        let selected = pending.selected;

        match selected {
            0 => {
                self.begin_confirmed_intentspec_plan_workflow(
                    pending.spec_json,
                    pending.intent_id,
                    pending.warnings,
                )
                .await;
            }
            _ => {
                if selected == 1 {
                    self.pending_plan_revision = Some(pending.spec_json);
                    let msg = format!(
                        "{} Your next plain-text message will revise the current IntentSpec.",
                        pending_plan_revision_help_message()
                    );
                    self.widget
                        .add_cell(Box::new(AssistantHistoryCell::new(msg.clone())));
                    self.history.push(Message::assistant(msg.clone()));
                    self.session.add_assistant_message(&msg);
                }
                self.widget.bottom_pane.set_status(AgentStatus::Idle);
                self.sync_mux_input_context();
            }
        }

        if let Some(code_ui_session) = self.code_ui_session.clone() {
            let next_status = if selected == 0 {
                CodeUiSessionStatus::Thinking
            } else {
                CodeUiSessionStatus::Idle
            };
            tokio::spawn(async move {
                code_ui_session.resolve_interaction(&interaction_id).await;
                code_ui_session.set_status(next_status).await;
            });
        }
        self.schedule_draw();
    }

    fn dismiss_intent_review_dialog(&mut self) {
        if let Some(interaction_id) = self
            .pending_intent_review
            .as_ref()
            .map(|pending| pending.interaction_id.clone())
            && let Some(code_ui_session) = self.code_ui_session.clone()
        {
            tokio::spawn(async move {
                code_ui_session.clear_interaction(&interaction_id).await;
            });
        }
        self.pending_intent_review = None;
        self.set_idle_and_draw();
    }

    async fn begin_confirmed_intentspec_plan_workflow(
        &mut self,
        spec_json: String,
        intent_id: Option<String>,
        warnings: Vec<String>,
    ) {
        let prompt = build_execution_plan_prompt(&spec_json);
        self.begin_llm_execution_plan_workflow(
            spec_json,
            intent_id,
            warnings,
            prompt,
            0,
            DEFAULT_AUTOMATIC_PLAN_REPAIR_ATTEMPTS,
        )
        .await;
    }

    async fn begin_automatic_execution_plan_repair(
        &mut self,
        mut pending: PendingExecutionPlanRevision,
        request: String,
        attempt: u8,
        max_attempts: u8,
    ) {
        pending.automatic_repair_attempts = pending.automatic_repair_attempts.max(attempt);
        pending.automatic_repair_max_attempts = max_attempts;
        self.pending_auto_plan_repair_execution = Some(PendingAutoPlanRepairExecution {
            attempt,
            max_attempts,
            network_access: pending.network_access,
        });
        self.begin_execution_plan_revision_flow(pending, &request)
            .await;
    }

    async fn continue_automatic_execution_plan_repair(
        &mut self,
        pending: PendingExecutionPlanRevision,
        requested_max_attempts: Option<u8>,
    ) {
        let Some(failure_report) = pending.failure_report.clone() else {
            self.pending_execution_plan_revision = Some(pending);
            self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                pending_execution_plan_revision_help_message(),
            )));
            self.sync_mux_input_context();
            self.schedule_draw();
            return;
        };

        let next_attempt = pending.automatic_repair_attempts.saturating_add(1);
        if next_attempt > MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS {
            self.pending_execution_plan_revision = Some(pending);
            let message = format!(
                "Automatic plan repair is capped at {} attempts. Describe specific Plan repair guidance, use `/plan modify <changes>`, or use `/plan cancel` to stop.",
                MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS
            );
            self.widget
                .add_cell(Box::new(AssistantHistoryCell::new(message.clone())));
            self.history.push(Message::assistant(message.clone()));
            self.session.add_assistant_message(&message);
            self.sync_mux_input_context();
            self.schedule_draw();
            return;
        }
        let max_attempts = requested_max_attempts
            .unwrap_or(pending.automatic_repair_max_attempts)
            .max(next_attempt)
            .min(MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS);
        let request =
            automatic_plan_repair_request_from_report(&failure_report, next_attempt, max_attempts);
        let mut note = String::new();
        if requested_max_attempts.is_some() && max_attempts != pending.automatic_repair_max_attempts
        {
            note.push_str(&format!(
                "Plan automatic repair retry limit set to {max_attempts}.\n"
            ));
        }
        note.push_str(&automatic_plan_repair_started_message(
            next_attempt,
            max_attempts,
        ));
        self.widget
            .add_cell(Box::new(AssistantHistoryCell::new(note.clone())));
        self.history.push(Message::assistant(note.clone()));
        self.session.add_assistant_message(&note);
        self.begin_automatic_execution_plan_repair(pending, request, next_attempt, max_attempts)
            .await;
    }

    async fn begin_execution_plan_revision_flow(
        &mut self,
        pending: PendingExecutionPlanRevision,
        request: &str,
    ) {
        let request = request.trim();
        if request.is_empty() {
            self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                pending_execution_plan_revision_help_message(),
            )));
            self.pending_execution_plan_revision = Some(pending);
            self.sync_mux_input_context();
            self.schedule_draw();
            return;
        }

        let prompt = build_execution_plan_revision_prompt(
            &pending.spec_json,
            &pending.current_plan,
            request,
            pending.failure_report.as_deref(),
        );
        self.begin_llm_execution_plan_workflow(
            pending.spec_json,
            pending.intent_id,
            pending.warnings,
            prompt,
            pending.automatic_repair_attempts,
            pending.automatic_repair_max_attempts,
        )
        .await;
    }

    async fn begin_llm_execution_plan_workflow(
        &mut self,
        spec_json: String,
        intent_id: Option<String>,
        mut warnings: Vec<String>,
        prompt: String,
        automatic_repair_attempts: u8,
        automatic_repair_max_attempts: u8,
    ) {
        let spec = match serde_json::from_str::<IntentSpec>(&spec_json) {
            Ok(spec) => spec,
            Err(error) => {
                self.pending_auto_plan_repair_execution = None;
                let msg = format!("Plan failed: stored IntentSpec JSON is invalid: {error}");
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(msg.clone())));
                self.history.push(Message::assistant(msg.clone()));
                self.session.add_assistant_message(&msg);
                self.set_idle_and_draw();
                return;
            }
        };

        let turn_id = self.begin_turn();
        self.running_tool_calls = 0;
        self.widget.clear_dag_panel();
        self.widget
            .add_cell(Box::new(AssistantHistoryCell::streaming()));
        self.widget.bottom_pane.set_status(AgentStatus::Thinking);
        self.sync_mux_input_context();
        self.schedule_draw();

        let model = self.model.clone();
        let registry = self.registry.clone();
        let mut config = phase1_plan_tool_loop_config(self.config.clone());
        let session_root = self.session_store.session_root(&self.session.id);
        config.context_frame_session_root = Some(session_root);
        config.context_frame_prompt_id = Some(format!("turn-{turn_id}"));
        let tx = self.app_event_tx.clone();
        let mcp_server = self.mcp_server.clone();
        let fallback_history = self.history.clone();
        let auto_repair_pending = self.pending_auto_plan_repair_execution.is_some();

        let handle = ClientStorage::spawn_background_index_work(async move {
            struct ExecutionPlanObserver {
                tx: UnboundedSender<AppEvent>,
                turn_id: TurnId,
                plan_draft: Option<ProviderPlanDraft>,
            }

            impl ToolLoopObserver for ExecutionPlanObserver {
                fn on_model_stream_event(&mut self, event: &CompletionStreamEvent) {
                    match event {
                        CompletionStreamEvent::TextDelta { delta, .. }
                            if should_forward_phase1_model_text_delta(delta) =>
                        {
                            let _ = self.tx.send(AppEvent::AgentEvent {
                                turn_id: self.turn_id,
                                event: AgentEvent::ResponseDelta {
                                    delta: delta.clone(),
                                },
                            });
                        }
                        CompletionStreamEvent::ThinkingDelta { delta, .. } if !delta.is_empty() => {
                            let _ = self.tx.send(AppEvent::AgentEvent {
                                turn_id: self.turn_id,
                                event: AgentEvent::ThinkingDelta {
                                    delta: delta.clone(),
                                },
                            });
                        }
                        CompletionStreamEvent::ToolCallPreview {
                            call_id,
                            tool_name,
                            arguments,
                            ..
                        } => {
                            if is_phase1_plan_draft_tool(tool_name) {
                                return;
                            }
                            let _ = self.tx.send(AppEvent::ToolCallPreview {
                                turn_id: self.turn_id,
                                call_id: call_id.clone(),
                                tool_name: tool_name.clone(),
                                arguments: arguments.clone(),
                            });
                        }
                        _ => {}
                    }
                }

                fn on_model_usage_recorded(
                    &mut self,
                    usage: &crate::internal::ai::completion::CompletionUsageSummary,
                    wall_clock_ms: u64,
                ) {
                    let _ = self.tx.send(AppEvent::AgentEvent {
                        turn_id: self.turn_id,
                        event: AgentEvent::UsageUpdated {
                            usage: usage.clone(),
                            wall_clock_ms,
                        },
                    });
                }

                fn on_tool_call_begin(
                    &mut self,
                    call_id: &str,
                    tool_name: &str,
                    arguments: &serde_json::Value,
                ) {
                    if is_phase1_plan_draft_tool(tool_name) {
                        if let Ok(args) =
                            serde_json::from_value::<SubmitPlanDraftArgs>(arguments.clone())
                            && let Ok(plan_draft) = provider_plan_draft_from_args(args)
                        {
                            self.plan_draft = Some(plan_draft);
                        }
                        return;
                    }

                    let _ = self.tx.send(AppEvent::ToolCallBegin {
                        turn_id: self.turn_id,
                        call_id: call_id.to_string(),
                        tool_name: tool_name.to_string(),
                        arguments: arguments.clone(),
                    });
                }

                fn on_tool_call_end(
                    &mut self,
                    call_id: &str,
                    tool_name: &str,
                    result: &Result<ToolOutput, String>,
                ) {
                    if is_phase1_plan_draft_tool(tool_name) {
                        return;
                    }
                    let _ = self.tx.send(AppEvent::ToolCallEnd {
                        turn_id: self.turn_id,
                        call_id: call_id.to_string(),
                        tool_name: tool_name.to_string(),
                        result: result.clone(),
                    });
                }
            }

            let mut observer = ExecutionPlanObserver {
                tx: tx.clone(),
                turn_id,
                plan_draft: None,
            };
            attach_memory_anchor_prompt_context(&mut config);
            let run_result = run_tool_loop_with_history_and_observer(
                &model,
                fallback_history.clone(),
                prompt,
                &registry,
                config,
                &mut observer,
            )
            .await;

            let _turn = match run_result {
                Ok(turn) => Some(turn),
                Err(e) if observer.plan_draft.is_some() => {
                    let _ = tx.send(AppEvent::InsertHistoryCell {
                        turn_id,
                        cell: Box::new(AssistantHistoryCell::new(format!(
                            "Planner response failed after plan submission. Continuing with the submitted plan.\nReason: {}",
                            e
                        ))),
                    });
                    None
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::AgentEvent {
                        turn_id,
                        event: AgentEvent::Error {
                            message: format!("Plan failed: LLM did not produce a usable plan: {e}"),
                        },
                    });
                    return;
                }
            };

            let plan_draft = match observer.plan_draft.take() {
                Some(plan_draft) => plan_draft,
                None => {
                    let _ = tx.send(AppEvent::AgentEvent {
                        turn_id,
                        event: AgentEvent::Error {
                            message: "Plan failed: LLM did not call submit_plan_draft.".to_string(),
                        },
                    });
                    return;
                }
            };

            let spec_for_plan = match intentspec_with_plan_draft_objectives(&spec, &plan_draft) {
                Ok(spec) => spec,
                Err(error) => {
                    let _ = tx.send(AppEvent::AgentEvent {
                        turn_id,
                        event: AgentEvent::Error {
                            message: format!("Plan failed: {error}"),
                        },
                    });
                    return;
                }
            };

            let execution_plan = match compile_execution_plan_spec(&spec_for_plan) {
                Ok(plan) => plan,
                Err(e) => {
                    let _ = tx.send(AppEvent::AgentEvent {
                        turn_id,
                        event: AgentEvent::Error {
                            message: format!("Plan failed: cannot compile execution plan: {e}"),
                        },
                    });
                    return;
                }
            };

            let mut summary = render_summary(&spec_for_plan, intent_id.as_deref());
            if let Some(explanation) = plan_draft.explanation.as_deref()
                && !explanation.trim().is_empty()
            {
                summary.push_str(&format!("\nPlan rationale: {}", explanation.trim()));
            }
            let mut plan_warning = None;
            let persisted_plan_bundle = if let (Some(mcp_server), Some(intent_id)) =
                (mcp_server.as_ref(), intent_id.as_ref())
            {
                match crate::internal::ai::orchestrator::persistence::persist_plan_review_bundle(
                    mcp_server,
                    intent_id,
                    &execution_plan,
                )
                .await
                {
                    Ok(bundle) => Some(bundle),
                    Err(e) => {
                        plan_warning = Some(format!(
                            "failed to persist execution plan review bundle: {e}"
                        ));
                        None
                    }
                }
            } else if mcp_server.is_some() {
                plan_warning = Some(
                    "intent persistence unavailable; execution plan not persisted.".to_string(),
                );
                None
            } else {
                plan_warning =
                    Some("MCP server unavailable; execution plan not persisted.".to_string());
                None
            };
            let plan_id = persisted_plan_bundle
                .as_ref()
                .map(|bundle| bundle.plan_id.clone());

            if let Some(ref warn) = plan_warning {
                summary.push_str(&format!("\nWarning: {warn}"));
            }
            if auto_repair_pending {
                summary.push_str(
                    "\n\nAutomatic repair plan ready. Libra will execute this revised plan now.",
                );
            } else {
                summary.push_str("\n\nExecution plan ready. Review the right-side workflow graph and choose Execute Plan / Modify Plan / Cancel below.");
            }

            if let Some(warning) = plan_warning {
                warnings.push(warning);
            }

            let llm_output = None;
            let mut new_history = fallback_history;
            new_history.push(Message::assistant(summary.clone()));

            let _ = tx.send(AppEvent::PlanWorkflowComplete {
                turn_id,
                text: summary,
                llm_output,
                new_history,
                intent_id,
                plan_id,
                persisted_plan_bundle,
                spec_json,
                spec: Box::new(spec_for_plan),
                plan: Box::new(execution_plan),
                plan_draft,
                warnings,
                automatic_repair_attempts,
                automatic_repair_max_attempts,
            });
        });

        self.agent_task = Some(handle);
    }

    // ── Post-plan dialog ────────────────────────────────────────────

    async fn handle_post_plan_choice(&mut self) {
        let pending = match self.pending_post_plan.take() {
            Some(p) => p,
            None => return,
        };
        let interaction_id = pending
            .plan_id
            .clone()
            .unwrap_or_else(|| "post-plan-choice".to_string());
        let selected = pending.selected;

        match selected {
            0 => {
                self.show_network_policy_dialog(pending);
            }
            _ => {
                if pending.selected == 1 {
                    self.pending_execution_plan_revision = Some(PendingExecutionPlanRevision {
                        spec_json: pending.spec_json,
                        intent_id: pending.intent_id.clone(),
                        current_plan: pending.plan_draft,
                        warnings: pending.warnings,
                        automatic_repair_attempts: pending.automatic_repair_attempts,
                        automatic_repair_max_attempts: pending.automatic_repair_max_attempts,
                        network_access: pending.network_access,
                        failure_report: None,
                    });
                    let msg = format!(
                        "{} Your next plain-text message will revise the current execution plan.",
                        pending_execution_plan_revision_help_message()
                    );
                    self.widget
                        .add_cell(Box::new(AssistantHistoryCell::new(msg.clone())));
                    self.history.push(Message::assistant(msg.clone()));
                    self.session.add_assistant_message(&msg);
                }
                self.widget.bottom_pane.set_status(AgentStatus::Idle);
                self.sync_mux_input_context();
            }
        }
        if let Some(code_ui_session) = self.code_ui_session.clone() {
            let next_status = if selected == 0 {
                CodeUiSessionStatus::AwaitingInteraction
            } else {
                CodeUiSessionStatus::Idle
            };
            tokio::spawn(async move {
                code_ui_session.resolve_interaction(&interaction_id).await;
                code_ui_session.set_status(next_status).await;
            });
        }
        self.schedule_draw();
    }

    fn show_network_policy_dialog(&mut self, pending: PendingPostPlan) {
        let selected = if pending.network_access { 1 } else { 0 };
        let interaction_id = network_policy_interaction_id(pending.plan_id.as_deref());
        let intent_id = pending.intent_id.clone();
        let plan_id = pending.plan_id.clone();
        let network_access = pending.network_access;
        self.pending_network_policy = Some(PendingNetworkPolicyChoice {
            post_plan: pending,
            selected,
        });
        self.widget.bottom_pane.post_plan_selected = selected;
        self.widget
            .bottom_pane
            .set_post_plan_network_access(network_access);
        self.widget
            .bottom_pane
            .set_status(AgentStatus::AwaitingNetworkPolicyChoice);
        self.sync_mux_input_context();
        if let Some(code_ui_session) = self.code_ui_session.clone() {
            tokio::spawn(async move {
                code_ui_session
                    .upsert_interaction(CodeUiInteractionRequest {
                        id: interaction_id,
                        kind: CodeUiInteractionKind::PostPlanChoice,
                        title: Some("Choose network policy".to_string()),
                        description: Some(
                            "Select whether shell tools and gates may use the network.".to_string(),
                        ),
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
                            "intentId": intent_id,
                            "planId": plan_id,
                            "networkAccess": network_access,
                            "phase": "networkPolicy",
                        }),
                        requested_at: Utc::now(),
                        resolved_at: None,
                    })
                    .await;
                code_ui_session
                    .set_status(CodeUiSessionStatus::AwaitingInteraction)
                    .await;
            });
        }
    }

    async fn handle_network_policy_choice(&mut self) {
        let pending = match self.pending_network_policy.take() {
            Some(p) => p,
            None => return,
        };
        let selected = pending.selected;
        if selected == 2 {
            self.restore_post_plan_dialog(pending.post_plan);
            self.schedule_draw();
            return;
        }

        let network_access = selected == 1;
        let interaction_id = network_policy_interaction_id(pending.post_plan.plan_id.as_deref());
        let request = Self::execute_request_from_post_plan(pending.post_plan, network_access);
        self.start_execute_workflow(request).await;

        if let Some(code_ui_session) = self.code_ui_session.clone() {
            tokio::spawn(async move {
                code_ui_session.resolve_interaction(&interaction_id).await;
                code_ui_session
                    .set_status(CodeUiSessionStatus::Thinking)
                    .await;
            });
        }
        self.schedule_draw();
    }

    fn return_to_post_plan_dialog(&mut self) {
        let Some(pending) = self.pending_network_policy.take() else {
            return;
        };
        self.restore_post_plan_dialog(pending.post_plan);
        self.schedule_draw();
    }

    fn restore_post_plan_dialog(&mut self, mut pending: PendingPostPlan) {
        pending.selected = 0;
        let interaction_id = network_policy_interaction_id(pending.plan_id.as_deref());
        self.pending_post_plan = Some(pending);
        self.widget.bottom_pane.reset_post_plan_selection();
        self.widget
            .bottom_pane
            .set_status(AgentStatus::AwaitingPostPlanChoice);
        self.sync_mux_input_context();
        if let Some(code_ui_session) = self.code_ui_session.clone() {
            tokio::spawn(async move {
                code_ui_session.clear_interaction(&interaction_id).await;
                code_ui_session
                    .set_status(CodeUiSessionStatus::AwaitingInteraction)
                    .await;
            });
        }
    }

    fn dismiss_post_plan_dialog(&mut self) {
        if let Some(interaction_id) = self
            .pending_post_plan
            .as_ref()
            .and_then(|pending| pending.plan_id.clone())
            && let Some(code_ui_session) = self.code_ui_session.clone()
        {
            tokio::spawn(async move {
                code_ui_session.clear_interaction(&interaction_id).await;
            });
        }
        self.pending_post_plan = None;
        self.set_idle_and_draw();
    }

    fn dismiss_network_policy_dialog(&mut self) {
        if let Some(interaction_id) = self
            .pending_network_policy
            .as_ref()
            .map(|pending| network_policy_interaction_id(pending.post_plan.plan_id.as_deref()))
            && let Some(code_ui_session) = self.code_ui_session.clone()
        {
            tokio::spawn(async move {
                code_ui_session.clear_interaction(&interaction_id).await;
            });
        }
        self.pending_network_policy = None;
        self.set_idle_and_draw();
    }

    fn execute_request_from_post_plan(
        pending: PendingPostPlan,
        network_access: bool,
    ) -> ExecuteWorkflowRequest {
        ExecuteWorkflowRequest {
            spec_json: pending.spec_json,
            persisted_intent_id: pending.intent_id.clone(),
            persisted_plan_id: pending.plan_id.clone(),
            persisted_plan_bundle: pending.persisted_plan_bundle.clone(),
            approved_plan: Some(pending.plan.clone()),
            approved_plan_draft: Some(pending.plan_draft.clone()),
            plan_warnings: pending.warnings.clone(),
            network_access_override: Some(network_access),
            automatic_repair_attempts: pending.automatic_repair_attempts,
            automatic_repair_max_attempts: pending.automatic_repair_max_attempts,
        }
    }

    async fn start_execute_workflow(&mut self, request: ExecuteWorkflowRequest) {
        use crate::internal::ai::{
            intentspec::types::IntentSpec,
            orchestrator::{
                Orchestrator,
                types::{OrchestratorConfig, OrchestratorObserver, PersistedExecution, TaskSpec},
            },
        };

        let ExecuteWorkflowRequest {
            spec_json,
            persisted_intent_id,
            persisted_plan_id,
            persisted_plan_bundle,
            approved_plan,
            approved_plan_draft,
            plan_warnings,
            network_access_override,
            automatic_repair_attempts,
            automatic_repair_max_attempts,
        } = request;

        let mut spec: IntentSpec = match serde_json::from_str(&spec_json) {
            Ok(s) => s,
            Err(e) => {
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(format!(
                        "Failed to parse IntentSpec: {e}"
                    ))));
                self.set_idle_and_draw();
                return;
            }
        };
        if let Some(network_access) = network_access_override {
            apply_developer_network_access(&mut spec, network_access);
        }

        self.widget.clear_dag_panel();
        self.widget
            .add_cell(Box::new(AssistantHistoryCell::streaming()));
        self.widget.bottom_pane.set_status(AgentStatus::Thinking);
        self.sync_mux_input_context();
        self.schedule_draw();
        let turn_id = self.begin_turn();
        self.running_tool_calls = 0;

        let model = self.model.clone();
        let registry = self.registry.clone();
        let mut tool_loop_config = self.config.clone();
        attach_file_history_context(
            &mut tool_loop_config,
            self.session_store.session_root(&self.session.id),
            turn_id,
        );
        let working_dir = self.registry.working_dir().to_path_buf();
        let coder_preamble = self
            .agent_router
            .get("coder")
            .map(|a| a.system_prompt.clone());
        let reviewer_preamble = self
            .agent_router
            .get("code_reviewer")
            .map(|a| a.system_prompt.clone());
        let mcp_server = self.mcp_server.clone();
        let tx = self.app_event_tx.clone();
        let history = self.history.clone();
        let execution_spec_json =
            serde_json::to_string_pretty(&spec).unwrap_or_else(|_| spec_json.clone());
        let execution_intent_id = persisted_intent_id.clone();
        let execution_plan_draft = approved_plan_draft
            .or_else(|| approved_plan.as_ref().map(provider_plan_draft_from_plan))
            .unwrap_or_else(|| provider_plan_draft_from_spec(&spec));
        let execution_network_access = matches!(
            spec.constraints.security.network_policy,
            NetworkPolicy::Allow
        );

        let handle = ClientStorage::spawn_background_index_work(async move {
            struct UiOrchestratorObserver {
                tx: UnboundedSender<AppEvent>,
                turn_id: TurnId,
                task_revisions: Arc<Mutex<HashMap<uuid::Uuid, u32>>>,
                plan_task_counts: Arc<Mutex<HashMap<u32, usize>>>,
                execution_notes_sent: Arc<Mutex<HashSet<u32>>>,
            }

            impl UiOrchestratorObserver {
                fn send_note(&self, text: String) {
                    let _ = self.tx.send(AppEvent::InsertHistoryCell {
                        turn_id: self.turn_id,
                        cell: Box::new(AssistantHistoryCell::new(text)),
                    });
                }

                fn scoped_call_id(task: &TaskSpec, call_id: &str) -> String {
                    format!("{}:{call_id}", task.id())
                }

                fn remember_plan(&self, plan: &ExecutionPlanSpec) {
                    if let Ok(mut task_revisions) = self.task_revisions.lock() {
                        for task in &plan.tasks {
                            task_revisions.insert(task.id(), plan.revision);
                        }
                    }
                    if let Ok(mut plan_task_counts) = self.plan_task_counts.lock() {
                        plan_task_counts.insert(plan.revision, plan.tasks.len());
                    }
                }

                fn send_execution_note_once(&self, task: &TaskSpec) {
                    let revision = self
                        .task_revisions
                        .lock()
                        .ok()
                        .and_then(|task_revisions| task_revisions.get(&task.id()).copied());
                    let Some(revision) = revision else {
                        return;
                    };

                    let should_send = self
                        .execution_notes_sent
                        .lock()
                        .map(|mut sent| sent.insert(revision))
                        .unwrap_or(false);
                    if !should_send {
                        return;
                    }

                    let task_count = self
                        .plan_task_counts
                        .lock()
                        .ok()
                        .and_then(|plan_task_counts| plan_task_counts.get(&revision).copied())
                        .unwrap_or(0);
                    self.send_note(format_plan_execution_stage_note(revision, task_count));
                }
            }

            impl OrchestratorObserver for UiOrchestratorObserver {
                fn on_plan_compiled(&self, plan: &ExecutionPlanSpec) {
                    self.remember_plan(plan);
                    let _ = self.tx.send(AppEvent::DagGraphBegin {
                        turn_id: self.turn_id,
                        plan: plan.clone(),
                    });
                    self.send_note(format_plan_compiled_stage_note(plan));
                }

                fn on_task_runtime_event(&self, task: &TaskSpec, event: TaskRuntimeEvent) {
                    match &event {
                        TaskRuntimeEvent::Phase(TaskRuntimePhase::Starting) => {
                            self.send_execution_note_once(task);
                            let _ = self.tx.send(AppEvent::AgentStatusUpdate {
                                turn_id: self.turn_id,
                                status: AgentStatus::Thinking,
                            });
                            let _ = self.tx.send(AppEvent::DagTaskStatus {
                                turn_id: self.turn_id,
                                task_id: task.id(),
                                status: TaskNodeStatus::Running,
                            });
                        }
                        TaskRuntimeEvent::Phase(TaskRuntimePhase::Completed) => {
                            let _ = self.tx.send(AppEvent::DagTaskStatus {
                                turn_id: self.turn_id,
                                task_id: task.id(),
                                status: TaskNodeStatus::Completed,
                            });
                        }
                        TaskRuntimeEvent::Phase(TaskRuntimePhase::Failed) => {
                            let _ = self.tx.send(AppEvent::DagTaskStatus {
                                turn_id: self.turn_id,
                                task_id: task.id(),
                                status: TaskNodeStatus::Failed,
                            });
                        }
                        TaskRuntimeEvent::WorkspaceReady {
                            working_dir,
                            isolated,
                            backend,
                            main_working_dir,
                        } => {
                            self.send_note(format_task_workspace_note(
                                task.title(),
                                working_dir,
                                *isolated,
                                *backend,
                                main_working_dir.as_deref(),
                            ));
                        }
                        TaskRuntimeEvent::Note { level, text } => {
                            let title = match level {
                                TaskRuntimeNoteLevel::Info => format!("Task · {}", task.title()),
                                TaskRuntimeNoteLevel::Error => {
                                    format!("Task Failed · {}", task.title())
                                }
                            };
                            self.send_note(format!("{title}  \n{text}"));
                        }
                        TaskRuntimeEvent::ThinkingDelta(delta) if !delta.is_empty() => {
                            let _ = self.tx.send(AppEvent::AgentEvent {
                                turn_id: self.turn_id,
                                event: AgentEvent::ThinkingDelta {
                                    delta: delta.clone(),
                                },
                            });
                        }
                        TaskRuntimeEvent::UsageUpdated {
                            usage,
                            wall_clock_ms,
                        } => {
                            let _ = self.tx.send(AppEvent::AgentEvent {
                                turn_id: self.turn_id,
                                event: AgentEvent::UsageUpdated {
                                    usage: usage.clone(),
                                    wall_clock_ms: *wall_clock_ms,
                                },
                            });
                        }
                        _ => {}
                    }

                    let mux_event = match event {
                        TaskRuntimeEvent::ToolCallBegin {
                            call_id,
                            tool_name,
                            arguments,
                        } => TaskRuntimeEvent::ToolCallBegin {
                            call_id: Self::scoped_call_id(task, &call_id),
                            tool_name,
                            arguments,
                        },
                        TaskRuntimeEvent::ToolCallEnd {
                            call_id,
                            tool_name,
                            result,
                        } => TaskRuntimeEvent::ToolCallEnd {
                            call_id: Self::scoped_call_id(task, &call_id),
                            tool_name,
                            result,
                        },
                        other => other,
                    };

                    let _ = self.tx.send(AppEvent::TaskRuntimeEvent {
                        turn_id: self.turn_id,
                        task_id: task.id(),
                        event: mux_event,
                    });
                }

                fn on_graph_progress(&self, completed: usize, total: usize) {
                    let _ = self.tx.send(AppEvent::DagGraphProgress {
                        turn_id: self.turn_id,
                        completed,
                        total,
                    });
                }

                fn on_graph_checkpoint_saved(
                    &self,
                    _checkpoint_id: &str,
                    _pc: usize,
                    _completed_nodes: usize,
                ) {
                }

                fn on_graph_checkpoint_restored(&self, _checkpoint_id: &str, _pc: usize) {}

                fn on_system_verification(&self, plan: &ExecutionPlanSpec, report: &SystemReport) {
                    let _ = self.tx.send(AppEvent::DagTaskMuxClear {
                        turn_id: self.turn_id,
                    });
                    // The right-side workflow graph tracks whether Phase 3 ran.
                    // Pass/fail details remain in the verification summary.
                    let _ = self.tx.send(AppEvent::DagValidationStatus {
                        turn_id: self.turn_id,
                        passed: true,
                    });
                    self.send_note(format_system_verification_stage_note(plan, report));
                }

                fn on_decision(&self, plan: &ExecutionPlanSpec, decision: &DecisionOutcome) {
                    // The release row represents Phase 4 completion, not whether
                    // the decision was Commit, HumanReviewRequired, or Abandon.
                    let _ = self.tx.send(AppEvent::DagReleaseStatus {
                        turn_id: self.turn_id,
                        passed: true,
                    });
                    self.send_note(format_decision_stage_note(plan, decision));
                }

                fn on_replan(
                    &self,
                    current_revision: u32,
                    next_revision: u32,
                    reason: &str,
                    diff_summary: &str,
                ) {
                    let _ = self.tx.send(AppEvent::DagTaskMuxClear {
                        turn_id: self.turn_id,
                    });
                    self.send_note(format_replan_stage_note(
                        current_revision,
                        next_revision,
                        reason,
                        diff_summary,
                    ));
                }

                fn on_persistence_complete(&self, _execution: &PersistedExecution) {}
            }

            let observer: Arc<dyn OrchestratorObserver> = Arc::new(UiOrchestratorObserver {
                tx: tx.clone(),
                turn_id,
                task_revisions: Arc::new(Mutex::new(HashMap::new())),
                plan_task_counts: Arc::new(Mutex::new(HashMap::new())),
                execution_notes_sent: Arc::new(Mutex::new(HashSet::new())),
            });
            struct TuiPhaseConfirmer {
                tx: UnboundedSender<AppEvent>,
                turn_id: TurnId,
            }

            #[async_trait]
            impl OrchestratorPhaseConfirmer for TuiPhaseConfirmer {
                async fn confirm(
                    &self,
                    prompt: PhaseConfirmationPrompt,
                ) -> PhaseConfirmationDecision {
                    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                    if self
                        .tx
                        .send(AppEvent::PhaseConfirmationRequired {
                            turn_id: self.turn_id,
                            prompt,
                            response_tx,
                        })
                        .is_err()
                    {
                        return PhaseConfirmationDecision::Abort;
                    }
                    response_rx
                        .await
                        .unwrap_or(PhaseConfirmationDecision::Abort)
                }
            }

            let config = OrchestratorConfig {
                working_dir,
                base_commit: None,
                persisted_intent_id,
                persisted_plan_bundle,
                persisted_plan_id,
                initial_plan: approved_plan,
                dagrs_resume_checkpoint_id: None,
                tool_loop_config,
                coder_preamble,
                reviewer_preamble,
                mcp_server,
                observer: Some(observer),
                phase_confirmer: Some(Arc::new(TuiPhaseConfirmer {
                    tx: tx.clone(),
                    turn_id,
                })),
            };
            let orchestrator = Orchestrator::new(model, registry, config);

            let result = orchestrator.run(spec).await;

            let (summary, ui_result) = match &result {
                Ok(r) => (format_orchestrator_result(r), Some(Box::new(r.clone()))),
                Err(e) => (format!("Orchestrator failed: {e}"), None),
            };

            let mut new_history = history;
            new_history.push(Message::assistant(summary.clone()));

            let _ = tx.send(AppEvent::ExecuteWorkflowComplete {
                turn_id,
                text: summary,
                new_history,
                result: ui_result,
                spec_json: execution_spec_json,
                intent_id: execution_intent_id,
                plan_draft: execution_plan_draft,
                warnings: plan_warnings,
                network_access: execution_network_access,
                automatic_repair_attempts,
                automatic_repair_max_attempts,
            });
        });

        self.agent_task = Some(handle);
    }

    async fn start_plan_workflow(&mut self, request: &str) {
        let request = request.trim();
        let user_text = format!("/plan {request}");
        self.start_plan_workflow_with_user_text(request, user_text)
            .await;
    }

    async fn start_plan_workflow_from_plain_message(&mut self, request: &str) {
        self.start_plan_workflow_with_user_text(request, request.trim().to_string())
            .await;
    }

    async fn start_plan_workflow_with_user_text(&mut self, request: &str, user_text: String) {
        let request = request.trim();
        if request.is_empty() {
            self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                "Usage: /plan <your requirement>".to_string(),
            )));
            self.schedule_draw();
            return;
        }

        let prompt_body = build_plan_prompt(request);
        let prompt = if let Some(agent) = self.agent_router.get("planner") {
            format!("{}\n\n---\n\n{}", agent.system_prompt, prompt_body)
        } else {
            prompt_body
        };
        self.begin_plan_workflow(user_text, prompt).await;
    }

    async fn begin_plan_revision_flow(&mut self, spec_json: String, request: &str) {
        let request = request.trim();
        if request.is_empty() {
            self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                pending_plan_revision_help_message(),
            )));
            self.pending_plan_revision = Some(spec_json);
            self.sync_mux_input_context();
            self.schedule_draw();
            return;
        }

        let prompt_body = build_plan_revision_prompt(&spec_json, request);
        let prompt = if let Some(agent) = self.agent_router.get("planner") {
            format!("{}\n\n---\n\n{}", agent.system_prompt, prompt_body)
        } else {
            prompt_body
        };
        let user_text = format!("Modify current spec: {request}");
        self.begin_plan_workflow(user_text, prompt).await;
    }

    async fn begin_plan_workflow(&mut self, user_text: String, prompt: String) {
        self.pending_plan_revision = None;
        let turn_id = self.begin_turn();
        self.running_tool_calls = 0;
        self.session.add_user_message(&user_text);
        self.save_session_snapshot_after_user_message();
        self.widget
            .add_cell(Box::new(UserHistoryCell::new(user_text.clone())));
        self.widget.clear_dag_panel();
        self.sync_mux_input_context();
        self.widget
            .add_cell(Box::new(AssistantHistoryCell::streaming()));
        self.widget.bottom_pane.set_status(AgentStatus::Thinking);
        self.sync_mux_input_context();
        self.schedule_draw();

        let model = self.model.clone();
        let registry = self.registry.clone();
        let mut config = phase0_plan_tool_loop_config(self.config.clone());
        let session_root = self.session_store.session_root(&self.session.id);
        attach_file_history_context(&mut config, session_root.clone(), turn_id);
        config.context_frame_session_root = Some(session_root.clone());
        config.context_frame_prompt_id = Some(format!("turn-{turn_id}"));
        let should_auto_classify = should_auto_classify_first_user_message(
            self.auto_classify_first_user_message,
            &self.history,
            None,
        );
        if self.auto_classify_first_user_message && self.history.is_empty() {
            self.auto_classify_first_user_message = false;
        }
        let history = self.history.clone();
        let tx = self.app_event_tx.clone();
        let mcp_server = self.mcp_server.clone();
        let working_dir = self.registry.working_dir().to_path_buf();
        let default_network_access = self.default_network_access;
        let classification_text = user_text.clone();

        let handle = ClientStorage::spawn_background_index_work(async move {
            struct PlanObserver {
                tx: UnboundedSender<AppEvent>,
                turn_id: TurnId,
                draft: Option<IntentDraft>,
                risk_prompted: bool,
                selected_risk: Option<RiskLevel>,
            }

            impl PlanObserver {
                fn new(tx: UnboundedSender<AppEvent>, turn_id: TurnId) -> Self {
                    Self {
                        tx,
                        turn_id,
                        draft: None,
                        risk_prompted: false,
                        selected_risk: None,
                    }
                }
            }

            impl ToolLoopObserver for PlanObserver {
                fn on_model_stream_event(&mut self, event: &CompletionStreamEvent) {
                    match event {
                        CompletionStreamEvent::TextDelta { delta, .. }
                            if should_forward_phase0_model_text_delta(delta) =>
                        {
                            let _ = self.tx.send(AppEvent::AgentEvent {
                                turn_id: self.turn_id,
                                event: AgentEvent::ResponseDelta {
                                    delta: delta.clone(),
                                },
                            });
                        }
                        CompletionStreamEvent::ThinkingDelta { delta, .. } if !delta.is_empty() => {
                            let _ = self.tx.send(AppEvent::AgentEvent {
                                turn_id: self.turn_id,
                                event: AgentEvent::ThinkingDelta {
                                    delta: delta.clone(),
                                },
                            });
                        }
                        CompletionStreamEvent::ToolCallPreview {
                            call_id,
                            tool_name,
                            arguments,
                            ..
                        } => {
                            let _ = self.tx.send(AppEvent::ToolCallPreview {
                                turn_id: self.turn_id,
                                call_id: call_id.clone(),
                                tool_name: tool_name.clone(),
                                arguments: arguments.clone(),
                            });
                        }
                        _ => {}
                    }
                }

                fn on_model_usage_recorded(
                    &mut self,
                    usage: &crate::internal::ai::completion::CompletionUsageSummary,
                    wall_clock_ms: u64,
                ) {
                    let _ = self.tx.send(AppEvent::AgentEvent {
                        turn_id: self.turn_id,
                        event: AgentEvent::UsageUpdated {
                            usage: usage.clone(),
                            wall_clock_ms,
                        },
                    });
                }

                fn on_tool_call_begin(
                    &mut self,
                    call_id: &str,
                    tool_name: &str,
                    arguments: &serde_json::Value,
                ) {
                    let _ = self.tx.send(AppEvent::ToolCallBegin {
                        turn_id: self.turn_id,
                        call_id: call_id.to_string(),
                        tool_name: tool_name.to_string(),
                        arguments: arguments.clone(),
                    });

                    if tool_name == "request_user_input"
                        && let Ok(req) =
                            parse_value_or_json_string::<RequestUserInputArgs>(arguments)
                        && req
                            .questions
                            .iter()
                            .any(|q| q.id.trim().eq_ignore_ascii_case("risk_profile"))
                    {
                        self.risk_prompted = true;
                    }

                    if tool_name == "submit_intent_draft"
                        && let Ok(args) = parse_submit_intent_draft_value(arguments)
                    {
                        self.draft = Some(args.draft);
                    }
                }

                fn on_tool_call_end(
                    &mut self,
                    call_id: &str,
                    tool_name: &str,
                    result: &Result<ToolOutput, String>,
                ) {
                    let _ = self.tx.send(AppEvent::ToolCallEnd {
                        turn_id: self.turn_id,
                        call_id: call_id.to_string(),
                        tool_name: tool_name.to_string(),
                        result: result.clone(),
                    });

                    if tool_name == "request_user_input"
                        && let Ok(output) = result
                        && let Some(content) = output.as_text()
                        && let Ok(resp) = serde_json::from_str::<UserInputResponse>(content)
                        && let Some(level) = extract_risk_level_from_response(&resp)
                    {
                        self.selected_risk = Some(level);
                    }
                }
            }

            let mut observer = PlanObserver::new(tx.clone(), turn_id);
            if should_auto_classify {
                match classify_first_turn_task_intent(
                    &model,
                    &registry,
                    &mut config,
                    &classification_text,
                    FirstTurnIntentPolicyUpdate::PromptOnly,
                )
                .await
                {
                    Ok(decision) => {
                        send_task_intent_classified_event(
                            &tx, turn_id, decision, &config, &registry, false,
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "first-turn task intent classification failed; using launch prompt and plan tool policy"
                        );
                    }
                }
            }
            attach_memory_anchor_prompt_context(&mut config);
            let fallback_history = history.clone();
            let run_result = run_tool_loop_with_history_and_observer(
                &model,
                history,
                prompt,
                &registry,
                config.clone(),
                &mut observer,
            )
            .await;

            let turn = match run_result {
                Ok(turn) => Some(turn),
                Err(e) => {
                    if observer.risk_prompted
                        && observer.selected_risk.is_some()
                        && observer.draft.is_some()
                    {
                        let _ = tx.send(AppEvent::InsertHistoryCell {
                            turn_id,
                            cell: Box::new(AssistantHistoryCell::new(format!(
                                "Planner response failed after draft submission. Continuing with the submitted draft.\nReason: {}",
                                e
                            ))),
                        });
                        None
                    } else {
                        let _ = tx.send(AppEvent::AgentEvent {
                            turn_id,
                            event: AgentEvent::Error {
                                message: e.to_string(),
                            },
                        });
                        return;
                    }
                }
            };

            if !observer.risk_prompted {
                let _ = tx.send(AppEvent::AgentEvent {
                    turn_id,
                    event: AgentEvent::Error {
                        message: "Plan failed: planner did not ask for risk profile.".to_string(),
                    },
                });
                return;
            }

            let risk_level = match observer.selected_risk.clone() {
                Some(level) => level,
                None => {
                    let _ = tx.send(AppEvent::AgentEvent {
                        turn_id,
                        event: AgentEvent::Error {
                            message: "Plan failed: risk profile was not selected.".to_string(),
                        },
                    });
                    return;
                }
            };

            let draft = match observer.draft.take() {
                Some(d) => d,
                None => {
                    let _ = tx.send(AppEvent::AgentEvent {
                        turn_id,
                        event: AgentEvent::Error {
                            message: "Plan failed: no intent draft was submitted.".to_string(),
                        },
                    });
                    return;
                }
            };

            let mut spec = resolve_intentspec(
                draft,
                risk_level,
                ResolveContext {
                    working_dir: canonical_working_dir_label(&working_dir),
                    base_ref: current_head_sha(&working_dir),
                    created_by_id: "tui-user".to_string(),
                },
            );
            apply_developer_network_access(&mut spec, default_network_access);

            let mut issues = validate_intentspec(&spec);
            for _ in 0..MAX_INTENTSPEC_REPAIR_ATTEMPTS {
                if issues.is_empty() {
                    break;
                }
                repair_intentspec(&mut spec, &issues);
                issues = validate_intentspec(&spec);
            }

            if !issues.is_empty() {
                let report = issues
                    .iter()
                    .map(|i| format!("- {}: {}", i.path, i.message))
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = tx.send(AppEvent::AgentEvent {
                    turn_id,
                    event: AgentEvent::Error {
                        message: format!(
                            "Plan failed after automatic repair.\nValidation issues:\n{}",
                            report
                        ),
                    },
                });
                return;
            }

            let mut persistence_warning = None;
            let intent_id = if let Some(ref mcp_server) = mcp_server {
                match persist_phase0_intent_for_review(&spec, mcp_server).await {
                    Ok(intent_id) => Some(intent_id),
                    Err(error) => {
                        persistence_warning =
                            Some(format!("failed to persist intent into MCP: {error}"));
                        None
                    }
                }
            } else {
                persistence_warning =
                    Some("MCP server unavailable; intent not persisted.".to_string());
                None
            };
            let mut context_snapshot_id = None;
            let mut context_snapshot_warning = None;
            if let Some(ref mcp_server) = mcp_server {
                match persist_phase0_context_snapshot_for_review(&spec, &working_dir, mcp_server)
                    .await
                {
                    Ok(snapshot_id) => context_snapshot_id = snapshot_id,
                    Err(error) => {
                        context_snapshot_warning = Some(format!(
                            "failed to persist Phase 0 ContextSnapshot: {error}"
                        ));
                    }
                }
            }

            let pretty_json =
                serde_json::to_string_pretty(&spec).unwrap_or_else(|_| "{}".to_string());
            let warnings = persistence_warning
                .into_iter()
                .chain(context_snapshot_warning)
                .collect::<Vec<_>>();
            let review = build_intentspec_review(
                &spec,
                intent_id.as_deref(),
                context_snapshot_id.as_deref(),
                &warnings,
            );

            let llm_output = turn
                .as_ref()
                .map(|turn| turn.final_text.clone())
                .filter(|text| !text.trim().is_empty());
            let mut new_history = turn.map(|turn| turn.history).unwrap_or(fallback_history);
            new_history.push(Message::assistant(review.clone()));

            let _ = tx.send(AppEvent::IntentSpecReviewReady {
                turn_id,
                text: review,
                llm_output,
                new_history,
                intent_id,
                spec_json: pretty_json,
                warnings,
            });
        });

        self.agent_task = Some(handle);
    }

    async fn handle_intent_command(&mut self, args: &str) {
        let trimmed = args.trim();
        let (command, rest) = trimmed
            .split_once(char::is_whitespace)
            .unwrap_or((trimmed, ""));
        match command.to_ascii_lowercase().as_str() {
            "show" => {
                let rendered = match self.load_latest_intentspec_json().await {
                    LatestIntentSpecLoad::Found(json) => json,
                    LatestIntentSpecLoad::Missing => {
                        "No IntentSpec found. Run `/plan <requirement>` first.".to_string()
                    }
                    LatestIntentSpecLoad::BindingMismatch(message) => message,
                };
                self.widget
                    .add_cell(Box::new(AssistantHistoryCell::new(rendered)));
                self.schedule_draw();
            }
            "execute" => match self.load_latest_intentspec_json().await {
                LatestIntentSpecLoad::Found(spec_json) => {
                    self.start_execute_workflow(ExecuteWorkflowRequest {
                        spec_json,
                        persisted_intent_id: self
                            .session
                            .metadata
                            .get(LATEST_INTENTSPEC_INTENT_ID)
                            .and_then(serde_json::Value::as_str)
                            .map(ToString::to_string),
                        persisted_plan_id: self
                            .session
                            .metadata
                            .get(LATEST_EXECUTION_PLAN_ID)
                            .and_then(serde_json::Value::as_str)
                            .map(ToString::to_string),
                        persisted_plan_bundle: None,
                        approved_plan: None,
                        approved_plan_draft: None,
                        plan_warnings: Vec::new(),
                        network_access_override: Some(self.default_network_access),
                        automatic_repair_attempts: 0,
                        automatic_repair_max_attempts: DEFAULT_AUTOMATIC_PLAN_REPAIR_ATTEMPTS,
                    })
                    .await;
                }
                LatestIntentSpecLoad::Missing => {
                    self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                        "No IntentSpec found. Run `/plan <requirement>` first.".to_string(),
                    )));
                    self.schedule_draw();
                }
                LatestIntentSpecLoad::BindingMismatch(message) => {
                    self.widget
                        .add_cell(Box::new(AssistantHistoryCell::new(message)));
                    self.schedule_draw();
                }
            },
            "modify" | "revise" => {
                let request = rest.trim();
                if request.is_empty() {
                    self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                        pending_plan_revision_help_message(),
                    )));
                    self.sync_mux_input_context();
                    self.schedule_draw();
                    return;
                }

                let spec_json = match self.pending_plan_revision.take() {
                    Some(spec_json) => spec_json,
                    None => match self.load_latest_intentspec_json().await {
                        LatestIntentSpecLoad::Found(json) => json,
                        LatestIntentSpecLoad::Missing => {
                            self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                                "No IntentSpec found. Run `/plan <requirement>` first.".to_string(),
                            )));
                            self.schedule_draw();
                            return;
                        }
                        LatestIntentSpecLoad::BindingMismatch(message) => {
                            self.widget
                                .add_cell(Box::new(AssistantHistoryCell::new(message)));
                            self.schedule_draw();
                            return;
                        }
                    },
                };

                self.begin_plan_revision_flow(spec_json, request).await;
            }
            "cancel" => {
                if self.pending_plan_revision.take().is_some() {
                    self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                        "Spec revision canceled.".to_string(),
                    )));
                    self.widget.bottom_pane.set_status(AgentStatus::Idle);
                    self.sync_mux_input_context();
                } else {
                    self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                        "No IntentSpec revision is active.".to_string(),
                    )));
                }
                self.schedule_draw();
            }
            _ => {
                self.widget.add_cell(Box::new(AssistantHistoryCell::new(
                    "Usage: /intent show|execute|modify <changes>|cancel".to_string(),
                )));
                self.schedule_draw();
            }
        }
    }

    async fn load_latest_intentspec_json(&self) -> LatestIntentSpecLoad {
        let binding = current_intentspec_binding(self.registry.working_dir());
        let binding_matches = latest_intentspec_binding_matches(&self.session.metadata, &binding);
        if !binding_matches {
            return LatestIntentSpecLoad::BindingMismatch(
                latest_intentspec_binding_mismatch_message(&self.session.metadata, &binding)
                    .unwrap_or_else(|| {
                        "Latest IntentSpec belongs to a different workspace or HEAD.".to_string()
                    }),
            );
        }

        if let Some(json_text) = self
            .session
            .metadata
            .get(LATEST_INTENTSPEC_JSON)
            .and_then(|v| v.as_str())
            && let Ok(spec) = serde_json::from_str::<IntentSpec>(json_text)
        {
            if intentspec_matches_workspace(&spec, &binding.workspace_key, &binding.base_ref) {
                return LatestIntentSpecLoad::Found(json_text.to_string());
            }
            return LatestIntentSpecLoad::BindingMismatch(format_intentspec_target_mismatch(
                spec.metadata.target.repo.locator.as_str(),
                &spec.metadata.target.base_ref,
                self.session
                    .metadata
                    .get(LATEST_INTENTSPEC_BRANCH_LABEL)
                    .and_then(|value| value.as_str()),
                &binding,
            ));
        }

        let Some(mcp) = self.mcp_server.clone() else {
            return LatestIntentSpecLoad::Missing;
        };
        if let Some(id) = self
            .session
            .metadata
            .get(LATEST_INTENTSPEC_INTENT_ID)
            .and_then(|v| v.as_str())
            && let Some(spec) = fetch_intentspec_from_object_id(&mcp, id).await
        {
            if intentspec_matches_workspace(&spec, &binding.workspace_key, &binding.base_ref) {
                return serde_json::to_string_pretty(&spec)
                    .map(LatestIntentSpecLoad::Found)
                    .unwrap_or(LatestIntentSpecLoad::Missing);
            }
            return LatestIntentSpecLoad::BindingMismatch(format_intentspec_target_mismatch(
                spec.metadata.target.repo.locator.as_str(),
                &spec.metadata.target.base_ref,
                self.session
                    .metadata
                    .get(LATEST_INTENTSPEC_BRANCH_LABEL)
                    .and_then(|value| value.as_str()),
                &binding,
            ));
        }

        LatestIntentSpecLoad::Missing
    }

    fn interrupt_agent_task(&mut self) {
        // v0.17.786: cancel the per-turn sub-agent abort token
        // BEFORE aborting the JoinHandle so cooperative-cancel
        // paths (the runner's `tokio::select!`) get the signal
        // even when the JoinHandle drop alone wouldn't unwind a
        // detached worker future.
        if let Some(token) = self.current_turn_abort_token.take() {
            token.cancel();
        }
        if let Some(handle) = self.agent_task.take() {
            handle.abort();
        }
        self.pending_auto_plan_repair_execution = None;
        self.complete_streaming_thinking_cells();
        self.clear_active_turn();
        self.running_tool_calls = 0;
    }

    fn update_status_after_tool_progress(&mut self) {
        let next_status = if self.pending_intent_review.is_some() {
            AgentStatus::AwaitingIntentReviewChoice
        } else if self.pending_network_policy.is_some() {
            AgentStatus::AwaitingNetworkPolicyChoice
        } else if self.pending_post_plan.is_some() {
            AgentStatus::AwaitingPostPlanChoice
        } else if self.pending_phase_confirmation.is_some() || self.pending_exec_approval.is_some()
        {
            AgentStatus::AwaitingApproval
        } else if self.pending_user_input.is_some() {
            AgentStatus::AwaitingUserInput
        } else if self.running_tool_calls > 0 {
            AgentStatus::ExecutingTool
        } else if self.agent_task.is_some() {
            AgentStatus::Thinking
        } else {
            AgentStatus::Idle
        };
        self.widget.bottom_pane.set_status(next_status);
        self.sync_mux_input_context();
    }

    fn insert_before_streaming_assistant(
        &mut self,
        cell: Box<dyn super::history_cell::HistoryCell>,
    ) {
        if let Some(index) = self.widget.cells.iter().rposition(|c| {
            c.as_any()
                .downcast_ref::<AssistantHistoryCell>()
                .is_some_and(|a| a.is_streaming)
        }) {
            self.widget.insert_cell(index, cell);
        } else {
            self.widget.add_cell(cell);
        }
    }

    /// Extract diff metadata from a successful `apply_patch` result and insert
    /// a [`DiffHistoryCell`] for visual diff rendering.
    fn try_insert_diff_cell(&mut self, result: &ToolOutput) {
        let ToolOutput::Function {
            metadata: Some(meta),
            ..
        } = result
        else {
            return;
        };
        let Some(diffs) = meta.get("diffs").and_then(|v| v.as_array()) else {
            return;
        };

        let cwd = self.registry.working_dir().to_path_buf();
        let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();

        for entry in diffs {
            let Some(path_str) = entry.get("path").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(diff_type) = entry.get("type").and_then(|v| v.as_str()) else {
                continue;
            };
            let diff_text = entry
                .get("diff")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let path = PathBuf::from(path_str);

            let change = match diff_type {
                "add" => FileChange::Add {
                    unified_diff: diff_text.to_string(),
                },
                "delete" => FileChange::Delete {
                    unified_diff: diff_text.to_string(),
                },
                _ => FileChange::Update {
                    unified_diff: diff_text.to_string(),
                    move_path: None,
                },
            };
            changes.insert(path, change);
        }

        if !changes.is_empty() {
            let cell = Box::new(DiffHistoryCell::new(changes, cwd));
            self.insert_before_streaming_assistant(cell);
        }
    }

    fn complete_streaming_assistant_cell(&mut self, content: String) {
        tracing::debug!(
            target: "libra::internal::tui::app",
            bytes = content.len(),
            content = %log_preview_text(&content),
            "tui assistant message completed"
        );
        for cell in self.widget.cells.iter_mut().rev() {
            if let Some(assistant_cell) = cell.as_any_mut().downcast_mut::<AssistantHistoryCell>()
                && assistant_cell.is_streaming
            {
                assistant_cell.content = content;
                assistant_cell.complete();
                return;
            }
        }
        self.widget
            .add_cell(Box::new(AssistantHistoryCell::new(content)));
    }

    fn append_streaming_assistant_delta(&mut self, delta: &str) {
        tracing::debug!(
            target: "libra::internal::tui::app",
            bytes = delta.len(),
            content = %log_preview_text(delta),
            "tui assistant delta rendered"
        );
        for cell in self.widget.cells.iter_mut().rev() {
            if let Some(assistant_cell) = cell.as_any_mut().downcast_mut::<AssistantHistoryCell>()
                && assistant_cell.is_streaming
            {
                assistant_cell.content.push_str(delta);
                return;
            }
        }
        let mut cell = AssistantHistoryCell::streaming();
        cell.content.push_str(delta);
        self.widget.add_cell(Box::new(cell));
    }

    fn append_streaming_thinking_delta(&mut self, delta: &str) {
        tracing::debug!(
            target: "libra::internal::tui::app",
            bytes = delta.len(),
            "tui thinking delta rendered"
        );
        for cell in self.widget.cells.iter_mut().rev() {
            if let Some(thinking_cell) = cell.as_any_mut().downcast_mut::<ThinkingHistoryCell>()
                && thinking_cell.is_streaming
            {
                thinking_cell.append(delta);
                return;
            }
        }
        let mut cell = ThinkingHistoryCell::streaming();
        cell.append(delta);
        self.insert_before_streaming_assistant(Box::new(cell));
    }

    fn complete_streaming_thinking_cells(&mut self) {
        for cell in self.widget.cells.iter_mut() {
            if let Some(thinking_cell) = cell.as_any_mut().downcast_mut::<ThinkingHistoryCell>() {
                thinking_cell.complete();
            }
        }
    }

    fn replace_streaming_assistant_cell(&mut self, replacement: Box<dyn HistoryCell>) {
        for idx in (0..self.widget.cells.len()).rev() {
            if let Some(assistant_cell) = self.widget.cells[idx]
                .as_any()
                .downcast_ref::<AssistantHistoryCell>()
                && assistant_cell.is_streaming
            {
                self.widget.cells[idx] = replacement;
                return;
            }
        }
        self.widget.add_cell(replacement);
    }

    fn tool_call_summary_for_browser(tool_name: &str, arguments: &serde_json::Value) -> String {
        if tool_name == "shell" {
            return arguments
                .get("command")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| "shell command".to_string());
        }

        if tool_name == "read_file" {
            return arguments
                .get("file_path")
                .and_then(serde_json::Value::as_str)
                .map(|path| format!("Read {path}"))
                .unwrap_or_else(|| "Read file".to_string());
        }

        if tool_name == "list_dir" {
            return arguments
                .get("dir_path")
                .and_then(serde_json::Value::as_str)
                .map(|path| format!("List {path}"))
                .unwrap_or_else(|| "List directory".to_string());
        }

        if tool_name == "grep_files" || tool_name == "search_files" {
            let pattern = arguments
                .get("pattern")
                .or_else(|| arguments.get("query"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("(pattern)");
            return format!("Search {pattern}");
        }

        if tool_name == "apply_patch" {
            return "Apply patch".to_string();
        }

        if tool_name == "request_user_input" {
            return "Ask for user input".to_string();
        }

        if tool_name == "update_plan" {
            return "Update plan".to_string();
        }

        tool_name.replace('_', " ")
    }

    fn patchset_snapshot_for_browser(
        call_id: &str,
        status: &str,
        result: &Result<ToolOutput, String>,
    ) -> Option<CodeUiPatchsetSnapshot> {
        let Ok(output) = result else {
            return None;
        };
        let ToolOutput::Function {
            metadata: Some(meta),
            ..
        } = output
        else {
            return None;
        };
        let diffs = meta.get("diffs")?.as_array()?;
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

    fn complete_running_tool_cells_with_interrupt(&mut self) {
        for cell in self.widget.cells.iter_mut() {
            if let Some(tool_cell) = cell.as_any_mut().downcast_mut::<ToolCallHistoryCell>() {
                tool_cell.interrupt_running();
            }
            if let Some(plan_cell) = cell.as_any_mut().downcast_mut::<PlanUpdateHistoryCell>()
                && plan_cell.is_running
            {
                plan_cell.complete();
            }
        }
        self.widget.interrupt_task_mux_tool_calls();
    }

    /// Schedule a frame draw with frame rate limiting.
    fn schedule_draw(&mut self) {
        if self
            .scheduled_draw_task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
        {
            self.scheduled_draw_task = None;
        }

        let now = Instant::now();
        let elapsed = now.duration_since(self.last_draw_time);
        if elapsed >= TARGET_FRAME_INTERVAL {
            if let Some(task) = self.scheduled_draw_task.take() {
                task.abort();
            }
            let _ = self.tui.frame_requester().send(());
            return;
        }

        if self.scheduled_draw_task.is_some() {
            return;
        }

        let delay = TARGET_FRAME_INTERVAL - elapsed;
        let draw_tx = self.tui.frame_requester();
        self.scheduled_draw_task = Some(tokio::spawn(async move {
            sleep(delay).await;
            let _ = draw_tx.send(());
        }));
    }

    /// Draw the current frame.
    fn draw(&mut self) -> anyhow::Result<()> {
        self.tui.draw(|frame| {
            let area = frame.area();
            let cursor_pos = if self.welcome_active {
                let chat_area = self.widget.chat_area_rect(area);
                let welcome_view = WelcomeView {
                    welcome_message: &self.welcome_message,
                    model_name: &self.model_name,
                    provider_name: &self.provider_name,
                    cwd: self.registry.working_dir(),
                };
                welcome_shader::render(chat_area, frame.buffer_mut(), welcome_view);
                self.widget
                    .render_bottom_pane_only(area, frame.buffer_mut())
            } else {
                self.widget.render(area, frame.buffer_mut())
            };
            if let Some(pos) = cursor_pos {
                frame.set_cursor_position(pos);
            }
        })?;
        Ok(())
    }
}

enum PendingPlanRevisionCommand<'a> {
    ContinueAutoRepair { max_attempts: Option<u8> },
    Modify(&'a str),
    Cancel,
    Invalid,
}

fn parse_pending_plan_revision_command(args: &str) -> PendingPlanRevisionCommand<'_> {
    let trimmed = args.trim();
    let mut parts = trimmed.split_whitespace();
    if let Some(command) = parts.next()
        && matches!(
            command.to_ascii_lowercase().as_str(),
            "continue" | "continue-auto" | "auto" | "auto-repair"
        )
    {
        let max_attempts = match parts.next() {
            Some(value) => match value.parse::<u8>() {
                Ok(value) if (1..=MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS).contains(&value) => {
                    Some(value)
                }
                _ => return PendingPlanRevisionCommand::Invalid,
            },
            None => None,
        };
        if parts.next().is_some() {
            return PendingPlanRevisionCommand::Invalid;
        }
        return PendingPlanRevisionCommand::ContinueAutoRepair { max_attempts };
    }
    if trimmed.eq_ignore_ascii_case("cancel") {
        return PendingPlanRevisionCommand::Cancel;
    }
    if let Some(request) = trimmed
        .strip_prefix("modify ")
        .or_else(|| trimmed.strip_prefix("revise "))
        .map(str::trim)
        .filter(|request| !request.is_empty())
    {
        return PendingPlanRevisionCommand::Modify(request);
    }
    PendingPlanRevisionCommand::Invalid
}

fn pending_plan_revision_help_message() -> String {
    "IntentSpec revise mode is active. Describe changes in plain text, use `/intent modify <changes>` to keep revising, or `/intent cancel` to exit.".to_string()
}

fn pending_execution_plan_revision_help_message() -> String {
    format!(
        "Plan revise mode is active. Describe execution-plan changes in plain text, use `continue` or `/plan continue <max-attempts>` to allow more automatic repair attempts (max {}), use `/plan modify <changes>` to keep revising, or `/plan cancel` to exit.",
        MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS
    )
}

fn append_to_last_tool_group_cell(
    cells: &mut Vec<Box<dyn super::history_cell::HistoryCell>>,
    call_id: String,
    tool_name: &str,
    arguments: serde_json::Value,
) -> bool {
    let anchor_index = if let Some(streaming_index) = cells.iter().rposition(|cell| {
        cell.as_any()
            .downcast_ref::<AssistantHistoryCell>()
            .is_some_and(|assistant| assistant.is_streaming)
    }) {
        streaming_index.checked_sub(1)
    } else {
        cells.len().checked_sub(1)
    };

    let Some(anchor_index) = anchor_index else {
        return false;
    };

    let Some(tool_cell) = cells[anchor_index]
        .as_any_mut()
        .downcast_mut::<ToolCallHistoryCell>()
    else {
        return false;
    };

    if !tool_cell.can_merge(tool_name) {
        return false;
    }

    tool_cell.append_call(call_id, tool_name.to_string(), arguments);
    true
}

fn append_to_last_tool_group_preview_cell(
    cells: &mut Vec<Box<dyn super::history_cell::HistoryCell>>,
    call_id: String,
    tool_name: &str,
    arguments: serde_json::Value,
) -> bool {
    let anchor_index = if let Some(streaming_index) = cells.iter().rposition(|cell| {
        cell.as_any()
            .downcast_ref::<AssistantHistoryCell>()
            .is_some_and(|assistant| assistant.is_streaming)
    }) {
        streaming_index.checked_sub(1)
    } else {
        cells.len().checked_sub(1)
    };

    let Some(anchor_index) = anchor_index else {
        return false;
    };

    let Some(tool_cell) = cells[anchor_index]
        .as_any_mut()
        .downcast_mut::<ToolCallHistoryCell>()
    else {
        return false;
    };

    if !tool_cell.can_merge(tool_name) {
        return false;
    }

    tool_cell.append_preview_call(call_id, tool_name.to_string(), arguments);
    true
}

fn parse_update_plan_arguments(
    arguments: &serde_json::Value,
) -> (
    Option<String>,
    Vec<crate::internal::ai::tools::context::PlanStep>,
) {
    if let Ok(args) = serde_json::from_value::<UpdatePlanArgs>(arguments.clone()) {
        (args.explanation, args.plan)
    } else {
        (None, Vec::new())
    }
}

fn mark_visible_tool_call_running(
    cells: &mut [Box<dyn super::history_cell::HistoryCell>],
    call_id: &str,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> bool {
    for cell in cells.iter_mut().rev() {
        if let Some(plan_cell) = cell.as_any_mut().downcast_mut::<PlanUpdateHistoryCell>()
            && plan_cell.call_id == call_id
        {
            let (explanation, steps) = parse_update_plan_arguments(arguments);
            plan_cell.update(explanation, steps);
            plan_cell.is_running = true;
            return true;
        }

        if let Some(tool_cell) = cell.as_any_mut().downcast_mut::<ToolCallHistoryCell>()
            && tool_cell.mark_call_running(call_id, tool_name, arguments)
        {
            return true;
        }
    }

    false
}

fn update_visible_tool_call_preview(
    cells: &mut [Box<dyn super::history_cell::HistoryCell>],
    call_id: &str,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> bool {
    for cell in cells.iter_mut().rev() {
        if let Some(plan_cell) = cell.as_any_mut().downcast_mut::<PlanUpdateHistoryCell>()
            && plan_cell.call_id == call_id
        {
            let (explanation, steps) = parse_update_plan_arguments(arguments);
            plan_cell.update(explanation, steps);
            return true;
        }

        if let Some(tool_cell) = cell.as_any_mut().downcast_mut::<ToolCallHistoryCell>()
            && tool_cell.contains_call_id(call_id)
        {
            tool_cell.append_preview_call(
                call_id.to_string(),
                tool_name.to_string(),
                arguments.clone(),
            );
            return true;
        }
    }

    false
}

fn log_preview_text(text: &str) -> String {
    const MAX_CHARS: usize = 240;
    summarize_llm_output_for_context(text, MAX_CHARS)
}

fn exec_approval_option_ids(
    cache_disabled: bool,
    allow_all_confirmation_requested: bool,
) -> Vec<&'static str> {
    if allow_all_confirmation_requested {
        return vec!["confirm_allow_all_commands", "deny", "abort"];
    }
    if cache_disabled {
        return vec!["approve", "deny", "abort"];
    }
    vec![
        "approve",
        "approve_session",
        "approve_ttl",
        "approve_directory_ttl",
        "approve_pattern_ttl",
        "allow_all_commands",
        "deny",
        "abort",
    ]
}

fn exec_approval_deny_selection(
    cache_disabled: bool,
    allow_all_confirmation_requested: bool,
) -> usize {
    if cache_disabled || allow_all_confirmation_requested {
        1
    } else {
        6
    }
}

fn exec_approval_interaction_options(cache_disabled: bool) -> Vec<CodeUiInteractionOption> {
    if cache_disabled {
        return vec![
            CodeUiInteractionOption {
                id: "approve".to_string(),
                label: "Approve Once".to_string(),
                description: Some("Run this command once; do not cache the decision".to_string()),
            },
            CodeUiInteractionOption {
                id: "deny".to_string(),
                label: "Deny".to_string(),
                description: Some("Reject this command".to_string()),
            },
            CodeUiInteractionOption {
                id: "abort".to_string(),
                label: "Abort".to_string(),
                description: Some("Stop the current turn".to_string()),
            },
        ];
    }

    vec![
        CodeUiInteractionOption {
            id: "approve".to_string(),
            label: "Approve".to_string(),
            description: Some("Run this command once".to_string()),
        },
        CodeUiInteractionOption {
            id: "approve_session".to_string(),
            label: "Approve Session".to_string(),
            description: Some("Approve this exact command for this session".to_string()),
        },
        CodeUiInteractionOption {
            id: "approve_ttl".to_string(),
            label: "Approve TTL".to_string(),
            description: Some("Approve this exact command until the TTL expires".to_string()),
        },
        CodeUiInteractionOption {
            id: "approve_directory_ttl".to_string(),
            label: "Directory TTL".to_string(),
            description: Some(
                "Approve matching command families in this directory until TTL expires".to_string(),
            ),
        },
        CodeUiInteractionOption {
            id: "approve_pattern_ttl".to_string(),
            label: "Pattern TTL".to_string(),
            description: Some(
                "Approve commands with the same argument pattern until TTL expires".to_string(),
            ),
        },
        CodeUiInteractionOption {
            id: "allow_all_commands".to_string(),
            label: "Allow All Commands".to_string(),
            description: Some("Requires a second confirmation".to_string()),
        },
        CodeUiInteractionOption {
            id: "deny".to_string(),
            label: "Deny".to_string(),
            description: Some("Reject this command".to_string()),
        },
        CodeUiInteractionOption {
            id: "abort".to_string(),
            label: "Abort".to_string(),
            description: Some("Stop the current turn".to_string()),
        },
    ]
}

fn exec_approval_allow_all_confirmation_options() -> Vec<CodeUiInteractionOption> {
    vec![
        CodeUiInteractionOption {
            id: "confirm_allow_all_commands".to_string(),
            label: "Confirm Allow All".to_string(),
            description: Some("Allow every command for this session".to_string()),
        },
        CodeUiInteractionOption {
            id: "deny".to_string(),
            label: "Deny".to_string(),
            description: Some("Reject this command".to_string()),
        },
        CodeUiInteractionOption {
            id: "abort".to_string(),
            label: "Abort".to_string(),
            description: Some("Stop the current turn".to_string()),
        },
    ]
}

fn exec_approval_decision_from_selection(
    selected: usize,
    cache_disabled: bool,
    allow_all_confirmation_requested: bool,
) -> ReviewDecision {
    if allow_all_confirmation_requested {
        return match selected {
            0 => ReviewDecision::ApprovedForAllCommands,
            1 => ReviewDecision::Denied,
            _ => ReviewDecision::Abort,
        };
    }
    if cache_disabled {
        return match selected {
            0 => ReviewDecision::Approved,
            1 => ReviewDecision::Denied,
            _ => ReviewDecision::Abort,
        };
    }

    match selected {
        0 => ReviewDecision::Approved,
        1 => ReviewDecision::ApprovedForSession,
        2 => ReviewDecision::ApprovedForTtl,
        3 => ReviewDecision::ApprovedForDirectoryTtl,
        4 => ReviewDecision::ApprovedForPatternTtl,
        5 => ReviewDecision::ApprovedForAllCommands,
        6 => ReviewDecision::Denied,
        _ => ReviewDecision::Abort,
    }
}

fn format_approval_memos(memos: &[ApprovalMemo]) -> String {
    memos
        .iter()
        .map(|memo| {
            let lifetime = memo
                .expires_at
                .map(|expires_at| format!("expires {}", expires_at.to_rfc3339()))
                .unwrap_or_else(|| "session".to_string());
            format!(
                "  {}  {:?} {:?} {:?} {}",
                memo.key, memo.decision, memo.scope, memo.sensitivity_tier, lifetime
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_source_statuses(statuses: &[SourceStatus]) -> String {
    if statuses.is_empty() {
        return "No sources registered.".to_string();
    }

    let mut lines = vec!["Sources:".to_string()];
    for status in statuses {
        let state = if status.shared_state {
            "shared"
        } else {
            "session"
        };
        lines.push(format!(
            "  {} enabled={} kind={:?} trust={:?} tools={} resources={} prompts={} state={}",
            status.slug,
            status.enablement.label(),
            status.kind,
            status.trust_tier,
            status.tool_count,
            status.resource_count,
            status.prompt_count,
            state
        ));
    }
    lines.join("\n")
}

fn parse_usage_grouping(args: &str) -> Result<UsageGrouping, String> {
    let args = args.trim();
    if args.is_empty() {
        return Ok(UsageGrouping::ProviderModel);
    }
    match args {
        "--by=model" | "--by=provider-model" => Ok(UsageGrouping::ProviderModel),
        "--by=agent" => Ok(UsageGrouping::Agent),
        "--by=agent-model" | "--by=agent-provider-model" => Ok(UsageGrouping::AgentProviderModel),
        _ => Err(
            "Usage: /usage [--by=model|--by=provider-model|--by=agent|--by=agent-model]"
                .to_string(),
        ),
    }
}

fn parse_task_command_args(args: &str) -> Result<TaskInvocation, String> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Err("Usage: /task <agent> <prompt>".to_string());
    }
    let Some((agent, prompt)) = trimmed.split_once(char::is_whitespace) else {
        return Err("Usage: /task <agent> <prompt>".to_string());
    };
    task_invocation_from_parts(agent, prompt)
        .map_err(|_| "Usage: /task <agent> <prompt>".to_string())
}

fn task_invocation_from_parts(agent: &str, prompt: &str) -> Result<TaskInvocation, String> {
    let agent = agent.trim();
    let prompt = prompt.trim();
    if agent.is_empty() {
        return Err("agent must not be empty".to_string());
    }
    if prompt.is_empty() {
        return Err("prompt must not be empty".to_string());
    }
    Ok(TaskInvocation {
        description: task_description_from_prompt(prompt),
        prompt: prompt.to_string(),
        subagent_type: agent.to_string(),
        task_id: None,
    })
}

fn task_description_from_prompt(prompt: &str) -> String {
    const MAX_DESCRIPTION_CHARS: usize = 96;
    let first_line = prompt.lines().next().unwrap_or(prompt).trim();
    if first_line.chars().count() <= MAX_DESCRIPTION_CHARS {
        return first_line.to_string();
    }
    let mut description = first_line
        .chars()
        .take(MAX_DESCRIPTION_CHARS.saturating_sub(3))
        .collect::<String>();
    description.push_str("...");
    description
}

fn format_task_command_result(result: SubAgentTaskResult) -> String {
    format!(
        "Task `{}` completed with `{}` on {}/{} ({} step(s)).\n\n{}",
        result.task_id,
        result.agent_name,
        result.provider_id,
        result.model_id,
        result.steps_used,
        result.final_text.trim(),
    )
}

fn usage_grouping_label(grouping: UsageGrouping) -> &'static str {
    match grouping {
        UsageGrouping::ProviderModel => "provider/model",
        UsageGrouping::Agent => "agent",
        UsageGrouping::AgentProviderModel => "agent/provider/model",
    }
}

fn apply_streaming_usage_delta(
    snapshot: &mut UsageDisplaySnapshot,
    delta: &str,
    pending_stream_output_tokens: &mut u64,
) {
    let tokens = estimate_streamed_output_tokens(delta);
    if tokens == 0 {
        return;
    }
    snapshot.completion_tokens = snapshot.completion_tokens.saturating_add(tokens);
    *pending_stream_output_tokens = pending_stream_output_tokens.saturating_add(tokens);
}

fn apply_final_usage_update(
    snapshot: &mut UsageDisplaySnapshot,
    usage: &CompletionUsageSummary,
    wall_clock_ms: u64,
    pending_stream_output_tokens: &mut u64,
) {
    if *pending_stream_output_tokens > 0 {
        snapshot.completion_tokens = snapshot
            .completion_tokens
            .saturating_sub(*pending_stream_output_tokens);
        *pending_stream_output_tokens = 0;
    }
    snapshot.prompt_tokens = snapshot.prompt_tokens.saturating_add(usage.input_tokens);
    snapshot.completion_tokens = snapshot
        .completion_tokens
        .saturating_add(usage.output_tokens);
    snapshot.wall_clock_ms = snapshot.wall_clock_ms.saturating_add(wall_clock_ms);
    snapshot.cost_usd = match (snapshot.cost_usd, usage.cost_usd) {
        (Some(current), Some(next)) => Some(current + next),
        (Some(current), None) => Some(current),
        (None, Some(next)) => Some(next),
        (None, None) => None,
    };
}

fn estimate_streamed_output_tokens(delta: &str) -> u64 {
    let chars = delta.chars().filter(|ch| !ch.is_whitespace()).count();
    if chars == 0 {
        0
    } else {
        u64::try_from(chars.div_ceil(4)).unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Deserialize)]
struct UsageTuiProjectConfig {
    usage: Option<UsageTuiSectionConfig>,
}

#[derive(Debug, Deserialize)]
struct UsageTuiSectionConfig {
    tui_detail_popup: Option<bool>,
}

fn usage_detail_popup_enabled(working_dir: &Path) -> bool {
    let path = working_dir.join(".libra").join("config.toml");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return true;
    };
    usage_detail_popup_enabled_from_toml(&contents).unwrap_or_else(|error| {
        tracing::warn!(
            target: "libra::tui::usage",
            path = %path.display(),
            error = %error,
            "failed to parse usage TUI config; enabling usage detail popup"
        );
        true
    })
}

fn usage_detail_popup_enabled_from_toml(contents: &str) -> Result<bool, toml::de::Error> {
    let config: UsageTuiProjectConfig = toml::from_str(contents)?;
    Ok(config
        .usage
        .and_then(|usage| usage.tui_detail_popup)
        .unwrap_or(true))
}

fn source_command_usage() -> String {
    "Usage: /source [list|enable <slug>|disable <slug>|reload <slug>]".to_string()
}

fn format_memory_anchors(anchors: &[MemoryAnchor]) -> String {
    if anchors.is_empty() {
        return "No memory anchors.".to_string();
    }

    let mut anchors = anchors.to_vec();
    anchors.sort_by_key(|anchor| {
        (
            anchor.review_state,
            anchor.scope,
            anchor.kind,
            anchor.anchor_id,
        )
    });

    let mut lines = vec!["Memory anchors:".to_string()];
    for anchor in anchors {
        let expires = anchor
            .expires_at
            .map(|expires_at| format!(" expires_at={}", expires_at.to_rfc3339()))
            .unwrap_or_default();
        let superseded_by = anchor
            .superseded_by
            .map(|id| format!(" superseded_by={}", short_anchor_id(id)))
            .unwrap_or_default();
        lines.push(format!(
            "  {} [{}] kind={} scope={} confidence={}{}{}",
            anchor.short_id(),
            anchor.review_state,
            anchor.kind,
            anchor.scope,
            anchor.confidence,
            expires,
            superseded_by
        ));
        lines.push(format!("    {}", anchor.content));
    }
    lines.join("\n")
}

fn short_anchor_id(anchor_id: uuid::Uuid) -> String {
    anchor_id.to_string().chars().take(8).collect()
}

fn split_anchor_prefix_and_tail(input: &str) -> (&str, &str) {
    input
        .trim()
        .split_once(char::is_whitespace)
        .map(|(prefix, tail)| (prefix, tail.trim()))
        .unwrap_or((input.trim(), ""))
}

fn anchor_lookup_message(error: MemoryAnchorLookupError) -> String {
    format!("Unable to find memory anchor: {error}")
}

fn summarize_llm_output_for_context(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }

    let mut out = normalized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

fn format_plan_compiled_stage_note(plan: &ExecutionPlanSpec) -> String {
    let groups = plan.parallel_groups();
    let layer_count = groups.len();
    let lane_count = groups.iter().map(Vec::len).max().unwrap_or(0);
    format!(
        "Phase 1 · Plan r{} compiled  \n{} tasks, {} layers, {} max lanes.",
        plan.revision,
        plan.tasks.len(),
        layer_count,
        lane_count
    )
}

fn format_plan_execution_stage_note(revision: u32, task_count: usize) -> String {
    let task_label = if task_count == 1 { "task" } else { "tasks" };
    format!(
        "Phase 2 · Executing Plan r{revision}  \nRunning {task_count} {task_label} through the workflow DAG."
    )
}

fn format_system_verification_stage_note(
    plan: &ExecutionPlanSpec,
    report: &SystemReport,
) -> String {
    format!(
        "Phase 3 · Verifying Plan r{}  \nIntegration: {} ({}) · Security: {} ({}) · Release: {} ({}) · Review: {} · Artifacts: {}.",
        plan.revision,
        bool_label(report.integration.all_required_passed),
        gate_report_summary(&report.integration),
        bool_label(report.security.all_required_passed),
        gate_report_summary(&report.security),
        bool_label(report.release.all_required_passed),
        gate_report_summary(&report.release),
        bool_label(report.review_passed),
        bool_label(report.artifacts_complete)
    )
}

fn format_replan_stage_note(
    current_revision: u32,
    next_revision: u32,
    reason: &str,
    diff_summary: &str,
) -> String {
    let mut lines = vec![format!(
        "Replan · Plan r{current_revision} -> r{next_revision}"
    )];
    let reason = reason.trim();
    if !reason.is_empty() {
        lines.push(format!("Reason: {reason}"));
    }
    let diff_summary = diff_summary.trim();
    if !diff_summary.is_empty() {
        lines.push(format!("Change: {diff_summary}"));
    }
    lines.join("  \n")
}

fn format_decision_stage_note(plan: &ExecutionPlanSpec, decision: &DecisionOutcome) -> String {
    format!(
        "Phase 4 · Decision for Plan r{}: {}",
        plan.revision,
        orchestrator_decision_label(decision)
    )
}

fn format_orchestrator_result(result: &OrchestratorResult) -> String {
    let mut lines = Vec::new();
    let decision_label = orchestrator_decision_label(&result.decision);
    let groups = result.execution_plan_spec.parallel_groups();
    let lane_count = groups.iter().map(Vec::len).max().unwrap_or(0);
    let layer_count = groups.len();
    lines.push(format!("# Execution Result: {decision_label}"));
    lines.push(String::new());

    lines.push("## Overview".to_string());
    lines.push("| Field | Value |".to_string());
    lines.push("| --- | --- |".to_string());
    lines.push(format!("| Decision | {decision_label} |"));
    lines.push(format!(
        "| Revision | {} |",
        result.execution_plan_spec.revision
    ));
    lines.push(format!(
        "| Tasks | {} |",
        result.execution_plan_spec.tasks.len()
    ));
    lines.push(format!(
        "| Max parallel | {} |",
        result.execution_plan_spec.max_parallel
    ));
    lines.push(format!("| Active lanes | {} |", lane_count));
    lines.push(format!("| Layers | {} |", layer_count));
    lines.push(format!("| Replans | {} |", result.replan_count));
    lines.push(format!(
        "| Intent | `{}` |",
        short_markdown_id(&result.intent_spec_id)
    ));
    if let Some(persistence) = &result.persistence {
        if let Some(command) = persistence
            .thread_id
            .as_deref()
            .and_then(graph_command_from_thread_id)
        {
            lines.push(format!("| Graph | `{command}` |"));
        }
        lines.push(format!(
            "| Run | `{}` |",
            short_markdown_id(&persistence.run_id)
        ));
        lines.push(format!("| Persisted tasks | {} |", persistence.tasks.len()));
        lines.push(format!(
            "| Checkpoints | {} |",
            persistence.checkpoints.len()
        ));
    }
    lines.push(String::new());

    lines.push("## Verification".to_string());
    lines.push("| Check | Status | Notes |".to_string());
    lines.push("| --- | --- | --- |".to_string());
    lines.push(format!(
        "| Integration | {} | {} |",
        bool_label(result.system_report.integration.all_required_passed),
        gate_report_summary(&result.system_report.integration)
    ));
    lines.push(format!(
        "| Security | {} | {} |",
        bool_label(result.system_report.security.all_required_passed),
        gate_report_summary(&result.system_report.security)
    ));
    lines.push(format!(
        "| Release | {} | {} |",
        bool_label(result.system_report.release.all_required_passed),
        gate_report_summary(&result.system_report.release)
    ));
    lines.push(format!(
        "| Review | {} | {} |",
        bool_label(result.system_report.review_passed),
        if result.system_report.review_findings.is_empty() {
            "No findings".to_string()
        } else {
            format!("{} findings", result.system_report.review_findings.len())
        }
    ));
    lines.push(format!(
        "| Artifacts | {} | {} |",
        bool_label(result.system_report.artifacts_complete),
        if result.system_report.missing_artifacts.is_empty() {
            "Complete".to_string()
        } else {
            format!(
                "Missing {}",
                result.system_report.missing_artifacts.join(", ")
            )
        }
    ));

    if !result.system_report.review_findings.is_empty() {
        lines.push(String::new());
        lines.push("### Review Findings".to_string());
        for finding in &result.system_report.review_findings {
            lines.push(format!("- {}", finding));
        }
    }
    if !result.system_report.missing_artifacts.is_empty() {
        lines.push(String::new());
        lines.push("### Missing Artifacts".to_string());
        for artifact in &result.system_report.missing_artifacts {
            lines.push(format!("- `{artifact}`"));
        }
    }

    lines.push(String::new());
    lines.push("## Tasks".to_string());
    lines.push("| Task | Status | Retries | Tools | Violations | Notes |".to_string());
    lines.push("| --- | --- | ---: | ---: | ---: | --- |".to_string());
    for (idx, task) in result.execution_plan_spec.tasks.iter().enumerate() {
        let task_result = result
            .task_results
            .iter()
            .find(|item| item.task_id == task.id());
        let kind = match task.kind {
            TaskKind::Implementation => "I",
            TaskKind::Analysis => "A",
            TaskKind::Gate => "G",
        };
        let label = format!("{kind}{:02} {}", idx + 1, task.title());
        let (status, retries, tools, violations, notes) = if let Some(task_result) = task_result {
            let notes = if let Some(review) = task_result.review.as_ref() {
                let mut note = format!(
                    "Review: {} | approved: {}",
                    review.summary,
                    if review.approved { "yes" } else { "no" }
                );
                if !review.issues.is_empty() {
                    note.push_str(&format!(" | Issues: {}", review.issues.join("; ")));
                }
                note
            } else if task_result.status == TaskNodeStatus::Failed {
                if let Some(reason) = task_result
                    .agent_output
                    .as_deref()
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty())
                {
                    format!("Reason: {reason}")
                } else if let Some(reason) =
                    summarize_failed_gate_report(task_result.gate_report.as_ref())
                {
                    format!("Reason: {reason}")
                } else {
                    "-".to_string()
                }
            } else {
                "-".to_string()
            };
            (
                orchestrator_status_label(&task_result.status),
                task_result.retry_count.to_string(),
                task_result.tool_calls.len().to_string(),
                task_result.policy_violations.len().to_string(),
                notes,
            )
        } else {
            (
                "pending",
                "0".to_string(),
                "0".to_string(),
                "0".to_string(),
                "-".to_string(),
            )
        };
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} |",
            escape_markdown_cell(&label),
            status,
            retries,
            tools,
            violations,
            escape_markdown_cell(&notes)
        ));
    }

    if let Some(persistence) = &result.persistence {
        lines.push(String::new());
        lines.push("## Persistence".to_string());
        lines.push("| Object | Value |".to_string());
        lines.push("| --- | --- |".to_string());
        lines.push(format!(
            "| Provenance | `{}` |",
            persistence
                .provenance_id
                .as_deref()
                .map(short_markdown_id)
                .unwrap_or_else(|| "none".to_string())
        ));
        lines.push(format!(
            "| Decision object | `{}` |",
            persistence
                .decision_id
                .as_deref()
                .map(short_markdown_id)
                .unwrap_or_else(|| "none".to_string())
        ));
        lines.push(format!(
            "| Initial snapshot | `{}` |",
            persistence
                .initial_snapshot_id
                .as_deref()
                .map(short_markdown_id)
                .unwrap_or_else(|| "none".to_string())
        ));
        if !persistence.checkpoints.is_empty() {
            lines.push(String::new());
            lines.push("### Checkpoints".to_string());
            lines.push("| Rev | Snapshot | Decision | Reason |".to_string());
            lines.push("| --- | --- | --- | --- |".to_string());
            for checkpoint in &persistence.checkpoints {
                lines.push(format!(
                    "| {} | `{}` | `{}` | {} |",
                    checkpoint.revision,
                    checkpoint
                        .snapshot_id
                        .as_deref()
                        .map(short_markdown_id)
                        .unwrap_or_else(|| "none".to_string()),
                    checkpoint
                        .decision_id
                        .as_deref()
                        .map(short_markdown_id)
                        .unwrap_or_else(|| "none".to_string()),
                    escape_markdown_cell(&checkpoint.reason)
                ));
            }
        }
    }

    lines.join("\n")
}

fn orchestrator_decision_label(decision: &DecisionOutcome) -> &'static str {
    match decision {
        DecisionOutcome::Commit => "Commit",
        DecisionOutcome::HumanReviewRequired => "Human Review Required",
        DecisionOutcome::Abandon => "Abandon",
    }
}

fn orchestrator_status_label(status: &TaskNodeStatus) -> &'static str {
    match status {
        TaskNodeStatus::Pending => "pending",
        TaskNodeStatus::Running => "running",
        TaskNodeStatus::Completed => "done",
        TaskNodeStatus::Failed => "failed",
        TaskNodeStatus::Skipped => "skipped",
    }
}

fn gate_report_summary(report: &GateReport) -> String {
    if report.results.is_empty() {
        return "No checks".to_string();
    }
    let passed = report.results.iter().filter(|item| item.passed).count();
    format!("{passed}/{} checks passed", report.results.len())
}

fn bool_label(value: bool) -> &'static str {
    if value { "Pass" } else { "Fail" }
}

fn short_markdown_id(id: &str) -> String {
    if id.len() <= 12 {
        id.to_string()
    } else {
        format!("{}…", &id[..12])
    }
}

fn escape_markdown_cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use std::{
        process::Command,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use git_internal::internal::object::{task::Task as GitTask, types::ActorRef};
    use serde_json::json;

    use super::{
        DEFAULT_AUTOMATIC_PLAN_REPAIR_ATTEMPTS, ExecutionFailureRevision,
        FirstTurnIntentPolicyUpdate, MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS,
        PendingPlanRevisionCommand, ProviderPlanDraft, ProviderPlanDraftStep, ReviewScrollAction,
        append_to_last_tool_group_cell, append_to_last_tool_group_preview_cell,
        apply_developer_network_access, apply_final_usage_update, apply_goal_tool_visibility,
        apply_streaming_usage_delta, automatic_plan_repair_request_from_report,
        automatic_plan_repair_threshold_message, bind_goal_stop_policy,
        build_execution_plan_prompt, build_execution_plan_revision_prompt, build_plan_prompt,
        build_plan_revision_prompt, changed_path_from_short_status_line,
        classify_execution_failure_revision, classify_first_turn_task_intent,
        code_ui_response_from_managed_selection, default_chat_allowed_tools,
        estimate_streamed_output_tokens, exec_approval_decision_from_selection,
        execution_failure_report, execution_failure_revision_message,
        execution_requires_plan_repair, format_decision_stage_note,
        format_intentspec_target_mismatch, format_orchestrator_result,
        format_plan_compiled_stage_note, format_plan_execution_stage_note,
        format_replan_stage_note, format_system_verification_stage_note,
        graph_thread_id_from_orchestrator_result, intentspec_failure_revision_message_from_report,
        intentspec_with_plan_draft_objectives, is_default_chat_tool, is_global_quit_command_input,
        is_phase1_plan_draft_tool, mark_visible_tool_call_running, newest_managed_assistant_text,
        normalize_terminal_paste_text, parse_pending_plan_revision_command,
        parse_task_command_args, parse_usage_grouping,
        pending_execution_plan_revision_help_message, pending_plan_revision_help_message,
        phase0_context_snapshot_request_from_changed_files, phase0_plan_tool_loop_config,
        phase1_plan_tool_loop_config, provider_plan_draft_from_args, provider_plan_draft_from_plan,
        record_orchestrator_thread_metadata, review_scroll_action, session_graph_thread_id,
        should_auto_classify_first_user_message, should_auto_repair_execution_failure,
        should_forward_phase0_model_text_delta, should_forward_phase1_model_text_delta,
        should_route_plain_message_to_plan, task_description_from_prompt,
        undo_should_prefer_vcs_rollback, usage_detail_popup_enabled_from_toml,
    };
    use crate::internal::{
        ai::{
            agent::{TaskIntent, ToolLoopConfig},
            completion::{
                AssistantContent, CompletionError, CompletionModel, CompletionRequest,
                CompletionResponse, CompletionUsageSummary, Message, Text,
            },
            goal::GoalStopPolicy,
            intentspec::{
                ResolveContext,
                draft::{DraftAcceptance, DraftIntent, DraftRisk, IntentDraft},
                resolve_intentspec,
                types::{
                    ChangeType, DecompositionMode, IntentSpec, NetworkPolicy, Objective,
                    ObjectiveKind, RiskLevel,
                },
            },
            orchestrator::types::{
                DecisionOutcome, ExecutionPlanSpec, GateReport, GateResult, OrchestratorResult,
                PersistedExecution, PolicyViolation, SystemReport, TaskContract, TaskKind,
                TaskNodeStatus, TaskResult, TaskSpec, ToolCallRecord,
            },
            sandbox::ReviewDecision,
            session::SessionState,
            tools::{
                ToolHandler, ToolRegistry, ToolRegistryBuilder,
                context::{PlanDraftStep, SubmitPlanDraftArgs},
                spec::ToolSpec,
            },
            usage::{UsageDisplaySnapshot, UsageGrouping, format_usage_badge},
            web::code_ui::{
                CodeUiApplyToFuture, CodeUiCapabilities, CodeUiInteractionKind,
                CodeUiInteractionOption, CodeUiInteractionRequest, CodeUiInteractionStatus,
                CodeUiProviderInfo, CodeUiSessionStatus, CodeUiTranscriptEntry,
                CodeUiTranscriptEntryKind, initial_snapshot,
            },
        },
        tui::history_cell::{AssistantHistoryCell, HistoryCell, ToolCallHistoryCell},
    };

    fn make_task(title: &str, kind: TaskKind) -> TaskSpec {
        let actor = ActorRef::agent("format-orchestrator-result").unwrap();
        let task = GitTask::new(actor, title, None).unwrap();
        TaskSpec {
            step: git_internal::internal::object::plan::PlanStep::new(title),
            task,
            objective: title.to_string(),
            kind,
            gate_stage: None,
            owner_role: None,
            scope_in: vec![],
            scope_out: vec![],
            checks: vec![],
            contract: TaskContract::default(),
        }
    }

    #[test]
    fn usage_grouping_parser_accepts_tui_flags() {
        assert_eq!(
            parse_usage_grouping("").unwrap(),
            UsageGrouping::ProviderModel
        );
        assert_eq!(
            parse_usage_grouping("--by=agent").unwrap(),
            UsageGrouping::Agent
        );
        assert_eq!(
            parse_usage_grouping("--by=agent-model").unwrap(),
            UsageGrouping::AgentProviderModel
        );
        assert!(parse_usage_grouping("--bad").is_err());
    }

    #[test]
    fn task_command_parser_requires_agent_and_prompt() {
        assert_eq!(
            parse_task_command_args("").unwrap_err(),
            "Usage: /task <agent> <prompt>"
        );
        assert_eq!(
            parse_task_command_args("explorer").unwrap_err(),
            "Usage: /task <agent> <prompt>"
        );

        let invocation = parse_task_command_args(" explorer   grep TODO src/ ").unwrap();
        assert_eq!(invocation.subagent_type, "explorer");
        assert_eq!(invocation.prompt, "grep TODO src/");
        assert_eq!(invocation.description, "grep TODO src/");
        assert!(invocation.task_id.is_none());
    }

    #[test]
    fn task_description_from_prompt_is_bounded_to_first_line() {
        assert_eq!(
            task_description_from_prompt("first line\nsecond line"),
            "first line"
        );
        let long = "a".repeat(140);
        let description = task_description_from_prompt(&long);
        assert!(description.ends_with("..."));
        assert!(description.chars().count() <= 96);
    }

    #[test]
    fn usage_detail_popup_config_defaults_to_enabled() {
        assert!(usage_detail_popup_enabled_from_toml("").unwrap());
        assert!(
            !usage_detail_popup_enabled_from_toml("[usage]\ntui_detail_popup = false\n").unwrap()
        );
        assert!(
            usage_detail_popup_enabled_from_toml("[usage]\ntui_detail_popup = true\n").unwrap()
        );
    }

    #[test]
    fn streaming_usage_delta_updates_estimate_until_final_usage_arrives() {
        let mut snapshot = UsageDisplaySnapshot {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            prompt_tokens: 0,
            completion_tokens: 10,
            wall_clock_ms: 0,
            cost_usd: None,
        };
        let mut pending = 0;

        apply_streaming_usage_delta(&mut snapshot, "abcdefgh", &mut pending);

        assert_eq!(estimate_streamed_output_tokens("abcdefgh"), 2);
        assert_eq!(snapshot.completion_tokens, 12);
        assert_eq!(pending, 2);
        assert_eq!(
            format_usage_badge(&snapshot),
            "openai/gpt-test · 12 tok · 0.0s"
        );

        apply_final_usage_update(
            &mut snapshot,
            &CompletionUsageSummary {
                input_tokens: 5,
                output_tokens: 7,
                cached_tokens: None,
                reasoning_tokens: None,
                total_tokens: Some(12),
                cost_usd: Some(0.01),
            },
            1500,
            &mut pending,
        );

        assert_eq!(pending, 0);
        assert_eq!(snapshot.prompt_tokens, 5);
        assert_eq!(snapshot.completion_tokens, 17);
        assert_eq!(snapshot.wall_clock_ms, 1500);
        assert_eq!(snapshot.cost_usd, Some(0.01));
        assert_eq!(
            format_usage_badge(&snapshot),
            "openai/gpt-test · 22 tok · 1.5s · $0.0100"
        );
    }

    #[test]
    fn review_scroll_keys_preserve_plain_selection_arrows() {
        assert_eq!(
            review_scroll_action(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
            Some(ReviewScrollAction::Top)
        );
        assert_eq!(
            review_scroll_action(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
            Some(ReviewScrollAction::Bottom)
        );
        assert_eq!(
            review_scroll_action(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
            Some(ReviewScrollAction::Up(10))
        );
        assert_eq!(
            review_scroll_action(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
            Some(ReviewScrollAction::Down(10))
        );
        assert_eq!(
            review_scroll_action(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL)),
            Some(ReviewScrollAction::Up(1))
        );
        assert_eq!(
            review_scroll_action(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT)),
            Some(ReviewScrollAction::Down(1))
        );
        assert_eq!(
            review_scroll_action(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(ReviewScrollAction::Up(1))
        );
        assert_eq!(
            review_scroll_action(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)),
            Some(ReviewScrollAction::Down(1))
        );
        assert_eq!(
            review_scroll_action(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            review_scroll_action(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            None
        );
    }

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git command should run");
        assert!(status.success(), "git {:?} failed with {status}", args);
    }

    fn orchestrator_fixture() -> OrchestratorResult {
        let first = make_task("Inspect sources", TaskKind::Implementation);
        let second = make_task("Run checks", TaskKind::Gate);
        let plan = ExecutionPlanSpec {
            intent_spec_id: "intent-1".into(),
            revision: 4,
            parent_revision: Some(3),
            replan_reason: Some("task kept failing after retries".into()),
            tasks: vec![first.clone(), second.clone()],
            max_parallel: 1,
            checkpoints: vec![],
        };
        OrchestratorResult {
            decision: DecisionOutcome::Abandon,
            execution_plan_spec: plan.clone(),
            plan_revision_specs: vec![plan],
            run_state: Default::default(),
            task_results: vec![TaskResult {
                task_id: first.id(),
                status: TaskNodeStatus::Failed,
                gate_report: None,
                agent_output: None,
                retry_count: 4,
                tool_calls: vec![],
                policy_violations: vec![],
                model_usage: None,
                review: None,
                thinking: None,
            }],
            system_report: SystemReport {
                integration: GateReport::empty(),
                security: GateReport::empty(),
                release: GateReport::empty(),
                review_passed: false,
                review_findings: vec!["missing regression test".into()],
                artifacts_complete: false,
                missing_artifacts: vec!["patchset@per-task".into()],
                overall_passed: false,
            },
            intent_spec_id: "019ce515-077c-7c12-8e90-755533e512e3".into(),
            lifecycle_change_log: vec![],
            replan_count: 3,
            persistence: None,
        }
    }

    fn minimal_intentspec(objectives: Vec<Objective>) -> IntentSpec {
        resolve_intentspec(
            IntentDraft {
                intent: DraftIntent {
                    summary: "Implement workflow".to_string(),
                    problem_statement: "Need a plan workflow".to_string(),
                    change_type: ChangeType::Feature,
                    objectives,
                    in_scope: vec!["src".to_string()],
                    out_of_scope: vec![],
                    touch_hints: None,
                },
                acceptance: DraftAcceptance {
                    success_criteria: vec!["workflow is implemented".to_string()],
                    fast_checks: vec![],
                    integration_checks: vec![],
                    security_checks: vec![],
                    release_checks: vec![],
                },
                risk: DraftRisk {
                    rationale: "test".to_string(),
                    factors: vec![],
                    level: Some(RiskLevel::Low),
                },
            },
            RiskLevel::Low,
            ResolveContext {
                working_dir: ".".to_string(),
                base_ref: "HEAD".to_string(),
                created_by_id: "tester".to_string(),
            },
        )
    }

    fn provider_plan_draft(steps: &[&str]) -> ProviderPlanDraft {
        ProviderPlanDraft {
            explanation: Some("Plan generated from confirmed IntentSpec".to_string()),
            steps: steps
                .iter()
                .map(|step| ProviderPlanDraftStep {
                    title: (*step).to_string(),
                })
                .collect(),
        }
    }

    fn function_body<'a>(source: &'a str, signature: &str) -> Option<&'a str> {
        let start = source.find(signature)?;
        let open = source[start..].find('{')? + start;
        let mut depth = 0usize;
        for (offset, ch) in source[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(&source[open..=open + offset]);
                    }
                }
                _ => {}
            }
        }
        None
    }

    #[test]
    fn provider_plan_draft_steps_replace_objectives_for_execution_plan_compile() {
        let spec = minimal_intentspec(vec![Objective {
            title: "original broad objective".to_string(),
            kind: ObjectiveKind::Implementation,
        }]);
        let plan = provider_plan_draft(&["Inspect current flow", "Add LLM plan review"]);

        let planned_spec = intentspec_with_plan_draft_objectives(&spec, &plan).unwrap();

        let titles = planned_spec
            .intent
            .objectives
            .iter()
            .map(|objective| objective.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(titles, vec!["Inspect current flow", "Add LLM plan review"]);
        assert!(
            planned_spec
                .intent
                .objectives
                .iter()
                .all(|objective| objective.kind == ObjectiveKind::Implementation)
        );
        assert_eq!(
            planned_spec
                .libra
                .as_ref()
                .and_then(|binding| binding.plan_generation.as_ref())
                .map(|config| &config.decomposition_mode),
            Some(&DecompositionMode::PerObjective)
        );
    }

    #[test]
    fn developer_network_choice_updates_intentspec_network_policy() {
        let mut spec = minimal_intentspec(vec![Objective {
            title: "download dependency metadata".to_string(),
            kind: ObjectiveKind::Implementation,
        }]);
        assert_eq!(
            spec.constraints.security.network_policy,
            NetworkPolicy::Deny
        );

        apply_developer_network_access(&mut spec, true);
        assert_eq!(
            spec.constraints.security.network_policy,
            NetworkPolicy::Allow
        );

        apply_developer_network_access(&mut spec, false);
        assert_eq!(
            spec.constraints.security.network_policy,
            NetworkPolicy::Deny
        );
    }

    #[test]
    fn execution_plan_prompt_requires_submit_plan_draft_after_intentspec_confirmation() {
        let prompt = build_execution_plan_prompt("{\"kind\":\"IntentSpec\"}");

        assert!(prompt.contains("already confirmed IntentSpec"));
        assert!(prompt.contains("call submit_plan_draft exactly once"));
        assert!(prompt.contains("web_search"));
        assert!(prompt.contains("Rust edition 2024 is stable"));
        assert!(!prompt.contains("call update_plan exactly once"));
        assert!(prompt.contains("Do not call submit_intent_draft"));
    }

    #[test]
    fn execution_plan_revision_prompt_includes_existing_plan_and_change_request() {
        let plan = provider_plan_draft(&["Inspect current flow"]);
        let prompt = build_execution_plan_revision_prompt(
            "{\"kind\":\"IntentSpec\"}",
            &plan,
            "split implementation and tests",
            None,
        );

        assert!(prompt.contains("Current execution plan"));
        assert!(prompt.contains("Inspect current flow"));
        assert!(prompt.contains("split implementation and tests"));
        assert!(prompt.contains("call submit_plan_draft exactly once"));
        assert!(!prompt.contains("call update_plan exactly once"));
    }

    #[test]
    fn execution_plan_revision_prompt_includes_previous_failure_context() {
        let plan = provider_plan_draft(&["Run failing check"]);
        let prompt = build_execution_plan_revision_prompt(
            "{\"kind\":\"IntentSpec\"}",
            &plan,
            "repair the plan",
            Some("Failed tasks: Run failing check.\nFailure details:\n- cargo test failed"),
        );

        assert!(prompt.contains("Previous execution failure evidence"));
        assert!(prompt.contains("cargo test failed"));
        assert!(prompt.contains("Do not start over from the IntentSpec"));
        assert!(prompt.contains("primary goal is to repair that specific failed execution"));
        assert!(prompt.contains("addresses the concrete failure"));
    }

    #[test]
    fn phase1_plan_draft_tool_is_suppressed_from_generic_transcript() {
        assert!(is_phase1_plan_draft_tool("submit_plan_draft"));
        assert!(!is_phase1_plan_draft_tool("update_plan"));
    }

    #[test]
    fn phase1_plan_generation_does_not_forward_provider_text() {
        assert!(!should_forward_phase1_model_text_delta(
            "provider draft markdown that should stay internal"
        ));
    }

    #[test]
    fn phase0_tool_loop_config_stops_after_submit_intent_draft() {
        let config = phase0_plan_tool_loop_config(ToolLoopConfig::default());

        assert_eq!(config.max_turns, Some(12));
        assert_eq!(
            config.terminal_tools.as_ref().unwrap(),
            &vec!["submit_intent_draft".to_string()]
        );
        assert!(
            config
                .allowed_tools
                .as_ref()
                .is_some_and(|tools| tools.iter().any(|tool| tool == "submit_intent_draft"))
        );
    }

    #[test]
    fn phase0_review_persistence_uses_runtime_write_helper() {
        let source = include_str!("app.rs");
        let body = function_body(source, "async fn begin_plan_workflow")
            .expect("begin_plan_workflow body should be found");

        assert!(
            body.contains("persist_phase0_intent_for_review("),
            "Phase 0 review persistence must go through the runtime formal-write helper"
        );
        assert!(
            body.contains("persist_phase0_context_snapshot_for_review("),
            "Phase 0 review persistence must evaluate ContextSnapshot freeze through the runtime helper"
        );
        assert!(
            !body.contains("persist_intentspec("),
            "begin_plan_workflow must not call lower-level IntentSpec persistence directly"
        );
        assert!(
            !body.contains("create_context_snapshot_impl("),
            "begin_plan_workflow must not write ContextSnapshot directly through MCP"
        );
    }

    #[test]
    fn phase0_context_snapshot_request_skips_clean_worktree() {
        let spec = minimal_intentspec(vec![Objective {
            title: "inspect current flow".to_string(),
            kind: ObjectiveKind::Analysis,
        }]);
        let actor = ActorRef::system("phase0-test").unwrap();

        let request = phase0_context_snapshot_request_from_changed_files(&spec, Vec::new(), actor);

        assert!(request.is_none());
    }

    #[test]
    fn phase0_context_snapshot_request_captures_changed_paths() {
        let spec = minimal_intentspec(vec![Objective {
            title: "inspect current flow".to_string(),
            kind: ObjectiveKind::Analysis,
        }]);
        let actor = ActorRef::system("phase0-test").unwrap();

        let request = phase0_context_snapshot_request_from_changed_files(
            &spec,
            vec!["src/main.rs".to_string(), " docs/agent.md ".to_string()],
            actor,
        )
        .expect("dirty worktree should produce a snapshot request");

        assert_eq!(request.items.len(), 2);
        assert_eq!(request.items[0].path, "src/main.rs");
        assert_eq!(request.items[1].path, "docs/agent.md");
        assert!(
            request
                .summary
                .as_deref()
                .unwrap()
                .contains("2 changed paths")
        );
    }

    #[test]
    fn phase1_tool_loop_config_stops_after_submit_plan_draft() {
        let config = phase1_plan_tool_loop_config(ToolLoopConfig::default());

        assert_eq!(config.max_turns, Some(12));
        assert_eq!(
            config.terminal_tools.as_ref().unwrap(),
            &vec!["submit_plan_draft".to_string()]
        );
        assert!(
            config
                .allowed_tools
                .as_ref()
                .is_some_and(|tools| tools.iter().any(|tool| tool == "submit_plan_draft"))
        );
    }

    #[test]
    fn phase0_intentspec_generation_does_not_forward_provider_text() {
        assert!(!should_forward_phase0_model_text_delta(
            "provider draft markdown that should stay internal"
        ));
    }

    #[test]
    fn default_chat_tools_exclude_phase_review_submission_tools() {
        assert!(!is_default_chat_tool("submit_intent_draft"));
        assert!(!is_default_chat_tool("submit_plan_draft"));
        assert!(!is_default_chat_tool("update_goal_progress"));
        assert!(!is_default_chat_tool("submit_goal_complete"));
        assert!(is_default_chat_tool("update_plan"));
        assert!(is_default_chat_tool("web_search"));
    }

    #[test]
    fn goal_tools_are_visible_only_when_goal_is_active() {
        let registry = ToolRegistryBuilder::with_working_dir(std::path::PathBuf::from("/tmp"))
            .register("read_file", Arc::new(NamedToolHandler("read_file")))
            .register(
                "update_goal_progress",
                Arc::new(NamedToolHandler("update_goal_progress")),
            )
            .register(
                "submit_goal_complete",
                Arc::new(NamedToolHandler("submit_goal_complete")),
            )
            .build();
        let mut allowed = vec![
            "read_file".to_string(),
            "update_goal_progress".to_string(),
            "submit_goal_complete".to_string(),
        ];

        apply_goal_tool_visibility(&mut allowed, false, &registry);
        assert_eq!(allowed, vec!["read_file".to_string()]);

        apply_goal_tool_visibility(&mut allowed, true, &registry);
        assert_eq!(
            allowed,
            vec![
                "read_file".to_string(),
                "update_goal_progress".to_string(),
                "submit_goal_complete".to_string()
            ]
        );
    }

    #[test]
    fn goal_stop_policy_is_bound_into_tool_loop_config() {
        let goal_id = uuid::Uuid::new_v4();
        let mut config = ToolLoopConfig::default();

        let policy = bind_goal_stop_policy(&mut config, goal_id);

        assert_eq!(policy, GoalStopPolicy::GoalBound { goal_id });
        assert_eq!(
            config.goal_stop_policy,
            Some(GoalStopPolicy::GoalBound { goal_id })
        );
    }

    #[test]
    fn short_status_parser_extracts_changed_path() {
        assert_eq!(
            changed_path_from_short_status_line(" M src/main.rs"),
            Some("src/main.rs".to_string())
        );
        assert_eq!(
            changed_path_from_short_status_line("R  old.rs -> new.rs"),
            Some("new.rs".to_string())
        );
        assert_eq!(changed_path_from_short_status_line("## main"), None);
    }

    #[test]
    fn default_chat_allowed_tools_prefers_launch_config_policy() {
        let registry = ToolRegistry::with_working_dir(std::path::PathBuf::from("/tmp"));
        let config = ToolLoopConfig {
            allowed_tools: Some(vec!["read_file".to_string(), "list_symbols".to_string()]),
            ..ToolLoopConfig::default()
        };

        assert_eq!(
            default_chat_allowed_tools(&registry, &config),
            vec!["read_file".to_string(), "list_symbols".to_string()]
        );
    }

    #[derive(Clone)]
    struct IntentTestModel {
        response: String,
        requests: Arc<Mutex<Vec<CompletionRequest>>>,
    }

    impl IntentTestModel {
        fn new(response: impl Into<String>) -> Self {
            Self {
                response: response.into(),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn requests(&self) -> Vec<CompletionRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl CompletionModel for IntentTestModel {
        type Response = serde_json::Value;

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            self.requests.lock().unwrap().push(request);
            Ok(CompletionResponse {
                content: vec![AssistantContent::Text(Text {
                    text: self.response.clone(),
                })],
                reasoning_content: None,
                raw_response: serde_json::json!({ "provider": "intent-test" }),
            })
        }
    }

    struct NamedToolHandler(&'static str);

    #[async_trait]
    impl ToolHandler for NamedToolHandler {
        fn kind(&self) -> crate::internal::ai::tools::context::ToolKind {
            crate::internal::ai::tools::context::ToolKind::Function
        }

        async fn handle(
            &self,
            _invocation: crate::internal::ai::tools::context::ToolInvocation,
        ) -> crate::internal::ai::tools::error::ToolResult<crate::internal::ai::tools::ToolOutput>
        {
            Ok(crate::internal::ai::tools::ToolOutput::success("ok"))
        }

        fn schema(&self) -> ToolSpec {
            ToolSpec::new(self.0, "test tool")
        }
    }

    #[tokio::test]
    async fn first_turn_classifier_updates_prompt_and_direct_chat_tool_policy() {
        let temp = tempfile::tempdir().unwrap();
        let registry = ToolRegistryBuilder::with_working_dir(temp.path().to_path_buf())
            .register("read_file", Arc::new(NamedToolHandler("read_file")))
            .register("list_symbols", Arc::new(NamedToolHandler("list_symbols")))
            .register("apply_patch", Arc::new(NamedToolHandler("apply_patch")))
            .register("shell", Arc::new(NamedToolHandler("shell")))
            .build();
        let model = IntentTestModel::new(
            r#"{"intent":"review","confidence":0.93,"rationale":"user asked for review"}"#,
        );
        let mut config = ToolLoopConfig {
            preamble: Some(
                crate::internal::ai::prompt::SystemPromptBuilder::new(temp.path())
                    .with_intent(TaskIntent::Unknown)
                    .with_dynamic_context()
                    .build(),
            ),
            allowed_tools: Some(registry.filter_by_intent(TaskIntent::Unknown)),
            ..ToolLoopConfig::default()
        };

        let decision = classify_first_turn_task_intent(
            &model,
            &registry,
            &mut config,
            "Please review this change for production risks",
            FirstTurnIntentPolicyUpdate::PromptAndAllowedTools,
        )
        .await
        .unwrap();

        assert_eq!(decision.intent, TaskIntent::Review);
        let allowed_tools = config.allowed_tools.as_ref().unwrap();
        assert!(allowed_tools.contains(&"read_file".to_string()));
        assert!(allowed_tools.contains(&"list_symbols".to_string()));
        assert!(!allowed_tools.contains(&"apply_patch".to_string()));
        assert!(!allowed_tools.contains(&"shell".to_string()));

        let preamble = config.preamble.as_deref().unwrap();
        assert!(preamble.contains("## Task Intent"));
        assert!(preamble.contains("intent=review"));
        assert!(!preamble.contains("intent=unknown"));

        let requests = model.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].tools.is_empty());
        assert!(
            requests[0]
                .preamble
                .as_deref()
                .unwrap()
                .contains("Classify the user's libra code request")
        );
    }

    #[test]
    fn first_turn_auto_classification_is_only_for_unprofiled_empty_history() {
        assert!(should_auto_classify_first_user_message(true, &[], None));
        assert!(!should_auto_classify_first_user_message(false, &[], None));
        assert!(!should_auto_classify_first_user_message(
            true,
            &[Message::user("previous turn")],
            None
        ));
        assert!(!should_auto_classify_first_user_message(
            true,
            &[],
            Some(&vec!["read_file".to_string()])
        ));
    }

    #[test]
    fn undo_prefers_vcs_rollback_only_for_clean_committed_worktree() {
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init"]);
        run_git(repo.path(), &["config", "user.email", "test@example.com"]);
        run_git(repo.path(), &["config", "user.name", "Test User"]);
        run_git(repo.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.path().join("tracked.txt"), "base\n").unwrap();
        run_git(repo.path(), &["add", "tracked.txt"]);
        run_git(repo.path(), &["commit", "-m", "base"]);

        assert!(undo_should_prefer_vcs_rollback(repo.path()));

        std::fs::write(repo.path().join("tracked.txt"), "dirty\n").unwrap();
        assert!(!undo_should_prefer_vcs_rollback(repo.path()));
    }

    #[test]
    fn provider_plan_draft_from_args_rejects_empty_titles() {
        let result = provider_plan_draft_from_args(SubmitPlanDraftArgs {
            explanation: None,
            steps: vec![PlanDraftStep {
                title: "  ".to_string(),
            }],
        });

        assert!(result.is_err());
    }

    #[test]
    fn execution_plan_revision_help_is_plan_specific() {
        let help = pending_execution_plan_revision_help_message();

        assert!(help.contains("Plan revise mode"));
        assert!(help.contains("/plan continue"));
        assert!(help.contains("/plan modify <changes>"));
    }

    #[test]
    fn automatic_plan_repair_request_includes_failure_report_and_attempt() {
        let mut result = orchestrator_fixture();
        let task_result = result
            .task_results
            .first_mut()
            .expect("fixture should include a failed task result");
        task_result.tool_calls = vec![ToolCallRecord {
            tool_name: "shell".to_string(),
            action: "cargo test".to_string(),
            arguments_json: None,
            paths_read: vec![],
            paths_written: vec![],
            success: false,
            summary: Some("error[E0425]: cannot find value `x`".to_string()),
            diffs: vec![],
        }];

        let report = execution_failure_report(Some(&result), None);
        let request = automatic_plan_repair_request_from_report(
            &report,
            2,
            MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS,
        );

        assert!(request.contains("Automatic plan repair attempt 2/10"));
        assert!(request.contains("repair of that failed execution"));
        assert!(request.contains("not a fresh plan from the original Intent"));
        assert!(request.contains("concrete repair Step"));
        assert!(request.contains("preserve successful prior steps only when"));
        assert!(request.contains("cargo test"));
        assert!(request.contains("cannot find value"));
    }

    #[test]
    fn automatic_plan_repair_default_threshold_is_ten() {
        assert_eq!(MAX_AUTOMATIC_PLAN_REPAIR_ATTEMPTS, 10);
        assert_eq!(DEFAULT_AUTOMATIC_PLAN_REPAIR_ATTEMPTS, 0);
    }

    #[test]
    fn automatic_plan_repair_stops_at_threshold_for_developer_confirmation() {
        let result = orchestrator_fixture();
        let report = execution_failure_report(Some(&result), None);
        let route = ExecutionFailureRevision::PlanRevision;

        assert!(should_auto_repair_execution_failure(route, 0, 3));
        assert!(should_auto_repair_execution_failure(route, 2, 3));
        assert!(!should_auto_repair_execution_failure(route, 3, 3));
        assert!(should_auto_repair_execution_failure(route, 3, 5));
        assert!(!should_auto_repair_execution_failure(route, 0, 0));

        let message = automatic_plan_repair_threshold_message(&report, 3, 3);
        assert!(message.contains("Developer confirmation"));
        assert!(message.contains("/plan continue"));
        assert!(message.contains("Plan repair guidance"));
    }

    #[test]
    fn automatic_plan_repair_uses_orchestrator_errors_before_a_final_decision() {
        let summary = "Orchestrator failed: config error: persisted plan not found: 019dbb06-b68c-72f3-b17a-47c46e7b7328";
        let report = execution_failure_report(None, Some(summary));

        assert!(report.contains("before producing a final decision"));
        assert!(report.contains("persisted plan not found"));
        let route = classify_execution_failure_revision(None, Some(summary));
        assert_eq!(route, ExecutionFailureRevision::ManualAction);
        assert!(!should_auto_repair_execution_failure(route, 0, 3));
        assert!(!should_auto_repair_execution_failure(route, 3, 3));

        let request = automatic_plan_repair_request_from_report(&report, 1, 3);
        assert!(request.contains("persisted plan not found"));
    }

    #[test]
    fn automatic_plan_repair_only_runs_for_plan_revision_route() {
        assert!(should_auto_repair_execution_failure(
            ExecutionFailureRevision::PlanRevision,
            0,
            3
        ));
        assert!(!should_auto_repair_execution_failure(
            ExecutionFailureRevision::IntentSpecRevision,
            0,
            3
        ));
        assert!(!should_auto_repair_execution_failure(
            ExecutionFailureRevision::ManualAction,
            0,
            3
        ));
    }

    #[test]
    fn execution_failure_classification_routes_plan_repairs_only_for_plan_failures() {
        let mut result = orchestrator_fixture();
        result.system_report.missing_artifacts.clear();
        result.system_report.artifacts_complete = true;

        assert_eq!(
            classify_execution_failure_revision(Some(&result), None),
            ExecutionFailureRevision::PlanRevision
        );
    }

    #[test]
    fn execution_failure_classification_routes_artifact_failure_to_intentspec_revision() {
        let result = orchestrator_fixture();

        assert_eq!(
            classify_execution_failure_revision(Some(&result), None),
            ExecutionFailureRevision::IntentSpecRevision
        );
    }

    #[test]
    fn execution_failure_classification_routes_policy_failures_to_intentspec_revision() {
        for code in [
            "scope-creep",
            "network-policy-deny",
            "tool-acl-deny",
            "sandbox-escalation-deny",
        ] {
            let mut result = orchestrator_fixture();
            result.system_report.missing_artifacts.clear();
            result.system_report.artifacts_complete = true;
            let task_result = result
                .task_results
                .first_mut()
                .expect("fixture should include a failed task result");
            task_result.policy_violations = vec![PolicyViolation {
                code: code.to_string(),
                message: "policy blocked execution".to_string(),
                tool_name: Some("shell".to_string()),
                path: None,
            }];

            assert_eq!(
                classify_execution_failure_revision(Some(&result), None),
                ExecutionFailureRevision::IntentSpecRevision,
                "policy code {code} should revise IntentSpec"
            );
        }
    }

    #[test]
    fn intentspec_revision_message_uses_intent_commands() {
        let message = intentspec_failure_revision_message_from_report(
            "Failed tasks: Gate failed.\nFailure details:\n- network policy denied",
        );

        assert!(message.contains("IntentSpec constraints"));
        assert!(message.contains("/intent modify <changes>"));
        assert!(message.contains("/intent cancel"));
        assert!(!message.contains("/plan modify"));
    }

    #[test]
    fn abandoned_execution_requires_plan_repair_loop() {
        let result = orchestrator_fixture();

        assert!(execution_requires_plan_repair(Some(&result)));
        let message = execution_failure_revision_message(Some(&result));

        assert!(message.contains("Plan execution failed"));
        assert!(message.contains("Decision: Abandon"));
        assert!(message.contains("Inspect sources"));
        assert!(message.contains("/plan cancel"));
    }

    #[test]
    fn failed_execution_revision_message_includes_gate_error_evidence() {
        let mut result = orchestrator_fixture();
        let task_result = result
            .task_results
            .first_mut()
            .expect("fixture should include a failed task result");
        task_result.gate_report = Some(GateReport {
            results: vec![GateResult {
                check_id: "cargo-check".to_string(),
                kind: "command".to_string(),
                passed: false,
                exit_code: 101,
                stdout: String::new(),
                stderr: "error[E0432]: unresolved import `clap`".to_string(),
                duration_ms: 42,
                timed_out: false,
            }],
            all_required_passed: false,
        });

        let message = execution_failure_revision_message(Some(&result));

        assert!(message.contains("Failure details:"));
        assert!(message.contains("cargo-check"));
        assert!(message.contains("unresolved import"));
        assert!(message.contains("Plan change hints:"));
        assert!(message.contains("compile error first"));
    }

    #[test]
    fn failed_execution_revision_message_prioritizes_dependency_network_hint() {
        let mut result = orchestrator_fixture();
        let task_result = result
            .task_results
            .first_mut()
            .expect("fixture should include a failed task result");
        task_result.gate_report = Some(GateReport {
            results: vec![GateResult {
                check_id: "build".to_string(),
                kind: "command".to_string(),
                passed: false,
                exit_code: 101,
                stdout: String::new(),
                stderr: "    Updating crates.io index".to_string(),
                duration_ms: 42,
                timed_out: false,
            }],
            all_required_passed: false,
        });

        let message = execution_failure_revision_message(Some(&result));

        assert!(message.contains("std-only implementation"));
        assert!(message.contains("dependency-policy:no-new"));
        assert!(!message.contains("compile error first"));
    }

    #[test]
    fn failed_execution_revision_message_includes_tool_and_policy_evidence() {
        let mut result = orchestrator_fixture();
        let task_result = result
            .task_results
            .first_mut()
            .expect("fixture should include a failed task result");
        task_result.tool_calls = vec![ToolCallRecord {
            tool_name: "run_libra_vcs".to_string(),
            action: "cargo run -- code".to_string(),
            arguments_json: None,
            paths_read: vec![],
            paths_written: vec![],
            success: false,
            summary: Some("command failed\nerror[E0599]: no variant named Code".to_string()),
            diffs: vec![],
        }];
        task_result.policy_violations = vec![PolicyViolation {
            code: "forbidden_command".to_string(),
            message: "run_libra_vcs may not execute cargo".to_string(),
            tool_name: Some("run_libra_vcs".to_string()),
            path: None,
        }];

        let message = execution_failure_revision_message(Some(&result));

        assert!(message.contains("tool failed: run_libra_vcs cargo run -- code"));
        assert!(message.contains("error[E0599]"));
        assert!(message.contains("policy violation: forbidden_command"));
        assert!(message.contains("compile error first"));
    }

    #[test]
    fn successful_or_reviewable_execution_does_not_force_plan_repair_loop() {
        let mut result = orchestrator_fixture();

        result.decision = DecisionOutcome::Commit;
        assert!(!execution_requires_plan_repair(Some(&result)));

        result.decision = DecisionOutcome::HumanReviewRequired;
        assert!(!execution_requires_plan_repair(Some(&result)));

        assert!(execution_requires_plan_repair(None));
    }

    #[test]
    fn failed_execution_revision_uses_latest_plan_tasks_as_draft() {
        let result = orchestrator_fixture();
        let draft = provider_plan_draft_from_plan(&result.execution_plan_spec);

        let titles = draft
            .steps
            .iter()
            .map(|step| step.title.as_str())
            .collect::<Vec<_>>();

        assert_eq!(titles, vec!["Inspect sources", "Run checks"]);
    }

    #[test]
    fn appends_to_last_matching_tool_group_before_streaming_cell() {
        let mut cells: Vec<Box<dyn HistoryCell>> = vec![
            Box::new(ToolCallHistoryCell::new(
                "1".to_string(),
                "read_file".to_string(),
                json!({"file_path":"src/main.rs"}),
            )),
            Box::new(AssistantHistoryCell::streaming()),
        ];

        assert!(append_to_last_tool_group_cell(
            &mut cells,
            "2".to_string(),
            "list_dir",
            json!({"dir_path":"src"}),
        ));

        let tool_cell = cells[0]
            .as_any()
            .downcast_ref::<ToolCallHistoryCell>()
            .expect("expected grouped tool cell");
        assert!(tool_cell.contains_call_id("1"));
        assert!(tool_cell.contains_call_id("2"));
    }

    #[test]
    fn does_not_append_across_non_tool_cells() {
        let mut cells: Vec<Box<dyn HistoryCell>> = vec![
            Box::new(ToolCallHistoryCell::new(
                "1".to_string(),
                "read_file".to_string(),
                json!({"file_path":"src/main.rs"}),
            )),
            Box::new(AssistantHistoryCell::new("note".to_string())),
            Box::new(AssistantHistoryCell::streaming()),
        ];

        assert!(!append_to_last_tool_group_cell(
            &mut cells,
            "2".to_string(),
            "list_dir",
            json!({"dir_path":"src"}),
        ));
    }

    #[test]
    fn appends_preview_to_last_matching_tool_group_before_streaming_cell() {
        let mut cells: Vec<Box<dyn HistoryCell>> = vec![
            Box::new(ToolCallHistoryCell::new_preview(
                "1".to_string(),
                "read_file".to_string(),
                json!({"file_path":"src/main.rs"}),
            )),
            Box::new(AssistantHistoryCell::streaming()),
        ];

        assert!(append_to_last_tool_group_preview_cell(
            &mut cells,
            "2".to_string(),
            "list_dir",
            json!({"dir_path":"src"}),
        ));

        let tool_cell = cells[0]
            .as_any()
            .downcast_ref::<ToolCallHistoryCell>()
            .expect("expected grouped tool cell");
        assert!(tool_cell.contains_call_id("1"));
        assert!(tool_cell.contains_call_id("2"));
    }

    #[test]
    fn marks_preview_tool_call_running_without_duplication() {
        let mut cells: Vec<Box<dyn HistoryCell>> =
            vec![Box::new(ToolCallHistoryCell::new_preview(
                "call_1".to_string(),
                "read_file".to_string(),
                json!({"file_path":"src/main.rs"}),
            ))];

        assert!(
            cells[0]
                .as_any()
                .downcast_ref::<ToolCallHistoryCell>()
                .expect("expected tool call cell")
                .contains_call_id("call_1")
        );
        assert!(mark_visible_tool_call_running(
            &mut cells,
            "call_1",
            "read_file",
            &json!({"file_path":"src/main.rs"}),
        ));

        let rendered = cells[0]
            .display_lines(100)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Exploring"));
        assert!(!rendered.contains("Preparing explore"));
    }

    #[test]
    fn orchestrator_result_markdown_uses_tables_and_sections() {
        let rendered = format_orchestrator_result(&orchestrator_fixture());

        assert!(rendered.contains("# Execution Result: Abandon"));
        assert!(rendered.contains("## Overview"));
        assert!(rendered.contains("| Field | Value |"));
        assert!(rendered.contains("## Verification"));
        assert!(rendered.contains("| Task | Status | Retries | Tools | Violations | Notes |"));
        assert!(rendered.contains("### Missing Artifacts"));
    }

    #[test]
    fn orchestrator_result_includes_graph_command_for_persisted_thread() {
        let thread_id = "11111111-1111-4111-8111-111111111111";
        let mut result = orchestrator_fixture();
        result.persistence = Some(PersistedExecution {
            thread_id: Some(thread_id.to_string()),
            run_id: "run-1".into(),
            ..PersistedExecution::default()
        });

        let rendered = format_orchestrator_result(&result);

        assert!(rendered.contains(&format!("| Graph | `libra graph {thread_id}` |")));
        assert_eq!(
            graph_thread_id_from_orchestrator_result(&result).as_deref(),
            Some(thread_id)
        );
    }

    #[test]
    fn orchestrator_result_thread_id_updates_session_graph_hint() {
        let thread_id = "11111111-1111-4111-8111-111111111111";
        let mut session = SessionState::new("/tmp/workspace");
        let mut result = orchestrator_fixture();
        result.persistence = Some(PersistedExecution {
            thread_id: Some(thread_id.to_string()),
            run_id: "run-1".into(),
            ..PersistedExecution::default()
        });

        record_orchestrator_thread_metadata(&mut session, &result);

        assert_eq!(
            session_graph_thread_id(&session).as_deref(),
            Some(thread_id)
        );
    }

    #[test]
    fn orchestrator_stage_notes_surface_later_phases() {
        let result = orchestrator_fixture();
        let plan = &result.execution_plan_spec;
        let report = &result.system_report;

        let compiled = format_plan_compiled_stage_note(plan);
        let executing = format_plan_execution_stage_note(plan.revision, plan.tasks.len());
        let verifying = format_system_verification_stage_note(plan, report);
        let replanning = format_replan_stage_note(1, 2, "gate failed", "split verification");
        let decision = format_decision_stage_note(plan, &result.decision);

        assert!(compiled.contains("Phase 1"));
        assert!(executing.contains("Phase 2"));
        assert!(executing.contains("Plan r4"));
        assert!(verifying.contains("Phase 3"));
        assert!(verifying.contains("Review: Fail"));
        assert!(replanning.contains("Plan r1 -> r2"));
        assert!(replanning.contains("split verification"));
        assert!(decision.contains("Phase 4"));
        assert!(decision.contains("Abandon"));
    }

    #[test]
    fn keeps_failed_tool_calls_visible() {
        let mut cell = ToolCallHistoryCell::new(
            "1".to_string(),
            "read_file".to_string(),
            json!({"file_path":"src/main.rs"}),
        );

        cell.complete_call("1", Err("file not found".to_string()));

        let rendered = cell.display_lines(100);
        let joined = rendered
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(joined.contains("Explore failed"));
        assert!(joined.contains("Read src/main.rs"));
        assert!(joined.contains("file not found"));
    }

    #[test]
    fn plan_revision_prompt_uses_current_spec_as_baseline() {
        let prompt = build_plan_revision_prompt(
            "{\n  \"kind\": \"IntentSpec\"\n}",
            "add an integration gate for cargo test",
        );

        assert!(prompt.contains("You are revising an existing IntentSpec."));
        assert!(prompt.contains("Current IntentSpec:"));
        assert!(prompt.contains("\"kind\": \"IntentSpec\""));
        assert!(prompt.contains("Requested changes:\nadd an integration gate for cargo test"));
        assert!(prompt.contains("submit_intent_draft exactly once"));
        assert!(prompt.contains("dependency-policy:no-new"));
        assert!(prompt.contains("prefer std::env"));
    }

    #[test]
    fn plain_developer_messages_are_routed_through_plan_workflow() {
        assert!(should_route_plain_message_to_plan(
            "init the project with cargo"
        ));
        assert!(should_route_plain_message_to_plan(
            "  fix the failing Ollama provider path"
        ));
        assert!(!should_route_plain_message_to_plan(""));
        assert!(!should_route_plain_message_to_plan("   "));
        assert!(!should_route_plain_message_to_plan(
            "/plan init the project"
        ));
        assert!(!should_route_plain_message_to_plan("/verify"));

        let prompt = build_plan_prompt("init the project with cargo");
        assert!(prompt.contains("request_user_input"));
        assert!(prompt.contains("submit_intent_draft exactly once"));
        assert!(prompt.contains("web_search"));
        assert!(prompt.contains("Rust edition 2024 is stable"));
        assert!(prompt.contains("dependency-policy:no-new"));
        assert!(prompt.contains("prefer std::env"));
    }

    #[test]
    fn quit_command_is_global_local_input() {
        assert!(is_global_quit_command_input("/quit"));
        assert!(is_global_quit_command_input("  /Quit now"));
        assert!(!is_global_quit_command_input("/mux quit"));
        assert!(!is_global_quit_command_input("quit"));
        assert!(!is_global_quit_command_input("/plan quit"));
    }

    #[test]
    fn terminal_paste_normalizes_crlf() {
        assert_eq!(
            normalize_terminal_paste_text("first\r\nsecond\rthird"),
            "first\nsecond\nthird"
        );
    }

    #[test]
    fn parses_pending_revision_builtin_commands() {
        assert!(matches!(
            parse_pending_plan_revision_command("modify tighten sandbox"),
            PendingPlanRevisionCommand::Modify("tighten sandbox")
        ));
        assert!(matches!(
            parse_pending_plan_revision_command("continue"),
            PendingPlanRevisionCommand::ContinueAutoRepair { max_attempts: None }
        ));
        assert!(matches!(
            parse_pending_plan_revision_command("continue 5"),
            PendingPlanRevisionCommand::ContinueAutoRepair {
                max_attempts: Some(5)
            }
        ));
        assert!(matches!(
            parse_pending_plan_revision_command("continue 10"),
            PendingPlanRevisionCommand::ContinueAutoRepair {
                max_attempts: Some(10)
            }
        ));
        assert!(matches!(
            parse_pending_plan_revision_command("continue 0"),
            PendingPlanRevisionCommand::Invalid
        ));
        assert!(matches!(
            parse_pending_plan_revision_command("continue 11"),
            PendingPlanRevisionCommand::Invalid
        ));
        assert!(matches!(
            parse_pending_plan_revision_command("revise add checks"),
            PendingPlanRevisionCommand::Modify("add checks")
        ));
        assert!(matches!(
            parse_pending_plan_revision_command("cancel"),
            PendingPlanRevisionCommand::Cancel
        ));
        assert!(matches!(
            parse_pending_plan_revision_command("status"),
            PendingPlanRevisionCommand::Invalid
        ));
    }

    #[test]
    fn pending_revision_help_mentions_escape_hatch() {
        let help = pending_plan_revision_help_message();
        assert!(help.contains("IntentSpec revise mode"));
        assert!(help.contains("/intent modify <changes>"));
        assert!(help.contains("/intent cancel"));
        assert!(!help.contains("/plan modify"));
    }

    #[test]
    fn intent_mismatch_message_mentions_current_and_stored_bindings() {
        let message = format_intentspec_target_mismatch(
            "/tmp/other",
            "1234567890abcdef",
            Some("feature/spec"),
            &super::IntentSpecBinding {
                workspace_key: "/tmp/current".into(),
                base_ref: "fedcba0987654321".into(),
                branch_label: Some("main".into()),
            },
        );

        assert!(message.contains("current · /tmp/current @ fedcba098765 (main)"));
        assert!(message.contains("stored · /tmp/other @ 1234567890ab (feature/spec)"));
        assert!(message.contains("different workspace or HEAD"));
    }

    #[test]
    fn managed_assistant_text_ignores_baseline_transcript_entries() {
        let now = chrono::Utc::now();
        let mut snapshot = initial_snapshot(
            "/tmp/libra",
            CodeUiProviderInfo {
                provider: "codex".to_string(),
                model: Some("gpt-5.4".to_string()),
                mode: Some("tui".to_string()),
                managed: true,
            },
            CodeUiCapabilities {
                streaming_text: true,
                ..CodeUiCapabilities::default()
            },
        );
        snapshot.status = CodeUiSessionStatus::Thinking;
        snapshot.transcript = vec![
            CodeUiTranscriptEntry {
                id: "old-assistant".to_string(),
                kind: CodeUiTranscriptEntryKind::AssistantMessage,
                title: Some("Assistant".to_string()),
                content: Some("old answer".to_string()),
                status: Some("completed".to_string()),
                streaming: false,
                metadata: serde_json::json!({}),
                created_at: now,
                updated_at: now,
            },
            CodeUiTranscriptEntry {
                id: "new-assistant".to_string(),
                kind: CodeUiTranscriptEntryKind::AssistantMessage,
                title: Some("Assistant".to_string()),
                content: Some("new answer".to_string()),
                status: Some("streaming".to_string()),
                streaming: true,
                metadata: serde_json::json!({}),
                created_at: now + chrono::Duration::seconds(1),
                updated_at: now + chrono::Duration::seconds(1),
            },
        ];

        let baseline = std::collections::HashSet::from(["old-assistant".to_string()]);

        assert_eq!(
            newest_managed_assistant_text(&snapshot, &baseline).as_deref(),
            Some("new answer")
        );
    }

    #[test]
    fn managed_approval_selection_maps_apply_to_future() {
        let interaction = CodeUiInteractionRequest {
            id: "approval-1".to_string(),
            kind: CodeUiInteractionKind::Approval,
            title: Some("Approval required".to_string()),
            description: None,
            prompt: Some("cargo test".to_string()),
            options: vec![
                CodeUiInteractionOption {
                    id: "approve".to_string(),
                    label: "Approve".to_string(),
                    description: None,
                },
                CodeUiInteractionOption {
                    id: "approve_all".to_string(),
                    label: "Approve All".to_string(),
                    description: None,
                },
                CodeUiInteractionOption {
                    id: "decline".to_string(),
                    label: "Decline".to_string(),
                    description: None,
                },
            ],
            status: CodeUiInteractionStatus::Pending,
            metadata: serde_json::json!({}),
            requested_at: chrono::Utc::now(),
            resolved_at: None,
        };

        let response = code_ui_response_from_managed_selection(&interaction, 1);

        assert_eq!(response.approved, Some(true));
        assert_eq!(
            response.apply_to_future,
            Some(CodeUiApplyToFuture::AcceptAll)
        );
        assert_eq!(response.selected_option.as_deref(), Some("approve_all"));
    }

    #[test]
    fn exec_approval_selection_maps_allow_all_commands() {
        assert_eq!(
            exec_approval_decision_from_selection(2, false, false),
            ReviewDecision::ApprovedForTtl
        );
        assert_eq!(
            exec_approval_decision_from_selection(3, false, false),
            ReviewDecision::ApprovedForDirectoryTtl
        );
        assert_eq!(
            exec_approval_decision_from_selection(4, false, false),
            ReviewDecision::ApprovedForPatternTtl
        );
        assert_eq!(
            exec_approval_decision_from_selection(5, false, false),
            ReviewDecision::ApprovedForAllCommands
        );
        assert_eq!(
            exec_approval_decision_from_selection(6, false, false),
            ReviewDecision::Denied
        );
        assert_eq!(
            exec_approval_decision_from_selection(7, false, false),
            ReviewDecision::Abort
        );
        assert_eq!(
            exec_approval_decision_from_selection(0, true, false),
            ReviewDecision::Approved
        );
        assert_eq!(
            exec_approval_decision_from_selection(0, false, true),
            ReviewDecision::ApprovedForAllCommands
        );
        assert_eq!(
            exec_approval_decision_from_selection(1, false, true),
            ReviewDecision::Denied
        );
        assert_eq!(
            exec_approval_decision_from_selection(2, false, true),
            ReviewDecision::Abort
        );
    }
}

fn assistant_entry_ids(snapshot: &CodeUiSessionSnapshot) -> HashSet<String> {
    snapshot
        .transcript
        .iter()
        .filter(|entry| entry.kind == CodeUiTranscriptEntryKind::AssistantMessage)
        .map(|entry| entry.id.clone())
        .collect()
}

fn newest_managed_assistant_text(
    snapshot: &CodeUiSessionSnapshot,
    baseline_assistant_ids: &HashSet<String>,
) -> Option<String> {
    snapshot
        .transcript
        .iter()
        .filter(|entry| entry.kind == CodeUiTranscriptEntryKind::AssistantMessage)
        .filter(|entry| !baseline_assistant_ids.contains(&entry.id))
        .max_by_key(|entry| (entry.created_at, entry.updated_at))
        .and_then(|entry| entry.content.clone())
}

fn managed_snapshot_is_terminal(snapshot: &CodeUiSessionSnapshot) -> bool {
    let has_pending_interaction = snapshot
        .interactions
        .iter()
        .any(|interaction| interaction.status == CodeUiInteractionStatus::Pending);
    !has_pending_interaction
        && matches!(
            snapshot.status,
            CodeUiSessionStatus::Idle
                | CodeUiSessionStatus::Completed
                | CodeUiSessionStatus::Error
                | CodeUiSessionStatus::IndeterminateSideEffect
        )
}

fn managed_agent_status(status: CodeUiSessionStatus) -> AgentStatus {
    match status {
        CodeUiSessionStatus::Thinking => AgentStatus::Thinking,
        CodeUiSessionStatus::ExecutingTool => AgentStatus::ExecutingTool,
        CodeUiSessionStatus::AwaitingInteraction => AgentStatus::AwaitingApproval,
        CodeUiSessionStatus::Idle
        | CodeUiSessionStatus::Completed
        | CodeUiSessionStatus::Error
        | CodeUiSessionStatus::IndeterminateSideEffect => AgentStatus::Idle,
    }
}

fn default_managed_interaction_options() -> Vec<CodeUiInteractionOption> {
    vec![
        CodeUiInteractionOption {
            id: "approve".to_string(),
            label: "Approve".to_string(),
            description: Some("Allow this request".to_string()),
        },
        CodeUiInteractionOption {
            id: "decline".to_string(),
            label: "Decline".to_string(),
            description: Some("Reject this request".to_string()),
        },
    ]
}

fn code_ui_response_from_managed_selection(
    interaction: &CodeUiInteractionRequest,
    selected: usize,
) -> CodeUiInteractionResponse {
    let option = interaction.options.get(selected);
    let selected_option = option.map(|option| option.id.clone());
    let decision_text = option
        .map(|option| format!("{} {}", option.id, option.label).to_ascii_lowercase())
        .unwrap_or_default();
    let approved = if decision_text.contains("approve")
        || decision_text.contains("accept")
        || decision_text.contains("allow")
    {
        Some(true)
    } else if decision_text.contains("decline")
        || decision_text.contains("deny")
        || decision_text.contains("reject")
        || decision_text.contains("no")
    {
        Some(false)
    } else {
        None
    };
    let apply_to_future = if decision_text.contains("all") || decision_text.contains("session") {
        match approved {
            Some(true) => Some(CodeUiApplyToFuture::AcceptAll),
            Some(false) => Some(CodeUiApplyToFuture::DeclineAll),
            None => None,
        }
    } else {
        None
    };

    CodeUiInteractionResponse {
        approved,
        apply_to_future,
        selected_option,
        ..CodeUiInteractionResponse::default()
    }
}

fn selection_from_interaction_options(
    options: &[CodeUiInteractionOption],
    response: &CodeUiInteractionResponse,
) -> Option<usize> {
    if let Some(selected_option) = response.selected_option.as_deref() {
        return options
            .iter()
            .position(|option| option.id == selected_option || option.label == selected_option);
    }
    let ids = options
        .iter()
        .map(|option| option.id.as_str())
        .collect::<Vec<_>>();
    selection_from_response(&ids, response, Some(0), Some(1))
}

fn selection_from_response(
    option_ids: &[&str],
    response: &CodeUiInteractionResponse,
    approved_index: Option<usize>,
    rejected_index: Option<usize>,
) -> Option<usize> {
    if let Some(selected_option) = response.selected_option.as_deref()
        && let Some(index) = option_ids.iter().position(|id| *id == selected_option)
    {
        return Some(index);
    }

    match response.approved {
        Some(true) => {
            if response.apply_to_future == Some(CodeUiApplyToFuture::AcceptAll)
                && let Some(index) = option_ids
                    .iter()
                    .position(|id| matches!(*id, "allow_all_commands" | "approve_session"))
            {
                return Some(index);
            }
            approved_index
        }
        Some(false) => rejected_index,
        None => None,
    }
}

fn user_input_answers_from_code_ui(
    pending: &PendingUserInput,
    response: CodeUiInteractionResponse,
) -> Result<HashMap<String, UserInputAnswer>, TuiControlError> {
    let mut answers = pending.answers.clone();
    if !response.answers.is_empty() {
        for (question_id, values) in response.answers {
            answers.insert(question_id, UserInputAnswer { answers: values });
        }
        return Ok(answers);
    }

    let Some(question) = pending.request.questions.get(pending.current_question) else {
        return Err(TuiControlError::InteractionNotActive);
    };

    let mut values = Vec::new();
    if let Some(selected_option) = response.selected_option.as_deref()
        && let Some(option_index) = selected_option
            .strip_prefix("option-")
            .and_then(|value| value.parse::<usize>().ok())
        && let Some(options) = question.options.as_ref()
        && let Some(option) = options.get(option_index)
    {
        values.push(option.label.clone());
    }
    if values.is_empty()
        && let Some(note) = response.note.as_deref().map(str::trim)
        && !note.is_empty()
    {
        values.push(note.to_string());
    }
    if values.is_empty() {
        match response.approved {
            Some(true) => values.push("yes".to_string()),
            Some(false) => values.push("no".to_string()),
            None => {}
        }
    }
    if values.is_empty() {
        return Err(TuiControlError::UnsupportedInteractionKind);
    }

    answers.insert(question.id.clone(), UserInputAnswer { answers: values });
    Ok(answers)
}

fn summarize_retry_error(error: &str) -> String {
    let lowered = error.to_ascii_lowercase();
    if lowered.contains("timeout") {
        "timeout".to_string()
    } else if lowered.contains("429") || lowered.contains("rate limit") {
        "rate limited".to_string()
    } else if lowered.contains("503") || lowered.contains("overloaded") {
        "upstream overloaded".to_string()
    } else if lowered.contains("connection") || lowered.contains("sending request") {
        "network issue".to_string()
    } else {
        "transient error".to_string()
    }
}

fn max_selectable_option(base: usize, is_other: bool) -> usize {
    if is_other {
        base
    } else {
        base.saturating_sub(1)
    }
}

async fn write_mcp_decision(
    mcp_server: Arc<LibraMcpServer>,
    run_id: String,
    decision_type: String,
    rationale: String,
) {
    let decision_params = CreateDecisionParams {
        run_id,
        decision_type,
        chosen_patchset_id: None,
        result_commit_sha: None,
        rationale: Some(rationale),
        checkpoint_id: None,
        tags: None,
        external_ids: None,
        actor_kind: Some("system".to_string()),
        actor_id: Some("libra-code".to_string()),
    };
    let actor = match mcp_server.resolve_actor_from_params(
        decision_params.actor_kind.as_deref(),
        decision_params.actor_id.as_deref(),
    ) {
        Ok(actor) => actor,
        Err(e) => {
            cli_error!(e, "error: failed to resolve actor for decision");
            return;
        }
    };

    match mcp_server
        .create_decision_impl(decision_params, actor)
        .await
    {
        Ok(result) => {
            if result.is_error.unwrap_or(false) {
                render_mcp_error("failed to create decision", result.content);
            }
        }
        Err(e) => {
            cli_error!(e, "error: failed to create decision");
        }
    }
}

fn summarize_tool_output(output: &ToolOutput) -> String {
    let raw = match output {
        ToolOutput::Function { content, .. } => content.as_str().trim().to_string(),
        ToolOutput::Mcp { result } => serde_json::to_string(result).unwrap_or_default(),
    };
    const MAX_LEN: usize = 240;
    if raw.chars().count() <= MAX_LEN {
        raw
    } else {
        let mut truncated: String = raw.chars().take(MAX_LEN).collect();
        truncated.push_str("...");
        truncated
    }
}

fn build_plan_prompt(request: &str) -> String {
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

fn build_plan_revision_prompt(spec_json: &str, request: &str) -> String {
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

fn build_execution_plan_prompt(spec_json: &str) -> String {
    format!(
        "You are generating an execution plan for an already confirmed IntentSpec.\n\
Use read-only repository tools if needed, then call submit_plan_draft exactly once with the full ordered draft.\n\
Use web_search when available before making version-sensitive external claims. Rust edition 2024 is stable in current Rust; do not reject Cargo.toml edition=\"2024\" unless local toolchain evidence proves it unsupported.\n\
Every draft step must be a concrete execution task the agent can perform. Provide ordered steps with title only; do not include runtime status.\n\
Do not call submit_intent_draft. Do not modify the IntentSpec. Do not execute commands that change files.\n\
After calling submit_plan_draft, stop; the developer must confirm the compiled plan before execution.\n\n\
Confirmed IntentSpec:\n```json\n{spec_json}\n```"
    )
}

fn build_execution_plan_revision_prompt(
    spec_json: &str,
    current_plan: &ProviderPlanDraft,
    request: &str,
    failure_report: Option<&str>,
) -> String {
    let current_plan_json = provider_plan_draft_json(current_plan);
    let failure_context = failure_report
        .map(str::trim)
        .filter(|report| !report.is_empty())
        .map(|report| format!("\n\nPrevious execution failure evidence:\n```text\n{report}\n```"))
        .unwrap_or_default();
    format!(
        "You are revising an execution plan for an already confirmed IntentSpec.\n\
Use the current draft as the baseline. Do not start over from the IntentSpec unless the evidence proves the current draft is unusable.\n\
Apply only the developer's requested changes, then call submit_plan_draft exactly once with the complete revised ordered draft.\n\
When previous execution failure evidence is provided, use it as hard context for the revised plan: the next plan's primary goal is to repair that specific failed execution; it addresses the concrete failure and then continues toward the original Intent.\n\
Keep successful prior steps only when they are still prerequisites for fixing the failure; add or reorder concrete repair and verification steps around the failing point instead of repeating the same approach.\n\
Use web_search when available before making version-sensitive external claims. Rust edition 2024 is stable in current Rust; do not reject Cargo.toml edition=\"2024\" unless local toolchain evidence proves it unsupported.\n\
Every draft step must be a concrete execution task the agent can perform. Provide ordered steps with title only; do not include runtime status.\n\
Do not call submit_intent_draft. Do not revise the IntentSpec. Do not execute commands that change files.\n\
After calling submit_plan_draft, stop; the developer must confirm the compiled plan before execution.\n\n\
Confirmed IntentSpec:\n```json\n{spec_json}\n```\n\n\
Current execution plan:\n```json\n{current_plan_json}\n```\n\n\
Requested plan changes:\n{request}{failure_context}"
    )
}

fn provider_plan_draft_from_plan(plan: &ExecutionPlanSpec) -> ProviderPlanDraft {
    ProviderPlanDraft {
        explanation: plan
            .replan_reason
            .as_ref()
            .map(|reason| reason.trim().to_string())
            .filter(|reason| !reason.is_empty()),
        steps: plan
            .tasks
            .iter()
            .map(|task| ProviderPlanDraftStep {
                title: task.title().trim().to_string(),
            })
            .filter(|step| !step.title.is_empty())
            .collect(),
    }
}

fn provider_plan_draft_from_spec(spec: &IntentSpec) -> ProviderPlanDraft {
    let mut steps = spec
        .intent
        .objectives
        .iter()
        .map(|objective| ProviderPlanDraftStep {
            title: objective.title.trim().to_string(),
        })
        .filter(|step| !step.title.is_empty())
        .collect::<Vec<_>>();

    if steps.is_empty() {
        let fallback_title = spec.intent.summary.trim();
        steps.push(ProviderPlanDraftStep {
            title: if fallback_title.is_empty() {
                "Revise execution plan".to_string()
            } else {
                fallback_title.to_string()
            },
        });
    }

    ProviderPlanDraft {
        explanation: Some("Recovered from the current IntentSpec.".to_string()),
        steps,
    }
}

fn provider_plan_draft_json(args: &ProviderPlanDraft) -> serde_json::Value {
    serde_json::json!({
        "explanation": args.explanation.clone(),
        "steps": args.steps.iter().map(|step| {
            serde_json::json!({
                "title": step.title.as_str(),
            })
        }).collect::<Vec<_>>(),
    })
}

fn execution_requires_plan_repair(result: Option<&OrchestratorResult>) -> bool {
    result.is_none_or(|result| result.decision == DecisionOutcome::Abandon)
}

#[cfg(test)]
fn execution_failure_revision_message(result: Option<&OrchestratorResult>) -> String {
    execution_failure_revision_message_from_report(&execution_failure_report(result, None))
}

fn execution_failure_revision_message_from_report(report: &str) -> String {
    let mut lines = vec![
        "Plan execution failed. Revise the execution plan before running it again.".to_string(),
    ];
    lines.extend(
        report
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string),
    );
    lines.push(
        "Reply `continue` or `/plan continue <max-attempts>` to allow automatic repair attempts, send plan changes as plain text or `/plan modify <changes>`, or use `/plan cancel` to stop."
            .to_string(),
    );
    lines.join("\n")
}

fn intentspec_failure_revision_message_from_report(report: &str) -> String {
    let mut lines = vec![
        "Plan execution failed because the current IntentSpec constraints or required artifacts need revision before another plan repair can help.".to_string(),
    ];
    lines.extend(
        report
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string),
    );
    lines.push(
        "Revise the IntentSpec scope, network policy, tool ACL, or artifact requirements with plain text or `/intent modify <changes>`. Use `/intent cancel` to stop."
            .to_string(),
    );
    lines.join("\n")
}

fn manual_execution_failure_message_from_report(report: &str) -> String {
    let mut lines =
        vec!["Plan execution failed before automatic plan repair can make progress.".to_string()];
    lines.extend(
        report
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string),
    );
    lines.push(
        "Resolve the configuration, MCP, persistence, or external blocker, then rerun `/intent execute` or revise the plan/spec explicitly."
            .to_string(),
    );
    lines.join("\n")
}

fn repair_message_for_execution_failure(
    route: ExecutionFailureRevision,
    report: &str,
    can_auto_repair: bool,
    automatic_repair_attempts: u8,
    automatic_repair_max_attempts: u8,
) -> String {
    match route {
        ExecutionFailureRevision::PlanRevision if can_auto_repair => {
            automatic_plan_repair_started_message(
                automatic_repair_attempts.saturating_add(1),
                automatic_repair_max_attempts,
            )
        }
        ExecutionFailureRevision::PlanRevision
            if automatic_repair_attempts >= automatic_repair_max_attempts
                && automatic_repair_max_attempts > 0 =>
        {
            automatic_plan_repair_threshold_message(
                report,
                automatic_repair_attempts,
                automatic_repair_max_attempts,
            )
        }
        ExecutionFailureRevision::PlanRevision => {
            execution_failure_revision_message_from_report(report)
        }
        ExecutionFailureRevision::IntentSpecRevision => {
            intentspec_failure_revision_message_from_report(report)
        }
        ExecutionFailureRevision::ManualAction => {
            manual_execution_failure_message_from_report(report)
        }
    }
}

fn execution_failure_report(
    result: Option<&OrchestratorResult>,
    execution_summary: Option<&str>,
) -> String {
    let mut lines = Vec::new();
    if let Some(result) = result {
        lines.push(format!(
            "Decision: {}.",
            orchestrator_decision_label(&result.decision)
        ));
        let failed_tasks = result
            .task_results
            .iter()
            .filter(|task_result| task_result.status == TaskNodeStatus::Failed)
            .filter_map(|task_result| {
                result
                    .execution_plan_spec
                    .tasks
                    .iter()
                    .find(|task| task.id() == task_result.task_id)
                    .map(|task| task.title().trim().to_string())
            })
            .filter(|title| !title.is_empty())
            .take(3)
            .collect::<Vec<_>>();
        if !failed_tasks.is_empty() {
            lines.push(format!("Failed tasks: {}.", failed_tasks.join(", ")));
        }
        let diagnostics = execution_failure_diagnostics(result, 3);
        if !diagnostics.is_empty() {
            lines.push("Failure details:".to_string());
            lines.extend(
                diagnostics
                    .into_iter()
                    .map(|diagnostic| format!("- {diagnostic}")),
            );
        }
        let repair_hints = execution_failure_repair_hints(result, 3);
        if !repair_hints.is_empty() {
            lines.push("Plan change hints:".to_string());
            lines.extend(repair_hints.into_iter().map(|hint| format!("- {hint}")));
        }
    } else {
        lines.push("The orchestrator failed before producing a final decision.".to_string());
        if let Some(error) = orchestrator_failure_detail(execution_summary) {
            lines.push(format!("Orchestrator error: {error}."));
            lines.push(
                "Plan change hints:\n- Add or adjust a repair step that restores the missing precondition before retrying the failed execution stage."
                    .to_string(),
            );
        }
    }

    lines.join("\n")
}

fn should_auto_repair_execution_failure(
    route: ExecutionFailureRevision,
    automatic_repair_attempts: u8,
    automatic_repair_max_attempts: u8,
) -> bool {
    automatic_repair_attempts < automatic_repair_max_attempts
        && route == ExecutionFailureRevision::PlanRevision
}

fn classify_execution_failure_revision(
    result: Option<&OrchestratorResult>,
    execution_summary: Option<&str>,
) -> ExecutionFailureRevision {
    if let Some(result) = result {
        if execution_failure_requires_intentspec_revision(result) {
            return ExecutionFailureRevision::IntentSpecRevision;
        }
        return if result.decision == DecisionOutcome::Abandon {
            ExecutionFailureRevision::PlanRevision
        } else {
            ExecutionFailureRevision::ManualAction
        };
    }

    if let Some(detail) = orchestrator_failure_detail(execution_summary)
        && orchestrator_failure_requires_manual_action(&detail)
    {
        return ExecutionFailureRevision::ManualAction;
    }

    ExecutionFailureRevision::ManualAction
}

fn execution_failure_requires_intentspec_revision(result: &OrchestratorResult) -> bool {
    !result.system_report.missing_artifacts.is_empty()
        || result.task_results.iter().any(|task_result| {
            task_result
                .policy_violations
                .iter()
                .any(policy_violation_requires_intentspec_revision)
        })
}

fn policy_violation_requires_intentspec_revision(violation: &PolicyViolation) -> bool {
    matches!(
        violation.code.as_str(),
        "scope-creep"
            | "network-policy-deny"
            | "tool-acl-deny"
            | "sandbox-escalation-deny"
            | "git-version-control-deny"
    )
}

fn orchestrator_failure_requires_manual_action(detail: &str) -> bool {
    let detail = detail.to_ascii_lowercase();
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
    .any(|needle| detail.contains(needle))
}

fn orchestrator_failure_detail(execution_summary: Option<&str>) -> Option<String> {
    execution_summary
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .and_then(|summary| summary.strip_prefix("Orchestrator failed:"))
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
        .map(str::to_string)
}

fn automatic_plan_repair_started_message(attempt: u8, max_attempts: u8) -> String {
    format!(
        "Plan execution failed. Automatic plan repair attempt {} is feeding the failure evidence back to the planner.",
        automatic_plan_repair_attempt_label(attempt, max_attempts)
    )
}

fn automatic_plan_repair_request_from_report(
    report: &str,
    attempt: u8,
    max_attempts: u8,
) -> String {
    format!(
        "Automatic plan repair attempt {}.\n\
The previous plan execution failed. The next plan must be a repair of that failed execution, not a fresh plan from the original Intent.\n\
Revise the execution plan by adding, replacing, or reordering concrete repair Step(s) at the failing point before the failed verification.\n\
Return a complete ordered execution plan draft, but preserve successful prior steps only when they are still prerequisites for the repair.\n\
Keep verification steps that can judge success automatically, and add a focused regression check for the observed failure when possible.\n\
Use this failure evidence as the source of truth:\n{report}",
        automatic_plan_repair_attempt_label(attempt, max_attempts)
    )
}

fn automatic_plan_repair_threshold_message(report: &str, attempts: u8, max_attempts: u8) -> String {
    format!(
        "Automatic plan repair stopped after {attempts} failed repair attempts (automatic threshold: {max_attempts}).\n\
Developer confirmation is required before more automatic correction.\n\
{report}\n\
Reply `continue` or `/plan continue <max-attempts>` to allow more automatic repair attempts, describe specific Plan repair guidance, or use `/plan cancel` to stop."
    )
}

fn automatic_plan_repair_attempt_label(attempt: u8, max_attempts: u8) -> String {
    if attempt <= max_attempts {
        format!("{attempt}/{max_attempts}")
    } else {
        format!("{attempt} (threshold {max_attempts})")
    }
}

fn execution_failure_diagnostics(result: &OrchestratorResult, limit: usize) -> Vec<String> {
    result
        .task_results
        .iter()
        .filter(|task_result| task_result.status == TaskNodeStatus::Failed)
        .filter_map(|task_result| {
            let (task_index, title) = execution_task_label(result, task_result.task_id)?;
            let details = task_failure_details(task_result);
            if details.is_empty() {
                return Some(format!(
                    "{task_index} {title}: no structured failure evidence was captured."
                ));
            }
            Some(format!("{task_index} {title}: {}", details.join("; ")))
        })
        .take(limit)
        .collect()
}

fn execution_failure_repair_hints(result: &OrchestratorResult, limit: usize) -> Vec<String> {
    let mut hints = Vec::new();
    for task_result in result
        .task_results
        .iter()
        .filter(|task_result| task_result.status == TaskNodeStatus::Failed)
    {
        let Some((task_index, title)) = execution_task_label(result, task_result.task_id) else {
            continue;
        };
        let details = task_failure_details(task_result);
        let failure_text = format!("{} {}", title, details.join(" "));
        let Some(hint) = plan_repair_hint_for_failure(&failure_text) else {
            continue;
        };
        let formatted = format!("{task_index}: {hint}");
        if !hints.contains(&formatted) {
            hints.push(formatted);
        }
        if hints.len() >= limit {
            break;
        }
    }

    if hints.is_empty()
        && result
            .task_results
            .iter()
            .any(|task_result| task_result.status == TaskNodeStatus::Failed)
    {
        hints.push(
            "Add an explicit required check that captures the failing command output before validation, then split the implementation work so that check can pass."
                .to_string(),
        );
    }

    hints
}

fn execution_task_label(
    result: &OrchestratorResult,
    task_id: uuid::Uuid,
) -> Option<(String, String)> {
    result
        .execution_plan_spec
        .tasks
        .iter()
        .enumerate()
        .find(|(_, task)| task.id() == task_id)
        .map(|(idx, task)| {
            let kind = match task.kind {
                TaskKind::Implementation => "I",
                TaskKind::Analysis => "A",
                TaskKind::Gate => "G",
            };
            (
                format!("{kind}{:02}", idx + 1),
                task.title().trim().to_string(),
            )
        })
}

fn task_failure_details(
    task_result: &crate::internal::ai::orchestrator::types::TaskResult,
) -> Vec<String> {
    let mut details = Vec::new();
    if let Some(reason) = summarize_failed_gate_report(task_result.gate_report.as_ref()) {
        details.push(format!("gate failed: {reason}"));
    }
    if let Some(reason) = summarize_failed_tool_calls(task_result) {
        details.push(format!("tool failed: {reason}"));
    }
    if let Some(reason) = summarize_policy_violations(task_result) {
        details.push(format!("policy violation: {reason}"));
    }
    if let Some(review) = task_result.review.as_ref()
        && (!review.approved || !review.issues.is_empty())
    {
        let issue = review
            .issues
            .iter()
            .find_map(|issue| concise_failure_line(issue))
            .or_else(|| concise_failure_line(&review.summary));
        if let Some(issue) = issue {
            details.push(format!("review: {issue}"));
        }
    }
    if let Some(reason) = task_result
        .agent_output
        .as_deref()
        .and_then(concise_failure_line)
    {
        details.push(format!("agent output: {reason}"));
    }

    dedupe_preserving_order(details)
}

fn summarize_failed_tool_calls(
    task_result: &crate::internal::ai::orchestrator::types::TaskResult,
) -> Option<String> {
    let summaries = task_result
        .tool_calls
        .iter()
        .filter(|tool_call| !tool_call.success)
        .filter_map(|tool_call| {
            let action = tool_call.action.trim();
            let label = if action.is_empty() {
                tool_call.tool_name.trim().to_string()
            } else {
                format!("{} {}", tool_call.tool_name.trim(), action)
            };
            tool_call
                .summary
                .as_deref()
                .and_then(concise_failure_line)
                .map(|summary| format!("{label}: {summary}"))
                .or_else(|| (!label.trim().is_empty()).then_some(label))
        })
        .take(2)
        .collect::<Vec<_>>();

    (!summaries.is_empty()).then(|| summaries.join("; "))
}

fn summarize_policy_violations(
    task_result: &crate::internal::ai::orchestrator::types::TaskResult,
) -> Option<String> {
    let summaries = task_result
        .policy_violations
        .iter()
        .filter_map(|violation| {
            let message = concise_failure_line(&violation.message)?;
            let mut parts = vec![violation.code.trim().to_string(), message];
            if let Some(tool_name) = violation
                .tool_name
                .as_deref()
                .map(str::trim)
                .filter(|tool_name| !tool_name.is_empty())
            {
                parts.push(format!("tool {tool_name}"));
            }
            if let Some(path) = violation
                .path
                .as_deref()
                .map(str::trim)
                .filter(|path| !path.is_empty())
            {
                parts.push(format!("path {path}"));
            }
            Some(parts.join(" · "))
        })
        .take(2)
        .collect::<Vec<_>>();

    (!summaries.is_empty()).then(|| summaries.join("; "))
}

fn plan_repair_hint_for_failure(failure_text: &str) -> Option<String> {
    let lower = failure_text.to_lowercase();
    if contains_any(
        &lower,
        &[
            "updating crates.io index",
            "download of config.json failed",
            "failed to get",
            "crates.io",
            "dependency-policy:no-new",
        ],
    ) {
        return Some(
            "remove any unrequested third-party dependency and prefer a std-only implementation under dependency-policy:no-new; only use networked dependency resolution after the developer explicitly allows network/dependency changes."
                .to_string(),
        );
    }
    if contains_any(
        &lower,
        &[
            "cargo",
            "rustc",
            "compile",
            "compiles",
            "compiler",
            "clippy",
            "error:",
            "error[",
            "cannot find",
            "unresolved",
            "mismatched",
        ],
    ) {
        return Some(
            "make the next plan fix the compile error first, then run the cargo/check gate again before any functional echo checks."
                .to_string(),
        );
    }
    if contains_any(&lower, &["echo", "subcommand", "command output", "stdout"]) {
        return Some(
            "split subcommand implementation from output verification, and make each expected command/output pair explicit."
                .to_string(),
        );
    }
    if contains_any(
        &lower,
        &["policy", "violation", "forbidden", "denied", "network"],
    ) {
        return Some(
            "remove the forbidden tool/path/network access from the plan, or choose an allowed access policy before rerunning."
                .to_string(),
        );
    }
    None
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn concise_failure_line(text: &str) -> Option<String> {
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let selected = [
        &[
            "error:",
            "error[",
            "cannot find",
            "unresolved",
            "mismatched",
        ][..],
        &["expected", "not found", "denied", "violation"][..],
        &["failed"][..],
    ]
    .iter()
    .find_map(|markers| {
        lines.iter().copied().find(|line| {
            let lower = line.to_lowercase();
            contains_any(&lower, markers)
        })
    })
    .or_else(|| lines.first().copied())?;
    Some(truncate_chars(selected, 180))
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx >= max_chars {
            output.push_str("...");
            return output;
        }
        output.push(ch);
    }
    output
}

fn dedupe_preserving_order(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            deduped.push(item);
        }
    }
    deduped
}

async fn persist_phase0_intent_for_review(
    spec: &IntentSpec,
    mcp_server: &Arc<LibraMcpServer>,
) -> anyhow::Result<String> {
    let outcome = write_intent(spec, mcp_server).await?;
    Ok(outcome.intent_id)
}

async fn persist_phase0_context_snapshot_for_review(
    spec: &IntentSpec,
    working_dir: &Path,
    mcp_server: &Arc<LibraMcpServer>,
) -> anyhow::Result<Option<String>> {
    let actor = mcp_server
        .resolve_actor_from_params(Some("system"), Some("libra-plan"))
        .map_err(|error| anyhow::anyhow!("failed to resolve actor for ContextSnapshot: {error}"))?;
    let Some(request) = phase0_context_snapshot_request_from_changed_files(
        spec,
        changed_files_from_status(working_dir),
        actor,
    ) else {
        return Ok(None);
    };

    let outcome = write_context_snapshot_if_needed(request, mcp_server).await?;
    Ok(outcome.map(|snapshot| snapshot.snapshot_id))
}

fn phase0_context_snapshot_request_from_changed_files(
    spec: &IntentSpec,
    changed_files: Vec<String>,
    actor: git_internal::internal::object::types::ActorRef,
) -> Option<ContextSnapshotRequest> {
    let items = changed_files
        .into_iter()
        .filter_map(|path| {
            let path = path.trim().to_string();
            (!path.is_empty()).then_some(ContextSnapshotItem {
                kind: Some("file".to_string()),
                path,
                preview: None,
                blob_hash: None,
            })
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        return None;
    }

    Some(ContextSnapshotRequest {
        summary: Some(format!(
            "Phase 0 worktree context for IntentSpec {}: {} changed paths",
            spec.metadata.id,
            items.len()
        )),
        items,
        selection_strategy: SelectionStrategy::Heuristic,
        actor,
    })
}

fn intentspec_with_plan_draft_objectives(
    spec: &IntentSpec,
    plan_draft: &ProviderPlanDraft,
) -> Result<IntentSpec, String> {
    let objectives = plan_draft
        .steps
        .iter()
        .filter_map(|step| {
            let title = step.title.trim();
            (!title.is_empty()).then(|| Objective {
                title: title.to_string(),
                kind: if spec.intent.has_implementation_objectives() {
                    ObjectiveKind::Implementation
                } else {
                    ObjectiveKind::Analysis
                },
            })
        })
        .collect::<Vec<_>>();

    if objectives.is_empty() {
        return Err("LLM plan did not include any non-empty steps".to_string());
    }

    let mut planned_spec = spec.clone();
    planned_spec.intent.objectives = objectives;
    let mut libra = planned_spec.libra.take().unwrap_or(LibraBinding {
        object_store: None,
        context_pipeline: None,
        plan_generation: None,
        run_policy: None,
        actor_mapping: None,
        decision_policy: None,
    });
    let plan_generation = libra
        .plan_generation
        .get_or_insert_with(PlanGenerationConfig::default);
    plan_generation.decomposition_mode = DecompositionMode::PerObjective;
    plan_generation.conflict_resolution = ConflictResolution::ForceSerial;
    planned_spec.libra = Some(libra);
    Ok(planned_spec)
}

fn provider_plan_draft_from_args(args: SubmitPlanDraftArgs) -> Result<ProviderPlanDraft, String> {
    let steps = args
        .steps
        .into_iter()
        .map(|step| ProviderPlanDraftStep {
            title: step.title.trim().to_string(),
        })
        .collect::<Vec<_>>();
    if steps.is_empty() || steps.iter().any(|step| step.title.is_empty()) {
        return Err("plan draft must include at least one non-empty step title".to_string());
    }
    Ok(ProviderPlanDraft {
        explanation: args.explanation.and_then(|explanation| {
            let trimmed = explanation.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        }),
        steps,
    })
}

fn network_access_label(network_access: &NetworkAccess) -> &'static str {
    match network_access {
        NetworkAccess::Denied => "denied",
        NetworkAccess::Allowlist { .. } => "allowlist",
        NetworkAccess::Full => "full",
    }
}

fn apply_developer_network_access(spec: &mut IntentSpec, network_access: bool) {
    spec.constraints.security.network_policy = if network_access {
        NetworkPolicy::Allow
    } else {
        NetworkPolicy::Deny
    };
}

fn network_policy_interaction_id(plan_id: Option<&str>) -> String {
    match plan_id {
        Some(plan_id) => format!("{plan_id}:network-policy"),
        None => "post-plan-network-policy".to_string(),
    }
}

fn is_phase1_plan_draft_tool(tool_name: &str) -> bool {
    tool_name == "submit_plan_draft"
}

fn should_forward_phase1_model_text_delta(_delta: &str) -> bool {
    false
}

fn phase0_plan_tool_loop_config(mut config: ToolLoopConfig) -> ToolLoopConfig {
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

fn phase1_plan_tool_loop_config(mut config: ToolLoopConfig) -> ToolLoopConfig {
    config.allowed_tools = Some(vec![
        "read_file".to_string(),
        "list_dir".to_string(),
        "grep_files".to_string(),
        "search_files".to_string(),
        "web_search".to_string(),
        "submit_plan_draft".to_string(),
    ]);
    config.terminal_tools = Some(vec!["submit_plan_draft".to_string()]);
    config.max_turns = Some(12);
    config
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FirstTurnIntentPolicyUpdate {
    PromptOnly,
    PromptAndAllowedTools,
}

fn should_auto_classify_first_user_message(
    enabled: bool,
    history: &[Message],
    requested_allowed_tools: Option<&Vec<String>>,
) -> bool {
    enabled && history.is_empty() && requested_allowed_tools.is_none()
}

async fn classify_first_turn_task_intent<M>(
    model: &M,
    registry: &ToolRegistry,
    config: &mut ToolLoopConfig,
    user_text: &str,
    policy_update: FirstTurnIntentPolicyUpdate,
) -> Result<TaskIntentDecision, TaskIntentClassifierError>
where
    M: CompletionModel,
{
    let classifier = TaskIntentClassifier::new(model.clone());
    let decision = classifier
        .classify(TaskIntentClassificationRequest::new(user_text))
        .await?;
    apply_task_intent_prompt(config, registry, decision.intent);
    if policy_update == FirstTurnIntentPolicyUpdate::PromptAndAllowedTools {
        config.allowed_tools = Some(registry.filter_by_intent(decision.intent));
    }
    Ok(decision)
}

fn apply_task_intent_prompt(
    config: &mut ToolLoopConfig,
    registry: &ToolRegistry,
    intent: TaskIntent,
) {
    config.preamble = Some(
        SystemPromptBuilder::new(registry.working_dir())
            .with_intent(intent)
            .with_dynamic_context()
            .build(),
    );
}

fn attach_memory_anchor_prompt_context(config: &mut ToolLoopConfig) {
    let Some(session_root) = config.context_frame_session_root.as_deref() else {
        return;
    };

    let replay = match SessionJsonlStore::new(session_root.to_path_buf()).load_memory_anchors() {
        Ok(replay) => replay,
        Err(error) => {
            tracing::warn!(
                session_root = %session_root.display(),
                error = %error,
                "failed to load memory anchors for prompt context"
            );
            return;
        }
    };
    let anchors = replay.anchors();
    let Some(section) = build_memory_anchor_prompt_section(&anchors, Utc::now()) else {
        return;
    };

    match config.preamble.as_mut() {
        Some(preamble) if !preamble.trim().is_empty() => {
            preamble.push_str("\n\n");
            preamble.push_str(&section);
        }
        _ => {
            config.preamble = Some(section);
        }
    }
}

fn send_task_intent_classified_event(
    tx: &UnboundedSender<AppEvent>,
    turn_id: TurnId,
    decision: TaskIntentDecision,
    config: &ToolLoopConfig,
    registry: &ToolRegistry,
    goal_active: bool,
) {
    let preamble = config.preamble.clone().unwrap_or_else(|| {
        SystemPromptBuilder::new(registry.working_dir())
            .with_intent(decision.intent)
            .with_dynamic_context()
            .build()
    });
    let mut allowed_tools = registry.filter_by_intent(decision.intent);
    apply_goal_tool_visibility(&mut allowed_tools, goal_active, registry);
    let _ = tx.send(AppEvent::TaskIntentClassified {
        turn_id,
        intent: decision.intent,
        preamble,
        allowed_tools,
    });
}

fn is_default_chat_tool(tool_name: &str) -> bool {
    !matches!(
        tool_name,
        "submit_intent_draft"
            | "submit_plan_draft"
            | "update_goal_progress"
            | "submit_goal_complete"
    )
}

fn default_chat_allowed_tools(registry: &ToolRegistry, config: &ToolLoopConfig) -> Vec<String> {
    config.allowed_tools.clone().unwrap_or_else(|| {
        registry
            .tool_specs()
            .into_iter()
            .map(|s| s.function.name)
            .filter(|name| is_default_chat_tool(name))
            .collect()
    })
}

fn apply_goal_tool_visibility(
    allowed_tools: &mut Vec<String>,
    goal_active: bool,
    registry: &ToolRegistry,
) {
    if goal_active {
        for tool_name in ["update_goal_progress", "submit_goal_complete"] {
            if registry.contains_tool(tool_name)
                && !allowed_tools.iter().any(|name| name == tool_name)
            {
                allowed_tools.push(tool_name.to_string());
            }
        }
    } else {
        allowed_tools.retain(|name| !is_goal_tool(name));
    }
}

fn bind_goal_stop_policy(config: &mut ToolLoopConfig, goal_id: uuid::Uuid) -> GoalStopPolicy {
    let policy = GoalStopPolicy::GoalBound { goal_id };
    config.goal_stop_policy = Some(policy);
    policy
}

fn is_goal_tool(tool_name: &str) -> bool {
    matches!(tool_name, "update_goal_progress" | "submit_goal_complete")
}

fn ensure_terminal_tool_name(config: &mut ToolLoopConfig, tool_name: &str) {
    let terminal_tools = config.terminal_tools.get_or_insert_with(Vec::new);
    if !terminal_tools.iter().any(|name| name == tool_name) {
        terminal_tools.push(tool_name.to_string());
    }
}

fn persist_goal_events_to_session_root(
    session_root: PathBuf,
    events: &[GoalEventEnvelope],
) -> std::io::Result<()> {
    let store = SessionJsonlStore::new(session_root);
    for event in events {
        store.append(&SessionEvent::goal(event.clone()))?;
    }
    Ok(())
}

/// Replay the resumed session's Goal envelope stream into a fresh
/// [`super::goal_session::GoalSession`].
///
/// Returns `None` when:
/// * The session JSONL is missing or has no [`SessionEvent::Goal`]
///   envelopes (the resumed thread never started a Goal).
/// * The envelope stream fails `goal::state::replay`'s shape checks
///   (no leading `Created`, mismatched `goal_id`, invalid spec).
///
/// On success the projected state is logged together with the
/// rejection count so a forged or corrupted JSONL slice surfaces
/// in `tracing` instead of silently producing a nonsense state.
/// I/O failures are downgraded to a warn-level trace and an
/// empty-replay return because the resume path must not abort on
/// a broken events.jsonl.
fn replay_goal_session_from_session_root(
    session_root: &Path,
) -> Option<super::goal_session::GoalSession> {
    let store = SessionJsonlStore::new(session_root.to_path_buf());
    match store.has_events() {
        Ok(true) => {}
        Ok(false) => return None,
        Err(error) => {
            tracing::warn!(
                error = %error,
                session_root = %session_root.display(),
                "failed to probe session events while replaying active Goal on resume — continuing without Goal session"
            );
            return None;
        }
    }
    let events = match store.load_events() {
        Ok(events) => events,
        Err(error) => {
            tracing::warn!(
                error = %error,
                session_root = %session_root.display(),
                "failed to load session events while replaying active Goal on resume — continuing without Goal session"
            );
            return None;
        }
    };
    let envelopes: Vec<GoalEventEnvelope> = events
        .into_iter()
        .filter_map(|event| match event {
            SessionEvent::Goal(envelope) => Some(envelope),
            _ => None,
        })
        .collect();
    if envelopes.is_empty() {
        return None;
    }
    let (session, outcome) =
        super::goal_session::GoalSession::from_replay(envelopes.clone()).or_else(|| {
            tracing::warn!(
                envelope_count = envelopes.len(),
                session_root = %session_root.display(),
                "Goal replay returned None on resume — first envelope is not Created or the spec failed validation"
            );
            None
        })?;
    if !outcome.rejected.is_empty() || outcome.truncated_rejection_count > 0 {
        tracing::warn!(
            rejected = outcome.rejected.len(),
            truncated = outcome.truncated_rejection_count,
            session_root = %session_root.display(),
            "Goal replay dropped envelopes on resume — partial state surfaced; check events.jsonl for corruption"
        );
    }
    Some(session)
}

fn render_goal_response_text(text: &str, decision: &GoalLoopDecision) -> String {
    match decision {
        GoalLoopDecision::AwaitUser { question } => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                format!("Goal paused: {question}")
            } else {
                format!("{trimmed}\n\nGoal paused: {question}")
            }
        }
        GoalLoopDecision::Cancelled if text.trim().is_empty() => "Goal cancelled.".to_string(),
        GoalLoopDecision::Continue { .. } if text.trim().is_empty() => {
            "Goal paused before completion.".to_string()
        }
        _ => text.to_string(),
    }
}

fn record_recent_goal_tool_result(
    recent_tool_results: &Arc<Mutex<Vec<RecentToolCall>>>,
    call_id: &str,
    tool_name: &str,
    result: &Result<ToolOutput, String>,
) {
    let status = match result {
        Ok(_) => ToolResultStatus::Succeeded,
        Err(message) if looks_like_permission_denial(message) => ToolResultStatus::Denied,
        Err(message) if looks_like_timeout(message) => ToolResultStatus::TimedOut,
        Err(_) => ToolResultStatus::Failed,
    };
    if let Ok(mut recent) = recent_tool_results.lock() {
        recent.insert(
            0,
            RecentToolCall {
                call_id: call_id.to_string(),
                tool_name: tool_name.to_string(),
                status,
            },
        );
        recent.truncate(32);
    }
}

fn looks_like_permission_denial(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("denied") || lower.contains("permission") || lower.contains("approval")
}

fn looks_like_timeout(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("timeout") || lower.contains("timed out")
}

fn usage_cost_micro_usd(usage: &CompletionUsageSummary) -> u64 {
    let Some(cost) = usage.cost_usd else {
        return 0;
    };
    if !cost.is_finite() || cost <= 0.0 {
        return 0;
    }
    let micros = cost * 1_000_000.0;
    if micros >= u64::MAX as f64 {
        u64::MAX
    } else {
        micros.round() as u64
    }
}

struct SystemGoalEventClock;

impl GoalEventClock for SystemGoalEventClock {
    fn mint_envelope_id(&self) -> uuid::Uuid {
        uuid::Uuid::new_v4()
    }

    fn now(&self) -> chrono::DateTime<Utc> {
        Utc::now()
    }
}

struct WorkspaceGoalVerifierContext {
    working_dir: PathBuf,
    recent_tool_results: Arc<Mutex<Vec<RecentToolCall>>>,
    goal_cost_micro_usd: Arc<AtomicU64>,
    created_at: chrono::DateTime<Utc>,
}

impl GoalVerifierContext for WorkspaceGoalVerifierContext {
    fn file_sha256(&self, path: &str) -> Option<String> {
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return None;
        }
        let bytes = fs::read(self.working_dir.join(relative)).ok()?;
        Some(hex::encode(
            digest::digest(&digest::SHA256, &bytes).as_ref(),
        ))
    }

    fn recent_tool_results(&self) -> Vec<RecentToolCall> {
        self.recent_tool_results
            .lock()
            .map(|recent| recent.clone())
            .unwrap_or_default()
    }

    fn changed_files(&self) -> Vec<String> {
        changed_files_from_status(&self.working_dir)
    }

    fn now(&self) -> chrono::DateTime<Utc> {
        Utc::now()
    }

    fn finalised_by(&self) -> GoalActor {
        GoalActor::System {
            reason: "deterministic Goal verifier accepted completion".to_string(),
        }
    }

    fn total_spent_micro_usd(&self) -> u64 {
        self.goal_cost_micro_usd.load(Ordering::Relaxed)
    }

    fn elapsed_wall_clock_seconds(&self) -> u64 {
        (Utc::now() - self.created_at).num_seconds().max(0) as u64
    }

    fn continuation_loops_used(&self) -> u32 {
        0
    }
}

fn changed_files_from_status(working_dir: &Path) -> Vec<String> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let Ok(output) = Command::new(exe)
        .arg("status")
        .arg("--short")
        .current_dir(working_dir)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(changed_path_from_short_status_line)
        .collect()
}

fn changed_path_from_short_status_line(line: &str) -> Option<String> {
    let trimmed = line.trim_end();
    if trimmed.is_empty() || trimmed.starts_with("##") {
        return None;
    }
    let path = trimmed.get(3..).unwrap_or(trimmed).trim();
    let path = path
        .rsplit_once(" -> ")
        .map_or(path, |(_, new_path)| new_path);
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn should_forward_phase0_model_text_delta(_delta: &str) -> bool {
    false
}

fn should_route_plain_message_to_plan(text: &str) -> bool {
    let trimmed = text.trim_start();
    !trimmed.trim().is_empty() && !trimmed.starts_with('/')
}

/// Build the [`GoalActor`] attribution for actions taken inside the
/// active session. Today we always attribute to the local user;
/// once the controller-lease layer threads automation actor metadata
/// through (P6.6 follow-up for the `Automation` actor variant), this
/// function will branch on the active controller.
fn goal_actor_for_session<M: CompletionModel>(
    _app: &App<M>,
) -> crate::internal::ai::goal::GoalActor {
    crate::internal::ai::goal::GoalActor::User { id: None }
}

fn is_global_quit_command_input(text: &str) -> bool {
    matches!(
        super::slash_command::parse_builtin(text),
        Some((super::slash_command::BuiltinCommand::Quit, _))
    )
}

fn is_control_reclaim_command_input(text: &str) -> bool {
    matches!(
        super::slash_command::parse_builtin(text),
        Some((super::slash_command::BuiltinCommand::Control, args))
            if args.eq_ignore_ascii_case("reclaim")
    )
}

fn undo_should_prefer_vcs_rollback(working_dir: &std::path::Path) -> bool {
    let head = std::process::Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(working_dir)
        .output();
    let Ok(head) = head else {
        return false;
    };
    if !head.status.success() {
        return false;
    }

    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(working_dir)
        .output();
    let Ok(status) = status else {
        return false;
    };
    status.status.success() && String::from_utf8_lossy(&status.stdout).trim().is_empty()
}

fn apply_automation_approval_scope(config: &mut ToolLoopConfig, turn_id: TurnId) {
    let Some(runtime_context) = config.runtime_context.as_mut() else {
        return;
    };
    let Some(approval) = runtime_context.approval.as_mut() else {
        return;
    };
    approval.scope_key_prefix = Some(format!("automation:{turn_id}"));
}

fn attach_file_history_context(
    config: &mut ToolLoopConfig,
    session_root: PathBuf,
    turn_id: TurnId,
) {
    let Some(runtime_context) = config.runtime_context.as_mut() else {
        return;
    };
    runtime_context.file_history = Some(FileHistoryRuntimeContext {
        session_root,
        batch_id: format!("turn-{turn_id}"),
    });
}

fn normalize_terminal_paste_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn parse_value_or_json_string<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
) -> Result<T, serde_json::Error> {
    match value {
        serde_json::Value::String(raw) => serde_json::from_str(raw),
        _ => serde_json::from_value(value.clone()),
    }
}

fn extract_risk_level_from_response(resp: &UserInputResponse) -> Option<RiskLevel> {
    for answer in resp.answers.values() {
        for item in &answer.answers {
            let normalized = item.to_lowercase();
            if normalized.contains("low") {
                return Some(RiskLevel::Low);
            }
            if normalized.contains("medium") {
                return Some(RiskLevel::Medium);
            }
            if normalized.contains("high") {
                return Some(RiskLevel::High);
            }
        }
    }
    None
}

fn current_head_sha(working_dir: &std::path::Path) -> String {
    let output = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(working_dir)
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if text.is_empty() {
                "HEAD".to_string()
            } else {
                text
            }
        }
        _ => "HEAD".to_string(),
    }
}

struct IntentSpecBinding {
    workspace_key: String,
    base_ref: String,
    branch_label: Option<String>,
}

enum LatestIntentSpecLoad {
    Found(String),
    Missing,
    BindingMismatch(String),
}

fn canonical_working_dir_label(working_dir: &std::path::Path) -> String {
    std::fs::canonicalize(working_dir)
        .unwrap_or_else(|_| working_dir.to_path_buf())
        .display()
        .to_string()
}

fn current_intentspec_binding(working_dir: &std::path::Path) -> IntentSpecBinding {
    IntentSpecBinding {
        workspace_key: canonical_working_dir_label(working_dir),
        base_ref: current_head_sha(working_dir),
        branch_label: current_git_branch_label(working_dir),
    }
}

fn latest_intentspec_binding_matches(
    metadata: &HashMap<String, serde_json::Value>,
    binding: &IntentSpecBinding,
) -> bool {
    metadata
        .get(LATEST_INTENTSPEC_WORKSPACE_KEY)
        .and_then(|value| value.as_str())
        .is_some_and(|workspace| workspace == binding.workspace_key)
        && metadata
            .get(LATEST_INTENTSPEC_BASE_REF)
            .and_then(|value| value.as_str())
            .is_some_and(|base_ref| base_ref == binding.base_ref)
        && metadata
            .get(LATEST_INTENTSPEC_BRANCH_LABEL)
            .and_then(|value| value.as_str())
            == binding.branch_label.as_deref()
}

fn latest_intentspec_binding_mismatch_message(
    metadata: &HashMap<String, serde_json::Value>,
    binding: &IntentSpecBinding,
) -> Option<String> {
    let workspace = metadata
        .get(LATEST_INTENTSPEC_WORKSPACE_KEY)
        .and_then(|value| value.as_str())?;
    let base_ref = metadata
        .get(LATEST_INTENTSPEC_BASE_REF)
        .and_then(|value| value.as_str())?;
    let branch = metadata
        .get(LATEST_INTENTSPEC_BRANCH_LABEL)
        .and_then(|value| value.as_str());
    Some(format_intentspec_target_mismatch(
        workspace, base_ref, branch, binding,
    ))
}

fn format_intentspec_target_mismatch(
    stored_workspace: &str,
    stored_base_ref: &str,
    stored_branch: Option<&str>,
    binding: &IntentSpecBinding,
) -> String {
    format!(
        "Latest IntentSpec belongs to a different workspace or HEAD.\ncurrent · {} @ {}{}\nstored · {} @ {}{}\nRun `/plan <requirement>` in this worktree/head, or switch back to the matching checkout.",
        binding.workspace_key,
        shorten_binding_ref(&binding.base_ref),
        format_binding_branch(binding.branch_label.as_deref()),
        stored_workspace,
        shorten_binding_ref(stored_base_ref),
        format_binding_branch(stored_branch),
    )
}

fn shorten_binding_ref(raw: &str) -> String {
    const MAX_LEN: usize = 12;
    if raw.len() <= MAX_LEN {
        raw.to_string()
    } else {
        raw[..MAX_LEN].to_string()
    }
}

fn format_binding_branch(branch: Option<&str>) -> String {
    branch
        .map(|branch| format!(" ({branch})"))
        .unwrap_or_default()
}

fn intentspec_matches_workspace(
    spec: &IntentSpec,
    workspace_locator: &str,
    head_sha: &str,
) -> bool {
    spec.metadata.target.repo.locator == workspace_locator
        && spec.metadata.target.base_ref == head_sha
}

fn current_git_branch_label(working_dir: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .current_dir(working_dir)
        .output()
        .ok()?;

    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !branch.is_empty() {
            return Some(branch);
        }
    }

    let detached = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(working_dir)
        .output()
        .ok()?;

    if !detached.status.success() {
        return None;
    }

    let sha = String::from_utf8_lossy(&detached.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(format!("detached@{sha}"))
    }
}

async fn current_head_sha_async(working_dir: std::path::PathBuf) -> String {
    tokio::task::spawn_blocking(move || current_head_sha(&working_dir))
        .await
        .unwrap_or_else(|_| "HEAD".to_string())
}

#[derive(Debug, Clone, Default)]
struct McpTurnTrackingResult {
    run_id: Option<String>,
}

async fn resolve_mcp_turn_tracking(
    mcp_server: Arc<LibraMcpServer>,
    plan_id: Option<String>,
    working_dir: std::path::PathBuf,
    text: String,
) -> McpTurnTrackingResult {
    let snapshot_params = CreateContextSnapshotParams {
        selection_strategy: "heuristic".to_string(),
        items: None,
        summary: Some(format!("Context for: {text}")),
        tags: None,
        external_ids: None,
        actor_kind: Some("system".to_string()),
        actor_id: Some("libra-code".to_string()),
    };
    let snapshot_actor = match mcp_server.resolve_actor_from_params(
        snapshot_params.actor_kind.as_deref(),
        snapshot_params.actor_id.as_deref(),
    ) {
        Ok(actor) => actor,
        Err(e) => {
            cli_error!(e, "error: failed to resolve actor for snapshot");
            return McpTurnTrackingResult::default();
        }
    };
    let context_snapshot_id = match mcp_server
        .create_context_snapshot_impl(snapshot_params, snapshot_actor)
        .await
    {
        Ok(result) => {
            if result.is_error.unwrap_or(false) {
                render_mcp_error("failed to create context snapshot", result.content);
                None
            } else {
                parse_created_id(&result)
            }
        }
        Err(e) => {
            cli_error!(e, "error: failed to create context snapshot");
            None
        }
    };

    let task_params = CreateTaskParams {
        title: summarize_turn_task_title(&text),
        description: Some("Interactive TUI user request".to_string()),
        goal_type: None,
        constraints: None,
        acceptance_criteria: None,
        requested_by_kind: Some("human".to_string()),
        requested_by_id: Some("user".to_string()),
        dependencies: None,
        intent_id: None,
        parent_task_id: None,
        origin_step_id: None,
        status: Some("created".to_string()),
        reason: Some("start user turn".to_string()),
        tags: None,
        external_ids: None,
        actor_kind: Some("human".to_string()),
        actor_id: Some("user".to_string()),
    };
    let task_actor = match mcp_server.resolve_actor_from_params(
        task_params.actor_kind.as_deref(),
        task_params.actor_id.as_deref(),
    ) {
        Ok(actor) => actor,
        Err(e) => {
            cli_error!(e, "error: failed to resolve actor for task");
            return McpTurnTrackingResult::default();
        }
    };

    let task_id = match mcp_server.create_task_impl(task_params, task_actor).await {
        Ok(result) => {
            if result.is_error.unwrap_or(false) {
                render_mcp_error("failed to create task", result.content);
                None
            } else {
                parse_created_id(&result)
            }
        }
        Err(e) => {
            cli_error!(e, "error: failed to create task");
            None
        }
    };
    let Some(task_id) = task_id else {
        return McpTurnTrackingResult::default();
    };

    let run_params = CreateRunParams {
        task_id: task_id.clone(),
        base_commit_sha: current_head_sha_async(working_dir).await,
        plan_id,
        status: Some("created".to_string()),
        context_snapshot_id: context_snapshot_id.clone(),
        error: None,
        agent_instances: None,
        metrics_json: None,
        reason: Some("start user turn".to_string()),
        orchestrator_version: None,
        tags: None,
        external_ids: None,
        actor_kind: Some("human".to_string()),
        actor_id: Some("user".to_string()),
    };
    let run_actor = match mcp_server.resolve_actor_from_params(
        run_params.actor_kind.as_deref(),
        run_params.actor_id.as_deref(),
    ) {
        Ok(actor) => actor,
        Err(e) => {
            cli_error!(e, "error: failed to resolve actor for run");
            return McpTurnTrackingResult::default();
        }
    };

    let run_id = match mcp_server.create_run_impl(run_params, run_actor).await {
        Ok(result) => {
            if result.is_error.unwrap_or(false) {
                render_mcp_error("failed to create run", result.content);
                None
            } else {
                parse_created_id(&result)
            }
        }
        Err(e) => {
            cli_error!(e, "error: failed to create run");
            None
        }
    };

    McpTurnTrackingResult { run_id }
}

fn parse_created_id(result: &rmcp::model::CallToolResult) -> Option<String> {
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

fn summarize_turn_task_title(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "TUI user request".to_string();
    }

    let mut title: String = trimmed.chars().take(72).collect();
    if trimmed.chars().count() > 72 {
        title.push_str("...");
    }
    format!("TUI: {title}")
}

#[cfg(test)]
fn format_task_completion_note(
    title: &str,
    result: &crate::internal::ai::orchestrator::types::TaskResult,
) -> String {
    let mut note = format!("{} · {}", task_status_heading(&result.status), title.trim());

    let mut metrics = Vec::new();
    if !result.tool_calls.is_empty() {
        metrics.push(format!("{} tools", result.tool_calls.len()));
    }
    if result.retry_count > 0 {
        metrics.push(format!("{} retries", result.retry_count));
    }
    if !result.policy_violations.is_empty() {
        let count = result.policy_violations.len();
        metrics.push(format!(
            "{} policy violation{}",
            count,
            if count == 1 { "" } else { "s" }
        ));
    }
    if !metrics.is_empty() {
        note.push_str(&format!("  \n{}", metrics.join(" · ")));
    }

    if let Some(review) = result.review.as_ref() {
        note.push_str(&format!(
            "  \nreview · {} · approved {}",
            review.summary,
            if review.approved { "yes" } else { "no" }
        ));
        if !review.issues.is_empty() {
            note.push_str(&format!("  \nissues · {}", review.issues.join("; ")));
        }
    } else if matches!(
        result.status,
        crate::internal::ai::orchestrator::types::TaskNodeStatus::Failed
    ) && let Some(reason) = result
        .agent_output
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
    {
        note.push_str(&format!("  \nreason · {}", reason));
    } else if matches!(
        result.status,
        crate::internal::ai::orchestrator::types::TaskNodeStatus::Failed
    ) && let Some(reason) = summarize_failed_gate_report(result.gate_report.as_ref())
    {
        note.push_str(&format!("  \nreason · {}", reason));
    }

    note
}

fn format_task_workspace_note(
    title: &str,
    working_dir: &std::path::Path,
    isolated: bool,
    backend: TaskWorkspaceBackend,
    main_working_dir: Option<&std::path::Path>,
) -> String {
    if isolated {
        let source = main_working_dir.unwrap_or(working_dir);
        return format!(
            "Workspace · {}  \nisolated {} · source {}  \nruntime path hidden · changes sync back after task",
            title.trim(),
            backend.label(),
            source.display()
        );
    }

    format!(
        "Workspace · {}  \n{} · {}",
        title.trim(),
        backend.label(),
        working_dir.display()
    )
}

#[cfg(test)]
fn task_status_heading(
    status: &crate::internal::ai::orchestrator::types::TaskNodeStatus,
) -> &'static str {
    match status {
        crate::internal::ai::orchestrator::types::TaskNodeStatus::Pending => "Pending",
        crate::internal::ai::orchestrator::types::TaskNodeStatus::Running => "Running",
        crate::internal::ai::orchestrator::types::TaskNodeStatus::Completed => "Completed",
        crate::internal::ai::orchestrator::types::TaskNodeStatus::Failed => "Failed",
        crate::internal::ai::orchestrator::types::TaskNodeStatus::Skipped => "Skipped",
    }
}

fn summarize_failed_gate_report(gate_report: Option<&GateReport>) -> Option<String> {
    let report = gate_report?;
    let failed_checks: Vec<_> = report
        .results
        .iter()
        .filter(|result| !result.passed)
        .collect();
    if failed_checks.is_empty() {
        return None;
    }

    let summary = failed_checks
        .iter()
        .take(2)
        .map(|result| {
            let outcome = if result.timed_out {
                "timed out".to_string()
            } else {
                format!("exit {}", result.exit_code)
            };
            let detail = result
                .stderr
                .lines()
                .find(|line| !line.trim().is_empty())
                .or_else(|| result.stdout.lines().find(|line| !line.trim().is_empty()))
                .map(str::trim)
                .filter(|detail| !detail.is_empty())
                .map(|detail| format!(": {detail}"))
                .unwrap_or_default();
            format!("{} ({outcome}{detail})", result.check_id)
        })
        .collect::<Vec<_>>()
        .join("; ");

    let remainder = failed_checks.len().saturating_sub(2);
    if remainder > 0 {
        Some(format!("{summary}; +{remainder} more failed checks"))
    } else {
        Some(summary)
    }
}

async fn fetch_intentspec_from_object_id(
    mcp: &Arc<LibraMcpServer>,
    object_id: &str,
) -> Option<IntentSpec> {
    let uri = format!("libra://object/{object_id}");
    let resources = mcp.read_resource_impl(&uri).await.ok()?;
    let content = resources.first()?;
    let text = resource_text(content)?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let intent_content = extract_content_field(&value)?;
    serde_json::from_str::<IntentSpec>(&intent_content).ok()
}

fn resource_text(content: &rmcp::model::ResourceContents) -> Option<String> {
    let value = serde_json::to_value(content).ok()?;
    value
        .get("text")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
}

fn extract_content_field(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(v) = map.get("content").and_then(|v| v.as_str()) {
                return Some(v.to_string());
            }
            map.values().find_map(extract_content_field)
        }
        serde_json::Value::Array(items) => items.iter().find_map(extract_content_field),
        _ => None,
    }
}

#[cfg(test)]
mod orchestrator_result_tests {
    use std::collections::HashMap;

    use git_internal::internal::object::{plan::PlanStep, task::Task as GitTask, types::ActorRef};
    use uuid::Uuid;

    use super::{
        IntentSpecBinding, LATEST_INTENTSPEC_BASE_REF, LATEST_INTENTSPEC_BRANCH_LABEL,
        LATEST_INTENTSPEC_WORKSPACE_KEY, canonical_working_dir_label, format_orchestrator_result,
        format_task_completion_note, format_task_workspace_note, intentspec_matches_workspace,
        latest_intentspec_binding_matches,
    };
    use crate::internal::ai::{
        intentspec::types::{
            Acceptance, Artifacts, ChangeType, ConstraintLicensing, ConstraintPlatform,
            ConstraintPrivacy, ConstraintResources, ConstraintSecurity, Constraints, CreatedBy,
            CreatorType, DomainAllowlistMode, EncodingPolicy, EvidencePolicy, EvidenceStrategy,
            ExecutionPolicy, HumanInLoop, Intent, IntentSpec, Lifecycle, LifecycleStatus, Metadata,
            NetworkPolicy, Objective, ObjectiveKind, OutputHandlingPolicy, PromptInjectionPolicy,
            ProvenanceBindings, ProvenancePolicy, RepoTarget, RepoType, RetryPolicy, Risk,
            RiskLevel, SecretAccessPolicy, SecretPolicy, SecurityPolicy, Target, ToolAcl,
            TransparencyLogPolicy, TransparencyMode, TrustTier, VerificationPlan,
        },
        orchestrator::{
            run_state::RunStateSnapshot,
            types::{
                DecisionOutcome, ExecutionPlanSpec, GateReport, GateResult, OrchestratorResult,
                ReviewOutcome, SystemReport, TaskContract, TaskKind, TaskNodeStatus, TaskResult,
                TaskSpec, TaskWorkspaceBackend,
            },
        },
    };

    fn test_task_spec(title: &str, kind: TaskKind) -> TaskSpec {
        let actor = ActorRef::agent("test-tui").unwrap();
        let task = GitTask::new(actor, title, None).unwrap();
        TaskSpec {
            step: PlanStep::new(title),
            task,
            objective: title.into(),
            kind,
            gate_stage: None,
            owner_role: Some("coder".into()),
            scope_in: vec![],
            scope_out: vec![],
            checks: vec![],
            contract: TaskContract::default(),
        }
    }

    fn intentspec_fixture(locator: &str, base_ref: &str) -> IntentSpec {
        IntentSpec {
            api_version: "intentspec.io/v1alpha1".into(),
            kind: "IntentSpec".into(),
            metadata: Metadata {
                id: "intent-1".into(),
                created_at: "2026-04-02T00:00:00Z".into(),
                created_by: CreatedBy {
                    creator_type: CreatorType::User,
                    id: "tester".into(),
                    display_name: None,
                },
                target: Target {
                    repo: RepoTarget {
                        repo_type: RepoType::Local,
                        locator: locator.into(),
                    },
                    base_ref: base_ref.into(),
                    workspace_id: None,
                    labels: Default::default(),
                },
            },
            intent: Intent {
                summary: "summary".into(),
                problem_statement: "problem".into(),
                change_type: ChangeType::Feature,
                objectives: vec![Objective {
                    title: "Ship feature".into(),
                    kind: ObjectiveKind::Implementation,
                }],
                in_scope: vec!["src/".into()],
                out_of_scope: vec![],
                touch_hints: None,
            },
            acceptance: Acceptance {
                success_criteria: vec!["tests pass".into()],
                verification_plan: VerificationPlan {
                    fast_checks: vec![],
                    integration_checks: vec![],
                    security_checks: vec![],
                    release_checks: vec![],
                },
                quality_gates: None,
            },
            constraints: Constraints {
                security: ConstraintSecurity {
                    network_policy: NetworkPolicy::Deny,
                    dependency_policy:
                        crate::internal::ai::intentspec::types::DependencyPolicy::NoNew,
                    crypto_policy: String::new(),
                },
                privacy: ConstraintPrivacy {
                    data_classes_allowed: vec![],
                    redaction_required: false,
                    retention_days: 30,
                },
                licensing: ConstraintLicensing {
                    allowed_spdx: vec![],
                    forbid_new_licenses: false,
                },
                platform: ConstraintPlatform {
                    language_runtime: "rust".into(),
                    supported_os: vec![],
                },
                resources: ConstraintResources {
                    max_wall_clock_seconds: 30,
                    max_cost_units: 0,
                },
            },
            risk: Risk {
                level: RiskLevel::Low,
                rationale: "low".into(),
                factors: vec![],
                human_in_loop: HumanInLoop {
                    required: false,
                    min_approvers: 0,
                },
            },
            evidence: EvidencePolicy {
                strategy: EvidenceStrategy::RepoFirst,
                trust_tiers: vec![TrustTier::Repo],
                domain_allowlist_mode: DomainAllowlistMode::Disabled,
                allowed_domains: vec![],
                blocked_domains: vec![],
                min_citations_per_decision: 1,
            },
            security: SecurityPolicy {
                tool_acl: ToolAcl {
                    allow: vec![],
                    deny: vec![],
                },
                secrets: SecretPolicy {
                    policy: SecretAccessPolicy::DenyAll,
                    allowed_scopes: vec![],
                },
                prompt_injection: PromptInjectionPolicy {
                    treat_retrieved_content_as_untrusted: true,
                    enforce_output_schema: true,
                    disallow_instruction_from_evidence: true,
                },
                output_handling: OutputHandlingPolicy {
                    encoding_policy: EncodingPolicy::StrictJson,
                    no_direct_eval: true,
                },
            },
            execution: ExecutionPolicy {
                retry: RetryPolicy {
                    max_retries: 0,
                    backoff_seconds: 0,
                },
                replan: crate::internal::ai::intentspec::types::ReplanPolicy { triggers: vec![] },
                concurrency: crate::internal::ai::intentspec::types::ConcurrencyPolicy {
                    max_parallel_tasks: 1,
                },
            },
            artifacts: Artifacts {
                required: vec![],
                retention: crate::internal::ai::intentspec::types::ArtifactRetention { days: 30 },
            },
            provenance: ProvenancePolicy {
                require_slsa_provenance: false,
                require_sbom: false,
                transparency_log: TransparencyLogPolicy {
                    mode: TransparencyMode::None,
                },
                bindings: ProvenanceBindings {
                    embed_intent_spec_digest: false,
                    embed_evidence_digests: false,
                },
            },
            lifecycle: Lifecycle {
                schema_version: "1.0.0".into(),
                status: LifecycleStatus::Active,
                change_log: vec![],
            },
            libra: None,
            extensions: Default::default(),
        }
    }

    #[test]
    fn failed_task_note_includes_review_summary() {
        let note = format_task_completion_note(
            "Analyze requested scope",
            &TaskResult {
                task_id: Uuid::new_v4(),
                status: TaskNodeStatus::Failed,
                gate_report: None,
                agent_output: Some("partial analysis".into()),
                retry_count: 4,
                tool_calls: vec![],
                policy_violations: vec![],
                model_usage: None,
                review: Some(ReviewOutcome {
                    approved: false,
                    summary: "response is incomplete".into(),
                    issues: vec!["missing final diagnosis".into()],
                }),
                thinking: None,
            },
        );

        assert!(note.contains("review · response is incomplete · approved no"));
        assert!(note.contains("issues · missing final diagnosis"));
    }

    #[test]
    fn failed_task_note_falls_back_to_failure_reason_when_review_is_missing() {
        let note = format_task_completion_note(
            "Analyze requested scope",
            &TaskResult {
                task_id: Uuid::new_v4(),
                status: TaskNodeStatus::Failed,
                gate_report: None,
                agent_output: Some("reviewer pass failed: invalid reviewer JSON".into()),
                retry_count: 4,
                tool_calls: vec![],
                policy_violations: vec![],
                model_usage: None,
                review: None,
                thinking: None,
            },
        );

        assert!(note.contains("reason · reviewer pass failed: invalid reviewer JSON"));
    }

    #[test]
    fn failed_gate_note_includes_gate_failure_reason() {
        let note = format_task_completion_note(
            "Integration gate",
            &TaskResult {
                task_id: Uuid::new_v4(),
                status: TaskNodeStatus::Failed,
                gate_report: Some(GateReport {
                    results: vec![GateResult {
                        check_id: "cargo-test".into(),
                        kind: "command".into(),
                        passed: false,
                        exit_code: 101,
                        stdout: String::new(),
                        stderr: "tests failed".into(),
                        duration_ms: 1234,
                        timed_out: false,
                    }],
                    all_required_passed: false,
                }),
                agent_output: None,
                retry_count: 0,
                tool_calls: vec![],
                policy_violations: vec![],
                model_usage: None,
                review: None,
                thinking: None,
            },
        );

        assert!(note.contains("reason · cargo-test (exit 101: tests failed)"));
    }

    #[test]
    fn workspace_note_includes_mode_and_directory() {
        let isolated_note = format_task_workspace_note(
            "Implement parser",
            std::path::Path::new("/tmp/libra/.libra/worktrees/task-1"),
            true,
            TaskWorkspaceBackend::Copy,
            Some(std::path::Path::new("/tmp/libra")),
        );
        assert!(isolated_note.contains("Workspace · Implement parser"));
        assert!(isolated_note.contains("isolated copy worktree"));
        assert!(isolated_note.contains("source /tmp/libra"));
        assert!(isolated_note.contains("runtime path hidden"));
        assert!(!isolated_note.contains(".libra/worktrees/task-1"));

        let shared_note = format_task_workspace_note(
            "Run gate",
            std::path::Path::new("/tmp/libra"),
            false,
            TaskWorkspaceBackend::Shared,
            None,
        );
        assert!(shared_note.contains("shared workspace"));
        assert!(shared_note.contains("/tmp/libra"));
    }

    #[test]
    fn intentspec_match_requires_same_workspace_and_head() {
        let workspace = tempfile::tempdir().unwrap();
        let locator = canonical_working_dir_label(workspace.path());
        let spec = intentspec_fixture(&locator, "abc123");
        assert!(intentspec_matches_workspace(&spec, &locator, "abc123"));
        assert!(!intentspec_matches_workspace(&spec, &locator, "def456"));
    }

    #[test]
    fn intentspec_match_rejects_other_worktree_locator() {
        let workspace = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let locator = canonical_working_dir_label(workspace.path());
        let other_locator = canonical_working_dir_label(other.path());
        let spec = intentspec_fixture(&other_locator, "abc123");
        assert!(!intentspec_matches_workspace(&spec, &locator, "abc123"));
    }

    #[test]
    fn latest_intentspec_binding_requires_exact_worktree_head_and_branch() {
        let mut metadata = HashMap::new();
        metadata.insert(
            LATEST_INTENTSPEC_WORKSPACE_KEY.to_string(),
            serde_json::Value::String("/repo/worktree-a".into()),
        );
        metadata.insert(
            LATEST_INTENTSPEC_BASE_REF.to_string(),
            serde_json::Value::String("abc123".into()),
        );
        metadata.insert(
            LATEST_INTENTSPEC_BRANCH_LABEL.to_string(),
            serde_json::Value::String("feature/spec".into()),
        );

        assert!(latest_intentspec_binding_matches(
            &metadata,
            &IntentSpecBinding {
                workspace_key: "/repo/worktree-a".into(),
                base_ref: "abc123".into(),
                branch_label: Some("feature/spec".into()),
            }
        ));
        assert!(!latest_intentspec_binding_matches(
            &metadata,
            &IntentSpecBinding {
                workspace_key: "/repo/worktree-b".into(),
                base_ref: "abc123".into(),
                branch_label: Some("feature/spec".into()),
            }
        ));
        assert!(!latest_intentspec_binding_matches(
            &metadata,
            &IntentSpecBinding {
                workspace_key: "/repo/worktree-a".into(),
                base_ref: "def456".into(),
                branch_label: Some("feature/spec".into()),
            }
        ));
        assert!(!latest_intentspec_binding_matches(
            &metadata,
            &IntentSpecBinding {
                workspace_key: "/repo/worktree-a".into(),
                base_ref: "abc123".into(),
                branch_label: Some("main".into()),
            }
        ));
    }

    #[test]
    fn orchestrator_result_includes_task_review_and_failure_reason() {
        let reviewed_task = test_task_spec("Summarize findings", TaskKind::Analysis);
        let failed_task = test_task_spec("Count Rust modules", TaskKind::Analysis);
        let result = OrchestratorResult {
            decision: DecisionOutcome::Abandon,
            execution_plan_spec: ExecutionPlanSpec {
                intent_spec_id: "intent-1".into(),
                revision: 1,
                parent_revision: None,
                replan_reason: None,
                tasks: vec![reviewed_task.clone(), failed_task.clone()],
                max_parallel: 2,
                checkpoints: vec![],
            },
            plan_revision_specs: vec![],
            run_state: RunStateSnapshot::default(),
            task_results: vec![
                TaskResult {
                    task_id: reviewed_task.id(),
                    status: TaskNodeStatus::Completed,
                    gate_report: None,
                    agent_output: Some("done".into()),
                    retry_count: 0,
                    tool_calls: vec![],
                    policy_violations: vec![],
                    model_usage: None,
                    review: Some(ReviewOutcome {
                        approved: true,
                        summary: "analysis is complete".into(),
                        issues: vec![],
                    }),
                    thinking: None,
                },
                TaskResult {
                    task_id: failed_task.id(),
                    status: TaskNodeStatus::Failed,
                    gate_report: None,
                    agent_output: Some(
                        "Agent reached final response without covering all objectives".into(),
                    ),
                    retry_count: 4,
                    tool_calls: vec![],
                    policy_violations: vec![],
                    model_usage: None,
                    review: None,
                    thinking: None,
                },
            ],
            system_report: SystemReport {
                integration: GateReport::empty(),
                security: GateReport::empty(),
                release: GateReport::empty(),
                review_passed: true,
                review_findings: vec![],
                artifacts_complete: true,
                missing_artifacts: vec![],
                overall_passed: true,
            },
            intent_spec_id: "intent-1".into(),
            lifecycle_change_log: vec![],
            replan_count: 0,
            persistence: None,
        };

        let rendered = format_orchestrator_result(&result);

        assert!(rendered.contains("Review: analysis is complete \\| approved: yes"));
        assert!(
            rendered
                .contains("Reason: Agent reached final response without covering all objectives")
        );
    }

    #[test]
    fn orchestrator_result_includes_gate_failure_reason() {
        let gate_task = test_task_spec("Integration gate", TaskKind::Gate);
        let result = OrchestratorResult {
            decision: DecisionOutcome::Abandon,
            execution_plan_spec: ExecutionPlanSpec {
                intent_spec_id: "intent-2".into(),
                revision: 1,
                parent_revision: None,
                replan_reason: None,
                tasks: vec![gate_task.clone()],
                max_parallel: 1,
                checkpoints: vec![],
            },
            plan_revision_specs: vec![],
            run_state: RunStateSnapshot::default(),
            task_results: vec![TaskResult {
                task_id: gate_task.id(),
                status: TaskNodeStatus::Failed,
                gate_report: Some(GateReport {
                    results: vec![GateResult {
                        check_id: "clippy".into(),
                        kind: "command".into(),
                        passed: false,
                        exit_code: 101,
                        stdout: String::new(),
                        stderr: "lint failed".into(),
                        duration_ms: 900,
                        timed_out: false,
                    }],
                    all_required_passed: false,
                }),
                agent_output: None,
                retry_count: 0,
                tool_calls: vec![],
                policy_violations: vec![],
                model_usage: None,
                review: None,
                thinking: None,
            }],
            system_report: SystemReport {
                integration: GateReport::empty(),
                security: GateReport::empty(),
                release: GateReport::empty(),
                review_passed: true,
                review_findings: vec![],
                artifacts_complete: true,
                missing_artifacts: vec![],
                overall_passed: false,
            },
            intent_spec_id: "intent-2".into(),
            lifecycle_change_log: vec![],
            replan_count: 0,
            persistence: None,
        };

        let rendered = format_orchestrator_result(&result);

        assert!(rendered.contains("Reason: clippy (exit 101: lint failed)"));
    }
}
