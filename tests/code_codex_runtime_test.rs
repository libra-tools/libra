//! Wave 9 / PR 9 — `libra code --provider codex` runtime
//! coverage (§5.13).
//!
//! What this test pins:
//!   * `--codex-port 0` is rejected at arg validation with the
//!     documented "must be a non-zero TCP port" error message.
//!     The runtime calls `resolve_codex_ws_url(Some(0))` from
//!     `src/command/code.rs:2212` which surfaces this error
//!     before any WebSocket connection attempt.
//!   * The shared `MockCodexWsServer` helper (lives at
//!     `tests/helpers/mock_codex_ws_server.rs`) accepts the
//!     `tokio-tungstenite` WebSocket handshake, parses incoming
//!     JSON-RPC requests, and replies with a method-aware
//!     success envelope (`initialize` → `{}`, `thread/start` →
//!     `{ thread: { id: ... } }`). This is the foundation a
//!     follow-up PR will use to drive a full
//!     `libra code --provider codex` boot end-to-end against the
//!     mock; landing the helper now de-risks the protocol
//!     plumbing for that future work.
//!   * A real `libra code --provider codex` process can boot
//!     against that mock through the normal `--codex-port` /
//!     managed-app-server path. The test pins that the binary
//!     emits `initialize` and `thread/start`, and that codex's
//!     default plan-mode instructions are present in the
//!     `thread/start` payload. It also proves the managed startup
//!     reconnects after the readiness probe's short-lived
//!     WebSocket connection is dropped.
//!   * Codex command-execution approval requests surface through
//!     Code UI as pending interactions and are not resolved back to
//!     Codex until the browser controller approves them.
//!
//! Coverage still deferred (extends the same helper):
//!   * Mid-turn Codex process restart recovery.

#[cfg(feature = "test-provider")]
mod harness;
mod helpers;

use std::{path::PathBuf, process::Command, time::Duration};
#[cfg(feature = "test-provider")]
use std::{thread, time::Instant};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
#[cfg(feature = "test-provider")]
use harness::{CodeSession, CodeSessionOptions};
use helpers::mock_codex_ws_server::{MockCodexWsConfig, MockCodexWsServer};
#[cfg(feature = "test-provider")]
use reqwest::StatusCode;
use serde_json::json;
#[cfg(feature = "test-provider")]
use serial_test::serial;
use tokio_tungstenite::tungstenite::Message;

fn libra_bin_path() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_libra")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_libra is set for integration tests")
}

#[cfg(feature = "test-provider")]
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/code_ui/basic_chat.json")
}

#[cfg(feature = "test-provider")]
fn fake_codex_bin_path() -> Result<String> {
    let path = std::env::current_exe().context("failed to resolve current test binary path")?;
    path.to_str()
        .map(str::to_owned)
        .context("current test binary path is not valid UTF-8")
}

#[cfg(feature = "test-provider")]
fn mcp_post(
    client: &reqwest::blocking::Client,
    url: &str,
    session_id: Option<&str>,
    body: &serde_json::Value,
) -> Result<(StatusCode, String)> {
    let mut request = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream, application/json");
    if let Some(session_id) = session_id {
        request = request.header("Mcp-Session-Id", session_id);
    }

    let response = request
        .json(body)
        .send()
        .with_context(|| format!("MCP POST to {url} failed"))?;
    let status = response.status();
    let body = response
        .text()
        .context("failed to read MCP response body")?;
    Ok((status, body))
}

#[cfg(feature = "test-provider")]
fn parse_sse_data(sse_text: &str) -> Vec<String> {
    sse_text
        .lines()
        .filter_map(|line| {
            line.strip_prefix("data:")
                .or_else(|| line.strip_prefix("data: "))
                .map(|data| data.trim().to_string())
        })
        .filter(|data| !data.is_empty())
        .collect()
}

#[cfg(feature = "test-provider")]
fn first_json_rpc_sse_body(method: &str, body: &str) -> Result<serde_json::Value> {
    let data = parse_sse_data(body);
    let first = data
        .first()
        .ok_or_else(|| anyhow::anyhow!("MCP {method} response had no SSE data lines: {body}"))?;
    serde_json::from_str(first)
        .with_context(|| format!("failed to parse MCP {method} JSON-RPC result: {first}"))
}

#[cfg(feature = "test-provider")]
fn mcp_initialize(client: &reqwest::blocking::Client, mcp_url: &str) -> Result<String> {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "libra-code-codex-runtime", "version": "0.0.0" }
        }
    });
    let response = client
        .post(mcp_url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream, application/json")
        .json(&initialize)
        .send()
        .with_context(|| format!("MCP initialize POST to {mcp_url} failed"))?;
    let status = response.status();
    let session_id = response
        .headers()
        .get("Mcp-Session-Id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response
        .text()
        .context("failed to read MCP initialize body")?;
    if !status.is_success() {
        bail!("MCP initialize returned non-success status {status}: {body}");
    }
    let session_id = session_id.ok_or_else(|| {
        anyhow::anyhow!("MCP initialize did not return Mcp-Session-Id header: {body}")
    })?;
    let init_result = first_json_rpc_sse_body("initialize", &body)?;
    if init_result.get("id") != Some(&serde_json::Value::from(1))
        || init_result.get("result").is_none()
    {
        bail!("MCP initialize returned malformed JSON-RPC result: {init_result}");
    }

    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    let (status, body) = mcp_post(client, mcp_url, Some(&session_id), &initialized)
        .context("failed to send MCP initialized notification")?;
    if !status.is_success() {
        bail!("MCP initialized notification failed with {status}: {body}");
    }

    Ok(session_id)
}

#[cfg(feature = "test-provider")]
fn mcp_read_resource_text(
    client: &reqwest::blocking::Client,
    mcp_url: &str,
    session_id: &str,
    request_id: u64,
    uri: &str,
) -> Result<String> {
    let request = json!({
        "jsonrpc": "2.0",
        "method": "resources/read",
        "params": { "uri": uri },
        "id": request_id,
    });
    let (status, body) = mcp_post(client, mcp_url, Some(session_id), &request)
        .with_context(|| format!("failed to read MCP resource {uri}"))?;
    if !status.is_success() {
        bail!("MCP resources/read {uri} failed with {status}: {body}");
    }
    let value = first_json_rpc_sse_body("resources/read", &body)?;
    if value.get("id") != Some(&serde_json::Value::from(request_id))
        || value.get("result").is_none()
    {
        bail!("MCP resources/read {uri} returned malformed JSON-RPC result: {value}");
    }
    Ok(value
        .pointer("/result/contents")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default())
}

#[cfg(feature = "test-provider")]
fn wait_for_mcp_resource_text_contains(
    client: &reqwest::blocking::Client,
    mcp_url: &str,
    session_id: &str,
    uri: &str,
    needle: &str,
    timeout: Duration,
) -> Result<String> {
    let deadline = Instant::now() + timeout;
    let mut last_text = String::new();
    let mut request_id = 20_u64;
    while Instant::now() < deadline {
        let text = mcp_read_resource_text(client, mcp_url, session_id, request_id, uri)?;
        if text.contains(needle) {
            return Ok(text);
        }
        last_text = text;
        request_id += 1;
        thread::sleep(Duration::from_millis(200));
    }
    bail!("timed out waiting for MCP resource {uri} to contain {needle:?}; last text:\n{last_text}")
}

/// Initialise a libra repo in a tempdir so `libra code` does not
/// trip the LBR-REPO-001 precondition before reaching the
/// `--codex-port` validator.
fn init_libra_repo() -> Result<tempfile::TempDir> {
    let temp = tempfile::Builder::new()
        .prefix("code-codex-runtime-")
        .tempdir()
        .context("failed to create codex test tempdir")?;
    let repo_dir = temp.path().to_path_buf();
    let output = Command::new(libra_bin_path())
        .current_dir(&repo_dir)
        .args(["init", "--vault=false", "--quiet"])
        .arg(&repo_dir)
        .output()
        .context("failed to run 'libra init'")?;
    if !output.status.success() {
        bail!(
            "libra init failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    Ok(temp)
}

/// Wave 9 §5.13 partial — `--codex-port 0` is rejected at arg
/// validation. Driven via `Command::new(libra)` because the
/// rejection happens during arg parsing / preflight, before any
/// runtime work runs (no PTY, no WebSocket connection attempt).
///
/// Pins the documented error message at
/// `src/command/code.rs:2216` ("--codex-port must be a non-zero
/// TCP port; omit it to auto-select a free port") so a future
/// validator change cannot silently downgrade the user-facing
/// guidance.
#[test]
fn libra_code_codex_port_zero_is_rejected_at_arg_validation() -> Result<()> {
    let repo = init_libra_repo()?;
    let output = Command::new(libra_bin_path())
        .arg("code")
        .arg("--cwd")
        .arg(repo.path())
        .args(["--provider", "codex", "--codex-port", "0"])
        .output()
        .context("failed to spawn libra code --provider codex --codex-port 0")?;
    if output.status.success() {
        bail!(
            "expected --codex-port 0 to fail validation, but exit was successful;\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        combined.contains("--codex-port") && combined.contains("non-zero"),
        "expected the documented '--codex-port must be a non-zero TCP port' error; got:\n{combined}",
    );
    Ok(())
}

/// Wave 9 §5.13 foundation — `MockCodexWsServer` round-trip
/// smoke.
///
/// Spawns the mock on `127.0.0.1:0`, opens a `tokio-tungstenite`
/// WebSocket client (mirroring how libra's Codex client calls
/// `connect_async`), and:
///
///   1. Sends a JSON-RPC `initialize` request and asserts the
///      mock responds with a success envelope keyed off the
///      request id.
///   2. Sends a JSON-RPC `thread/start` request and asserts the
///      response carries the configured thread id at
///      `result.thread.id` (matching the real Codex server's
///      shape that libra's runtime extracts via the
///      `thread.id` / `threadId` / `thread_id` fallback chain
///      at `src/internal/ai/codex/mod.rs:6200`).
///   3. Asserts both requests appear in `captured_requests()`
///      so a follow-up PR can drive the libra binary against
///      this same mock and verify the protocol payload shape.
#[tokio::test]
async fn mock_codex_ws_server_handles_initialize_and_thread_start_round_trip() -> Result<()> {
    let server = MockCodexWsServer::start(MockCodexWsConfig {
        thread_id: Some("wave-9-§5-13-thread".to_string()),
        ..Default::default()
    })
    .await?;
    let url = server.ws_url();
    let (ws_stream, _response) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(url.as_str()),
    )
    .await
    .context("ws connect timed out after 5s")??;
    let (mut write, mut read) = ws_stream.split();

    // initialize round trip.
    let init_payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": { "name": "libra-test", "version": "0.0.0" },
            "cwd": "/tmp/wave-9-codex"
        }
    });
    write
        .send(Message::Text(init_payload.to_string().into()))
        .await
        .context("send initialize")?;
    let init_resp_text = match tokio::time::timeout(Duration::from_secs(5), read.next())
        .await
        .context("await initialize response")?
    {
        Some(Ok(Message::Text(text))) => text.to_string(),
        Some(Ok(other)) => bail!("unexpected initialize response frame: {other:?}"),
        Some(Err(err)) => bail!("ws error reading initialize response: {err}"),
        None => bail!("mock closed before responding to initialize"),
    };
    let init_resp: serde_json::Value =
        serde_json::from_str(&init_resp_text).context("parse initialize response")?;
    assert_eq!(
        init_resp.get("id").and_then(|v| v.as_u64()),
        Some(1),
        "initialize response must echo the request id; got {init_resp}",
    );
    assert!(
        init_resp.get("result").is_some(),
        "initialize response must carry a result envelope; got {init_resp}",
    );

    // thread/start round trip.
    let thread_payload = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "thread/start",
        "params": {
            "cwd": "/tmp/wave-9-codex",
            "approvalPolicy": "on-request",
        }
    });
    write
        .send(Message::Text(thread_payload.to_string().into()))
        .await
        .context("send thread/start")?;
    let thread_resp_text = match tokio::time::timeout(Duration::from_secs(5), read.next())
        .await
        .context("await thread/start response")?
    {
        Some(Ok(Message::Text(text))) => text.to_string(),
        Some(Ok(other)) => bail!("unexpected thread/start response frame: {other:?}"),
        Some(Err(err)) => bail!("ws error reading thread/start response: {err}"),
        None => bail!("mock closed before responding to thread/start"),
    };
    let thread_resp: serde_json::Value =
        serde_json::from_str(&thread_resp_text).context("parse thread/start response")?;
    assert_eq!(
        thread_resp
            .pointer("/result/thread/id")
            .and_then(|v| v.as_str()),
        Some("wave-9-§5-13-thread"),
        "thread/start response must surface result.thread.id matching the configured value; got {thread_resp}",
    );

    // Captured requests for downstream-PR assertions.
    let captured = server.captured_requests();
    assert_eq!(captured.len(), 2, "expected exactly two captured requests");
    assert_eq!(
        captured[0].get("method").and_then(|v| v.as_str()),
        Some("initialize"),
    );
    assert_eq!(
        captured[1].get("method").and_then(|v| v.as_str()),
        Some("thread/start"),
    );
    Ok(())
}

/// Wave 9 §5.13 binary boot smoke — real `libra code` process
/// against the mock Codex app-server.
///
/// The production path always calls `start_managed_codex_server`,
/// which first spawns `--codex-bin app-server --listen <ws_url>`
/// and then probes the chosen `--codex-port`. To keep the test
/// deterministic without the real Codex binary, the mock binds
/// the selected port first and `--codex-bin <current-test-binary>`
/// satisfies the child-spawn contract. The readiness probe and the actual
/// Code UI runtime then connect to the mock WebSocket endpoint.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn libra_code_provider_codex_boots_against_mock_app_server() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("create tokio runtime")?;
    let server = runtime.block_on(MockCodexWsServer::start(MockCodexWsConfig {
        thread_id: Some("wave-9-binary-codex-thread".to_string()),
        ..Default::default()
    }))?;
    let port = server.port().to_string();
    let fake_codex_bin = fake_codex_bin_path()?;

    let session = CodeSession::spawn(
        CodeSessionOptions::new("code-codex-binary-boot", fixture_path())
            .with_live_provider("codex", "codex-test")
            .push_extra_cli_arg("--codex-port")
            .push_extra_cli_arg(port)
            .push_extra_cli_arg("--codex-bin")
            .push_extra_cli_arg(fake_codex_bin),
    )?;

    let snapshot = session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        snapshot
            .pointer("/provider/provider")
            .and_then(serde_json::Value::as_str)
            == Some("codex")
    })?;
    assert_eq!(
        snapshot
            .pointer("/provider/mode")
            .and_then(serde_json::Value::as_str),
        Some("web"),
        "the actual managed Codex launch path must publish the Web-only provider mode; got {snapshot}",
    );

    let captured = wait_for_codex_methods(
        &server,
        &["initialize", "thread/start"],
        Duration::from_secs(10),
    )?;
    assert!(
        server.connection_count() >= 2,
        "managed Codex startup must reconnect after the readiness probe disconnects; accepted {} connection(s)",
        server.connection_count(),
    );
    let methods = captured
        .iter()
        .filter_map(|request| request.get("method").and_then(|value| value.as_str()))
        .collect::<Vec<_>>();
    assert!(
        methods.contains(&"initialize") && methods.contains(&"thread/start"),
        "binary codex boot must issue initialize and thread/start; got {methods:?}",
    );

    let thread_start = captured
        .iter()
        .find(|request| {
            request.get("method").and_then(|value| value.as_str()) == Some("thread/start")
        })
        .ok_or_else(|| anyhow::anyhow!("captured requests did not include thread/start"))?;
    assert_eq!(
        thread_start
            .pointer("/params/model")
            .and_then(|value| value.as_str()),
        Some("codex-test"),
        "thread/start must forward the selected model; got {thread_start}",
    );
    assert!(
        thread_start
            .pointer("/params/developerInstructions")
            .is_some_and(|value| value.is_string()),
        "codex defaults to plan mode, so thread/start must include developerInstructions; got {thread_start}",
    );
    assert!(
        thread_start
            .pointer("/params/baseInstructions")
            .is_some_and(|value| value.is_string()),
        "codex defaults to plan mode, so thread/start must include baseInstructions; got {thread_start}",
    );
    Ok(())
}

/// Wave 9 §5.13 notification persistence smoke — mock Codex
/// `thread/started` notification is written to `.libra/objects/`
/// and becomes visible through the MCP history index.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn libra_code_provider_codex_persists_thread_started_notification() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("create tokio runtime")?;
    let thread_id = "wave-9-codex-persisted-thread";
    let server = runtime.block_on(MockCodexWsServer::start(MockCodexWsConfig {
        thread_id: Some(thread_id.to_string()),
        emit_thread_started: true,
        ..Default::default()
    }))?;
    let port = server.port().to_string();
    let fake_codex_bin = fake_codex_bin_path()?;

    let session = CodeSession::spawn(
        CodeSessionOptions::new("code-codex-notification-persist", fixture_path())
            .with_live_provider("codex", "codex-test")
            .push_extra_cli_arg("--codex-port")
            .push_extra_cli_arg(port)
            .push_extra_cli_arg("--codex-bin")
            .push_extra_cli_arg(fake_codex_bin),
    )?;
    let _captured = wait_for_codex_methods(
        &server,
        &["initialize", "thread/start"],
        Duration::from_secs(10),
    )?;

    let mcp_url = session
        .mcp_url()
        .ok_or_else(|| anyhow::anyhow!("control.json did not surface mcpUrl after spawn"))?
        .to_string();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("build MCP history client")?;
    let mcp_session_id = mcp_initialize(&client, &mcp_url)?;
    let event_index = wait_for_mcp_resource_text_contains(
        &client,
        &mcp_url,
        &mcp_session_id,
        "libra://objects/event",
        thread_id,
        Duration::from_secs(10),
    )?;
    let event_id = event_index
        .lines()
        .find(|line| line.contains(thread_id))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "event history index did not expose an event id for {thread_id}: {event_index}"
            )
        })?;
    let event_json = mcp_read_resource_text(
        &client,
        &mcp_url,
        &mcp_session_id,
        200,
        &format!("libra://object/{event_id}"),
    )?;
    assert!(
        event_json.contains("\"status\":\"started\"")
            || event_json.contains("\"status\": \"started\""),
        "persisted thread event must record status=started; got:\n{event_json}",
    );
    assert!(
        event_json.contains(thread_id),
        "persisted thread event must include the Codex thread id; got:\n{event_json}",
    );
    Ok(())
}

/// Wave 9 §5.13 plan gate smoke — Codex must not receive an
/// execution approval resolve until the Code UI user explicitly
/// responds to the surfaced interaction.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn libra_code_provider_codex_waits_for_user_before_execution_approval_resolve() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("create tokio runtime")?;
    let server = runtime.block_on(MockCodexWsServer::start(MockCodexWsConfig {
        thread_id: Some("wave-9-codex-plan-gate-thread".to_string()),
        emit_turn_command_approval: true,
        ..Default::default()
    }))?;
    let port = server.port().to_string();
    let fake_codex_bin = fake_codex_bin_path()?;

    let session = CodeSession::spawn(
        CodeSessionOptions::new("code-codex-plan-gate", fixture_path())
            .with_live_provider("codex", "codex-test")
            .with_browser_control_loopback()
            .push_extra_cli_arg("--codex-port")
            .push_extra_cli_arg(port)
            .push_extra_cli_arg("--codex-bin")
            .push_extra_cli_arg(fake_codex_bin),
    )?;
    let _captured = wait_for_codex_methods(
        &server,
        &["initialize", "thread/start"],
        Duration::from_secs(10),
    )?;

    let browser_token = session.attach_browser("codex-plan-gate-browser")?;
    let (status, body) =
        session.browser_submit_message(&browser_token, "execute only after approval")?;
    assert!(
        status.is_success(),
        "browser submit must succeed before approval gate assertion; got {status}: {body}",
    );
    let _captured = wait_for_codex_methods(&server, &["turn/start"], Duration::from_secs(10))?;

    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        snapshot
            .pointer("/interactions")
            .and_then(serde_json::Value::as_array)
            .map(|interactions| {
                interactions.iter().any(|interaction| {
                    interaction
                        .pointer("/id")
                        .and_then(serde_json::Value::as_str)
                        == Some("codex-plan-gate-command-approval")
                        && interaction
                            .pointer("/status")
                            .and_then(serde_json::Value::as_str)
                            == Some("pending")
                })
            })
            .unwrap_or(false)
    })?;

    let before_response = server.captured_requests();
    assert!(
        !before_response.iter().any(|request| {
            request.get("method").and_then(serde_json::Value::as_str)
                == Some("item/commandExecution/requestApproval/resolve")
        }),
        "Codex execution approval must not be resolved before user response; captured {before_response:?}",
    );

    let (status, body) =
        session.browser_respond_interaction(&browser_token, "codex-plan-gate-command-approval")?;
    assert!(
        status.is_success(),
        "browser approval response must succeed; got {status}: {body}",
    );
    let captured = wait_for_codex_methods(
        &server,
        &["item/commandExecution/requestApproval/resolve"],
        Duration::from_secs(10),
    )?;
    let resolve = captured
        .iter()
        .find(|request| {
            request.get("method").and_then(serde_json::Value::as_str)
                == Some("item/commandExecution/requestApproval/resolve")
        })
        .ok_or_else(|| anyhow::anyhow!("captured requests did not include approval resolve"))?;
    assert_eq!(
        resolve
            .pointer("/params/requestId")
            .and_then(serde_json::Value::as_str),
        Some("codex-plan-gate-command-approval"),
        "approval resolve must target the surfaced interaction; got {resolve}",
    );
    assert_eq!(
        resolve
            .pointer("/params/approved")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "approval resolve must carry the user's approval decision; got {resolve}",
    );
    Ok(())
}

/// W3-07: a second approval on the same managed Codex turn must remain
/// registerable after the first response is forwarded (no auto-decline).
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn libra_code_provider_codex_registers_sequential_approvals_on_same_turn() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("create tokio runtime")?;
    let server = runtime.block_on(MockCodexWsServer::start(MockCodexWsConfig {
        thread_id: Some("wave-9-codex-multi-approval-thread".to_string()),
        emit_turn_command_approval: true,
        emit_second_turn_command_approval: true,
        ..Default::default()
    }))?;
    let port = server.port().to_string();
    let fake_codex_bin = fake_codex_bin_path()?;

    let session = CodeSession::spawn(
        CodeSessionOptions::new("code-codex-multi-approval", fixture_path())
            .with_live_provider("codex", "codex-test")
            .with_browser_control_loopback()
            .push_extra_cli_arg("--codex-port")
            .push_extra_cli_arg(port)
            .push_extra_cli_arg("--codex-bin")
            .push_extra_cli_arg(fake_codex_bin),
    )?;
    let _ = wait_for_codex_methods(
        &server,
        &["initialize", "thread/start"],
        Duration::from_secs(10),
    )?;

    let browser_token = session.attach_browser("codex-multi-approval-browser")?;
    let (status, body) =
        session.browser_submit_message(&browser_token, "approve two sequential gates")?;
    assert!(
        status.is_success(),
        "browser submit must succeed; got {status}: {body}",
    );
    let _ = wait_for_codex_methods(&server, &["turn/start"], Duration::from_secs(10))?;

    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        snapshot
            .pointer("/interactions")
            .and_then(serde_json::Value::as_array)
            .map(|interactions| {
                interactions.iter().any(|interaction| {
                    interaction
                        .pointer("/id")
                        .and_then(serde_json::Value::as_str)
                        == Some("codex-plan-gate-command-approval")
                        && interaction
                            .pointer("/status")
                            .and_then(serde_json::Value::as_str)
                            == Some("pending")
                })
            })
            .unwrap_or(false)
    })?;

    let (status, body) =
        session.browser_respond_interaction(&browser_token, "codex-plan-gate-command-approval")?;
    assert!(
        status.is_success(),
        "first approval response must succeed; got {status}: {body}",
    );
    let _ = wait_for_codex_methods(
        &server,
        &["item/commandExecution/requestApproval/resolve"],
        Duration::from_secs(10),
    )?;

    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        snapshot
            .pointer("/interactions")
            .and_then(serde_json::Value::as_array)
            .map(|interactions| {
                interactions.iter().any(|interaction| {
                    interaction
                        .pointer("/id")
                        .and_then(serde_json::Value::as_str)
                        == Some("codex-plan-gate-command-approval-2")
                        && interaction
                            .pointer("/status")
                            .and_then(serde_json::Value::as_str)
                            == Some("pending")
                })
            })
            .unwrap_or(false)
    })?;

    let (status, body) = session
        .browser_respond_interaction(&browser_token, "codex-plan-gate-command-approval-2")?;
    assert!(
        status.is_success(),
        "second approval response must succeed; got {status}: {body}",
    );
    let captured = wait_for_codex_methods(
        &server,
        &["item/commandExecution/requestApproval/resolve"],
        Duration::from_secs(10),
    )?;
    let resolve_ids: Vec<&str> = captured
        .iter()
        .filter(|request| {
            request.get("method").and_then(serde_json::Value::as_str)
                == Some("item/commandExecution/requestApproval/resolve")
        })
        .filter_map(|request| {
            request
                .pointer("/params/requestId")
                .and_then(serde_json::Value::as_str)
        })
        .collect();
    assert!(
        resolve_ids.contains(&"codex-plan-gate-command-approval")
            && resolve_ids.contains(&"codex-plan-gate-command-approval-2"),
        "both sequential approvals must resolve to the app-server; got {resolve_ids:?}"
    );
    Ok(())
}

/// W3-07: cancel while an approval is pending must decline the gate, interrupt
/// the known app-server turn, and free the sidecar so a later submit works.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn libra_code_provider_codex_cancel_during_pending_approval_interrupts_turn() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("create tokio runtime")?;
    let server = runtime.block_on(MockCodexWsServer::start(MockCodexWsConfig {
        thread_id: Some("wave-9-codex-cancel-approval-thread".to_string()),
        emit_turn_command_approval: true,
        ..Default::default()
    }))?;
    let port = server.port().to_string();
    let fake_codex_bin = fake_codex_bin_path()?;

    let session = CodeSession::spawn(
        CodeSessionOptions::new("code-codex-cancel-approval", fixture_path())
            .with_live_provider("codex", "codex-test")
            .with_browser_control_loopback()
            .push_extra_cli_arg("--codex-port")
            .push_extra_cli_arg(port)
            .push_extra_cli_arg("--codex-bin")
            .push_extra_cli_arg(fake_codex_bin),
    )?;
    let _ = wait_for_codex_methods(
        &server,
        &["initialize", "thread/start"],
        Duration::from_secs(10),
    )?;

    let browser_token = session.attach_browser("codex-cancel-approval-browser")?;
    let (status, body) =
        session.browser_submit_message(&browser_token, "cancel while approval is pending")?;
    assert!(
        status.is_success(),
        "browser submit must succeed; got {status}: {body}",
    );
    let _ = wait_for_codex_methods(&server, &["turn/start"], Duration::from_secs(10))?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        snapshot
            .pointer("/interactions")
            .and_then(serde_json::Value::as_array)
            .map(|interactions| {
                interactions.iter().any(|interaction| {
                    interaction
                        .pointer("/id")
                        .and_then(serde_json::Value::as_str)
                        == Some("codex-plan-gate-command-approval")
                })
            })
            .unwrap_or(false)
    })?;

    let (status, body) = session.browser_cancel_turn(&browser_token)?;
    assert!(
        status.is_success(),
        "browser cancel during approval must succeed; got {status}: {body}",
    );
    let captured = wait_for_codex_methods(
        &server,
        &[
            "item/commandExecution/requestApproval/resolve",
            "turn/interrupt",
        ],
        Duration::from_secs(10),
    )?;
    let declined = captured.iter().any(|request| {
        request.get("method").and_then(serde_json::Value::as_str)
            == Some("item/commandExecution/requestApproval/resolve")
            && request
                .pointer("/params/approved")
                .and_then(serde_json::Value::as_bool)
                == Some(false)
    });
    assert!(
        declined,
        "cancel must decline the pending approval; captured {captured:?}"
    );
    assert!(
        captured.iter().any(|request| {
            request.get("method").and_then(serde_json::Value::as_str) == Some("turn/interrupt")
        }),
        "cancel must interrupt the known app-server turn; captured {captured:?}"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_submit = None;
    while Instant::now() < deadline {
        let (status, body) = session.browser_submit_message(&browser_token, "after cancel")?;
        if status.is_success() {
            last_submit = Some(Ok(()));
            break;
        }
        last_submit = Some(Err((status, body)));
        thread::sleep(Duration::from_millis(100));
    }
    match last_submit {
        Some(Ok(())) => {}
        Some(Err((status, body))) => {
            bail!(
                "submit after cancel must succeed once turn/completed arrives; last {status}: {body}"
            );
        }
        None => bail!("submit after cancel was not attempted"),
    }
    Ok(())
}

/// W3-07: cancel after `turn/started` still declines a pending approval and
/// interrupts using the nested turn id.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn libra_code_provider_codex_cancel_after_turn_started_interrupts_turn() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("create tokio runtime")?;
    let server = runtime.block_on(MockCodexWsServer::start(MockCodexWsConfig {
        thread_id: Some("wave-9-codex-cancel-started-thread".to_string()),
        emit_turn_command_approval: true,
        emit_turn_started_after_approval: true,
        ..Default::default()
    }))?;
    let port = server.port().to_string();
    let fake_codex_bin = fake_codex_bin_path()?;

    let session = CodeSession::spawn(
        CodeSessionOptions::new("code-codex-cancel-started", fixture_path())
            .with_live_provider("codex", "codex-test")
            .with_browser_control_loopback()
            .push_extra_cli_arg("--codex-port")
            .push_extra_cli_arg(port)
            .push_extra_cli_arg("--codex-bin")
            .push_extra_cli_arg(fake_codex_bin),
    )?;
    let _ = wait_for_codex_methods(
        &server,
        &["initialize", "thread/start"],
        Duration::from_secs(10),
    )?;

    let browser_token = session.attach_browser("codex-cancel-started-browser")?;
    let (status, body) =
        session.browser_submit_message(&browser_token, "cancel after turn started")?;
    assert!(
        status.is_success(),
        "browser submit must succeed; got {status}: {body}",
    );
    let _ = wait_for_codex_methods(&server, &["turn/start"], Duration::from_secs(10))?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        snapshot
            .pointer("/interactions")
            .and_then(serde_json::Value::as_array)
            .map(|interactions| {
                interactions.iter().any(|interaction| {
                    interaction
                        .pointer("/id")
                        .and_then(serde_json::Value::as_str)
                        == Some("codex-plan-gate-command-approval")
                })
            })
            .unwrap_or(false)
    })?;

    let (status, body) = session.browser_cancel_turn(&browser_token)?;
    assert!(
        status.is_success(),
        "browser cancel after turn/started must succeed; got {status}: {body}",
    );
    let captured = wait_for_codex_methods(
        &server,
        &[
            "item/commandExecution/requestApproval/resolve",
            "turn/interrupt",
        ],
        Duration::from_secs(10),
    )?;
    assert!(
        captured.iter().any(|request| {
            request.get("method").and_then(serde_json::Value::as_str) == Some("turn/interrupt")
                && request
                    .pointer("/params/turnId")
                    .or_else(|| request.pointer("/params/turn/id"))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| id.contains("mock-turn-"))
        }),
        "cancel after turn/started must interrupt with the nested turn id; captured {captured:?}"
    );
    Ok(())
}

/// W3-07: cancel before `turn/started` parks interrupt; when the nested turn
/// id later arrives, interrupt is dispatched off the WS reader (no deadlock).
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn libra_code_provider_codex_cancel_before_turn_started_interrupts_when_started_arrives()
-> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("create tokio runtime")?;
    let server = runtime.block_on(MockCodexWsServer::start(MockCodexWsConfig {
        thread_id: Some("wave-9-codex-cancel-prestart-thread".to_string()),
        emit_turn_command_approval: true,
        turn_start_omits_turn_id: true,
        emit_turn_started_after_declined_approval: true,
        ..Default::default()
    }))?;
    let port = server.port().to_string();
    let fake_codex_bin = fake_codex_bin_path()?;

    let session = CodeSession::spawn(
        CodeSessionOptions::new("code-codex-cancel-prestart", fixture_path())
            .with_live_provider("codex", "codex-test")
            .with_browser_control_loopback()
            .push_extra_cli_arg("--codex-port")
            .push_extra_cli_arg(port)
            .push_extra_cli_arg("--codex-bin")
            .push_extra_cli_arg(fake_codex_bin),
    )?;
    let _ = wait_for_codex_methods(
        &server,
        &["initialize", "thread/start"],
        Duration::from_secs(10),
    )?;

    let browser_token = session.attach_browser("codex-cancel-prestart-browser")?;
    let (status, body) =
        session.browser_submit_message(&browser_token, "cancel before turn started")?;
    assert!(
        status.is_success(),
        "browser submit must succeed; got {status}: {body}",
    );
    let _ = wait_for_codex_methods(&server, &["turn/start"], Duration::from_secs(10))?;
    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        snapshot
            .pointer("/interactions")
            .and_then(serde_json::Value::as_array)
            .map(|interactions| {
                interactions.iter().any(|interaction| {
                    interaction
                        .pointer("/id")
                        .and_then(serde_json::Value::as_str)
                        == Some("codex-plan-gate-command-approval")
                })
            })
            .unwrap_or(false)
    })?;

    let (status, body) = session.browser_cancel_turn(&browser_token)?;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "cancel before turn id must park interrupt; got {status}: {body}",
    );
    assert!(
        body.pointer("/error/code")
            .and_then(serde_json::Value::as_str)
            == Some("TURN_INTERRUPT_PENDING"),
        "expected TURN_INTERRUPT_PENDING; got {body}",
    );

    let captured = wait_for_codex_methods(
        &server,
        &[
            "item/commandExecution/requestApproval/resolve",
            "turn/interrupt",
        ],
        Duration::from_secs(10),
    )?;
    assert!(
        captured.iter().any(|request| {
            request.get("method").and_then(serde_json::Value::as_str) == Some("turn/interrupt")
        }),
        "pending cancel must interrupt once turn/started arrives; captured {captured:?}"
    );
    Ok(())
}

/// W3-04: every Codex MethodKind (plus unknown) maps into the shared runtime
/// `AgentEvent` envelope; unknown methods take an explicit diagnosable fallback.
#[test]
fn codex_events_normalized_into_runtime_envelope() {
    use libra::internal::ai::{
        codex::{
            all_known_method_kinds, method_kind_classification, normalize_codex_notification,
            protocol::MethodKind, sample_method_for_kind,
        },
        runtime::AgentEventKind,
    };
    use serde_json::json;

    for kind in all_known_method_kinds() {
        let method = sample_method_for_kind(*kind);
        assert_eq!(
            MethodKind::from(method),
            *kind,
            "sample_method_for_kind({kind:?}) must classify back to the same MethodKind"
        );
        let params = match kind {
            MethodKind::RequestApproval
            | MethodKind::RequestApprovalCommandExecution
            | MethodKind::RequestApprovalFileChange
            | MethodKind::RequestApprovalApplyPatch
            | MethodKind::RequestApprovalExec => json!({"requestId": "req-table"}),
            MethodKind::TurnCompleted => json!({"status": "completed"}),
            _ => json!({}),
        };
        let envelope = normalize_codex_notification(method, &params);
        assert!(
            !envelope.used_fallback,
            "known MethodKind {kind:?} must not use unknown fallback"
        );
        assert_eq!(
            envelope.classification,
            method_kind_classification(*kind),
            "classification must match MethodKind discriminant"
        );
        assert!(
            !envelope.kinds().is_empty(),
            "known MethodKind {kind:?} must emit ≥1 AgentEventKind"
        );
        match kind {
            MethodKind::TurnStarted => {
                assert!(matches!(
                    envelope.primary_kind(),
                    Some(AgentEventKind::TurnStarted)
                ));
            }
            MethodKind::TurnCompleted => {
                assert!(matches!(
                    envelope.primary_kind(),
                    Some(AgentEventKind::TurnCompleted { .. })
                ));
            }
            MethodKind::RequestApproval
            | MethodKind::RequestApprovalCommandExecution
            | MethodKind::RequestApprovalFileChange
            | MethodKind::RequestApprovalApplyPatch
            | MethodKind::RequestApprovalExec => {
                assert!(matches!(
                    envelope.primary_kind(),
                    Some(AgentEventKind::InteractionRequested { .. })
                ));
            }
            _ => match envelope.primary_kind() {
                Some(AgentEventKind::ProviderNotification {
                    provider,
                    method: recorded_method,
                    classification,
                    ..
                }) => {
                    assert_eq!(provider, "codex");
                    assert_eq!(recorded_method, method);
                    assert_eq!(classification, &method_kind_classification(*kind));
                }
                other => panic!("expected ProviderNotification for {kind:?}, got {other:?}"),
            },
        }
    }

    let unknown =
        normalize_codex_notification("codex/future/notYetClassified", &json!({"ignored": true}));
    assert!(unknown.used_fallback);
    assert_eq!(unknown.classification, "unknown");
    match unknown.primary_kind() {
        Some(AgentEventKind::ProviderNotification {
            provider,
            method,
            classification,
            detail,
        }) => {
            assert_eq!(provider, "codex");
            assert_eq!(method, "codex/future/notYetClassified");
            assert_eq!(classification, "unknown");
            assert!(
                detail.contains("unrecognized"),
                "fallback detail must be diagnosable; got {detail}"
            );
        }
        other => panic!("unknown Codex method must not silent-drop; got {other:?}"),
    }
}

/// W3-04: mock Codex `turn/completed` (failed) + unknown notification must
/// normalize through the Code UI WebSocket reader into a terminal Error
/// snapshot without panicking on the unknown method.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn libra_code_provider_codex_normalizes_failed_turn_and_unknown_notification() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("create tokio runtime")?;
    let server = runtime.block_on(MockCodexWsServer::start(MockCodexWsConfig {
        thread_id: Some("wave-w3-04-codex-envelope-thread".to_string()),
        emit_turn_completed_status: Some("failed".to_string()),
        emit_unknown_notification: true,
        ..Default::default()
    }))?;
    let port = server.port().to_string();
    let fake_codex_bin = fake_codex_bin_path()?;

    let session = CodeSession::spawn(
        CodeSessionOptions::new("code-codex-envelope-normalize", fixture_path())
            .with_live_provider("codex", "codex-test")
            .with_browser_control_loopback()
            .push_extra_cli_arg("--codex-port")
            .push_extra_cli_arg(port)
            .push_extra_cli_arg("--codex-bin")
            .push_extra_cli_arg(fake_codex_bin),
    )?;
    let _captured = wait_for_codex_methods(
        &server,
        &["initialize", "thread/start"],
        Duration::from_secs(10),
    )?;

    let browser_token = session.attach_browser("codex-envelope-browser")?;
    let (status, body) =
        session.browser_submit_message(&browser_token, "trigger failed turn envelope")?;
    assert!(
        status.is_success(),
        "browser submit must succeed before envelope assertion; got {status}: {body}",
    );
    let _captured = wait_for_codex_methods(&server, &["turn/start"], Duration::from_secs(10))?;

    session.wait_for_snapshot(Duration::from_secs(10), |snapshot| {
        snapshot
            .pointer("/status")
            .and_then(serde_json::Value::as_str)
            == Some("error")
    })?;
    Ok(())
}

#[cfg(feature = "test-provider")]
fn wait_for_codex_methods(
    server: &MockCodexWsServer,
    expected: &[&str],
    timeout: Duration,
) -> Result<Vec<serde_json::Value>> {
    let deadline = Instant::now() + timeout;
    let mut last = Vec::new();
    while Instant::now() < deadline {
        let captured = server.captured_requests();
        if expected.iter().all(|method| {
            captured.iter().any(|request| {
                request.get("method").and_then(|value| value.as_str()) == Some(*method)
            })
        }) {
            return Ok(captured);
        }
        last = captured;
        thread::sleep(Duration::from_millis(100));
    }
    bail!("timed out waiting for codex methods {expected:?}; captured requests: {last:?}")
}
