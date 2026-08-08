//! Task executor for running planned AI work through providers and tool loops.
//!
//! Boundary: executor launches attempts and captures outputs, while policy, workspace
//! sync, verification, and persistence remain separate. DAG/tool-loop and runtime
//! tests cover tool events, provider errors, and timeout boundaries.

use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use dagrs::{
    Action, CheckpointConfig, CompletionStatus, DefaultNode, EnvVar, FileCheckpointStore, Graph,
    InChannels, Node, NodeTable, OutChannels, Output, event::GraphEvent,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Semaphore;
use uuid::Uuid;

use super::{
    acl::{AclVerdict, cargo_lock_companion_allowed, check_tool_acl},
    checkpoint_policy::dagrs_checkpointing_enabled,
    gate, policy,
    run_state::{RunStateSnapshot, RunStateStore},
    types::{
        ExecutionPlanSpec, GateReport, OrchestratorError, OrchestratorObserver, PolicyViolation,
        ReviewOutcome, TaskKind, TaskNodeStatus, TaskResult, TaskRuntimeEvent,
        TaskRuntimeNoteLevel, TaskRuntimePhase, TaskSpec, TaskWorkspaceBackend, ToolCallRecord,
    },
    workspace::{
        FuseAttemptOutcome, FuseProvisionState, detect_contract_violations,
        format_contract_violation_message, is_fuse_infrastructure_error_message,
    },
};
use crate::internal::ai::{
    agent::{ToolLoopConfig, ToolLoopObserver, run_tool_loop_with_history_and_observer},
    completion::{
        CompletionError, CompletionModel, CompletionStreamEvent, CompletionUsage,
        CompletionUsageSummary,
    },
    hooks::HookRunner,
    intentspec::types::{IntentSpec, NetworkPolicy, ToolAcl},
    libra_vcs::run_libra_vcs_tool_guidance,
    runtime::environment::{ExecutionEnvironmentProvider, SyncBackRequest},
    sandbox::{
        NetworkAccess, SandboxPermissions, SandboxPolicy, ToolRuntimeContext, ToolSandboxContext,
    },
    tools::{ToolOutput, registry::ToolRegistry},
    workspace_snapshot::WorkspaceSnapshot,
};

/// Configuration for task execution.
#[derive(Clone)]
pub struct ExecutorConfig {
    pub tool_loop_config: ToolLoopConfig,
    pub max_retries: u8,
    pub backoff_seconds: u32,
    pub working_dir: PathBuf,
    pub spec: Arc<IntentSpec>,
    pub reviewer_preamble: Option<String>,
    pub dagrs_resume_checkpoint_id: Option<String>,
    pub observer: Option<Arc<dyn OrchestratorObserver>>,
    /// Pre-execution snapshot of the worktree this task runs in. When present,
    /// `execute_task` validates the contract immediately after reviewer approval
    /// so out-of-scope writes surface as retryable feedback to the LLM instead
    /// of escaping as terminal sync-back failures that force a replan.
    pub(crate) workspace_baseline: Option<Arc<WorkspaceSnapshot>>,
    /// Session-scoped FUSE provisioning gate. Once any task fails to mount FUSE,
    /// every subsequent provisioning skips FUSE and falls back to the copy
    /// backend immediately, with one user-visible note emitted at the moment
    /// of the first failure.
    pub fuse_state: FuseProvisionState,
}

const NO_CHANGES_NEEDED_TOKEN: &str = "[NO_CHANGES_NEEDED]";
const MAX_STORED_THINKING_CHARS: usize = 64_000;
const MAX_GATE_FAILURE_OUTPUT_CHARS: usize = 2_000;

struct TaskExecutionArtifacts {
    tool_calls: Vec<ToolCallRecord>,
    policy_violations: Vec<super::types::PolicyViolation>,
    model_usage: Option<CompletionUsageSummary>,
    thinking: Option<String>,
}

struct TaskExecutionObserver {
    spec: Arc<IntentSpec>,
    task: TaskSpec,
    working_dir: PathBuf,
    in_flight: HashMap<String, ToolCallRecord>,
    tool_calls: Vec<ToolCallRecord>,
    violations: Vec<super::types::PolicyViolation>,
    model_usage: CompletionUsageSummary,
    thinking: String,
    thinking_truncated: bool,
    observer: Option<Arc<dyn OrchestratorObserver>>,
}

impl TaskExecutionObserver {
    fn new(
        spec: Arc<IntentSpec>,
        task: TaskSpec,
        working_dir: PathBuf,
        observer: Option<Arc<dyn OrchestratorObserver>>,
    ) -> Self {
        Self {
            spec,
            task,
            working_dir,
            in_flight: HashMap::new(),
            tool_calls: Vec::new(),
            violations: Vec::new(),
            model_usage: CompletionUsageSummary::default(),
            thinking: String::new(),
            thinking_truncated: false,
            observer,
        }
    }

    fn finish(self) -> TaskExecutionArtifacts {
        let usage = (!self.model_usage.is_zero()).then_some(self.model_usage);
        TaskExecutionArtifacts {
            tool_calls: self.tool_calls,
            policy_violations: self.violations,
            model_usage: usage,
            thinking: non_empty_text(self.thinking),
        }
    }

    fn append_thinking_delta(&mut self, delta: &str) {
        if self.thinking_truncated {
            return;
        }

        let remaining = MAX_STORED_THINKING_CHARS.saturating_sub(self.thinking.chars().count());
        if delta.chars().count() <= remaining {
            self.thinking.push_str(delta);
            return;
        }

        self.thinking
            .extend(delta.chars().take(remaining.saturating_sub(1)));
        self.thinking.push_str("\n[thinking truncated]");
        self.thinking_truncated = true;
    }
}

impl ToolLoopObserver for TaskExecutionObserver {
    fn on_model_turn_start(&mut self, turn: usize) {
        if let Some(observer) = &self.observer {
            observer.on_task_runtime_event(
                &self.task,
                TaskRuntimeEvent::Phase(TaskRuntimePhase::AwaitingModel { turn }),
            );
        }
    }

    fn on_assistant_step_text(&mut self, text: &str) {
        if let Some(observer) = &self.observer {
            observer.on_task_runtime_event(
                &self.task,
                TaskRuntimeEvent::AssistantMessage(text.to_string()),
            );
        }
    }

    fn on_model_usage(&mut self, usage: &CompletionUsageSummary) {
        self.model_usage.merge(usage);
    }

    fn on_model_usage_recorded(&mut self, usage: &CompletionUsageSummary, wall_clock_ms: u64) {
        self.model_usage.merge(usage);
        if let Some(observer) = &self.observer {
            observer.on_task_runtime_event(
                &self.task,
                TaskRuntimeEvent::UsageUpdated {
                    usage: usage.clone(),
                    wall_clock_ms,
                },
            );
        }
    }

    fn on_model_stream_event(&mut self, event: &CompletionStreamEvent) {
        if let CompletionStreamEvent::ThinkingDelta { delta, .. } = event
            && !delta.is_empty()
        {
            self.append_thinking_delta(delta);
            if let Some(observer) = &self.observer {
                observer.on_task_runtime_event(
                    &self.task,
                    TaskRuntimeEvent::ThinkingDelta(delta.clone()),
                );
            }
        }
    }

    fn on_tool_call_begin(&mut self, call_id: &str, tool_name: &str, arguments: &Value) {
        if let Some(observer) = &self.observer {
            observer.on_task_runtime_event(
                &self.task,
                TaskRuntimeEvent::Phase(TaskRuntimePhase::ExecutingTool {
                    tool_name: tool_name.to_string(),
                }),
            );
            observer.on_task_runtime_event(
                &self.task,
                TaskRuntimeEvent::ToolCallBegin {
                    call_id: call_id.to_string(),
                    tool_name: tool_name.to_string(),
                    arguments: arguments.clone(),
                },
            );
        }
    }

    fn on_tool_call_preflight(
        &mut self,
        call_id: &str,
        tool_name: &str,
        arguments: &Value,
    ) -> Result<(), String> {
        match policy::evaluate_tool_call(
            &self.spec,
            &self.task,
            tool_name,
            arguments,
            &self.working_dir,
        ) {
            Ok(preflight) => {
                self.in_flight.insert(call_id.to_string(), preflight.record);
                Ok(())
            }
            Err(violation) => {
                self.violations.push(violation.clone());
                Err(violation.message)
            }
        }
    }

    fn on_tool_call_end(
        &mut self,
        call_id: &str,
        tool_name: &str,
        result: &Result<ToolOutput, String>,
    ) {
        if let Some(observer) = &self.observer {
            observer.on_task_runtime_event(
                &self.task,
                TaskRuntimeEvent::ToolCallEnd {
                    call_id: call_id.to_string(),
                    tool_name: tool_name.to_string(),
                    result: result.clone(),
                },
            );
        }
        if let Some(mut record) = self.in_flight.remove(call_id) {
            match result {
                Ok(output) => {
                    if let Err(violation) = policy::evaluate_tool_result(
                        &self.spec,
                        &self.task,
                        tool_name,
                        output,
                        &mut record,
                    ) {
                        self.violations.push(violation);
                    }
                }
                Err(message) => {
                    record.success = false;
                    record.summary = Some(message.clone());
                }
            }
            self.tool_calls.push(record);
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReviewerDecision {
    approved: bool,
    summary: String,
    #[serde(default)]
    issues: Vec<String>,
}

struct ReviewerPassArtifacts {
    outcome: Result<Option<ReviewOutcome>, String>,
    tool_calls: Vec<ToolCallRecord>,
    policy_violations: Vec<super::types::PolicyViolation>,
    model_usage: Option<CompletionUsageSummary>,
    thinking: Option<String>,
}

/// Execute a single task with retry logic.
pub async fn execute_task<M: CompletionModel>(
    task: &TaskSpec,
    model: &M,
    registry: &ToolRegistry,
    config: &ExecutorConfig,
) -> TaskResult
where
    M::Response: CompletionUsage,
{
    if task.kind == TaskKind::Gate {
        return execute_gate_task_in_task_worktree(
            task,
            &config.working_dir,
            &config.spec,
            config.tool_loop_config.runtime_context.as_ref(),
            config.observer.as_ref(),
            &config.fuse_state,
        )
        .await;
    }

    let allowed_tools = allowed_tools_for_task(&config.spec, task);
    let runtime_context = runtime_context_for_task(
        &config.spec,
        task,
        &config.working_dir,
        config.tool_loop_config.runtime_context.as_ref(),
    );
    let prompt = build_task_prompt(task, &config.working_dir, &allowed_tools);
    let mut retry_count: u8 = 0;
    let mut accumulated_tool_calls = Vec::new();
    let mut accumulated_policy_violations = Vec::new();
    let mut accumulated_model_usage = CompletionUsageSummary::default();
    let mut accumulated_thinking = String::new();
    let mut last_review = None;
    let mut retry_feedback: Option<String> = None;

    loop {
        let mut observer = TaskExecutionObserver::new(
            Arc::clone(&config.spec),
            task.clone(),
            config.working_dir.clone(),
            config.observer.clone(),
        );
        let tool_loop_config = ToolLoopConfig {
            allowed_tools: Some(allowed_tools.clone()),
            runtime_context: Some(runtime_context.clone()),
            // Reasoning models (DeepSeek v4-pro reasoning, OpenAI o-series, …)
            // routinely need 30+ turns to scaffold a project, run verification
            // commands, and submit a terminal handshake. The previous default
            // of 24 was tripping the cap on otherwise-healthy implementation
            // tasks (see Tool-loop-exceeded-maximum-turns clusters in libra.log
            // 2026-05-05). Repeated-call detection (window=6, abort=3) below
            // is the actual pathological-loop guard; this is just the outer
            // safety net.
            max_turns: Some(default_executor_max_turns(
                task,
                config.tool_loop_config.max_turns,
            )),
            // Convergence safeguards: a successful `submit_task_complete` call
            // ends the loop immediately, and tighter repeat thresholds catch
            // pathological "re-run the same shell command every turn" loops
            // (see L1499/L8434 in /Volumes/Data/libra.log for the I05 case).
            terminal_tools: Some(vec!["submit_task_complete".to_string()]),
            repeat_detection_window: Some(
                config.tool_loop_config.repeat_detection_window.unwrap_or(6),
            ),
            repeat_warning_threshold: Some(
                config
                    .tool_loop_config
                    .repeat_warning_threshold
                    .unwrap_or(2),
            ),
            repeat_abort_threshold: Some(
                config.tool_loop_config.repeat_abort_threshold.unwrap_or(3),
            ),
            ..config.tool_loop_config.clone()
        };
        let attempt_prompt = match retry_feedback.as_deref() {
            Some(feedback) => format!(
                "{prompt}\n\n## Previous Attempt Failure\n{feedback}\nCorrect this failure before reporting completion."
            ),
            None => prompt.clone(),
        };
        let agent_result = run_tool_loop_with_history_and_observer(
            model,
            Vec::new(),
            &attempt_prompt,
            registry,
            tool_loop_config,
            &mut observer,
        )
        .await;
        let artifacts = observer.finish();
        let TaskExecutionArtifacts {
            tool_calls,
            policy_violations,
            model_usage,
            thinking,
        } = artifacts;
        accumulated_tool_calls.extend(tool_calls.iter().cloned());
        accumulated_policy_violations.extend(policy_violations.iter().cloned());
        if let Some(usage) = model_usage.as_ref() {
            accumulated_model_usage.merge(usage);
        }
        append_optional_text(&mut accumulated_thinking, thinking);

        if let Some(reason) = tool_calls_fuse_infrastructure_failure(&accumulated_tool_calls) {
            return TaskResult {
                task_id: task.id(),
                status: TaskNodeStatus::Failed,
                gate_report: None,
                agent_output: Some(reason),
                retry_count,
                tool_calls: accumulated_tool_calls,
                policy_violations: accumulated_policy_violations,
                model_usage: (!accumulated_model_usage.is_zero())
                    .then_some(accumulated_model_usage),
                review: last_review,
                thinking: non_empty_text(accumulated_thinking),
            };
        }

        let retryable_failure = match agent_result {
            Ok(turn)
                if task_completion_allows_blocked_escalation(&policy_violations, &tool_calls) =>
            {
                if let Some(observer) = &config.observer
                    && !turn.final_text.trim().is_empty()
                {
                    observer.on_task_runtime_event(
                        task,
                        TaskRuntimeEvent::AssistantMessage(turn.final_text.clone()),
                    );
                }
                if let Some(reason) = submit_task_complete_fail_reason(&accumulated_tool_calls) {
                    (
                        Some(reason.clone()),
                        tool_calls,
                        policy_violations,
                        reason,
                        None,
                    )
                } else if let Some(reason) = implementation_missing_write_output(
                    task,
                    &accumulated_tool_calls,
                    turn.final_text.as_str(),
                ) {
                    (
                        Some(reason.clone()),
                        tool_calls,
                        policy_violations,
                        reason,
                        None,
                    )
                } else {
                    let review_artifacts = run_reviewer_pass(
                        task,
                        &turn.final_text,
                        &accumulated_tool_calls,
                        model,
                        registry,
                        config,
                    )
                    .await;
                    accumulated_tool_calls.extend(review_artifacts.tool_calls.iter().cloned());
                    accumulated_policy_violations
                        .extend(review_artifacts.policy_violations.iter().cloned());
                    if let Some(usage) = review_artifacts.model_usage.as_ref() {
                        accumulated_model_usage.merge(usage);
                    }
                    append_optional_text(
                        &mut accumulated_thinking,
                        review_artifacts.thinking.clone(),
                    );
                    let mut reviewer_infrastructure_failure = false;
                    let review = match review_artifacts.outcome {
                        Ok(review) => review,
                        Err(message) => {
                            reviewer_infrastructure_failure = true;
                            let review = Some(reviewer_infrastructure_failure_outcome(&message));
                            if let Some(observer) = &config.observer {
                                observer.on_task_runtime_event(
                                    task,
                                    TaskRuntimeEvent::Note {
                                        level: TaskRuntimeNoteLevel::Error,
                                        text: format!("review inconclusive · {message}"),
                                    },
                                );
                            }
                            review
                        }
                    };
                    if let Some(review) = review.as_ref()
                        && !reviewer_infrastructure_failure
                        && !review.approved
                    {
                        last_review = Some(review.clone());
                        // INVARIANT: agent_output records the *actual* failure
                        // reason so a Failed TaskResult surfaces "review
                        // rejected: …" through dagrs and observers, not the
                        // agent's now-disproved success claim. The verbatim
                        // final_text is preserved separately on retry via the
                        // failure_reason → retry_feedback channel below.
                        let reason = format!("review rejected: {}", review.summary);
                        (
                            Some(reason.clone()),
                            tool_calls,
                            policy_violations,
                            reason,
                            Some(review.clone()),
                        )
                    } else if let Some(reason) =
                        submit_task_complete_evidence_mismatch(&accumulated_tool_calls)
                    {
                        (
                            Some(reason.clone()),
                            tool_calls,
                            policy_violations,
                            reason,
                            review,
                        )
                    } else if let Some(violation_message) = workspace_contract_failure(task, config)
                    {
                        // Surface workspace contract violations (e.g. files
                        // modified outside the touch_files contract) to the LLM
                        // so it can correct course on the next attempt rather
                        // than failing terminally during sync-back. agent_output
                        // mirrors the violation message — same rationale as the
                        // review-rejected branch above.
                        (
                            Some(violation_message.clone()),
                            tool_calls,
                            policy_violations,
                            violation_message,
                            review,
                        )
                    } else {
                        return TaskResult {
                            task_id: task.id(),
                            status: TaskNodeStatus::Completed,
                            gate_report: None,
                            agent_output: Some(turn.final_text),
                            retry_count,
                            tool_calls: accumulated_tool_calls,
                            policy_violations: accumulated_policy_violations,
                            model_usage: (!accumulated_model_usage.is_zero())
                                .then_some(accumulated_model_usage),
                            review,
                            thinking: non_empty_text(accumulated_thinking),
                        };
                    }
                }
            }
            Ok(_turn) => {
                let reason = policy_violations
                    .iter()
                    .map(|violation| violation.message.clone())
                    .collect::<Vec<_>>()
                    .join("; ");
                // INVARIANT: this arm fires when policy violations blocked task
                // completion. agent_output should reflect those violations so a
                // terminally Failed TaskResult does not advertise the agent's
                // pre-violation final text as the failure cause.
                (
                    Some(reason.clone()),
                    tool_calls,
                    policy_violations,
                    reason,
                    None,
                )
            }
            Err(CompletionError::ResponseError(msg)) => {
                (Some(msg.clone()), tool_calls, policy_violations, msg, None)
            }
            Err(e) => {
                return TaskResult {
                    task_id: task.id(),
                    status: TaskNodeStatus::Failed,
                    gate_report: None,
                    agent_output: Some(e.to_string()),
                    retry_count,
                    tool_calls: accumulated_tool_calls,
                    policy_violations: accumulated_policy_violations,
                    model_usage: (!accumulated_model_usage.is_zero())
                        .then_some(accumulated_model_usage),
                    review: last_review,
                    thinking: non_empty_text(accumulated_thinking),
                };
            }
        };

        retry_count += 1;
        let (agent_output, _, _, failure_reason, review) = retryable_failure;
        if let Some(review) = review {
            last_review = Some(review);
        }
        if retry_count > config.max_retries {
            return TaskResult {
                task_id: task.id(),
                status: TaskNodeStatus::Failed,
                gate_report: None,
                agent_output,
                retry_count,
                tool_calls: accumulated_tool_calls,
                policy_violations: accumulated_policy_violations,
                model_usage: (!accumulated_model_usage.is_zero())
                    .then_some(accumulated_model_usage),
                review: last_review,
                thinking: non_empty_text(accumulated_thinking),
            };
        }

        tracing::warn!(task_id = %task.id(), "retrying task after failure: {}", failure_reason);
        retry_feedback = Some(failure_reason);
        if config.backoff_seconds > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(
                config.backoff_seconds as u64,
            ))
            .await;
        }
    }
}

/// Per-attempt tool-loop turn cap for non-Gate tasks.
///
/// Honors an explicit override on `ExecutorConfig.tool_loop_config.max_turns`
/// when present; otherwise picks a default sized to the task kind. Gates run
/// through `execute_gate_task_in_task_worktree` and never reach this path, so
/// only Implementation/Analysis cases need a bespoke default.
const DEFAULT_IMPLEMENTATION_MAX_TURNS: usize = 48;
const DEFAULT_ANALYSIS_MAX_TURNS: usize = 32;

fn default_executor_max_turns(task: &TaskSpec, override_value: Option<usize>) -> usize {
    if let Some(value) = override_value {
        return value;
    }
    match task.kind {
        TaskKind::Implementation => DEFAULT_IMPLEMENTATION_MAX_TURNS,
        TaskKind::Analysis => DEFAULT_ANALYSIS_MAX_TURNS,
        // Gate tasks take a separate execution path; default conservatively
        // for any future caller that lands here.
        TaskKind::Gate => DEFAULT_ANALYSIS_MAX_TURNS,
    }
}

fn workspace_contract_failure(task: &TaskSpec, config: &ExecutorConfig) -> Option<String> {
    let baseline = config.workspace_baseline.as_ref()?;
    match detect_contract_violations(
        &config.working_dir,
        baseline,
        &task.contract.touch_files,
        &task.scope_in,
        &task.scope_out,
    ) {
        Ok(violations) if violations.is_empty() => None,
        Ok(violations) => {
            let detail = format_contract_violation_message(&violations);
            Some(format!(
                "workspace contract violation detected before sync-back. Revert or move these changes back inside the declared contract before reporting completion.\n{detail}"
            ))
        }
        Err(err) => Some(format!(
            "failed to inspect workspace before sync-back: {err}. Investigate the workspace state and retry."
        )),
    }
}

fn implementation_missing_write_output(
    task: &TaskSpec,
    tool_calls: &[ToolCallRecord],
    agent_output: &str,
) -> Option<String> {
    if task.kind != TaskKind::Implementation {
        return None;
    }

    if has_successful_write(tool_calls) {
        return None;
    }

    if submit_task_complete_no_changes_needed(tool_calls) {
        return None;
    }

    if agent_declared_no_changes_needed(agent_output) && has_noop_evidence(tool_calls) {
        return None;
    }

    Some(format!(
        "implementation task completed without writing any files; use apply_patch or an allowed shell write to create or modify the expected project files before reporting completion. If you verified that no change is needed or the write scope is wrong, either call submit_task_complete with `result: \"no_changes_needed\"` and supporting evidence, or end the final response with {NO_CHANGES_NEEDED_TOKEN}"
    ))
}

fn has_successful_write(tool_calls: &[ToolCallRecord]) -> bool {
    tool_calls
        .iter()
        .any(|call| call.success && (!call.paths_written.is_empty() || !call.diffs.is_empty()))
}

fn has_noop_evidence(tool_calls: &[ToolCallRecord]) -> bool {
    tool_calls.iter().any(|call| {
        call.success
            && call.paths_written.is_empty()
            && call.diffs.is_empty()
            && (!call.paths_read.is_empty()
                || matches!(call.action.as_str(), "read" | "query" | "execute"))
    })
}

fn agent_declared_no_changes_needed(agent_output: &str) -> bool {
    agent_output.trim_end().ends_with(NO_CHANGES_NEEDED_TOKEN)
}

/// Returns `true` when the most recent successful `submit_task_complete` call
/// declared `result: "no_changes_needed"`. Models that submit the structured
/// argument do not always echo the `[NO_CHANGES_NEEDED]` text sentinel in
/// `final_text`, so this honours the documented tool-arg path as an equivalent
/// signal — otherwise the convergence check forces an infinite retry loop on
/// tasks whose worktree already satisfies acceptance criteria.
fn submit_task_complete_no_changes_needed(tool_calls: &[ToolCallRecord]) -> bool {
    submit_task_complete_result(tool_calls)
        .map(|result| result == "no_changes_needed")
        .unwrap_or(false)
}

fn submit_task_complete_fail_reason(tool_calls: &[ToolCallRecord]) -> Option<String> {
    let call = latest_successful_submit_task_complete(tool_calls)?;
    let result = call
        .arguments_json
        .as_ref()
        .and_then(|args| args.get("result"))
        .and_then(|value| value.as_str())?;
    if result != "fail" {
        return None;
    }

    let summary = call
        .arguments_json
        .as_ref()
        .and_then(|args| args.get("summary"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .unwrap_or("task reported failure without a summary");

    Some(format!(
        "task reported failure via submit_task_complete: {summary}"
    ))
}

fn submit_task_complete_evidence_mismatch(tool_calls: &[ToolCallRecord]) -> Option<String> {
    let (submit_index, submit_call) =
        latest_successful_submit_task_complete_with_index(tool_calls)?;
    let args = submit_call.arguments_json.as_ref()?;
    let result = args.get("result").and_then(|value| value.as_str())?;
    if !matches!(result, "pass" | "no_changes_needed") {
        return None;
    }
    let evidence = args.get("evidence")?.as_array()?;

    for (idx, entry) in evidence.iter().enumerate() {
        let command = entry
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|command| !command.is_empty())?;
        let claimed_exit = entry
            .get("exit_code")
            .and_then(|value| value.as_i64())
            .and_then(|code| i32::try_from(code).ok())?;
        let observed = observed_shell_exit_codes_before(tool_calls, submit_index, command);
        if observed.contains(&claimed_exit) {
            continue;
        }
        let observed_text = if observed.is_empty() {
            "not observed".to_string()
        } else {
            format!("observed exit codes: {:?}", observed)
        };
        return Some(format!(
            "submit_task_complete evidence[{idx}] does not match executed tool history: command '{}' claimed exit {}; {}",
            command, claimed_exit, observed_text
        ));
    }

    None
}

fn observed_shell_exit_codes_before(
    tool_calls: &[ToolCallRecord],
    submit_index: usize,
    command: &str,
) -> Vec<i32> {
    tool_calls
        .iter()
        .take(submit_index)
        .filter(|call| call.tool_name == "shell")
        .filter(|call| {
            call.arguments_json
                .as_ref()
                .and_then(|args| args.get("command"))
                .and_then(|value| value.as_str())
                .is_some_and(|observed| observed.trim() == command)
        })
        .filter_map(shell_call_exit_code)
        .collect()
}

fn shell_call_exit_code(call: &ToolCallRecord) -> Option<i32> {
    call.summary
        .as_deref()
        .and_then(|summary| summary.strip_prefix("Exit code: "))
        .and_then(|code| code.split_whitespace().next())
        .and_then(|code| code.parse::<i32>().ok())
}

fn submit_task_complete_result(tool_calls: &[ToolCallRecord]) -> Option<&str> {
    latest_successful_submit_task_complete(tool_calls)
        .and_then(|call| call.arguments_json.as_ref())
        .and_then(|args| args.get("result"))
        .and_then(|value| value.as_str())
}

fn task_completion_allows_blocked_escalation(
    policy_violations: &[PolicyViolation],
    tool_calls: &[ToolCallRecord],
) -> bool {
    policy_violations.is_empty()
        || (latest_successful_submit_task_complete(tool_calls).is_some()
            && policy_violations
                .iter()
                .all(|violation| violation.code == "sandbox-escalation-deny"))
}

fn latest_successful_submit_task_complete(
    tool_calls: &[ToolCallRecord],
) -> Option<&ToolCallRecord> {
    latest_successful_submit_task_complete_with_index(tool_calls).map(|(_, call)| call)
}

fn latest_successful_submit_task_complete_with_index(
    tool_calls: &[ToolCallRecord],
) -> Option<(usize, &ToolCallRecord)> {
    tool_calls
        .iter()
        .enumerate()
        .rev()
        .find(|(_, call)| call.success && call.tool_name == "submit_task_complete")
}

fn reviewer_infrastructure_failure_outcome(message: &str) -> ReviewOutcome {
    ReviewOutcome {
        approved: false,
        summary: "automated review did not complete; human review required".to_string(),
        issues: vec![message.to_string()],
    }
}

fn append_optional_text(target: &mut String, text: Option<String>) {
    let Some(text) = text else {
        return;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    if !target.trim().is_empty() {
        target.push_str("\n\n");
    }
    target.push_str(trimmed);
}

fn non_empty_text(text: String) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

async fn execute_gate_task(
    task: &TaskSpec,
    working_dir: &Path,
    spec: &IntentSpec,
    inherited_runtime: Option<&ToolRuntimeContext>,
    observer: Option<&Arc<dyn OrchestratorObserver>>,
) -> TaskResult {
    let runtime_context = runtime_context_for_gate_task(spec, working_dir, inherited_runtime);
    let gate_report = if task.checks.is_empty() {
        GateReport::empty()
    } else {
        let mut results = Vec::with_capacity(task.checks.len());
        let mut all_required_passed = true;

        for check in &task.checks {
            if let Some(observer) = observer {
                observer.on_task_runtime_event(
                    task,
                    TaskRuntimeEvent::Note {
                        level: TaskRuntimeNoteLevel::Info,
                        text: format!("gate running · {}", check.id),
                    },
                );
            }

            let result = gate::run_check_with_context(
                check,
                working_dir,
                Some(spec),
                Some(task),
                Some(&runtime_context),
            )
            .await;

            if let Some(observer) = observer {
                observer.on_task_runtime_event(
                    task,
                    TaskRuntimeEvent::Note {
                        level: if result.passed {
                            TaskRuntimeNoteLevel::Info
                        } else {
                            TaskRuntimeNoteLevel::Error
                        },
                        text: format!(
                            "gate {} · {} · exit {}",
                            if result.passed { "passed" } else { "failed" },
                            check.id,
                            result.exit_code
                        ),
                    },
                );
            }

            if !result.passed && check.required {
                all_required_passed = false;
            }
            results.push(result);
        }

        GateReport {
            results,
            all_required_passed,
        }
    };

    let status = if gate_report.all_required_passed {
        TaskNodeStatus::Completed
    } else {
        TaskNodeStatus::Failed
    };
    let agent_output = failed_gate_report_output(&gate_report);

    TaskResult {
        task_id: task.id(),
        status,
        gate_report: Some(gate_report),
        agent_output,
        retry_count: 0,
        tool_calls: Vec::new(),
        policy_violations: Vec::new(),
        model_usage: None,
        review: None,
        thinking: None,
    }
}

fn failed_gate_report_output(report: &GateReport) -> Option<String> {
    if report.all_required_passed {
        return None;
    }

    let failures = report
        .results
        .iter()
        .filter(|result| !result.passed)
        .map(|result| {
            let mut summary = format!(
                "- {} ({}) failed with exit {}{}",
                result.check_id,
                result.kind,
                result.exit_code,
                if result.timed_out {
                    " after timeout"
                } else {
                    ""
                }
            );
            if let Some(stderr) = gate_output_excerpt("stderr", &result.stderr) {
                summary.push('\n');
                summary.push_str(&stderr);
            }
            if let Some(stdout) = gate_output_excerpt("stdout", &result.stdout) {
                summary.push('\n');
                summary.push_str(&stdout);
            }
            summary
        })
        .collect::<Vec<_>>();

    if failures.is_empty() {
        Some("required gate checks failed without per-check output".to_string())
    } else {
        Some(format!(
            "required gate checks failed:\n{}",
            failures.join("\n")
        ))
    }
}

fn gate_output_excerpt(label: &str, output: &str) -> Option<String> {
    let output = output.trim();
    if output.is_empty() {
        return None;
    }

    let mut excerpt = output
        .chars()
        .take(MAX_GATE_FAILURE_OUTPUT_CHARS)
        .collect::<String>();
    if output.chars().count() > MAX_GATE_FAILURE_OUTPUT_CHARS {
        excerpt.push_str("\n...<truncated>");
    }
    Some(format!("{label}:\n{}", indent_lines(&excerpt, "  ")))
}

fn indent_lines(text: &str, indent: &str) -> String {
    text.lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn execute_gate_task_in_task_worktree(
    task: &TaskSpec,
    working_dir: &Path,
    spec: &IntentSpec,
    inherited_runtime: Option<&ToolRuntimeContext>,
    observer: Option<&Arc<dyn OrchestratorObserver>>,
    fuse_state: &FuseProvisionState,
) -> TaskResult {
    let environment_provider = ExecutionEnvironmentProvider;
    let mut retried_after_fuse_failure = false;

    loop {
        let environment = match environment_provider
            .provision_task_worktree(working_dir.to_path_buf(), task.id(), fuse_state.clone())
            .await
        {
            Ok(environment) => environment,
            Err(err) => return task_workspace_failure(task, err),
        };
        let task_worktree_root = environment.root().to_path_buf();
        let backend = environment.backend();

        if let Some(observer) = observer {
            emit_fuse_disabled_note_if_needed(task, environment.fuse_outcome(), Some(observer));
            observer.on_task_runtime_event(
                task,
                TaskRuntimeEvent::WorkspaceReady {
                    working_dir: task_worktree_root.clone(),
                    isolated: true,
                    backend,
                    main_working_dir: Some(working_dir.to_path_buf()),
                },
            );
        }

        let result =
            execute_gate_task(task, &task_worktree_root, spec, inherited_runtime, observer).await;
        let gate_fuse_failure =
            if backend == TaskWorkspaceBackend::Fuse && result.status == TaskNodeStatus::Failed {
                gate_result_fuse_infrastructure_failure(&result)
            } else {
                None
            };

        let cleanup_result = environment_provider.cleanup(environment).await;
        let cleanup_fuse_failure = cleanup_result.as_ref().err().and_then(|err| {
            if backend == TaskWorkspaceBackend::Fuse && result.status == TaskNodeStatus::Failed {
                fuse_cleanup_failure_reason(err)
            } else {
                None
            }
        });
        if let Err(err) = cleanup_result {
            tracing::warn!(
                path = %task_worktree_root.display(),
                "failed to clean up gate worktree: {}",
                err
            );
        }

        if backend == TaskWorkspaceBackend::Fuse
            && !retried_after_fuse_failure
            && let Some(reason) = gate_fuse_failure.or(cleanup_fuse_failure)
        {
            disable_fuse_after_runtime_failure(task, fuse_state, observer, &reason);
            retried_after_fuse_failure = true;
            continue;
        }

        return result;
    }
}

async fn run_reviewer_pass<M: CompletionModel>(
    task: &TaskSpec,
    agent_output: &str,
    tool_calls: &[ToolCallRecord],
    model: &M,
    registry: &ToolRegistry,
    config: &ExecutorConfig,
) -> ReviewerPassArtifacts
where
    M::Response: CompletionUsage,
{
    let Some(reviewer_preamble) = config.reviewer_preamble.clone() else {
        return ReviewerPassArtifacts {
            outcome: Ok(None),
            tool_calls: Vec::new(),
            policy_violations: Vec::new(),
            model_usage: None,
            thinking: None,
        };
    };

    let allowed_tools = allowed_tools_for_reviewer(&config.spec);
    let review_prompt = build_reviewer_prompt(
        task,
        agent_output,
        tool_calls,
        &config.working_dir,
        &allowed_tools,
    );
    let review_config = ToolLoopConfig {
        preamble: Some(reviewer_preamble),
        allowed_tools: Some(allowed_tools),
        runtime_context: Some(runtime_context_for_reviewer(
            &config.spec,
            config.tool_loop_config.runtime_context.as_ref(),
        )),
        max_turns: Some(config.tool_loop_config.max_turns.unwrap_or(8)),
        ..config.tool_loop_config.clone()
    };

    if let Some(observer) = &config.observer {
        observer.on_task_runtime_event(task, TaskRuntimeEvent::Phase(TaskRuntimePhase::Reviewing));
    }
    let mut observer = TaskExecutionObserver::new(
        Arc::clone(&config.spec),
        task.clone(),
        config.working_dir.clone(),
        config.observer.clone(),
    );
    let turn = run_tool_loop_with_history_and_observer(
        model,
        Vec::new(),
        &review_prompt,
        registry,
        review_config,
        &mut observer,
    )
    .await;
    let artifacts = observer.finish();
    let TaskExecutionArtifacts {
        tool_calls,
        policy_violations,
        model_usage,
        thinking,
    } = artifacts;
    let turn = match turn {
        Ok(turn) => turn,
        Err(err) => {
            return ReviewerPassArtifacts {
                outcome: Err(format!("reviewer pass failed: {err}")),
                tool_calls,
                policy_violations,
                model_usage,
                thinking,
            };
        }
    };

    let review = match parse_reviewer_decision(&turn.final_text) {
        Ok(review) => review,
        Err(error) => {
            return ReviewerPassArtifacts {
                outcome: Err(error),
                tool_calls,
                policy_violations,
                model_usage,
                thinking,
            };
        }
    };
    let outcome = ReviewOutcome {
        approved: review.approved,
        summary: review.summary,
        issues: review.issues,
    };
    if let Some(observer) = &config.observer {
        observer.on_task_runtime_event(
            task,
            TaskRuntimeEvent::Note {
                level: if outcome.approved {
                    TaskRuntimeNoteLevel::Info
                } else {
                    TaskRuntimeNoteLevel::Error
                },
                text: format!(
                    "review {} · {}",
                    if outcome.approved {
                        "approved"
                    } else {
                        "rejected"
                    },
                    outcome.summary
                ),
            },
        );
    }
    ReviewerPassArtifacts {
        outcome: Ok(Some(outcome)),
        tool_calls,
        policy_violations,
        model_usage,
        thinking,
    }
}

fn clone_tool_loop_config_for_workdir(
    config: &ToolLoopConfig,
    working_dir: &Path,
) -> ToolLoopConfig {
    let mut cloned = config.clone();
    if cloned.hook_runner.is_some() {
        cloned.hook_runner = Some(Arc::new(HookRunner::load(working_dir)));
    }
    cloned
}

fn should_use_task_worktree(task: &TaskSpec) -> bool {
    task.kind == TaskKind::Implementation
}

async fn execute_task_in_task_worktree<M: CompletionModel>(
    task: &TaskSpec,
    model: &M,
    registry: &Arc<ToolRegistry>,
    config: &ExecutorConfig,
    workspace_sync: &Arc<tokio::sync::Mutex<()>>,
) -> TaskResult
where
    M::Response: CompletionUsage,
{
    let environment_provider = ExecutionEnvironmentProvider;
    let mut sync_retry_count = 0_u8;
    let mut retried_after_fuse_failure = false;

    loop {
        let environment = match environment_provider
            .provision_task_worktree(
                config.working_dir.clone(),
                task.id(),
                config.fuse_state.clone(),
            )
            .await
        {
            Ok(environment) => environment,
            Err(err) => return task_workspace_failure(task, err),
        };
        let task_worktree_root = environment.root().to_path_buf();
        let backend = environment.backend();
        let baseline = environment.baseline_snapshot();

        let task_registry = Arc::new(registry.clone_with_working_dir_and_alias(
            task_worktree_root.clone(),
            config.working_dir.clone(),
        ));
        let mut task_config = config.clone();
        task_config.working_dir = task_worktree_root.clone();
        task_config.tool_loop_config =
            clone_tool_loop_config_for_workdir(&config.tool_loop_config, &task_worktree_root);
        task_config.workspace_baseline = Some(Arc::new(baseline));
        if let Some(observer) = &config.observer {
            emit_fuse_disabled_note_if_needed(task, environment.fuse_outcome(), Some(observer));
            observer.on_task_runtime_event(
                task,
                TaskRuntimeEvent::WorkspaceReady {
                    working_dir: task_worktree_root.clone(),
                    isolated: true,
                    backend,
                    main_working_dir: Some(config.working_dir.clone()),
                },
            );
        }

        let mut result = execute_task(task, model, &task_registry, &task_config).await;

        if backend == TaskWorkspaceBackend::Fuse
            && !retried_after_fuse_failure
            && let Some(reason) = task_result_fuse_infrastructure_failure(&result)
        {
            disable_fuse_after_runtime_failure(
                task,
                &config.fuse_state,
                config.observer.as_ref(),
                &reason,
            );
            cleanup_task_environment(
                &environment_provider,
                environment,
                &task_worktree_root,
                "task worktree",
            )
            .await;
            retried_after_fuse_failure = true;
            continue;
        }

        if result.status == TaskNodeStatus::Completed {
            let sync_result = {
                let _guard = workspace_sync.lock().await;
                environment_provider
                    .sync_back(
                        &environment,
                        SyncBackRequest {
                            main_working_dir: config.working_dir.clone(),
                            touch_files: task.contract.touch_files.clone(),
                            scope_in: task.scope_in.clone(),
                            scope_out: task.scope_out.clone(),
                        },
                    )
                    .await
            };

            match sync_result {
                Ok(report) => {
                    if sync_retry_count > 0 {
                        result.retry_count = result.retry_count.saturating_add(sync_retry_count);
                    }
                    emit_sync_back_report(task, &report, config.observer.as_ref());
                }
                Err(err)
                    if backend == TaskWorkspaceBackend::Fuse
                        && err.is_fuse_infrastructure()
                        && !retried_after_fuse_failure =>
                {
                    let reason = err.to_string();
                    disable_fuse_after_runtime_failure(
                        task,
                        &config.fuse_state,
                        config.observer.as_ref(),
                        &reason,
                    );
                    cleanup_task_environment(
                        &environment_provider,
                        environment,
                        &task_worktree_root,
                        "failed FUSE task worktree",
                    )
                    .await;
                    retried_after_fuse_failure = true;
                    continue;
                }
                Err(err)
                    if err.is_retryable_conflict() && sync_retry_count < config.max_retries =>
                {
                    sync_retry_count = sync_retry_count.saturating_add(1);
                    if let Some(observer) = &config.observer {
                        observer.on_task_runtime_event(
                            task,
                            TaskRuntimeEvent::Note {
                                level: TaskRuntimeNoteLevel::Info,
                                text: format!(
                                    "sync-back conflict detected; retrying task with a fresh baseline ({}/{}) · {}",
                                    sync_retry_count,
                                    config.max_retries,
                                    err
                                ),
                            },
                        );
                    }
                    cleanup_task_environment(
                        &environment_provider,
                        environment,
                        &task_worktree_root,
                        "conflicted task worktree",
                    )
                    .await;
                    continue;
                }
                Err(err) => {
                    let detail = format!(
                        "task completed in isolated worktree but failed to sync changes back: {err}"
                    );
                    if let Some(observer) = &config.observer {
                        observer.on_task_runtime_event(
                            task,
                            TaskRuntimeEvent::Note {
                                level: TaskRuntimeNoteLevel::Error,
                                text: detail.clone(),
                            },
                        );
                    }
                    result.status = TaskNodeStatus::Failed;
                    result.agent_output = Some(detail);
                }
            }
        }

        cleanup_task_environment(
            &environment_provider,
            environment,
            &task_worktree_root,
            "task worktree",
        )
        .await;

        return result;
    }
}

async fn cleanup_task_environment(
    environment_provider: &ExecutionEnvironmentProvider,
    environment: crate::internal::ai::runtime::environment::TaskExecutionEnvironment,
    task_worktree_root: &Path,
    label: &str,
) {
    if let Err(err) = environment_provider.cleanup(environment).await {
        tracing::warn!(
            path = %task_worktree_root.display(),
            "failed to clean up {}: {}",
            label,
            err
        );
    }
}

fn emit_sync_back_report(
    task: &TaskSpec,
    report: &crate::internal::ai::orchestrator::workspace::SyncBackReport,
    observer: Option<&Arc<dyn OrchestratorObserver>>,
) {
    if report.already_applied.is_empty() && report.merged.is_empty() && report.skipped.is_empty() {
        return;
    }
    let Some(observer) = observer else { return };
    let mut details = Vec::new();
    if !report.already_applied.is_empty() {
        details.push(format!(
            "already applied: {}",
            display_paths(&report.already_applied)
        ));
    }
    if !report.merged.is_empty() {
        details.push(format!("merged: {}", display_paths(&report.merged)));
    }
    if !report.skipped.is_empty() {
        details.push(format!(
            "skipped: {}",
            report
                .skipped
                .iter()
                .map(|path| path.path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    observer.on_task_runtime_event(
        task,
        TaskRuntimeEvent::Note {
            level: TaskRuntimeNoteLevel::Info,
            text: format!("sync-back completed with {}", details.join("; ")),
        },
    );
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn task_result_fuse_infrastructure_failure(result: &TaskResult) -> Option<String> {
    tool_calls_fuse_infrastructure_failure(&result.tool_calls).or_else(|| {
        result
            .agent_output
            .as_deref()
            .filter(|output| is_fuse_infrastructure_error_message(output))
            .map(str::to_string)
    })
}

fn gate_result_fuse_infrastructure_failure(result: &TaskResult) -> Option<String> {
    task_result_fuse_infrastructure_failure(result).or_else(|| {
        let report = result.gate_report.as_ref()?;
        report.results.iter().find_map(|gate| {
            if gate.passed {
                return None;
            }

            let detail = [&gate.stderr, &gate.stdout]
                .into_iter()
                .map(|output| output.trim())
                .find(|output| is_fuse_backend_failure_message(output))?;
            Some(format!(
                "gate '{}' encountered FUSE infrastructure failure: {}",
                gate.check_id,
                truncate_fuse_failure_reason(detail)
            ))
        })
    })
}

fn tool_calls_fuse_infrastructure_failure(tool_calls: &[ToolCallRecord]) -> Option<String> {
    tool_calls
        .iter()
        .rev()
        .filter(|call| !call.success)
        .filter_map(|call| call.summary.as_deref())
        .find(|summary| is_fuse_infrastructure_error_message(summary))
        .map(str::to_string)
}

fn disable_fuse_after_runtime_failure(
    task: &TaskSpec,
    fuse_state: &FuseProvisionState,
    observer: Option<&Arc<dyn OrchestratorObserver>>,
    reason: &str,
) {
    let first_disable = fuse_state.disable_first_time();
    if let Some(observer) = observer {
        let prefix = if first_disable {
            "FUSE worktree failed during task execution"
        } else {
            "FUSE worktree remained unavailable during task execution"
        };
        observer.on_task_runtime_event(
            task,
            TaskRuntimeEvent::Note {
                level: TaskRuntimeNoteLevel::Error,
                text: format!(
                    "{}: {}. Cleaning the failed mount and retrying once with copy backend.",
                    prefix,
                    truncate_fuse_failure_reason(reason)
                ),
            },
        );
    }
}

fn fuse_cleanup_failure_reason(err: &io::Error) -> Option<String> {
    if is_fuse_backend_failure_message(&err.to_string()) {
        Some(format!("failed to clean up FUSE worktree: {err}"))
    } else {
        None
    }
}

fn is_fuse_backend_failure_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    is_fuse_infrastructure_error_message(message)
        || lower.contains("os error 22")
        || lower.contains("invalid argument (os error 22)")
}

fn task_workspace_failure(task: &TaskSpec, err: io::Error) -> TaskResult {
    TaskResult {
        task_id: task.id(),
        status: TaskNodeStatus::Failed,
        gate_report: None,
        agent_output: Some(format!("failed to prepare isolated worktree: {err}")),
        retry_count: 0,
        tool_calls: Vec::new(),
        policy_violations: Vec::new(),
        model_usage: None,
        review: None,
        thinking: None,
    }
}

/// Emit a single user-visible TUI note when this task was the first to fail
/// FUSE provisioning and triggered the session-wide disable. All subsequent
/// task worktree provisioning will skip FUSE silently.
fn emit_fuse_disabled_note_if_needed(
    task: &TaskSpec,
    outcome: &FuseAttemptOutcome,
    observer: Option<&Arc<dyn OrchestratorObserver>>,
) {
    let Some(reason) = outcome.disabled_reason() else {
        return;
    };
    let Some(observer) = observer else { return };
    observer.on_task_runtime_event(
        task,
        TaskRuntimeEvent::Note {
            level: TaskRuntimeNoteLevel::Info,
            text: format!(
                "FUSE worktree mount failed: {}. Disabled for this session, using copy backend (slightly slower startup, identical behavior).",
                truncate_fuse_failure_reason(reason)
            ),
        },
    );
}

fn truncate_fuse_failure_reason(reason: &str) -> String {
    const MAX_LEN: usize = 1024;
    if reason.chars().count() <= MAX_LEN {
        return reason.to_string();
    }

    let mut truncated = reason.chars().take(MAX_LEN).collect::<String>();
    truncated.push_str("...<truncated>");
    truncated
}

fn terminal_task_result(
    task: &TaskSpec,
    status: TaskNodeStatus,
    agent_output: impl Into<Option<String>>,
) -> TaskResult {
    TaskResult {
        task_id: task.id(),
        status,
        gate_report: None,
        agent_output: agent_output.into(),
        retry_count: 0,
        tool_calls: Vec::new(),
        policy_violations: Vec::new(),
        model_usage: None,
        review: None,
        thinking: None,
    }
}

async fn record_terminal_result(
    run_state: &RunStateStore,
    observer: Option<&Arc<dyn OrchestratorObserver>>,
    task: &TaskSpec,
    result: TaskResult,
) {
    if let Some(observer) = observer {
        observer.on_task_runtime_event(
            task,
            TaskRuntimeEvent::Phase(match result.status {
                TaskNodeStatus::Completed => TaskRuntimePhase::Completed,
                TaskNodeStatus::Failed => TaskRuntimePhase::Failed,
                _ => TaskRuntimePhase::Pending,
            }),
        );
    }
    run_state.record_result(result).await;
}

struct TaskDagrsAction<M: CompletionModel + 'static> {
    task: TaskSpec,
    model: M,
    registry: Arc<ToolRegistry>,
    config: ExecutorConfig,
    run_state: RunStateStore,
    metered_task_ids: Arc<HashSet<Uuid>>,
    parallelism: Arc<Semaphore>,
    cost_budget_serial: Arc<tokio::sync::Mutex<()>>,
    workspace_sync_serial: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone)]
struct DagrsBuildContext {
    run_state: RunStateStore,
    metered_task_ids: Arc<HashSet<Uuid>>,
    parallelism: Arc<Semaphore>,
    cost_budget_serial: Arc<tokio::sync::Mutex<()>>,
    workspace_sync_serial: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone)]
struct DagrsDependencySignal {
    success: bool,
}

async fn broadcast_dependency_signal(out_channels: &mut OutChannels, success: bool) {
    let _ = out_channels
        .broadcast(dagrs::Content::new(DagrsDependencySignal { success }))
        .await;
}

#[async_trait]
impl<M: CompletionModel + 'static> Action for TaskDagrsAction<M>
where
    M::Response: CompletionUsage,
{
    async fn run(
        &self,
        in_channels: &mut InChannels,
        out_channels: &mut OutChannels,
        _env: Arc<EnvVar>,
    ) -> Output {
        let sender_ids = in_channels.get_sender_ids();
        let mut dependencies_ok = true;
        for sender_id in sender_ids {
            match in_channels.recv_from(&sender_id).await {
                Ok(content) => {
                    let signal = content.into_inner::<DagrsDependencySignal>();
                    if signal.as_deref().is_none_or(|signal| !signal.success) {
                        dependencies_ok = false;
                    }
                }
                Err(_) => {
                    dependencies_ok = false;
                }
            }
        }

        if !dependencies_ok {
            record_terminal_result(
                &self.run_state,
                self.config.observer.as_ref(),
                &self.task,
                terminal_task_result(
                    &self.task,
                    TaskNodeStatus::Skipped,
                    Some(
                        "skipped because an upstream dependency did not complete successfully"
                            .into(),
                    ),
                ),
            )
            .await;
            broadcast_dependency_signal(out_channels, false).await;
            return Output::empty();
        }

        let _parallel_permit = match Arc::clone(&self.parallelism).acquire_owned().await {
            Ok(permit) => permit,
            Err(err) => {
                let message = format!(
                    "failed to acquire execution permit for task {}: {}",
                    self.task.title(),
                    err
                );
                record_terminal_result(
                    &self.run_state,
                    self.config.observer.as_ref(),
                    &self.task,
                    terminal_task_result(&self.task, TaskNodeStatus::Failed, Some(message.clone())),
                )
                .await;
                broadcast_dependency_signal(out_channels, false).await;
                return Output::execution_failed(message);
            }
        };

        // Cost units are currently metered as completed/failed implementation tasks.
        // TODO: switch to provider token/cost usage accumulation when usage plumbing lands.
        let max_cost_units = self.config.spec.constraints.resources.max_cost_units as usize;
        let mut cost_budget_guard = None;
        if max_cost_units > 0 {
            let consumed = self
                .run_state
                .metered_result_count(&self.metered_task_ids)
                .await;
            if consumed >= max_cost_units {
                let message = format!(
                    "cost budget exceeded: maxCostUnits={} consumed={}",
                    max_cost_units, consumed
                );
                record_terminal_result(
                    &self.run_state,
                    self.config.observer.as_ref(),
                    &self.task,
                    terminal_task_result(&self.task, TaskNodeStatus::Failed, Some(message.clone())),
                )
                .await;
                broadcast_dependency_signal(out_channels, false).await;
                return Output::execution_failed(message);
            }

            let remaining = max_cost_units.saturating_sub(consumed);
            if remaining <= 1 {
                let guard = self.cost_budget_serial.lock().await;
                let consumed_after_lock = self
                    .run_state
                    .metered_result_count(&self.metered_task_ids)
                    .await;
                if consumed_after_lock >= max_cost_units {
                    let message = format!(
                        "cost budget exceeded: maxCostUnits={} consumed={}",
                        max_cost_units, consumed_after_lock
                    );
                    record_terminal_result(
                        &self.run_state,
                        self.config.observer.as_ref(),
                        &self.task,
                        terminal_task_result(
                            &self.task,
                            TaskNodeStatus::Failed,
                            Some(message.clone()),
                        ),
                    )
                    .await;
                    broadcast_dependency_signal(out_channels, false).await;
                    return Output::execution_failed(message);
                }
                cost_budget_guard = Some(guard);
            }
        }

        if let Some(observer) = &self.config.observer {
            observer.on_task_runtime_event(
                &self.task,
                TaskRuntimeEvent::Phase(TaskRuntimePhase::Starting),
            );
        }

        let use_task_worktree = should_use_task_worktree(&self.task);
        if !use_task_worktree
            && self.task.kind != TaskKind::Gate
            && let Some(observer) = &self.config.observer
        {
            observer.on_task_runtime_event(
                &self.task,
                TaskRuntimeEvent::WorkspaceReady {
                    working_dir: self.config.working_dir.clone(),
                    isolated: false,
                    backend: TaskWorkspaceBackend::Shared,
                    main_working_dir: None,
                },
            );
        }

        let result = if use_task_worktree {
            execute_task_in_task_worktree(
                &self.task,
                &self.model,
                &self.registry,
                &self.config,
                &self.workspace_sync_serial,
            )
            .await
        } else {
            execute_task(&self.task, &self.model, &self.registry, &self.config).await
        };

        if let Some(observer) = &self.config.observer {
            observer.on_task_runtime_event(
                &self.task,
                TaskRuntimeEvent::Phase(match result.status {
                    TaskNodeStatus::Completed => TaskRuntimePhase::Completed,
                    TaskNodeStatus::Failed => TaskRuntimePhase::Failed,
                    _ => TaskRuntimePhase::Pending,
                }),
            );
            observer.on_task_runtime_event(
                &self.task,
                TaskRuntimeEvent::Note {
                    level: if result.status == TaskNodeStatus::Failed {
                        TaskRuntimeNoteLevel::Error
                    } else {
                        TaskRuntimeNoteLevel::Info
                    },
                    text: match result.status {
                        TaskNodeStatus::Completed => "task completed".to_string(),
                        TaskNodeStatus::Failed => result
                            .agent_output
                            .clone()
                            .map(|message| format!("task failed · {message}"))
                            .unwrap_or_else(|| "task failed".to_string()),
                        TaskNodeStatus::Pending => "task pending".to_string(),
                        TaskNodeStatus::Running => "task running".to_string(),
                        TaskNodeStatus::Skipped => "task skipped".to_string(),
                    },
                },
            );
        }

        self.run_state.record_result(result.clone()).await;
        drop(cost_budget_guard);

        match result.status {
            TaskNodeStatus::Completed => {
                broadcast_dependency_signal(out_channels, true).await;
                Output::empty()
            }
            TaskNodeStatus::Failed => {
                broadcast_dependency_signal(out_channels, false).await;
                Output::execution_failed(
                    result
                        .agent_output
                        .clone()
                        .unwrap_or_else(|| format!("task {} failed", self.task.title())),
                )
            }
            TaskNodeStatus::Skipped => {
                broadcast_dependency_signal(out_channels, false).await;
                Output::empty()
            }
            TaskNodeStatus::Pending | TaskNodeStatus::Running => {
                broadcast_dependency_signal(out_channels, false).await;
                Output::execution_failed(format!(
                    "task {} returned invalid terminal state",
                    self.task.title()
                ))
            }
        }
    }
}

fn build_dagrs_graph<M: CompletionModel + 'static>(
    plan: &ExecutionPlanSpec,
    model: &M,
    registry: &Arc<ToolRegistry>,
    config: &ExecutorConfig,
    context: DagrsBuildContext,
) -> Result<Graph, OrchestratorError>
where
    M::Response: CompletionUsage,
{
    let mut graph = Graph::new();
    configure_graph_runtime(&mut graph, plan, config);
    let mut node_table = NodeTable::new();
    let mut dagrs_ids = HashMap::new();

    for task_spec in &plan.tasks {
        let action = TaskDagrsAction {
            task: task_spec.clone(),
            model: model.clone(),
            registry: Arc::clone(registry),
            config: config.clone(),
            run_state: context.run_state.clone(),
            metered_task_ids: Arc::clone(&context.metered_task_ids),
            parallelism: Arc::clone(&context.parallelism),
            cost_budget_serial: Arc::clone(&context.cost_budget_serial),
            workspace_sync_serial: Arc::clone(&context.workspace_sync_serial),
        };
        let dagrs_node =
            DefaultNode::with_action(task_spec.id().to_string(), action, &mut node_table);
        let dagrs_id = dagrs_node.id();
        graph.add_node(dagrs_node).map_err(|err| {
            OrchestratorError::PlanningFailed(format!(
                "failed to add dagrs node for task {}: {err}",
                task_spec.id()
            ))
        })?;
        dagrs_ids.insert(task_spec.id(), dagrs_id);
    }

    for task_spec in &plan.tasks {
        let to_id = dagrs_ids.get(&task_spec.id()).copied().ok_or_else(|| {
            OrchestratorError::PlanningFailed(format!(
                "missing dagrs node for task {}",
                task_spec.id()
            ))
        })?;
        for dep in task_spec.dependencies() {
            let from_id = dagrs_ids.get(dep).copied().ok_or_else(|| {
                OrchestratorError::PlanningFailed(format!(
                    "missing dagrs node for dependency {dep}"
                ))
            })?;
            graph.add_edge(from_id, vec![to_id]).map_err(|err| {
                OrchestratorError::PlanningFailed(format!(
                    "failed to add dagrs edge {dep} -> {}: {err}",
                    task_spec.id()
                ))
            })?;
        }
    }

    Ok(graph)
}

fn configure_graph_runtime(graph: &mut Graph, plan: &ExecutionPlanSpec, config: &ExecutorConfig) {
    if !dagrs_checkpointing_enabled(&config.spec) {
        return;
    }

    let checkpoint_dir = dagrs_checkpoint_dir(&config.working_dir, plan);
    graph.set_checkpoint_store(Box::new(FileCheckpointStore::new(checkpoint_dir)));

    let checkpoint_interval = usize::from(plan.max_parallel.max(1));
    graph.set_checkpoint_config(
        CheckpointConfig::enabled()
            .with_node_interval(checkpoint_interval)
            .with_max_checkpoints(8),
    );
}

fn dagrs_checkpoint_dir(working_dir: &Path, plan: &ExecutionPlanSpec) -> PathBuf {
    working_dir
        .join(".libra")
        .join("dagrs-checkpoints")
        .join(sanitize_checkpoint_component(&plan.intent_spec_id))
        .join(format!("rev-{}", plan.revision))
}

fn sanitize_checkpoint_component(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '_',
        })
        .collect();
    if sanitized.is_empty() {
        "intent-spec".to_string()
    } else {
        sanitized
    }
}

async fn monitor_graph_events(
    mut events: tokio::sync::broadcast::Receiver<GraphEvent>,
    run_state: RunStateStore,
    observer: Option<Arc<dyn OrchestratorObserver>>,
    total_nodes: usize,
) {
    run_state.record_graph_progress(0, total_nodes).await;
    loop {
        match events.recv().await {
            Ok(GraphEvent::NodeSuccess { .. })
            | Ok(GraphEvent::NodeFailed { .. })
            | Ok(GraphEvent::NodeSkipped { .. }) => {
                let next_completed = run_state.increment_graph_completed(total_nodes).await;
                if let Some(observer) = &observer {
                    observer.on_graph_progress(next_completed, total_nodes);
                }
            }
            Ok(GraphEvent::CheckpointSaved {
                checkpoint_id,
                pc,
                completed_nodes,
            }) => {
                run_state
                    .record_graph_checkpoint(checkpoint_id.clone(), pc, completed_nodes)
                    .await;
                if let Some(observer) = &observer {
                    observer.on_graph_checkpoint_saved(&checkpoint_id, pc, completed_nodes);
                }
            }
            Ok(GraphEvent::CheckpointRestored { checkpoint_id, pc }) => {
                run_state
                    .record_graph_restore(checkpoint_id.clone(), pc)
                    .await;
                if let Some(observer) = &observer {
                    observer.on_graph_checkpoint_restored(&checkpoint_id, pc);
                }
            }
            Ok(GraphEvent::ExecutionTerminated { .. }) => break,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Execute all tasks in the DAG in topological order.
pub async fn execute_dag<M: CompletionModel + 'static>(
    plan_spec: &ExecutionPlanSpec,
    model: &M,
    registry: &Arc<ToolRegistry>,
    config: &ExecutorConfig,
) -> Result<RunStateSnapshot, OrchestratorError>
where
    M::Response: CompletionUsage,
{
    if config.dagrs_resume_checkpoint_id.is_some() {
        return Err(OrchestratorError::ConfigError(
            "dagrs checkpoint resume is not supported yet; TODO: redesign resume semantics after userspace-fs checkpoint integration".to_string(),
        ));
    }

    let run_state = RunStateStore::new();
    let plan_snapshot = plan_spec.clone();
    let model_snapshot = model.clone();
    let registry_snapshot = Arc::clone(registry);
    let config_snapshot = config.clone();
    let run_state_snapshot = run_state.clone();
    let metered_task_ids = Arc::new(
        plan_spec
            .tasks
            .iter()
            .filter(|task| matches!(task.kind, TaskKind::Implementation))
            .map(TaskSpec::id)
            .collect::<HashSet<_>>(),
    );
    let max_cost_units = config.spec.constraints.resources.max_cost_units as usize;
    let initial_parallel = if max_cost_units > 0 && max_cost_units <= 2 {
        1
    } else {
        plan_spec.max_parallel.max(1) as usize
    };
    let parallelism = Arc::new(Semaphore::new(initial_parallel));
    let cost_budget_serial = Arc::new(tokio::sync::Mutex::new(()));
    let workspace_sync_serial = Arc::new(tokio::sync::Mutex::new(()));
    let graph_context = DagrsBuildContext {
        run_state: run_state_snapshot,
        metered_task_ids,
        parallelism,
        cost_budget_serial,
        workspace_sync_serial,
    };
    let mut graph = tokio::task::spawn_blocking(move || {
        build_dagrs_graph(
            &plan_snapshot,
            &model_snapshot,
            &registry_snapshot,
            &config_snapshot,
            graph_context,
        )
    })
    .await
    .map_err(|err| {
        OrchestratorError::ConfigError(format!("failed to build dagrs graph: {err}"))
    })??;

    let event_monitor = tokio::spawn(monitor_graph_events(
        graph.subscribe(),
        run_state.clone(),
        config.observer.clone(),
        plan_spec.tasks.len(),
    ));

    let wall_clock_limit_secs = config.spec.constraints.resources.max_wall_clock_seconds as u64;
    let execution_result = if wall_clock_limit_secs == 0 {
        graph.async_start().await
    } else {
        match tokio::time::timeout(
            Duration::from_secs(wall_clock_limit_secs),
            graph.async_start(),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                drop(graph);
                if let Err(err) = event_monitor.await {
                    tracing::warn!("dagrs event monitor terminated unexpectedly: {}", err);
                }
                return Err(OrchestratorError::AgentError(format!(
                    "execution exceeded constraints.resources.maxWallClockSeconds={}s",
                    wall_clock_limit_secs
                )));
            }
        }
    };
    drop(graph);
    if let Err(err) = event_monitor.await {
        tracing::warn!("dagrs event monitor terminated unexpectedly: {}", err);
    }

    let (execution_report, execution_error) = match execution_result {
        Ok(report) => {
            tracing::debug!(
                run_id = %report.run_id,
                node_total = report.node_total,
                node_succeeded = report.node_succeeded,
                node_failed = report.node_failed,
                node_skipped = report.node_skipped,
                status = ?report.status,
                "dagrs execution completed"
            );
            run_state
                .record_graph_execution_report(
                    report.node_succeeded,
                    report.node_failed,
                    report.node_skipped,
                    report.node_total,
                )
                .await;
            (Some(report), None)
        }
        Err(err) => {
            tracing::warn!("dagrs execution terminated with error: {}", err);
            (None, Some(err.to_string()))
        }
    };

    let snapshot = run_state.snapshot(plan_spec).await;
    let incomplete_tasks = plan_spec
        .tasks
        .iter()
        .filter(|task| {
            matches!(
                snapshot.status_for(task.id()),
                TaskNodeStatus::Pending | TaskNodeStatus::Running
            )
        })
        .map(|task| task.title().to_string())
        .collect::<Vec<_>>();
    if !incomplete_tasks.is_empty() {
        let detail = execution_error
            .as_deref()
            .map(|err| format!("; dagrs_error={err}"))
            .unwrap_or_default();
        return Err(OrchestratorError::AgentError(format!(
            "dagrs execution ended with incomplete tasks: {}{}",
            incomplete_tasks.join(", "),
            detail
        )));
    }
    if let Some(report) = execution_report
        && matches!(report.status, CompletionStatus::Aborted)
    {
        return Err(OrchestratorError::AgentError(format!(
            "dagrs execution aborted: run_id={}, succeeded={}, failed={}, skipped={}",
            report.run_id, report.node_succeeded, report.node_failed, report.node_skipped
        )));
    }
    Ok(snapshot)
}

fn build_task_prompt(task: &TaskSpec, working_dir: &Path, allowed_tools: &[String]) -> String {
    let mut parts = Vec::new();
    parts.push(format!("## Task\n{}", task.title()));
    parts.push(format!("## Objective\n{}", task.objective));

    if let Some(desc) = task.description() {
        parts.push(format!("## Background\n{}", desc));
    }

    parts.push(format!(
        "## Runtime Workspace\nRepository root: `.`\nInternal absolute path: {}\nAll file access must stay inside this workspace. Treat the internal path as diagnostic only; do not pass it to file tools or shell `cd` commands.",
        working_dir.display()
    ));

    if !allowed_tools.is_empty() {
        parts.push(format!("## Allowed Tools\n{}", allowed_tools.join(", ")));
    }

    parts.push(
        "## Path Rules\nUse `.` or repository-relative paths for read_file, list_dir, grep_files, apply_patch, and shell commands. The runtime already executes tools from the task workspace. Do not use the original --repo path or /var/folders/.../libra-task-worktree-* paths as tool arguments.".to_string(),
    );
    parts.push(format!(
        "## Version Control\nDo not use git for status, diff, add, commit, branch, log, \
             show, or switch operations. Use Libra version-control only; when the \
             run_libra_vcs tool is available, call it for those operations.\n{}\nrun_libra_vcs \
             is not a shell and must not be used for cargo, fmt, clippy, test, build, or \
             arbitrary command execution; deterministic verification commands are owned by \
             gate tasks.",
        run_libra_vcs_tool_guidance()
    ));

    if !task.contract.touch_files.is_empty() {
        parts.push(format!(
            "## Write Contract\nOnly modify these files/patterns:\n{}\nTreat this list as a hard boundary. If the task requires another file, explain the mismatch instead of editing it.",
            task.contract
                .touch_files
                .iter()
                .map(|path| format!("- {path}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !task.contract.touch_symbols.is_empty() {
        parts.push(format!(
            "Symbols: {}",
            task.contract.touch_symbols.join(", ")
        ));
    }

    if !task.contract.touch_apis.is_empty() {
        parts.push(format!("APIs: {}", task.contract.touch_apis.join(", ")));
    }

    if !task.acceptance_criteria().is_empty() {
        parts.push(format!(
            "## Acceptance Criteria\n{}",
            task.acceptance_criteria()
                .iter()
                .map(|criterion| format!("- {}", criterion))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !task.scope_in.is_empty() {
        let heading = match task.kind {
            TaskKind::Analysis => "## Focus Scope",
            _ => "## Write Scope",
        };
        parts.push(format!("{heading}\n{}", task.scope_in.join(", ")));
    }

    if !task.scope_out.is_empty() {
        parts.push(format!("## Forbidden Scope\n{}", task.scope_out.join(", ")));
    }

    if !task.contract.expected_outputs.is_empty() {
        parts.push(format!(
            "## Expected Outputs\n{}",
            task.contract
                .expected_outputs
                .iter()
                .map(|output| format!("- {}", output))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if task.kind == TaskKind::Implementation {
        parts.push(format!(
            "## Completion Requirement\nBefore reporting completion, use apply_patch or an allowed shell command that creates or modifies at least one in-scope project file. If no file change is needed or the write scope is wrong, first use read-only tools or verification commands to gather evidence, then explain the evidence and end your final response with {NO_CHANGES_NEEDED_TOKEN}. Do not use {NO_CHANGES_NEEDED_TOKEN} without tool-based evidence."
        ));
    }

    if matches!(task.kind, TaskKind::Implementation | TaskKind::Analysis) {
        parts.push(
            "## Task Termination\nWhen you have produced enough evidence to report a verdict, call `submit_task_complete` ONCE with:\n\
             - `result`: \"pass\" if every acceptance criterion is verified, \"fail\" if any criterion failed or the task is blocked, or \"no_changes_needed\" if the workspace already satisfies the criteria.\n\
             - `summary`: one paragraph describing what changed (or why nothing was needed) and which evidence supports `result`.\n\
             - `evidence`: command + exit_code (+ optional output_excerpt) for each verification command you ran.\n\
             The tool loop ends as soon as `submit_task_complete` succeeds. Do NOT re-run a shell command that you already executed in this task — read the prior tool result from history instead."
                .to_string(),
        );
    }

    if !task.constraints().is_empty() {
        parts.push(format!(
            "## Constraints\n{}",
            task.constraints()
                .iter()
                .map(|constraint| format!("- {}", constraint))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if task
        .constraints()
        .iter()
        .any(|constraint| constraint == "dependency-policy:no-new")
    {
        parts.push(
            "## Dependency Policy\nDo not add new third-party dependencies or edit dependency manifests to introduce new crates/packages. Prefer the standard library or dependencies already present in the project. If the requested task truly requires a new dependency, report the policy mismatch instead of adding it."
                .to_string(),
        );
    }

    if task
        .constraints()
        .iter()
        .any(|constraint| constraint == "dependency-policy:allow-with-review")
    {
        parts.push(
            "## Dependency Policy\nNew third-party dependencies are allowed only when the user explicitly requested that dependency. If you add one, list each dependency name in your summary and include verification evidence commands that exercised the updated dependency graph."
                .to_string(),
        );
    }

    if task.kind == TaskKind::Analysis {
        parts.push(
            "## Analysis Mode\nDo not modify repository files. Use read-only exploration and, if shell is allowed, only non-mutating inspection or verification commands."
                .to_string(),
        );
    }

    parts.join("\n\n")
}

fn build_reviewer_prompt(
    task: &TaskSpec,
    agent_output: &str,
    tool_calls: &[ToolCallRecord],
    working_dir: &Path,
    allowed_tools: &[String],
) -> String {
    let raw_touched: std::collections::BTreeSet<String> = tool_calls
        .iter()
        .flat_map(|call| call.paths_written.iter().cloned())
        .collect();
    // Auto-generated companion files (e.g. Cargo.lock that pairs with an
    // in-contract Cargo.toml) are accepted by sync-back; surfacing them to the
    // reviewer caused spurious rejections that triggered endless replans.
    let companion_scope: Vec<String> = task
        .contract
        .touch_files
        .iter()
        .chain(task.scope_in.iter())
        .cloned()
        .collect();
    let (touched_files, generated_companions): (Vec<String>, Vec<String>) = raw_touched
        .into_iter()
        .partition(|path| !cargo_lock_companion_allowed(&companion_scope, path));

    let mut parts = vec![
        format!("## Review Task\n{}", task.title()),
        format!("## Objective\n{}", task.objective),
        format!(
            "## Runtime Workspace\nRepository root: `.`\nInternal absolute path: {}\nAll file access must stay inside this workspace. Treat the internal path as diagnostic only; do not pass it to file tools or shell `cd` commands.",
            working_dir.display()
        ),
        format!("## Allowed Tools\n{}", allowed_tools.join(", ")),
        "## Path Rules\nUse `.` or repository-relative paths. The runtime resolves file tools and shell commands from the task workspace; do not use the original --repo path or /var/folders/.../libra-task-worktree-* paths.".to_string(),
        format!("## Candidate Output\n{}", agent_output.trim()),
        "Return JSON only in this exact shape: {\"approved\":true|false,\"summary\":\"...\",\"issues\":[\"...\"]}".to_string(),
    ];

    if !touched_files.is_empty() {
        parts.push(format!("## Touched Files\n{}", touched_files.join(", ")));
    }

    if !generated_companions.is_empty() {
        parts.push(format!(
            "## Auto-Generated Companions\n{}\nThese were produced as side effects of in-contract files (e.g. Cargo.lock for an in-contract Cargo.toml). The runtime accepts them; do not treat them as contract violations.",
            generated_companions.join(", ")
        ));
    }

    let dependency_manifest_changes = dependency_manifest_changes(tool_calls);
    if !dependency_manifest_changes.is_empty()
        || task
            .constraints()
            .iter()
            .any(|constraint| constraint == "dependency-policy:allow-with-review")
    {
        let changed_manifests = if dependency_manifest_changes.is_empty() {
            "None detected in tool diffs".to_string()
        } else {
            dependency_manifest_changes.join(", ")
        };
        parts.push(format!(
            "## Dependency Change Review\nChanged manifests: {changed_manifests}\nIf new dependencies were added, approve only when they match the user's explicit intent. The candidate summary or evidence must name the added dependencies and include verification commands that ran after the dependency change."
        ));
    }

    if !task.contract.touch_files.is_empty() {
        parts.push(format!(
            "## Write Contract\nOnly these paths may be modified:\n{}\nReject the candidate only if Touched Files contains a path outside this list. Auto-generated companions listed above do not count as out-of-contract writes.",
            task.contract
                .touch_files
                .iter()
                .map(|path| format!("- {path}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !task.acceptance_criteria().is_empty() {
        parts.push(format!(
            "## Acceptance Criteria\n{}",
            task.acceptance_criteria().join("\n")
        ));
    }

    parts.join("\n\n")
}

fn dependency_manifest_changes(tool_calls: &[ToolCallRecord]) -> Vec<String> {
    let mut manifests = tool_calls
        .iter()
        .flat_map(|call| call.diffs.iter())
        .filter(|diff| {
            Path::new(&diff.path)
                .file_name()
                .is_some_and(|name| name == "Cargo.toml")
        })
        .map(|diff| diff.path.clone())
        .collect::<Vec<_>>();
    manifests.sort();
    manifests.dedup();
    manifests
}

fn allowed_tools_for_task(spec: &IntentSpec, task: &TaskSpec) -> Vec<String> {
    let mut tools = Vec::new();

    if acl_allows(&spec.security.tool_acl, "workspace.fs", "read") {
        tools.extend([
            "read_file".to_string(),
            "list_dir".to_string(),
            "grep_files".to_string(),
            "search_files".to_string(),
        ]);
    }

    if task.kind == TaskKind::Implementation
        && acl_allows(&spec.security.tool_acl, "workspace.fs", "write")
    {
        tools.push("apply_patch".to_string());
    }

    if matches!(task.kind, TaskKind::Implementation | TaskKind::Analysis)
        && (acl_allows(&spec.security.tool_acl, "libra.vcs", "read")
            || acl_allows(&spec.security.tool_acl, "libra.vcs", "write"))
    {
        tools.push("run_libra_vcs".to_string());
    }

    if matches!(task.kind, TaskKind::Implementation | TaskKind::Analysis)
        && acl_allows(&spec.security.tool_acl, "shell", "execute")
    {
        tools.push("shell".to_string());
    }

    if matches!(task.kind, TaskKind::Implementation | TaskKind::Analysis)
        && spec.constraints.security.network_policy == NetworkPolicy::Allow
        && acl_allows(&spec.security.tool_acl, "web.search", "query")
    {
        tools.push("web_search".to_string());
    }

    // `submit_task_complete` is the agent's terminal handshake — required for
    // every Implementation/Analysis task so the tool loop can converge
    // deterministically. Exposed to the model regardless of IntentSpec ACL; the
    // matching ACL bypass lives in policy::terminal_handshake_allowance, so an
    // explicit deny rule still wins but the default "no allow rule" verdict
    // does not deadlock the loop.
    if matches!(task.kind, TaskKind::Implementation | TaskKind::Analysis) {
        tools.push("submit_task_complete".to_string());
    }

    tools
}

fn allowed_tools_for_reviewer(spec: &IntentSpec) -> Vec<String> {
    let mut tools = if acl_allows(&spec.security.tool_acl, "workspace.fs", "read") {
        vec![
            "read_file".to_string(),
            "list_dir".to_string(),
            "grep_files".to_string(),
            "search_files".to_string(),
        ]
    } else {
        Vec::new()
    };
    if spec.constraints.security.network_policy == NetworkPolicy::Allow
        && acl_allows(&spec.security.tool_acl, "web.search", "query")
    {
        tools.push("web_search".to_string());
    }
    tools
}

fn acl_allows(acl: &ToolAcl, tool: &str, action: &str) -> bool {
    matches!(check_tool_acl(acl, tool, action), AclVerdict::Allow)
}

fn parse_reviewer_decision(raw: &str) -> Result<ReviewerDecision, String> {
    if let Ok(parsed) = serde_json::from_str::<ReviewerDecision>(raw.trim()) {
        return Ok(parsed);
    }

    let mut last_error = None;
    for candidate in fenced_json_blocks(raw).chain(json_object_candidates(raw)) {
        match parse_reviewer_decision_prefix(candidate) {
            Ok(parsed) => return Ok(parsed),
            Err(error) => last_error = Some(error),
        }
    }

    match last_error {
        Some(error) => Err(format!("invalid reviewer JSON: {error}")),
        None => Err("reviewer response missing JSON object".to_string()),
    }
}

fn fenced_json_blocks(raw: &str) -> impl Iterator<Item = &str> {
    struct Blocks<'a> {
        raw: &'a str,
        offset: usize,
    }

    impl<'a> Iterator for Blocks<'a> {
        type Item = &'a str;

        fn next(&mut self) -> Option<Self::Item> {
            while let Some(relative_start) = self.raw[self.offset..].find("```") {
                let fence_start = self.offset + relative_start;
                let header_start = fence_start + 3;
                let Some(relative_header_end) = self.raw[header_start..].find('\n') else {
                    self.offset = header_start;
                    continue;
                };
                let header_end = header_start + relative_header_end;
                let header = self.raw[header_start..header_end].trim();
                let body_start = header_end + 1;
                let Some(relative_end) = self.raw[body_start..].find("```") else {
                    self.offset = body_start;
                    continue;
                };
                let body_end = body_start + relative_end;
                self.offset = body_end + 3;
                if header.is_empty() || header.eq_ignore_ascii_case("json") {
                    return Some(self.raw[body_start..body_end].trim());
                }
            }
            None
        }
    }

    Blocks { raw, offset: 0 }
}

fn json_object_candidates(raw: &str) -> impl Iterator<Item = &str> {
    const MAX_JSON_OBJECT_CANDIDATES: usize = 16;

    raw.char_indices()
        .filter(|(_, ch)| *ch == '{')
        .map(|(index, _)| &raw[index..])
        .take(MAX_JSON_OBJECT_CANDIDATES)
}

fn parse_reviewer_decision_prefix(raw: &str) -> Result<ReviewerDecision, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.trim_start());
    ReviewerDecision::deserialize(&mut deserializer)
}

fn runtime_context_for_task(
    spec: &IntentSpec,
    task: &TaskSpec,
    working_dir: &Path,
    inherited_runtime: Option<&ToolRuntimeContext>,
) -> ToolRuntimeContext {
    let network_access = NetworkAccess::from_legacy_bool(matches!(
        spec.constraints.security.network_policy,
        NetworkPolicy::Allow
    ));
    let writable_roots = collect_writable_roots(spec, task, working_dir);
    let policy = SandboxPolicy::WorkspaceWrite {
        writable_roots,
        network_access,
        exclude_tmpdir_env_var: false,
        exclude_slash_tmp: false,
        allow_metadata_writes: false,
    };
    ToolRuntimeContext {
        sandbox: Some(ToolSandboxContext {
            policy,
            permissions: SandboxPermissions::UseDefault,
        }),
        sandbox_runtime: inherited_runtime.and_then(|ctx| ctx.sandbox_runtime.clone()),
        approval: inherited_runtime.and_then(|ctx| ctx.approval.clone()),
        file_history: inherited_runtime.and_then(|ctx| ctx.file_history.clone()),
        max_output_bytes: max_output_limit(&spec.security.tool_acl, "shell", "execute"),
    }
}

fn runtime_context_for_gate_task(
    spec: &IntentSpec,
    working_dir: &Path,
    inherited_runtime: Option<&ToolRuntimeContext>,
) -> ToolRuntimeContext {
    ToolRuntimeContext {
        sandbox: Some(ToolSandboxContext {
            policy: SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![working_dir.to_path_buf()],
                network_access: NetworkAccess::from_legacy_bool(matches!(
                    spec.constraints.security.network_policy,
                    NetworkPolicy::Allow
                )),
                exclude_tmpdir_env_var: false,
                exclude_slash_tmp: false,
                allow_metadata_writes: false,
            },
            permissions: SandboxPermissions::UseDefault,
        }),
        sandbox_runtime: inherited_runtime.and_then(|ctx| ctx.sandbox_runtime.clone()),
        approval: inherited_runtime.and_then(|ctx| ctx.approval.clone()),
        file_history: inherited_runtime.and_then(|ctx| ctx.file_history.clone()),
        max_output_bytes: max_output_limit(&spec.security.tool_acl, "shell", "execute"),
    }
}

fn runtime_context_for_reviewer(
    spec: &IntentSpec,
    inherited_runtime: Option<&ToolRuntimeContext>,
) -> ToolRuntimeContext {
    let policy = if matches!(
        spec.constraints.security.network_policy,
        NetworkPolicy::Allow
    ) {
        SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Full,
        }
    } else {
        SandboxPolicy::ReadOnly
    };
    ToolRuntimeContext {
        sandbox: Some(ToolSandboxContext {
            policy,
            permissions: SandboxPermissions::UseDefault,
        }),
        sandbox_runtime: inherited_runtime.and_then(|ctx| ctx.sandbox_runtime.clone()),
        approval: inherited_runtime.and_then(|ctx| ctx.approval.clone()),
        file_history: inherited_runtime.and_then(|ctx| ctx.file_history.clone()),
        max_output_bytes: max_output_limit(&spec.security.tool_acl, "workspace.fs", "read"),
    }
}

fn collect_writable_roots(spec: &IntentSpec, task: &TaskSpec, working_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::<PathBuf>::new();

    for touch_file in &task.contract.touch_files {
        if let Some(resolved) = resolve_writable_root_candidate(touch_file, working_dir, true) {
            push_unique_root(&mut roots, resolved);
        }
    }

    if roots.is_empty() {
        for scope in task.scope_in.iter().chain(task.contract.write_scope.iter()) {
            if let Some(resolved) = resolve_writable_root_candidate(scope, working_dir, false) {
                push_unique_root(&mut roots, resolved);
            }
        }
    }

    if roots.is_empty() {
        for rule in &spec.security.tool_acl.allow {
            let writes_allowed = rule
                .actions
                .iter()
                .any(|action| action == "*" || action == "write");
            if !writes_allowed {
                continue;
            }
            if let Some(serde_json::Value::Array(write_roots)) = rule.constraints.get("writeRoots")
            {
                for root in write_roots.iter().filter_map(serde_json::Value::as_str) {
                    if let Some(resolved) =
                        resolve_writable_root_candidate(root, working_dir, false)
                    {
                        push_unique_root(&mut roots, resolved);
                    }
                }
            }
        }
    }

    if roots.is_empty() {
        push_unique_root(&mut roots, working_dir.to_path_buf());
    }

    roots
}

fn resolve_writable_root_candidate(
    raw: &str,
    working_dir: &Path,
    prefer_parent_for_missing_leaf: bool,
) -> Option<PathBuf> {
    let normalized = raw.trim().replace('\\', "/");
    let trimmed = normalized.trim_start_matches("./");
    if trimmed.is_empty() || matches!(trimmed, "*" | "**") {
        return None;
    }

    let literal_or_root = trimmed
        .find(['*', '?', '[', '{'])
        .map(|index| trimmed[..index].trim_end_matches('/').to_string())
        .unwrap_or_else(|| trimmed.trim_end_matches('/').to_string());

    if literal_or_root.is_empty() {
        return None;
    }

    let resolved = if Path::new(&literal_or_root).is_absolute() {
        PathBuf::from(&literal_or_root)
    } else {
        working_dir.join(&literal_or_root)
    };

    if prefer_parent_for_missing_leaf && !resolved.exists() {
        return resolved.parent().map(Path::to_path_buf).or(Some(resolved));
    }

    Some(resolved)
}

fn push_unique_root(roots: &mut Vec<PathBuf>, root: PathBuf) {
    let normalized = root.canonicalize().unwrap_or(root);
    if roots.iter().any(|existing| existing == &normalized) {
        return;
    }
    roots.push(normalized);
}

fn max_output_limit(acl: &ToolAcl, tool_name: &str, action: &str) -> Option<usize> {
    acl.allow
        .iter()
        .filter(|rule| {
            rule.tool == "*" || rule.tool == tool_name || matches_tool_glob(&rule.tool, tool_name)
        })
        .filter(|rule| {
            rule.actions
                .iter()
                .any(|rule_action| rule_action == "*" || rule_action == action)
        })
        .filter_map(|rule| rule.constraints.get("maxOutputBytes"))
        .filter_map(serde_json::Value::as_u64)
        .map(|value| value as usize)
        .min()
}

fn matches_tool_glob(pattern: &str, tool_name: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return tool_name.starts_with(prefix)
            && tool_name
                .get(prefix.len()..)
                .is_some_and(|rest| rest.starts_with('.'));
    }
    false
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use git_internal::internal::object::{task::Task as GitTask, types::ActorRef};
    use serial_test::serial;

    use super::*;
    use crate::{
        internal::ai::{
            completion::{
                CompletionError, CompletionRequest, CompletionResponse,
                message::{AssistantContent, Function, Message, Text, ToolCall, UserContent},
            },
            intentspec::{profiles, types::*},
            orchestrator::types::{
                ExecutionPlanSpec, GateResult, TaskContract, TaskKind, TaskSpec, ToolDiffRecord,
            },
            tools::{
                ToolError, ToolHandler, ToolInvocation, ToolKind, ToolOutput, ToolSpec,
                handlers::{
                    ApplyPatchHandler, ListDirHandler, ShellHandler, SubmitTaskCompleteHandler,
                },
                registry::ToolRegistry,
            },
        },
        utils::test,
    };

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: callers are serialized with `#[serial(sandbox_env)]`.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: callers are serialized with `#[serial(sandbox_env)]`.
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: callers are serialized with `#[serial(sandbox_env)]`.
            unsafe {
                if let Some(previous) = &self.previous {
                    std::env::set_var(self.key, previous);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn force_no_ambient_bwrap() -> (EnvVarGuard, EnvVarGuard) {
        (
            EnvVarGuard::unset("LIBRA_LINUX_SANDBOX_EXE"),
            EnvVarGuard::set("LIBRA_BWRAP_BINARY", "/tmp/libra-never-exists"),
        )
    }

    #[derive(Clone)]
    struct MockModel {
        final_text: String,
    }

    impl CompletionModel for MockModel {
        type Response = ();

        fn completion(
            &self,
            _request: CompletionRequest,
        ) -> impl std::future::Future<
            Output = Result<CompletionResponse<Self::Response>, CompletionError>,
        > + Send {
            let text = self.final_text.clone();
            async move {
                Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text { text })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }
    }

    fn shell_record(command: &str, exit_code: i32) -> ToolCallRecord {
        ToolCallRecord {
            tool_name: "shell".into(),
            action: "execute".into(),
            arguments_json: Some(serde_json::json!({ "command": command })),
            success: exit_code == 0,
            summary: Some(format!("Exit code: {exit_code}")),
            ..ToolCallRecord::default()
        }
    }

    fn submit_task_complete_record(command: &str, exit_code: i32) -> ToolCallRecord {
        ToolCallRecord {
            tool_name: "submit_task_complete".into(),
            action: "execute".into(),
            arguments_json: Some(serde_json::json!({
                "result": "pass",
                "summary": "verification passed",
                "evidence": [{
                    "command": command,
                    "exit_code": exit_code,
                    "output_excerpt": "ok"
                }]
            })),
            success: true,
            summary: Some("Task complete: pass (1 evidence entries)".into()),
            ..ToolCallRecord::default()
        }
    }

    #[test]
    fn submit_task_complete_evidence_matches_prior_shell_history() {
        let tool_calls = vec![
            shell_record("cargo test", 0),
            submit_task_complete_record("cargo test", 0),
        ];

        assert!(submit_task_complete_evidence_mismatch(&tool_calls).is_none());
    }

    #[test]
    fn submit_task_complete_evidence_rejects_unobserved_success_claim() {
        let tool_calls = vec![
            shell_record("cargo test", 101),
            submit_task_complete_record("cargo test", 0),
        ];

        let reason = submit_task_complete_evidence_mismatch(&tool_calls)
            .expect("mismatched evidence must be rejected");

        assert!(reason.contains("claimed exit 0"));
        assert!(reason.contains("observed exit codes: [101]"));
    }

    #[derive(Clone)]
    struct ConditionalModel;

    impl CompletionModel for ConditionalModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let prompt = request
                .chat_history
                .iter()
                .rev()
                .find_map(|message| match message {
                    Message::User { content } => content.iter().find_map(|item| match item {
                        UserContent::Text(text) => Some(text.text.clone()),
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or_default();
            let has_tool_result = request.chat_history.iter().any(|message| match message {
                Message::User { content } => content
                    .iter()
                    .any(|item| matches!(item, UserContent::ToolResult(_))),
                _ => false,
            });

            if prompt.contains("## Task\nB") {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }

            if prompt.contains("Fail first") {
                Err(CompletionError::ResponseError("intentional failure".into()))
            } else if !has_tool_result && prompt.contains("apply_patch") {
                Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(add_file_patch_call(
                        &prompt,
                        "conditional",
                    ))],
                    reasoning_content: None,
                    raw_response: (),
                })
            } else {
                Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text {
                        text: "done".into(),
                    })],
                    reasoning_content: None,
                    raw_response: (),
                })
            }
        }
    }

    #[derive(Clone)]
    struct AddFilePatchModel;

    impl CompletionModel for AddFilePatchModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let has_tool_result = request.chat_history.iter().any(|message| match message {
                Message::User { content } => content
                    .iter()
                    .any(|item| matches!(item, UserContent::ToolResult(_))),
                _ => false,
            });
            if has_tool_result {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text {
                        text: "done".to_string(),
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            let prompt = request
                .chat_history
                .iter()
                .rev()
                .find_map(|message| match message {
                    Message::User { content } => content.iter().find_map(|item| match item {
                        UserContent::Text(text) => Some(text.text.clone()),
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or_default();

            Ok(CompletionResponse {
                content: vec![AssistantContent::ToolCall(add_file_patch_call(
                    &prompt, "budget",
                ))],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    fn add_file_patch_call(prompt: &str, prefix: &str) -> ToolCall {
        let slug = prompt
            .split("## Task\n")
            .nth(1)
            .and_then(|tail| tail.lines().next())
            .map(slugify_task_title)
            .filter(|slug| !slug.is_empty())
            .unwrap_or_else(|| "task".to_string());
        let path = match slug.as_str() {
            "do_thing" | "second" => "src/main.rs".to_string(),
            _ => format!("src/{slug}.txt"),
        };
        let patch = format!("*** Begin Patch\n*** Add File: {path}\n+done\n*** End Patch");
        ToolCall {
            id: format!("call_{prefix}_{slug}"),
            name: "apply_patch".to_string(),
            function: Function {
                name: "apply_patch".to_string(),
                arguments: serde_json::json!({ "input": patch }),
            },
        }
    }

    fn slugify_task_title(title: &str) -> String {
        title
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches('_')
            .to_string()
    }

    #[derive(Clone)]
    struct PatchApplyingModel;

    impl CompletionModel for PatchApplyingModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let has_successful_tool_result =
                request.chat_history.iter().any(|message| match message {
                    Message::User { content } => content.iter().any(|item| match item {
                        UserContent::ToolResult(result) => result
                            .result
                            .get("success")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                        _ => false,
                    }),
                    _ => false,
                });
            if has_successful_tool_result {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text {
                        text: "done".to_string(),
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            let prompt = request
                .chat_history
                .iter()
                .filter_map(|message| match message {
                    Message::User { content } => Some(
                        content
                            .iter()
                            .filter_map(|item| match item {
                                UserContent::Text(text) => Some(text.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");

            let is_task_a = prompt.contains("task_a.txt") || prompt.contains("## Task\nTask A");

            let (call_id, patch) = if is_task_a {
                (
                    "call_a",
                    "*** Begin Patch\n*** Update File: task_a.txt\n@@\n-base\n+task-a\n*** End Patch",
                )
            } else {
                (
                    "call_b",
                    "*** Begin Patch\n*** Update File: task_b.txt\n@@\n-base\n+task-b\n*** End Patch",
                )
            };

            Ok(CompletionResponse {
                content: vec![AssistantContent::ToolCall(ToolCall {
                    id: call_id.to_string(),
                    name: "apply_patch".to_string(),
                    function: Function {
                        name: "apply_patch".to_string(),
                        arguments: serde_json::json!({ "input": patch }),
                    },
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    #[derive(Clone)]
    struct PatchWithStaleCargoLockModel;

    impl CompletionModel for PatchWithStaleCargoLockModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let has_successful_tool_result =
                request.chat_history.iter().any(|message| match message {
                    Message::User { content } => content.iter().any(|item| match item {
                        UserContent::ToolResult(result) => result
                            .result
                            .get("success")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                        _ => false,
                    }),
                    _ => false,
                });
            if has_successful_tool_result {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text {
                        text: "done".to_string(),
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            let prompt = request
                .chat_history
                .iter()
                .filter_map(|message| match message {
                    Message::User { content } => Some(
                        content
                            .iter()
                            .filter_map(|item| match item {
                                UserContent::Text(text) => Some(text.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let is_task_a = prompt.contains("task_a.txt") || prompt.contains("## Task\nTask A");
            let (call_id, file_path, replacement, lock_text) = if is_task_a {
                ("call_stale_a", "task_a.txt", "task-a", "# stale a")
            } else {
                ("call_stale_b", "task_b.txt", "task-b", "# stale b")
            };
            let patch = format!(
                "*** Begin Patch\n*** Update File: {file_path}\n@@\n-base\n+{replacement}\n*** Update File: Cargo.lock\n@@\n-# base lock\n+{lock_text}\n*** End Patch"
            );

            Ok(CompletionResponse {
                content: vec![AssistantContent::ToolCall(ToolCall {
                    id: call_id.to_string(),
                    name: "apply_patch".to_string(),
                    function: Function {
                        name: "apply_patch".to_string(),
                        arguments: serde_json::json!({ "input": patch }),
                    },
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    #[derive(Clone)]
    struct SrcMainPatchModel;

    impl CompletionModel for SrcMainPatchModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let has_tool_result = request.chat_history.iter().any(|message| match message {
                Message::User { content } => content
                    .iter()
                    .any(|item| matches!(item, UserContent::ToolResult(_))),
                _ => false,
            });
            if has_tool_result {
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
                    id: "call_src_main".to_string(),
                    name: "apply_patch".to_string(),
                    function: Function {
                        name: "apply_patch".to_string(),
                        arguments: serde_json::json!({
                            "input": "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-fn main() {}\n+fn main() { println!(\"hello\"); }\n*** End Patch"
                        }),
                    },
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    #[derive(Clone)]
    struct ReviewerLoopLimitModel;

    impl CompletionModel for ReviewerLoopLimitModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let prompt = request
                .chat_history
                .iter()
                .rev()
                .find_map(|message| match message {
                    Message::User { content } => content.iter().find_map(|item| match item {
                        UserContent::Text(text) => Some(text.text.clone()),
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or_default();
            if prompt.contains("## Review Task") {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_review_list".to_string(),
                        name: "list_dir".to_string(),
                        function: Function {
                            name: "list_dir".to_string(),
                            arguments: serde_json::json!({ "dir_path": "." }),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            let has_tool_result = request.chat_history.iter().any(|message| match message {
                Message::User { content } => content
                    .iter()
                    .any(|item| matches!(item, UserContent::ToolResult(_))),
                _ => false,
            });
            if has_tool_result {
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
                    id: "call_src_main".to_string(),
                    name: "apply_patch".to_string(),
                    function: Function {
                        name: "apply_patch".to_string(),
                        arguments: serde_json::json!({
                            "input": "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-fn main() {}\n+fn main() { println!(\"hello\"); }\n*** End Patch"
                        }),
                    },
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    #[derive(Clone)]
    struct ImplementationTurnLimitModel {
        tool_turns_before_final: usize,
    }

    impl CompletionModel for ImplementationTurnLimitModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let tool_result_count = request
                .chat_history
                .iter()
                .filter_map(|message| match message {
                    Message::User { content } => Some(
                        content
                            .iter()
                            .filter(|item| matches!(item, UserContent::ToolResult(_)))
                            .count(),
                    ),
                    _ => None,
                })
                .sum::<usize>();

            if tool_result_count >= self.tool_turns_before_final {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text {
                        text: "done".to_string(),
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            let (id, name, arguments) = if tool_result_count == 0 {
                (
                    "call_turn_limit_patch".to_string(),
                    "apply_patch".to_string(),
                    serde_json::json!({
                        "input": "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-fn main() {}\n+fn main() { println!(\"turn limit\"); }\n*** End Patch"
                    }),
                )
            } else {
                let dir_name = format!("probe_{tool_result_count}");
                (
                    format!("call_turn_limit_list_{tool_result_count}"),
                    "list_dir".to_string(),
                    serde_json::json!({ "dir_path": dir_name }),
                )
            };

            Ok(CompletionResponse {
                content: vec![AssistantContent::ToolCall(ToolCall {
                    id,
                    name: name.clone(),
                    function: Function { name, arguments },
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    #[derive(Clone)]
    struct RetryAfterNoWriteModel;

    impl CompletionModel for RetryAfterNoWriteModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let has_tool_result = request.chat_history.iter().any(|message| match message {
                Message::User { content } => content
                    .iter()
                    .any(|item| matches!(item, UserContent::ToolResult(_))),
                _ => false,
            });
            if has_tool_result {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text {
                        text: "done".to_string(),
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            let prompt = request
                .chat_history
                .iter()
                .rev()
                .find_map(|message| match message {
                    Message::User { content } => content.iter().find_map(|item| match item {
                        UserContent::Text(text) => Some(text.text.clone()),
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or_default();

            if prompt.contains("## Previous Attempt Failure") {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_retry_patch".to_string(),
                        name: "apply_patch".to_string(),
                        function: Function {
                            name: "apply_patch".to_string(),
                            arguments: serde_json::json!({
                                "input": "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-fn main() {}\n+fn main() { println!(\"fixed\"); }\n*** End Patch"
                            }),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            Ok(CompletionResponse {
                content: vec![AssistantContent::Text(Text {
                    text: "done without edits".to_string(),
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    #[derive(Clone)]
    struct PatchThenFailModel;

    impl CompletionModel for PatchThenFailModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let has_tool_result = request.chat_history.iter().any(|message| match message {
                Message::User { content } => content
                    .iter()
                    .any(|item| matches!(item, UserContent::ToolResult(_))),
                _ => false,
            });
            if has_tool_result {
                return Err(CompletionError::ResponseError(
                    "intentional failure after patch".to_string(),
                ));
            }

            Ok(CompletionResponse {
                content: vec![AssistantContent::ToolCall(ToolCall {
                    id: "call_fail".to_string(),
                    name: "apply_patch".to_string(),
                    function: Function {
                        name: "apply_patch".to_string(),
                        arguments: serde_json::json!({
                            "input": "*** Begin Patch\n*** Update File: task_a.txt\n@@\n-base\n+task-a\n*** End Patch"
                        }),
                    },
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    #[derive(Clone)]
    struct PatchThenSubmitFailModel;

    impl CompletionModel for PatchThenSubmitFailModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let prompt = request
                .chat_history
                .iter()
                .filter_map(|message| match message {
                    Message::User { content } => Some(
                        content
                            .iter()
                            .filter_map(|item| match item {
                                UserContent::Text(text) => Some(text.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if prompt.contains("## Review Task") {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text {
                        text: r#"{"approved":true,"summary":"ok","issues":[]}"#.into(),
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            let saw_apply_patch_result = request.chat_history.iter().any(|message| match message {
                Message::User { content } => content.iter().any(|item| match item {
                    UserContent::ToolResult(result) => result.name == "apply_patch",
                    _ => false,
                }),
                _ => false,
            });
            if saw_apply_patch_result {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_submit_fail".to_string(),
                        name: "submit_task_complete".to_string(),
                        function: Function {
                            name: "submit_task_complete".to_string(),
                            arguments: serde_json::json!({
                                "result": "fail",
                                "summary": "acceptance blocked by verification failure",
                                "evidence": [
                                    {
                                        "command": "cargo test",
                                        "exit_code": 1,
                                        "output_excerpt": "failed"
                                    }
                                ]
                            }),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            Ok(CompletionResponse {
                content: vec![AssistantContent::ToolCall(ToolCall {
                    id: "call_patch_then_submit_fail".to_string(),
                    name: "apply_patch".to_string(),
                    function: Function {
                        name: "apply_patch".to_string(),
                        arguments: serde_json::json!({
                            "input": "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-fn main() {}\n+fn main() { println!(\"hello\"); }\n*** End Patch"
                        }),
                    },
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    #[derive(Clone)]
    struct FuseFailToolModel;

    impl CompletionModel for FuseFailToolModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let has_tool_result = request.chat_history.iter().any(|message| match message {
                Message::User { content } => content
                    .iter()
                    .any(|item| matches!(item, UserContent::ToolResult(_))),
                _ => false,
            });

            if has_tool_result {
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
                    id: "call_fuse_fail".to_string(),
                    name: "read_file".to_string(),
                    function: Function {
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({ "file_path": "/tmp/workspace/Cargo.toml" }),
                    },
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    struct FuseFailHandler;

    #[async_trait::async_trait]
    impl ToolHandler for FuseFailHandler {
        fn kind(&self) -> ToolKind {
            ToolKind::Function
        }

        async fn handle(&self, _invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
            Err(ToolError::ExecutionFailed(
                "failed to snapshot workspace: IO error for operation on /tmp/workspace: Device not configured (os error 6)"
                    .into(),
            ))
        }

        fn schema(&self) -> ToolSpec {
            ToolSpec::read_file()
        }
    }

    #[derive(Clone)]
    struct CargoInitShellModel;

    impl CompletionModel for CargoInitShellModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let has_tool_result = request.chat_history.iter().any(|message| match message {
                Message::User { content } => content
                    .iter()
                    .any(|item| matches!(item, UserContent::ToolResult(_))),
                _ => false,
            });
            if has_tool_result {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text {
                        text: "created cargo project".to_string(),
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            Ok(CompletionResponse {
                content: vec![AssistantContent::ToolCall(ToolCall {
                    id: "call_cargo_init".to_string(),
                    name: "shell".to_string(),
                    function: Function {
                        name: "shell".to_string(),
                        arguments: serde_json::json!({
                            "command": "cargo init libra --vcs none && cargo build --manifest-path libra/Cargo.toml --target-dir libra/target",
                            "timeout_ms": 120000
                        }),
                    },
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    struct RecordingObserver {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl OrchestratorObserver for RecordingObserver {
        fn on_task_runtime_event(&self, task: &TaskSpec, event: TaskRuntimeEvent) {
            let mut events = self.events.lock().unwrap();
            match event {
                TaskRuntimeEvent::Phase(TaskRuntimePhase::Starting) => {
                    events.push(format!("start:{}", task.title()));
                }
                TaskRuntimeEvent::Phase(TaskRuntimePhase::Completed) => {
                    events.push(format!("done:{}", task.title()));
                }
                TaskRuntimeEvent::Note { text, .. } if text.starts_with("gate running") => {
                    events.push(format!("gate-start:{}:{}", task.title(), text));
                }
                TaskRuntimeEvent::Note { text, .. } if text.starts_with("gate passed") => {
                    events.push(format!("gate-done:{}:pass", task.title()));
                }
                TaskRuntimeEvent::Note { text, .. } if text.starts_with("gate failed") => {
                    events.push(format!("gate-done:{}:fail", task.title()));
                }
                _ => {}
            }
        }
    }

    fn spec() -> Arc<IntentSpec> {
        Arc::new(IntentSpec {
            api_version: "intentspec.io/v1alpha1".into(),
            kind: "IntentSpec".into(),
            metadata: Metadata {
                id: "test".into(),
                created_at: "2025-01-01T00:00:00Z".into(),
                created_by: CreatedBy {
                    creator_type: CreatorType::User,
                    id: "tester".into(),
                    display_name: None,
                },
                target: Target {
                    repo: RepoTarget {
                        repo_type: RepoType::Local,
                        locator: "/tmp".into(),
                    },
                    base_ref: "HEAD".into(),
                    workspace_id: None,
                    labels: BTreeMap::new(),
                },
            },
            intent: Intent {
                summary: "summary".into(),
                problem_statement: "problem".into(),
                change_type: ChangeType::Feature,
                objectives: vec![Objective {
                    title: "do thing".into(),
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
                    network_policy: NetworkPolicy::Allow,
                    dependency_policy: DependencyPolicy::NoNew,
                    crypto_policy: String::new(),
                },
                privacy: ConstraintPrivacy {
                    data_classes_allowed: vec![DataClass::Public],
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
                    max_wall_clock_seconds: 60,
                    max_cost_units: 10,
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
                    allow: vec![ToolRule {
                        tool: "*".into(),
                        actions: vec!["*".into()],
                        constraints: BTreeMap::new(),
                    }],
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
                    encoding_policy: EncodingPolicy::ContextualEscape,
                    no_direct_eval: true,
                },
            },
            execution: ExecutionPolicy {
                retry: RetryPolicy {
                    max_retries: 1,
                    backoff_seconds: 0,
                },
                replan: ReplanPolicy { triggers: vec![] },
                concurrency: ConcurrencyPolicy {
                    max_parallel_tasks: 1,
                },
            },
            artifacts: Artifacts {
                required: vec![],
                retention: ArtifactRetention { days: 30 },
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
            extensions: BTreeMap::new(),
        })
    }

    fn implementation_task() -> TaskSpec {
        let actor = ActorRef::agent("test-executor").unwrap();
        let mut task = GitTask::new(actor, "Do thing", None).unwrap();
        task.set_description(Some("Implement change".into()));
        task.add_constraint("network:allow");
        task.add_acceptance_criterion("tests pass");
        TaskSpec {
            step: git_internal::internal::object::plan::PlanStep::new("Do thing"),
            task,
            objective: "Do thing".into(),
            kind: TaskKind::Implementation,
            gate_stage: None,
            owner_role: Some("coder".into()),
            scope_in: vec!["src/".into()],
            scope_out: vec![],
            checks: vec![],
            contract: TaskContract {
                write_scope: vec!["src/".into()],
                forbidden_scope: vec![],
                touch_files: vec!["src/main.rs".into()],
                touch_symbols: vec![],
                touch_apis: vec![],
                expected_outputs: vec!["tests pass".into()],
            },
        }
    }

    fn analysis_task() -> TaskSpec {
        TaskSpec {
            kind: TaskKind::Analysis,
            owner_role: Some("analyst".into()),
            ..implementation_task()
        }
    }

    #[test]
    fn task_execution_observer_persists_streamed_thinking() {
        let mut observer = TaskExecutionObserver::new(
            spec(),
            implementation_task(),
            PathBuf::from("/tmp/workspace"),
            None,
        );

        observer.on_model_stream_event(&CompletionStreamEvent::ThinkingDelta {
            request_id: None,
            delta: "inspect ".to_string(),
        });
        observer.on_model_stream_event(&CompletionStreamEvent::ThinkingDelta {
            request_id: None,
            delta: "workspace".to_string(),
        });

        let artifacts = observer.finish();

        assert_eq!(artifacts.thinking.as_deref(), Some("inspect workspace"));
    }

    fn scoped_implementation_task(title: &str, file: &str) -> TaskSpec {
        let actor = ActorRef::agent("test-executor").unwrap();
        let mut task = GitTask::new(actor, title, None).unwrap();
        task.set_description(Some(format!("Modify {file}")));
        task.add_constraint("network:allow");
        task.add_acceptance_criterion("tests pass");
        TaskSpec {
            step: git_internal::internal::object::plan::PlanStep::new(title),
            task,
            objective: title.into(),
            kind: TaskKind::Implementation,
            gate_stage: None,
            owner_role: Some("coder".into()),
            scope_in: vec![file.into()],
            scope_out: vec![],
            checks: vec![],
            contract: TaskContract {
                write_scope: vec![file.into()],
                forbidden_scope: vec![],
                touch_files: vec![file.into()],
                touch_symbols: vec![],
                touch_apis: vec![],
                expected_outputs: vec![format!("update {file}")],
            },
        }
    }

    fn plan_for_tasks(tasks: Vec<TaskSpec>, max_parallel: u8) -> ExecutionPlanSpec {
        ExecutionPlanSpec {
            intent_spec_id: "test".into(),
            revision: 1,
            parent_revision: None,
            replan_reason: None,
            tasks,
            max_parallel,
            checkpoints: vec![],
        }
    }

    #[tokio::test]
    #[serial(sandbox_env)]
    async fn test_execute_gate_task() {
        let (_helper_guard, _bwrap_guard) = force_no_ambient_bwrap();
        let task = TaskSpec {
            kind: TaskKind::Gate,
            gate_stage: Some(super::super::types::GateStage::Fast),
            checks: vec![Check {
                id: "ok".into(),
                kind: CheckKind::Command,
                command: Some(":".into()),
                timeout_seconds: Some(10),
                expected_exit_code: Some(0),
                required: true,
                artifacts_produced: vec![],
            }],
            ..implementation_task()
        };
        let dir = tempfile::tempdir().unwrap();
        let result = execute_gate_task(&task, dir.path(), &spec(), None, None).await;
        assert_eq!(result.status, TaskNodeStatus::Completed);
        assert!(result.gate_report.unwrap().all_required_passed);
    }

    #[tokio::test]
    #[serial(sandbox_env)]
    async fn execute_gate_task_reports_failed_check_details() {
        let (_helper_guard, _bwrap_guard) = force_no_ambient_bwrap();
        let task = TaskSpec {
            kind: TaskKind::Gate,
            gate_stage: Some(super::super::types::GateStage::Fast),
            checks: vec![Check {
                id: "fmt".into(),
                kind: CheckKind::Command,
                command: Some("printf 'formatting failed\\n' >&2; exit 1".into()),
                timeout_seconds: Some(10),
                expected_exit_code: Some(0),
                required: true,
                artifacts_produced: vec![],
            }],
            ..implementation_task()
        };
        let dir = tempfile::tempdir().unwrap();
        let result = execute_gate_task(&task, dir.path(), &spec(), None, None).await;

        assert_eq!(result.status, TaskNodeStatus::Failed);
        let agent_output = result.agent_output.unwrap();
        assert!(agent_output.contains("required gate checks failed"));
        assert!(agent_output.contains("fmt"));
        assert!(agent_output.contains("exit 1"));
        assert!(agent_output.contains("formatting failed"));
    }

    #[tokio::test]
    #[serial(sandbox_env)]
    async fn execute_gate_task_runs_cargo_checks_in_manifest_touch_scope() {
        let (_helper_guard, _bwrap_guard) = force_no_ambient_bwrap();
        let dir = tempfile::tempdir().unwrap();
        let linked = dir.path().join("linked");
        std::fs::create_dir_all(linked.join("src")).unwrap();
        std::fs::write(
            linked.join("Cargo.toml"),
            "[package]\nname = \"linked\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(linked.join("src/lib.rs"), "").unwrap();

        let task = TaskSpec {
            kind: TaskKind::Gate,
            gate_stage: Some(super::super::types::GateStage::Fast),
            checks: vec![Check {
                id: "cargo-metadata".into(),
                kind: CheckKind::Command,
                command: Some("cargo metadata --format-version=1 --no-deps >/dev/null".into()),
                timeout_seconds: Some(30),
                expected_exit_code: Some(0),
                required: true,
                artifacts_produced: vec![],
            }],
            scope_in: vec!["linked/".into()],
            contract: TaskContract {
                write_scope: vec!["linked/".into()],
                forbidden_scope: vec![],
                touch_files: vec!["linked/Cargo.toml".into(), "linked/src/lib.rs".into()],
                touch_symbols: vec![],
                touch_apis: vec![],
                expected_outputs: vec!["cargo checks pass".into()],
            },
            ..implementation_task()
        };

        let result = execute_gate_task(&task, dir.path(), &spec(), None, None).await;

        assert_eq!(
            result.status,
            TaskNodeStatus::Completed,
            "{}",
            result.agent_output.as_deref().unwrap_or("")
        );
        assert!(result.gate_report.unwrap().all_required_passed);
    }

    #[tokio::test]
    #[serial(sandbox_env)]
    async fn test_execute_gate_task_emits_check_progress_events() {
        let (_helper_guard, _bwrap_guard) = force_no_ambient_bwrap();
        let task = TaskSpec {
            kind: TaskKind::Gate,
            gate_stage: Some(super::super::types::GateStage::Fast),
            checks: vec![Check {
                id: "fmt".into(),
                kind: CheckKind::Command,
                command: Some(":".into()),
                timeout_seconds: Some(10),
                expected_exit_code: Some(0),
                required: true,
                artifacts_produced: vec![],
            }],
            ..implementation_task()
        };
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let observer: Arc<dyn OrchestratorObserver> = Arc::new(RecordingObserver {
            events: Arc::clone(&events),
        });

        let result = execute_gate_task(&task, dir.path(), &spec(), None, Some(&observer)).await;

        assert_eq!(result.status, TaskNodeStatus::Completed);
        let recorded = events.lock().unwrap();
        assert!(
            recorded
                .iter()
                .any(|event| event.starts_with("gate-start:Do thing:gate running"))
        );
        assert!(recorded.contains(&"gate-done:Do thing:pass".to_string()));
    }

    #[tokio::test]
    #[serial(sandbox_env)]
    async fn test_execute_gate_task_with_default_security() {
        let (_helper_guard, _bwrap_guard) = force_no_ambient_bwrap();
        let task = TaskSpec {
            kind: TaskKind::Gate,
            gate_stage: Some(super::super::types::GateStage::Fast),
            checks: vec![Check {
                id: "verify".into(),
                kind: CheckKind::Command,
                command: Some(":".into()),
                timeout_seconds: Some(10),
                expected_exit_code: Some(0),
                required: true,
                artifacts_produced: vec![],
            }],
            ..implementation_task()
        };
        let dir = tempfile::tempdir().unwrap();
        let mut spec = (*spec()).clone();
        spec.security = profiles::default_security();

        let result = execute_gate_task(&task, dir.path(), &spec, None, None).await;

        assert_eq!(result.status, TaskNodeStatus::Completed);
        assert!(result.gate_report.unwrap().all_required_passed);
    }

    #[tokio::test]
    #[serial(sandbox_env)]
    async fn execute_task_runs_gate_checks_in_isolated_worktree() {
        let (_helper_guard, _bwrap_guard) = force_no_ambient_bwrap();
        let repo = tempfile::tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        std::fs::write(repo.path().join("tracked.txt"), "base\n").unwrap();

        let task = TaskSpec {
            kind: TaskKind::Gate,
            gate_stage: Some(super::super::types::GateStage::Fast),
            checks: vec![Check {
                id: "scratch".into(),
                kind: CheckKind::Command,
                command: Some("printf 'scratch\\n' > gate-output.txt".into()),
                timeout_seconds: Some(10),
                expected_exit_code: Some(0),
                required: true,
                artifacts_produced: vec![],
            }],
            ..implementation_task()
        };
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: repo.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };
        let registry = ToolRegistry::new();
        let model = MockModel {
            final_text: "unused".into(),
        };

        let result = execute_task(&task, &model, &registry, &config).await;

        assert_eq!(result.status, TaskNodeStatus::Completed);
        assert!(!repo.path().join("gate-output.txt").exists());
        assert_eq!(
            std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
            "base\n"
        );
    }

    #[tokio::test]
    async fn test_execute_implementation_task() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut registry = ToolRegistry::with_working_dir(dir.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        let registry = Arc::new(registry);
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 1,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };
        let task = implementation_task();
        let result = execute_task(&task, &SrcMainPatchModel, &registry, &config).await;
        assert_eq!(result.status, TaskNodeStatus::Completed);
        assert_eq!(result.retry_count, 0);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
            "fn main() { println!(\"hello\"); }\n"
        );
    }

    #[tokio::test]
    async fn execute_implementation_task_preserves_output_when_reviewer_hits_turn_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut registry = ToolRegistry::with_working_dir(dir.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        registry.register("list_dir", Arc::new(ListDirHandler));
        let registry = Arc::new(registry);
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig {
                max_turns: Some(3),
                ..ToolLoopConfig::default()
            },
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: Some("review the candidate and return JSON".into()),
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let result = execute_task(
            &implementation_task(),
            &ReviewerLoopLimitModel,
            &registry,
            &config,
        )
        .await;

        assert_eq!(result.status, TaskNodeStatus::Completed);
        assert_eq!(result.agent_output.as_deref(), Some("done"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
            "fn main() { println!(\"hello\"); }\n"
        );
        let review = result
            .review
            .expect("reviewer infrastructure failure should be preserved as a review finding");
        assert!(!review.approved);
        assert!(review.summary.contains("review did not complete"));
        assert!(
            review
                .issues
                .iter()
                .any(|issue| issue.contains("Tool loop exceeded maximum turns"))
        );
    }

    #[test]
    fn default_executor_max_turns_use_task_kind_defaults_and_override() {
        assert_eq!(
            default_executor_max_turns(&implementation_task(), None),
            DEFAULT_IMPLEMENTATION_MAX_TURNS
        );
        assert_eq!(
            default_executor_max_turns(&analysis_task(), None),
            DEFAULT_ANALYSIS_MAX_TURNS
        );
        assert_eq!(
            default_executor_max_turns(&implementation_task(), Some(7)),
            7
        );
    }

    #[tokio::test]
    async fn execute_implementation_task_allows_final_text_on_last_default_turn() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        for index in 1..DEFAULT_IMPLEMENTATION_MAX_TURNS {
            std::fs::create_dir_all(dir.path().join(format!("probe_{index}"))).unwrap();
        }
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut registry = ToolRegistry::with_working_dir(dir.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        registry.register("list_dir", Arc::new(ListDirHandler));
        let registry = Arc::new(registry);
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig {
                repeat_detection_window: Some(DEFAULT_IMPLEMENTATION_MAX_TURNS + 1),
                repeat_warning_threshold: Some(usize::MAX),
                repeat_abort_threshold: Some(usize::MAX),
                ..ToolLoopConfig::default()
            },
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let result = execute_task(
            &implementation_task(),
            &ImplementationTurnLimitModel {
                tool_turns_before_final: DEFAULT_IMPLEMENTATION_MAX_TURNS - 1,
            },
            &registry,
            &config,
        )
        .await;

        assert_eq!(result.status, TaskNodeStatus::Completed);
        assert_eq!(result.agent_output.as_deref(), Some("done"));
        assert_eq!(
            result.tool_calls.len(),
            DEFAULT_IMPLEMENTATION_MAX_TURNS - 1
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
            "fn main() { println!(\"turn limit\"); }\n"
        );
    }

    #[tokio::test]
    async fn execute_implementation_task_reports_default_turn_limit_in_agent_output() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        for index in 1..=DEFAULT_IMPLEMENTATION_MAX_TURNS {
            std::fs::create_dir_all(dir.path().join(format!("probe_{index}"))).unwrap();
        }
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut registry = ToolRegistry::with_working_dir(dir.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        registry.register("list_dir", Arc::new(ListDirHandler));
        let registry = Arc::new(registry);
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig {
                repeat_detection_window: Some(DEFAULT_IMPLEMENTATION_MAX_TURNS + 1),
                repeat_warning_threshold: Some(usize::MAX),
                repeat_abort_threshold: Some(usize::MAX),
                ..ToolLoopConfig::default()
            },
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let result = execute_task(
            &implementation_task(),
            &ImplementationTurnLimitModel {
                tool_turns_before_final: DEFAULT_IMPLEMENTATION_MAX_TURNS + 1,
            },
            &registry,
            &config,
        )
        .await;

        assert_eq!(result.status, TaskNodeStatus::Failed);
        let agent_output = result
            .agent_output
            .as_deref()
            .expect("failed result should include the turn-limit reason");
        assert!(
            agent_output.contains("Tool loop exceeded maximum turns (48)"),
            "unexpected agent_output: {agent_output}"
        );
        assert_eq!(result.tool_calls.len(), DEFAULT_IMPLEMENTATION_MAX_TURNS);
    }

    #[tokio::test]
    async fn execute_implementation_task_requires_file_write() {
        let model = MockModel {
            final_text: "done".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(ToolRegistry::with_working_dir(dir.path().to_path_buf()));
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };
        let task = implementation_task();

        let result = execute_task(&task, &model, &registry, &config).await;

        assert_eq!(result.status, TaskNodeStatus::Failed);
        assert!(
            result
                .agent_output
                .as_deref()
                .is_some_and(|output| output.contains("without writing any files"))
        );
        assert!(result.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn execute_implementation_task_retries_with_missing_write_feedback() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut registry = ToolRegistry::with_working_dir(dir.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        let registry = Arc::new(registry);
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 1,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let result = execute_task(
            &implementation_task(),
            &RetryAfterNoWriteModel,
            &registry,
            &config,
        )
        .await;

        assert_eq!(result.status, TaskNodeStatus::Completed);
        assert_eq!(result.retry_count, 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
            "fn main() { println!(\"fixed\"); }\n"
        );
    }

    #[tokio::test]
    async fn execute_implementation_task_honors_submit_task_complete_fail() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut registry = ToolRegistry::with_working_dir(dir.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        registry.register("submit_task_complete", Arc::new(SubmitTaskCompleteHandler));
        let registry = Arc::new(registry);
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let result = execute_task(
            &implementation_task(),
            &PatchThenSubmitFailModel,
            &registry,
            &config,
        )
        .await;

        assert_eq!(result.status, TaskNodeStatus::Failed);
        assert!(
            result
                .agent_output
                .as_deref()
                .is_some_and(|output| output.contains("acceptance blocked"))
        );
        assert!(
            result
                .tool_calls
                .iter()
                .any(|call| call.tool_name == "apply_patch" && call.success)
        );
        assert!(
            result
                .tool_calls
                .iter()
                .any(|call| call.tool_name == "submit_task_complete" && call.success)
        );
    }

    /// Regression: when the agent submits `submit_task_complete: pass` but the
    /// reviewer rejects the result and retries are exhausted, `agent_output`
    /// must carry the actual `review rejected: …` reason — not echo the
    /// agent's now-disproved success claim. The previous behaviour produced
    /// misleading dagrs error logs of the form
    /// `[DgRun0006] Task complete: pass (N evidence entries)`.
    #[derive(Clone)]
    struct PatchThenSubmitPassModel;

    impl CompletionModel for PatchThenSubmitPassModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            let prompt = request
                .chat_history
                .iter()
                .filter_map(|message| match message {
                    Message::User { content } => Some(
                        content
                            .iter()
                            .filter_map(|item| match item {
                                UserContent::Text(text) => Some(text.text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if prompt.contains("## Review Task") {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::Text(Text {
                        text: r#"{"approved":false,"summary":"missing test for new branch","issues":["no test"]}"#.into(),
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            let saw_apply_patch_result = request.chat_history.iter().any(|message| match message {
                Message::User { content } => content.iter().any(|item| match item {
                    UserContent::ToolResult(result) => result.name == "apply_patch",
                    _ => false,
                }),
                _ => false,
            });
            if saw_apply_patch_result {
                return Ok(CompletionResponse {
                    content: vec![AssistantContent::ToolCall(ToolCall {
                        id: "call_submit_pass".to_string(),
                        name: "submit_task_complete".to_string(),
                        function: Function {
                            name: "submit_task_complete".to_string(),
                            arguments: serde_json::json!({
                                "result": "pass",
                                "summary": "implementation done",
                                "evidence": [
                                    {"command": "cargo test", "exit_code": 0}
                                ]
                            }),
                        },
                    })],
                    reasoning_content: None,
                    raw_response: (),
                });
            }

            Ok(CompletionResponse {
                content: vec![AssistantContent::ToolCall(ToolCall {
                    id: "call_patch_then_submit_pass".to_string(),
                    name: "apply_patch".to_string(),
                    function: Function {
                        name: "apply_patch".to_string(),
                        arguments: serde_json::json!({
                            "input": "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-fn main() {}\n+fn main() { println!(\"hello\"); }\n*** End Patch"
                        }),
                    },
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    #[tokio::test]
    async fn execute_implementation_task_carries_review_rejection_into_agent_output() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut registry = ToolRegistry::with_working_dir(dir.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        registry.register("submit_task_complete", Arc::new(SubmitTaskCompleteHandler));
        let registry = Arc::new(registry);
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: Some("review the candidate and return JSON".into()),
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let result = execute_task(
            &implementation_task(),
            &PatchThenSubmitPassModel,
            &registry,
            &config,
        )
        .await;

        assert_eq!(result.status, TaskNodeStatus::Failed);
        let agent_output = result
            .agent_output
            .as_deref()
            .expect("failed task must carry an agent_output");
        assert!(
            agent_output.starts_with("review rejected:"),
            "agent_output should reflect the rejection reason, got: {agent_output}",
        );
        assert!(
            agent_output.contains("missing test for new branch"),
            "agent_output should include the reviewer summary, got: {agent_output}",
        );
        assert!(
            !agent_output.contains("Task complete: pass"),
            "agent_output must not echo the agent's pre-rejection success claim, got: {agent_output}",
        );
        let review = result
            .review
            .expect("rejected review should be persisted on the result");
        assert!(!review.approved);
    }

    #[tokio::test]
    async fn execute_implementation_task_stops_retrying_on_fuse_tool_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let mut registry = ToolRegistry::with_working_dir(dir.path().to_path_buf());
        registry.register("read_file", Arc::new(FuseFailHandler));
        let registry = Arc::new(registry);
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 3,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let result = execute_task(
            &implementation_task(),
            &FuseFailToolModel,
            &registry,
            &config,
        )
        .await;

        assert_eq!(result.status, TaskNodeStatus::Failed);
        assert_eq!(
            result.retry_count, 0,
            "FUSE infrastructure failures should escape the in-workspace retry loop"
        );
        assert!(
            result
                .agent_output
                .as_deref()
                .is_some_and(|output| output.contains("Device not configured"))
        );
        assert_eq!(result.tool_calls.len(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn execute_dag_fails_noop_implementation_task() {
        let repo = tempfile::tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;

        let registry = Arc::new(ToolRegistry::with_working_dir(repo.path().to_path_buf()));
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: repo.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };
        let plan = plan_for_tasks(vec![implementation_task()], 1);
        let model = MockModel {
            final_text: "done".into(),
        };

        let run_state = execute_dag(&plan, &model, &registry, &config)
            .await
            .unwrap();

        let result = &run_state.ordered_task_results()[0];
        assert_eq!(result.status, TaskNodeStatus::Failed);
        assert!(
            result
                .agent_output
                .as_deref()
                .is_some_and(|output| output.contains("without writing any files"))
        );
        assert!(!repo.path().join("src/main.rs").exists());
    }

    #[tokio::test]
    #[serial]
    async fn execute_dag_replays_parallel_task_worktrees_back_to_main_workspace() {
        let repo = tempfile::tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        std::fs::write(repo.path().join("task_a.txt"), "base\n").unwrap();
        std::fs::write(repo.path().join("task_b.txt"), "base\n").unwrap();

        let mut registry = ToolRegistry::with_working_dir(repo.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        let registry = Arc::new(registry);

        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: repo.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let plan = plan_for_tasks(
            vec![
                scoped_implementation_task("Task A", "task_a.txt"),
                scoped_implementation_task("Task B", "task_b.txt"),
            ],
            2,
        );

        let run_state = execute_dag(&plan, &PatchApplyingModel, &registry, &config)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(repo.path().join("task_a.txt")).unwrap(),
            "task-a\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("task_b.txt")).unwrap(),
            "task-b\n"
        );
        assert!(
            run_state
                .ordered_task_results()
                .iter()
                .all(|result| result.status == TaskNodeStatus::Completed)
        );
    }

    #[tokio::test]
    #[serial]
    async fn execute_dag_skips_parallel_stale_cargo_lock_side_effects() {
        let repo = tempfile::tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        std::fs::write(repo.path().join("Cargo.lock"), "# base lock\n").unwrap();
        std::fs::write(repo.path().join("task_a.txt"), "base\n").unwrap();
        std::fs::write(repo.path().join("task_b.txt"), "base\n").unwrap();

        let mut registry = ToolRegistry::with_working_dir(repo.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        let registry = Arc::new(registry);

        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: repo.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let mut task_a = scoped_implementation_task("Task A", "task_a.txt");
        task_a.contract.touch_files = Vec::new();
        task_a.scope_in = vec![".".into()];
        let mut task_b = scoped_implementation_task("Task B", "task_b.txt");
        task_b.contract.touch_files = Vec::new();
        task_b.scope_in = vec![".".into()];

        let run_state = execute_dag(
            &plan_for_tasks(vec![task_a, task_b], 2),
            &PatchWithStaleCargoLockModel,
            &registry,
            &config,
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(repo.path().join("task_a.txt")).unwrap(),
            "task-a\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("task_b.txt")).unwrap(),
            "task-b\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("Cargo.lock")).unwrap(),
            "# base lock\n"
        );
        assert!(
            run_state
                .ordered_task_results()
                .iter()
                .all(|result| result.status == TaskNodeStatus::Completed)
        );
    }

    #[tokio::test]
    #[serial(sandbox_env)]
    async fn execute_dag_syncs_cargo_project_without_treating_lockfile_or_target_as_scope_creep() {
        let (_helper_guard, _bwrap_guard) = force_no_ambient_bwrap();
        let repo = tempfile::tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;

        let mut registry = ToolRegistry::with_working_dir(repo.path().to_path_buf());
        registry.register("shell", Arc::new(ShellHandler));
        let registry = Arc::new(registry);

        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: repo.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let mut task = scoped_implementation_task("Initialize Cargo project", "libra/Cargo.toml");
        task.task.set_description(Some(
            "Initialize a Cargo project named libra and verify it builds".into(),
        ));
        task.scope_in = vec!["libra/".into()];
        task.contract.write_scope = vec!["libra/".into()];
        task.contract.touch_files = vec!["libra/Cargo.toml".into(), "libra/src/main.rs".into()];
        task.contract.expected_outputs = vec![
            "libra/Cargo.toml exists".into(),
            "libra/src/main.rs exists".into(),
            "cargo build succeeds".into(),
        ];

        let run_state = execute_dag(
            &plan_for_tasks(vec![task], 1),
            &CargoInitShellModel,
            &registry,
            &config,
        )
        .await
        .unwrap();

        let result = &run_state.ordered_task_results()[0];
        assert_eq!(result.status, TaskNodeStatus::Completed);
        assert!(result.policy_violations.is_empty());
        assert!(repo.path().join("libra/Cargo.toml").exists());
        assert!(repo.path().join("libra/src/main.rs").exists());
        assert!(repo.path().join("libra/Cargo.lock").exists());
        assert!(!repo.path().join("libra/target/.rustc_info.json").exists());
    }

    #[tokio::test]
    #[serial]
    async fn execute_dag_keeps_main_workspace_clean_when_serial_task_fails() {
        let repo = tempfile::tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        std::fs::write(repo.path().join("task_a.txt"), "base\n").unwrap();

        let mut registry = ToolRegistry::with_working_dir(repo.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        let registry = Arc::new(registry);

        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: repo.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let plan = plan_for_tasks(vec![scoped_implementation_task("Task A", "task_a.txt")], 1);

        let run_state = execute_dag(&plan, &PatchThenFailModel, &registry, &config)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(repo.path().join("task_a.txt")).unwrap(),
            "base\n"
        );
        assert_eq!(
            run_state.ordered_task_results()[0].status,
            TaskNodeStatus::Failed
        );
    }

    #[test]
    fn task_prompt_includes_runtime_workspace() {
        let prompt = build_task_prompt(
            &implementation_task(),
            Path::new("/tmp/workspace"),
            &["read_file".into(), "apply_patch".into()],
        );
        assert!(prompt.contains("Repository root: `.`"));
        assert!(prompt.contains("Internal absolute path: /tmp/workspace"));
        assert!(prompt.contains("do not pass it to file tools or shell `cd` commands"));
        assert!(prompt.contains("Allowed Tools"));
    }

    #[test]
    fn task_prompt_marks_touch_files_as_hard_write_contract() {
        let mut task = implementation_task();
        task.task.add_constraint("dependency-policy:no-new");
        let prompt = build_task_prompt(
            &task,
            Path::new("/tmp/workspace"),
            &["read_file".into(), "apply_patch".into()],
        );

        assert!(prompt.contains("## Write Contract"));
        assert!(prompt.contains("Only modify these files"));
        assert!(prompt.contains("## Version Control"));
        assert!(prompt.contains("Do not use git"));
        assert!(prompt.contains("run_libra_vcs"));
        assert!(prompt.contains("Allowed run_libra_vcs commands"));
        assert!(prompt.contains("status --json"));
        assert!(prompt.contains("ls-files"));
        assert!(prompt.contains("status -uall"));
        assert!(prompt.contains("must not be used for cargo"));
        assert!(prompt.contains("verification commands are owned by gate tasks"));
        assert!(prompt.contains("## Completion Requirement"));
        assert!(prompt.contains("Before reporting completion"));
        assert!(prompt.contains(NO_CHANGES_NEEDED_TOKEN));
        assert!(prompt.contains("without tool-based evidence"));
        assert!(prompt.contains("## Dependency Policy"));
        assert!(prompt.contains("Do not add new third-party dependencies"));
        assert!(prompt.contains("src/main.rs"));
    }

    #[test]
    fn task_prompt_explains_allow_with_review_dependency_policy() {
        let mut task = implementation_task();
        task.task
            .add_constraint("dependency-policy:allow-with-review");
        let prompt = build_task_prompt(
            &task,
            Path::new("/tmp/workspace"),
            &["read_file".into(), "apply_patch".into()],
        );

        assert!(prompt.contains("## Dependency Policy"));
        assert!(prompt.contains("only when the user explicitly requested"));
        assert!(prompt.contains("list each dependency name"));
        assert!(prompt.contains("verification evidence commands"));
    }

    #[test]
    fn implementation_task_allows_no_changes_needed_with_evidence() {
        let record = ToolCallRecord {
            tool_name: "read_file".into(),
            action: "read".into(),
            paths_read: vec!["src/main.rs".into()],
            success: true,
            ..ToolCallRecord::default()
        };

        assert_eq!(
            implementation_missing_write_output(
                &implementation_task(),
                &[record],
                &format!("Already correct. {NO_CHANGES_NEEDED_TOKEN}"),
            ),
            None
        );
    }

    #[test]
    fn implementation_task_rejects_no_changes_needed_without_evidence() {
        let reason = implementation_missing_write_output(
            &implementation_task(),
            &[],
            &format!("Already correct. {NO_CHANGES_NEEDED_TOKEN}"),
        )
        .unwrap();

        assert!(reason.contains("without writing any files"));
        assert!(reason.contains(NO_CHANGES_NEEDED_TOKEN));
    }

    #[test]
    fn implementation_task_rejects_no_changes_needed_token_not_at_end() {
        let record = ToolCallRecord {
            tool_name: "read_file".into(),
            action: "read".into(),
            paths_read: vec!["src/main.rs".into()],
            success: true,
            ..ToolCallRecord::default()
        };

        let reason = implementation_missing_write_output(
            &implementation_task(),
            &[record],
            &format!("{NO_CHANGES_NEEDED_TOKEN} but I kept writing"),
        )
        .unwrap();

        assert!(reason.contains("without writing any files"));
    }

    #[test]
    fn implementation_task_accepts_submit_task_complete_no_changes_needed_arg() {
        let submit_call = ToolCallRecord {
            tool_name: "submit_task_complete".into(),
            action: "execute".into(),
            arguments_json: Some(serde_json::json!({
                "result": "no_changes_needed",
                "summary": "workspace already satisfies acceptance criteria",
                "evidence": [],
            })),
            success: true,
            ..ToolCallRecord::default()
        };

        assert_eq!(
            implementation_missing_write_output(&implementation_task(), &[submit_call], ""),
            None
        );
    }

    #[test]
    fn implementation_task_ignores_submit_task_complete_pass_arg() {
        let submit_call = ToolCallRecord {
            tool_name: "submit_task_complete".into(),
            action: "execute".into(),
            arguments_json: Some(serde_json::json!({
                "result": "pass",
                "summary": "wrote nothing but claimed pass",
                "evidence": [],
            })),
            success: true,
            ..ToolCallRecord::default()
        };

        let reason =
            implementation_missing_write_output(&implementation_task(), &[submit_call], "")
                .unwrap();

        assert!(reason.contains("without writing any files"));
    }

    #[test]
    fn implementation_task_ignores_failed_submit_task_complete() {
        let submit_call = ToolCallRecord {
            tool_name: "submit_task_complete".into(),
            action: "execute".into(),
            arguments_json: Some(serde_json::json!({
                "result": "no_changes_needed",
                "summary": "rejected",
                "evidence": [],
            })),
            success: false,
            ..ToolCallRecord::default()
        };

        let reason =
            implementation_missing_write_output(&implementation_task(), &[submit_call], "")
                .unwrap();

        assert!(reason.contains("without writing any files"));
    }

    #[test]
    fn task_completion_allows_only_blocked_shell_escalation_after_terminal_submit() {
        let submit_call = ToolCallRecord {
            tool_name: "submit_task_complete".into(),
            action: "execute".into(),
            arguments_json: Some(serde_json::json!({
                "result": "no_changes_needed",
                "summary": "workspace already satisfies acceptance criteria",
                "evidence": [],
            })),
            success: true,
            ..ToolCallRecord::default()
        };
        let violations = vec![PolicyViolation {
            code: "sandbox-escalation-deny".into(),
            message: "shell escalation is not allowed for orchestrator-managed tasks".into(),
            tool_name: Some("shell".into()),
            path: None,
        }];

        assert!(task_completion_allows_blocked_escalation(
            &violations,
            &[submit_call]
        ));
    }

    #[test]
    fn task_completion_keeps_other_policy_violations_blocking() {
        let submit_call = ToolCallRecord {
            tool_name: "submit_task_complete".into(),
            action: "execute".into(),
            success: true,
            ..ToolCallRecord::default()
        };
        let violations = vec![PolicyViolation {
            code: "scope-creep".into(),
            message: "write escaped declared scope".into(),
            tool_name: Some("apply_patch".into()),
            path: Some("outside.txt".into()),
        }];

        assert!(!task_completion_allows_blocked_escalation(
            &violations,
            &[submit_call]
        ));
    }

    #[test]
    fn task_result_detects_fuse_infrastructure_tool_failure() {
        let result = TaskResult {
            task_id: Uuid::new_v4(),
            status: TaskNodeStatus::Failed,
            gate_report: None,
            agent_output: None,
            retry_count: 0,
            tool_calls: vec![ToolCallRecord {
                success: false,
                summary: Some("Tool 'read_file' failed: Device not configured (os error 6)".into()),
                ..ToolCallRecord::default()
            }],
            policy_violations: Vec::new(),
            model_usage: None,
            review: None,
            thinking: None,
        };

        assert!(
            task_result_fuse_infrastructure_failure(&result)
                .as_deref()
                .is_some_and(|reason| reason.contains("Device not configured"))
        );
    }

    #[test]
    fn task_result_detects_fuse_infrastructure_shell_snapshot_failure() {
        let result = TaskResult {
            task_id: Uuid::new_v4(),
            status: TaskNodeStatus::Completed,
            gate_report: None,
            agent_output: None,
            retry_count: 0,
            tool_calls: vec![ToolCallRecord {
                tool_name: "shell".into(),
                success: false,
                summary: Some(
                    "Tool execution failed: failed to inspect workspace changes after shell command: IO error for operation on /tmp/workspace: Device not configured (os error 6)"
                        .into(),
                ),
                ..ToolCallRecord::default()
            }],
            policy_violations: Vec::new(),
            model_usage: None,
            review: None,
            thinking: None,
        };

        assert!(
            task_result_fuse_infrastructure_failure(&result)
                .as_deref()
                .is_some_and(|reason| reason.contains("failed to inspect workspace changes"))
        );
    }

    #[test]
    fn gate_result_detects_fuse_backend_os_error_22() {
        let result = TaskResult {
            task_id: Uuid::new_v4(),
            status: TaskNodeStatus::Failed,
            gate_report: Some(GateReport {
                results: vec![GateResult {
                    check_id: "cargo-build".into(),
                    kind: "command".into(),
                    passed: false,
                    exit_code: 101,
                    stdout: String::new(),
                    stderr: "error: failed to read source: Invalid argument (os error 22)".into(),
                    duration_ms: 1,
                    timed_out: false,
                }],
                all_required_passed: false,
            }),
            agent_output: None,
            retry_count: 0,
            tool_calls: Vec::new(),
            policy_violations: Vec::new(),
            model_usage: None,
            review: None,
            thinking: None,
        };

        assert!(
            gate_result_fuse_infrastructure_failure(&result)
                .as_deref()
                .is_some_and(|reason| {
                    reason.contains("cargo-build") && reason.contains("os error 22")
                })
        );
    }

    #[test]
    fn fuse_cleanup_failure_reason_detects_os_error_22() {
        let err = io::Error::other("Invalid argument (os error 22)");

        assert!(
            fuse_cleanup_failure_reason(&err)
                .as_deref()
                .is_some_and(|reason| reason.contains("failed to clean up FUSE worktree"))
        );
    }

    #[test]
    fn workspace_contract_failure_detects_writes_outside_touch_files() {
        use crate::internal::ai::workspace_snapshot::snapshot_workspace;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let baseline = snapshot_workspace(dir.path()).unwrap();
        std::fs::write(dir.path().join("untracked.bin"), b"junk").unwrap();

        let mut config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };
        // Without baseline, the precheck is a no-op.
        assert!(workspace_contract_failure(&implementation_task(), &config).is_none());

        config.workspace_baseline = Some(Arc::new(baseline));
        let message =
            workspace_contract_failure(&implementation_task(), &config).expect("violation");
        assert!(message.contains("workspace contract violation"));
        assert!(
            message.contains("untracked.bin"),
            "expected violating path in message: {}",
            message
        );
    }

    #[test]
    fn workspace_contract_failure_accepts_cargo_lock_companion_via_absolute_touch_file() {
        use crate::internal::ai::workspace_snapshot::snapshot_workspace;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let baseline = snapshot_workspace(dir.path()).unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), "# generated\n").unwrap();

        let mut task = implementation_task();
        task.contract.touch_files = vec![
            "/some/abs/Cargo.toml".into(),
            "/some/abs/src/main.rs".into(),
        ];
        task.scope_in = vec![];

        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: Some(Arc::new(baseline)),
            fuse_state: FuseProvisionState::default(),
        };

        assert!(
            workspace_contract_failure(&task, &config).is_none(),
            "Cargo.lock companion should be accepted even when touch_files contain absolute Cargo.toml"
        );
    }

    #[test]
    fn reviewer_prompt_filters_cargo_lock_companion_into_auto_generated_section() {
        let record = ToolCallRecord {
            paths_written: vec!["src/main.rs".into(), "Cargo.lock".into()],
            ..ToolCallRecord::default()
        };
        let mut task = implementation_task();
        task.contract.touch_files = vec!["Cargo.toml".into(), "src/main.rs".into()];
        let prompt = build_reviewer_prompt(
            &task,
            "done",
            &[record],
            Path::new("/tmp/workspace"),
            &["apply_patch".to_string()],
        );

        assert!(prompt.contains("## Touched Files\nsrc/main.rs"));
        assert!(prompt.contains("## Auto-Generated Companions\nCargo.lock"));
        assert!(
            !prompt.contains("## Touched Files\nCargo.lock")
                && !prompt.contains("Cargo.lock, src/main.rs"),
            "Cargo.lock should not appear in the Touched Files section"
        );
    }

    #[test]
    fn reviewer_prompt_includes_dependency_change_review_block() {
        let record = ToolCallRecord {
            diffs: vec![ToolDiffRecord {
                path: "Cargo.toml".into(),
                change_type: "update".into(),
                diff: "@@\n [dependencies]\n+clap = \"4\"\n".into(),
            }],
            ..ToolCallRecord::default()
        };
        let mut task = implementation_task();
        task.task
            .add_constraint("dependency-policy:allow-with-review");

        let prompt = build_reviewer_prompt(
            &task,
            "added clap",
            &[record],
            Path::new("/tmp/workspace"),
            &["read_file".to_string()],
        );

        assert!(prompt.contains("## Dependency Change Review"));
        assert!(prompt.contains("Changed manifests: Cargo.toml"));
        assert!(prompt.contains("match the user's explicit intent"));
        assert!(prompt.contains("name the added dependencies"));
    }

    #[test]
    fn parses_reviewer_json_from_fenced_block_after_markdown_notes() {
        let raw = "\
Review notes:
- Do not parse this pseudo object: {approved: true}

```json
{\"approved\":true,\"summary\":\"clap dependency added\",\"issues\":[]}
```

Done.";

        let decision = parse_reviewer_decision(raw).unwrap();

        assert!(decision.approved);
        assert_eq!(decision.summary, "clap dependency added");
        assert!(decision.issues.is_empty());
    }

    #[test]
    fn parses_reviewer_json_embedded_in_plain_text() {
        let raw = "The change is acceptable. {\"approved\":false,\"summary\":\"needs test\",\"issues\":[\"missing CLI test\"]}";

        let decision = parse_reviewer_decision(raw).unwrap();

        assert!(!decision.approved);
        assert_eq!(decision.summary, "needs test");
        assert_eq!(decision.issues, vec!["missing CLI test"]);
    }

    #[test]
    fn rejects_reviewer_text_without_json_object() {
        let error = parse_reviewer_decision("approved: true").unwrap_err();

        assert!(error.contains("missing JSON object"));
    }

    #[test]
    fn reviewer_prompt_includes_write_contract() {
        let record = ToolCallRecord {
            paths_written: vec!["src/main.rs".into()],
            ..ToolCallRecord::default()
        };
        let prompt = build_reviewer_prompt(
            &implementation_task(),
            "done",
            &[record],
            Path::new("/tmp/workspace"),
            &["read_file".into()],
        );

        assert!(prompt.contains("## Write Contract"));
        assert!(prompt.contains("Only these paths may be modified"));
        assert!(prompt.contains("src/main.rs"));
    }

    #[test]
    fn default_acl_does_not_expose_shell_to_coder() {
        let task = implementation_task();
        let mut spec = (*spec()).clone();
        spec.security = profiles::default_security();
        spec.constraints.security.network_policy = NetworkPolicy::Deny;
        let tools = allowed_tools_for_task(&spec, &task);
        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"apply_patch".to_string()));
        assert!(tools.contains(&"run_libra_vcs".to_string()));
        assert!(!tools.contains(&"shell".to_string()));
        assert!(!tools.contains(&"web_search".to_string()));
    }

    #[test]
    fn shell_acl_exposes_shell_to_coder() {
        let task = implementation_task();
        let mut spec = (*spec()).clone();
        spec.security = profiles::default_security();
        spec.constraints.security.network_policy = NetworkPolicy::Deny;
        spec.security.tool_acl.allow.push(ToolRule {
            tool: "shell".into(),
            actions: vec!["execute".into()],
            constraints: BTreeMap::new(),
        });

        let tools = allowed_tools_for_task(&spec, &task);

        assert!(tools.contains(&"shell".to_string()));
    }

    #[test]
    fn network_allow_exposes_web_search_to_coder() {
        let task = analysis_task();
        let mut spec = (*spec()).clone();
        spec.security = profiles::default_security();
        spec.constraints.security.network_policy = NetworkPolicy::Allow;

        let tools = allowed_tools_for_task(&spec, &task);

        assert!(tools.contains(&"web_search".to_string()));
    }

    #[test]
    fn gate_runtime_uses_workspace_write_sandbox() {
        let runtime = runtime_context_for_gate_task(&spec(), Path::new("/tmp/workspace"), None);
        let sandbox = runtime
            .sandbox
            .expect("gate tasks should always execute with sandbox context");
        assert!(matches!(
            sandbox.policy,
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                ..
            } if writable_roots == vec![PathBuf::from("/tmp/workspace")]
        ));
    }

    #[test]
    fn task_runtime_prefers_touch_files_as_writable_roots() {
        let workspace = tempfile::tempdir().unwrap();
        let src_dir = workspace.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.rs"), "fn main() {}\n").unwrap();
        let expected_root = workspace.path().join("src/main.rs").canonicalize().unwrap();

        let runtime =
            runtime_context_for_task(&spec(), &implementation_task(), workspace.path(), None);
        let sandbox = runtime
            .sandbox
            .expect("implementation tasks should always execute with sandbox context");
        assert!(matches!(
            sandbox.policy,
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                ..
            } if writable_roots == vec![expected_root]
        ));
    }

    #[test]
    fn task_runtime_falls_back_to_scope_roots_when_touch_files_are_absent() {
        let mut task = implementation_task();
        task.contract.touch_files.clear();
        task.scope_in = vec!["src/**/*.rs".into()];
        task.contract.write_scope = vec!["src/**/*.rs".into()];

        let runtime = runtime_context_for_task(&spec(), &task, Path::new("/tmp/workspace"), None);
        let sandbox = runtime
            .sandbox
            .expect("implementation tasks should always execute with sandbox context");
        assert!(matches!(
            sandbox.policy,
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                ..
            } if writable_roots == vec![PathBuf::from("/tmp/workspace/src")]
        ));
    }

    #[test]
    fn analysis_tasks_do_not_get_apply_patch() {
        let task = analysis_task();
        let mut spec = (*spec()).clone();
        spec.security = profiles::default_security();
        let tools = allowed_tools_for_task(&spec, &task);
        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"run_libra_vcs".to_string()));
        assert!(!tools.contains(&"apply_patch".to_string()));
    }

    #[tokio::test]
    async fn execute_dag_uses_real_dependencies_without_batch_barriers() {
        let model = ConditionalModel;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let mut registry = ToolRegistry::with_working_dir(dir.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        let registry = Arc::new(registry);
        let events = Arc::new(Mutex::new(Vec::new()));
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: Some(Arc::new(RecordingObserver {
                events: Arc::clone(&events),
            })),
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let a = scoped_implementation_task("A", "src/a.txt");
        let mut c = scoped_implementation_task("C", "src/c.txt");
        c.task.add_dependency(a.id());

        let b = scoped_implementation_task("B", "src/b.txt");

        let plan = plan_for_tasks(vec![a, c, b], 2);

        let run_state = execute_dag(&plan, &model, &registry, &config)
            .await
            .unwrap();

        let timeline = events.lock().unwrap().clone();
        let c_started_at = timeline
            .iter()
            .position(|event| event == "start:C")
            .expect("C should start");
        let b_completed_at = timeline
            .iter()
            .position(|event| event == "done:B")
            .expect("B should complete");
        assert!(
            c_started_at < b_completed_at,
            "C should start before B completes when only real dependencies are wired"
        );
        assert_eq!(run_state.task_results.len(), 3);
        assert_eq!(run_state.dagrs_runtime.total_nodes, 3);
        assert_eq!(run_state.dagrs_runtime.completed_nodes, 3);
        assert!(
            run_state
                .ordered_task_results()
                .iter()
                .all(|result| result.status == TaskNodeStatus::Completed)
        );
    }

    #[tokio::test]
    async fn execute_dag_skips_dependent_tasks_after_failure() {
        let model = ConditionalModel;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let mut registry = ToolRegistry::with_working_dir(dir.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        let registry = Arc::new(registry);
        let events = Arc::new(Mutex::new(Vec::new()));
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: Some(Arc::new(RecordingObserver {
                events: Arc::clone(&events),
            })),
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let mut failing = implementation_task();
        failing.task = {
            let actor = ActorRef::agent("test-executor").unwrap();
            let mut task = GitTask::new(actor, "Fail first", None).unwrap();
            task.set_description(Some("Implement change".into()));
            task.add_constraint("network:allow");
            task.add_acceptance_criterion("tests pass");
            task
        };
        failing.objective = "Fail first".into();

        let mut later = implementation_task();
        later.task = {
            let actor = ActorRef::agent("test-executor").unwrap();
            let mut task = GitTask::new(actor, "Later", None).unwrap();
            task.set_description(Some("Implement change".into()));
            task.add_constraint("network:allow");
            task.add_acceptance_criterion("tests pass");
            task
        };
        later.objective = "Later".into();
        later.task.add_dependency(failing.id());

        let plan = plan_for_tasks(vec![failing.clone(), later.clone()], 1);

        let run_state = execute_dag(&plan, &model, &registry, &config)
            .await
            .unwrap();

        let timeline = events.lock().unwrap().clone();
        let start_order: Vec<String> = timeline
            .iter()
            .filter_map(|event| event.strip_prefix("start:").map(ToString::to_string))
            .collect();
        assert_eq!(start_order, vec!["Fail first"]);
        assert_eq!(run_state.task_results.len(), 2);
        assert_eq!(run_state.task_results[0].task_id, failing.id());
        assert_eq!(run_state.task_results[0].status, TaskNodeStatus::Failed);
        assert_eq!(run_state.task_results[1].task_id, later.id());
        assert_eq!(run_state.task_results[1].status, TaskNodeStatus::Skipped);
        assert_eq!(run_state.dagrs_runtime.total_nodes, 2);
        assert_eq!(run_state.dagrs_runtime.completed_nodes, 2);
        assert_eq!(run_state.status_for(later.id()), TaskNodeStatus::Skipped);
    }

    #[tokio::test]
    async fn execute_dag_records_cost_budget_abort_as_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let mut registry = ToolRegistry::with_working_dir(dir.path().to_path_buf());
        registry.register("apply_patch", Arc::new(ApplyPatchHandler));
        let registry = Arc::new(registry);
        let mut spec = (*spec()).clone();
        spec.constraints.resources.max_cost_units = 1;
        spec.execution.concurrency.max_parallel_tasks = 1;
        let config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: Arc::new(spec),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: None,
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };

        let first = implementation_task();
        let mut second = implementation_task();
        second.task = {
            let actor = ActorRef::agent("test-executor").unwrap();
            let mut task = GitTask::new(actor, "Second", None).unwrap();
            task.set_description(Some("Implement change".into()));
            task.add_constraint("network:allow");
            task.add_acceptance_criterion("tests pass");
            task
        };
        second.objective = "Second".into();

        let plan = plan_for_tasks(vec![first.clone(), second.clone()], 1);
        let run_state = execute_dag(&plan, &AddFilePatchModel, &registry, &config)
            .await
            .unwrap();

        let completed = run_state
            .ordered_task_results()
            .iter()
            .filter(|result| result.status == TaskNodeStatus::Completed)
            .count();
        let failed = run_state
            .ordered_task_results()
            .iter()
            .filter(|result| result.status == TaskNodeStatus::Failed)
            .count();
        assert_eq!(completed, 1, "{:?}", run_state.ordered_task_results());
        assert_eq!(failed, 1, "{:?}", run_state.ordered_task_results());

        let failure = run_state
            .ordered_task_results()
            .iter()
            .find(|result| result.status == TaskNodeStatus::Failed)
            .expect("budget-exhausted task result should be recorded");
        assert!(
            failure
                .agent_output
                .as_deref()
                .is_some_and(|message| message.contains("cost budget exceeded")),
            "{failure:?}"
        );
        assert!(
            [first.id(), second.id()].into_iter().all(|task_id| {
                !matches!(
                    run_state.status_for(task_id),
                    TaskNodeStatus::Pending | TaskNodeStatus::Running
                )
            }),
            "{:?}",
            run_state.task_statuses
        );
    }

    #[tokio::test]
    async fn execute_dag_rejects_unimplemented_checkpoint_resume() {
        let registry = Arc::new(ToolRegistry::new());
        let dir = tempfile::tempdir().unwrap();

        let task = implementation_task();
        let plan = plan_for_tasks(vec![task], 1);

        let resume_config = ExecutorConfig {
            tool_loop_config: ToolLoopConfig::default(),
            max_retries: 0,
            backoff_seconds: 0,
            working_dir: dir.path().to_path_buf(),
            spec: spec(),
            reviewer_preamble: None,
            dagrs_resume_checkpoint_id: Some("todo".into()),
            observer: None,
            workspace_baseline: None,
            fuse_state: FuseProvisionState::default(),
        };
        let err = execute_dag(
            &plan,
            &MockModel {
                final_text: "done".into(),
            },
            &registry,
            &resume_config,
        )
        .await
        .expect_err("resume should remain disabled until checkpoint semantics are redesigned");

        let message = err.to_string();
        assert!(message.contains("not supported yet"));
        assert!(message.contains("userspace-fs"));
    }
}
