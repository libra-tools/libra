//! End-to-end MCP (Model Context Protocol) flow tests over SSE/HTTP streaming.
//!
//! Spawns the real `libra code` binary (default Web Code UI; W5-07 removed the
//! deprecated `--web-only` alias) on dynamically allocated MCP/Web
//! ports, walks through the full Streamable HTTP transport handshake (initialize →
//! initialized notification → tools/call), and verifies a created task is visible
//! both via `list_tasks` and on disk under `.libra/objects`. This is the canonical
//! smoke test for the MCP server: TUI-side details may change, but the wire protocol
//! must keep round-tripping.
//!
//! **Layer:** L1 — uses local HTTP server on dynamically allocated ports. Builds
//! the binary on demand inside the test so the harness picks up local edits.

use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use serde_json::json;
use tokio::time::sleep;

/// Stream a child's stdout into a shared line buffer (TA-04: the server is
/// launched with `--port 0 --mcp-port 0`, so the kernel picks free ports —
/// no fixed range to probe, no collision window under parallel test
/// scheduling; the ACTUAL endpoints are read back from the startup banner,
/// which is part of the printed UX contract).
/// SIGKILL the child on any early return/panic so failed assertions can
/// never leak a running `libra code` server.
struct KillChildOnDrop(Option<std::process::Child>);
impl Drop for KillChildOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_stdout_reader(
    child: &mut std::process::Child,
) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    use std::io::BufRead;
    let stdout = child.stdout.take().expect("child stdout piped");
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = std::sync::Arc::clone(&buf);
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            println!("[libra code] {line}");
            sink.lock().expect("stdout buffer poisoned").push(line);
        }
    });
    buf
}

/// Wait for both startup-banner URLs:
/// `Libra Code server running at http://…` (query stripped) and
/// `MCP: http://…`. Returns `(mcp_url, web_url)`.
async fn wait_for_banner_urls(
    lines: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    child: &mut std::process::Child,
    timeout: Duration,
) -> (String, String) {
    let deadline = Instant::now() + timeout;
    loop {
        let (mut web, mut mcp) = (None, None);
        for line in lines.lock().expect("stdout buffer poisoned").iter() {
            if let Some(rest) = line.trim().strip_prefix("Libra Code server running at ") {
                let url = rest.split(['?', ' ']).next().unwrap_or(rest);
                web = Some(url.trim_end_matches('/').to_string());
            }
            if let Some(rest) = line.trim().strip_prefix("MCP: ") {
                mcp = Some(rest.trim().trim_end_matches('/').to_string());
            }
        }
        if let (Some(web), Some(mcp)) = (web, mcp) {
            return (mcp, web);
        }
        if let Some(status) = child.try_wait().expect("poll libra code") {
            let seen = lines.lock().expect("stdout buffer poisoned").join("\n");
            panic!(
                "libra code exited before printing its endpoints: {status}\nstdout so far:\n{seen}"
            );
        }
        if Instant::now() >= deadline {
            let seen = lines.lock().expect("stdout buffer poisoned").join("\n");
            panic!(
                "libra code did not print its endpoint banner within {timeout:?}\nstdout so far:\n{seen}"
            );
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// Extract all `data:` values from an SSE event stream body.
///
/// Both `data:` (no space) and `data: ` (with space) prefixes are recognised because
/// MCP servers in the wild emit either. Empty `data:` lines (heartbeats) are
/// dropped. Whitespace around the value is trimmed so callers can `serde_json`
/// parse directly.
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

/// POST a JSON-RPC message to the MCP server using the Streamable HTTP transport.
///
/// Returns `(status, sse_body)`. On requests (with an `id`), the response is an SSE
/// stream (`text/event-stream`); on notifications (no `id`), expect `202 Accepted`.
///
/// Honours `Mcp-Session-Id` when supplied (every request after `initialize` carries
/// it). Panics on transport-level failures so the test surfaces them immediately
/// rather than masking them as silent assertion failures further down.
async fn mcp_post(
    client: &reqwest::Client,
    url: &str,
    session_id: Option<&str>,
    body: &serde_json::Value,
) -> (reqwest::StatusCode, String) {
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream, application/json");

    if let Some(sid) = session_id {
        req = req.header("Mcp-Session-Id", sid);
    }

    let res = req
        .json(body)
        .send()
        .await
        .unwrap_or_else(|e| panic!("MCP POST failed: {e}"));

    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    (status, text)
}

/// Scenario: full end-to-end MCP flow over the Streamable HTTP transport.
///
/// 1. Build the `libra` binary (so the test runs against current code).
/// 2. Initialize a temp-dir repo with isolated HOME/XDG_CONFIG_HOME.
/// 3. Start `libra code` (default Web Code UI) on dynamically allocated ports.
/// 4. Wait up to 30 seconds for the MCP TCP listener to accept connections.
/// 5. Initialize handshake → notifications/initialized → tools/call create_task →
///    resources/list → tools/call list_tasks.
/// 6. Verify `.libra/objects` exists and `refs/libra/intent` does NOT (the AI
///    history ref now lives in SQLite, not on disk).
///
/// Boundary conditions guarded:
/// - Server startup race: poll loop with timeout and explicit child-process
///   stdout/stderr capture so a startup failure surfaces useful diagnostics.
/// - Initialize transport flakiness: retry up to 60 times at 250 ms intervals so a
///   slow first request does not flake the whole test.
/// - Session ID redaction: the printed log redacts the actual session id length only
///   so credential-grade strings never end up in CI logs.
///
/// Acts as the canonical regression guard for the MCP wire protocol.
#[tokio::test]
async fn test_e2e_mcp_flow() {
    // ── 1. Setup ───────────────────────────────────────────────────────────────
    let temp_dir = tempfile::tempdir().unwrap();
    let repo_path = temp_dir.path();
    let home_dir = repo_path.join(".home");
    let config_home = home_dir.join(".config");
    std::fs::create_dir_all(&config_home).expect("failed to create isolated HOME");

    println!("Test Repo Path: {:?}", repo_path);

    // Build binary first to ensure it's fresh
    let status = Command::new("cargo")
        .args(["build", "--bin", "libra"])
        .status()
        .expect("Failed to build libra");
    assert!(status.success(), "cargo build failed");

    let project_root = std::env::current_dir().expect("Failed to get current dir");
    // Honor CARGO_TARGET_DIR like the sigterm case below — a hardcoded
    // target/ path ENOENTs under an isolated target dir (PS-02 terra R1).
    let libra_bin = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| project_root.join("target"))
        .join("debug/libra");

    // Init repo
    let status = Command::new(&libra_bin)
        .args(["init"])
        .current_dir(repo_path)
        .env("HOME", &home_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home_dir)
        .status()
        .expect("Failed to init repo");
    assert!(status.success(), "libra init failed");

    // ── 2. Start Server ────────────────────────────────────────────────────────
    // The default Web Code UI launch is headless, so the test can run without a
    // terminal. The MCP server is started by the current Web launch.
    let mut child = Command::new(&libra_bin)
        // PS-02 (ADR-PS-01): bare `libra code` no longer defaults to gemini.
        .args([
            "code",
            "--provider",
            "gemini",
            "--mcp-port",
            "0",
            "--port",
            "0",
        ])
        .current_dir(repo_path)
        .env("HOME", &home_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home_dir)
        .env("GEMINI_API_KEY", "test-gemini-api-key")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start libra code");

    // The MCP URL is read back from the STDOUT BANNER; the `MCP:` line is
    // printed after the listener binds, so seeing it means the server is up.
    let stdout_lines = spawn_stdout_reader(&mut child);
    // SIGKILL on any early return/panic so a banner timeout cannot leak the
    // server process (same guard pattern as the SIGTERM case).
    let mut child_guard = KillChildOnDrop(Some(child));
    let (mcp_url, _web_url) = wait_for_banner_urls(
        &stdout_lines,
        child_guard.0.as_mut().expect("child present"),
        Duration::from_secs(30),
    )
    .await;
    println!("MCP server is ready at {mcp_url}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .no_proxy()
        .build()
        .unwrap();

    // ── 3. MCP Handshake (Streamable HTTP transport) ───────────────────────────
    //
    // Protocol summary:
    //   1. POST Initialize (no Mcp-Session-Id) → SSE stream with result + session id header.
    //   2. POST initialized notification (with Mcp-Session-Id) → 202 Accepted.
    //   3. POST tools/call or resources/list (with Mcp-Session-Id) → SSE stream.
    //
    // See: https://spec.modelcontextprotocol.io/specification/2025-03-26/basic/transports/#streamable-http

    // Step 1: Initialize — no session id yet
    let init_msg = json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "e2e-test-client", "version": "1.0" }
        },
        "id": 1
    });

    println!("Sending Initialize...");
    let mut response_opt = None;
    let mut last_init_error = None;
    for _ in 0..60 {
        match client
            .post(&mcp_url)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream, application/json")
            .json(&init_msg)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                response_opt = Some(response);
                break;
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                last_init_error = Some(format!("status {status}, body: {body}"));
            }
            Err(e) => {
                last_init_error = Some(e.to_string());
            }
        }
        sleep(Duration::from_millis(250)).await;
    }

    let response = response_opt.unwrap_or_else(|| {
        panic!(
            "Initialize failed after retries: {}",
            last_init_error.unwrap_or_else(|| "unknown".to_string())
        )
    });

    // Extract Mcp-Session-Id from response headers
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .expect("Server did not return Mcp-Session-Id header on initialize")
        .to_str()
        .unwrap()
        .to_string();
    println!("Session ID: <redacted, len={}>", session_id.len());

    // Parse SSE body
    let init_sse = response.text().await.unwrap();
    println!("Initialize SSE response:\n{init_sse}");
    let init_data = parse_sse_data(&init_sse);
    assert!(
        !init_data.is_empty(),
        "No SSE data lines in initialize response"
    );

    let init_result: serde_json::Value =
        serde_json::from_str(&init_data[0]).expect("Failed to parse initialize JSON-RPC result");
    assert_eq!(init_result["id"], 1, "Initialize response id mismatch");
    assert!(
        init_result.get("result").is_some(),
        "Initialize response missing 'result'"
    );
    println!(
        "Server info: {}",
        serde_json::to_string_pretty(&init_result["result"]["serverInfo"]).unwrap()
    );

    // Step 2: Send initialized notification (no id → it is a notification)
    let initialized_msg = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    println!("Sending initialized notification...");
    let (status, _body) = mcp_post(&client, &mcp_url, Some(&session_id), &initialized_msg).await;
    assert!(
        status.is_success(),
        "initialized notification failed: {status}"
    );
    println!("Initialized OK (status {status})");

    // ── 4. Call Tool: create_task ──────────────────────────────────────────────
    let create_task_msg = json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": "create_task",
            "arguments": {
                "title": "E2E Test Task",
                "description": "Created via E2E test"
            }
        },
        "id": 2
    });

    println!("Calling create_task...");
    let (status, task_sse) = mcp_post(&client, &mcp_url, Some(&session_id), &create_task_msg).await;
    assert!(status.is_success(), "create_task failed: {status}");
    println!("create_task SSE:\n{task_sse}");

    let task_data = parse_sse_data(&task_sse);
    assert!(!task_data.is_empty(), "No SSE data in create_task response");

    let task_result: serde_json::Value =
        serde_json::from_str(&task_data[0]).expect("Failed to parse create_task JSON-RPC result");
    assert_eq!(task_result["id"], 2);
    let content = &task_result["result"]["content"];
    assert!(
        content.is_array(),
        "create_task result.content must be an array"
    );
    let text = content[0]["text"]
        .as_str()
        .expect("create_task result content[0].text missing");
    assert!(
        text.contains("Task created with ID"),
        "Unexpected create_task result: {text}"
    );
    println!("create_task OK: {text}");

    // ── 5. List Resources ─────────────────────────────────────────────────────
    let list_resources_msg = json!({
        "jsonrpc": "2.0",
        "method": "resources/list",
        "params": {},
        "id": 3
    });

    println!("Calling resources/list...");
    let (status, res_sse) =
        mcp_post(&client, &mcp_url, Some(&session_id), &list_resources_msg).await;
    assert!(status.is_success(), "resources/list failed: {status}");
    println!("resources/list SSE:\n{res_sse}");

    let res_data = parse_sse_data(&res_sse);
    assert!(
        !res_data.is_empty(),
        "No SSE data in resources/list response"
    );

    let resources_result: serde_json::Value =
        serde_json::from_str(&res_data[0]).expect("Failed to parse resources/list JSON-RPC result");
    assert_eq!(resources_result["id"], 3);
    let resources = &resources_result["result"]["resources"];
    assert!(
        resources.is_array(),
        "resources/list result.resources must be an array"
    );
    println!(
        "Resources ({} items): {}",
        resources.as_array().unwrap().len(),
        serde_json::to_string_pretty(resources).unwrap()
    );

    // ── 6. List Tasks — verify our task shows up ──────────────────────────────
    let list_tasks_msg = json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": "list_tasks",
            "arguments": {}
        },
        "id": 4
    });

    println!("Calling list_tasks...");
    let (status, tasks_sse) = mcp_post(&client, &mcp_url, Some(&session_id), &list_tasks_msg).await;
    assert!(status.is_success(), "list_tasks failed: {status}");
    println!("list_tasks SSE:\n{tasks_sse}");

    let tasks_data = parse_sse_data(&tasks_sse);
    assert!(!tasks_data.is_empty(), "No SSE data in list_tasks response");

    let tasks_result: serde_json::Value =
        serde_json::from_str(&tasks_data[0]).expect("Failed to parse list_tasks JSON-RPC result");
    assert_eq!(tasks_result["id"], 4);
    let task_content = &tasks_result["result"]["content"];
    assert!(
        task_content.is_array() && !task_content.as_array().unwrap().is_empty(),
        "list_tasks should return at least one task"
    );
    let tasks_text = task_content[0]["text"].as_str().unwrap_or("");
    assert!(
        tasks_text.contains("E2E Test Task"),
        "Created task not found in list_tasks output: {tasks_text}"
    );
    println!("list_tasks OK — task found");

    // ── 7. Verification on disk ───────────────────────────────────────────────
    let objects_dir = repo_path.join(".libra/objects");
    assert!(objects_dir.exists(), ".libra/objects should exist");

    let history_ref = repo_path.join(".libra/refs/libra/intent");
    assert!(
        !history_ref.exists(),
        "AI history ref should NOT be created on disk (it is in DB)"
    );

    // ── 8. Cleanup ────────────────────────────────────────────────────────────
    drop(child_guard);
    println!("E2E MCP flow test passed!");
}

/// W1-08: web-only mode must exit naturally on SIGTERM and release listeners
/// so the same ports can bind again without a forced SIGKILL.
#[cfg(unix)]
#[tokio::test]
async fn test_web_only_sigterm_releases_ports() {
    use std::time::Instant;

    let temp_dir = tempfile::tempdir().unwrap();
    let repo_path = temp_dir.path();
    let home_dir = repo_path.join(".home");
    let config_home = home_dir.join(".config");
    std::fs::create_dir_all(&config_home).expect("failed to create isolated HOME");

    let status = Command::new("cargo")
        .args(["build", "--bin", "libra"])
        .env("LIBRA_SKIP_WEB_BUILD", "1")
        .status()
        .expect("Failed to build libra");
    assert!(status.success(), "cargo build failed");

    let project_root = std::env::current_dir().expect("Failed to get current dir");
    let libra_bin = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| project_root.join("target"))
        .join("debug/libra");

    let status = Command::new(&libra_bin)
        .args(["init"])
        .current_dir(repo_path)
        .env("HOME", &home_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home_dir)
        .status()
        .expect("Failed to init repo");
    assert!(status.success(), "libra init failed");

    let child = Command::new(&libra_bin)
        // PS-02 (ADR-PS-01): bare `libra code` no longer defaults to gemini.
        .args([
            "code",
            "--provider",
            "gemini",
            "--mcp-port",
            "0",
            "--port",
            "0",
        ])
        .current_dir(repo_path)
        .env("HOME", &home_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home_dir)
        .env("GEMINI_API_KEY", "test-gemini-api-key")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start libra code");
    // SIGKILL+wait on any early return/panic so failed assertions cannot leak
    // a running Web process into later tests.
    let mut child = child;
    let stdout_lines = spawn_stdout_reader(&mut child);
    let mut child_guard = KillChildOnDrop(Some(child));

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let (mcp_url, web_base) = wait_for_banner_urls(
        &stdout_lines,
        child_guard.0.as_mut().expect("child present"),
        Duration::from_secs(30),
    )
    .await;
    let web_url = format!("{web_base}/");
    let ready_deadline = Instant::now() + Duration::from_secs(45);
    let mut ready = false;
    while Instant::now() < ready_deadline {
        let child = child_guard.0.as_mut().expect("child present");
        if let Some(status) = child.try_wait().expect("poll child") {
            panic!("libra code exited before ready: {status}");
        }
        match client.get(&web_url).send().await {
            Ok(resp) if resp.status().is_success() || resp.status().as_u16() < 500 => {
                ready = true;
                break;
            }
            _ => sleep(Duration::from_millis(200)).await,
        }
    }
    assert!(
        ready,
        "web-only server did not become ready for SIGTERM test"
    );

    let pid = child_guard.0.as_ref().expect("child present").id();
    let kill_rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(kill_rc, 0, "failed to SIGTERM libra code pid {pid}");

    let exit_deadline = Instant::now() + Duration::from_secs(60);
    let mut exited = false;
    while Instant::now() < exit_deadline {
        let child = child_guard.0.as_mut().expect("child present");
        if child.try_wait().expect("poll after SIGTERM").is_some() {
            exited = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(
        exited,
        "libra code did not exit naturally after SIGTERM within 60s (would require SIGKILL)"
    );
    let status = child_guard
        .0
        .as_mut()
        .expect("child present")
        .wait()
        .expect("wait after natural exit");
    assert!(
        status.success(),
        "SIGTERM graceful shutdown should exit successfully, got {status}"
    );
    // Natural exit succeeded — disarm the SIGKILL fallback.
    let _ = child_guard.0.take();

    // TA-04: what the old rebind asserted was really "no orphaned child
    // keeps serving". Assert that at the TCP layer: a plain connect to the
    // old endpoints must FAIL (refused) — connecting claims nothing, so
    // there is no bind race with other processes reusing the OS-assigned
    // port, and a leaked listener that accepts but stalls HTTP still turns
    // this red.
    for url in [web_url.as_str(), mcp_url.as_str()] {
        let hostport = url.trim_start_matches("http://").trim_end_matches('/');
        match tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::TcpStream::connect(hostport),
        )
        .await
        {
            Ok(Err(_)) => {}
            Ok(Ok(_)) => panic!(
                "{hostport} still accepting connections after SIGTERM graceful shutdown — orphaned server?"
            ),
            Err(_) => panic!(
                "{hostport} neither refused nor accepted within 3s after SIGTERM — half-dead listener?"
            ),
        }
    }
}
