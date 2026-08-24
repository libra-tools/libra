//! Provider request/response transform pipeline (OC-Phase 4 P4.1).
//!
//! Centralises the per-provider normalisation rules that were previously
//! scattered across each provider's `completion.rs`. The trait operates on
//! the **canonical** `CompletionRequest` / `CompletionResponse` envelopes
//! — wire-format quirks (empty-content whitespace insertion, `tool_call_id`
//! shape, JSON-schema mapping) continue to live in each provider's
//! `build_messages` / response parser, because they need access to the
//! provider-specific wire types. This module is for normalisation that can
//! and should happen **before** the wire conversion (drop reasoning content
//! the provider would 400 on, validate `ToolResult.name` for providers that
//! require it) or **after** (clean up emitted `reasoning_content` so the
//! handoff stays consistent across providers).
//!
//! ## Design constraints
//!
//! 1. Every transform must be **idempotent**. The runtime applies the same
//!    transform on every turn; running it twice on the same request must
//!    produce the same output. This means in particular: `prepare_request`
//!    cannot append to mutable Vecs without a guard, and
//!    `finalize_response` cannot rewrite `reasoning_content` if the second
//!    pass would produce different text.
//! 2. The trait is **object-safe**. Callers reach a transform via
//!    `transform_for(provider_id) -> &'static dyn ProviderTransform`, so
//!    new providers can be plugged in without an extra generic parameter
//!    on every callsite (see `AnyCompletionModel::completion` in
//!    [`super::runtime`]).
//! 3. The transform must **never** panic on inputs the runtime can produce.
//!    Instead, return [`CompletionError::ProviderError`] with a message that
//!    points the operator at the specific request/response field that broke
//!    the contract. The runtime translates this into a Failed task with a
//!    human-readable error.
//! 4. **No I/O, no clock reads, no `RefCell`.** The transform must be a
//!    pure function of `(provider_id, model_id, request)`. Tests rely on
//!    this to assert deterministic output.
//!
//! ## Quirks covered in this version
//!
//! | Provider          | Request quirks                                          | Response quirks |
//! |-------------------|---------------------------------------------------------|-----------------|
//! | OpenAI            | strip `reasoning_content` from history (would 400)      | none            |
//! | DeepSeek / Kimi   | preserve `reasoning_content` (chain-of-thought handoff) | none            |
//! | Zhipu             | strip `reasoning_content` (provider rejects it)         | none            |
//! | Ollama            | strip `reasoning_content`                               | none            |
//! | Anthropic         | drop assistant turns whose content is fully empty       | trim trailing whitespace from emitted reasoning |
//! | Gemini            | require non-empty `ToolResult.name`                     | none            |
//!
//! Wire-format quirks (e.g. Anthropic's empty-string insertion in
//! `build_messages`, Gemini's `systemInstruction` shape) intentionally
//! remain in their provider's `completion.rs` — they need access to types
//! that only the wire layer sees.
//!
//! ## Variants
//!
//! Each transform also exposes a [`variants`](ProviderTransform::variants)
//! list — the provider-side capability flags applicable to a given model
//! (e.g. `"thinking"`, `"interleaved"`). The list mirrors opencode's
//! `transform.ts:761-770` `variants(model)` substring-match pattern so new
//! reasoning models can be added by appending an id to the corresponding
//! constant slice (see [`super::capability`]). The list is read by tests
//! and the agent profile loader; it does not feed the wire request.

use super::runtime::provider_id;
use crate::internal::ai::completion::{
    AssistantContent, CompletionError, CompletionRequest, Message, UserContent,
};

/// Substring-matched model ids that expose a reasoning / thinking knob, per
/// provider. Mirrors opencode's `variants(model)` table; adding a new
/// reasoning model means appending a substring to the relevant slice and
/// extending the unit test coverage. The match is `to_lowercase().contains`
/// so suffixes like `-2025-04-01` keep matching the canonical id prefix.
mod reasoning_ids {
    pub const ANTHROPIC: &[&str] = &[
        "claude-3-7-sonnet",
        "claude-opus-4",
        "claude-sonnet-4",
        "claude-opus-4-5",
        "claude-sonnet-4-5",
    ];
    pub const OPENAI: &[&str] = &["o1", "o3", "o4", "gpt-5"];
    pub const DEEPSEEK: &[&str] = &["deepseek-reasoner", "deepseek-r1"];
    pub const KIMI: &[&str] = &["kimi-thinking", "kimi-k1.5", "kimi-k2"];
    pub const GEMINI: &[&str] = &["gemini-2.5", "gemini-2-5"];
    pub const ZHIPU: &[&str] = &["glm-4.5", "glm-z1"];
}

/// Variant identifier strings emitted by [`ProviderTransform::variants`].
/// Stable, snake_case, lowercased; read by tests and the agent profile
/// loader. Adding a new variant requires updating this list and at least
/// one provider transform.
pub mod variant {
    /// The model exposes a reasoning / chain-of-thought channel.
    pub const REASONING: &str = "reasoning";
    /// The model can interleave reasoning with tool calls within one turn.
    pub const INTERLEAVED: &str = "interleaved";
    /// The model accepts cache-control hints on prompt segments.
    pub const CACHE_CONTROL: &str = "cache_control";
}

/// Error wrapper for transform failures. Converted to
/// [`CompletionError::ProviderError`] at the call site so a missing
/// `ToolResult.name` surfaces the same way as any other provider-side
/// rejection.
#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    #[error("transform '{provider}' rejected request: {reason}")]
    InvalidRequest {
        provider: &'static str,
        reason: String,
    },
}

impl From<TransformError> for CompletionError {
    fn from(err: TransformError) -> Self {
        CompletionError::ProviderError(err.to_string())
    }
}

/// Provider-specific request/response normalisation hook.
///
/// Implementations live below this trait definition, one per supported
/// provider. The runtime never instantiates these structs — it reaches them
/// through [`transform_for`] which returns a static reference, so the
/// transforms behave like singleton policy objects.
pub trait ProviderTransform: Send + Sync + std::fmt::Debug {
    /// Stable provider id (matches [`provider_id`]).
    fn provider_id(&self) -> &'static str;

    /// Variants the provider supports for `model_id`.
    ///
    /// Return value is a slice of strings drawn from the [`variant`] module.
    /// An empty list means "no opt-in features beyond the baseline chat
    /// completions contract". Order is not significant.
    fn variants(&self, model_id: &str) -> Vec<&'static str> {
        let _ = model_id;
        Vec::new()
    }

    /// Apply provider-specific normalisation to `request` before it is
    /// handed to the wire-format builder.
    ///
    /// Default impl is a no-op so providers without canonical-level quirks
    /// can omit the override.
    fn prepare_request(
        &self,
        model_id: &str,
        request: &mut CompletionRequest,
    ) -> Result<(), TransformError> {
        let _ = (model_id, request);
        Ok(())
    }

    /// Apply provider-specific cleanup to a parsed response.
    ///
    /// Operates on the canonical `content` and `reasoning_content` fields
    /// (the same fields as [`CompletionResponse`](
    /// crate::internal::ai::completion::CompletionResponse)) so the
    /// transform stays generic over the provider's `raw_response` type.
    fn finalize_response(
        &self,
        model_id: &str,
        content: &mut Vec<AssistantContent>,
        reasoning_content: &mut Option<String>,
    ) -> Result<(), TransformError> {
        let _ = (model_id, content, reasoning_content);
        Ok(())
    }
}

/// Look up the canonical transform for a provider id.
///
/// Unknown ids fall through to a no-op transform so callers stay panic-free
/// when the runtime hands us an id we have not catalogued yet (e.g. a fake
/// provider in tests). The runtime additionally guards against this case at
/// construction time by validating provider ids against [`provider_id`].
pub fn transform_for(provider: &str) -> &'static dyn ProviderTransform {
    match provider {
        provider_id::ANTHROPIC => &AnthropicTransform,
        provider_id::OPENAI => &OpenAiTransform,
        provider_id::DEEPSEEK => &DeepSeekTransform,
        provider_id::GEMINI => &GeminiTransform,
        provider_id::KIMI => &KimiTransform,
        provider_id::ZHIPU => &ZhipuTransform,
        provider_id::OLLAMA => &OllamaTransform,
        _ => &NoopTransform,
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Returns `true` if `model_id` (case-insensitively) contains any of the
/// substrings in `ids`. Mirrors opencode's `variants(model)` matching rule.
fn matches_any(model_id: &str, ids: &[&str]) -> bool {
    let normalised = model_id.to_ascii_lowercase();
    ids.iter().any(|s| normalised.contains(s))
}

/// Reject any `Message::System` whose content carries a non-Text part
/// (`UserContent::Image`, `UserContent::ToolResult`). Every production
/// provider's wire builder forwards only `UserContent::Text` from System
/// messages — the other variants would silently disappear on the wire,
/// turning a documented data-bearing message into nothing. Failing fast
/// here surfaces the schema misuse with the offending index instead of
/// letting it vanish.
///
/// Called unconditionally by [`AnyCompletionModel::completion`](
/// crate::internal::ai::providers::AnyCompletionModel) before any
/// provider-specific [`ProviderTransform::prepare_request`], so providers
/// do not need to repeat the check inside their own transform.
pub fn reject_non_text_system_content(
    request: &CompletionRequest,
    provider: &'static str,
) -> Result<(), TransformError> {
    for (idx, message) in request.chat_history.iter().enumerate() {
        if let Message::System { content } = message {
            for part in content.iter() {
                let kind = match part {
                    UserContent::Text(_) => continue,
                    UserContent::Image(_) => "Image",
                    UserContent::ToolResult(result) => {
                        return Err(TransformError::InvalidRequest {
                            provider,
                            reason: format!(
                                "ToolResult at chat_history[{idx}] (id={}) is embedded \
                                 in a System message; provider {provider} forwards only \
                                 text content from System, so the tool_result would \
                                 silently disappear on the wire — move it to a User message",
                                result.id
                            ),
                        });
                    }
                };
                return Err(TransformError::InvalidRequest {
                    provider,
                    reason: format!(
                        "{kind} at chat_history[{idx}] is embedded in a System message; \
                         provider {provider} forwards only text content from System, so \
                         the {kind} would silently disappear on the wire — move it to a \
                         User message"
                    ),
                });
            }
        }
    }
    Ok(())
}

/// Strip `reasoning_content` from every assistant turn in `request.chat_history`.
/// Used by providers (OpenAI, Zhipu, Ollama) that reject the field.
fn strip_assistant_reasoning(request: &mut CompletionRequest) {
    for message in request.chat_history.iter_mut() {
        if let Message::Assistant {
            reasoning_content, ..
        } = message
        {
            *reasoning_content = None;
        }
    }
    // Top-level cached reasoning text on the request envelope is not part of
    // the wire payload for any current provider — there is no analogous
    // field to strip.
}

/// Drop assistant turns where every content part is an empty Text. Anthropic
/// rejects messages whose `content` array carries a single zero-length
/// block, but it tolerates assistant turns with at least one non-empty
/// content part. Tool calls are *never* empty in the canonical sense, so a
/// turn carrying a ToolCall is preserved even when its accompanying Text is
/// empty — this also makes the operation safe for tool_use/tool_result
/// pairing: only turns with no `ToolCall` parts are eligible for removal,
/// so dropping one cannot orphan a downstream `ToolResult`.
fn drop_empty_assistant_turns(request: &mut CompletionRequest) {
    request.chat_history.retain(|message| match message {
        Message::Assistant { content, .. } => content.iter().any(|part| match part {
            AssistantContent::Text(text) => !text.text.trim().is_empty(),
            AssistantContent::ToolCall(_) => true,
        }),
        _ => true,
    });
}

/// Validate Anthropic's tool_use / tool_result pairing rule: every
/// `UserContent::ToolResult{ id }` must be preceded earlier in the
/// transcript by an `AssistantContent::ToolCall { id }` with the same id,
/// each tool-call id appears at most once across the transcript, and each
/// tool-call id is paired with at most one tool result. Anthropic rejects
/// orphan, duplicate, or mismatched-id tool_results with a 400 that is
/// hard to trace back to the offending message; failing fast here points
/// the caller at the exact `chat_history[i]` index.
///
/// `System` messages get a stricter check: Anthropic's wire system
/// extractor only forwards `UserContent::Text` from system messages, so
/// any `ToolResult` embedded there would silently vanish on the wire —
/// even when its id pairs with a prior `ToolCall`. A paired
/// `Assistant(ToolCall) → System(ToolResult)` shape would therefore turn
/// into an orphan `tool_use` on the wire and trigger an opaque 400. The
/// validator rejects every System `ToolResult` unconditionally, regardless
/// of pairing, so the contract violation surfaces with the offending id
/// instead of vanishing.
///
/// `ToolCall` is only reachable via `AssistantContent`, so System
/// messages have no symmetric assistant-side check to perform.
///
/// Idempotent: the second pass over the same history reaches the same
/// set of pairings.
fn require_anthropic_tool_pairing(request: &CompletionRequest) -> Result<(), TransformError> {
    use std::collections::HashSet;

    let mut pending_calls: HashSet<String> = HashSet::new();
    let mut seen_call_ids: HashSet<String> = HashSet::new();
    for (idx, message) in request.chat_history.iter().enumerate() {
        match message {
            Message::Assistant { content, .. } => {
                for part in content.iter() {
                    if let AssistantContent::ToolCall(call) = part {
                        if !seen_call_ids.insert(call.id.clone()) {
                            return Err(TransformError::InvalidRequest {
                                provider: provider_id::ANTHROPIC,
                                reason: format!(
                                    "ToolCall at chat_history[{idx}] (id={}) reuses an \
                                     id that already appeared earlier in the transcript; \
                                     Anthropic requires every tool_use id to be unique",
                                    call.id
                                ),
                            });
                        }
                        pending_calls.insert(call.id.clone());
                    }
                }
            }
            Message::User { content } => {
                check_user_tool_result_pairing(content.iter(), idx, &mut pending_calls)?;
            }
            Message::System { content } => {
                reject_system_tool_results(content.iter(), idx)?;
            }
        }
    }
    Ok(())
}

/// User-arm pairing check: every ToolResult must consume one pending
/// ToolCall id; orphans and duplicates fail. Mutating `pending_calls`
/// enforces the "each ToolCall id pairs with at most one ToolResult"
/// invariant.
fn check_user_tool_result_pairing<'a, I>(
    content: I,
    idx: usize,
    pending_calls: &mut std::collections::HashSet<String>,
) -> Result<(), TransformError>
where
    I: IntoIterator<Item = &'a UserContent>,
{
    for part in content {
        if let UserContent::ToolResult(result) = part
            && !pending_calls.remove(&result.id)
        {
            return Err(TransformError::InvalidRequest {
                provider: provider_id::ANTHROPIC,
                reason: format!(
                    "ToolResult at chat_history[{idx}] (id={}) has no \
                     preceding ToolCall with matching id; Anthropic \
                     requires every tool_result to follow the assistant \
                     tool_use that produced it (or to be paired only once)",
                    result.id
                ),
            });
        }
    }
    Ok(())
}

/// System-arm guard: Anthropic's wire system extractor drops anything
/// that is not `UserContent::Text`, so a `ToolResult` embedded in a
/// System message would vanish silently on the wire — even when paired.
/// Reject unconditionally to surface the contract violation with the
/// offending tool-call id.
fn reject_system_tool_results<'a, I>(content: I, idx: usize) -> Result<(), TransformError>
where
    I: IntoIterator<Item = &'a UserContent>,
{
    for part in content {
        if let UserContent::ToolResult(result) = part {
            return Err(TransformError::InvalidRequest {
                provider: provider_id::ANTHROPIC,
                reason: format!(
                    "ToolResult at chat_history[{idx}] (id={}) is embedded in a \
                     System message; Anthropic's system extractor only forwards \
                     text content, so the tool_result would silently disappear on \
                     the wire — move it to a User message",
                    result.id
                ),
            });
        }
    }
    Ok(())
}

/// Validate that every `UserContent::ToolResult` in `chat_history` has a
/// non-empty `name`. Gemini rejects nameless function-response parts with a
/// 400; without this guard the failure surfaces deep inside the wire
/// builder where the original message reference is gone, making the error
/// hard to trace back to the offending tool call.
fn require_tool_result_name(request: &CompletionRequest) -> Result<(), TransformError> {
    for (idx, message) in request.chat_history.iter().enumerate() {
        if let Message::User { content } = message {
            for part in content.iter() {
                if let UserContent::ToolResult(result) = part
                    && result.name.trim().is_empty()
                {
                    return Err(TransformError::InvalidRequest {
                        provider: provider_id::GEMINI,
                        reason: format!(
                            "ToolResult at chat_history[{idx}] (id={}) has empty name; \
                             Gemini requires the tool function name on every functionResponse part",
                            result.id
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Trim trailing whitespace + newlines from `reasoning_content`. Several
/// providers append a `\n` after the closing `</think>` tag in stream
/// reassembly, which is harmless but produces noisy diffs in JSONL replay
/// fixtures. Idempotent.
fn trim_trailing_whitespace(reasoning_content: &mut Option<String>) {
    if let Some(text) = reasoning_content {
        let trimmed = text.trim_end();
        if trimmed.len() != text.len() {
            text.truncate(trimmed.len());
        }
    }
}

// ============================================================================
// Concrete transforms
// ============================================================================

#[derive(Debug, Default)]
pub struct NoopTransform;

impl ProviderTransform for NoopTransform {
    fn provider_id(&self) -> &'static str {
        "noop"
    }
}

#[derive(Debug, Default)]
pub struct AnthropicTransform;

impl ProviderTransform for AnthropicTransform {
    fn provider_id(&self) -> &'static str {
        provider_id::ANTHROPIC
    }

    fn variants(&self, model_id: &str) -> Vec<&'static str> {
        let mut out = vec![variant::CACHE_CONTROL];
        if matches_any(model_id, reasoning_ids::ANTHROPIC) {
            out.push(variant::REASONING);
            // Anthropic's reasoning-capable models also support interleaved
            // thinking + tool use within the same turn.
            out.push(variant::INTERLEAVED);
        }
        out
    }

    fn prepare_request(
        &self,
        _model_id: &str,
        request: &mut CompletionRequest,
    ) -> Result<(), TransformError> {
        drop_empty_assistant_turns(request);
        // Pairing check after the drop so the validator sees the exact
        // shape Anthropic will receive on the wire. Drop is documented to
        // never introduce orphans (it cannot remove a turn that carries a
        // ToolCall) — running the check on the post-drop history is
        // therefore equivalent to running it before, and idempotent.
        require_anthropic_tool_pairing(request)?;
        Ok(())
    }

    fn finalize_response(
        &self,
        _model_id: &str,
        _content: &mut Vec<AssistantContent>,
        reasoning_content: &mut Option<String>,
    ) -> Result<(), TransformError> {
        trim_trailing_whitespace(reasoning_content);
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct OpenAiTransform;

impl ProviderTransform for OpenAiTransform {
    fn provider_id(&self) -> &'static str {
        provider_id::OPENAI
    }

    fn variants(&self, model_id: &str) -> Vec<&'static str> {
        if matches_any(model_id, reasoning_ids::OPENAI) {
            vec![variant::REASONING]
        } else {
            Vec::new()
        }
    }

    fn prepare_request(
        &self,
        _model_id: &str,
        request: &mut CompletionRequest,
    ) -> Result<(), TransformError> {
        // OpenAI's chat-completions endpoint rejects unknown message fields,
        // and `reasoning_content` is only emitted by reasoning-mode SDKs
        // through a different envelope. Strip it so a model upgrade to a
        // non-reasoning OpenAI model does not regress with a 400.
        //
        // Note: redundant on the canonical → wire path because
        // `openai_compat::build_messages()` already drops `reasoning_content`
        // for callers that pick the non-reasoning variant. The transform
        // is kept as defense-in-depth — it expresses the policy at the
        // canonical level so a future consumer of `CompletionRequest`
        // (observability hooks, retry middleware, additional wire
        // adapters) cannot accidentally surface stale chain-of-thought.
        strip_assistant_reasoning(request);
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct DeepSeekTransform;

impl ProviderTransform for DeepSeekTransform {
    fn provider_id(&self) -> &'static str {
        provider_id::DEEPSEEK
    }

    fn variants(&self, model_id: &str) -> Vec<&'static str> {
        if matches_any(model_id, reasoning_ids::DEEPSEEK) {
            vec![variant::REASONING]
        } else {
            Vec::new()
        }
    }

    // DeepSeek is OpenAI-compatible *except* for one rule: when a previous
    // assistant turn emitted `reasoning_content`, the next request must
    // echo that field back to keep the chain-of-thought coherent. We
    // therefore deliberately *do not* strip `reasoning_content` here — the
    // wire builder selects `build_messages_with_reasoning_content` for this
    // provider so the field survives.
}

#[derive(Debug, Default)]
pub struct KimiTransform;

impl ProviderTransform for KimiTransform {
    fn provider_id(&self) -> &'static str {
        provider_id::KIMI
    }

    fn variants(&self, model_id: &str) -> Vec<&'static str> {
        if matches_any(model_id, reasoning_ids::KIMI) {
            vec![variant::REASONING]
        } else {
            Vec::new()
        }
    }

    // Kimi follows the same chain-of-thought handoff rule as DeepSeek; do
    // not strip `reasoning_content`. The wire builder for Kimi uses
    // `build_messages_with_reasoning_content`.
}

#[derive(Debug, Default)]
pub struct ZhipuTransform;

impl ProviderTransform for ZhipuTransform {
    fn provider_id(&self) -> &'static str {
        provider_id::ZHIPU
    }

    fn variants(&self, model_id: &str) -> Vec<&'static str> {
        if matches_any(model_id, reasoning_ids::ZHIPU) {
            vec![variant::REASONING]
        } else {
            Vec::new()
        }
    }

    fn prepare_request(
        &self,
        _model_id: &str,
        request: &mut CompletionRequest,
    ) -> Result<(), TransformError> {
        strip_assistant_reasoning(request);
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct OllamaTransform;

impl ProviderTransform for OllamaTransform {
    fn provider_id(&self) -> &'static str {
        provider_id::OLLAMA
    }

    fn variants(&self, _model_id: &str) -> Vec<&'static str> {
        // Ollama serves arbitrary user-installed models; we cannot
        // statically know which expose a thinking channel. The wire layer
        // probes the `think` field at runtime and the capability matrix
        // intentionally returns `None` for Ollama, so transforms also stay
        // capability-neutral here.
        Vec::new()
    }

    fn prepare_request(
        &self,
        _model_id: &str,
        request: &mut CompletionRequest,
    ) -> Result<(), TransformError> {
        // Ollama's OpenAI-compatible adapter does not surface a stable
        // `reasoning_content` shape across all installed models — strip it
        // so a model that does not understand the field cannot 400 on it.
        strip_assistant_reasoning(request);
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct GeminiTransform;

impl ProviderTransform for GeminiTransform {
    fn provider_id(&self) -> &'static str {
        provider_id::GEMINI
    }

    fn variants(&self, model_id: &str) -> Vec<&'static str> {
        if matches_any(model_id, reasoning_ids::GEMINI) {
            vec![variant::REASONING]
        } else {
            Vec::new()
        }
    }

    fn prepare_request(
        &self,
        _model_id: &str,
        request: &mut CompletionRequest,
    ) -> Result<(), TransformError> {
        // Gemini's `functionResponse` part requires a `name` field;
        // catching the contract violation here gives a far clearer error
        // than the wire builder's "field missing" panic deep inside JSON
        // serialisation.
        require_tool_result_name(request)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::ai::completion::{
        AssistantContent, Function, Message, OneOrMany, Text, ToolCall, ToolResult, UserContent,
    };

    fn assistant_with_reasoning(text: &str, reasoning: &str) -> Message {
        Message::Assistant {
            id: None,
            reasoning_content: Some(reasoning.to_string()),
            content: OneOrMany::One(AssistantContent::Text(Text {
                text: text.to_string(),
            })),
        }
    }

    fn assistant_text(text: &str) -> Message {
        Message::Assistant {
            id: None,
            reasoning_content: None,
            content: OneOrMany::One(AssistantContent::Text(Text {
                text: text.to_string(),
            })),
        }
    }

    fn assistant_empty() -> Message {
        Message::Assistant {
            id: None,
            reasoning_content: None,
            content: OneOrMany::One(AssistantContent::Text(Text {
                text: String::new(),
            })),
        }
    }

    fn assistant_tool_call(id: &str, name: &str) -> Message {
        Message::Assistant {
            id: None,
            reasoning_content: None,
            content: OneOrMany::One(AssistantContent::ToolCall(ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                function: Function {
                    name: name.to_string(),
                    arguments: serde_json::json!({}),
                },
            })),
        }
    }

    fn user_with_tool_result(id: &str, name: &str, value: serde_json::Value) -> Message {
        Message::User {
            content: OneOrMany::One(UserContent::ToolResult(ToolResult {
                id: id.to_string(),
                name: name.to_string(),
                result: value,
            })),
        }
    }

    fn request_with(history: Vec<Message>) -> CompletionRequest {
        CompletionRequest {
            chat_history: history,
            ..Default::default()
        }
    }

    #[test]
    fn transform_for_returns_correct_provider_id_for_each_known_id() {
        for &id in &[
            provider_id::ANTHROPIC,
            provider_id::OPENAI,
            provider_id::DEEPSEEK,
            provider_id::GEMINI,
            provider_id::KIMI,
            provider_id::ZHIPU,
            provider_id::OLLAMA,
        ] {
            let transform = transform_for(id);
            assert_eq!(
                transform.provider_id(),
                id,
                "transform_for({id}) returned {} but should match",
                transform.provider_id()
            );
        }
    }

    #[test]
    fn transform_for_unknown_provider_returns_noop() {
        let transform = transform_for("does-not-exist");
        assert_eq!(transform.provider_id(), "noop");
        // No-op must accept any request unchanged.
        let mut req = request_with(vec![assistant_with_reasoning("hi", "thoughts")]);
        transform.prepare_request("model", &mut req).unwrap();
        match &req.chat_history[0] {
            Message::Assistant {
                reasoning_content, ..
            } => assert_eq!(reasoning_content.as_deref(), Some("thoughts")),
            _ => panic!("history shape changed"),
        }
    }

    #[test]
    fn openai_transform_strips_assistant_reasoning_content() {
        let mut req = request_with(vec![
            Message::user("hi"),
            assistant_with_reasoning("answer", "internal monologue"),
        ]);
        OpenAiTransform.prepare_request("gpt-4o", &mut req).unwrap();
        match &req.chat_history[1] {
            Message::Assistant {
                reasoning_content, ..
            } => assert!(
                reasoning_content.is_none(),
                "OpenAI must not echo reasoning_content (got {reasoning_content:?})"
            ),
            other => panic!("unexpected message {other:?}"),
        }
    }

    #[test]
    fn openai_transform_is_idempotent() {
        // Applying twice must not mutate further.
        let mut req = request_with(vec![assistant_with_reasoning("answer", "thoughts")]);
        OpenAiTransform.prepare_request("gpt-4o", &mut req).unwrap();
        let snapshot = req.chat_history.clone();
        OpenAiTransform.prepare_request("gpt-4o", &mut req).unwrap();
        assert_eq!(req.chat_history, snapshot);
    }

    #[test]
    fn deepseek_transform_preserves_reasoning_content() {
        let mut req = request_with(vec![assistant_with_reasoning(
            "answer",
            "deep reasoning trace",
        )]);
        DeepSeekTransform
            .prepare_request("deepseek-reasoner", &mut req)
            .unwrap();
        match &req.chat_history[0] {
            Message::Assistant {
                reasoning_content, ..
            } => assert_eq!(
                reasoning_content.as_deref(),
                Some("deep reasoning trace"),
                "DeepSeek must preserve reasoning_content for chain-of-thought continuity"
            ),
            other => panic!("unexpected message {other:?}"),
        }
    }

    #[test]
    fn kimi_transform_preserves_reasoning_content() {
        let mut req = request_with(vec![assistant_with_reasoning("answer", "kimi thoughts")]);
        KimiTransform
            .prepare_request("kimi-thinking", &mut req)
            .unwrap();
        match &req.chat_history[0] {
            Message::Assistant {
                reasoning_content, ..
            } => assert_eq!(reasoning_content.as_deref(), Some("kimi thoughts")),
            other => panic!("unexpected message {other:?}"),
        }
    }

    #[test]
    fn anthropic_transform_drops_empty_assistant_turn() {
        let mut req = request_with(vec![
            Message::user("hi"),
            assistant_empty(),
            assistant_text("real answer"),
        ]);
        AnthropicTransform
            .prepare_request("claude-sonnet-4-0", &mut req)
            .unwrap();
        assert_eq!(req.chat_history.len(), 2, "empty assistant turn dropped");
        match &req.chat_history[1] {
            Message::Assistant { content, .. } => {
                let text = content.iter().next();
                assert!(matches!(
                    text,
                    Some(AssistantContent::Text(Text { text })) if text == "real answer"
                ));
            }
            other => panic!("unexpected message {other:?}"),
        }
    }

    #[test]
    fn anthropic_transform_keeps_assistant_turn_with_tool_call() {
        // A tool-call-only assistant turn (no Text part) must survive — it
        // is non-empty in the wire sense.
        let mut req = request_with(vec![
            Message::user("hi"),
            assistant_tool_call("call_1", "shell"),
        ]);
        AnthropicTransform
            .prepare_request("claude-sonnet-4-0", &mut req)
            .unwrap();
        assert_eq!(req.chat_history.len(), 2);
    }

    #[test]
    fn anthropic_transform_rejects_orphan_tool_result() {
        // ToolResult appears with no preceding ToolCall of the same id.
        let mut req = request_with(vec![
            Message::user("hi"),
            user_with_tool_result("call_orphan", "shell", serde_json::json!({})),
        ]);
        let err = AnthropicTransform
            .prepare_request("claude-sonnet-4-0", &mut req)
            .expect_err("orphan ToolResult must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("call_orphan"),
            "error must reference offending tool result id, got: {msg}"
        );
        assert!(
            msg.contains("ToolResult") && msg.contains("ToolCall"),
            "error must reference both halves of the pairing, got: {msg}"
        );
    }

    #[test]
    fn anthropic_transform_rejects_tool_result_with_mismatched_id() {
        let mut req = request_with(vec![
            Message::user("hi"),
            assistant_tool_call("call_1", "shell"),
            user_with_tool_result("call_2", "shell", serde_json::json!({})),
        ]);
        let err = AnthropicTransform
            .prepare_request("claude-sonnet-4-0", &mut req)
            .expect_err("mismatched tool_result id must be rejected");
        assert!(err.to_string().contains("call_2"));
    }

    #[test]
    fn anthropic_transform_accepts_paired_tool_call_and_result() {
        let mut req = request_with(vec![
            Message::user("hi"),
            assistant_tool_call("call_1", "shell"),
            user_with_tool_result("call_1", "shell", serde_json::json!({"ok": true})),
        ]);
        AnthropicTransform
            .prepare_request("claude-sonnet-4-0", &mut req)
            .expect("matched tool pair must pass");
    }

    #[test]
    fn anthropic_transform_drops_empty_assistant_between_paired_tool_messages() {
        // The empty assistant turn between ToolCall and ToolResult is
        // safe to drop because the ToolCall is in the *previous* assistant
        // turn (its id remains visible to the pairing validator).
        let mut req = request_with(vec![
            assistant_tool_call("call_1", "shell"),
            assistant_empty(),
            user_with_tool_result("call_1", "shell", serde_json::json!({"ok": true})),
        ]);
        AnthropicTransform
            .prepare_request("claude-sonnet-4-0", &mut req)
            .expect("dropping the empty turn must preserve pairing");
        assert_eq!(req.chat_history.len(), 2, "empty turn dropped");
    }

    #[test]
    fn anthropic_transform_rejects_duplicate_tool_result_for_same_id() {
        // Anthropic rejects two tool_results for the same tool_use; the
        // pairing validator must catch the duplicate.
        let mut req = request_with(vec![
            assistant_tool_call("call_1", "shell"),
            user_with_tool_result("call_1", "shell", serde_json::json!({"first": true})),
            user_with_tool_result("call_1", "shell", serde_json::json!({"second": true})),
        ]);
        let err = AnthropicTransform
            .prepare_request("claude-sonnet-4-0", &mut req)
            .expect_err("duplicate tool_result for same id must be rejected");
        assert!(err.to_string().contains("call_1"));
    }

    #[test]
    fn anthropic_transform_rejects_duplicate_tool_call_ids() {
        // Two assistant turns sharing the same tool_use id is an Anthropic
        // 400; the validator must catch it instead of silently overwriting.
        let mut req = request_with(vec![
            assistant_tool_call("call_dup", "shell"),
            user_with_tool_result("call_dup", "shell", serde_json::json!({"ok": true})),
            assistant_tool_call("call_dup", "shell"),
        ]);
        let err = AnthropicTransform
            .prepare_request("claude-sonnet-4-0", &mut req)
            .expect_err("duplicate ToolCall id must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("call_dup"),
            "error must reference duplicate id, got: {msg}"
        );
        assert!(
            msg.contains("ToolCall") && msg.contains("unique"),
            "error must explain the uniqueness requirement, got: {msg}"
        );
    }

    #[test]
    fn anthropic_transform_rejects_orphan_tool_result_inside_system_message() {
        // System messages carry OneOrMany<UserContent>; a ToolResult
        // embedded there is not forwarded by Anthropic's system extractor
        // (it only pulls UserContent::Text), so it must fail loudly here
        // rather than vanish silently.
        let mut req = request_with(vec![Message::System {
            content: OneOrMany::One(UserContent::ToolResult(ToolResult {
                id: "call_in_system".to_string(),
                name: "shell".to_string(),
                result: serde_json::json!({}),
            })),
        }]);
        let err = AnthropicTransform
            .prepare_request("claude-sonnet-4-0", &mut req)
            .expect_err("orphan ToolResult inside System must be rejected");
        assert!(err.to_string().contains("call_in_system"));
    }

    #[test]
    fn anthropic_transform_rejects_paired_tool_result_inside_system_message() {
        // Even when the ToolResult id pairs with a prior ToolCall, embedding
        // it in a System message would let the Anthropic wire extractor
        // silently drop the result and ship an orphan tool_use to the API.
        // The validator must still reject so the contract violation
        // surfaces with the offending id.
        let mut req = request_with(vec![
            assistant_tool_call("call_paired", "shell"),
            Message::System {
                content: OneOrMany::One(UserContent::ToolResult(ToolResult {
                    id: "call_paired".to_string(),
                    name: "shell".to_string(),
                    result: serde_json::json!({"ok": true}),
                })),
            },
        ]);
        let err = AnthropicTransform
            .prepare_request("claude-sonnet-4-0", &mut req)
            .expect_err("paired ToolResult inside System must still be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("call_paired"),
            "error must reference the paired id, got: {msg}"
        );
        assert!(
            msg.contains("System") && msg.contains("silently"),
            "error must explain the wire-side disappearance risk, got: {msg}"
        );
    }

    #[test]
    fn anthropic_transform_pairing_validator_is_idempotent() {
        let mut req = request_with(vec![
            assistant_tool_call("call_1", "shell"),
            user_with_tool_result("call_1", "shell", serde_json::json!({"ok": true})),
            assistant_tool_call("call_2", "shell"),
            user_with_tool_result("call_2", "shell", serde_json::json!({"ok": true})),
        ]);
        AnthropicTransform
            .prepare_request("claude-sonnet-4-0", &mut req)
            .unwrap();
        let snapshot = req.chat_history.clone();
        AnthropicTransform
            .prepare_request("claude-sonnet-4-0", &mut req)
            .unwrap();
        assert_eq!(req.chat_history, snapshot);
    }

    #[test]
    fn anthropic_transform_finalize_trims_trailing_whitespace() {
        let mut content: Vec<AssistantContent> = vec![];
        let mut reasoning = Some("internal trace\n\n  ".to_string());
        AnthropicTransform
            .finalize_response("claude-opus-4-0", &mut content, &mut reasoning)
            .unwrap();
        assert_eq!(reasoning.as_deref(), Some("internal trace"));
    }

    #[test]
    fn anthropic_transform_finalize_is_idempotent() {
        let mut content: Vec<AssistantContent> = vec![];
        let mut reasoning = Some("internal trace\n".to_string());
        AnthropicTransform
            .finalize_response("claude-opus-4-0", &mut content, &mut reasoning)
            .unwrap();
        let snapshot = reasoning.clone();
        AnthropicTransform
            .finalize_response("claude-opus-4-0", &mut content, &mut reasoning)
            .unwrap();
        assert_eq!(reasoning, snapshot);
    }

    #[test]
    fn anthropic_transform_advertises_cache_control_variant_for_all_models() {
        let variants = AnthropicTransform.variants("claude-3-5-haiku-latest");
        assert!(variants.contains(&variant::CACHE_CONTROL));
        // Non-reasoning model must not advertise reasoning.
        assert!(!variants.contains(&variant::REASONING));
    }

    #[test]
    fn anthropic_transform_advertises_reasoning_for_thinking_models() {
        let variants = AnthropicTransform.variants("claude-opus-4-0");
        assert!(variants.contains(&variant::REASONING));
        assert!(variants.contains(&variant::INTERLEAVED));
    }

    #[test]
    fn gemini_transform_rejects_tool_result_with_empty_name() {
        let req = request_with(vec![user_with_tool_result(
            "call_1",
            "",
            serde_json::json!({"ok": true}),
        )]);
        let mut req_owned = req;
        let err = GeminiTransform
            .prepare_request("gemini-2.5-flash", &mut req_owned)
            .expect_err("empty ToolResult.name must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("ToolResult"),
            "error message should name the offending field, got: {msg}"
        );
        assert!(
            msg.contains("call_1"),
            "error message should include the tool call id, got: {msg}"
        );
    }

    #[test]
    fn gemini_transform_accepts_tool_result_with_whitespace_only_name_as_invalid() {
        let mut req = request_with(vec![user_with_tool_result(
            "call_2",
            "   ",
            serde_json::json!({}),
        )]);
        let err = GeminiTransform
            .prepare_request("gemini-2.5-flash", &mut req)
            .expect_err("whitespace-only name is also invalid");
        assert!(err.to_string().contains("call_2"));
    }

    #[test]
    fn gemini_transform_accepts_valid_tool_result() {
        let mut req = request_with(vec![user_with_tool_result(
            "call_3",
            "shell",
            serde_json::json!({}),
        )]);
        GeminiTransform
            .prepare_request("gemini-2.5-flash", &mut req)
            .expect("valid ToolResult must pass");
    }

    #[test]
    fn variants_are_case_insensitive_on_model_id() {
        assert!(matches_any("Claude-Opus-4-0", reasoning_ids::ANTHROPIC));
        assert!(matches_any("DeepSeek-Reasoner", reasoning_ids::DEEPSEEK));
    }

    #[test]
    fn ollama_transform_strips_reasoning_content() {
        let mut req = request_with(vec![assistant_with_reasoning("hi", "thoughts")]);
        OllamaTransform
            .prepare_request("llama3.3", &mut req)
            .unwrap();
        match &req.chat_history[0] {
            Message::Assistant {
                reasoning_content, ..
            } => assert!(reasoning_content.is_none()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn zhipu_transform_strips_reasoning_content() {
        let mut req = request_with(vec![assistant_with_reasoning("hi", "thoughts")]);
        ZhipuTransform
            .prepare_request("glm-4-plus", &mut req)
            .unwrap();
        match &req.chat_history[0] {
            Message::Assistant {
                reasoning_content, ..
            } => assert!(reasoning_content.is_none()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn transform_error_display_pins_invalid_request_template() {
        assert_eq!(
            TransformError::InvalidRequest {
                provider: "openai",
                reason: "missing tool name".to_string(),
            }
            .to_string(),
            "transform 'openai' rejected request: missing tool name",
        );
    }
}
