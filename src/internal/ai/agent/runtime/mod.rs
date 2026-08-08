//! Agent runtime — the execution layer that turns a [`CompletionModel`] plus
//! configuration into something callable from the rest of the codebase.
//!
//! The runtime exposes three top-level building blocks:
//!
//! - [`Agent`] — a stateless wrapper that bundles a model with a system preamble and
//!   tool set. One call, one tool loop, no memory between calls.
//! - [`AgentBuilder`] (in `builder`) — fluent constructor that validates configuration
//!   before producing an `Agent`.
//! - [`ChatAgent`] (in `chat`) — stateful counterpart that owns a conversation history
//!   and is the type the TUI/MCP layers actually drive.
//!
//! The lower-level [`tool_loop`] module exposes `run_tool_loop` /
//! `run_tool_loop_with_history_and_observer` for callers that want full control over
//! the model/tool ping-pong without hiding it behind `Agent`. Those entry points are
//! also what the codex executor (`codex/`) uses to execute a long-running plan.

use std::sync::Arc;

use crate::internal::ai::{
    completion::{
        Chat, CompletionError, CompletionModel, CompletionRequest, Message, Prompt,
        message::{AssistantContent, OneOrMany, ToolResult, UserContent},
    },
    tools::{ToolDefinition, ToolSet},
};

pub mod builder;
pub mod tool_loop;
pub use builder::AgentBuilder;
pub use tool_loop::{
    ToolLoopCancellation, ToolLoopConfig, ToolLoopObserver, ToolLoopTurn, run_tool_loop,
    run_tool_loop_with_history_and_observer,
};

pub mod chat;
pub use chat::ChatAgent;

// OC-Phase 3: SubAgentDispatcher trait + DispatchContext + TaskFailure
// vocabulary, plus the owned tool-loop runtime bundle used to intercept
// `task` calls when multi-agent mode is enabled.
pub mod sub_agent;
pub use sub_agent::{
    AbortToken, BudgetExceededReason, CancellationSource, ChannelPermissionAsk,
    ChannelPermissionAsker, ContextFrameLoader, ContextHandoffError, DefaultSubAgentChildRunner,
    DenyByDefaultPermissionAsker, DispatchContext, MessageId, PermissionAskRequest,
    PermissionAskSource, PermissionAsker, PermissionReply, PermissionService,
    ProviderBuildOptionsResolver, SafetyDecisionDenial, SubAgentChildRunRequest,
    SubAgentChildRunner, SubAgentDispatcher, SubAgentToolLoopRuntime, TaskEntryKind, TaskFailure,
    TaskInvocation, TaskResult, ToolLoopError, WorkspaceIsolationConfig,
};

// OC-Phase 3 P3.3 + P3.4 + P3.6 + P3.7:
// DefaultSubAgentDispatcher gate + ask implementation, child
// tool-loop tail, user-initiated dispatch entrypoints, and
// parent-cancel propagation, SafetyDecision checks, AgentRunEvent
// Spawned/Completed/Failed wiring, timeout caps, and budget caps.
pub mod sub_agent_dispatcher;
pub use sub_agent_dispatcher::{AgentSpecRegistry, DefaultSubAgentDispatcher, MultiAgentConfig};

/// An AI Agent that manages interactions with a CompletionModel.
///
/// This is a **stateless** agent (also known as a Simple Agent). It handles configuration
/// (preamble, tools, temperature) and requests, but it does not maintain conversation history
/// between calls.
///
/// For a stateful agent that remembers context, use [`ChatAgent`].
///
/// The Agent is responsible for:
/// - Maintaining configuration (temperature, preamble/system prompt).
/// - Managing tools (optional).
/// - Constructing requests to the underlying model.
///
/// It implements the `Prompt` and `Chat` traits for easy interaction.
#[derive(Clone)]
pub struct Agent<M: CompletionModel> {
    /// The underlying completion model (e.g., Gemini, OpenAI).
    model: Arc<M>,
    /// System prompt or preamble to set the agent's behavior context.
    preamble: Option<String>,
    /// Sampling temperature (0.0 to 2.0). Higher values mean more creativity.
    temperature: Option<f64>,
    /// Set of tools available to the agent.
    tools: ToolSet,
}

impl<M: CompletionModel> Agent<M> {
    /// Creates a new Agent with the given model.
    ///
    /// Functional scope: wraps `model` in an [`Arc`] so cheap clones share the same
    /// network client, and initializes the rest of the configuration to zero/default.
    /// Most callers should go through [`AgentBuilder`] instead, which validates the
    /// preamble and tool set before construction.
    ///
    /// # Arguments
    /// * `model` - The completion model instance.
    pub fn new(model: M) -> Self {
        Self {
            model: Arc::new(model),
            preamble: None,
            temperature: None,
            tools: ToolSet::default(),
        }
    }

    /// Drive the model/tool ping-pong starting from a pre-populated chat history.
    ///
    /// Functional scope:
    /// - Snapshots the configured tool definitions once and reuses them across
    ///   iterations to avoid repeating the rebuild on every step.
    /// - Repeatedly issues completion requests; if the response contains tool calls,
    ///   it appends the assistant turn, executes each tool, appends the tool results
    ///   as a synthetic user turn, and loops. As soon as the model stops calling
    ///   tools, the function joins all text content and returns.
    ///
    /// Boundary conditions:
    /// - Returns `Err(CompletionError::ResponseError)` if the model produced content
    ///   that contained no text (e.g. only thoughts) — this is surfaced verbatim to
    ///   the user instead of being silently treated as success.
    /// - Returns `Err(CompletionError::RequestError)` with `NotFound` when the model
    ///   tried to call a tool that is not registered on the agent.
    /// - Empty content in either the assistant turn or the tool-result turn is treated
    ///   as a malformed response and surfaces as `ResponseError` rather than panicking.
    /// - This loop has no iteration limit by design; callers that need a budget should
    ///   use the [`tool_loop`] entry points instead.
    pub(crate) async fn run_with_history(
        &self,
        mut chat_history: Vec<Message>,
    ) -> Result<String, CompletionError> {
        let tools: Vec<ToolDefinition> = self.tools.tools.iter().map(|t| t.definition()).collect();

        loop {
            let request = CompletionRequest {
                preamble: self.preamble.clone(),
                chat_history: chat_history.clone(),
                temperature: self.temperature,
                tools: tools.clone(),
                ..Default::default()
            };

            let response = self.model.completion(request).await?;

            let mut tool_calls = Vec::new();
            for item in &response.content {
                if let AssistantContent::ToolCall(tc) = item {
                    tool_calls.push(tc.clone());
                }
            }

            if tool_calls.is_empty() {
                // Terminal state: aggregate every text fragment into the final answer.
                let text_response = response
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        AssistantContent::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                if text_response.is_empty() && !response.content.is_empty() {
                    // Content existed but contained no text — typically only reasoning
                    // tokens. Return a more user-friendly error instead of debug format.
                    return Err(CompletionError::ResponseError(
                        "Model returned non-text response (likely only thought or unsupported content)".into()
                    ));
                }

                return Ok(text_response);
            }

            let assistant_content = match OneOrMany::many(response.content.clone()) {
                Some(content) => content,
                None => {
                    return Err(CompletionError::ResponseError(
                        "Empty assistant content in tool call response".into(),
                    ));
                }
            };

            chat_history.push(Message::Assistant {
                id: None,
                reasoning_content: response.reasoning_content.clone(),
                content: assistant_content,
            });

            let mut results = Vec::new();
            for tc in tool_calls {
                let tool = self
                    .tools
                    .tools
                    .iter()
                    .find(|t| t.name() == tc.function.name)
                    .ok_or_else(|| {
                        CompletionError::RequestError(
                            std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                format!("Tool not found: {}", tc.function.name),
                            )
                            .into(),
                        )
                    })?;

                let result = tool
                    .call(tc.function.arguments.clone())
                    .map_err(CompletionError::RequestError)?;

                results.push(UserContent::ToolResult(ToolResult {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    result,
                }));
            }

            let tool_result_content = match OneOrMany::many(results) {
                Some(content) => content,
                None => {
                    return Err(CompletionError::ResponseError("Empty tool results".into()));
                }
            };

            chat_history.push(Message::User {
                content: tool_result_content,
            });
        }
    }
}

impl<M: CompletionModel> Prompt for Agent<M> {
    /// Single-shot prompt: starts a fresh conversation containing only `prompt` and
    /// drives the tool loop until the model stops calling tools.
    async fn prompt(&self, prompt: impl Into<Message> + Send) -> Result<String, CompletionError> {
        let msg = prompt.into();
        self.run_with_history(vec![msg]).await
    }
}

impl<M: CompletionModel> Chat for Agent<M> {
    /// Multi-turn prompt: appends `prompt` to `chat_history` (without persisting the
    /// updated history anywhere) and drives the tool loop. The agent itself stays
    /// stateless — the caller owns the history.
    async fn chat(
        &self,
        prompt: impl Into<Message> + Send,
        mut chat_history: Vec<Message>,
    ) -> Result<String, CompletionError> {
        let msg = prompt.into();
        chat_history.push(msg);
        self.run_with_history(chat_history).await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::AgentBuilder;
    use crate::internal::ai::{
        completion::{
            CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Message,
            Prompt,
            message::{AssistantContent, Function, Text, ToolCall, UserContent},
        },
        tools::{Tool, ToolDefinition, ToolSet},
    };

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

    struct MockTool;

    impl Tool for MockTool {
        fn name(&self) -> String {
            "mock_tool".to_string()
        }

        fn description(&self) -> String {
            "Mock tool".to_string()
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name(),
                description: self.description(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "value": { "type": "number" }
                    }
                }),
            }
        }

        fn call(
            &self,
            _args: serde_json::Value,
        ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
            Ok(json!({"ok": true}))
        }
    }

    /// Scenario: a mock model first emits a tool call, then on the second iteration
    /// emits a plain text response. This exercises the full tool-execution path:
    /// assistant turn → tool dispatch → user (tool result) turn → final text.
    #[tokio::test]
    async fn test_tool_call_loop_executes_tool() {
        let mut tool_set = ToolSet::default();
        tool_set.tools.push(std::sync::Arc::new(MockTool));

        let agent = AgentBuilder::new(MockModel).tools(tool_set).build();
        let response = Prompt::prompt(&agent, "hi").await.unwrap();

        assert_eq!(response, "done");
    }
}
