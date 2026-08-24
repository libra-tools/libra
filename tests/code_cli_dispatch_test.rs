//! Wave 2 / PR 2 — `libra code` CLI dispatch L1 tests.
//!
//! Per `docs/development/commands/_general.md` §5.1, Wave 2's CLI surface must
//! cover mode selection, mutual exclusion, and parser smoke without
//! ever spawning the binary. We assert directly against
//! `clap::Parser::try_parse_from` so a bad flag combination fails
//! at parse time, before the runtime starts.
//!
//! What this file covers (P0 set):
//!
//! * W5-07: `--web` / `--web-only` were removed — the binary rejects them
//!   with clap's unexpected-argument usage error plus a migration hint, and
//!   the removed hidden legacy-TUI rollback env no longer changes the
//!   default Web launch (spawns the real binary).
//! * `--mcp-port 0` is accepted (kernel-assigned port, used by the
//!   Web process harness).
//! * `--port 0` likewise.
//! * `--env-file` is parsed into the right field.
//! * `--repo`, `--cwd`, `--resume` pass through as `Some(...)`.
//! * `--browser-control loopback` together with `--stdio` is
//!   rejected (clap conflicts_with).
//!
//! What this file does NOT cover (deferred per the plan):
//!
//! * Provider boot smoke (Wave 10 / PR 10).
//! * `--plan-mode` default per provider — already covered by
//!   `effective_plan_mode_*` tests inside `src/command/code.rs`.
//! * Code UI / MCP / Codex runtime — Waves 9/13.

use std::path::PathBuf;

use clap::Parser;
use libra::command::code::{CodeArgs, CodeContext, CodeNetworkAccess, CodeProvider, ControlMode};

/// Helper: parse `argv0 + args` with a fixed binary name. clap expects the
/// binary name as `argv[0]`, so prepend one to the caller's spelling.
fn parse(args: &[&str]) -> Result<CodeArgs, clap::Error> {
    let mut full: Vec<String> = vec!["code".to_string()];
    for arg in args {
        full.push((*arg).to_string());
    }
    CodeArgs::try_parse_from(full)
}

#[test]
fn mcp_port_zero_is_accepted() {
    let parsed = parse(&["--mcp-port", "0"]).expect("--mcp-port 0 is the kernel-pick sentinel");
    assert_eq!(parsed.mcp_port, 0);
}

#[test]
fn web_port_zero_is_accepted() {
    let parsed = parse(&["--port", "0"]).expect("--port 0 is the kernel-pick sentinel");
    assert_eq!(parsed.port, 0);
}

#[test]
fn env_file_parses_into_pathbuf() {
    let parsed = parse(&["--env-file", "/tmp/.env.test"]).expect(".env paths are valid input");
    assert_eq!(parsed.env_file, Some(PathBuf::from("/tmp/.env.test")));
}

#[test]
fn repo_and_cwd_and_resume_are_optional() {
    let bare = parse(&[]).expect("CodeArgs has no required positional args");
    assert!(bare.repo.is_none());
    assert!(bare.cwd.is_none());
    assert!(bare.resume.is_none());

    let with_paths = parse(&[
        "--repo",
        "/tmp/some-repo",
        "--cwd",
        "/tmp/some-cwd",
        "--resume",
        "thread-2026-05-10-001",
    ])
    .expect("--repo / --cwd / --resume are optional but well-typed");
    assert_eq!(with_paths.repo, Some(PathBuf::from("/tmp/some-repo")));
    assert_eq!(with_paths.cwd, Some(PathBuf::from("/tmp/some-cwd")));
    assert_eq!(with_paths.resume.as_deref(), Some("thread-2026-05-10-001"));
}

#[test]
fn browser_control_loopback_conflicts_with_stdio() {
    // `--browser-control loopback` is incompatible with `--stdio`
    // because the stdio MCP server has no HTTP surface for a
    // browser to attach to. clap's conflicts_with should reject.
    let error = parse(&["--browser-control", "loopback", "--stdio"])
        .expect_err("--browser-control + --stdio must be rejected");
    let rendered = error.to_string();
    assert!(
        rendered.contains("--browser-control") && rendered.contains("--stdio"),
        "expected conflict error to mention both flags; got: {rendered}",
    );
}

#[test]
fn default_web_with_non_gemini_provider_parses() {
    // C2 (GAP-1): default-Web `--provider <non-gemini>` must parse cleanly at the
    // CLI layer; the previous web-only rejection lived in `validate_mode_args`,
    // not the parser, and is now relaxed (verified in code.rs unit tests).
    for provider in [
        "codex",
        "openai",
        "anthropic",
        "deepseek",
        "kimi",
        "zhipu",
        "ollama",
    ] {
        let parsed = parse(&["--provider", provider])
            .unwrap_or_else(|e| panic!("default Web --provider {provider} must parse: {e}"));
        assert_ne!(parsed.provider, CodeProvider::Gemini);
    }
}

#[test]
fn default_web_with_provider_tuning_flags_parse() {
    // C2 (GAP-3): the provider-tuning flags the headless runtime consumes must
    // reach `CodeArgs` on the default Web launch.
    let parsed = parse(&[
        "--provider",
        "ollama",
        "--model",
        "llama3",
        "--api-base",
        "http://127.0.0.1:11434/v1",
        "--temperature",
        "0.2",
        "--ollama-thinking",
        "high",
    ])
    .expect("default Web provider-tuning flags must parse");
    assert_eq!(parsed.provider, CodeProvider::Ollama);
    assert_eq!(parsed.model.as_deref(), Some("llama3"));
    assert_eq!(
        parsed.api_base.as_deref(),
        Some("http://127.0.0.1:11434/v1")
    );
    assert_eq!(parsed.temperature, Some(0.2));
    assert!(parsed.ollama_thinking.is_some());
}

#[test]
fn default_web_env_file_context_and_approval_flags_parse() {
    // W3-13: public TUI flags that feed headless bootstrap must parse on the
    // default Web launch (mode validation is covered in `code.rs` unit tests).
    let parsed = parse(&[
        "--env-file",
        "/tmp/.env.web-test",
        "--context",
        "dev",
        "--approval-policy",
        "on-request",
        "--approval-ttl",
        "42",
    ])
    .expect("default Web must parse env-file/context/approval flags");
    assert_eq!(
        parsed.env_file.as_deref(),
        Some(std::path::Path::new("/tmp/.env.web-test"))
    );
    assert_eq!(parsed.context, Some(CodeContext::Dev));
    assert_eq!(parsed.approval_ttl, Some(42));
}

#[test]
fn defaults_are_observe_control_and_deny_network() {
    let bare = parse(&[]).expect("CodeArgs has no required args");
    // Spot-check that the documented defaults from publish.md /
    // docs/commands/code.md actually flow through.
    // ControlMode::Observe is the safe default (no automation
    // writes); CodeNetworkAccess::Deny is the safe default for
    // shell tools.
    //
    // Codex pass-1 P3: assert via PartialEq on the enum directly
    // instead of `format!("{:?}")` substring matching, which
    // would pass on accidental Debug-impl substring overlap.
    assert_eq!(
        bare.control,
        ControlMode::Observe,
        "control default must be ControlMode::Observe",
    );
    assert_eq!(
        bare.network_access,
        CodeNetworkAccess::Deny,
        "network_access default must be CodeNetworkAccess::Deny",
    );
}

#[test]
fn control_stdio_mode_parses_url_and_token_file() {
    let parsed = parse(&[
        "--control",
        "stdio",
        "--control-url",
        "http://127.0.0.1:3000",
        "--control-token-file",
        "/tmp/control.token",
    ])
    .expect("--control stdio must parse with explicit URL/token");
    assert_eq!(parsed.control, ControlMode::Stdio);
    assert_eq!(parsed.control_url.as_deref(), Some("http://127.0.0.1:3000"));
    assert_eq!(
        parsed.control_token_file.as_deref(),
        Some(std::path::Path::new("/tmp/control.token"))
    );
    assert!(!parsed.stdio, "--control stdio must not set MCP --stdio");
}

#[test]
fn control_stdio_mode_keeps_observe_write_parseable() {
    assert_eq!(
        parse(&["--control", "observe"]).expect("observe").control,
        ControlMode::Observe
    );
    assert_eq!(
        parse(&["--control", "write"]).expect("write").control,
        ControlMode::Write
    );
}

/// W3-11: occupied listen port fail-closes with an actionable `--port` hint
/// and never auto-increments to another free port.
#[tokio::test]
async fn default_port_conflict_fails_fast() {
    use libra::internal::ai::web::{WebServerOptions, describe_web_bind_error, start};

    let holder = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve a local port");
    let port = holder.local_addr().expect("local addr").port();
    let temp = tempfile::tempdir().expect("tempdir");

    let start_result = start(
        "127.0.0.1",
        port,
        temp.path().to_path_buf(),
        WebServerOptions::default(),
    )
    .await;
    let Err(err) = start_result else {
        panic!("second bind of the same port must fail closed");
    };

    let message = describe_web_bind_error("127.0.0.1", port, &err);
    assert!(
        message.contains("--port"),
        "operator message must mention --port; got: {message}"
    );
    assert!(
        message.contains("does not auto-scan") || message.contains("already in use"),
        "operator message must state fail-fast / no auto-scan; got: {message}"
    );

    // Holding `holder` until here proves we did not silently bind another port.
    drop(holder);
}

/// W4-01: default `libra code` prints a Web URL, stays
/// resident without a TTY, and exits cleanly on SIGTERM (ports released).
#[cfg(unix)]
#[tokio::test]
async fn default_web_no_tty_and_sigterm_clean_shutdown() {
    use std::{
        io::{BufRead, BufReader, Read},
        net::TcpListener,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let repo_path = temp_dir.path();
    let home_dir = repo_path.join(".home");
    let config_home = home_dir.join(".config");
    std::fs::create_dir_all(&config_home).expect("isolated HOME");

    // Use the binary cargo already built for this test target — never nest
    // `cargo build` under `cargo test` (target-dir lock deadlock).
    let libra_bin = env!("CARGO_BIN_EXE_libra");

    let status = Command::new(libra_bin)
        .args(["init"])
        .current_dir(repo_path)
        .env("HOME", &home_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home_dir)
        .status()
        .expect("libra init");
    assert!(status.success(), "libra init failed");

    // Let the child bind ephemeral ports (`--port 0` / `--mcp-port 0`) and
    // discover the URL from stdout. Pre-bind+drop races with parallel tests.
    let child = Command::new(libra_bin)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(repo_path)
        .env("HOME", &home_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home_dir)
        .env("GEMINI_API_KEY", "test-gemini-api-key")
        .env("LIBRA_TEST", "1") // skip best-effort browser open
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start default libra code");

    struct KillChildOnDrop(Option<std::process::Child>);
    impl Drop for KillChildOnDrop {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let mut child_guard = KillChildOnDrop(Some(child));

    let stdout = child_guard
        .0
        .as_mut()
        .expect("child")
        .stdout
        .take()
        .expect("stdout pipe");
    // Drain stdout on a dedicated thread for the child's whole lifetime.
    // Stopping early and dropping the pipe can SIGPIPE the child on the next
    // println! (bootstrap token / MCP URL), which then fails the health probe.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut captured = String::new();
        let mut line = String::new();
        let mut notified = false;
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    captured.push_str(&line);
                    if !notified
                        && let Some(rest) =
                            line.trim().strip_prefix("Libra Code server running at ")
                    {
                        notified = true;
                        let _ = tx.send(rest.trim_end_matches('/').to_string());
                    }
                }
                Err(_) => break,
            }
        }
        let _ = done_tx.send(captured);
    });
    let printed_url = match rx.recv_timeout(Duration::from_secs(45)) {
        Ok(url) => url,
        Err(_) => {
            let mut failed = child_guard.0.take().expect("child");
            let _ = failed.kill();
            let _ = failed.wait();
            let mut err = String::new();
            if let Some(mut stderr) = failed.stderr.take() {
                let _ = stderr.read_to_string(&mut err);
            }
            let captured = done_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap_or_default();
            panic!("timed out waiting for default Web bind URL; stdout={captured}; stderr={err}");
        }
    };
    let web_base = printed_url;
    assert!(
        web_base.starts_with("http://127.0.0.1:") || web_base.starts_with("http://[::1]:"),
        "default Web must bind loopback; got {web_base}"
    );
    assert!(
        web_base.contains("?bt="),
        "default Web open URL must embed browser bootstrap token; got {web_base}"
    );
    let web_origin = web_base
        .split_once('?')
        .map(|(origin, _)| origin)
        .unwrap_or(web_base.as_str());
    let web_port: u16 = web_origin
        .rsplit(':')
        .next()
        .expect("port")
        .parse()
        .unwrap_or_else(|_| panic!("parse port from {web_base}"));

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let health_url = format!("{web_origin}/api/health");
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if Instant::now() > deadline {
            let mut failed = child_guard.0.take().expect("child");
            let _ = failed.kill();
            let _ = failed.wait();
            let mut err = String::new();
            if let Some(mut stderr) = failed.stderr.take() {
                let _ = stderr.read_to_string(&mut err);
            }
            panic!("default Web UI did not become healthy at {health_url}; stderr={err}");
        }
        if let Ok(resp) = client.get(&health_url).send().await
            && resp.status().is_success()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let pid = child_guard.0.as_ref().expect("child").id();
    let kill_status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(kill_status.success());

    let mut child = child_guard.0.take().expect("child");
    let exit = child
        .wait()
        .expect("wait for default Web process after SIGTERM");
    assert!(
        exit.success(),
        "SIGTERM must shut down cleanly (exit 0); status={exit:?}"
    );
    let captured_stdout = done_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| String::new());

    assert!(
        captured_stdout.contains("Libra Code server running")
            || captured_stdout.contains(&web_base),
        "default Web launch must print the Code UI URL on stdout; got:\n{captured_stdout}"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    let rebind_web = TcpListener::bind(format!("127.0.0.1:{web_port}"));
    assert!(
        rebind_web.is_ok(),
        "web port must be released after SIGTERM"
    );
}

#[test]
fn mcp_stdio_deprecation_warning_pins_legacy_boundary() {
    use libra::command::code::MCP_STDIO_DEPRECATION_WARNING;

    assert!(
        MCP_STDIO_DEPRECATION_WARNING.contains("deprecated MCP-only legacy"),
        "W4-03 must mark --stdio as MCP-only legacy: {MCP_STDIO_DEPRECATION_WARNING}"
    );
    assert!(
        MCP_STDIO_DEPRECATION_WARNING.contains("not live turn control"),
        "W4-03 must exclude turn control: {MCP_STDIO_DEPRECATION_WARNING}"
    );
    assert!(
        MCP_STDIO_DEPRECATION_WARNING.contains("libra code --control stdio"),
        "W4-03 must point automation at --control stdio: {MCP_STDIO_DEPRECATION_WARNING}"
    );
    assert!(
        MCP_STDIO_DEPRECATION_WARNING.contains("libra mcp --stdio"),
        "W4-03 must point to future libra mcp --stdio (DEFER-02): {MCP_STDIO_DEPRECATION_WARNING}"
    );
}

#[test]
fn code_help_documents_mcp_stdio_legacy_and_control_client() {
    use clap::CommandFactory;
    use libra::command::code::{CODE_EXAMPLES, CodeArgs};

    let help = CodeArgs::command().render_long_help().to_string();
    assert!(
        help.contains("Deprecated MCP-only legacy") || help.contains("deprecated MCP-only legacy"),
        "clap --stdio help must document MCP-only legacy; got:\n{help}"
    );
    assert!(
        help.contains("libra mcp --stdio") || CODE_EXAMPLES.contains("libra mcp --stdio"),
        "help/examples must mention future libra mcp --stdio"
    );
    assert!(
        CODE_EXAMPLES.contains("--control stdio"),
        "EXAMPLES must keep canonical automation client"
    );
    assert!(
        CODE_EXAMPLES.contains("Deprecated MCP-only legacy"),
        "EXAMPLES must mark --stdio as deprecated MCP-only legacy"
    );
}

#[tokio::test]
async fn code_control_command_removed() {
    use std::{
        fs,
        io::Write,
        process::{Command, Stdio},
        sync::Arc,
        time::Duration,
    };

    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
    };
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::sync::{Mutex, oneshot};

    // W5-01: the deprecated forwarding shim is physically removed — the binary
    // rejects `code-control` as an unknown command on the stable CLI error
    // path; the migration path lives in docs/commands/code-control.md.
    let removal_probe_dir = tempdir().expect("tempdir for removed-command probe");
    let rejected = Command::new(env!("CARGO_BIN_EXE_libra"))
        .args([
            "code-control",
            "--stdio",
            "--url",
            "http://127.0.0.1:3000",
            "--token-file",
            "control-token",
        ])
        .current_dir(removal_probe_dir.path())
        .output()
        .expect("run removed code-control probe");
    assert_eq!(
        rejected.status.code(),
        Some(129),
        "removed code-control must use the stable CLI-error exit"
    );
    let rejected_diag = format!(
        "{}{}",
        String::from_utf8_lossy(&rejected.stdout),
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert!(
        rejected_diag.contains("'code-control' is not a libra command"),
        "removed code-control must render the unknown-command diagnostic: {rejected_diag}"
    );

    // Root help must not expose the removed command either.
    let help = Command::new(env!("CARGO_BIN_EXE_libra"))
        .arg("--help")
        .current_dir(removal_probe_dir.path())
        .output()
        .expect("run libra --help");
    let help_text = format!(
        "{}{}",
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(
        !help_text.contains("code-control"),
        "root --help must not list removed code-control: {help_text}"
    );

    #[derive(Clone, Default)]
    struct Capture {
        tokens: Arc<Mutex<Vec<String>>>,
    }

    async fn conflict_attach(
        State(state): State<Capture>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        let token = headers
            .get("x-libra-control-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        state.tokens.lock().await.push(token);
        (
            StatusCode::CONFLICT,
            Json(json!({
                "error": {
                    "code": "CONTROLLER_CONFLICT",
                    "message": "another controller already holds the lease"
                }
            })),
        )
    }

    let capture = Capture::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback mock");
    let addr = listener.local_addr().expect("addr");
    let app = Router::new()
        .route("/api/code/controller/attach", post(conflict_attach))
        .with_state(capture.clone());
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });

    let temp = tempdir().expect("tempdir for control stdio probe");
    let token_path = temp.path().join("control-token");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let _ = fs::remove_file(&token_path);
        fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&token_path)
            .expect("create token")
            .write_all(b"control-stdio-token\n")
            .expect("write token");
        fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    #[cfg(not(unix))]
    {
        fs::write(&token_path, "control-stdio-token\n").expect("write token");
    }
    let token_str = token_path.to_str().expect("utf8 token path");
    let base_url = format!("http://{addr}");
    let attach_req = concat!(
        r#"{"jsonrpc":"2.0","method":"controller.attach","params":{"clientId":"w5-01","kind":"automation"},"id":1}"#,
        "\n",
    );
    let libra_bin = env!("CARGO_BIN_EXE_libra");
    let cwd = temp.path().to_path_buf();

    let run_client = {
        let cwd = cwd.clone();
        move |args: Vec<String>| {
            let cwd = cwd.clone();
            let attach = attach_req.to_string();
            tokio::task::spawn_blocking(move || {
                let mut child = Command::new(libra_bin)
                    .args(&args)
                    .current_dir(&cwd)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("spawn control client");
                {
                    let mut stdin = child.stdin.take().expect("stdin");
                    stdin
                        .write_all(attach.as_bytes())
                        .expect("write attach request");
                }
                child.wait_with_output().expect("wait control client")
            })
        }
    };

    // W5-01 must not regress the canonical `code --control stdio` client: it
    // still reaches the control endpoint and forwards JSON-RPC errors.
    let canonical = run_client(vec![
        "code".into(),
        "--control".into(),
        "stdio".into(),
        "--control-url".into(),
        base_url,
        "--control-token-file".into(),
        token_str.to_string(),
    ])
    .await
    .expect("join canonical");
    let canonical_stdout = String::from_utf8_lossy(&canonical.stdout);
    let canonical_stderr = String::from_utf8_lossy(&canonical.stderr);
    assert!(
        canonical.status.success(),
        "canonical must reach mock attach; stderr:\n{canonical_stderr}"
    );
    assert!(
        canonical_stdout.contains("CONTROLLER_CONFLICT") && canonical_stdout.contains("-32000"),
        "canonical must forward the JSON-RPC conflict; stdout:\n{canonical_stdout}"
    );

    // Wait briefly for the attach handler to record the token.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let tokens = loop {
        let tokens = capture.tokens.lock().await.clone();
        if !tokens.is_empty() || tokio::time::Instant::now() >= deadline {
            break tokens;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        tokens,
        vec!["control-stdio-token".to_string()],
        "canonical client must forward the control token header"
    );

    let _ = shutdown_tx.send(());
    let _ = server.await;
}

/// W5-07: the deprecated `--web` / `--web-only` aliases and the hidden
/// legacy-TUI rollback env are removed. The old flags must fail with clap's
/// unexpected-argument usage error plus a migration hint pointing at the
/// default Web Code UI, and the env var must no longer switch `libra code`
/// off the Web launch path.
#[tokio::test]
async fn web_alias_and_legacy_tui_env_removed() {
    use std::process::Command;

    let libra_bin = env!("CARGO_BIN_EXE_libra");

    // (a) Removed aliases: usage error + migration hint.
    for flag in ["--web", "--web-only"] {
        let probe_dir = tempfile::tempdir().expect("tempdir for removed-alias probe");
        let rejected = Command::new(libra_bin)
            .args(["code", flag])
            .current_dir(probe_dir.path())
            .output()
            .expect("run removed-alias probe");
        assert!(
            !rejected.status.success(),
            "removed alias {flag} must exit non-zero"
        );
        let diag = format!(
            "{}{}",
            String::from_utf8_lossy(&rejected.stdout),
            String::from_utf8_lossy(&rejected.stderr)
        );
        assert!(
            diag.contains("unexpected argument"),
            "{flag} must surface clap's unexpected-argument error; got:\n{diag}"
        );
        assert!(
            diag.contains("already defaults to the Web Code UI"),
            "{flag} must surface the W5-07 migration hint; got:\n{diag}"
        );
    }

    // (b) The removed rollback env no longer reaches the legacy TUI: with the
    // variable set, `code --provider codex --env-file …` must still hit the
    // Web-mode managed-Codex rejection instead of booting a TUI.
    let temp = tempfile::tempdir().expect("tempdir for legacy-env probe");
    let home_dir = temp.path().join(".home");
    let config_home = home_dir.join(".config");
    std::fs::create_dir_all(&config_home).expect("isolated HOME");
    let init = Command::new(libra_bin)
        .args(["init"])
        .current_dir(temp.path())
        .env("HOME", &home_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home_dir)
        .output()
        .expect("libra init");
    assert!(init.status.success(), "libra init failed");
    let env_path = temp.path().join(".env.w5-07");
    std::fs::write(&env_path, "OPENAI_API_KEY=from-env-file\n").expect("write env file");

    let rejected = Command::new(libra_bin)
        .args([
            "code",
            "--provider",
            "codex",
            "--env-file",
            env_path.to_str().expect("utf8 path"),
        ])
        .current_dir(temp.path())
        .env("HOME", &home_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home_dir)
        .env("LIBRA_CODE_LEGACY_TUI", "1")
        .output()
        .expect("run legacy-env probe");
    assert!(
        !rejected.status.success(),
        "codex + --env-file must remain a Web-mode rejection"
    );
    let diag = format!(
        "{}{}",
        String::from_utf8_lossy(&rejected.stdout),
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert!(
        diag.contains("Web Code UI") && diag.contains("--provider=codex"),
        "the removed env var must not switch to the legacy TUI; expected the Web-mode \
         codex/--env-file rejection, got:\n{diag}"
    );
}

/// W5-09: aggregate breaking-surface guard for the W5-01 family release.
///
/// One run pins every deprecated public surface the family deleted, so the
/// W5-09 release point cannot ship a partially-applied breaking state:
///   (a) W5-07 — `--web` / `--web-only` fail with a usage error + migration hint;
///   (b) W5-07 — `LIBRA_CODE_LEGACY_TUI` no longer changes any behavior;
///   (c) W5-01 — `code-control` is not exposed (unknown command + absent from help);
///   (d) W5-08 — bare `libra graph` AND bare `libra agent graph` are refused
///       with the interactive-TUI removal diagnostic while their `--json` /
///       `--machine` forms stay on the structured path;
///   (e) W5-06 — bare `libra code --provider codex --resume` fail-closes with a
///       migration hint (legacy TUI resume driver removed).
/// The heavy per-surface probes live in `web_alias_and_legacy_tui_env_removed`,
/// `code_control_command_removed`, and `graph_machine_survives_tui_removal`;
/// this guard is the cheap pre-push aggregate (Codex R28-P1-2).
#[test]
fn breaking_code_surface_migration() {
    use std::process::Command;

    let libra_bin = env!("CARGO_BIN_EXE_libra");
    let diag_of = |output: &std::process::Output| {
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };

    // (a) + (b): removed aliases still fail with the migration hint even when
    // the removed rollback env is set — the env cannot resurrect anything.
    for flag in ["--web", "--web-only"] {
        let probe_dir = tempfile::tempdir().expect("tempdir for aggregate alias probe");
        let rejected = Command::new(libra_bin)
            .args(["code", flag])
            .current_dir(probe_dir.path())
            .env("LIBRA_CODE_LEGACY_TUI", "1")
            .output()
            .expect("run aggregate alias probe");
        assert!(
            !rejected.status.success(),
            "removed alias {flag} must exit non-zero"
        );
        let diag = diag_of(&rejected);
        assert!(
            diag.contains("unexpected argument")
                && diag.contains("already defaults to the Web Code UI"),
            "{flag} must surface the W5-07 migration error even with the removed env set; got:\n{diag}"
        );
    }

    // (c): code-control is an unknown command and absent from root help.
    let probe_dir = tempfile::tempdir().expect("tempdir for aggregate code-control probe");
    let rejected = Command::new(libra_bin)
        .args(["code-control", "--stdio"])
        .current_dir(probe_dir.path())
        .output()
        .expect("run aggregate code-control probe");
    assert_eq!(
        rejected.status.code(),
        Some(129),
        "removed code-control must use the stable CLI-error exit"
    );
    assert!(
        diag_of(&rejected).contains("'code-control' is not a libra command"),
        "removed code-control must render the unknown-command diagnostic"
    );
    let help = Command::new(libra_bin)
        .arg("--help")
        .current_dir(probe_dir.path())
        .output()
        .expect("run libra --help");
    assert!(
        !diag_of(&help).contains("code-control"),
        "root --help must not list removed code-control"
    );

    // (d): bare `libra graph` AND bare `libra agent graph` are refused with
    // the W5-08 diagnostic (Codex r6: W5-08 removed BOTH interactive entries,
    // so a partial regression on either surface must fail this guard);
    // `--json` and `--machine` stay on the structured path — outside a repo
    // they fail with the stable structured repo error (LBR-REPO-001, exit
    // 128), NOT with the interactive-removal usage error (LBR-CLI-002, 129).
    let graph_dir = tempfile::tempdir().expect("tempdir for aggregate graph probe");
    for graph_cmd in [&["graph"][..], &["agent", "graph"][..]] {
        let cmd_label = graph_cmd.join(" ");
        let bare = Command::new(libra_bin)
            .args(graph_cmd)
            .arg("11111111-1111-4111-8111-111111111111")
            .current_dir(graph_dir.path())
            .output()
            .expect("run aggregate bare graph probe");
        let bare_diag = diag_of(&bare);
        assert!(
            bare_diag.contains("no longer opens an interactive TUI"),
            "bare {cmd_label} must surface the W5-08 removal diagnostic; got:\n{bare_diag}"
        );
        for structured_flag in ["--json", "--machine"] {
            let structured = Command::new(libra_bin)
                .args(graph_cmd)
                .args([structured_flag, "11111111-1111-4111-8111-111111111111"])
                .current_dir(graph_dir.path())
                .output()
                .expect("run aggregate structured graph probe");
            let structured_diag = diag_of(&structured);
            assert_eq!(
                structured.status.code(),
                Some(128),
                "{cmd_label} {structured_flag} must reach the structured repo error, not the removal usage error; got:\n{structured_diag}"
            );
            assert!(
                structured_diag.contains("LBR-REPO-001"),
                "{cmd_label} {structured_flag} must emit the stable structured error code; got:\n{structured_diag}"
            );
            assert!(
                !structured_diag.contains("no longer opens an interactive TUI"),
                "{cmd_label} {structured_flag} must not hit the W5-08 removal diagnostic; got:\n{structured_diag}"
            );
        }
    }

    // (e): bare `--provider codex --resume` fail-closes with the W5-06
    // migration hint. validate_mode_args runs after the worktree gate, so the
    // probe needs a registered repo (isolated HOME keeps it hermetic).
    let temp = tempfile::tempdir().expect("tempdir for aggregate codex-resume probe");
    let home_dir = temp.path().join(".home");
    let config_home = home_dir.join(".config");
    std::fs::create_dir_all(&config_home).expect("isolated HOME");
    let init = Command::new(libra_bin)
        .args(["init", "--vault=false", "--quiet"])
        .current_dir(temp.path())
        .env("HOME", &home_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home_dir)
        .output()
        .expect("libra init");
    assert!(init.status.success(), "libra init failed");
    let rejected = Command::new(libra_bin)
        .args([
            "code",
            "--provider",
            "codex",
            "--resume",
            "11111111-1111-4111-8111-111111111111",
        ])
        .current_dir(temp.path())
        .env("HOME", &home_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home_dir)
        .output()
        .expect("run aggregate codex-resume probe");
    assert!(
        !rejected.status.success(),
        "bare codex --resume must exit non-zero"
    );
    let diag = diag_of(&rejected);
    assert!(
        diag.contains("--resume is not supported with --provider=codex")
            && diag.contains("legacy TUI resume driver"),
        "bare codex --resume must surface the W5-06 migration hint; got:\n{diag}"
    );

    // (b+): the removed rollback env must not alter NORMAL dispatch either
    // (Codex r5: probing it only on clap-rejected aliases proves nothing
    // about the dispatch path). With `LIBRA_CODE_LEGACY_TUI=1` set, codex +
    // `--env-file` must still hit the Web-mode rejection — never a TUI boot.
    let env_path = temp.path().join(".env.w5-09");
    std::fs::write(&env_path, "OPENAI_API_KEY=from-env-file\n").expect("write env file");
    let rejected = Command::new(libra_bin)
        .args([
            "code",
            "--provider",
            "codex",
            "--env-file",
            env_path.to_str().expect("utf8 path"),
        ])
        .current_dir(temp.path())
        .env("HOME", &home_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home_dir)
        .env("LIBRA_CODE_LEGACY_TUI", "1")
        .output()
        .expect("run aggregate legacy-env dispatch probe");
    assert!(
        !rejected.status.success(),
        "codex + --env-file must remain a Web-mode rejection"
    );
    let diag = diag_of(&rejected);
    assert!(
        diag.contains("Web Code UI") && diag.contains("--provider=codex"),
        "the removed env var must not switch dispatch off the Web path; got:\n{diag}"
    );
}
