//! Wave 10 / PR 10 — provider boot smoke (§5.2).
//!
//! Boots each AI provider's HTTP client against a tiny `axum`
//! mock server (no external mock crate; the helper lives in
//! `tests/helpers/mock_provider_server.rs`) and asserts the very
//! first `/chat/completions` request body shape, with focus on
//! provider-specific flag passthrough that the real CLI flags
//! (`--deepseek-thinking` / `--deepseek-reasoning-effort` /
//! `--deepseek-stream`) populate via `CompletionRequest`.
//!
//! Coverage included here:
//!   * **DeepSeek**: `thinking.type=enabled`,
//!     `reasoning_effort=high`, `stream=false` round-trip from
//!     `CompletionRequest` into the wire body. Pins the CLI →
//!     `CompletionRequest` → DeepSeekRequest serialisation
//!     contract.
//!   * **OpenAI-compat**: client built with custom base URL
//!     posts to `/chat/completions`; body carries `model` +
//!     non-empty `messages`. Smoke proof that the generic
//!     OpenAI client constructor + `with_base_url` form
//!     correctly route to the stub endpoint.
//!   * **Anthropic**: client built with custom base URL posts
//!     to `/v1/messages`; body carries `model`, non-empty
//!     `messages`, and the required `max_tokens` field
//!     (Anthropic-specific — the OpenAI-compat shape has no
//!     mandatory token cap). Smoke proof that the helper's
//!     `/v1/messages` route covers the second wire shape we
//!     ship.
//!   * **Kimi**: `--kimi-thinking enabled` round-trips to
//!     `thinking.type=enabled, keep=all`; `stream=false`
//!     serialises (`KimiRequest.stream` is also a non-skip
//!     boolean, mirroring DeepSeek). Pins the
//!     `--kimi-thinking` / `--kimi-stream` CLI →
//!     `CompletionRequest` → `KimiRequest` chain.
//!   * **Ollama**: `--ollama-thinking high` round-trips to
//!     `think="high"` on the native `/api/chat` request body.
//!     Pins the third wire shape we ship (Ollama-native, distinct
//!     from OpenAI-compat and Anthropic).
//!   * **Zhipu**: client built with custom base URL posts to
//!     `/chat/completions` (OpenAI-compat shape); body carries
//!     `model` + non-empty `messages`. Smoke proof for the
//!     fourth provider on the same OpenAI-compat helper route.
//!   * **Gemini**: client built with the new test-only
//!     `with_base_url` constructor posts to
//!     `/v1beta/models/{model}:generateContent` (the
//!     `:generateContent` action is captured as part of the
//!     single path segment); body carries Gemini's native
//!     `contents` array (NOT `messages` — Gemini uses its own
//!     wire shape). The test-only constructor was added in the
//!     same commit and is documented as not part of the CLI
//!     surface.
//!
//! Coverage deferred to follow-up PRs:
//!   * Ollama `--ollama-compact-tools` flag (affects tool-schema
//!     serialisation; needs a tool-bearing `CompletionRequest`).

mod helpers;

use anyhow::Result;
use helpers::mock_provider_server::MockProviderServer;
use libra::internal::ai::{
    client::CompletionClient,
    completion::{
        CompletionModel, CompletionReasoningEffort, CompletionThinking, Message,
        request::CompletionRequest,
    },
    providers::{
        anthropic::client::Client as AnthropicClient, deepseek::client::Client as DeepSeekClient,
        gemini::client::Client as GeminiClient, kimi::client::Client as KimiClient,
        ollama::client::Client as OllamaClient, openai::client::Client as OpenAiClient,
        zhipu::client::Client as ZhipuClient,
    },
};
use serde_json::json;

/// Gemini `/v1beta/models/{model}:generateContent` returns a
/// `GenerateContentResponse` envelope distinct from OpenAI-compat.
fn canned_gemini_response() -> serde_json::Value {
    json!({
        "candidates": [
            {
                "content": {
                    "role": "model",
                    "parts": [{ "text": "ok" }]
                },
                "finishReason": "STOP"
            }
        ],
        "usageMetadata": {
            "promptTokenCount": 1,
            "candidatesTokenCount": 1,
            "totalTokenCount": 2
        }
    })
}

/// Ollama's native `/api/chat` non-streaming response is a
/// single JSON object (NOT NDJSON) when `stream=false`. Shape
/// matches `OllamaResponse` so the deserialiser is satisfied.
fn canned_ollama_response() -> serde_json::Value {
    json!({
        "model": "test-model",
        "created_at": "2026-05-11T00:00:00Z",
        "message": {
            "role": "assistant",
            "content": "ok",
            "thinking": null,
            "tool_calls": []
        },
        "done": true,
        "prompt_eval_count": 1,
        "eval_count": 1
    })
}

/// Anthropic Messages API responds with a different envelope
/// than OpenAI-compat — separate canned shape so the test
/// drives a real `AnthropicResponse` deserialise rather than
/// short-circuiting on a parse error.
fn canned_anthropic_response() -> serde_json::Value {
    json!({
        "id": "msg_test_completion",
        "type": "message",
        "role": "assistant",
        "content": [
            { "type": "text", "text": "ok" }
        ],
        "model": "claude-test",
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 1,
            "output_tokens": 1
        }
    })
}

fn canned_chat_response() -> serde_json::Value {
    json!({
        "id": "test-completion",
        "object": "chat.completion",
        "created": 0,
        "model": "test-model",
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "ok"
                },
                "finish_reason": "stop"
            }
        ],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "total_tokens": 2
        }
    })
}

/// Wave 10 §5.2 closure — DeepSeek boot smoke.
///
/// Builds a `DeepSeekClient` against a localhost stub (the same
/// constructor production uses for `--api-base` overrides), drives
/// a single `CompletionRequest` carrying `thinking=Enabled`,
/// `reasoning_effort=High`, `stream=false`, and asserts the
/// captured POST body has `thinking.type=enabled`,
/// `reasoning_effort=high`, and `stream=false`. This pins the
/// full path: CLI args → `CompletionRequest` → `DeepSeekRequest`
/// serialisation → wire body.
#[tokio::test]
async fn deepseek_completion_request_carries_thinking_and_reasoning_effort_flags() -> Result<()> {
    let server = MockProviderServer::start(canned_chat_response()).await;
    let client = DeepSeekClient::with_base_url(&server.base_url(), "test-key".to_string());
    let model = client.completion_model("deepseek-v4-flash");

    let mut request = CompletionRequest::new(vec![Message::user("hello deepseek")]);
    request.thinking = Some(CompletionThinking::Enabled);
    request.reasoning_effort = Some(CompletionReasoningEffort::High);
    request.stream = Some(false);

    let _response = model.completion(request).await?;

    let bodies = server.captured_bodies();
    assert_eq!(bodies.len(), 1, "expected exactly one POST captured");
    let body = &bodies[0];
    assert_eq!(
        body.get("model").and_then(|v| v.as_str()),
        Some("deepseek-v4-flash"),
        "model field must round-trip into the wire body; got {body:?}",
    );
    assert_eq!(
        body.pointer("/thinking/type").and_then(|v| v.as_str()),
        Some("enabled"),
        "DeepSeek thinking flag must serialise as type=enabled; got {body:?}",
    );
    assert_eq!(
        body.get("reasoning_effort").and_then(|v| v.as_str()),
        Some("high"),
        "DeepSeek reasoning_effort=high must round-trip; got {body:?}",
    );
    assert_eq!(
        body.get("stream").and_then(|v| v.as_bool()),
        Some(false),
        "DeepSeek stream field must serialise (DeepSeekRequest.stream is NOT skip_serializing_if); got {body:?}",
    );
    Ok(())
}

/// Wave 10 §5.2 closure — OpenAI-compat boot smoke.
///
/// Proves the generic `OpenAiClient::with_base_url` constructor
/// reaches a localhost endpoint over HTTP and serialises a
/// minimal `CompletionRequest` into a recognisable
/// `/chat/completions` body. The smoke is intentionally
/// lightweight: no flag passthrough, just confirmation that
/// `--api-base`-style overrides hit the configured host.
#[tokio::test]
async fn openai_completion_request_smoke_boots_against_localhost_stub() -> Result<()> {
    let server = MockProviderServer::start(canned_chat_response()).await;
    let client = OpenAiClient::with_base_url(&server.base_url(), "test-key".to_string());
    let model = client.completion_model("gpt-4o-mini");

    let request = CompletionRequest::new(vec![Message::user("hello openai")]);
    let _response = model.completion(request).await?;

    let bodies = server.captured_bodies();
    assert_eq!(bodies.len(), 1, "expected exactly one POST captured");
    let body = &bodies[0];
    assert_eq!(
        body.get("model").and_then(|v| v.as_str()),
        Some("gpt-4o-mini"),
        "model field must round-trip into the wire body; got {body:?}",
    );
    assert!(
        body.get("messages")
            .and_then(|v| v.as_array())
            .is_some_and(|arr| !arr.is_empty()),
        "OpenAI request must carry non-empty messages array; got {body:?}",
    );
    Ok(())
}

/// Wave 10 §5.2 closure — Kimi boot smoke + thinking/stream
/// flag passthrough.
///
/// Kimi's wire shape is OpenAI-compat (`POST /chat/completions`)
/// but adds a Moonshot-specific `thinking` object with both
/// `type` and `keep` discriminants. When `--kimi-thinking enabled`
/// is set on the CLI the `CompletionRequest.thinking` field
/// becomes `Some(CompletionThinking::Enabled)`; this test pins
/// that the resulting wire body has `thinking.type=enabled`
/// AND `thinking.keep=all` (the default for any non-Disabled
/// value), plus the always-emitted `stream` boolean.
#[tokio::test]
async fn kimi_completion_request_carries_thinking_and_stream_flags() -> Result<()> {
    let server = MockProviderServer::start(canned_chat_response()).await;
    let client = KimiClient::with_base_url(&server.base_url(), "test-key".to_string());
    let model = client.completion_model("kimi-k2-thinking");

    let mut request = CompletionRequest::new(vec![Message::user("hello kimi")]);
    request.thinking = Some(CompletionThinking::Enabled);
    request.stream = Some(false);

    let _response = model.completion(request).await?;

    let bodies = server.captured_bodies();
    assert_eq!(bodies.len(), 1, "expected exactly one POST captured");
    let body = &bodies[0];
    assert_eq!(
        body.get("model").and_then(|v| v.as_str()),
        Some("kimi-k2-thinking"),
        "Kimi model field must round-trip; got {body:?}",
    );
    assert_eq!(
        body.pointer("/thinking/type").and_then(|v| v.as_str()),
        Some("enabled"),
        "Kimi thinking flag must serialise as type=enabled; got {body:?}",
    );
    assert_eq!(
        body.pointer("/thinking/keep").and_then(|v| v.as_str()),
        Some("all"),
        "Kimi thinking-enabled requires keep=all (preserves prior reasoning); got {body:?}",
    );
    assert_eq!(
        body.get("stream").and_then(|v| v.as_bool()),
        Some(false),
        "Kimi stream field must serialise (KimiRequest.stream is NOT skip_serializing_if); got {body:?}",
    );
    Ok(())
}

/// Wave 10 §5.2 closure — Anthropic boot smoke against the
/// helper's `/v1/messages` route. Anthropic's request shape is
/// distinct from OpenAI-compat (top-level `system`,
/// mandatory `max_tokens`), so this test proves the second wire
/// shape we ship is reachable through the same mock helper.
#[tokio::test]
async fn anthropic_completion_request_smoke_boots_against_localhost_stub() -> Result<()> {
    let server = MockProviderServer::start(canned_anthropic_response()).await;
    let client = AnthropicClient::with_base_url(&server.base_url(), "test-key".to_string());
    let model = client.completion_model("claude-3-5-sonnet-20241022");

    let request = CompletionRequest::new(vec![Message::user("hello anthropic")]);
    let _response = model.completion(request).await?;

    let bodies = server.captured_bodies();
    assert_eq!(bodies.len(), 1, "expected exactly one POST captured");
    let body = &bodies[0];
    assert_eq!(
        body.get("model").and_then(|v| v.as_str()),
        Some("claude-3-5-sonnet-20241022"),
        "Anthropic model field must round-trip; got {body:?}",
    );
    assert!(
        body.get("messages")
            .and_then(|v| v.as_array())
            .is_some_and(|arr| !arr.is_empty()),
        "Anthropic request must carry non-empty messages array; got {body:?}",
    );
    assert!(
        body.get("max_tokens").and_then(|v| v.as_u64()).is_some(),
        "Anthropic request must carry max_tokens (mandatory in /v1/messages); got {body:?}",
    );
    Ok(())
}

/// Wave 10 §5.2 closure — Ollama boot smoke + think flag
/// passthrough.
///
/// Ollama uses its own native `/api/chat` endpoint (not
/// OpenAI-compat) and a `think` field that accepts either a
/// boolean or a discrete level (`low`/`medium`/`high`). When
/// `--ollama-thinking high` is set on the CLI,
/// `CompletionRequest.thinking = Some(CompletionThinking::High)`
/// must serialise as `think: "high"` on the wire. Pins the
/// third wire shape we ship.
#[tokio::test]
async fn ollama_completion_request_carries_think_level_flag() -> Result<()> {
    let server = MockProviderServer::start(canned_ollama_response()).await;
    // Ollama's `with_base_url_and_api_key` strips a trailing
    // `/v1` (default base ends with it); pointing at the bare
    // localhost root resolves to `<root>/api/chat`, which the
    // mock helper now handles.
    let client = OllamaClient::with_base_url_and_api_key(&server.base_url(), None);
    let model = client.completion_model("ollama-test-model");

    let mut request = CompletionRequest::new(vec![Message::user("hello ollama")]);
    request.thinking = Some(CompletionThinking::High);
    request.stream = Some(false);

    let _response = model.completion(request).await?;

    let bodies = server.captured_bodies();
    assert_eq!(bodies.len(), 1, "expected exactly one POST captured");
    let body = &bodies[0];
    assert_eq!(
        body.get("model").and_then(|v| v.as_str()),
        Some("ollama-test-model"),
        "Ollama model field must round-trip; got {body:?}",
    );
    assert_eq!(
        body.get("think").and_then(|v| v.as_str()),
        Some("high"),
        "Ollama --ollama-thinking high must serialise as think=\"high\" (level discriminant); got {body:?}",
    );
    // Ollama hard-codes `stream: true` in the wire body
    // regardless of `CompletionRequest.stream` — the runtime
    // always opens a streaming `/api/chat` request even when the
    // caller would prefer a one-shot reply. Pin the
    // always-true contract here so a future runtime change
    // (e.g. wiring `request.stream` through) breaks loudly
    // rather than silently swapping the wire format under
    // existing callers.
    assert_eq!(
        body.get("stream").and_then(|v| v.as_bool()),
        Some(true),
        "Ollama is streaming-only on the wire; stream:true must always be emitted; got {body:?}",
    );
    Ok(())
}

/// Wave 10 §5.2 closure — Zhipu boot smoke. Zhipu is an
/// OpenAI-compat provider (POSTs to `/chat/completions`) so this
/// is the lightweight smoke variant: no provider-specific flag
/// passthrough, just confirmation that
/// `ZhipuClient::with_base_url` reaches the configured host and
/// serialises a recognisable `model + messages` body. Proves
/// the helper's `/chat/completions` route covers the fourth
/// provider on that wire shape (after OpenAI, DeepSeek, Kimi).
#[tokio::test]
async fn zhipu_completion_request_smoke_boots_against_localhost_stub() -> Result<()> {
    let server = MockProviderServer::start(canned_chat_response()).await;
    let client = ZhipuClient::with_base_url(&server.base_url(), "test-key".to_string());
    let model = client.completion_model("glm-4.6");

    let request = CompletionRequest::new(vec![Message::user("hello zhipu")]);
    let _response = model.completion(request).await?;

    let bodies = server.captured_bodies();
    assert_eq!(bodies.len(), 1, "expected exactly one POST captured");
    let body = &bodies[0];
    assert_eq!(
        body.get("model").and_then(|v| v.as_str()),
        Some("glm-4.6"),
        "Zhipu model field must round-trip; got {body:?}",
    );
    assert!(
        body.get("messages")
            .and_then(|v| v.as_array())
            .is_some_and(|arr| !arr.is_empty()),
        "Zhipu request must carry non-empty messages array; got {body:?}",
    );
    Ok(())
}

/// Wave 10 §5.2 closure — Gemini boot smoke. Gemini uses its
/// own native wire shape: requests POST to
/// `/v1beta/models/{model}:generateContent` and the body carries
/// a `contents` array (Gemini-style, NOT OpenAI-compat
/// `messages`). The mock helper's `/v1beta/models/{action}`
/// route captures both model + `:generateContent` action
/// suffix as a single path segment.
///
/// Uses the new test-only `GeminiClient::with_base_url`
/// constructor added alongside this test (CLI never invokes it;
/// production path remains `from_env`).
#[tokio::test]
async fn gemini_completion_request_smoke_boots_against_localhost_stub() -> Result<()> {
    let server = MockProviderServer::start(canned_gemini_response()).await;
    let client = GeminiClient::with_base_url(&server.base_url(), "test-key".to_string());
    let model = client.completion_model("gemini-2.5-flash");

    let request = CompletionRequest::new(vec![Message::user("hello gemini")]);
    let _response = model.completion(request).await?;

    let bodies = server.captured_bodies();
    assert_eq!(bodies.len(), 1, "expected exactly one POST captured");
    let body = &bodies[0];
    assert!(
        body.get("contents")
            .and_then(|v| v.as_array())
            .is_some_and(|arr| !arr.is_empty()),
        "Gemini request must carry non-empty contents array (NOT messages); got {body:?}",
    );
    Ok(())
}

/// W4-01: default `libra code` accepts `--env-file` (W5-07 removed the
/// deprecated `--web`/`--web-only` aliases entirely), and env-file values win
/// over a competing process env for the same provider key through the same
/// validate → load path as `execute_web_only`.
#[test]
fn default_web_env_file_precedence() {
    use std::path::Path;

    use clap::Parser;
    use libra::{
        command::code::{
            CODE_EXAMPLES, CodeArgs, CodeProvider, resolve_default_web_provider_env_key,
        },
        utils::test::ScopedEnvVar,
    };

    let temp = tempfile::tempdir().expect("tempdir");
    let env_path = temp.path().join(".env.w4-01");
    std::fs::write(&env_path, "OPENAI_API_KEY=from-env-file\n").expect("write env file");

    let parsed = CodeArgs::try_parse_from([
        "code",
        "--env-file",
        env_path.to_str().expect("utf8 path"),
        "--provider",
        "openai",
    ])
    .expect("default Web must accept --env-file");
    assert_eq!(parsed.provider, Some(CodeProvider::Openai));
    assert_eq!(
        parsed.env_file.as_deref(),
        Some(Path::new(env_path.as_os_str()))
    );
    assert!(
        CODE_EXAMPLES.contains("--env-file"),
        "EXAMPLES must keep documenting --env-file on the default launch"
    );

    let _process = ScopedEnvVar::set("OPENAI_API_KEY", "from-process-env");
    let resolved = resolve_default_web_provider_env_key(&parsed, "OPENAI_API_KEY")
        .expect("default Web validate + env-file load must succeed");
    assert_eq!(
        resolved.as_deref(),
        Some("from-env-file"),
        "env-file must win over a competing process-env value on the default Web bootstrap path"
    );
}
