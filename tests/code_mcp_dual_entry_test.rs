//! Wave 9 / PR 9 — `libra code` MCP entry-point coverage (§5.14,
//! partial).
//!
//! Coverage included here:
//!   * **Item 1 — automation discovery**: after `libra code`
//!     starts, the runtime writes the MCP server URL into
//!     `--control-info-file` so a downstream automation client
//!     can discover the MCP endpoint without scraping logs.
//!     The harness now parses `mcpUrl` from `control.json` and
//!     this test asserts (a) the field is populated, (b) it
//!     points at a loopback `http://127.0.0.1:<port>/mcp`-style
//!     URL, (c) the `<port>` differs from the web port (the
//!     runtime requires the two to be distinct outside `--stdio`
//!     mode, see `code.rs:3354` "Web and MCP ports must differ").
//!   * **Item 2 — `--stdio` mutex**: clap-level mutual exclusion
//!     of `--stdio` and `--web-only`. Pins that the conflict is
//!     surfaced as a usage error before any runtime work runs.
//!
//!   * **Item 3 — dual-reachability smoke**: same `libra code`
//!     process responds on BOTH the web HTTP transport
//!     (`/api/code/session`) AND the MCP Streamable HTTP
//!     transport (`<mcpUrl>` POST `initialize`). Proves the two
//!     entry points share a process.
//!   * **Item 3 — web→MCP consistency**: a message submitted
//!     through web `/messages` is observed by a live web SSE
//!     subscriber and is then visible through MCP `tools/call`
//!     `list_tasks` on the same process.
//!   * **Item 3 — MCP→web consistency**: a task created through
//!     MCP `tools/call create_task` is broadcast through the Code
//!     UI read model and observed by a live web SSE subscriber.

#[cfg(feature = "test-provider")]
mod harness;

#[cfg(feature = "test-provider")]
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[cfg(feature = "test-provider")]
use anyhow::{Context, Result, bail};
#[cfg(feature = "test-provider")]
use harness::{CodeSession, CodeSessionOptions};
#[cfg(feature = "test-provider")]
use reqwest::StatusCode;
#[cfg(feature = "test-provider")]
use serde_json::{Value, json};
#[cfg(feature = "test-provider")]
use serial_test::serial;

#[cfg(feature = "test-provider")]
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/code_ui/basic_chat.json")
}

#[cfg(feature = "test-provider")]
fn libra_bin_path() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_libra")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_libra is set for integration tests")
}

#[cfg(feature = "test-provider")]
fn parse_sse_data(sse_text: &str) -> Vec<String> {
    sse_text
        .lines()
        .filter_map(|line| {
            line.strip_prefix("data:")
                .or_else(|| line.strip_prefix("data: "))
                .map(|d| d.trim().to_string())
        })
        .filter(|d| !d.is_empty())
        .collect()
}

#[cfg(feature = "test-provider")]
fn mcp_post(
    client: &reqwest::blocking::Client,
    url: &str,
    session_id: Option<&str>,
    body: &Value,
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
fn first_json_rpc_sse_body(method: &str, body: &str) -> Result<Value> {
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
            "clientInfo": { "name": "libra-code-mcp-dual-entry", "version": "0.0.0" }
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
        .and_then(|v| v.to_str().ok())
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
    if session_id.is_empty() {
        bail!("MCP initialize returned an empty Mcp-Session-Id header");
    }

    let init_result = first_json_rpc_sse_body("initialize", &body)?;
    if init_result.get("id") != Some(&Value::from(1)) || init_result.get("result").is_none() {
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
fn mcp_call_tool(
    client: &reqwest::blocking::Client,
    mcp_url: &str,
    session_id: &str,
    request_id: u64,
    name: &str,
    arguments: Value,
) -> Result<Value> {
    let request = json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments,
        },
        "id": request_id,
    });
    let (status, body) = mcp_post(client, mcp_url, Some(session_id), &request)
        .with_context(|| format!("failed to call MCP tool {name}"))?;
    if !status.is_success() {
        bail!("MCP tools/call {name} failed with {status}: {body}");
    }
    let value = first_json_rpc_sse_body(name, &body)?;
    if value.get("id") != Some(&Value::from(request_id)) || value.get("result").is_none() {
        bail!("MCP tools/call {name} returned malformed JSON-RPC result: {value}");
    }
    Ok(value)
}

#[cfg(feature = "test-provider")]
fn mcp_result_text(value: &Value) -> String {
    value
        .pointer("/result/content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[cfg(feature = "test-provider")]
fn event_payload_transcript_contains(payload: &Value, needle: &str) -> bool {
    payload
        .pointer("/data/transcript")
        .and_then(Value::as_array)
        .is_some_and(|transcript| {
            transcript.iter().any(|entry| {
                let matches = |key: &str| {
                    entry
                        .get(key)
                        .and_then(Value::as_str)
                        .is_some_and(|value| value.contains(needle))
                };
                matches("content") || matches("title")
            })
        })
}

#[cfg(feature = "test-provider")]
fn wait_for_sse_transcript(
    events: &mut harness::EventStream,
    needle: &str,
    timeout: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    let mut last_event = "<none>".to_string();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Some(event) = events.next_event(remaining.min(Duration::from_secs(1)))? else {
            continue;
        };
        last_event = format!("event={} data={}", event.event, event.data);
        if event.event != "session_updated" {
            continue;
        }
        let payload: Value = serde_json::from_str(&event.data)
            .with_context(|| format!("failed to parse SSE payload: {}", event.data))?;
        if event_payload_transcript_contains(&payload, needle) {
            return Ok(payload);
        }
    }
    bail!("timed out waiting for SSE transcript to contain {needle:?}; last event: {last_event}")
}

#[cfg(feature = "test-provider")]
fn wait_for_mcp_task(
    client: &reqwest::blocking::Client,
    mcp_url: &str,
    session_id: &str,
    needle: &str,
    timeout: Duration,
) -> Result<String> {
    let deadline = Instant::now() + timeout;
    let mut last_text = String::new();
    let mut request_id = 10_u64;
    while Instant::now() < deadline {
        let value = mcp_call_tool(
            client,
            mcp_url,
            session_id,
            request_id,
            "list_tasks",
            json!({ "limit": 20 }),
        )?;
        let text = mcp_result_text(&value);
        if text.contains(needle) {
            return Ok(text);
        }
        last_text = text;
        request_id += 1;
        thread::sleep(Duration::from_millis(200));
    }
    bail!("timed out waiting for MCP list_tasks to contain {needle:?}; last tasks:\n{last_text}")
}

/// Wave 9 §5.14 item 1 — automation MCP discovery.
///
/// After spawning `libra code`, `control.json` (the file the CLI
/// writes when `--control-info-file` is set) must contain the
/// MCP server's URL so an automation client can find it without
/// log scraping. The harness now parses `mcpUrl` from the
/// runtime-emitted JSON; this test pins that:
///   * The field is populated for a normal spawn (the runtime
///     starts the MCP server alongside the web server).
///   * The URL is a loopback `http://127.0.0.1:<port>/mcp`-style
///     string (the harness already pins `host=127.0.0.1` and the
///     code runtime appends `/mcp` to the bind address).
///   * The MCP port is distinct from the web port — `code.rs`
///     enforces "Web and MCP ports must differ" outside `--stdio`
///     mode, so a regression that collapses them would silently
///     break automation.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn libra_code_writes_mcp_url_into_control_info_file() -> Result<()> {
    let session = CodeSession::spawn(CodeSessionOptions::new(
        "code-mcp-control-info",
        fixture_path(),
    ))?;
    let mcp_url = session
        .mcp_url()
        .ok_or_else(|| {
            anyhow::anyhow!("control.json did not surface mcpUrl after libra code spawn")
        })?
        .to_string();

    assert!(
        mcp_url.starts_with("http://127.0.0.1:"),
        "mcpUrl must point at the loopback bind; got {mcp_url:?}",
    );

    // Extract the port segment from `http://127.0.0.1:<port>/...`.
    let after_scheme = mcp_url
        .strip_prefix("http://127.0.0.1:")
        .expect("checked by the assert above");
    let mcp_port_str: String = after_scheme
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let mcp_port: u16 = mcp_port_str
        .parse()
        .with_context(|| format!("could not parse MCP port from {mcp_url:?}"))?;
    let base_url = session.matrix_attach_url();
    let web_port: u16 = base_url
        .strip_prefix("http://127.0.0.1:")
        .and_then(|tail| tail.split('/').next())
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("could not parse web port from base url {base_url}"))?;
    assert_ne!(
        mcp_port, web_port,
        "Web and MCP ports must differ outside --stdio mode (code.rs:3354); both were {mcp_port}",
    );
    Ok(())
}

/// Wave 9 §5.14 item 2 — `--stdio` + `--web-only` mutual
/// exclusion.
///
/// `code.rs:439` declares `pub web_only: bool` with
/// `conflicts_with = "stdio"`. This test pins clap surfaces that
/// conflict as a usage error before the runtime starts, so a
/// future refactor that drops the `conflicts_with` attribute
/// silently breaks the documented mutex.
///
/// Driven via `Command` (no PTY) because the conflict is
/// resolved during arg parsing — neither mode actually starts.
#[cfg(feature = "test-provider")]
#[test]
fn libra_code_stdio_web_only_combo_is_rejected_at_arg_parse() -> Result<()> {
    let output = Command::new(libra_bin_path())
        .args(["code", "--stdio", "--web-only"])
        .output()
        .context("failed to spawn libra code --stdio --web-only")?;
    if output.status.success() {
        bail!(
            "expected --stdio + --web-only to fail at arg parse, but exit was successful;\nstdout: {}\nstderr: {}",
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
        combined.contains("--stdio") && combined.contains("--web-only"),
        "clap conflict error must reference both flags; got:\n{combined}",
    );
    // clap's conflict-resolution error commonly includes the
    // phrase "cannot be used with" or "the argument ... cannot be
    // used with"; assert the keyword "cannot" so any future clap
    // wording change still passes as long as the conflict is
    // reported.
    assert!(
        combined.contains("cannot") || combined.contains("conflicts"),
        "expected a conflict-style error mentioning the mutex; got:\n{combined}",
    );
    Ok(())
}

/// Wave 9 §5.14 item 3 smoke — dual-reachability. After spawn,
/// the same `libra code` process must respond on BOTH:
///   * the web HTTP transport (proven via the existing
///     `session.snapshot()` GET `/api/code/session`), AND
///   * the MCP Streamable HTTP transport (proven via a fresh
///     reqwest POST to `<mcpUrl>` with a JSON-RPC `initialize`
///     payload).
///
/// The MCP transport is gated on the `Mcp-Session-Id` header
/// pattern from `tests/e2e_mcp_flow.rs`: initialize must succeed
/// (status `200 OK` + the response carries an `Mcp-Session-Id`
/// response header). This does NOT walk the full handshake
/// (notifications/initialized + tools/list) — that's covered by
/// `e2e_mcp_flow.rs` already; this test's contribution is
/// proving both surfaces are reachable on the SAME process.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn libra_code_serves_both_web_and_mcp_transports_on_same_process() -> Result<()> {
    let session = CodeSession::spawn(CodeSessionOptions::new(
        "code-mcp-dual-reachability",
        fixture_path(),
    ))?;

    // 1. Web reachability — drive the existing snapshot accessor
    //    so the failure mode is identical to other tests that
    //    rely on web HTTP.
    let snapshot = session.snapshot().context("web /api/code/session probe")?;
    assert!(
        snapshot.get("sessionId").and_then(|v| v.as_str()).is_some(),
        "web /api/code/session must surface a sessionId after spawn; got {snapshot:?}",
    );

    // 2. MCP reachability — POST a JSON-RPC initialize to the
    //    Streamable HTTP transport on the same process. The
    //    Mcp-Session-Id response header is the success contract
    //    (per `tests/e2e_mcp_flow.rs:291` "Server did not return
    //    Mcp-Session-Id header on initialize").
    let mcp_url = session
        .mcp_url()
        .ok_or_else(|| anyhow::anyhow!("control.json did not surface mcpUrl after spawn"))?
        .to_string();
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("build mcp probe client")?;
    let init_payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "libra-dual-reach-probe", "version": "0.0.0" }
        }
    });
    let response = client
        .post(&mcp_url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream, application/json")
        .json(&init_payload)
        .send()
        .with_context(|| format!("MCP initialize POST to {mcp_url} failed"))?;
    let status = response.status();
    let session_id = response
        .headers()
        .get("Mcp-Session-Id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        bail!("MCP initialize returned non-success status {status}: {body}");
    }
    assert!(
        session_id.is_some_and(|id| !id.is_empty()),
        "MCP initialize must return a non-empty Mcp-Session-Id header so a downstream automation client can continue the handshake",
    );
    Ok(())
}

/// Wave 9 §5.14 item 3 consistency — web write → web SSE +
/// MCP observe.
///
/// The same `libra code` process exposes web `/messages`, web
/// `/events`, and MCP Streamable HTTP. This test drives all three:
///
///   1. initialize an MCP client against the runtime's `mcpUrl`;
///   2. subscribe to web SSE before writing;
///   3. submit a message through the web automation endpoint;
///   4. assert the SSE stream observes that transcript update;
///   5. poll MCP `tools/call list_tasks` until it sees the TUI
///      turn-tracking Task created from the same user text.
///
/// This pins the currently implemented consistency direction.
/// MCP-originated tool writes are still not broadcast into Code UI
/// transcript state, so that opposite direction remains explicit
/// roadmap work rather than an overclaimed test assertion.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn web_message_turn_is_observable_through_sse_and_mcp_task_list() -> Result<()> {
    let mut session = CodeSession::spawn(CodeSessionOptions::new(
        "code-mcp-web-message-consistency",
        fixture_path(),
    ))?;
    let mcp_url = session
        .mcp_url()
        .ok_or_else(|| anyhow::anyhow!("control.json did not surface mcpUrl after spawn"))?
        .to_string();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("build MCP consistency client")?;
    let mcp_session_id = mcp_initialize(&client, &mcp_url)?;

    let mut events = session.open_event_stream()?;
    session.attach_automation("code-mcp-web-message-consistency")?;

    let marker = "mcp-dual-web-observe-marker";
    let user_text = format!("/chat {marker}");
    session.submit_message(&user_text)?;
    let _payload = wait_for_sse_transcript(&mut events, marker, Duration::from_secs(10))?;
    let tasks_text = wait_for_mcp_task(
        &client,
        &mcp_url,
        &mcp_session_id,
        marker,
        Duration::from_secs(10),
    )?;
    assert!(
        tasks_text.contains(&format!("TUI: {user_text}")) || tasks_text.contains(marker),
        "MCP list_tasks must expose the web-submitted turn text; got:\n{tasks_text}",
    );
    Ok(())
}

/// Wave 9 §5.14 item 3 consistency — MCP write → web SSE observe.
///
/// External MCP clients write workflow objects through the same
/// `LibraMcpServer` instance that the TUI bridge uses. This test
/// pins the reverse direction from the web→MCP case above:
///
///   1. initialize an MCP client against the runtime's `mcpUrl`;
///   2. subscribe to web SSE before writing;
///   3. call MCP `tools/call create_task`;
///   4. assert the web SSE `session_updated` snapshot contains a
///      transcript entry for the MCP-created task.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn mcp_created_task_is_observable_through_web_sse() -> Result<()> {
    let session = CodeSession::spawn(CodeSessionOptions::new(
        "code-mcp-write-web-sse",
        fixture_path(),
    ))?;
    let mcp_url = session
        .mcp_url()
        .ok_or_else(|| anyhow::anyhow!("control.json did not surface mcpUrl after spawn"))?
        .to_string();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("build MCP consistency client")?;
    let mcp_session_id = mcp_initialize(&client, &mcp_url)?;
    let mut events = session.open_event_stream()?;

    let marker = "mcp-originated-create-task-marker";
    let title = format!("MCP task {marker}");
    let value = mcp_call_tool(
        &client,
        &mcp_url,
        &mcp_session_id,
        100,
        "create_task",
        json!({
            "title": title,
            "description": "Created by an external MCP client for dual-entry consistency.",
            "status": "created",
        }),
    )?;
    let result_text = mcp_result_text(&value);
    assert!(
        result_text.contains("Task created with ID:"),
        "MCP create_task should return the created task id; got:\n{result_text}",
    );

    let _payload = wait_for_sse_transcript(&mut events, marker, Duration::from_secs(10))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// C6 — MCP stdio transport regression coverage.
//
// The tests above drive only the MCP Streamable-HTTP transport (plus the
// clap-level `--stdio`/`--web-only` mutex). C6's acceptance criterion
// (`plan.md:1346`) requires the MCP HTTP/stdio *dual entry* to have regression
// coverage for the shared **tool set**, **error behavior**, and **shutdown**
// behavior. The helpers below drive the real `libra code --stdio` MCP server
// over its newline-delimited JSON-RPC transport so those three facets are
// pinned for the stdio side, and the parity test proves the stdio tool set is
// identical to the HTTP one (both entries share `init_mcp_server` →
// `build_tool_router`, so a divergence is a regression).
// ---------------------------------------------------------------------------

/// Collected result of one `libra code --stdio` MCP session driven to stdin EOF.
#[cfg(feature = "test-provider")]
struct StdioMcpRun {
    exited_success: bool,
    values: Vec<Value>,
    stderr: String,
}

#[cfg(feature = "test-provider")]
impl StdioMcpRun {
    /// The JSON-RPC response value carrying `id == want`, if any.
    fn response_with_id(&self, want: u64) -> Option<&Value> {
        self.values
            .iter()
            .find(|value| value.get("id") == Some(&Value::from(want)))
    }
}

/// Create an isolated, freshly-initialized Libra repo for a stdio MCP probe.
/// Returns the `TempDir` (kept alive by the caller) — the MCP server only needs
/// a repo root under which it can open `.libra/libra.db`; the stdio tool
/// surface is repo-independent.
#[cfg(feature = "test-provider")]
fn init_stdio_repo() -> Result<tempfile::TempDir> {
    let temp = tempfile::Builder::new()
        .prefix("libra-code-stdio-mcp-")
        .tempdir()
        .context("failed to create stdio MCP repo tempdir")?;
    let output = Command::new(libra_bin_path())
        .args(["init", "--vault=false", "--quiet"])
        .arg(temp.path())
        .output()
        .context("failed to run libra init for stdio MCP repo")?;
    if !output.status.success() {
        bail!(
            "libra init failed for stdio MCP repo\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    Ok(temp)
}

/// Drive a full `libra code --stdio` MCP session: write every `request_line` to
/// the server's stdin (newline-delimited JSON-RPC), then close stdin so the
/// server observes EOF and shuts down. Collects the JSON-RPC responses printed
/// on stdout and the process exit status.
///
/// A watchdog fails the call (rather than hanging the test) if the server does
/// not close stdout within 30s — which is exactly the shutdown-on-EOF
/// regression this coverage exists to catch.
#[cfg(feature = "test-provider")]
fn run_stdio_mcp_session(repo_dir: &Path, request_lines: &[String]) -> Result<StdioMcpRun> {
    let repo_str = repo_dir
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("stdio MCP repo path is not valid UTF-8"))?;
    let mut child = Command::new(libra_bin_path())
        .args(["code", "--stdio", "--cwd", repo_str])
        .current_dir(repo_dir)
        .env("LIBRA_ENABLE_TEST_PROVIDER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn libra code --stdio")?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("libra code --stdio child had no stdin handle"))?;
    for line in request_lines {
        stdin
            .write_all(line.as_bytes())
            .context("failed to write JSON-RPC request to stdio MCP server")?;
        stdin
            .write_all(b"\n")
            .context("failed to write newline to stdio MCP server")?;
    }
    stdin.flush().context("failed to flush stdio MCP stdin")?;
    // Closing stdin is the shutdown signal: the rmcp transport hits EOF and the
    // server exits. Dropping the handle here is what the shutdown assertion
    // relies on.
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("libra code --stdio child had no stdout handle"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("libra code --stdio child had no stderr handle"))?;

    let (stdout_tx, stdout_rx) = mpsc::channel();
    let stdout_reader = thread::spawn(move || {
        let mut lines = Vec::new();
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => lines.push(line),
                Err(_) => break,
            }
        }
        let _ = stdout_tx.send(lines);
    });
    let stderr_reader = thread::spawn(move || {
        let mut buf = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut buf);
        buf
    });

    let stdout_lines = match stdout_rx.recv_timeout(Duration::from_secs(30)) {
        Ok(lines) => lines,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let stderr = stderr_reader.join().unwrap_or_default();
            bail!(
                "libra code --stdio did not close stdout within 30s after stdin EOF \
                 (possible MCP-stdio shutdown regression)\nstderr:\n{stderr}"
            );
        }
    };
    let _ = stdout_reader.join();

    // stdout reached EOF, which means the child *should* be exiting — but a
    // regression could close stdout while leaving the process alive, so bound
    // the exit wait too (codex C6 review): poll try_wait to a deadline, then
    // kill+fail rather than hang indefinitely on child.wait().
    let exit_deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait().context("try_wait on libra code --stdio")? {
            Some(status) => break status,
            None if Instant::now() >= exit_deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let stderr = stderr_reader.join().unwrap_or_default();
                bail!(
                    "libra code --stdio closed stdout but did not exit within 10s \
                     after stdin EOF (possible MCP-stdio shutdown regression: the \
                     process is stuck alive)\nstderr:\n{stderr}"
                );
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    };
    let stderr = stderr_reader.join().unwrap_or_default();

    let values = stdout_lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();

    Ok(StdioMcpRun {
        exited_success: status.success(),
        values,
        stderr,
    })
}

/// The three MCP handshake lines every stdio probe needs before it can call a
/// tool-router method: initialize (id 1), the initialized notification, then
/// the caller's request lines.
#[cfg(feature = "test-provider")]
fn stdio_handshake_lines() -> Vec<String> {
    vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "libra-code-stdio-dual-entry", "version": "0.0.0" }
            }
        })
        .to_string(),
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        })
        .to_string(),
    ]
}

/// Extract the sorted set of MCP tool names from a `tools/list` JSON-RPC result.
#[cfg(feature = "test-provider")]
fn tool_names_from_list_result(value: &Value) -> BTreeSet<String> {
    value
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Call `tools/list` over the MCP Streamable-HTTP transport and return the
/// sorted tool-name set.
#[cfg(feature = "test-provider")]
fn http_tool_names(
    client: &reqwest::blocking::Client,
    mcp_url: &str,
    session_id: &str,
) -> Result<BTreeSet<String>> {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    let (status, body) = mcp_post(client, mcp_url, Some(session_id), &request)
        .context("MCP HTTP tools/list failed")?;
    if !status.is_success() {
        bail!("MCP HTTP tools/list returned non-success status {status}: {body}");
    }
    let value = first_json_rpc_sse_body("tools/list", &body)?;
    let names = tool_names_from_list_result(&value);
    if names.is_empty() {
        bail!("MCP HTTP tools/list returned no tools: {value}");
    }
    Ok(names)
}

/// C6 §5.14 / `plan.md:1346` — the `libra code --stdio` MCP entry must (a)
/// expose the shared tool surface via `tools/list`, (b) surface a JSON-RPC
/// error for an unknown method AND an error-shaped result for an unknown tool,
/// and (c) shut down cleanly when its stdin is closed (EOF). All three facets
/// are driven over the real stdio transport in one session.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn libra_code_stdio_serves_tool_surface_reports_errors_and_shuts_down() -> Result<()> {
    let repo = init_stdio_repo()?;

    let mut lines = stdio_handshake_lines();
    // id 2 — tool set.
    lines.push(
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        })
        .to_string(),
    );
    // id 3 — unknown method → canonical JSON-RPC "method not found" error.
    lines.push(
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "libra/definitely-not-a-method",
            "params": {}
        })
        .to_string(),
    );
    // id 4 — unknown tool → error-shaped tools/call response.
    lines.push(
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "definitely_not_a_tool", "arguments": {} }
        })
        .to_string(),
    );

    let run = run_stdio_mcp_session(repo.path(), &lines)?;

    // (c) Shutdown: closing stdin (EOF) must terminate the server cleanly.
    assert!(
        run.exited_success,
        "libra code --stdio must exit 0 after stdin EOF; stderr:\n{}",
        run.stderr,
    );

    // (a) Tool set: tools/list must expose the shared MCP tool surface.
    let tools_list = run
        .response_with_id(2)
        .ok_or_else(|| anyhow::anyhow!("stdio MCP produced no tools/list response (id 2)"))?;
    let names = tool_names_from_list_result(tools_list);
    for expected in [
        "run_libra_vcs",
        "create_task",
        "list_tasks",
        "create_intent",
    ] {
        assert!(
            names.contains(expected),
            "stdio MCP tools/list must expose `{expected}`; got: {names:?}",
        );
    }
    assert!(
        names.len() >= 10,
        "stdio MCP tools/list must expose the full workflow tool surface (>=10 tools); got {} : {names:?}",
        names.len(),
    );

    // (b) Error behavior — unknown method → top-level JSON-RPC error with the
    // exact "method not found" code (codex C6 review: pin the code, not just
    // error.is_some(), so a wrong-code regression is caught).
    let unknown_method = run.response_with_id(3).ok_or_else(|| {
        anyhow::anyhow!("stdio MCP produced no response for the unknown method (id 3)")
    })?;
    assert_eq!(
        unknown_method
            .pointer("/error/code")
            .and_then(Value::as_i64),
        Some(-32601),
        "unknown method must map to JSON-RPC -32601 (method not found) over stdio; got: {unknown_method}",
    );

    // (b) Error behavior — unknown tool → top-level JSON-RPC -32602
    // (invalid params) per rmcp 1.5.0, not a result flagged isError.
    let unknown_tool = run.response_with_id(4).ok_or_else(|| {
        anyhow::anyhow!("stdio MCP produced no response for the unknown tool (id 4)")
    })?;
    assert_eq!(
        unknown_tool.pointer("/error/code").and_then(Value::as_i64),
        Some(-32602),
        "calling an unknown tool over stdio must map to JSON-RPC -32602 (invalid params); got: {unknown_tool}",
    );

    Ok(())
}

/// C6 §5.14 / `plan.md:1346` — dual-entry tool-set parity. The HTTP and stdio
/// MCP entries are the same server (`init_mcp_server` → `build_tool_router`),
/// so `tools/list` over each transport must return an identical tool set. A
/// divergence would mean one entry point drifted from the other — exactly the
/// dual-entry regression this test guards.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn mcp_http_and_stdio_expose_identical_tool_set() -> Result<()> {
    // HTTP side — reuse the harness-spawned `libra code` MCP HTTP transport.
    let session = CodeSession::spawn(CodeSessionOptions::new(
        "code-mcp-tool-set-parity",
        fixture_path(),
    ))?;
    let mcp_url = session
        .mcp_url()
        .ok_or_else(|| anyhow::anyhow!("control.json did not surface mcpUrl after spawn"))?
        .to_string();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("build MCP tool-set parity client")?;
    let http_session_id = mcp_initialize(&client, &mcp_url)?;
    let http_names = http_tool_names(&client, &mcp_url, &http_session_id)?;

    // stdio side — an independent, isolated repo (the tool surface is
    // repo-independent, so a separate repo avoids DB contention with the HTTP
    // session while still exercising the real stdio transport).
    let repo = init_stdio_repo()?;
    let mut lines = stdio_handshake_lines();
    lines.push(
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        })
        .to_string(),
    );
    let run = run_stdio_mcp_session(repo.path(), &lines)?;
    assert!(
        run.exited_success,
        "stdio MCP parity probe must exit cleanly; stderr:\n{}",
        run.stderr,
    );
    let tools_list = run
        .response_with_id(2)
        .ok_or_else(|| anyhow::anyhow!("stdio MCP parity probe produced no tools/list response"))?;
    let stdio_names = tool_names_from_list_result(tools_list);

    assert!(
        !stdio_names.is_empty(),
        "stdio MCP tools/list returned no tools",
    );
    assert_eq!(
        http_names,
        stdio_names,
        "MCP HTTP and stdio entries must expose an identical tool set\n  http-only:  {:?}\n  stdio-only: {:?}",
        http_names.difference(&stdio_names).collect::<Vec<_>>(),
        stdio_names.difference(&http_names).collect::<Vec<_>>(),
    );

    Ok(())
}

#[cfg(not(feature = "test-provider"))]
#[test]
fn mcp_dual_entry_test_requires_test_provider_feature() {
    eprintln!("skipping mcp dual entry test; enable --features test-provider");
}
