//! Iterative model-and-tool execution loop.
//!
//! This is the heart of the agent runtime: every assistant turn that may include tool
//! calls funnels through [`run_tool_loop_with_history_and_observer`]. The loop:
//!
//! 1. Sends the current chat history to the [`CompletionModel`] together with the
//!    available tool definitions.
//! 2. Splits the response into text fragments and tool calls.
//! 3. Either dispatches each tool call (with hook integration, allow-list enforcement,
//!    and repeated-call protection) and folds the results back into the history, or —
//!    when the model produced only text — returns the text as the final answer.
//!
//! The loop also enforces three safety budgets that protect against runaway agents:
//!
//! - `max_turns` — hard upper bound on iterations.
//! - Repeated-call detection (warning + abort thresholds over a sliding window) —
//!   stops the model from looping on the same tool call.
//! - Identical-blocked-call counter — if a hook or allow-list keeps blocking the
//!   exact same call, abort instead of letting the model retry forever.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    future, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::sub_agent::{
    SubAgentToolLoopRuntime, TaskEntryKind, TaskFailure, TaskInvocation, TaskResult,
};
use crate::internal::ai::{
    completion::{
        AssistantContent, CompletionError, CompletionModel, CompletionReasoningEffort,
        CompletionRequest, CompletionStreamEvent, CompletionThinking, CompletionUsage,
        CompletionUsageSummary, Message, OneOrMany, ToolResult, UserContent,
        request::CompletionResponse,
    },
    context_budget::{
        CompactionEvent, CompactionReason, ContextAttachmentStore, ContextBudget,
        ContextFrameBuilder, ContextFrameCandidate, ContextFrameEvent, ContextFrameKind,
        ContextFrameSource, ContextSegmentKind, ContextTrustLevel,
    },
    goal::GoalStopPolicy,
    hooks::{HookAction, HookRunner},
    session::jsonl::{SessionEvent, SessionJsonlStore},
    sources::{SourcePool, SourcePoolError, SourceToolNaming},
    tools::{
        FunctionParameters, ToolDefinition, ToolInvocation, ToolOutput, ToolPayload, ToolRegistry,
        ToolRuntimeContext, ToolSpec,
    },
    usage::{UsageContext, UsageRecorder},
};

/// A single complete tool-loop turn result.
///
/// Returned by [`run_tool_loop_with_history_and_observer`]. `final_text` is what the
/// model produced as its final answer (after all tool calls resolved), and `history`
/// is the full history including the user prompt, every assistant turn, and every
/// tool result, ready to be fed into another `run_tool_loop_*` call to continue the
/// conversation.
#[derive(Clone, Debug)]
pub struct ToolLoopTurn {
    pub final_text: String,
    pub history: Vec<Message>,
}

/// Cooperative cancellation supplied by a runtime owner. It separates the
/// normal cancellation signal from the active mutation-dispatch marker:
/// callers must request cancellation before the latter is set, while a loop
/// that has started a mutating call waits for a determinate handler result.
#[derive(Clone, Debug)]
pub struct ToolLoopCancellation {
    token: CancellationToken,
    mutation_started: Arc<AtomicBool>,
}

impl ToolLoopCancellation {
    pub fn new(token: CancellationToken, mutation_started: Arc<AtomicBool>) -> Self {
        Self {
            token,
            mutation_started,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub fn mark_mutation_started(&self) {
        self.mutation_started.store(true, Ordering::Release);
    }

    /// Clear the marker only after the handler has returned its determinate
    /// result. A later model/read-only phase is safe to cancel cooperatively.
    pub fn mark_mutation_finished(&self) {
        self.mutation_started.store(false, Ordering::Release);
    }

    async fn cancelled(&self) {
        self.token.cancelled().await;
    }
}

/// Observer hooks for tool-loop execution.
///
/// Implementations receive call-by-call notifications without affecting the loop
/// itself (except for [`Self::on_tool_call_preflight`], which can cancel a tool call).
/// Used by the TUI to render thoughts/calls/results live, and by tests to assert the
/// expected sequence of events.
///
/// All callbacks are best-effort and must be non-panicking.
pub trait ToolLoopObserver: Send {
    fn on_model_turn_start(&mut self, _turn: usize) {}

    fn on_model_usage(&mut self, _usage: &CompletionUsageSummary) {}

    fn on_model_usage_recorded(&mut self, usage: &CompletionUsageSummary, _wall_clock_ms: u64) {
        self.on_model_usage(usage);
    }

    fn on_model_stream_event(&mut self, _event: &CompletionStreamEvent) {}

    fn on_assistant_step_text(&mut self, _text: &str) {}

    fn on_tool_call_begin(&mut self, _call_id: &str, _tool_name: &str, _arguments: &Value) {}

    fn on_tool_call_preflight(
        &mut self,
        _call_id: &str,
        _tool_name: &str,
        _arguments: &Value,
    ) -> Result<(), String> {
        Ok(())
    }

    fn on_tool_call_end(
        &mut self,
        _call_id: &str,
        _tool_name: &str,
        _result: &Result<ToolOutput, String>,
    ) {
    }

    /// Called after a `task` tool call completes with a successful
    /// `TaskResult` from the sub-agent dispatcher. The default impl
    /// folds the child's usage into the parent's anonymous bucket
    /// via [`Self::on_model_usage`], which keeps the totals accurate
    /// even when the consumer ignores per-agent attribution.
    /// Per-agent budget enforcement (OC-Phase 5 P5.3) overrides this
    /// to route the usage through the budget tracker with
    /// `agent_name = Some(...)` so `check_agent` fires correctly.
    ///
    /// `agent_name` is the sub-agent's spec name as resolved by the
    /// dispatcher. `usage` is the accumulated `CompletionUsageSummary`
    /// the runner's `ChildRunObserver` collected across every model
    /// turn in the child loop (v0.17.762).
    fn on_sub_agent_completed(&mut self, _agent_name: &str, usage: &CompletionUsageSummary) {
        self.on_model_usage(usage);
    }
}

/// Default observer used when callers do not provide one (the simple `run_tool_loop`
/// entrypoint). Every method falls back to the trait's no-op default.
struct NoopObserver;

impl ToolLoopObserver for NoopObserver {}

/// Runtime configuration for iterative tool-calling execution.
///
/// Every knob is optional; the [`Default`] impl wires sensible thresholds that match
/// the typical interactive `libra code` usage. Callers (multi-agent plan executors,
/// MCP server, etc.) override individual fields as needed.
#[derive(Clone, Debug)]
pub struct ToolLoopConfig {
    pub preamble: Option<String>,
    pub temperature: Option<f64>,
    pub thinking: Option<CompletionThinking>,
    pub reasoning_effort: Option<CompletionReasoningEffort>,
    pub stream: Option<bool>,
    /// Optional hook runner for pre/post tool-use hooks.
    pub hook_runner: Option<Arc<HookRunner>>,
    /// If set, only expose these tools to the model (agent tool restriction).
    pub allowed_tools: Option<Vec<String>>,
    /// Optional runtime constraints injected into every tool invocation.
    pub runtime_context: Option<ToolRuntimeContext>,
    /// Optional runtime-owned cooperative cancellation contract. The loop
    /// observes it before model work and immediately before dispatch; mutating
    /// calls mark their side-effect boundary and are never force-cancelled.
    pub cancellation: Option<ToolLoopCancellation>,
    /// Hard cap for model turns in one tool loop run.
    pub max_turns: Option<usize>,
    /// Number of recent executed tool calls used to detect repeated calls.
    pub repeat_detection_window: Option<usize>,
    /// Same executed tool-call signature count that triggers a strategy warning.
    pub repeat_warning_threshold: Option<usize>,
    /// Same executed tool-call signature count that aborts the loop.
    pub repeat_abort_threshold: Option<usize>,
    /// Tools that complete the current loop immediately after a successful call.
    pub terminal_tools: Option<Vec<String>>,
    /// Optional sub-agent runtime. When present, the model sees the
    /// `task` schema and `task(...)` calls are intercepted here instead
    /// of being routed through the normal [`ToolRegistry`].
    pub subagent_runtime: Option<SubAgentToolLoopRuntime>,
    /// Session root used for append-only context-frame recording.
    pub context_frame_session_root: Option<PathBuf>,
    /// Stable prefix for context-frame prompt IDs.
    pub context_frame_prompt_id: Option<String>,
    /// Optional budget override used by tests and constrained runtimes.
    pub context_frame_budget: Option<ContextBudget>,
    /// Optional attachment threshold override used by tests and constrained runtimes.
    pub context_frame_attachment_threshold_bytes: Option<usize>,
    /// Optional usage recorder for provider-neutral token/cost stats.
    pub usage_recorder: Option<UsageRecorder>,
    /// Provider/model/thread metadata attached to usage rows.
    pub usage_context: Option<UsageContext>,
    /// Optional Source Pool whose enabled handlers are merged into the tool
    /// registry at the start of each tool-loop run.
    pub source_pool: Option<SourcePool>,
    /// Session id used to build Source Pool state namespaces.
    pub source_session_id: Option<String>,
    /// Whether assistant reasoning content should be retained in model history.
    pub preserve_reasoning_content: bool,
    /// Optional Goal-mode stop policy that the supervisor-aware
    /// driver consults to decide whether a finished tool-loop turn
    /// releases the session to idle or feeds the supervisor for the
    /// next iteration.
    ///
    /// `None` (the default) means the legacy non-Goal behaviour:
    /// the tool loop ends after a single turn unless the model
    /// continues. `Some(GoalStopPolicy::Normal)` is intentionally
    /// equivalent — the field carries a `Some(GoalBound { goal_id })`
    /// when callers want to pin a turn to a specific Goal, matching
    /// the supervisor's [`GoalStopPolicy`] enum.
    ///
    /// OC-Phase 6 P6.3 contract field: the TUI Goal path binds
    /// `Some(GoalBound { goal_id })` into this config before calling
    /// the Goal-aware driver. The inner
    /// [`super::run_tool_loop_with_history_and_observer`] entry point
    /// still does not branch on it; the field keeps policy visible to
    /// observers and future loop integrations without changing legacy
    /// non-Goal callers.
    pub goal_stop_policy: Option<GoalStopPolicy>,
}

impl Default for ToolLoopConfig {
    fn default() -> Self {
        Self {
            preamble: None,
            temperature: Some(0.0),
            thinking: None,
            reasoning_effort: None,
            stream: None,
            hook_runner: None,
            allowed_tools: None,
            runtime_context: None,
            cancellation: None,
            max_turns: None,
            repeat_detection_window: Some(DEFAULT_REPEAT_DETECTION_WINDOW),
            repeat_warning_threshold: Some(DEFAULT_REPEAT_WARNING_THRESHOLD),
            repeat_abort_threshold: Some(DEFAULT_REPEAT_ABORT_THRESHOLD),
            terminal_tools: None,
            subagent_runtime: None,
            context_frame_session_root: None,
            context_frame_prompt_id: None,
            context_frame_budget: None,
            context_frame_attachment_threshold_bytes: None,
            usage_recorder: None,
            usage_context: None,
            source_pool: None,
            source_session_id: None,
            preserve_reasoning_content: false,
            goal_stop_policy: None,
        }
    }
}

/// Maximum model turns when `ToolLoopConfig::max_turns` is unset.
///
/// 64 is enough for the longest real interactive turn we have observed (deep file
/// exploration with many small reads) while still bounding pathological agent loops.
const DEFAULT_MAX_TOOL_LOOP_TURNS: usize = 64;
/// Sliding window (in executed tool calls) used by the repeat detector.
const DEFAULT_REPEAT_DETECTION_WINDOW: usize = 10;
/// Repeat count at which we inject a "you keep calling the same thing" warning into
/// the next tool result.
const DEFAULT_REPEAT_WARNING_THRESHOLD: usize = 3;
/// Repeat count at which we hard-abort the loop with `CompletionError::ResponseError`.
const DEFAULT_REPEAT_ABORT_THRESHOLD: usize = 5;
/// Cap on identical *blocked* tool calls (hook deny / allow-list miss / preflight
/// rejection). Exceeding this means the model is stuck retrying a forbidden call and
/// the loop aborts to avoid wasted tokens.
const MAX_IDENTICAL_BLOCKED_TOOL_CALLS: usize = 3;

fn tool_loop_cancelled(config: &ToolLoopConfig) -> bool {
    config
        .cancellation
        .as_ref()
        .is_some_and(ToolLoopCancellation::is_cancelled)
}

async fn wait_for_tool_loop_cancellation(cancellation: Option<&ToolLoopCancellation>) {
    match cancellation {
        Some(cancellation) => cancellation.cancelled().await,
        None => future::pending::<()>().await,
    }
}

fn tool_loop_cancelled_error() -> CompletionError {
    CompletionError::ResponseError("Tool loop cancelled before a new side effect began".to_string())
}

/// Run a prompt through a completion model, allowing iterative tool calls.
///
/// Functional scope: the simplest entry point — starts with no history, no observer,
/// returns just the final assistant text. Used by the codex executor and any caller
/// that does not need streaming events or to extend the conversation afterwards.
///
/// Boundary conditions: every error case from the underlying loop bubbles up
/// unchanged (max-turns, repeated tool calls, hook denials, etc.).
pub async fn run_tool_loop<M: CompletionModel>(
    model: &M,
    prompt: impl Into<String>,
    registry: &ToolRegistry,
    config: ToolLoopConfig,
) -> Result<String, CompletionError>
where
    M::Response: CompletionUsage,
{
    let mut observer = NoopObserver;
    let turn = run_tool_loop_with_history_and_observer(
        model,
        Vec::new(),
        prompt,
        registry,
        config,
        &mut observer,
    )
    .await?;
    Ok(turn.final_text)
}

/// Run a prompt through a completion model with an existing conversation history,
/// allowing iterative tool calls and emitting observer callbacks.
///
/// Functional scope:
/// - Appends the user `prompt` to `existing_history` and iterates: request →
///   inspect response → either dispatch tools and loop, or return the final text.
/// - Streams `CompletionStreamEvent`s from the model task to the observer using a
///   `tokio::select!` between the awaiting completion future and an `mpsc` receiver,
///   so progressive UIs see chunks as they arrive instead of only at end-of-turn.
/// - Stamps each tool call into the history (assistant turn) and each tool result
///   back into the history (synthetic user turn) so the next request includes the
///   full context.
/// - Honors hooks, allow-listed tools, and three independent safety budgets.
///
/// Boundary conditions:
/// - `max_turns == 0` is rejected up-front because zero would mean "never call the
///   model" while still expecting an answer.
/// - When the model emits no tool calls and no text but produced reasoning content,
///   the loop returns a `ResponseError` rather than spinning forever — see
///   [`empty_or_reasoning_only_error`].
/// - `terminal_tools` short-circuits the loop the moment a successful call to one of
///   them is recorded; the call's text output (if any) becomes `final_text`.
/// - `MAX_IDENTICAL_BLOCKED_TOOL_CALLS` aborts the loop if hooks/allow-list/preflight
///   reject the same call signature this many times.
/// - `repeat_abort_threshold` aborts the loop if the model successfully repeats the
///   same tool/argument signature too often within the rolling window.
pub async fn run_tool_loop_with_history_and_observer<M: CompletionModel, O: ToolLoopObserver>(
    model: &M,
    mut existing_history: Vec<Message>,
    prompt: impl Into<String>,
    registry: &ToolRegistry,
    config: ToolLoopConfig,
    observer: &mut O,
) -> Result<ToolLoopTurn, CompletionError>
where
    M::Response: CompletionUsage,
{
    existing_history.push(Message::user(prompt.into()));
    let mut history = existing_history;
    let max_turns = config.max_turns.unwrap_or(DEFAULT_MAX_TOOL_LOOP_TURNS);
    if max_turns == 0 {
        return Err(CompletionError::ResponseError(
            "Tool loop max_turns must be greater than 0".to_string(),
        ));
    }
    let mut turn_count = 0usize;
    let mut blocked_signatures: HashMap<String, usize> = HashMap::new();
    let repeat_detection_window = config
        .repeat_detection_window
        .unwrap_or(DEFAULT_REPEAT_DETECTION_WINDOW);
    let repeat_warning_threshold = config
        .repeat_warning_threshold
        .unwrap_or(DEFAULT_REPEAT_WARNING_THRESHOLD);
    let repeat_abort_threshold = config
        .repeat_abort_threshold
        .unwrap_or(DEFAULT_REPEAT_ABORT_THRESHOLD);
    let terminal_tools = config
        .terminal_tools
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect::<HashSet<_>>();
    let mut executed_tool_signatures: VecDeque<String> = VecDeque::new();
    let mut executed_tool_signature_counts: HashMap<String, usize> = HashMap::new();

    let effective_registry = registry_with_source_tools(registry, &config).map_err(|error| {
        CompletionError::ResponseError(format!("failed to load source tools: {error}"))
    })?;
    let registry = &effective_registry;
    let mut tools = registry_tool_definitions(registry);
    if config.subagent_runtime.is_some() && !tools.iter().any(|tool| tool.name == "task") {
        tools.push(tool_definition_from_spec(ToolSpec::task()));
    }

    // Apply agent tool restriction at the *definition* level so the model never sees
    // tools outside its allow-list. The same list is re-checked at execution time
    // below to defend against models that hallucinate names regardless.
    if let Some(ref allowed) = config.allowed_tools {
        tools.retain(|t| {
            allowed.iter().any(|a| a == &t.name) || task_tool_allowed_by_runtime(&config, &t.name)
        });
    }

    loop {
        if tool_loop_cancelled(&config) {
            return Err(tool_loop_cancelled_error());
        }
        if turn_count >= max_turns {
            return Err(CompletionError::ResponseError(format!(
                "Tool loop exceeded maximum turns ({max_turns})"
            )));
        }
        turn_count += 1;

        let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel();
        let request = CompletionRequest {
            preamble: config.preamble.clone(),
            chat_history: history.clone(),
            temperature: config.temperature,
            thinking: config.thinking,
            reasoning_effort: config.reasoning_effort,
            stream: config.stream,
            tools: tools.clone(),
            stream_events: Some(stream_tx),
            ..Default::default()
        };

        record_tool_loop_context_frame(&config, turn_count, &request);
        observer.on_model_turn_start(turn_count);
        let model_request_started = std::time::Instant::now();
        let response_result = {
            // Drive the completion future and stream events concurrently. When the
            // future resolves we drain any events still buffered on the channel before
            // breaking out, otherwise late events would be lost.
            let completion = model.completion(request);
            tokio::pin!(completion);

            loop {
                tokio::select! {
                    result = &mut completion => {
                        while let Ok(event) = stream_rx.try_recv() {
                            observer.on_model_stream_event(&event);
                        }
                        break result;
                    }
                    Some(event) = stream_rx.recv() => {
                        observer.on_model_stream_event(&event);
                    }
                    _ = wait_for_tool_loop_cancellation(config.cancellation.as_ref()) => {
                        break Err(tool_loop_cancelled_error());
                    }
                }
            }
        };
        let wall_clock_ms = duration_millis_u64(model_request_started.elapsed());
        let response = match response_result {
            Ok(response) => response,
            Err(error) => {
                if let (Some(recorder), Some(context)) = (
                    config.usage_recorder.as_ref(),
                    config.usage_context.as_ref(),
                ) && let Err(record_error) = recorder
                    .record_failure(context, completion_error_kind(&error), Some(wall_clock_ms))
                    .await
                {
                    tracing::warn!("failed to record failed model usage stats: {record_error}");
                }
                return Err(error);
            }
        };

        let mut tool_calls = Vec::new();
        let mut text_parts = Vec::new();
        for content in &response.content {
            match content {
                AssistantContent::ToolCall(call) => tool_calls.push(call.clone()),
                AssistantContent::Text(text) => {
                    if !text.text.trim().is_empty() {
                        text_parts.push(text.text.clone());
                    }
                }
            }
        }
        if let (Some(recorder), Some(context)) = (
            config.usage_recorder.as_ref(),
            config.usage_context.as_ref(),
        ) {
            let tool_call_count = u64::try_from(tool_calls.len()).unwrap_or(u64::MAX);
            match response.raw_response.usage_summary() {
                Some(usage) => {
                    if let Err(error) = recorder
                        .record_summary_with_tool_count(
                            context,
                            &usage,
                            Some(wall_clock_ms),
                            tool_call_count,
                        )
                        .await
                    {
                        tracing::warn!("failed to record model usage stats: {error}");
                    }
                    observer.on_model_usage_recorded(&usage, wall_clock_ms);
                }
                None => {
                    if let Err(error) = recorder
                        .record_missing_usage(context, Some(wall_clock_ms), tool_call_count)
                        .await
                    {
                        tracing::warn!("failed to record estimated model usage stats: {error}");
                    }
                }
            }
        } else if let Some(usage) = response.raw_response.usage_summary() {
            observer.on_model_usage_recorded(&usage, wall_clock_ms);
        }

        if !tool_calls.is_empty() {
            if !text_parts.is_empty() {
                observer.on_assistant_step_text(&text_parts.join("\n"));
            }

            let assistant_content = OneOrMany::many(response.content.clone()).ok_or_else(|| {
                CompletionError::ResponseError(
                    "Empty assistant content in tool call response".to_string(),
                )
            })?;
            history.push(Message::Assistant {
                id: None,
                reasoning_content: history_reasoning_content(
                    response.reasoning_content.clone(),
                    config.preserve_reasoning_content,
                ),
                content: assistant_content,
            });

            for call in tool_calls {
                if tool_loop_cancelled(&config) {
                    return Err(tool_loop_cancelled_error());
                }
                observer.on_tool_call_begin(
                    &call.id,
                    &call.function.name,
                    &call.function.arguments,
                );

                if let Err(reason) = observer.on_tool_call_preflight(
                    &call.id,
                    &call.function.name,
                    &call.function.arguments,
                ) {
                    let blocked_result: Result<ToolOutput, String> = Err(reason.clone());
                    observer.on_tool_call_end(&call.id, &call.function.name, &blocked_result);

                    let result_json = ToolOutput::failure(reason).into_response();
                    history.push(Message::User {
                        content: OneOrMany::One(UserContent::ToolResult(ToolResult {
                            id: call.id,
                            name: call.function.name.clone(),
                            result: result_json,
                        })),
                    });
                    let signature =
                        blocked_tool_call_signature(&call.function.name, &call.function.arguments);
                    if increment_blocked_count(&mut blocked_signatures, &signature)
                        >= MAX_IDENTICAL_BLOCKED_TOOL_CALLS
                    {
                        return Err(CompletionError::ResponseError(format!(
                            "Tool loop aborted after repeated blocked calls to '{}' with identical arguments",
                            call.function.name
                        )));
                    }
                    continue;
                }

                // Run PreToolUse hooks (may block the tool call)
                if let Some(ref hook_runner) = config.hook_runner {
                    let action = hook_runner
                        .run_pre_tool_use(&call.function.name, call.function.arguments.clone())
                        .await;
                    if let HookAction::Block(reason) = action {
                        let blocked_result: Result<ToolOutput, String> =
                            Err(format!("Blocked by hook: {reason}"));
                        observer.on_tool_call_end(&call.id, &call.function.name, &blocked_result);

                        let result_json = ToolOutput::failure(format!("Blocked by hook: {reason}"))
                            .into_response();

                        history.push(Message::User {
                            content: OneOrMany::One(UserContent::ToolResult(ToolResult {
                                id: call.id,
                                name: call.function.name.clone(),
                                result: result_json,
                            })),
                        });
                        let signature = blocked_tool_call_signature(
                            &call.function.name,
                            &call.function.arguments,
                        );
                        if increment_blocked_count(&mut blocked_signatures, &signature)
                            >= MAX_IDENTICAL_BLOCKED_TOOL_CALLS
                        {
                            return Err(CompletionError::ResponseError(format!(
                                "Tool loop aborted after repeated blocked calls to '{}' with identical arguments",
                                call.function.name
                            )));
                        }
                        continue;
                    }
                }

                // Enforce allowed_tools at execution time (not just definition filtering)
                if let Some(ref allowed) = config.allowed_tools
                    && !allowed.iter().any(|a| a == &call.function.name)
                    && !task_tool_allowed_by_runtime(&config, &call.function.name)
                {
                    let blocked_msg = format!(
                        "Tool '{}' is not in the allowed_tools list for this agent",
                        call.function.name
                    );
                    let blocked_result: Result<ToolOutput, String> = Err(blocked_msg.clone());
                    observer.on_tool_call_end(&call.id, &call.function.name, &blocked_result);

                    let result_json = ToolOutput::failure(blocked_msg).into_response();

                    history.push(Message::User {
                        content: OneOrMany::One(UserContent::ToolResult(ToolResult {
                            id: call.id,
                            name: call.function.name.clone(),
                            result: result_json,
                        })),
                    });
                    let signature =
                        blocked_tool_call_signature(&call.function.name, &call.function.arguments);
                    if increment_blocked_count(&mut blocked_signatures, &signature)
                        >= MAX_IDENTICAL_BLOCKED_TOOL_CALLS
                    {
                        return Err(CompletionError::ResponseError(format!(
                            "Tool loop aborted after repeated blocked calls to '{}' with identical arguments",
                            call.function.name
                        )));
                    }
                    continue;
                }

                let mut invocation = ToolInvocation::new(
                    call.id.clone(),
                    call.function.name.clone(),
                    ToolPayload::Function {
                        arguments: tool_arguments_json(&call.function.arguments),
                    },
                    registry.working_dir().to_path_buf(),
                );
                if let Some(runtime_context) = config.runtime_context.clone() {
                    invocation = invocation.with_runtime_context(runtime_context);
                }

                let tool_name = call.function.name.clone();
                let mutates_state = if tool_name == "task" {
                    // A task may delegate an arbitrary child tool surface, so
                    // treat it as mutating until the child contract is folded
                    // into this runtime boundary.
                    true
                } else {
                    registry
                        .invocation_may_mutate(&invocation)
                        .await
                        .map_err(|error| {
                            CompletionError::ResponseError(format!(
                                "failed to classify tool '{tool_name}' for cancellation: {error}"
                            ))
                        })?
                };
                // Claim the mutation boundary before the final cancellation
                // check. If cancellation won the race, clear the marker and
                // exit without dispatching. If it arrives after this check,
                // the runtime sees the marker and waits for the handler's
                // determinate result instead of hard-aborting it.
                if mutates_state && let Some(cancellation) = config.cancellation.as_ref() {
                    cancellation.mark_mutation_started();
                    if cancellation.is_cancelled() {
                        cancellation.mark_mutation_finished();
                        return Err(tool_loop_cancelled_error());
                    }
                } else if tool_loop_cancelled(&config) {
                    return Err(tool_loop_cancelled_error());
                }
                let mut tool_result: Result<ToolOutput, String> = if tool_name == "task" {
                    dispatch_task_tool_call(
                        config.subagent_runtime.as_ref(),
                        // Hand the dispatched child the parent loop's
                        // *live* per-turn runtime context (sandbox /
                        // approval / file-history authority) rather than
                        // the session-start snapshot, so child tool
                        // calls inherit the current file-history batch
                        // (undo preimage recording) and approval scope
                        // (S2-INV-06).
                        config.runtime_context.clone(),
                        &call.id,
                        &call.function.arguments,
                        observer,
                    )
                    .await
                } else if mutates_state {
                    match registry.dispatch(invocation).await {
                        Ok(output) => Ok(output),
                        Err(err) => Err(format!("Tool '{}' failed: {}", tool_name, err)),
                    }
                } else {
                    tokio::select! {
                        _ = wait_for_tool_loop_cancellation(config.cancellation.as_ref()) => {
                            return Err(tool_loop_cancelled_error());
                        }
                        result = registry.dispatch(invocation) => match result {
                            Ok(output) => Ok(output),
                            Err(err) => Err(format!("Tool '{}' failed: {}", tool_name, err)),
                        }
                    }
                };
                if mutates_state && let Some(cancellation) = config.cancellation.as_ref() {
                    cancellation.mark_mutation_finished();
                }
                blocked_signatures.clear();
                let repeat_status = record_executed_tool_signature(
                    &mut executed_tool_signatures,
                    &mut executed_tool_signature_counts,
                    &tool_name,
                    &call.function.arguments,
                    repeat_detection_window,
                    repeat_warning_threshold,
                );
                if let Some(warning) = repeat_status.warning.as_deref() {
                    append_repeat_warning_to_tool_result(&mut tool_result, warning);
                }

                observer.on_tool_call_end(&call.id, &tool_name, &tool_result);

                // Run PostToolUse hooks
                if let Some(ref hook_runner) = config.hook_runner {
                    let output_json = match &tool_result {
                        Ok(output) => output.clone().into_response(),
                        Err(msg) => serde_json::json!({"error": msg}),
                    };
                    hook_runner
                        .run_post_tool_use(&tool_name, call.function.arguments.clone(), output_json)
                        .await;
                }

                let result_json = match &tool_result {
                    Ok(output) => output.clone().into_response(),
                    Err(message) => ToolOutput::failure(message.clone()).into_response(),
                };

                history.push(Message::User {
                    content: OneOrMany::One(UserContent::ToolResult(ToolResult {
                        id: call.id,
                        name: tool_name.clone(),
                        result: result_json,
                    })),
                });

                if should_abort_repeated_tool_call(repeat_status.count, repeat_abort_threshold) {
                    return Err(CompletionError::ResponseError(format!(
                        "Tool loop aborted after repeated calls to '{}' with identical arguments {} times in the last {} executed tool calls: {}",
                        tool_name,
                        repeat_status.count,
                        repeat_detection_window,
                        truncate_signature_arguments(&call.function.arguments),
                    )));
                }

                if tool_result.as_ref().is_ok_and(|output| output.is_success())
                    && terminal_tools.contains(&tool_name)
                {
                    return Ok(ToolLoopTurn {
                        final_text: terminal_tool_final_text(&tool_name, &tool_result),
                        history,
                    });
                }
            }

            continue;
        }

        let final_text = text_parts.join("\n");
        if !final_text.trim().is_empty() {
            let assistant_content = OneOrMany::many(response.content.clone()).ok_or_else(|| {
                CompletionError::ResponseError("Empty assistant text response".to_string())
            })?;
            history.push(Message::Assistant {
                id: None,
                reasoning_content: history_reasoning_content(
                    response.reasoning_content.clone(),
                    config.preserve_reasoning_content,
                ),
                content: assistant_content,
            });
            return Ok(ToolLoopTurn {
                final_text,
                history,
            });
        }

        if !response.content.is_empty() {
            return Err(empty_or_reasoning_only_error(&response));
        }
        return Err(empty_or_reasoning_only_error(&response));
    }
}

/// Normalize tool-call arguments to the JSON string shape that tool dispatch expects.
///
/// Models may already emit a JSON-serialized string for arguments (some providers do
/// this for backward compatibility with pre-tool-use APIs). When the value is a string
/// that itself parses as JSON we forward it verbatim; otherwise we re-serialize.
fn tool_arguments_json(arguments: &Value) -> String {
    match arguments {
        Value::String(raw) => {
            if serde_json::from_str::<Value>(raw).is_ok() {
                raw.clone()
            } else {
                arguments.to_string()
            }
        }
        _ => arguments.to_string(),
    }
}

/// Build a signature `"<tool>|<canonical-args>"` used to detect repeated *blocked*
/// calls. The canonical form sorts object keys so semantically identical arguments
/// produce identical signatures regardless of JSON ordering.
fn blocked_tool_call_signature(tool_name: &str, arguments: &Value) -> String {
    format!("{tool_name}|{}", canonical_json_value(arguments))
}

/// Increment and return the blocked count for `signature`, mutating the cache in
/// place. The caller compares the returned count against `MAX_IDENTICAL_BLOCKED_TOOL_CALLS`.
fn increment_blocked_count(
    blocked_signatures: &mut HashMap<String, usize>,
    signature: &str,
) -> usize {
    let count = blocked_signatures.entry(signature.to_string()).or_insert(0);
    *count += 1;
    *count
}

/// Decide whether to keep the model's reasoning content in subsequent requests.
///
/// Some providers (notably reasoning models) charge for re-sending long thoughts; we
/// drop them by default. Setting `preserve_reasoning_content` is opt-in for callers
/// that want exact replay (e.g. plan executors that need the same chain of thought).
fn history_reasoning_content(
    reasoning_content: Option<String>,
    preserve_reasoning_content: bool,
) -> Option<String> {
    if preserve_reasoning_content {
        reasoning_content
    } else {
        None
    }
}

/// Outcome of recording an executed tool call against the rolling window.
///
/// `count` is the number of times the same signature appears in the current window
/// (after recording the new call); `warning` is set when the count crosses the warn
/// threshold so the loop can surface a system message back to the model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RepeatToolCallStatus {
    count: usize,
    warning: Option<String>,
}

/// Record an executed tool call and update the rolling window.
///
/// Functional scope: pushes the new signature, increments its count, and evicts the
/// oldest entry if the window grew beyond `window`. Returns the updated count and a
/// warning string when the count crosses `threshold`.
///
/// Boundary conditions:
/// - `window == 0` disables repeat detection entirely; `threshold == 0` disables the
///   warning while leaving the counter intact for the abort check.
/// - When a signature's eviction drops its count to zero the entry is removed from
///   the count map to keep memory bounded over very long sessions.
fn record_executed_tool_signature(
    recent_signatures: &mut VecDeque<String>,
    signature_counts: &mut HashMap<String, usize>,
    tool_name: &str,
    arguments: &Value,
    window: usize,
    threshold: usize,
) -> RepeatToolCallStatus {
    if window == 0 {
        return RepeatToolCallStatus::default();
    }

    let signature = format!("{tool_name}|{}", canonical_json_value(arguments));
    recent_signatures.push_back(signature.clone());
    *signature_counts.entry(signature.clone()).or_insert(0) += 1;

    while recent_signatures.len() > window {
        if let Some(expired) = recent_signatures.pop_front()
            && let Some(count) = signature_counts.get_mut(&expired)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                signature_counts.remove(&expired);
            }
        }
    }

    let count = signature_counts
        .get(&signature)
        .copied()
        .unwrap_or_default();
    let warning = if threshold > 0 && count >= threshold {
        Some(format!(
            "[system warning: repeated tool call] You have called `{tool_name}` with the same arguments {count} times in the last {window} executed tool calls. The prior result is in history above — read it instead of re-running the call. Either explore something new, or, if you have enough evidence, call `submit_task_complete` to end the task with a structured verdict."
        ))
    } else {
        None
    };
    RepeatToolCallStatus { count, warning }
}

/// Decide whether the loop must abort because the same tool signature has executed
/// too often. `abort_threshold == 0` disables the abort check.
fn should_abort_repeated_tool_call(count: usize, abort_threshold: usize) -> bool {
    abort_threshold > 0 && count >= abort_threshold
}

/// Build a `CompletionError::ResponseError` that explains why the loop refused to
/// continue with the given response.
///
/// Three cases produce specific messages:
/// 1. Empty content with non-empty reasoning content — the model is "thinking out
///    loud" but never producing visible text or tool calls.
/// 2. Empty content overall — provider returned a malformed response.
/// 3. Non-empty content that contains nothing actionable (e.g. only thoughts as
///    `AssistantContent` blocks).
fn empty_or_reasoning_only_error<R>(response: &CompletionResponse<R>) -> CompletionError {
    if response.content.is_empty() {
        if response
            .reasoning_content
            .as_deref()
            .is_some_and(|content| !content.trim().is_empty())
        {
            return CompletionError::ResponseError(
                "Model returned reasoning_content without text or tool calls; aborting to prevent an infinite tool loop".to_string(),
            );
        }
        return CompletionError::ResponseError(
            "Model returned empty response with no text or tool calls; aborting to prevent an infinite tool loop".to_string(),
        );
    }

    CompletionError::ResponseError(
        "Model returned non-text response (likely only thought or unsupported content)".to_string(),
    )
}

/// Render a compact preview of repeated arguments for the abort error message.
///
/// `MAX_LEN` of 240 keeps the error readable even for large argument blobs; we count
/// chars (not bytes) so multibyte content does not slice through a code point.
fn truncate_signature_arguments(arguments: &Value) -> String {
    let serialized = canonical_json_value(arguments);
    const MAX_LEN: usize = 240;
    if serialized.chars().count() <= MAX_LEN {
        serialized
    } else {
        let mut truncated = serialized.chars().take(MAX_LEN).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

/// Compute the `final_text` to surface when a terminal tool short-circuits the loop.
///
/// Prefers the tool's textual output, falls back to a human-readable confirmation,
/// and on failure uses the error message verbatim so the caller still learns what
/// went wrong.
fn terminal_tool_final_text(tool_name: &str, tool_result: &Result<ToolOutput, String>) -> String {
    match tool_result {
        Ok(output) => output
            .as_text()
            .map(str::trim)
            .filter(|content| !content.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("Tool '{tool_name}' completed.")),
        Err(message) => message.clone(),
    }
}

async fn dispatch_task_tool_call<O: ToolLoopObserver>(
    runtime: Option<&SubAgentToolLoopRuntime>,
    live_runtime_context: Option<ToolRuntimeContext>,
    call_id: &str,
    arguments: &Value,
    observer: &mut O,
) -> Result<ToolOutput, String> {
    let runtime =
        runtime.ok_or_else(|| "Tool 'task' is not available in this session".to_string())?;
    let invocation = parse_task_invocation(arguments)?;
    let ctx = runtime.dispatch_context(format!("tool-call:{call_id}"), live_runtime_context);
    match runtime
        .dispatcher
        .dispatch(ctx, invocation, TaskEntryKind::LlmInitiated)
        .await
    {
        Ok(result) => {
            // Surface the sub-agent's usage to the parent observer
            // with attribution so OC-Phase 5 per-agent budget
            // enforcement (`check_agent`) sees a non-default
            // accumulation. The default observer impl folds this
            // into the parent's anonymous `on_model_usage` bucket;
            // a TUI observer override routes it through
            // `BudgetTracker::accumulate(..., Some(agent_name))`.
            observer.on_sub_agent_completed(&result.agent_name, &result.usage);
            Ok(task_result_output(result))
        }
        Err(failure) => Err(format!(
            "Task dispatch failed: {}",
            task_failure_label(&failure)
        )),
    }
}

fn parse_task_invocation(arguments: &Value) -> Result<TaskInvocation, String> {
    let raw = tool_arguments_json(arguments);
    let mut invocation: TaskInvocation = serde_json::from_str(&raw)
        .map_err(|error| format!("task arguments must be valid JSON object: {error}"))?;
    invocation.description = trim_required_task_string("description", invocation.description)?;
    invocation.prompt = trim_required_task_string("prompt", invocation.prompt)?;
    invocation.subagent_type =
        trim_required_task_string("subagent_type", invocation.subagent_type)?;
    invocation.task_id = invocation
        .task_id
        .and_then(|value| non_empty_trimmed_string(value).ok());
    Ok(invocation)
}

fn task_tool_allowed_by_runtime(config: &ToolLoopConfig, tool_name: &str) -> bool {
    tool_name == "task" && config.subagent_runtime.is_some()
}

fn trim_required_task_string(key: &str, value: String) -> Result<String, String> {
    non_empty_trimmed_string(value)
        .map_err(|_| format!("task argument `{key}` must be a non-empty string"))
}

fn non_empty_trimmed_string(value: String) -> Result<String, ()> {
    let value = value.trim().to_string();
    if value.is_empty() { Err(()) } else { Ok(value) }
}

fn task_result_output(result: TaskResult) -> ToolOutput {
    ToolOutput::success(format!(
        "task_id: {}\nagent: {}\nmodel: {}/{}\nsteps_used: {}\n<task_result>\n{}\n</task_result>",
        result.task_id,
        result.agent_name,
        result.provider_id,
        result.model_id,
        result.steps_used,
        result.final_text.trim()
    ))
}

fn task_failure_label(failure: &TaskFailure) -> String {
    match failure {
        TaskFailure::FeatureDisabled => "multi-agent feature is disabled".to_string(),
        TaskFailure::UnknownSubagent { name, suggestions } => {
            if suggestions.is_empty() {
                format!("unknown sub-agent `{name}`")
            } else {
                format!(
                    "unknown sub-agent `{name}`; available sub-agents: {}",
                    suggestions.join(", ")
                )
            }
        }
        TaskFailure::DepthExceeded { current, limit } => {
            format!("sub-agent depth {current} exceeds configured limit {limit}")
        }
        TaskFailure::ConcurrencyExceeded { current, limit } => {
            format!("sub-agent concurrency {current} exceeds configured limit {limit}")
        }
        TaskFailure::PermissionEscalationDenied {
            permission,
            pattern,
        } => {
            format!("permission escalation denied for `{permission}:{pattern}`")
        }
        TaskFailure::SafetyDenied(denial) => {
            format!("safety policy denied spawn: {}", denial.reason)
        }
        TaskFailure::ApprovalRejected { feedback } => feedback
            .as_deref()
            .map(|message| format!("approval rejected: {message}"))
            .unwrap_or_else(|| "approval rejected".to_string()),
        TaskFailure::BudgetExceeded(reason) => format!("sub-agent budget exceeded: {reason:?}"),
        TaskFailure::ContextHandoffFailed(reason) => {
            format!("context handoff failed: {reason:?}")
        }
        TaskFailure::ProviderError(error) => format!("provider error: {error}"),
        TaskFailure::ChildToolLoopFailed(error) => format!("child tool loop failed: {error:?}"),
        TaskFailure::Cancelled { source } => format!("sub-agent cancelled by {source:?}"),
        TaskFailure::Timeout { wall_clock_ms } => {
            format!("sub-agent timed out after {wall_clock_ms} ms")
        }
    }
}

/// Inject the repeat-call warning into the next tool result so the model sees it on
/// its next turn.
///
/// The injection point depends on the result shape:
/// - Function tools: append to the textual content with a blank-line separator.
/// - MCP tools: add a `repeat_warning` key to the JSON object so structured callers
///   can detect it without regex.
/// - Errors: append to the error string the same way as function content.
fn append_repeat_warning_to_tool_result(result: &mut Result<ToolOutput, String>, warning: &str) {
    match result {
        Ok(ToolOutput::Function { content, .. }) => {
            if !content.is_empty() {
                content.push_str("\n\n");
            }
            content.push_str(warning);
        }
        Ok(ToolOutput::Mcp { result }) => {
            if let Value::Object(object) = result {
                object.insert(
                    "repeat_warning".to_string(),
                    Value::String(warning.to_string()),
                );
            }
        }
        Err(message) => {
            if !message.is_empty() {
                message.push_str("\n\n");
            }
            message.push_str(warning);
        }
    }
}

/// Stable string representation of a JSON value with object keys sorted.
///
/// Used to compare argument payloads for equality regardless of how the LLM emitted
/// the keys. Cheaper than re-parsing because we walk the in-memory `Value` directly.
fn canonical_json_value(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.to_string(),
        Value::Array(items) => {
            let inner = items
                .iter()
                .map(canonical_json_value)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{inner}]")
        }
        Value::Object(object) => {
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);
            let inner = entries
                .into_iter()
                .map(|(key, value)| {
                    let key = match serde_json::to_string(key) {
                        Ok(serialized) => serialized,
                        Err(_) => "\"<invalid-key>\"".to_string(),
                    };
                    format!("{key}:{}", canonical_json_value(value))
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{inner}}}")
        }
    }
}

/// Convert a [`ToolRegistry`] into `ToolDefinition`s shaped for the completion model.
///
/// Functional scope: pulls the registry's specs and converts each parameter schema to
/// JSON. `FunctionParameters::Empty` is rewritten to an explicit empty-object schema
/// because some providers reject tools with no `parameters` field.
fn registry_tool_definitions(registry: &ToolRegistry) -> Vec<ToolDefinition> {
    registry
        .tool_specs()
        .into_iter()
        .map(tool_definition_from_spec)
        .collect()
}

fn tool_definition_from_spec(spec: ToolSpec) -> ToolDefinition {
    let parameters = match spec.function.parameters {
        FunctionParameters::Empty => serde_json::json!({
            "type": "object",
            "properties": {}
        }),
        params => serde_json::to_value(params).unwrap_or_else(|_| {
            serde_json::json!({
                "type": "object",
                "properties": {}
            })
        }),
    };
    ToolDefinition {
        name: spec.function.name,
        description: spec.function.description,
        parameters,
    }
}

fn registry_with_source_tools(
    registry: &ToolRegistry,
    config: &ToolLoopConfig,
) -> Result<ToolRegistry, SourcePoolError> {
    let mut effective = registry.clone();
    let Some(source_pool) = config.source_pool.as_ref() else {
        return Ok(effective);
    };
    let session_id = config
        .source_session_id
        .as_deref()
        .or_else(|| {
            config
                .usage_context
                .as_ref()
                .and_then(|context| context.session_id.as_deref())
        })
        .unwrap_or("session");
    for (name, handler) in
        source_pool.tool_handlers_for_session(session_id, SourceToolNaming::Prefixed)?
    {
        effective.register(name, handler);
    }
    Ok(effective)
}

fn record_tool_loop_context_frame(
    config: &ToolLoopConfig,
    model_turn: usize,
    request: &CompletionRequest,
) {
    let Some(session_root) = config.context_frame_session_root.as_deref() else {
        return;
    };

    let prompt_id = context_frame_prompt_id(config.context_frame_prompt_id.as_deref(), model_turn);
    let budget = config.context_frame_budget.clone().unwrap_or_default();
    let frame = match build_tool_loop_context_frame(
        session_root,
        prompt_id,
        request,
        budget,
        config.context_frame_attachment_threshold_bytes,
    ) {
        Ok(frame) => frame,
        Err(error) => {
            tracing::warn!(
                model_turn,
                error = %error,
                "failed to build tool-loop context frame"
            );
            return;
        }
    };

    let store = SessionJsonlStore::new(session_root.to_path_buf());
    if let Err(error) = store.append(&SessionEvent::context_frame(frame.clone())) {
        tracing::warn!(
            model_turn,
            error = %error,
            "failed to append tool-loop context frame"
        );
        return;
    }

    if frame_requires_compaction_event(&frame) {
        let compaction = CompactionEvent::from_frame(
            &frame,
            CompactionReason::BudgetPressure,
            format!("deterministic context allocation for model turn {model_turn}"),
        );
        if let Err(error) = store.append(&SessionEvent::compaction(compaction)) {
            tracing::warn!(
                model_turn,
                error = %error,
                "failed to append context compaction event"
            );
        }
    }
}

fn context_frame_prompt_id(base_prompt_id: Option<&str>, model_turn: usize) -> String {
    match base_prompt_id.filter(|value| !value.trim().is_empty()) {
        Some(base) => format!("{base}/model-turn-{model_turn}"),
        None => format!("model-turn-{model_turn}"),
    }
}

fn duration_millis_u64(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn completion_error_kind(error: &CompletionError) -> &'static str {
    match error {
        CompletionError::HttpError(error) if error.is_timeout() => "timeout",
        CompletionError::HttpError(_) => "http_error",
        CompletionError::JsonError(_) => "json_error",
        CompletionError::RequestError(error) if error_message_is_cancelled(&error.to_string()) => {
            "cancelled"
        }
        CompletionError::RequestError(error) if error_message_is_timeout(&error.to_string()) => {
            "timeout"
        }
        CompletionError::RequestError(_) => "request_error",
        CompletionError::ProviderError(message) if error_message_is_cancelled(message) => {
            "cancelled"
        }
        CompletionError::ProviderError(message) if error_message_is_timeout(message) => "timeout",
        CompletionError::ProviderError(_) => "provider_error",
        CompletionError::ResponseError(message) if error_message_is_cancelled(message) => {
            "cancelled"
        }
        CompletionError::ResponseError(message) if error_message_is_timeout(message) => "timeout",
        CompletionError::ResponseError(_) => "response_error",
        CompletionError::NotImplemented(_) => "not_implemented",
    }
}

fn error_message_is_cancelled(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("cancelled") || lower.contains("canceled")
}

fn error_message_is_timeout(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("timeout") || lower.contains("timed out")
}

fn build_tool_loop_context_frame(
    session_root: &Path,
    prompt_id: String,
    request: &CompletionRequest,
    budget: ContextBudget,
    attachment_threshold_bytes: Option<usize>,
) -> io::Result<ContextFrameEvent> {
    let attachments = ContextAttachmentStore::new(session_root);
    let mut builder =
        ContextFrameBuilder::new(ContextFrameKind::PromptBuild, budget).with_prompt_id(prompt_id);
    if let Some(threshold) = attachment_threshold_bytes {
        builder = builder.with_attachment_threshold_bytes(threshold);
    }

    if let Some(preamble) = request.preamble.as_deref()
        && !preamble.trim().is_empty()
    {
        builder = builder.push(
            ContextFrameCandidate::new("preamble", ContextSegmentKind::SystemRules, preamble)
                .source(ContextFrameSource::runtime("tool_loop_preamble"))
                .trust(ContextTrustLevel::Trusted)
                .non_compressible(true),
        );
    }

    for (message_index, message) in request.chat_history.iter().enumerate() {
        builder = push_message_context_candidates(builder, message_index, message);
    }

    builder.build(&attachments)
}

fn push_message_context_candidates(
    mut builder: ContextFrameBuilder,
    message_index: usize,
    message: &Message,
) -> ContextFrameBuilder {
    match message {
        Message::User { content } => {
            for (part_index, part) in content.iter().enumerate() {
                match part {
                    UserContent::Text(text) => {
                        builder = push_text_context_candidate(
                            builder,
                            format!("message-{message_index}-user-{part_index}"),
                            ContextSegmentKind::RecentMessages,
                            text.text.clone(),
                            ContextFrameSource::runtime("conversation_user"),
                            ContextTrustLevel::Untrusted,
                            false,
                        );
                    }
                    UserContent::Image(image) => {
                        builder = push_text_context_candidate(
                            builder,
                            format!("message-{message_index}-user-image-{part_index}"),
                            ContextSegmentKind::RecentMessages,
                            format!(
                                "image mime_type={} bytes={}",
                                image.mime_type.as_deref().unwrap_or("unknown"),
                                image.data.len()
                            ),
                            ContextFrameSource::runtime("conversation_user_image"),
                            ContextTrustLevel::Untrusted,
                            false,
                        );
                    }
                    UserContent::ToolResult(result) => {
                        builder = push_text_context_candidate(
                            builder,
                            format!("message-{message_index}-tool-result-{part_index}"),
                            ContextSegmentKind::ToolResults,
                            render_tool_result_context(result),
                            ContextFrameSource::tool(result.name.clone(), result.id.clone()),
                            ContextTrustLevel::External,
                            false,
                        );
                    }
                }
            }
        }
        Message::Assistant { content, .. } => {
            for (part_index, part) in content.iter().enumerate() {
                match part {
                    AssistantContent::Text(text) => {
                        builder = push_text_context_candidate(
                            builder,
                            format!("message-{message_index}-assistant-{part_index}"),
                            ContextSegmentKind::RecentMessages,
                            text.text.clone(),
                            ContextFrameSource::runtime("conversation_assistant"),
                            ContextTrustLevel::Trusted,
                            false,
                        );
                    }
                    AssistantContent::ToolCall(call) => {
                        builder = push_text_context_candidate(
                            builder,
                            format!("message-{message_index}-tool-call-{part_index}"),
                            ContextSegmentKind::RecentMessages,
                            render_tool_call_context(call),
                            ContextFrameSource::runtime("conversation_assistant_tool_call"),
                            ContextTrustLevel::Trusted,
                            false,
                        );
                    }
                }
            }
        }
        Message::System { content } => {
            for (part_index, part) in content.iter().enumerate() {
                if let Some(content) = render_user_content_context(part) {
                    builder = push_text_context_candidate(
                        builder,
                        format!("message-{message_index}-system-{part_index}"),
                        ContextSegmentKind::SystemRules,
                        content,
                        ContextFrameSource::runtime("conversation_system"),
                        ContextTrustLevel::Trusted,
                        true,
                    );
                }
            }
        }
    }
    builder
}

fn push_text_context_candidate(
    builder: ContextFrameBuilder,
    id: String,
    segment: ContextSegmentKind,
    content: String,
    source: ContextFrameSource,
    trust: ContextTrustLevel,
    non_compressible: bool,
) -> ContextFrameBuilder {
    if content.trim().is_empty() {
        return builder;
    }

    builder.push(
        ContextFrameCandidate::new(id, segment, content)
            .source(source)
            .trust(trust)
            .non_compressible(non_compressible),
    )
}

fn render_user_content_context(content: &UserContent) -> Option<String> {
    match content {
        UserContent::Text(text) => Some(text.text.clone()),
        UserContent::Image(image) => Some(format!(
            "image mime_type={} bytes={}",
            image.mime_type.as_deref().unwrap_or("unknown"),
            image.data.len()
        )),
        UserContent::ToolResult(result) => Some(render_tool_result_context(result)),
    }
}

fn render_tool_call_context(call: &crate::internal::ai::completion::ToolCall) -> String {
    format!(
        "tool_call id={} name={} arguments={}",
        call.id,
        call.function.name,
        json_string(&call.function.arguments)
    )
}

fn render_tool_result_context(result: &ToolResult) -> String {
    format!(
        "tool_result id={} name={} result={}",
        result.id,
        result.name,
        json_string(&result.result)
    )
}

fn json_string(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|error| {
        format!("\"<failed to serialize JSON value for context frame: {error}>\"")
    })
}

fn frame_requires_compaction_event(frame: &ContextFrameEvent) -> bool {
    frame.budget_exceeded_by > 0
        || !frame.omissions.is_empty()
        || frame
            .segments
            .iter()
            .any(|segment| segment.attachment.is_some())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use serde_json::json;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::internal::ai::{
        agent::profile::{AgentExecutionSpec, AgentMode, ModelBinding},
        completion::{
            CompletionResponse,
            message::{Function, Text, ToolCall},
        },
        permission::PermissionRuleset,
        providers::{ProviderBuildOptions, ProviderFactory},
        session::jsonl::SessionJsonlStore,
        sources::{
            CapabilityManifest, Source, SourceCallContext, SourceKind, SourcePool,
            SourceToolCapability, TrustTier,
        },
        tools::{ToolHandler, ToolKind, ToolSpec},
        usage::UsageRecorder,
    };

    /// Default `ToolLoopConfig` is the legacy non-Goal shape:
    /// `goal_stop_policy` is `None`. The TUI Goal path binds
    /// `GoalStopPolicy::GoalBound { goal_id }` explicitly before it
    /// enters the supervisor-aware driver.
    #[test]
    fn tool_loop_config_default_goal_stop_policy_is_none() {
        let config = ToolLoopConfig::default();
        assert!(
            config.goal_stop_policy.is_none(),
            "goal_stop_policy must default to None so existing callers stay on the legacy non-Goal path",
        );
    }

    /// `goal_stop_policy` accepts a `GoalBound { goal_id }` policy
    /// without translation — the field is a plain
    /// `Option<GoalStopPolicy>` so callers can distinguish legacy
    /// non-Goal `None` from an explicitly bound Goal policy. This
    /// pins the schema shape; changing it (e.g. to `Option<Uuid>` or
    /// to a sibling enum) trips this guard.
    #[test]
    fn tool_loop_config_accepts_goal_bound_stop_policy() {
        let goal_id = Uuid::from_u128(0xbadc_afed_eadb_eef0_0000_0000_0000_0001);
        let config = ToolLoopConfig {
            goal_stop_policy: Some(GoalStopPolicy::GoalBound { goal_id }),
            ..ToolLoopConfig::default()
        };
        match config.goal_stop_policy {
            Some(GoalStopPolicy::GoalBound { goal_id: pinned }) => {
                assert_eq!(pinned, goal_id);
            }
            other => panic!("unexpected goal_stop_policy shape: {other:?}"),
        }
    }

    #[derive(Clone)]
    struct MockModel;

    impl CompletionModel for MockModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let has_tool_result = request.chat_history.iter().any(|msg| match msg {
                Message::User { content } => content
                    .iter()
                    .any(|c| matches!(c, UserContent::ToolResult(_))),
                _ => false,
            });

            if !has_tool_result {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_1".to_string(),
                        name: "mock_tool".to_string(),
                        function: Function {
                            name: "mock_tool".to_string(),
                            arguments: json!({"value": 1}),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            Ok(CompletionResponse {
                content: vec![AssistantContent::Text(Text {
                    text: "done".to_string(),
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    struct MockHandler;

    #[async_trait]
    impl ToolHandler for MockHandler {
        fn kind(&self) -> ToolKind {
            ToolKind::Function
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
        ) -> crate::internal::ai::tools::ToolResult<ToolOutput> {
            Ok(ToolOutput::success("ok"))
        }

        fn schema(&self) -> ToolSpec {
            ToolSpec::new("mock_tool", "mock tool")
        }
    }

    struct MutatingMockHandler;

    #[async_trait]
    impl ToolHandler for MutatingMockHandler {
        fn kind(&self) -> ToolKind {
            ToolKind::Function
        }

        async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
            true
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
        ) -> crate::internal::ai::tools::ToolResult<ToolOutput> {
            Ok(ToolOutput::success("mutated"))
        }

        fn schema(&self) -> ToolSpec {
            ToolSpec::new("mock_tool", "mutating mock tool")
        }
    }

    /// The mutation marker must describe only the dispatch boundary. Leaving
    /// it set after the handler returns would make a following model/read-only
    /// phase permanently reject safe cooperative cancellation.
    #[tokio::test]
    async fn tool_loop_clears_mutation_marker_after_determinate_handler_result() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(MutatingMockHandler));
        let token = CancellationToken::new();
        let mutation_started = Arc::new(AtomicBool::new(false));
        let config = ToolLoopConfig {
            cancellation: Some(ToolLoopCancellation::new(
                token,
                Arc::clone(&mutation_started),
            )),
            ..ToolLoopConfig::default()
        };

        let turn = run_tool_loop_with_history_and_observer(
            &MockModel,
            Vec::new(),
            "run mutation",
            &registry,
            config,
            &mut NoopObserver,
        )
        .await
        .expect("the mutating mock handler should reach a determinate result");

        assert_eq!(turn.final_text, "done");
        assert!(
            !mutation_started.load(Ordering::Acquire),
            "the marker must be cleared after the mutation handler returns",
        );
    }

    struct ClassificationBlockingMutationHandler {
        classification_started: Arc<tokio::sync::Notify>,
        allow_classification: Arc<tokio::sync::Notify>,
        dispatches: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ToolHandler for ClassificationBlockingMutationHandler {
        fn kind(&self) -> ToolKind {
            ToolKind::Function
        }

        async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
            self.classification_started.notify_one();
            self.allow_classification.notified().await;
            true
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
        ) -> crate::internal::ai::tools::ToolResult<ToolOutput> {
            self.dispatches.fetch_add(1, Ordering::AcqRel);
            Ok(ToolOutput::success("must not dispatch after cancellation"))
        }

        fn schema(&self) -> ToolSpec {
            ToolSpec::new("mock_tool", "classification-blocking mutating mock tool")
        }
    }

    /// Cancellation that lands after mutability classification begins but
    /// before dispatch must win without executing the mutation. The loop first
    /// claims the boundary, then rechecks the token, closing the former race
    /// between a pre-dispatch check and marker publication.
    #[tokio::test]
    async fn cancellation_between_mutation_classification_and_dispatch_skips_tool() {
        let temp_dir = TempDir::new().unwrap();
        let classification_started = Arc::new(tokio::sync::Notify::new());
        let allow_classification = Arc::new(tokio::sync::Notify::new());
        let dispatches = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register(
            "mock_tool",
            Arc::new(ClassificationBlockingMutationHandler {
                classification_started: Arc::clone(&classification_started),
                allow_classification: Arc::clone(&allow_classification),
                dispatches: Arc::clone(&dispatches),
            }),
        );
        let registry = Arc::new(registry);
        let token = CancellationToken::new();
        let mutation_started = Arc::new(AtomicBool::new(false));
        let config = ToolLoopConfig {
            cancellation: Some(ToolLoopCancellation::new(
                token.clone(),
                Arc::clone(&mutation_started),
            )),
            ..ToolLoopConfig::default()
        };

        let task_registry = Arc::clone(&registry);
        let task = tokio::spawn(async move {
            let mut observer = NoopObserver;
            run_tool_loop_with_history_and_observer(
                &MockModel,
                Vec::new(),
                "run mutation",
                task_registry.as_ref(),
                config,
                &mut observer,
            )
            .await
        });
        classification_started.notified().await;
        token.cancel();
        allow_classification.notify_one();

        let error = task
            .await
            .expect("tool loop task must not panic")
            .expect_err("cancellation before dispatch must stop the loop");
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(
            dispatches.load(Ordering::Acquire),
            0,
            "the handler must not run after cancellation wins before dispatch",
        );
        assert!(
            !mutation_started.load(Ordering::Acquire),
            "a cancellation before dispatch must release the mutation marker",
        );
    }

    #[derive(Clone)]
    struct LoopFakeSource {
        manifest: CapabilityManifest,
    }

    #[async_trait]
    impl Source for LoopFakeSource {
        fn manifest(&self) -> &CapabilityManifest {
            &self.manifest
        }

        async fn call_tool(
            &self,
            context: SourceCallContext,
            _invocation: ToolInvocation,
        ) -> crate::internal::ai::tools::ToolResult<ToolOutput> {
            Ok(ToolOutput::success(format!(
                "{}:{}",
                context.source_slug, context.tool_name
            )))
        }
    }

    fn source_manifest(tool_names: &[&str]) -> CapabilityManifest {
        tool_names.iter().fold(
            CapabilityManifest::new("project_docs", SourceKind::LocalDocs, TrustTier::Project),
            |manifest, name| {
                manifest.with_tool(SourceToolCapability::new(
                    *name,
                    ToolSpec::new(*name, "source tool"),
                ))
            },
        )
    }

    struct LongOutputHandler;

    #[async_trait]
    impl ToolHandler for LongOutputHandler {
        fn kind(&self) -> ToolKind {
            ToolKind::Function
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
        ) -> crate::internal::ai::tools::ToolResult<ToolOutput> {
            Ok(ToolOutput::success(long_tool_context_output()))
        }

        fn schema(&self) -> ToolSpec {
            ToolSpec::new("mock_tool", "mock tool")
        }
    }

    #[derive(Default)]
    struct RecordingObserver {
        begins: Vec<(String, String)>,
        ends: Vec<(String, String, bool)>,
        result_texts: Vec<String>,
        stream_events: Vec<CompletionStreamEvent>,
        sub_agent_completions: Vec<(String, CompletionUsageSummary)>,
    }

    impl ToolLoopObserver for RecordingObserver {
        fn on_model_stream_event(&mut self, event: &CompletionStreamEvent) {
            self.stream_events.push(event.clone());
        }

        fn on_tool_call_begin(&mut self, call_id: &str, tool_name: &str, _arguments: &Value) {
            self.begins
                .push((call_id.to_string(), tool_name.to_string()));
        }

        fn on_tool_call_end(
            &mut self,
            call_id: &str,
            tool_name: &str,
            result: &Result<ToolOutput, String>,
        ) {
            self.result_texts.push(match result {
                Ok(output) => output.as_text().unwrap_or("").to_string(),
                Err(message) => message.clone(),
            });
            self.ends.push((
                call_id.to_string(),
                tool_name.to_string(),
                result.as_ref().is_ok_and(|o| o.is_success()),
            ));
        }

        fn on_sub_agent_completed(&mut self, agent_name: &str, usage: &CompletionUsageSummary) {
            self.sub_agent_completions
                .push((agent_name.to_string(), usage.clone()));
        }
    }

    #[test]
    fn completion_error_kind_classifies_cancel_and_timeout() {
        assert_eq!(
            completion_error_kind(&CompletionError::ProviderError(
                "mock timeout after 100ms".to_string()
            )),
            "timeout"
        );
        assert_eq!(
            completion_error_kind(&CompletionError::ResponseError(
                "request cancelled by user".to_string()
            )),
            "cancelled"
        );
        assert_eq!(
            completion_error_kind(&CompletionError::ProviderError(
                "provider overloaded".to_string()
            )),
            "provider_error"
        );
    }

    /// Scenario: a model that emits text and thinking deltas via the stream channel
    /// has every event delivered to the observer before the loop returns.
    #[tokio::test]
    async fn tool_loop_forwards_completion_stream_events() {
        #[derive(Clone)]
        struct StreamingModel;

        impl CompletionModel for StreamingModel {
            type Response = ();

            async fn completion(
                &self,
                request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                if let Some(stream_events) = request.stream_events {
                    stream_events
                        .send(CompletionStreamEvent::TextDelta {
                            request_id: Some("req_1".to_string()),
                            delta: "hel".to_string(),
                        })
                        .expect("stream receiver should be open");
                    stream_events
                        .send(CompletionStreamEvent::ThinkingDelta {
                            request_id: Some("req_1".to_string()),
                            delta: "checking".to_string(),
                        })
                        .expect("stream receiver should be open");
                    stream_events
                        .send(CompletionStreamEvent::TextDelta {
                            request_id: Some("req_1".to_string()),
                            delta: "lo".to_string(),
                        })
                        .expect("stream receiver should be open");
                }

                Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text {
                        text: "hello".to_string(),
                    })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }

        let temp_dir = TempDir::new().unwrap();
        let registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        let mut observer = RecordingObserver::default();

        let turn = run_tool_loop_with_history_and_observer(
            &StreamingModel,
            Vec::new(),
            "hello",
            &registry,
            ToolLoopConfig::default(),
            &mut observer,
        )
        .await
        .unwrap();

        assert_eq!(turn.final_text, "hello");
        assert_eq!(observer.stream_events.len(), 3);
        assert!(observer.stream_events.iter().any(|event| matches!(
            event,
            CompletionStreamEvent::ThinkingDelta { delta, .. } if delta == "checking"
        )));
        let streamed = observer
            .stream_events
            .iter()
            .filter_map(|event| match event {
                CompletionStreamEvent::TextDelta { delta, .. } => Some(delta.as_str()),
                CompletionStreamEvent::ThinkingDelta { .. } => None,
                CompletionStreamEvent::ToolCallPreview { .. } => None,
            })
            .collect::<String>();
        assert_eq!(streamed, "hello");
    }

    /// Scenario: a tool-call preview event surfaces in the observer's stream history
    /// but does not trigger tool execution — previews are pure UI hints.
    #[tokio::test]
    async fn tool_loop_forwards_tool_call_preview_without_execution() {
        #[derive(Clone)]
        struct ToolPreviewModel;

        impl CompletionModel for ToolPreviewModel {
            type Response = ();

            async fn completion(
                &self,
                request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                if let Some(stream_events) = request.stream_events {
                    stream_events
                        .send(CompletionStreamEvent::ToolCallPreview {
                            request_id: Some("req_1".to_string()),
                            call_id: "call_preview".to_string(),
                            tool_name: "mock_tool".to_string(),
                            arguments: json!({"value": 1}),
                        })
                        .expect("stream receiver should be open");
                }

                Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text {
                        text: "done".to_string(),
                    })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }

        let temp_dir = TempDir::new().unwrap();
        let registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        let mut observer = RecordingObserver::default();

        let turn = run_tool_loop_with_history_and_observer(
            &ToolPreviewModel,
            Vec::new(),
            "hello",
            &registry,
            ToolLoopConfig::default(),
            &mut observer,
        )
        .await
        .unwrap();

        assert_eq!(turn.final_text, "done");
        assert!(observer.begins.is_empty());
        assert_eq!(observer.stream_events.len(), 1);
        match &observer.stream_events[0] {
            CompletionStreamEvent::ToolCallPreview {
                call_id,
                tool_name,
                arguments,
                ..
            } => {
                assert_eq!(call_id, "call_preview");
                assert_eq!(tool_name, "mock_tool");
                assert_eq!(arguments, &json!({"value": 1}));
            }
            other => panic!("expected tool preview event, got {other:?}"),
        }
    }

    /// Scenario: a single tool call followed by a text response yields the canonical
    /// four-message history (user, assistant tool-call, user tool-result, assistant
    /// text) and emits the matching begin/end events.
    #[tokio::test]
    async fn tool_loop_emits_tool_events_and_updates_history() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(MockHandler));

        let mut observer = RecordingObserver::default();
        let turn = run_tool_loop_with_history_and_observer(
            &MockModel,
            Vec::new(),
            "hello",
            &registry,
            ToolLoopConfig {
                preamble: None,
                temperature: Some(0.0),
                thinking: None,
                reasoning_effort: None,
                stream: None,
                hook_runner: None,
                allowed_tools: None,
                runtime_context: None,
                cancellation: None,
                max_turns: None,
                repeat_detection_window: Some(DEFAULT_REPEAT_DETECTION_WINDOW),
                repeat_warning_threshold: Some(DEFAULT_REPEAT_WARNING_THRESHOLD),
                repeat_abort_threshold: Some(DEFAULT_REPEAT_ABORT_THRESHOLD),
                terminal_tools: None,
                subagent_runtime: None,
                context_frame_session_root: None,
                context_frame_prompt_id: None,
                context_frame_budget: None,
                context_frame_attachment_threshold_bytes: None,
                usage_recorder: None,
                usage_context: None,
                source_pool: None,
                source_session_id: None,
                preserve_reasoning_content: false,
                goal_stop_policy: None,
            },
            &mut observer,
        )
        .await
        .unwrap();

        assert_eq!(turn.final_text, "done");
        assert_eq!(
            observer.begins,
            vec![("call_1".to_string(), "mock_tool".to_string())]
        );
        assert_eq!(
            observer.ends,
            vec![("call_1".to_string(), "mock_tool".to_string(), true)]
        );

        // User(prompt) + Assistant(toolcall) + User(toolresult) + Assistant(text)
        assert_eq!(turn.history.len(), 4);
        assert!(matches!(&turn.history[0], Message::User { .. }));
        assert!(matches!(&turn.history[1], Message::Assistant { .. }));
        assert!(matches!(&turn.history[2], Message::User { .. }));
        assert!(matches!(&turn.history[3], Message::Assistant { .. }));
    }

    /// Scenario: the `task` tool is not a normal registry handler.
    /// When the model emits `task(...)` and the tool-loop config
    /// carries a sub-agent runtime, the call must route through
    /// `SubAgentDispatcher`, feed the returned `<task_result>` back
    /// to the parent model, and preserve the normal observer
    /// begin/end lifecycle.
    #[tokio::test]
    async fn tool_loop_routes_task_tool_to_subagent_dispatcher() {
        use futures::future::BoxFuture;
        use sea_orm::Database;

        use crate::internal::ai::{
            agent::runtime::{
                AbortToken, ContextFrameLoader, DispatchContext, PermissionAskRequest,
                PermissionAsker, PermissionReply, PermissionService, SubAgentDispatcher,
                SubAgentToolLoopRuntime, TaskEntryKind, TaskFailure, TaskInvocation, TaskResult,
            },
            sandbox::FileHistoryRuntimeContext,
        };

        #[derive(Clone)]
        struct TaskCallingModel {
            seen_task_results: Arc<Mutex<Vec<String>>>,
        }

        impl CompletionModel for TaskCallingModel {
            type Response = ();

            async fn completion(
                &self,
                request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                let task_result = request.chat_history.iter().find_map(|msg| match msg {
                    Message::User { content } => content.iter().find_map(|item| match item {
                        UserContent::ToolResult(result) if result.name == "task" => {
                            Some(result.result.to_string())
                        }
                        _ => None,
                    }),
                    _ => None,
                });

                if let Some(result) = task_result {
                    self.seen_task_results.lock().unwrap().push(result);
                    return Ok(CompletionResponse {
                        content: vec![AssistantContent::Text(Text {
                            text: "parent saw task result".to_string(),
                        })],
                        reasoning_content: None,
                        raw_response: (),
                    });
                }

                assert!(
                    request.tools.iter().any(|tool| tool.name == "task"),
                    "sub-agent runtime must expose task schema to the model"
                );
                Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_task_1".to_string(),
                        name: "task".to_string(),
                        function: Function {
                            name: "task".to_string(),
                            arguments: json!({
                                "description": "find TODOs",
                                "prompt": "grep TODO src/",
                                "subagent_type": "explore"
                            }),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }

        #[derive(Default)]
        struct RecordingDispatcher {
            calls: Mutex<Vec<TaskInvocation>>,
            // Capture the `runtime_context` each dispatch received so
            // the test can prove the parent loop's *live* per-turn
            // context (not the runtime's session-start snapshot) is
            // threaded into the child `DispatchContext` (S2-INV-06).
            captured_runtime_context: Mutex<Option<Option<ToolRuntimeContext>>>,
        }

        impl SubAgentDispatcher for RecordingDispatcher {
            fn dispatch<'a>(
                &'a self,
                ctx: DispatchContext<'a>,
                invocation: TaskInvocation,
                entry_kind: TaskEntryKind,
            ) -> BoxFuture<'a, Result<TaskResult, TaskFailure>> {
                Box::pin(async move {
                    assert_eq!(entry_kind, TaskEntryKind::LlmInitiated);
                    assert_eq!(ctx.parent_thread_id, "thread-task");
                    assert_eq!(ctx.depth, 0);
                    *self.captured_runtime_context.lock().unwrap() =
                        Some(ctx.runtime_context.clone());
                    self.calls.lock().unwrap().push(invocation.clone());
                    Ok(TaskResult {
                        task_id: "task-123".to_string(),
                        agent_name: invocation.subagent_type,
                        provider_id: "fake".to_string(),
                        model_id: "default".to_string(),
                        final_text: "Found 3 TODOs in 2 files.".to_string(),
                        steps_used: 2,
                        // Stamp a recognisable non-default usage so
                        // the observer's `on_sub_agent_completed`
                        // assertion below can distinguish a forwarded
                        // payload from a default-clone.
                        usage: CompletionUsageSummary {
                            input_tokens: 100,
                            output_tokens: 42,
                            ..CompletionUsageSummary::default()
                        },
                    })
                })
            }
        }

        struct AllowAsker;
        impl PermissionAsker for AllowAsker {
            fn ask<'a>(
                &'a self,
                _request: PermissionAskRequest<'a>,
            ) -> BoxFuture<'a, PermissionReply> {
                Box::pin(async move { PermissionReply::Once })
            }
        }

        let temp_dir = TempDir::new().unwrap();
        let registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        let conn = Database::connect("sqlite::memory:").await.unwrap();
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let seen_task_results = Arc::new(Mutex::new(Vec::new()));
        let model = TaskCallingModel {
            seen_task_results: Arc::clone(&seen_task_results),
        };

        let runtime = SubAgentToolLoopRuntime {
            dispatcher: dispatcher.clone(),
            parent_thread_id: "thread-task".to_string(),
            parent_session_id: "session-task".to_string(),
            parent_agent: AgentExecutionSpec {
                name: "build".to_string(),
                mode: AgentMode::Primary,
                model: Some(ModelBinding {
                    provider_id: "fake".to_string(),
                    model_id: "default".to_string(),
                    variant: None,
                }),
                ..Default::default()
            },
            parent_ruleset: PermissionRuleset::default(),
            parent_model_binding: ModelBinding {
                provider_id: "fake".to_string(),
                model_id: "default".to_string(),
                variant: None,
            },
            permission_service: Arc::new(PermissionService::with_asker(AllowAsker)),
            session_store: SessionJsonlStore::new(temp_dir.path().join("session-jsonl")),
            provider_factory: Arc::new(ProviderFactory),
            provider_build_options: ProviderBuildOptions::default(),
            provider_build_options_resolver: None,
            tool_registry: registry.clone(),
            runtime_context: None,
            usage_recorder: Arc::new(UsageRecorder::new(conn)),
            context_frame_loader: Arc::new(ContextFrameLoader::default()),
            abort_token: AbortToken::new(),
            depth: 0,
            compaction_model: None,
            hook_runner: None,
        };

        // The parent loop's LIVE per-turn context: the runtime above
        // stored `runtime_context: None` (the session-start snapshot),
        // so anything observed on the dispatched child can only have
        // come from this `config.runtime_context`, proving the live
        // context is threaded through (S2-INV-06). `file_history`
        // mirrors what the TUI attaches per turn (the batch that drives
        // child `apply_patch` undo preimage recording).
        let live_runtime_context = ToolRuntimeContext {
            file_history: Some(FileHistoryRuntimeContext {
                session_root: temp_dir.path().join("session-root"),
                batch_id: "turn-7".to_string(),
            }),
            max_output_bytes: Some(0x00C0_FFEE),
            ..ToolRuntimeContext::default()
        };

        let mut observer = RecordingObserver::default();
        let turn = run_tool_loop_with_history_and_observer(
            &model,
            Vec::new(),
            "Find all TODOs",
            &registry,
            ToolLoopConfig {
                allowed_tools: Some(vec!["read_file".to_string()]),
                subagent_runtime: Some(runtime),
                runtime_context: Some(live_runtime_context),
                ..Default::default()
            },
            &mut observer,
        )
        .await
        .unwrap();

        assert_eq!(turn.final_text, "parent saw task result");
        assert_eq!(
            dispatcher.calls.lock().unwrap().as_slice(),
            &[TaskInvocation {
                description: "find TODOs".to_string(),
                prompt: "grep TODO src/".to_string(),
                subagent_type: "explore".to_string(),
                task_id: None,
            }]
        );

        // S2-INV-06: the dispatched child's `DispatchContext` must carry
        // the parent loop's LIVE `config.runtime_context`, not the
        // runtime's `None` session-start snapshot. A regression that
        // drops the threading (reverting `dispatch_context`'s
        // `live_runtime_context` arg or the tool-loop call site) makes
        // this observe `None` / a defaulted context.
        let captured = dispatcher
            .captured_runtime_context
            .lock()
            .unwrap()
            .clone()
            .expect("the dispatcher must have been invoked for the task call");
        let captured = captured.expect(
            "the child DispatchContext must inherit the parent's live runtime_context, \
             not the runtime's None snapshot",
        );
        assert_eq!(
            captured.max_output_bytes,
            Some(0x00C0_FFEE),
            "child must inherit the live per-turn output-budget cap",
        );
        let file_history = captured.file_history.as_ref().expect(
            "child must inherit the live per-turn file-history batch so its \
             apply_patch calls record undo preimages",
        );
        assert_eq!(
            file_history.batch_id, "turn-7",
            "child must inherit the live per-turn file-history batch verbatim",
        );
        let seen = seen_task_results.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].contains("task_id: task-123"));
        assert!(seen[0].contains("<task_result>"));
        assert!(seen[0].contains("Found 3 TODOs in 2 files."));
        assert_eq!(
            observer.begins,
            vec![("call_task_1".to_string(), "task".to_string())]
        );
        assert_eq!(
            observer.ends,
            vec![("call_task_1".to_string(), "task".to_string(), true)]
        );
        // OC-Phase 5 per-agent attribution: the new
        // `on_sub_agent_completed` hook (v0.17.768) must fire with
        // the sub-agent's resolved spec name and the
        // dispatcher-returned usage envelope verbatim.
        assert_eq!(
            observer.sub_agent_completions.len(),
            1,
            "task tool success must fire exactly one on_sub_agent_completed callback"
        );
        let (sub_name, sub_usage) = &observer.sub_agent_completions[0];
        assert_eq!(sub_name, "explore");
        assert_eq!(sub_usage.input_tokens, 100);
        assert_eq!(sub_usage.output_tokens, 42);
    }

    #[test]
    fn parse_task_invocation_rejects_unknown_fields_and_trims_known_values() {
        let parsed = parse_task_invocation(&json!({
            "description": " find TODOs ",
            "prompt": " grep TODO src/ ",
            "subagent_type": " explore ",
            "task_id": " explicit "
        }))
        .expect("valid task invocation parses");

        assert_eq!(parsed.description, "find TODOs");
        assert_eq!(parsed.prompt, "grep TODO src/");
        assert_eq!(parsed.subagent_type, "explore");
        assert_eq!(parsed.task_id.as_deref(), Some("explicit"));

        let err = parse_task_invocation(&json!({
            "description": "find TODOs",
            "prompt": "grep TODO src/",
            "subagent_type": "explore",
            "unexpected": true
        }))
        .expect_err("unknown task fields must be rejected")
        .to_string();
        assert!(
            err.contains("unexpected") || err.contains("unknown field"),
            "error should mention the unknown field, got: {err}"
        );
    }

    /// Scenario: every provider request is mirrored as an append-only context frame,
    /// and large tool results are moved to session attachments before replay.
    #[tokio::test]
    async fn tool_loop_records_context_frames_and_attachments() {
        let temp_dir = TempDir::new().unwrap();
        let session_root = temp_dir.path().join("session-1");
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(LongOutputHandler));

        let turn = run_tool_loop_with_history_and_observer(
            &MockModel,
            Vec::new(),
            "hello",
            &registry,
            ToolLoopConfig {
                preamble: Some("system rules\nprotected branch=main".to_string()),
                context_frame_session_root: Some(session_root.clone()),
                context_frame_prompt_id: Some("turn-42".to_string()),
                context_frame_attachment_threshold_bytes: Some(64),
                ..Default::default()
            },
            &mut RecordingObserver::default(),
        )
        .await
        .unwrap();

        assert_eq!(turn.final_text, "done");
        let replay = SessionJsonlStore::new(session_root.clone())
            .load_context_replay()
            .unwrap();
        assert_eq!(replay.frames.len(), 2);
        assert_eq!(
            replay.frames[0].prompt_id.as_deref(),
            Some("turn-42/model-turn-1")
        );
        assert_eq!(
            replay.frames[1].prompt_id.as_deref(),
            Some("turn-42/model-turn-2")
        );
        assert!(replay.frames[0].segments.iter().any(|segment| {
            segment.id == "preamble"
                && segment.segment == ContextSegmentKind::SystemRules
                && segment.non_compressible
        }));

        let tool_segment = replay.frames[1]
            .segments
            .iter()
            .find(|segment| segment.segment == ContextSegmentKind::ToolResults)
            .unwrap();
        let attachment = tool_segment.attachment.as_ref().unwrap();
        let attachments = ContextAttachmentStore::new(&session_root);
        assert_eq!(
            attachments.read_to_string(attachment).unwrap(),
            render_tool_result_context(&ToolResult {
                id: "call_1".to_string(),
                name: "mock_tool".to_string(),
                result: ToolOutput::success(long_tool_context_output()).into_response(),
            })
        );
        assert_eq!(replay.compactions.len(), 1);
        assert!(
            replay.compactions[0]
                .protected_segment_ids
                .iter()
                .any(|id| id == "preamble")
        );
        let events = std::fs::read_to_string(session_root.join("events.jsonl")).unwrap();
        assert!(!events.contains("tool output line 7"));
    }

    /// Scenario: when `preserve_reasoning_content` is `false` (default), assistant
    /// reasoning is dropped before being re-sent to the model on the next turn.
    #[tokio::test]
    async fn tool_loop_clears_assistant_reasoning_content_by_default() {
        #[derive(Clone)]
        struct ReasoningToolModel {
            seen_followup_reasoning: Arc<Mutex<Vec<Option<String>>>>,
        }

        impl CompletionModel for ReasoningToolModel {
            type Response = ();

            async fn completion(
                &self,
                request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                let has_tool_result = request.chat_history.iter().any(|msg| match msg {
                    Message::User { content } => content
                        .iter()
                        .any(|item| matches!(item, UserContent::ToolResult(_))),
                    _ => false,
                });

                if has_tool_result {
                    let reasoning = request.chat_history.iter().find_map(|msg| match msg {
                        Message::Assistant {
                            reasoning_content, ..
                        } => reasoning_content.clone(),
                        _ => None,
                    });
                    self.seen_followup_reasoning.lock().unwrap().push(reasoning);

                    return Ok(CompletionResponse {
                        content: vec![AssistantContent::Text(Text {
                            text: "done".to_string(),
                        })],
                        reasoning_content: Some("final reasoning".to_string()),
                        raw_response: (),
                    });
                }

                Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_1".to_string(),
                        name: "mock_tool".to_string(),
                        function: Function {
                            name: "mock_tool".to_string(),
                            arguments: json!({"value": 1}),
                        },
                    })],
                    reasoning_content: Some("need tool result".to_string()),
                    raw_response: (),
                })
            }
        }

        let seen_followup_reasoning = Arc::new(Mutex::new(Vec::new()));
        let model = ReasoningToolModel {
            seen_followup_reasoning: Arc::clone(&seen_followup_reasoning),
        };
        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(MockHandler));

        let turn = run_tool_loop_with_history_and_observer(
            &model,
            Vec::new(),
            "hello",
            &registry,
            ToolLoopConfig::default(),
            &mut RecordingObserver::default(),
        )
        .await
        .unwrap();

        assert_eq!(turn.final_text, "done");
        assert_eq!(seen_followup_reasoning.lock().unwrap().as_slice(), &[None]);
        assert!(matches!(
            &turn.history[1],
            Message::Assistant {
                reasoning_content: None,
                ..
            }
        ));
        assert!(matches!(
            &turn.history[3],
            Message::Assistant {
                reasoning_content: None,
                ..
            }
        ));
    }

    /// Scenario: when `preserve_reasoning_content` is `true`, the model's chain of
    /// thought is replayed in the next request — used by long-horizon plan executors.
    #[tokio::test]
    async fn tool_loop_preserves_assistant_reasoning_content_when_configured() {
        #[derive(Clone)]
        struct ReasoningToolModel {
            seen_followup_reasoning: Arc<Mutex<Vec<Option<String>>>>,
        }

        impl CompletionModel for ReasoningToolModel {
            type Response = ();

            async fn completion(
                &self,
                request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                let has_tool_result = request.chat_history.iter().any(|msg| match msg {
                    Message::User { content } => content
                        .iter()
                        .any(|item| matches!(item, UserContent::ToolResult(_))),
                    _ => false,
                });

                if has_tool_result {
                    let reasoning = request.chat_history.iter().find_map(|msg| match msg {
                        Message::Assistant {
                            reasoning_content, ..
                        } => reasoning_content.clone(),
                        _ => None,
                    });
                    self.seen_followup_reasoning.lock().unwrap().push(reasoning);

                    return Ok(CompletionResponse {
                        content: vec![AssistantContent::Text(Text {
                            text: "done".to_string(),
                        })],
                        reasoning_content: Some("final reasoning".to_string()),
                        raw_response: (),
                    });
                }

                Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_1".to_string(),
                        name: "mock_tool".to_string(),
                        function: Function {
                            name: "mock_tool".to_string(),
                            arguments: json!({"value": 1}),
                        },
                    })],
                    reasoning_content: Some("need tool result".to_string()),
                    raw_response: (),
                })
            }
        }

        let seen_followup_reasoning = Arc::new(Mutex::new(Vec::new()));
        let model = ReasoningToolModel {
            seen_followup_reasoning: Arc::clone(&seen_followup_reasoning),
        };
        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(MockHandler));

        let turn = run_tool_loop_with_history_and_observer(
            &model,
            Vec::new(),
            "hello",
            &registry,
            ToolLoopConfig {
                preserve_reasoning_content: true,
                ..Default::default()
            },
            &mut RecordingObserver::default(),
        )
        .await
        .unwrap();

        assert_eq!(turn.final_text, "done");
        assert_eq!(
            seen_followup_reasoning.lock().unwrap().as_slice(),
            &[Some("need tool result".to_string())]
        );
        assert!(matches!(
            &turn.history[1],
            Message::Assistant {
                reasoning_content: Some(reasoning),
                ..
            } if reasoning == "need tool result"
        ));
        assert!(matches!(
            &turn.history[3],
            Message::Assistant {
                reasoning_content: Some(reasoning),
                ..
            } if reasoning == "final reasoning"
        ));
    }

    /// Scenario: a `PreToolUse` hook returns `Block`. The tool is never dispatched,
    /// the model receives the failure as a tool result, and the loop continues.
    #[tokio::test]
    async fn tool_loop_hook_blocks_tool_call() {
        use crate::internal::ai::hooks::{
            HookRunner,
            config::{HookConfig, HookDefinition},
            event::HookEvent,
        };

        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(MockHandler));

        // Create a HookRunner with a PreToolUse hook that blocks mock_tool
        let hook_runner = HookRunner::new(
            HookConfig {
                hooks: vec![HookDefinition {
                    event: HookEvent::PreToolUse,
                    matcher: "mock_tool".to_string(),
                    command:
                        r#"exec 0<&-; sleep 0.05; echo "{\"message\":\"tool blocked by test hook\"}"; exit 129"#
                            .to_string(),
                    description: "test blocker".to_string(),
                    timeout_ms: 5000,
                    enabled: true,
                }],
            },
            temp_dir.path().to_path_buf(),
        );

        let mut observer = RecordingObserver::default();
        let turn = run_tool_loop_with_history_and_observer(
            &MockModel,
            Vec::new(),
            "hello",
            &registry,
            ToolLoopConfig {
                hook_runner: Some(Arc::new(hook_runner)),
                ..Default::default()
            },
            &mut observer,
        )
        .await
        .unwrap();

        // The tool call should have been issued (begin) but blocked (end with failure)
        assert_eq!(observer.begins.len(), 1);
        assert_eq!(observer.begins[0].1, "mock_tool");
        assert_eq!(observer.ends.len(), 1);
        // The end should show failure (blocked)
        assert!(
            !observer.ends[0].2,
            "blocked tool call should report as not successful"
        );

        // Model should still produce final text after seeing the block result
        assert_eq!(turn.final_text, "done");

        // History: User(prompt) + Assistant(toolcall) + User(blocked result) + Assistant(text)
        assert_eq!(turn.history.len(), 4);
    }

    /// Scenario: a tool that returns an error has the error text fed back to the
    /// model so it can recover, rather than aborting the whole loop.
    #[tokio::test]
    async fn tool_loop_tool_error_is_reported_to_model() {
        /// A handler that always fails.
        struct FailingHandler;

        #[async_trait]
        impl ToolHandler for FailingHandler {
            fn kind(&self) -> ToolKind {
                ToolKind::Function
            }

            async fn handle(
                &self,
                _invocation: ToolInvocation,
            ) -> crate::internal::ai::tools::ToolResult<ToolOutput> {
                Err(crate::internal::ai::tools::ToolError::ExecutionFailed(
                    "something went wrong".to_string(),
                ))
            }

            fn schema(&self) -> ToolSpec {
                ToolSpec::new("failing_tool", "a tool that always fails")
            }
        }

        /// A model that calls failing_tool on first turn, then returns text.
        #[derive(Clone)]
        struct FailToolModel;

        impl CompletionModel for FailToolModel {
            type Response = ();

            async fn completion(
                &self,
                request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                let has_tool_result = request.chat_history.iter().any(|msg| match msg {
                    Message::User { content } => content
                        .iter()
                        .any(|c| matches!(c, UserContent::ToolResult(_))),
                    _ => false,
                });

                if !has_tool_result {
                    return Ok(CompletionResponse {
                        content: vec![AssistantContent::ToolCall(ToolCall {
                            id: "call_fail".to_string(),
                            name: "failing_tool".to_string(),
                            function: Function {
                                name: "failing_tool".to_string(),
                                arguments: json!({}),
                            },
                        })],
                        reasoning_content: None,
                        raw_response: (),
                    });
                }

                Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text {
                        text: "handled error".to_string(),
                    })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }

        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("failing_tool", Arc::new(FailingHandler));

        let mut observer = RecordingObserver::default();
        let turn = run_tool_loop_with_history_and_observer(
            &FailToolModel,
            Vec::new(),
            "try the tool",
            &registry,
            ToolLoopConfig {
                preamble: None,
                temperature: Some(0.0),
                thinking: None,
                reasoning_effort: None,
                stream: None,
                hook_runner: None,
                allowed_tools: None,
                runtime_context: None,
                cancellation: None,
                max_turns: None,
                repeat_detection_window: Some(DEFAULT_REPEAT_DETECTION_WINDOW),
                repeat_warning_threshold: Some(DEFAULT_REPEAT_WARNING_THRESHOLD),
                repeat_abort_threshold: Some(DEFAULT_REPEAT_ABORT_THRESHOLD),
                terminal_tools: None,
                subagent_runtime: None,
                context_frame_session_root: None,
                context_frame_prompt_id: None,
                context_frame_budget: None,
                context_frame_attachment_threshold_bytes: None,
                usage_recorder: None,
                usage_context: None,
                source_pool: None,
                source_session_id: None,
                preserve_reasoning_content: false,
                goal_stop_policy: None,
            },
            &mut observer,
        )
        .await
        .unwrap();

        // Tool call should have been attempted and failed
        assert_eq!(observer.begins.len(), 1);
        assert_eq!(observer.ends.len(), 1);
        assert!(
            !observer.ends[0].2,
            "failed tool should report as not successful"
        );

        // Model should still get the error and produce final text
        assert_eq!(turn.final_text, "handled error");
    }

    /// Scenario: `allowed_tools` removes filtered tools from the definition list sent
    /// to the model so they never appear as available choices.
    #[tokio::test]
    async fn tool_loop_allowed_tools_filters_definitions() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(MockHandler));

        // With allowed_tools that doesn't include "mock_tool",
        // the model shouldn't see the tool and should return text directly.
        // But our MockModel always issues a tool call first, so the tool call will
        // fail because it's not in the registry's dispatch (it IS in the registry,
        // but allowed_tools only affects the definitions sent to the model).
        // For a proper test, we verify registry_tool_definitions filtering:
        let all_tools = registry_tool_definitions(&registry);
        assert_eq!(all_tools.len(), 1);
        assert_eq!(all_tools[0].name, "mock_tool");

        // Filter with allowed_tools
        let mut filtered = registry_tool_definitions(&registry);
        let allowed = ["nonexistent_tool"];
        filtered.retain(|t| allowed.contains(&t.name.as_str()));
        assert!(filtered.is_empty());

        // Filter with allowed_tools that includes mock_tool
        let mut filtered = registry_tool_definitions(&registry);
        let allowed = ["mock_tool"];
        filtered.retain(|t| allowed.contains(&t.name.as_str()));
        assert_eq!(filtered.len(), 1);
    }

    /// Scenario: Source Pool reloads must take effect on the next tool-loop
    /// setup, not require a TUI restart. The loop builds an effective registry
    /// from the current SourcePool snapshot at run start, so a reloaded manifest
    /// changes the next request's tool definitions.
    #[tokio::test]
    async fn source_pool_reload_updates_next_tool_loop_registry() {
        let temp_dir = TempDir::new().unwrap();
        let registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        let pool = SourcePool::new();
        pool.register_source(Arc::new(LoopFakeSource {
            manifest: source_manifest(&["lookup"]),
        }))
        .expect("register initial source");

        let config = ToolLoopConfig {
            source_pool: Some(pool.clone()),
            source_session_id: Some("session-a".to_string()),
            ..Default::default()
        };
        let effective = registry_with_source_tools(&registry, &config)
            .expect("source handlers should merge into registry");
        let tool_names = registry_tool_definitions(&effective)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert_eq!(tool_names, vec!["project_docs__lookup"]);

        pool.reload_source(Arc::new(LoopFakeSource {
            manifest: source_manifest(&["lookup", "search"]),
        }))
        .expect("reload source manifest");

        let effective = registry_with_source_tools(&registry, &config)
            .expect("reloaded source handlers should merge into registry");
        let mut tool_names = registry_tool_definitions(&effective)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        tool_names.sort();
        assert_eq!(
            tool_names,
            vec!["project_docs__lookup", "project_docs__search"]
        );
    }

    /// Scenario: even if a model hallucinates a tool name outside `allowed_tools`,
    /// execution is rejected and the model receives a structured failure message.
    #[tokio::test]
    async fn tool_loop_allowed_tools_blocks_execution() {
        // MockModel always calls "mock_tool" on first turn.
        // allowed_tools = ["other_tool"] should block "mock_tool" at execution time,
        // returning an error to the model which then produces "done".
        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(MockHandler));

        let mut observer = RecordingObserver::default();
        let turn = run_tool_loop_with_history_and_observer(
            &MockModel,
            Vec::new(),
            "hello",
            &registry,
            ToolLoopConfig {
                preamble: None,
                temperature: Some(0.0),
                thinking: None,
                reasoning_effort: None,
                stream: None,
                hook_runner: None,
                allowed_tools: Some(vec!["other_tool".to_string()]),
                runtime_context: None,
                cancellation: None,
                max_turns: None,
                repeat_detection_window: Some(DEFAULT_REPEAT_DETECTION_WINDOW),
                repeat_warning_threshold: Some(DEFAULT_REPEAT_WARNING_THRESHOLD),
                repeat_abort_threshold: Some(DEFAULT_REPEAT_ABORT_THRESHOLD),
                terminal_tools: None,
                subagent_runtime: None,
                context_frame_session_root: None,
                context_frame_prompt_id: None,
                context_frame_budget: None,
                context_frame_attachment_threshold_bytes: None,
                usage_recorder: None,
                usage_context: None,
                source_pool: None,
                source_session_id: None,
                preserve_reasoning_content: false,
                goal_stop_policy: None,
            },
            &mut observer,
        )
        .await
        .unwrap();

        assert_eq!(turn.final_text, "done");

        // The tool call should have been begun (observer fires before the check)
        assert_eq!(observer.begins.len(), 1);
        assert_eq!(observer.begins[0].1, "mock_tool");

        // The tool call should have ended with failure (blocked)
        assert_eq!(observer.ends.len(), 1);
        assert!(
            !observer.ends[0].2,
            "blocked tool call should report as not successful"
        );
    }

    /// Scenario: a runaway model that never stops calling tools is stopped at the
    /// configured `max_turns` cap with a `ResponseError`.
    #[tokio::test]
    async fn tool_loop_stops_when_max_turns_is_reached() {
        #[derive(Clone)]
        struct EndlessToolCallModel;

        impl CompletionModel for EndlessToolCallModel {
            type Response = ();

            async fn completion(
                &self,
                _request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_loop".to_string(),
                        name: "mock_tool".to_string(),
                        function: Function {
                            name: "mock_tool".to_string(),
                            arguments: json!({"value": 1}),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }

        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(MockHandler));

        let err = run_tool_loop_with_history_and_observer(
            &EndlessToolCallModel,
            Vec::new(),
            "loop",
            &registry,
            ToolLoopConfig {
                max_turns: Some(3),
                ..Default::default()
            },
            &mut RecordingObserver::default(),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, CompletionError::ResponseError(msg) if msg.contains("maximum turns"))
        );
    }

    /// Scenario: model returns reasoning content but no text and no tool calls. The
    /// loop must error rather than retrying — repeated reasoning-only responses would
    /// infinitely loop without progress.
    #[tokio::test]
    async fn tool_loop_errors_on_reasoning_only_empty_content() {
        #[derive(Clone)]
        struct ReasoningOnlyModel {
            calls: Arc<AtomicUsize>,
        }

        impl CompletionModel for ReasoningOnlyModel {
            type Response = ();

            async fn completion(
                &self,
                _request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(CompletionResponse {
                    content: Vec::new(),
                    reasoning_content: Some("thinking without an answer".to_string()),
                    raw_response: (),
                })
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let model = ReasoningOnlyModel {
            calls: Arc::clone(&calls),
        };
        let temp_dir = TempDir::new().unwrap();
        let registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());

        let err = run_tool_loop_with_history_and_observer(
            &model,
            Vec::new(),
            "hello",
            &registry,
            ToolLoopConfig::default(),
            &mut RecordingObserver::default(),
        )
        .await
        .unwrap_err();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            matches!(err, CompletionError::ResponseError(msg) if msg.contains("reasoning_content"))
        );
    }

    /// Scenario: a completely empty response (no content, no reasoning) errors out
    /// immediately rather than burning through `max_turns` retrying.
    #[tokio::test]
    async fn tool_loop_errors_on_empty_response_without_retrying_to_max_turns() {
        #[derive(Clone)]
        struct EmptyModel {
            calls: Arc<AtomicUsize>,
        }

        impl CompletionModel for EmptyModel {
            type Response = ();

            async fn completion(
                &self,
                _request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(CompletionResponse {
                    content: Vec::new(),
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let model = EmptyModel {
            calls: Arc::clone(&calls),
        };
        let temp_dir = TempDir::new().unwrap();
        let registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());

        let err = run_tool_loop_with_history_and_observer(
            &model,
            Vec::new(),
            "hello",
            &registry,
            ToolLoopConfig {
                max_turns: Some(20),
                ..Default::default()
            },
            &mut RecordingObserver::default(),
        )
        .await
        .unwrap_err();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            matches!(err, CompletionError::ResponseError(msg) if msg.contains("empty response"))
        );
    }

    /// Scenario: when a model repeats the same tool/argument signature past the warn
    /// threshold, the next tool result includes a system warning so the model has a
    /// chance to change strategy.
    #[tokio::test]
    async fn tool_loop_warns_on_repeated_executed_tool_call() {
        #[derive(Clone)]
        struct RepeatingToolModel;

        impl CompletionModel for RepeatingToolModel {
            type Response = ();

            async fn completion(
                &self,
                request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                let tool_result_count = request
                    .chat_history
                    .iter()
                    .filter_map(|msg| match msg {
                        Message::User { content } => Some(
                            content
                                .iter()
                                .filter(|item| matches!(item, UserContent::ToolResult(_)))
                                .count(),
                        ),
                        _ => None,
                    })
                    .sum::<usize>();

                if tool_result_count >= 3 {
                    return Ok(CompletionResponse {
                        content: vec![AssistantContent::Text(Text {
                            text: "done".to_string(),
                        })],
                        reasoning_content: None,
                        raw_response: (),
                    });
                }

                Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: format!("call_{tool_result_count}"),
                        name: "mock_tool".to_string(),
                        function: Function {
                            name: "mock_tool".to_string(),
                            arguments: json!({"b": 2, "a": 1}),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }

        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(MockHandler));

        let turn = run_tool_loop_with_history_and_observer(
            &RepeatingToolModel,
            Vec::new(),
            "repeat",
            &registry,
            ToolLoopConfig::default(),
            &mut RecordingObserver::default(),
        )
        .await
        .unwrap();

        let tool_result_contents = turn
            .history
            .iter()
            .filter_map(|msg| match msg {
                Message::User { content } => content.iter().find_map(|item| match item {
                    UserContent::ToolResult(result) => {
                        result.result["content"].as_str().map(str::to_string)
                    }
                    _ => None,
                }),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(tool_result_contents.len(), 3);
        assert!(!tool_result_contents[0].contains("repeated tool call"));
        assert!(!tool_result_contents[1].contains("repeated tool call"));
        assert!(tool_result_contents[2].contains("repeated tool call"));
        assert!(tool_result_contents[2].contains("same arguments 3 times"));
    }

    /// Scenario: if the model ignores the repeat warning and continues, the loop
    /// hard-aborts at the abort threshold.
    #[tokio::test]
    async fn tool_loop_warns_then_aborts_repeated_successful_tool_call() {
        #[derive(Clone)]
        struct EndlessRepeatingToolModel;

        impl CompletionModel for EndlessRepeatingToolModel {
            type Response = ();

            async fn completion(
                &self,
                request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                let tool_result_count = request
                    .chat_history
                    .iter()
                    .filter_map(|msg| match msg {
                        Message::User { content } => Some(
                            content
                                .iter()
                                .filter(|item| matches!(item, UserContent::ToolResult(_)))
                                .count(),
                        ),
                        _ => None,
                    })
                    .sum::<usize>();

                Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: format!("call_{tool_result_count}"),
                        name: "mock_tool".to_string(),
                        function: Function {
                            name: "mock_tool".to_string(),
                            arguments: json!({"same": true}),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }

        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(MockHandler));
        let mut observer = RecordingObserver::default();

        let err = run_tool_loop_with_history_and_observer(
            &EndlessRepeatingToolModel,
            Vec::new(),
            "repeat",
            &registry,
            ToolLoopConfig {
                max_turns: Some(20),
                ..Default::default()
            },
            &mut observer,
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, CompletionError::ResponseError(msg) if msg.contains("repeated calls"))
        );
        assert_eq!(observer.result_texts.len(), DEFAULT_REPEAT_ABORT_THRESHOLD);
        assert!(observer.result_texts[2].contains("repeated tool call"));
        assert!(observer.result_texts[3].contains("repeated tool call"));
        assert!(observer.result_texts[4].contains("same arguments 5 times"));
    }

    /// Scenario: a successful call to a tool listed in `terminal_tools` short-circuits
    /// the loop and returns its output as the final answer.
    #[tokio::test]
    async fn tool_loop_stops_after_successful_terminal_tool() {
        #[derive(Clone)]
        struct TerminalToolModel {
            calls: Arc<AtomicUsize>,
            tool_name: &'static str,
        }

        impl CompletionModel for TerminalToolModel {
            type Response = ();

            async fn completion(
                &self,
                _request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_terminal".to_string(),
                        name: self.tool_name.to_string(),
                        function: Function {
                            name: self.tool_name.to_string(),
                            arguments: json!({"value": 1}),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let model = TerminalToolModel {
            calls: Arc::clone(&calls),
            tool_name: "mock_tool",
        };
        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(MockHandler));

        let turn = run_tool_loop_with_history_and_observer(
            &model,
            Vec::new(),
            "terminal",
            &registry,
            ToolLoopConfig {
                terminal_tools: Some(vec!["mock_tool".to_string()]),
                ..Default::default()
            },
            &mut RecordingObserver::default(),
        )
        .await
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(turn.final_text, "ok");
        assert_eq!(turn.history.len(), 3);
    }

    /// Helper: asserts the terminal-tool short-circuit works for a specific tool
    /// name. Used by the two `submit_*_draft` regression tests below.
    async fn assert_successful_named_terminal_tool(tool_name: &'static str) {
        #[derive(Clone)]
        struct TerminalToolModel {
            calls: Arc<AtomicUsize>,
            tool_name: &'static str,
        }

        impl CompletionModel for TerminalToolModel {
            type Response = ();

            async fn completion(
                &self,
                _request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_terminal".to_string(),
                        name: self.tool_name.to_string(),
                        function: Function {
                            name: self.tool_name.to_string(),
                            arguments: json!({"value": 1}),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let model = TerminalToolModel {
            calls: Arc::clone(&calls),
            tool_name,
        };
        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register(tool_name, Arc::new(MockHandler));

        let turn = run_tool_loop_with_history_and_observer(
            &model,
            Vec::new(),
            "terminal",
            &registry,
            ToolLoopConfig {
                terminal_tools: Some(vec![tool_name.to_string()]),
                ..Default::default()
            },
            &mut RecordingObserver::default(),
        )
        .await
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(turn.final_text, "ok");
    }

    /// Scenario: regression — `submit_intent_draft` is one of the terminal tools used
    /// by intent flows; a successful call must end the loop with the tool's text.
    #[tokio::test]
    async fn tool_loop_stops_after_successful_submit_intent_draft_terminal_tool() {
        assert_successful_named_terminal_tool("submit_intent_draft").await;
    }

    /// Scenario: regression — same as above for `submit_plan_draft`, used by plan
    /// generation flows.
    #[tokio::test]
    async fn tool_loop_stops_after_successful_submit_plan_draft_terminal_tool() {
        assert_successful_named_terminal_tool("submit_plan_draft").await;
    }

    /// Scenario: a *failed* terminal tool call must NOT short-circuit the loop —
    /// only successful ones count, so the model can retry or reroute.
    #[tokio::test]
    async fn tool_loop_does_not_stop_on_failed_terminal_tool() {
        struct FailingTerminalHandler;

        #[async_trait]
        impl ToolHandler for FailingTerminalHandler {
            fn kind(&self) -> ToolKind {
                ToolKind::Function
            }

            async fn handle(
                &self,
                _invocation: ToolInvocation,
            ) -> crate::internal::ai::tools::ToolResult<ToolOutput> {
                Err(crate::internal::ai::tools::ToolError::ExecutionFailed(
                    "terminal failed".to_string(),
                ))
            }

            fn schema(&self) -> ToolSpec {
                ToolSpec::new("terminal_tool", "terminal tool")
            }
        }

        #[derive(Clone)]
        struct FailedTerminalModel;

        impl CompletionModel for FailedTerminalModel {
            type Response = ();

            async fn completion(
                &self,
                request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                let has_tool_result = request.chat_history.iter().any(|msg| match msg {
                    Message::User { content } => content
                        .iter()
                        .any(|item| matches!(item, UserContent::ToolResult(_))),
                    _ => false,
                });
                if has_tool_result {
                    return Ok(CompletionResponse {
                        content: vec![AssistantContent::Text(Text {
                            text: "handled terminal failure".to_string(),
                        })],
                        reasoning_content: None,
                        raw_response: (),
                    });
                }

                Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_terminal".to_string(),
                        name: "terminal_tool".to_string(),
                        function: Function {
                            name: "terminal_tool".to_string(),
                            arguments: json!({}),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }

        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("terminal_tool", Arc::new(FailingTerminalHandler));

        let turn = run_tool_loop_with_history_and_observer(
            &FailedTerminalModel,
            Vec::new(),
            "terminal",
            &registry,
            ToolLoopConfig {
                terminal_tools: Some(vec!["terminal_tool".to_string()]),
                ..Default::default()
            },
            &mut RecordingObserver::default(),
        )
        .await
        .unwrap();

        assert_eq!(turn.final_text, "handled terminal failure");
    }

    /// Scenario: when an observer's preflight keeps blocking the same call signature,
    /// the loop aborts after `MAX_IDENTICAL_BLOCKED_TOOL_CALLS` rejections instead of
    /// letting the model retry forever.
    #[tokio::test]
    async fn tool_loop_stops_on_repeated_blocked_identical_calls() {
        #[derive(Clone)]
        struct BlockedLoopModel;

        impl CompletionModel for BlockedLoopModel {
            type Response = ();

            async fn completion(
                &self,
                _request: CompletionRequest,
            ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
                Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_blocked".to_string(),
                        name: "mock_tool".to_string(),
                        function: Function {
                            name: "mock_tool".to_string(),
                            arguments: json!({"value": 7}),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }

        #[derive(Default)]
        struct AlwaysBlockPreflightObserver;

        impl ToolLoopObserver for AlwaysBlockPreflightObserver {
            fn on_tool_call_preflight(
                &mut self,
                _call_id: &str,
                _tool_name: &str,
                _arguments: &Value,
            ) -> Result<(), String> {
                Err("blocked by test".to_string())
            }
        }

        let temp_dir = TempDir::new().unwrap();
        let mut registry = ToolRegistry::with_working_dir(temp_dir.path().to_path_buf());
        registry.register("mock_tool", Arc::new(MockHandler));

        let err = run_tool_loop_with_history_and_observer(
            &BlockedLoopModel,
            Vec::new(),
            "loop",
            &registry,
            ToolLoopConfig {
                max_turns: Some(20),
                ..Default::default()
            },
            &mut AlwaysBlockPreflightObserver,
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, CompletionError::ResponseError(msg) if msg.contains("repeated blocked calls"))
        );
    }

    fn long_tool_context_output() -> String {
        (0..8)
            .map(|index| format!("tool output line {index}: xxxxxxxxxxxxxxxxxxxx"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
