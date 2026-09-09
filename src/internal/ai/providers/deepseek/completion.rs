//! DeepSeek completion model implementation.
//!
//! DeepSeek exposes an OpenAI-compatible Chat Completions endpoint. Common wire
//! types and helpers are imported from [`openai_compat`](super::super::openai_compat).
//!
//! # DeepSeek-specific behaviour
//!
//! - **Thinking mode**: When `thinking.type = "enabled"` is set, the API expects every
//!   prior assistant turn to either carry `reasoning_content` *or* be downgraded to a
//!   plain user note. [`normalize_messages_for_deepseek_thinking`] performs that
//!   downgrade for synthetic history (e.g. tool-only intent flows) so the API does
//!   not reject the request.
//! - **Streaming**: `stream = true` switches the response shape to NDJSON `data: ...`
//!   chunks terminated by `data: [DONE]`. [`DeepSeekStreamAccumulator`] reassembles
//!   chunks into a final [`ChatResponse`]; on partial-body failure the request is
//!   transparently retried in non-streaming mode (see
//!   [`should_retry_deepseek_stream_without_stream`]).
//! - **`reasoning_effort`**: a four-valued discrete control (`low`/`medium`/`high`/`max`)
//!   that tunes the depth of the chain-of-thought.

use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::internal::ai::{
    client::{CompletionClient, Provider},
    completion::{
        CompletionError, CompletionModel as CompletionModelTrait, CompletionReasoningEffort,
        CompletionStreamEvent, CompletionThinking,
        request::{CompletionRequest, CompletionResponse},
    },
    providers::{
        deepseek::client::Client,
        openai_compat::{
            ChatChoice, ChatErrorResponse, ChatFunctionCall, ChatMessage, ChatResponse,
            ChatToolCall, ChatToolDefinition, ChatUsage, build_messages_with_reasoning_content,
            choice_reasoning_content, parse_choice_content_for_provider, parse_tools,
        },
    },
};

const DEEPSEEK_REQUEST_MAX_ATTEMPTS: u32 = 3;
const DEEPSEEK_RETRY_BASE_DELAY_MS: u64 = 250;
const DEEPSEEK_RETRY_MAX_DELAY_MS: u64 = 2_000;

/// DeepSeek completion model bound to a specific model identifier and HTTP [`Client`].
///
/// Construct via [`CompletionClient::completion_model`] (preferred) or
/// [`Model::new`].
#[derive(Clone, Debug)]
pub struct Model {
    client: Client,
    model: String,
}

impl Model {
    /// Creates a new DeepSeek completion model.
    ///
    /// Boundary conditions:
    /// - The `model` string is forwarded verbatim; unknown ids fail at request
    ///   time with a 404.
    pub fn new(client: Client, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }

    /// Returns the model name.
    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// Send a single `POST /chat/completions` request and translate non-2xx responses
    /// into [`CompletionError::ProviderError`].
    ///
    /// Functional scope:
    /// - On HTTP success the raw `reqwest::Response` is returned so the caller can
    ///   choose to read the body as JSON (non-streaming) or as a byte stream.
    /// - On HTTP failure the body is consumed and an attempt is made to decode it as
    ///   an OpenAI-style `{"error":{"message": ...}}` envelope. If decoding fails the
    ///   raw body is embedded in the error message instead.
    ///
    /// Boundary conditions:
    /// - Network-level failures bubble up via [`CompletionError::HttpError`].
    /// - Tracing emits `provider = "deepseek"` for every code path so production logs
    ///   can be filtered by provider.
    async fn send_chat_completion_request(
        &self,
        request: &DeepSeekRequest,
    ) -> Result<reqwest::Response, CompletionError> {
        for attempt in 1..=DEEPSEEK_REQUEST_MAX_ATTEMPTS {
            match self.send_chat_completion_request_once(request).await {
                Ok(response) => return Ok(response),
                Err(error)
                    if attempt < DEEPSEEK_REQUEST_MAX_ATTEMPTS
                        && should_retry_deepseek_request_error(&error) =>
                {
                    let delay = deepseek_retry_delay(attempt);
                    tracing::warn!(
                        provider = "deepseek",
                        attempt,
                        next_attempt = attempt + 1,
                        max_attempts = DEEPSEEK_REQUEST_MAX_ATTEMPTS,
                        delay_ms = delay.as_millis() as u64,
                        error = %error,
                        "DeepSeek request failed with retryable error; retrying"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        }

        Err(CompletionError::ResponseError(
            "DeepSeek retry loop exited without returning a response".to_string(),
        ))
    }

    async fn send_chat_completion_request_once(
        &self,
        request: &DeepSeekRequest,
    ) -> Result<reqwest::Response, CompletionError> {
        let mut req_builder = self
            .client
            .http_client
            .post(format!("{}/chat/completions", self.client.base_url))
            .json(request);
        req_builder = self.client.provider.on_request(req_builder);

        let response = req_builder
            .send()
            .await
            .map_err(CompletionError::HttpError)?;

        let status = response.status();

        if status.is_success() {
            tracing::debug!(
                provider = "deepseek",
                status = status.as_u16(),
                "DeepSeek HTTP request succeeded"
            );
            return Ok(response);
        }

        let response_text = response.text().await.map_err(CompletionError::HttpError)?;
        tracing::debug!(
            provider = "deepseek",
            status = status.as_u16(),
            body_bytes = response_text.len(),
            "DeepSeek HTTP request failed"
        );
        if let Ok(error_response) = serde_json::from_str::<ChatErrorResponse>(&response_text) {
            return Err(CompletionError::ProviderError(format!(
                "status {}: {}",
                status.as_u16(),
                error_response.error.message
            )));
        }
        Err(CompletionError::ProviderError(format!(
            "status {}: {}",
            status.as_u16(),
            response_text
        )))
    }
}

// ================================================================
// DeepSeek-specific Request / ToolChoice Types
// ================================================================

/// DeepSeek request body sent to `POST /chat/completions`.
///
/// Mostly identical to OpenAI's request shape but adds two DeepSeek-specific
/// fields:
/// - `thinking` — toggles chain-of-thought emission.
/// - `reasoning_effort` — a discrete budget knob for depth of reasoning.
///
/// `stream` is *always* serialised (no `skip_serializing_if`) because DeepSeek
/// treats omission as opt-in to streaming for some models.
#[derive(Debug, Serialize)]
struct DeepSeekRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<DeepSeekToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<DeepSeekThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<DeepSeekReasoningEffort>,
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct DeepSeekThinking {
    #[serde(rename = "type")]
    r#type: DeepSeekThinkingType,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum DeepSeekThinkingType {
    Enabled,
    Disabled,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum DeepSeekReasoningEffort {
    Low,
    Medium,
    High,
    Max,
}

/// DeepSeek uses a tagged enum for tool choice (differs from OpenAI's untagged approach).
#[derive(Debug, Serialize, Deserialize)]
enum DeepSeekToolChoice {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "none")]
    None,
    #[serde(rename = "required")]
    Required,
    Function(DeepSeekFunctionToolChoice),
}

#[derive(Debug, Serialize, Deserialize)]
struct DeepSeekFunctionToolChoice {
    #[serde(rename = "type")]
    r#type: String,
    function: DeepSeekToolChoiceFunction,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeepSeekToolChoiceFunction {
    name: String,
}

/// Map Libra's provider-agnostic [`CompletionThinking`] enum to the DeepSeek wire
/// shape.
///
/// Functional scope: Libra's `Auto` is treated as "do not send the field" because
/// DeepSeek defaults to disabled when the field is absent; explicit `Disabled`
/// sends `type = "disabled"` so users can override per-request.
fn deepseek_thinking(thinking: Option<CompletionThinking>) -> Option<DeepSeekThinking> {
    match thinking {
        Some(CompletionThinking::Disabled) => Some(DeepSeekThinking {
            r#type: DeepSeekThinkingType::Disabled,
        }),
        Some(
            CompletionThinking::Enabled
            | CompletionThinking::Low
            | CompletionThinking::Medium
            | CompletionThinking::High,
        ) => Some(DeepSeekThinking {
            r#type: DeepSeekThinkingType::Enabled,
        }),
        Some(CompletionThinking::Auto) | None => None,
    }
}

/// Convert Libra's [`CompletionReasoningEffort`] into the DeepSeek wire enum.
///
/// Functional scope: identity mapping; preserved as a function so the conversion
/// stays explicit and is easy to extend if either enum gains new variants.
fn deepseek_reasoning_effort(
    reasoning_effort: Option<CompletionReasoningEffort>,
) -> Option<DeepSeekReasoningEffort> {
    match reasoning_effort {
        Some(CompletionReasoningEffort::Low) => Some(DeepSeekReasoningEffort::Low),
        Some(CompletionReasoningEffort::Medium) => Some(DeepSeekReasoningEffort::Medium),
        Some(CompletionReasoningEffort::High) => Some(DeepSeekReasoningEffort::High),
        Some(CompletionReasoningEffort::Max) => Some(DeepSeekReasoningEffort::Max),
        None => None,
    }
}

/// Tell whether the request will instruct DeepSeek to emit reasoning content.
///
/// Used to decide whether [`normalize_messages_for_deepseek_thinking`] needs to run.
fn deepseek_thinking_enabled(thinking: &Option<DeepSeekThinking>) -> bool {
    matches!(
        thinking,
        Some(DeepSeekThinking {
            r#type: DeepSeekThinkingType::Enabled
        })
    )
}

/// Rewrite history so DeepSeek's thinking mode does not reject the request.
///
/// Functional scope:
/// - DeepSeek thinking models require every prior assistant turn to either carry
///   `reasoning_content` or be replaced with a synthetic user note that summarises
///   the previous text and any tool exchange.
/// - Walks the message list in order, collapses each `Assistant` turn that lacks
///   `reasoning_content` (plus any directly-following `Tool` results that reference
///   one of its tool calls) into a single user-role recap message.
///
/// Boundary conditions:
/// - Returns `(rewritten_messages, count)` so the caller can log how many turns
///   were collapsed; `count == 0` means the request was already compatible.
/// - Tool messages whose `tool_call_id` does not match any of the collapsed
///   assistant tool calls are passed through unmodified — they continue to belong
///   to a *different* assistant turn that did carry `reasoning_content`.
///
/// See: `tests::deepseek_thinking_normalizes_synthetic_assistant_messages_without_reasoning`,
/// `tests::deepseek_thinking_keeps_assistant_messages_with_reasoning_content`,
/// `tests::deepseek_thinking_collapses_unusable_tool_history_without_reasoning_content`.
fn normalize_messages_for_deepseek_thinking(
    messages: Vec<ChatMessage>,
) -> (Vec<ChatMessage>, usize) {
    let mut normalized = Vec::with_capacity(messages.len());
    let mut iter = messages.into_iter().peekable();
    let mut converted = 0usize;

    while let Some(message) = iter.next() {
        match message {
            ChatMessage::Assistant {
                content,
                reasoning_content: None,
                tool_calls,
            } => {
                let mut note_parts = Vec::new();
                if let Some(text) = content.filter(|text| !text.trim().is_empty()) {
                    note_parts.push(format!("Previous assistant message:\n{text}"));
                }

                if !tool_calls.is_empty() {
                    let tool_call_ids = tool_calls
                        .iter()
                        .map(|call| call.id.clone())
                        .collect::<HashSet<_>>();
                    let tool_call_summary = tool_calls
                        .iter()
                        .map(|call| {
                            format!(
                                "- {} ({}) arguments: {}",
                                call.function.name, call.id, call.function.arguments
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    note_parts.push(format!(
                        "Previous assistant tool calls:\n{tool_call_summary}"
                    ));

                    while matches!(
                        iter.peek(),
                        Some(ChatMessage::Tool { tool_call_id, .. })
                            if tool_call_ids.contains(tool_call_id)
                    ) {
                        if let Some(ChatMessage::Tool {
                            tool_call_id,
                            name,
                            content,
                        }) = iter.next()
                        {
                            note_parts.push(format!(
                                "Previous tool result for {name} ({tool_call_id}):\n{content}"
                            ));
                        }
                    }
                }

                if !note_parts.is_empty() {
                    normalized.push(ChatMessage::User {
                        content: note_parts.join("\n\n"),
                    });
                }
                converted += 1;
            }
            other => normalized.push(other),
        }
    }

    (normalized, converted)
}

/// Classification of a DeepSeek response choice used purely for observability.
///
/// `ReasoningOnly` is a known DeepSeek failure mode where the model returns
/// only chain-of-thought but no actionable output; logging it separately makes
/// these incidents easy to spot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeepSeekResponseKind {
    /// At least one text part or tool call is present — the agent loop has work to do.
    TextOrTool,
    /// Only `reasoning_content` is populated; the model produced thinking but no answer.
    ReasoningOnly,
    /// No content fields at all — the response is unusable.
    Empty,
}

/// Categorise an assistant choice for tracing/observability.
///
/// Functional scope:
/// - Returns `TextOrTool` when there is at least one non-blank text part or one
///   tool call.
/// - Returns `ReasoningOnly` when only chain-of-thought is present.
/// - Returns `Empty` for whitespace-only / no-content responses.
fn classify_deepseek_choice(choice: &ChatChoice) -> DeepSeekResponseKind {
    let ChatMessage::Assistant {
        content,
        reasoning_content,
        tool_calls,
    } = &choice.message
    else {
        return DeepSeekResponseKind::Empty;
    };

    if content
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || !tool_calls.is_empty()
    {
        return DeepSeekResponseKind::TextOrTool;
    }

    if reasoning_content
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return DeepSeekResponseKind::ReasoningOnly;
    }

    DeepSeekResponseKind::Empty
}

/// A single NDJSON chunk emitted by the streaming `/chat/completions` endpoint.
///
/// Each `data: ...` line decodes into one of these. Final `data: [DONE]` markers
/// are handled by [`process_deepseek_stream_line`] before this struct is touched.
#[derive(Debug, Deserialize)]
struct DeepSeekStreamChunk {
    id: String,
    choices: Vec<DeepSeekStreamChoice>,
    created: u64,
    model: String,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct DeepSeekStreamChoice {
    index: usize,
    delta: DeepSeekStreamDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct DeepSeekStreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<DeepSeekStreamToolCallDelta>,
}

#[derive(Debug, Deserialize)]
struct DeepSeekStreamToolCallDelta {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<DeepSeekStreamFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct DeepSeekStreamFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Default)]
struct DeepSeekStreamAccumulator {
    id: Option<String>,
    created: Option<u64>,
    model: Option<String>,
    content: String,
    reasoning_content: String,
    tool_calls: BTreeMap<usize, DeepSeekStreamToolCallBuilder>,
    finish_reason: Option<String>,
    usage: Option<ChatUsage>,
}

#[derive(Debug, Default)]
struct DeepSeekStreamToolCallBuilder {
    id: Option<String>,
    name: String,
    arguments: String,
}

impl DeepSeekStreamToolCallBuilder {
    /// Tell whether the builder has accumulated enough fragments to form a valid
    /// tool call.
    ///
    /// Functional scope:
    /// - Requires a non-empty function name.
    /// - Requires the arguments buffer to parse as valid JSON. An empty buffer is
    ///   ambiguous during streaming and is treated as a partial fragment for
    ///   body-error recovery.
    fn is_complete(&self) -> bool {
        !self.name.is_empty()
            && !self.arguments.trim().is_empty()
            && serde_json::from_str::<Value>(&self.arguments).is_ok()
    }
}

impl DeepSeekStreamAccumulator {
    /// Is there enough decoded content that the agent can usefully act on it?
    ///
    /// Used to decide whether a mid-stream body error should still be reported as
    /// success ("we got a usable response, log a warning and continue") versus
    /// surfaced to the caller as a hard failure.
    fn has_salvageable_response(&self) -> bool {
        !self.has_incomplete_tool_calls()
            && (!self.content.trim().is_empty() || !self.tool_calls.is_empty())
    }

    fn has_incomplete_tool_calls(&self) -> bool {
        self.tool_calls
            .values()
            .any(|builder| !builder.is_complete())
    }

    /// Has the stream emitted *any* fragment, even if it is not yet usable?
    ///
    /// Distinguishes "stream produced reasoning-only or partial tool calls" from
    /// "stream produced absolutely nothing"; the former must be wrapped in a
    /// descriptive error so log readers can see what was lost.
    fn has_partial_output(&self) -> bool {
        !self.content.is_empty()
            || !self.reasoning_content.is_empty()
            || !self.tool_calls.is_empty()
    }

    /// Merge a freshly-decoded NDJSON chunk into the accumulator and emit any
    /// stream events the consumer can render incrementally.
    ///
    /// Functional scope:
    /// - Captures `id`, `created`, `model`, and `usage` once per stream so the
    ///   later [`Self::into_response`] can produce a complete final response even
    ///   when fields appear only on the first or last chunk.
    /// - Forwards `TextDelta`, `ThinkingDelta`, and (for completed tool calls)
    ///   `ToolCallPreview` events to `stream_events`, allowing the Code UI to render
    ///   characters as they arrive.
    fn push_chunk(
        &mut self,
        chunk: DeepSeekStreamChunk,
        stream_events: Option<&UnboundedSender<CompletionStreamEvent>>,
    ) {
        let request_id = Some(chunk.id.clone());
        self.id.get_or_insert_with(|| chunk.id.clone());
        self.created.get_or_insert(chunk.created);
        self.model.get_or_insert_with(|| chunk.model.clone());
        if chunk.usage.is_some() {
            self.usage = chunk.usage;
        }

        for choice in chunk.choices {
            if choice.finish_reason.is_some() {
                self.finish_reason = choice.finish_reason;
            }

            if let Some(delta) = choice.delta.content.filter(|delta| !delta.is_empty()) {
                if self.content.is_empty() {
                    tracing::debug!(
                        provider = "deepseek",
                        "DeepSeek stream received first text delta"
                    );
                }
                self.content.push_str(&delta);
                if let Some(stream_events) = stream_events {
                    let _ = stream_events.send(CompletionStreamEvent::TextDelta {
                        request_id: request_id.clone(),
                        delta,
                    });
                }
            }

            if let Some(delta) = choice
                .delta
                .reasoning_content
                .filter(|delta| !delta.is_empty())
            {
                if self.reasoning_content.is_empty() {
                    tracing::debug!(
                        provider = "deepseek",
                        "DeepSeek stream received first reasoning delta"
                    );
                }
                self.reasoning_content.push_str(&delta);
                if let Some(stream_events) = stream_events {
                    let _ = stream_events.send(CompletionStreamEvent::ThinkingDelta {
                        request_id: request_id.clone(),
                        delta,
                    });
                }
            }

            for tool_call in choice.delta.tool_calls {
                self.push_tool_call_delta(choice.index, tool_call, stream_events, &request_id);
            }
        }
    }

    /// Apply a tool-call fragment delta to the accumulator.
    ///
    /// Functional scope:
    /// - Tool call fragments may arrive across many chunks: the first chunk
    ///   typically supplies the `id` and function `name`; subsequent chunks
    ///   stream the JSON `arguments` payload character-by-character.
    /// - Once a builder's arguments parse as valid JSON, a `ToolCallPreview` event
    ///   is published so the UI can render the call before the stream completes.
    ///
    /// Boundary conditions:
    /// - DeepSeek occasionally omits the per-tool-call `index`; in that case the
    ///   choice's index is used as a fallback so concurrent calls do not collide.
    fn push_tool_call_delta(
        &mut self,
        fallback_index: usize,
        delta: DeepSeekStreamToolCallDelta,
        stream_events: Option<&UnboundedSender<CompletionStreamEvent>>,
        request_id: &Option<String>,
    ) {
        let index = delta.index.unwrap_or(fallback_index);
        let builder = self.tool_calls.entry(index).or_default();
        if let Some(id) = delta.id {
            builder.id = Some(id);
        }
        if let Some(function) = delta.function {
            if let Some(name) = function.name {
                builder.name.push_str(&name);
            }
            if let Some(arguments) = function.arguments {
                builder.arguments.push_str(&arguments);
            }
        }

        if builder.name.is_empty() {
            return;
        }
        let Ok(arguments) = serde_json::from_str::<Value>(&builder.arguments) else {
            return;
        };
        if let Some(stream_events) = stream_events {
            let call_id = builder
                .id
                .clone()
                .unwrap_or_else(|| format!("call_{index}"));
            let _ = stream_events.send(CompletionStreamEvent::ToolCallPreview {
                request_id: request_id.clone(),
                call_id,
                tool_name: builder.name.clone(),
                arguments,
            });
        }
    }

    /// Finalise the accumulator into a single [`ChatResponse`] equivalent to a
    /// non-streaming request response.
    ///
    /// Functional scope:
    /// - Fills in sensible defaults for `id`, `model`, and `finish_reason` when
    ///   the stream omitted them.
    /// - Forces empty arguments to `"{}"` so the resulting tool call is still
    ///   valid JSON.
    ///
    /// Boundary conditions:
    /// - Returns [`CompletionError::ResponseError`] if any tool-call builder lacks
    ///   a function name. This indicates a server-side protocol violation; the
    ///   caller must surface it to the user rather than silently dropping the
    ///   broken tool call.
    fn into_response(self, fallback_model: &str) -> Result<ChatResponse, CompletionError> {
        let mut tool_calls = Vec::new();
        for (index, builder) in self.tool_calls {
            if builder.name.is_empty() {
                return Err(CompletionError::ResponseError(format!(
                    "DeepSeek stream returned tool call {index} without a function name"
                )));
            }

            let arguments = if builder.arguments.trim().is_empty() {
                "{}".to_string()
            } else {
                builder.arguments
            };
            tool_calls.push(ChatToolCall {
                id: builder.id.unwrap_or_else(|| format!("call_{index}")),
                r#type: "function".to_string(),
                function: ChatFunctionCall {
                    name: builder.name,
                    arguments,
                },
            });
        }

        let content = if self.content.trim().is_empty() {
            None
        } else {
            Some(self.content)
        };
        let finish_reason = self.finish_reason.or_else(|| {
            if tool_calls.is_empty() {
                Some("stop".to_string())
            } else {
                Some("tool_calls".to_string())
            }
        });

        Ok(ChatResponse {
            id: self.id.unwrap_or_else(|| "deepseek-stream".to_string()),
            object: "chat.completion".to_string(),
            created: self.created.unwrap_or_default(),
            model: self.model.unwrap_or_else(|| fallback_model.to_string()),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage::Assistant {
                    content,
                    reasoning_content: if self.reasoning_content.trim().is_empty() {
                        None
                    } else {
                        Some(self.reasoning_content)
                    },
                    tool_calls,
                },
                finish_reason,
            }],
            usage: self.usage,
        })
    }
}

/// Decode and apply one logical line from the streaming response body.
///
/// Functional scope:
/// - Skips empty lines and SSE comments (lines beginning with `:`).
/// - Recognises the literal `data: [DONE]` terminator and signals end-of-stream
///   via the returned `bool`.
/// - Decodes any other `data: ...` payload as a [`DeepSeekStreamChunk`] and
///   merges it into `accumulator`.
///
/// Boundary conditions:
/// - Invalid UTF-8 lines surface as [`CompletionError::ResponseError`].
/// - JSON decode errors propagate via the `From<serde_json::Error>` impl on
///   [`CompletionError`].
fn process_deepseek_stream_line(
    line: &[u8],
    accumulator: &mut DeepSeekStreamAccumulator,
    stream_events: Option<&UnboundedSender<CompletionStreamEvent>>,
) -> Result<bool, CompletionError> {
    let line = std::str::from_utf8(line).map_err(|error| {
        CompletionError::ResponseError(format!("invalid DeepSeek stream UTF-8: {error}"))
    })?;
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return Ok(false);
    }
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(false);
    };
    let data = data.trim();
    if data == "[DONE]" {
        tracing::debug!(
            provider = "deepseek",
            "DeepSeek stream received DONE marker"
        );
        return Ok(true);
    }

    let chunk: DeepSeekStreamChunk = serde_json::from_str(data)?;
    accumulator.push_chunk(chunk, stream_events);
    Ok(false)
}

/// Drive the streaming HTTP body to completion and assemble the final response.
///
/// Functional scope:
/// - Reads bytes from `response`'s `bytes_stream`, splits them on newlines, and
///   dispatches each line to [`process_deepseek_stream_line`].
/// - Salvages the response when a body read error arrives *after* the model has
///   already produced a usable answer; otherwise the error is wrapped in a
///   [`CompletionError`] that explains how much partial state was lost.
///
/// Boundary conditions:
/// - Trailing bytes that do not end in a newline are still attempted as a final
///   line so the last `data: ...` chunk is not silently discarded.
/// - The `fallback_model` is only used when DeepSeek omits the `model` field
///   from every chunk, which the spec allows.
async fn read_deepseek_stream_response(
    response: reqwest::Response,
    fallback_model: &str,
    stream_events: Option<&UnboundedSender<CompletionStreamEvent>>,
) -> Result<ChatResponse, CompletionError> {
    let mut accumulator = DeepSeekStreamAccumulator::default();
    let mut pending = Vec::<u8>::new();
    let mut stream = response.bytes_stream();
    let mut done = false;

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                if accumulator.has_salvageable_response() {
                    tracing::warn!(
                        provider = "deepseek",
                        error = %error,
                        content_bytes = accumulator.content.len(),
                        reasoning_bytes = accumulator.reasoning_content.len(),
                        tool_call_fragments = accumulator.tool_calls.len(),
                        "DeepSeek stream ended with a body read error after a usable response; using accumulated response"
                    );
                    break;
                }

                return Err(deepseek_stream_body_error(error, &accumulator));
            }
        };
        pending.extend_from_slice(&chunk);

        while let Some(newline_index) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=newline_index).collect::<Vec<_>>();
            done = process_deepseek_stream_line(&line, &mut accumulator, stream_events)? || done;
        }
    }

    if !pending.is_empty() && !done {
        process_deepseek_stream_line(&pending, &mut accumulator, stream_events)?;
    }

    tracing::debug!(
        provider = "deepseek",
        done,
        content_bytes = accumulator.content.len(),
        reasoning_bytes = accumulator.reasoning_content.len(),
        tool_call_fragments = accumulator.tool_calls.len(),
        finish_reason = accumulator.finish_reason.as_deref().unwrap_or(""),
        "DeepSeek stream accumulated response"
    );

    accumulator.into_response(fallback_model)
}

/// Wrap a `reqwest` body-read error with a description of any partial output that
/// was lost.
///
/// Functional scope: keeps the original `reqwest::Error` when there is nothing to
/// report; otherwise emits a [`CompletionError::ResponseError`] message with a
/// byte/fragment count so operators can correlate the failure with logs.
fn deepseek_stream_body_error(
    error: reqwest::Error,
    accumulator: &DeepSeekStreamAccumulator,
) -> CompletionError {
    if accumulator.has_partial_output() {
        return CompletionError::ResponseError(format!(
            "DeepSeek stream ended before a usable response: {error} \
             (received {} visible bytes, {} reasoning bytes, {} tool call fragments)",
            accumulator.content.len(),
            accumulator.reasoning_content.len(),
            accumulator.tool_calls.len()
        ));
    }

    CompletionError::HttpError(error)
}

/// Read a non-streaming JSON response body and decode it into a [`ChatResponse`].
///
/// Functional scope: emits a `tracing::debug!` event with byte counts and per-token
/// usage on success so production logs include billing information without
/// requiring a separate metrics path.
async fn read_deepseek_json_response(
    response: reqwest::Response,
) -> Result<ChatResponse, CompletionError> {
    let response_text = response.text().await.map_err(CompletionError::HttpError)?;
    let response =
        serde_json::from_str::<ChatResponse>(&response_text).map_err(CompletionError::JsonError)?;
    tracing::debug!(
        provider = "deepseek",
        body_bytes = response_text.len(),
        choices = response.choices.len(),
        prompt_tokens = response
            .usage
            .as_ref()
            .map(|usage| usage.prompt_tokens)
            .unwrap_or_default(),
        completion_tokens = response
            .usage
            .as_ref()
            .map(|usage| usage.completion_tokens)
            .unwrap_or_default(),
        total_tokens = response
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens)
            .unwrap_or_default(),
        "DeepSeek JSON response decoded"
    );
    Ok(response)
}

/// Decide whether a stream failure should be retried as a non-streaming request.
///
/// Functional scope:
/// - Body and timeout errors are retryable: DeepSeek occasionally drops the
///   socket mid-stream while the same request would have succeeded synchronously.
/// - Custom `ResponseError` messages emitted by [`deepseek_stream_body_error`] are
///   matched verbatim so the retry path stays in lockstep with the wrapper.
/// - All other error categories (HTTP 4xx, JSON malformed, non-implemented) are
///   *not* retried — re-issuing them would only burn tokens.
fn should_retry_deepseek_stream_without_stream(error: &CompletionError) -> bool {
    match error {
        CompletionError::HttpError(error) => error.is_body() || error.is_timeout(),
        CompletionError::ResponseError(message) => {
            message.contains("DeepSeek stream ended before a usable response")
        }
        CompletionError::ProviderError(_)
        | CompletionError::JsonError(_)
        | CompletionError::RequestError(_)
        | CompletionError::NotImplemented(_) => false,
    }
}

fn should_retry_deepseek_request_error(error: &CompletionError) -> bool {
    match error {
        CompletionError::HttpError(error) => {
            error.is_timeout() || error.is_connect() || error.is_request() || error.is_body()
        }
        CompletionError::ProviderError(message) | CompletionError::ResponseError(message) => {
            is_retryable_deepseek_provider_message(message)
        }
        CompletionError::JsonError(_)
        | CompletionError::RequestError(_)
        | CompletionError::NotImplemented(_) => false,
    }
}

fn is_retryable_deepseek_provider_message(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    [
        "status 429",
        "status 500",
        "status 502",
        "status 503",
        "status 504",
        "rate limit",
        "temporarily unavailable",
        "temporarily overloaded",
        "overloaded",
        "try again",
        "timeout",
        "timed out",
        "connection reset",
        "connection closed",
        "error sending request",
        "error decoding response body",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn deepseek_retry_delay(attempt: u32) -> Duration {
    let exp = 2_u64.saturating_pow(attempt.saturating_sub(1));
    Duration::from_millis(
        DEEPSEEK_RETRY_BASE_DELAY_MS
            .saturating_mul(exp)
            .min(DEEPSEEK_RETRY_MAX_DELAY_MS),
    )
}

// ================================================================
// CompletionModel Implementation
// ================================================================

impl CompletionModelTrait for Model {
    type Response = ChatResponse;

    /// Drive a single chat completion against DeepSeek.
    ///
    /// Functional scope:
    /// 1. Build messages — `build_messages_with_reasoning_content` because
    ///    DeepSeek thinking mode requires the previous reasoning to be echoed.
    /// 2. If thinking is enabled, run [`normalize_messages_for_deepseek_thinking`]
    ///    so prior assistant turns without `reasoning_content` are downgraded to
    ///    user notes (otherwise the API rejects the request).
    /// 3. Send the request. For streaming responses, parse with
    ///    [`read_deepseek_stream_response`]; on a salvageable mid-body failure,
    ///    retry once in non-streaming mode.
    /// 4. Decode the chosen result, classify it for tracing, and return the
    ///    provider-agnostic [`CompletionResponse`].
    ///
    /// Boundary conditions:
    /// - Empty `choices` arrays produce [`CompletionError::ResponseError`].
    /// - The retry policy is intentionally limited to a single fallback attempt
    ///   to avoid burning tokens on persistent failures.
    /// - All `tracing::debug!` events tag `provider = "deepseek"` so logs can be
    ///   filtered or aggregated across providers.
    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        let tools = parse_tools(&request.tools);
        let thinking = deepseek_thinking(request.thinking);
        let reasoning_effort = deepseek_reasoning_effort(request.reasoning_effort);
        let mut messages = build_messages_with_reasoning_content(&request)?;
        if deepseek_thinking_enabled(&thinking) {
            let (normalized, converted) = normalize_messages_for_deepseek_thinking(messages);
            if converted > 0 {
                tracing::debug!(
                    provider = "deepseek",
                    converted,
                    "converted assistant history entries without reasoning_content for DeepSeek thinking mode"
                );
            }
            messages = normalized;
        }
        let stream = request.stream.unwrap_or(false);
        let stream_events = request.stream_events.clone();

        let mut deepseek_request = DeepSeekRequest {
            model: self.model.clone(),
            messages,
            temperature: request.temperature,
            tool_choice: if tools.is_empty() {
                None
            } else {
                Some(DeepSeekToolChoice::Auto)
            },
            thinking,
            reasoning_effort,
            tools,
            stream,
        };

        tracing::debug!(
            provider = "deepseek",
            model = %deepseek_request.model,
            stream = deepseek_request.stream,
            messages = deepseek_request.messages.len(),
            tools = deepseek_request.tools.len(),
            has_temperature = deepseek_request.temperature.is_some(),
            thinking = ?deepseek_request.thinking.as_ref(),
            reasoning_effort = ?deepseek_request.reasoning_effort.as_ref(),
            "DeepSeek completion request started"
        );

        let response = self.send_chat_completion_request(&deepseek_request).await?;

        let deepseek_response = if stream {
            match read_deepseek_stream_response(response, &self.model, stream_events.as_ref()).await
            {
                Ok(response) => response,
                Err(error) if should_retry_deepseek_stream_without_stream(&error) => {
                    tracing::warn!(
                        provider = "deepseek",
                        error = %error,
                        "DeepSeek stream failed before a usable response; retrying once without stream"
                    );
                    deepseek_request.stream = false;
                    let response = self.send_chat_completion_request(&deepseek_request).await?;
                    read_deepseek_json_response(response).await?
                }
                Err(error) => return Err(error),
            }
        } else {
            read_deepseek_json_response(response).await?
        };

        let choice = deepseek_response
            .choices
            .first()
            .ok_or_else(|| CompletionError::ResponseError("No choices in response".to_string()))?;

        let response_kind = classify_deepseek_choice(choice);
        if matches!(
            response_kind,
            DeepSeekResponseKind::ReasoningOnly | DeepSeekResponseKind::Empty
        ) {
            tracing::debug!(
                provider = "deepseek",
                response_kind = ?response_kind,
                "DeepSeek returned no text or tool calls"
            );
        }
        let content = parse_choice_content_for_provider("deepseek", choice)?;
        let reasoning_content = choice_reasoning_content(choice);
        let text_parts = content
            .iter()
            .filter(|part| {
                matches!(
                    part,
                    crate::internal::ai::completion::AssistantContent::Text(_)
                )
            })
            .count();
        let tool_calls = content
            .iter()
            .filter(|part| {
                matches!(
                    part,
                    crate::internal::ai::completion::AssistantContent::ToolCall(_)
                )
            })
            .count();
        tracing::debug!(
            provider = "deepseek",
            model = %deepseek_response.model,
            response_id = %deepseek_response.id,
            response_kind = ?response_kind,
            finish_reason = choice.finish_reason.as_deref().unwrap_or(""),
            choices = deepseek_response.choices.len(),
            text_parts,
            tool_calls,
            reasoning_bytes = reasoning_content.as_deref().map(str::len).unwrap_or_default(),
            prompt_tokens = deepseek_response
                .usage
                .as_ref()
                .map(|usage| usage.prompt_tokens)
                .unwrap_or_default(),
            completion_tokens = deepseek_response
                .usage
                .as_ref()
                .map(|usage| usage.completion_tokens)
                .unwrap_or_default(),
            total_tokens = deepseek_response
                .usage
                .as_ref()
                .map(|usage| usage.total_tokens)
                .unwrap_or_default(),
            "DeepSeek completion response parsed"
        );

        Ok(CompletionResponse {
            content,
            reasoning_content,
            raw_response: deepseek_response,
        })
    }
}

impl CompletionClient for Client {
    type Model = Model;

    /// Bind a model identifier to this client and return a ready-to-use [`Model`].
    fn completion_model(&self, model: impl Into<String>) -> Self::Model {
        Model::new(self.clone(), model)
    }
}

/// Backwards-compatible type alias.
pub type CompletionModel = Model;

/// Type alias for the raw response type.
pub type DeepSeekResponse = ChatResponse;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::ai::providers::openai_compat::{ChatFunctionDefinition, ChatMessage};

    /// Scenario: pin the on-the-wire request shape, especially `stream` which is
    /// always serialised to avoid DeepSeek's "absent means streaming" quirk.
    #[test]
    fn test_deepseek_request_serialization() {
        let request = DeepSeekRequest {
            model: "deepseek-chat".to_string(),
            messages: vec![
                ChatMessage::System {
                    content: "You are a helpful assistant.".to_string(),
                },
                ChatMessage::User {
                    content: "Hello!".to_string(),
                },
            ],
            temperature: Some(0.7),
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            reasoning_effort: None,
            stream: false,
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"model\":\"deepseek-chat\""));
        assert!(json.contains("\"temperature\":0.7"));
        assert!(json.contains("\"stream\":false"));
    }

    /// Scenario: DeepSeek serialises `tool_choice = Auto` as a bare string
    /// (`"auto"`), not the OpenAI-style object form. Pin that distinction.
    #[test]
    fn test_deepseek_tool_choice_serialization() {
        let request = DeepSeekRequest {
            model: "deepseek-chat".to_string(),
            messages: vec![ChatMessage::User {
                content: "hi".to_string(),
            }],
            temperature: None,
            tools: vec![ChatToolDefinition {
                r#type: "function".to_string(),
                function: ChatFunctionDefinition {
                    name: "read_file".to_string(),
                    description: "Read file".to_string(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "file_path": {"type": "string"}
                        },
                        "required": ["file_path"]
                    }),
                },
            }],
            tool_choice: Some(DeepSeekToolChoice::Auto),
            thinking: None,
            reasoning_effort: None,
            stream: false,
        };

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["tool_choice"], "auto");
    }

    /// Scenario: when thinking + reasoning_effort are both supplied, they must
    /// arrive on the wire as the nested `thinking.type = enabled` object plus a
    /// flat `reasoning_effort` string.
    #[test]
    fn test_deepseek_reasoning_request_serialization() {
        let request = DeepSeekRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![ChatMessage::User {
                content: "Hello!".to_string(),
            }],
            temperature: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: deepseek_thinking(Some(CompletionThinking::Enabled)),
            reasoning_effort: deepseek_reasoning_effort(Some(CompletionReasoningEffort::High)),
            stream: false,
        };

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["thinking"]["type"], "enabled");
        assert_eq!(json["reasoning_effort"], "high");
        assert_eq!(json["stream"], false);
    }

    /// Scenario: prior assistant turns without `reasoning_content` are
    /// disallowed in thinking mode. Verify the helper rewrites them as
    /// user-role recap notes that carry forward the original text.
    #[test]
    fn deepseek_thinking_normalizes_synthetic_assistant_messages_without_reasoning() {
        let messages = vec![
            ChatMessage::User {
                content: "Build an intent".to_string(),
            },
            ChatMessage::Assistant {
                content: Some("IntentSpec ready for review.".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
            },
            ChatMessage::User {
                content: "Create the execution plan".to_string(),
            },
        ];

        let (normalized, converted) = normalize_messages_for_deepseek_thinking(messages);

        assert_eq!(converted, 1);
        assert_eq!(normalized.len(), 3);
        assert!(matches!(
            &normalized[1],
            ChatMessage::User { content }
                if content.contains("Previous assistant message")
                    && content.contains("IntentSpec ready for review.")
        ));
    }

    /// Scenario: assistant messages that *do* carry `reasoning_content` are
    /// already valid in thinking mode and must pass through untouched.
    #[test]
    fn deepseek_thinking_keeps_assistant_messages_with_reasoning_content() {
        let messages = vec![ChatMessage::Assistant {
            content: None,
            reasoning_content: Some("Need to submit the draft.".to_string()),
            tool_calls: vec![ChatToolCall {
                id: "call_1".to_string(),
                r#type: "function".to_string(),
                function: ChatFunctionCall {
                    name: "submit_intent_draft".to_string(),
                    arguments: "{\"draft\":{}}".to_string(),
                },
            }],
        }];

        let (normalized, converted) = normalize_messages_for_deepseek_thinking(messages);

        assert_eq!(converted, 0);
        assert!(matches!(
            &normalized[0],
            ChatMessage::Assistant {
                reasoning_content: Some(reasoning),
                tool_calls,
                ..
            } if reasoning == "Need to submit the draft." && tool_calls.len() == 1
        ));
    }

    /// Scenario: tool exchanges that follow an assistant turn without
    /// `reasoning_content` must collapse into a single user recap, dropping
    /// only the turns that thinking mode cannot accept.
    #[test]
    fn deepseek_thinking_collapses_unusable_tool_history_without_reasoning_content() {
        let messages = vec![
            ChatMessage::Assistant {
                content: Some("I will inspect the repo.".to_string()),
                reasoning_content: None,
                tool_calls: vec![ChatToolCall {
                    id: "call_1".to_string(),
                    r#type: "function".to_string(),
                    function: ChatFunctionCall {
                        name: "read_file".to_string(),
                        arguments: "{\"file_path\":\"Cargo.toml\"}".to_string(),
                    },
                }],
            },
            ChatMessage::Tool {
                tool_call_id: "call_1".to_string(),
                name: "read_file".to_string(),
                content: "{\"content\":\"[package]\"}".to_string(),
            },
            ChatMessage::User {
                content: "Continue".to_string(),
            },
        ];

        let (normalized, converted) = normalize_messages_for_deepseek_thinking(messages);

        assert_eq!(converted, 1);
        assert_eq!(normalized.len(), 2);
        assert!(matches!(
            &normalized[0],
            ChatMessage::User { content }
                if content.contains("Previous assistant tool calls")
                    && content.contains("read_file")
                    && content.contains("Previous tool result")
        ));
        assert!(matches!(&normalized[1], ChatMessage::User { content } if content == "Continue"));
    }

    /// Scenario: opt-in streaming must surface as `"stream": true` so the API
    /// dispatches the SSE codepath rather than the JSON one.
    #[test]
    fn test_deepseek_stream_request_serialization() {
        let request = DeepSeekRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![ChatMessage::User {
                content: "Hello!".to_string(),
            }],
            temperature: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            reasoning_effort: None,
            stream: true,
        };

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["stream"], true);
    }

    /// Scenario: a multi-chunk text stream with intermediate reasoning content
    /// and a final `[DONE]` marker must produce a fully-merged response while
    /// also publishing the per-delta stream events the Code UI consumes.
    #[test]
    fn test_deepseek_stream_accumulates_text_and_usage() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut accumulator = DeepSeekStreamAccumulator::default();

        let done = process_deepseek_stream_line(
            br#"data: {"id":"chunk_1","choices":[{"index":0,"delta":{"content":"Hello","role":"assistant"},"finish_reason":null}],"created":1718345013,"model":"deepseek-v4-pro","object":"chat.completion.chunk","usage":null}"#,
            &mut accumulator,
            Some(&tx),
        )
        .unwrap();
        assert!(!done);

        process_deepseek_stream_line(
            br#"data: {"id":"chunk_1","choices":[{"index":0,"delta":{"reasoning_content":"thinking"},"finish_reason":null}],"created":1718345013,"model":"deepseek-v4-pro","object":"chat.completion.chunk","usage":null}"#,
            &mut accumulator,
            Some(&tx),
        )
        .unwrap();

        process_deepseek_stream_line(
            br#"data: {"id":"chunk_1","choices":[{"index":0,"delta":{"content":"!"},"finish_reason":"stop"}],"created":1718345013,"model":"deepseek-v4-pro","object":"chat.completion.chunk","usage":{"completion_tokens":2,"prompt_tokens":3,"total_tokens":5}}"#,
            &mut accumulator,
            Some(&tx),
        )
        .unwrap();

        let done =
            process_deepseek_stream_line(b"data: [DONE]", &mut accumulator, Some(&tx)).unwrap();
        assert!(done);

        let response = accumulator.into_response("fallback-model").unwrap();
        assert_eq!(response.id, "chunk_1");
        assert_eq!(response.model, "deepseek-v4-pro");
        assert_eq!(
            choice_reasoning_content(&response.choices[0]).as_deref(),
            Some("thinking")
        );
        assert_eq!(
            response.usage.as_ref().map(|usage| usage.total_tokens),
            Some(5)
        );
        let content = parse_choice_content_for_provider("deepseek", &response.choices[0]).unwrap();
        assert!(matches!(
            &content[0],
            crate::internal::ai::completion::AssistantContent::Text(text) if text.text == "Hello!"
        ));

        let first = rx.try_recv().unwrap();
        assert!(matches!(
            first,
            CompletionStreamEvent::TextDelta { delta, .. } if delta == "Hello"
        ));
        let second = rx.try_recv().unwrap();
        assert!(matches!(
            second,
            CompletionStreamEvent::ThinkingDelta { delta, .. } if delta == "thinking"
        ));
        let third = rx.try_recv().unwrap();
        assert!(matches!(
            third,
            CompletionStreamEvent::TextDelta { delta, .. } if delta == "!"
        ));
    }

    /// Scenario: a streaming tool call with full arguments in a single chunk
    /// must reconstruct as a complete tool call and emit a `ToolCallPreview`
    /// event so the UI can render the call before completion.
    #[test]
    fn test_deepseek_stream_accumulates_tool_calls() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut accumulator = DeepSeekStreamAccumulator::default();

        process_deepseek_stream_line(
            br#"data: {"id":"chunk_2","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"file_path\":\"Cargo.toml\"}"}}]},"finish_reason":"tool_calls"}],"created":1718345014,"model":"deepseek-v4-pro","object":"chat.completion.chunk"}"#,
            &mut accumulator,
            Some(&tx),
        )
        .unwrap();

        let response = accumulator.into_response("fallback-model").unwrap();
        let content = parse_choice_content_for_provider("deepseek", &response.choices[0]).unwrap();
        assert!(matches!(
            &content[0],
            crate::internal::ai::completion::AssistantContent::ToolCall(tool_call)
                if tool_call.id == "call_1"
                    && tool_call.function.name == "read_file"
                    && tool_call.function.arguments == serde_json::json!({"file_path": "Cargo.toml"})
        ));

        let event = rx.try_recv().unwrap();
        assert!(matches!(
            event,
            CompletionStreamEvent::ToolCallPreview { tool_name, .. } if tool_name == "read_file"
        ));
    }

    /// Scenario: only text or completed tool calls count as "salvageable" —
    /// reasoning-only output must not trigger the warn-and-continue path,
    /// because the agent loop has nothing to act on.
    #[test]
    fn test_deepseek_stream_salvageable_response_detection() {
        let mut reasoning_only = DeepSeekStreamAccumulator::default();
        process_deepseek_stream_line(
            br#"data: {"id":"chunk_3","choices":[{"index":0,"delta":{"reasoning_content":"thinking only"},"finish_reason":null}],"created":1718345015,"model":"deepseek-v4-pro","object":"chat.completion.chunk"}"#,
            &mut reasoning_only,
            None,
        )
        .unwrap();
        assert!(reasoning_only.has_partial_output());
        assert!(!reasoning_only.has_salvageable_response());

        let mut with_text = DeepSeekStreamAccumulator::default();
        process_deepseek_stream_line(
            br#"data: {"id":"chunk_4","choices":[{"index":0,"delta":{"content":"usable"},"finish_reason":null}],"created":1718345016,"model":"deepseek-v4-pro","object":"chat.completion.chunk"}"#,
            &mut with_text,
            None,
        )
        .unwrap();
        assert!(with_text.has_salvageable_response());

        let mut with_tool_call = DeepSeekStreamAccumulator::default();
        process_deepseek_stream_line(
            br#"data: {"id":"chunk_5","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"submit_plan_draft","arguments":"{\"steps\":[\"inspect\"]}"}}]},"finish_reason":"tool_calls"}],"created":1718345017,"model":"deepseek-v4-pro","object":"chat.completion.chunk"}"#,
            &mut with_tool_call,
            None,
        )
        .unwrap();
        assert!(with_tool_call.has_salvageable_response());
    }

    /// Scenario: a stream body error after only a tool name must not be treated
    /// as a usable response; otherwise an incomplete call like `shell {}` can
    /// reach the dispatcher and fail as a malformed tool invocation.
    #[test]
    fn test_deepseek_stream_name_only_tool_call_not_salvageable() {
        let mut accumulator = DeepSeekStreamAccumulator::default();
        process_deepseek_stream_line(
            br#"data: {"id":"chunk_6","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"shell"}}]},"finish_reason":null}],"created":1718345018,"model":"deepseek-v4-pro","object":"chat.completion.chunk"}"#,
            &mut accumulator,
            None,
        )
        .unwrap();

        assert!(accumulator.has_partial_output());
        assert!(!accumulator.has_salvageable_response());
    }

    /// Scenario: the descriptive `ResponseError` produced when a stream ends
    /// before usable output must be matched by the retry predicate so the
    /// agent transparently falls back to the non-streaming endpoint.
    #[test]
    fn test_deepseek_stream_body_error_triggers_non_stream_fallback() {
        let error = CompletionError::ResponseError(
            "DeepSeek stream ended before a usable response: error decoding response body"
                .to_string(),
        );

        assert!(should_retry_deepseek_stream_without_stream(&error));
    }

    #[test]
    fn test_deepseek_request_retry_predicate_accepts_transport_text() {
        let provider_error = CompletionError::ProviderError(
            "error sending request for url (https://api.deepseek.com/chat/completions)".into(),
        );
        let status_error = CompletionError::ProviderError("status 503: overloaded".into());
        let response_error = CompletionError::ResponseError("connection reset by peer".into());

        assert!(should_retry_deepseek_request_error(&provider_error));
        assert!(should_retry_deepseek_request_error(&status_error));
        assert!(should_retry_deepseek_request_error(&response_error));
    }

    #[test]
    fn test_deepseek_request_retry_predicate_rejects_non_transients() {
        let provider_error = CompletionError::ProviderError("status 400: bad request".into());
        let json_error = serde_json::from_str::<serde_json::Value>("{")
            .expect_err("invalid json should produce an error");

        assert!(!should_retry_deepseek_request_error(&provider_error));
        assert!(!should_retry_deepseek_request_error(
            &CompletionError::JsonError(json_error)
        ));
    }

    #[test]
    fn test_deepseek_retry_delay_is_bounded_exponential_backoff() {
        assert_eq!(deepseek_retry_delay(1), Duration::from_millis(250));
        assert_eq!(deepseek_retry_delay(2), Duration::from_millis(500));
        assert_eq!(deepseek_retry_delay(10), Duration::from_millis(2_000));
    }

    /// Scenario: `Max` must serialise as the lowercase string `"max"` per
    /// DeepSeek's wire spec; a stray capitalisation would be silently rejected.
    #[test]
    fn test_deepseek_max_reasoning_effort_serialization() {
        let effort = deepseek_reasoning_effort(Some(CompletionReasoningEffort::Max));

        let json = serde_json::to_value(effort).unwrap();
        assert_eq!(json, "max");
    }

    /// Scenario: a canonical text-only response should round-trip through
    /// serde with usage stats intact.
    #[test]
    fn test_deepseek_response_deserialization() {
        let json = r#"
        {
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1677652288,
            "model": "deepseek-chat",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello there!"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 9,
                "completion_tokens": 12,
                "total_tokens": 21
            }
        }
        "#;

        let response: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.id, "chatcmpl-123");
        assert_eq!(response.model, "deepseek-chat");
        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.usage.unwrap().total_tokens, 21);
    }

    /// Scenario: thinking-mode responses arrive with `reasoning_content` plus
    /// a tool call; both fields must decode and surface through the helper
    /// accessors.
    #[test]
    fn test_deepseek_reasoning_content_deserialization() {
        let json = r#"
        {
            "id": "chatcmpl-456",
            "object": "chat.completion",
            "created": 1677652288,
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "I need to inspect the repository before editing.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"Cargo.toml\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": null
        }
        "#;

        let response: ChatResponse = serde_json::from_str(json).unwrap();
        let choice = &response.choices[0];
        let content = parse_choice_content_for_provider("deepseek", choice).unwrap();

        assert_eq!(
            choice_reasoning_content(choice).as_deref(),
            Some("I need to inspect the repository before editing.")
        );
        assert!(matches!(
            &content[0],
            crate::internal::ai::completion::AssistantContent::ToolCall(tool_call)
                if tool_call.id == "call_1"
                    && tool_call.function.name == "read_file"
        ));
    }

    #[test]
    fn test_deepseek_repairs_malformed_tool_arguments() {
        let json = r#"
        {
            "id": "chatcmpl-repair",
            "object": "chat.completion",
            "created": 1677652288,
            "model": "deepseek-chat",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{file_path: 'Cargo.toml',}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": null
        }
        "#;

        let response: ChatResponse = serde_json::from_str(json).unwrap();
        let content = parse_choice_content_for_provider("deepseek", &response.choices[0]).unwrap();

        assert!(matches!(
            &content[0],
            crate::internal::ai::completion::AssistantContent::ToolCall(tool_call)
                if tool_call.function.arguments == serde_json::json!({"file_path": "Cargo.toml"})
        ));
    }

    /// Scenario: cover all four classifier branches — text only, text+tool,
    /// reasoning only, and empty — so the tracing labels never drift.
    #[test]
    fn deepseek_classifies_text_tool_reasoning_only_and_empty_responses() {
        let text_choice = ChatChoice {
            index: 0,
            message: ChatMessage::Assistant {
                content: Some("hello".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
            },
            finish_reason: Some("stop".to_string()),
        };
        assert_eq!(
            classify_deepseek_choice(&text_choice),
            DeepSeekResponseKind::TextOrTool
        );

        let tool_choice = ChatChoice {
            index: 0,
            message: ChatMessage::Assistant {
                content: Some(String::new()),
                reasoning_content: Some("thinking".to_string()),
                tool_calls: vec![ChatToolCall {
                    id: "call_1".to_string(),
                    r#type: "function".to_string(),
                    function: ChatFunctionCall {
                        name: "read_file".to_string(),
                        arguments: "{\"file_path\":\"Cargo.toml\"}".to_string(),
                    },
                }],
            },
            finish_reason: Some("tool_calls".to_string()),
        };
        assert_eq!(
            classify_deepseek_choice(&tool_choice),
            DeepSeekResponseKind::TextOrTool
        );

        let reasoning_only_choice = ChatChoice {
            index: 0,
            message: ChatMessage::Assistant {
                content: Some(String::new()),
                reasoning_content: Some("thinking only".to_string()),
                tool_calls: Vec::new(),
            },
            finish_reason: Some("stop".to_string()),
        };
        assert_eq!(
            classify_deepseek_choice(&reasoning_only_choice),
            DeepSeekResponseKind::ReasoningOnly
        );

        let empty_choice = ChatChoice {
            index: 0,
            message: ChatMessage::Assistant {
                content: Some("   ".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
            },
            finish_reason: Some("stop".to_string()),
        };
        assert_eq!(
            classify_deepseek_choice(&empty_choice),
            DeepSeekResponseKind::Empty
        );
    }

    /// Scenario: smoke-test that `Model::new` stores the model identifier
    /// verbatim.
    #[test]
    fn test_model_new() {
        let client = Client::with_api_key("test-key".to_string());
        let model = Model::new(client, "deepseek-chat");
        assert_eq!(model.model_name(), "deepseek-chat");
    }

    /// Scenario: `CompletionClient::completion_model` is the canonical entry
    /// point used by the agent runtime; verify it produces a `Model` bound to
    /// the requested identifier.
    #[test]
    fn test_client_completion_model() {
        let client = Client::with_api_key("test-key".to_string());
        let model = client.completion_model("deepseek-chat");
        assert_eq!(model.model_name(), "deepseek-chat");
    }
}
