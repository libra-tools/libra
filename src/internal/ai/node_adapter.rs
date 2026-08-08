//! DAG node adapters for AI agents.
//!
//! This module is the bridge layer between the high-level AI agent runtime and the
//! lower-level `dagrs` DAG executor. By implementing the `dagrs::Action` trait on
//! agent-shaped wrappers, callers can drop an LLM-driven agent into any node of an
//! orchestrated workflow alongside non-AI nodes (shell tasks, file operations,
//! gates) without leaking AI concerns into the executor itself.
//!
//! # Adapters
//!
//! - [`AgentAction`]: Wraps a single [`Agent`] for one-shot prompt-response execution
//!   within a DAG node.
//! - [`ToolLoopAction`]: Wraps the iterative tool-calling loop ([`run_tool_loop`]) within
//!   a DAG node, allowing the agent to invoke tools repeatedly until a final answer is produced.
//!
//! # Data Flow
//!
//! Both adapters follow the same input/output pattern:
//! 1. **Input**: Collect string outputs from all upstream nodes and concatenate them
//!    with `"\n\n"` as a separator to form a single prompt.
//! 2. **Execution**: Run the agent (or tool loop) with the assembled prompt.
//! 3. **Output**: Broadcast the agent's response to all downstream nodes.
//!
//! Non-string upstream payloads are skipped with a warning: this keeps the adapter
//! resilient when a sibling DAG node emits structured data, but it does mean callers
//! must coerce structured outputs to `String` if they want the agent to see them.

use std::sync::Arc;

use async_trait::async_trait;
use dagrs::{Action, Content, EnvVar, InChannels, OutChannels, Output};

use crate::internal::ai::{
    agent::{Agent, ToolLoopConfig, run_tool_loop},
    completion::{CompletionModel, CompletionUsage, Prompt},
    tools::ToolRegistry,
};

/// Drain every upstream channel and concatenate their string payloads into a
/// single prompt fragment.
///
/// Functional scope:
/// - Iterates over all senders connected to this node, awaits one message per
///   sender, and joins the collected strings with `"\n\n"`.
/// - Logs a warning when an upstream channel produces a non-string `Content`
///   payload and silently drops that input (the agent receives the joined
///   fragments without it).
///
/// Boundary conditions:
/// - Returns `Err(String)` if any upstream `recv_from` fails — the caller must
///   convert this into `Output::execution_failed` so the DAG executor sees the
///   failure rather than letting the node silently emit an empty prompt.
/// - When there are no upstream nodes, returns an empty string. The caller
///   decides whether an empty prompt is acceptable (typically only the DAG
///   entry node).
async fn collect_upstream_prompt(in_channels: &mut InChannels) -> Result<String, String> {
    let ids = in_channels.get_sender_ids();
    let mut inputs = Vec::new();

    for id in ids {
        match in_channels.recv_from(&id).await {
            Ok(content) => {
                if let Some(text) = content.get::<String>() {
                    inputs.push(text.to_owned());
                } else {
                    // Non-string payloads are tolerated but skipped — agents only
                    // understand text. A surrounding orchestrator that wants to
                    // pipe structured data should convert it before broadcast.
                    tracing::warn!(
                        "Received content from upstream {:?} is not a String. Defaulting to empty.",
                        id
                    );
                }
            }
            Err(e) => {
                let error_msg = format!("Failed to receive input from upstream {:?}: {:?}", id, e);
                tracing::error!("{}", error_msg);
                return Err(error_msg);
            }
        }
    }

    Ok(inputs.join("\n\n"))
}

/// An [`Action`] adapter that wraps an AI [`Agent`] for use in a DAG node.
///
/// This adapter bridges the gap between `dagrs::Action` and the AI `Agent`.
/// It automatically handles:
/// 1. Reading input from upstream nodes.
///    - If there are multiple upstream nodes, their outputs are concatenated
///      with newlines (`"\n\n"`) to form a single prompt.
/// 2. Invoking the Agent with the assembled prompt.
/// 3. Broadcasting the Agent's response to downstream nodes.
///
/// # Type Parameters
///
/// * `M` - The [`CompletionModel`] implementation used by the agent.
pub struct AgentAction<M: CompletionModel + 'static> {
    /// The wrapped AI Agent instance.
    agent: Agent<M>,
}

impl<M: CompletionModel> AgentAction<M> {
    /// Creates a new `AgentAction` adapter wrapping the given agent.
    ///
    /// Functional scope:
    /// - Takes ownership of the configured `Agent`. The adapter is the sole
    ///   driver of the agent within the DAG; sharing the same agent across
    ///   multiple nodes would require an `Arc` wrapper, which this constructor
    ///   intentionally does not perform.
    ///
    /// Boundary conditions:
    /// - No validation is performed on the agent — callers are responsible for
    ///   ensuring its configuration (model, preamble, tools) is consistent with
    ///   the role it will play in the DAG.
    ///
    /// # Arguments
    ///
    /// * `agent` - The configured [`Agent`] instance to wrap.
    pub fn new(agent: Agent<M>) -> Self {
        Self { agent }
    }
}

#[async_trait]
impl<M: CompletionModel> Action for AgentAction<M> {
    /// Executes the agent within the DAG node lifecycle.
    ///
    /// This method performs the following steps:
    /// 1. Collects string outputs from all upstream nodes via `in_channels`.
    /// 2. Concatenates them into a single prompt separated by `"\n\n"`.
    /// 3. Sends the prompt to the wrapped agent.
    /// 4. On success, broadcasts the response to all downstream nodes via `out_channels`.
    /// 5. On failure, logs the error and returns `Output::Err`.
    ///
    /// Boundary conditions:
    /// - Upstream collection failure short-circuits to `Output::execution_failed`
    ///   without invoking the agent, so a broken predecessor does not consume
    ///   model quota.
    /// - Agent errors are logged at `error` level and converted into
    ///   `Output::execution_failed`; the underlying error type is stringified to
    ///   satisfy `dagrs`'s opaque error contract.
    ///
    /// # Arguments
    ///
    /// * `in_channels` - Channels for receiving input from upstream DAG nodes.
    /// * `out_channels` - Channels for sending output to downstream DAG nodes.
    /// * `_env` - Shared environment variables (unused by this adapter).
    async fn run(
        &self,
        in_channels: &mut InChannels,
        out_channels: &mut OutChannels,
        _env: Arc<EnvVar>,
    ) -> Output {
        let input = match collect_upstream_prompt(in_channels).await {
            Ok(input) => input,
            Err(e) => return Output::execution_failed(e),
        };

        // Step 2: Run the agent with the assembled prompt
        match self.agent.prompt(input).await {
            Ok(resp) => {
                // Broadcast the successful response to all downstream nodes —
                // we clone so the same payload is also returned via `Output::Out`
                // for the executor's bookkeeping.
                let content = Content::new(resp);
                out_channels.broadcast(content.clone()).await;
                Output::Out(Some(content))
            }
            Err(e) => {
                tracing::error!("Agent Execution Error: {}", e);
                Output::execution_failed(e.to_string())
            }
        }
    }
}

/// An [`Action`] adapter that runs the iterative tool-calling agent loop inside a DAG node.
///
/// Unlike [`AgentAction`], which performs a single prompt-response cycle, this adapter
/// invokes [`run_tool_loop`], allowing the agent to call tools repeatedly in a loop
/// until it produces a final answer.
///
/// # Type Parameters
///
/// * `M` - The [`CompletionModel`] implementation used by the tool loop.
///
/// # Fields
///
/// * `model` - The language model used for generating completions.
/// * `registry` - The registry of tools available to the agent during the loop.
/// * `config` - Configuration for the tool loop (preamble, temperature, hooks, etc.).
pub struct ToolLoopAction<M: CompletionModel + 'static> {
    /// The language model used for generating completions.
    model: M,
    /// The registry of tools available to the agent during the tool loop.
    registry: ToolRegistry,
    /// Configuration controlling the behavior of the tool loop.
    config: ToolLoopConfig,
}

impl<M: CompletionModel> ToolLoopAction<M> {
    /// Creates a new `ToolLoopAction` adapter.
    ///
    /// Functional scope:
    /// - Builds a default [`ToolLoopConfig`] populated only with the supplied
    ///   `preamble` and `temperature`. Every other knob (max turns, repeat
    ///   detection, hooks, terminal tools, etc.) is left at its default —
    ///   callers that need finer control should construct the config directly
    ///   and bypass this convenience constructor.
    ///
    /// Boundary conditions:
    /// - Passing `None` for either knob means the underlying loop falls back to
    ///   the implementation's defaults; this method does not expose those
    ///   defaults.
    ///
    /// # Arguments
    ///
    /// * `model` - The language model instance for generating completions.
    /// * `registry` - The tool registry containing all available tools.
    /// * `preamble` - An optional system preamble prepended to every prompt.
    /// * `temperature` - An optional temperature value controlling response randomness.
    pub fn new(
        model: M,
        registry: ToolRegistry,
        preamble: Option<String>,
        temperature: Option<f64>,
    ) -> Self {
        Self {
            model,
            registry,
            config: ToolLoopConfig {
                preamble,
                temperature,
                thinking: None,
                reasoning_effort: None,
                stream: None,
                hook_runner: None,
                allowed_tools: None,
                runtime_context: None,
                cancellation: None,
                max_turns: None,
                repeat_detection_window: None,
                repeat_warning_threshold: None,
                repeat_abort_threshold: None,
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
        }
    }
}

#[async_trait]
impl<M: CompletionModel> Action for ToolLoopAction<M>
where
    M::Response: CompletionUsage,
{
    /// Executes the tool loop within the DAG node lifecycle.
    ///
    /// Follows the same input collection pattern as [`AgentAction::run`]:
    /// collects and concatenates upstream outputs, then runs the tool loop
    /// instead of a simple agent prompt.
    ///
    /// Boundary conditions:
    /// - The loop's `config` is cloned per invocation, so mutating
    ///   `self.config` between runs is safe but does not retroactively affect
    ///   in-flight loops.
    /// - `run_tool_loop` may exit due to max-turn or repeat-detection limits;
    ///   such terminations propagate as `Err` and are reported as
    ///   `Output::execution_failed` to the executor.
    ///
    /// # Arguments
    ///
    /// * `in_channels` - Channels for receiving input from upstream DAG nodes.
    /// * `out_channels` - Channels for sending output to downstream DAG nodes.
    /// * `_env` - Shared environment variables (unused by this adapter).
    async fn run(
        &self,
        in_channels: &mut InChannels,
        out_channels: &mut OutChannels,
        _env: Arc<EnvVar>,
    ) -> Output {
        let prompt = match collect_upstream_prompt(in_channels).await {
            Ok(prompt) => prompt,
            Err(e) => return Output::execution_failed(e),
        };

        // Run the iterative tool-calling loop with the assembled prompt.
        // The loop owns its own turn budget and repeat detection — this adapter
        // simply forwards the final answer (or surfaces the loop's terminal error).
        match run_tool_loop(&self.model, prompt, &self.registry, self.config.clone()).await {
            Ok(resp) => {
                // Broadcast the successful response to all downstream nodes
                let content = Content::new(resp);
                out_channels.broadcast(content.clone()).await;
                Output::Out(Some(content))
            }
            Err(e) => {
                tracing::error!("Agent Tool Loop Error: {}", e);
                Output::execution_failed(e.to_string())
            }
        }
    }
}
