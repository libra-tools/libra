//! 强制补强项 #10 crash regression for the AG-19 hook ingest path
//! (`src/internal/ai/hooks/runtime.rs::ingest_agent_traces_payload`).
//!
//! Drives the built `libra` binary end-to-end (mirroring the harness in
//! `tests/agent_lifecycle_event_test.rs`) and proves that a hook handler
//! dying mid-flight — SIGKILL before/while reading stdin, an injected
//! panic after the payload is read+validated, or SIGKILL racing a `stop`
//! ingest — never leaves partial `agent_session` / `agent_checkpoint`
//! state visible through the CLI JSON surfaces, and never echoes raw
//! stdin bytes to stderr.
//!
//! The panic injection uses the test-only `LIBRA_TEST_HOOK_PANIC_AFTER_READ`
//! knob, which fires after envelope validation but before any database
//! write.

#![cfg(unix)]

use std::{
    io::Write,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::Duration,
};

use serde_json::{Value, json};

/// One isolated libra repository. Every test builds its own so no state is
/// shared between tests.
struct HookRepo {
    _tempdir: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

impl HookRepo {
    fn init() -> Self {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let home = tempdir.path().join("home");
        let repo = tempdir.path().join("repo");
        std::fs::create_dir_all(&home).expect("create fake home");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        let this = Self {
            _tempdir: tempdir,
            repo,
            home,
        };
        let out = this.run(&["init"], None, &[]);
        assert!(
            out.status.success(),
            "libra init failed: {}",
            describe(&out)
        );
        this
    }

    fn command(&self, args: &[&str], envs: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in envs {
            cmd.env(key, value);
        }
        cmd
    }

    /// Run to completion, optionally piping `stdin` (closed after writing).
    fn run(&self, args: &[&str], stdin: Option<&str>, envs: &[(&str, &str)]) -> Output {
        let mut cmd = self.command(args, envs);
        cmd.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = cmd.spawn().expect("spawn libra binary");
        if let Some(payload) = stdin {
            child
                .stdin
                .take()
                .expect("stdin piped")
                .write_all(payload.as_bytes())
                .expect("write hook envelope to stdin");
        }
        child.wait_with_output().expect("wait for libra binary")
    }

    /// Spawn without waiting, stdin piped and left under the caller's
    /// control (kill/write/drop as each scenario needs).
    fn spawn_hook(&self, verb: &str, envs: &[(&str, &str)]) -> Child {
        let mut cmd = self.command(&["agent", "hooks", "claude-code", verb], envs);
        cmd.stdin(Stdio::piped());
        cmd.spawn().expect("spawn libra hook handler")
    }

    fn sessions(&self) -> Vec<Value> {
        json_data_rows(
            &self.run(&["agent", "session", "list", "--json"], None, &[]),
            "sessions",
        )
    }

    fn checkpoints(&self) -> Vec<Value> {
        json_data_rows(
            &self.run(&["agent", "checkpoint", "list", "--json"], None, &[]),
            "checkpoints",
        )
    }

    fn session_show(&self, session: &str) -> Value {
        let out = self.run(&["agent", "session", "show", session, "--json"], None, &[]);
        assert!(
            out.status.success(),
            "session show failed: {}",
            describe(&out)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let parsed: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|err| panic!("session show stdout is not JSON ({err}): {stdout}"));
        assert_eq!(
            parsed["ok"],
            json!(true),
            "session show envelope not ok: {parsed}"
        );
        parsed["data"].clone()
    }

    fn envelope(&self, hook_event_name: &str, session_id: &str, extra: Value) -> String {
        let mut obj = json!({
            "hook_event_name": hook_event_name,
            "session_id": session_id,
            "cwd": self.repo.to_string_lossy(),
        });
        if let Value::Object(fields) = extra {
            for (key, value) in fields {
                obj[key.as_str()] = value;
            }
        }
        obj.to_string()
    }
}

fn describe(out: &Output) -> String {
    format!(
        "status: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

/// AG-20 paged list payload: rows live under `data.<rows_key>`
/// (`sessions` / `checkpoints`) next to `next_cursor`.
fn json_data_rows(out: &Output, rows_key: &str) -> Vec<Value> {
    assert!(out.status.success(), "CLI query failed: {}", describe(out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout is not JSON ({err}): {stdout}"));
    assert_eq!(parsed["ok"], json!(true), "envelope not ok: {parsed}");
    parsed["data"][rows_key]
        .as_array()
        .unwrap_or_else(|| panic!("data.{rows_key} is not an array: {parsed}"))
        .clone()
}

/// SIGKILL the child, reap it, and assert it actually died from the signal
/// (not a clean exit that raced ahead of the kill).
fn kill_and_reap(mut child: Child) -> std::process::ExitStatus {
    child.kill().expect("SIGKILL the hook handler");
    let status = child.wait().expect("reap the killed hook handler");
    assert!(!status.success(), "killed handler must not report success");
    status
}

/// A handler killed while stdin is still open (nothing or only half an
/// envelope written) has not started ingesting — the ingest only begins
/// after EOF/full read — so no session row, no checkpoint, no partial DB
/// or checkpoint write may be visible afterwards.
#[test]
fn hook_handler_killed_mid_ingest_leaves_no_partial_write() {
    use std::os::unix::process::ExitStatusExt;

    let repo = HookRepo::init();

    // (a) stdin left open with nothing written: the handler blocks reading;
    // SIGKILL it and verify nothing was persisted.
    let mut child = repo.spawn_hook("session-start", &[]);
    let stdin = child.stdin.take().expect("stdin piped"); // hold it open
    std::thread::sleep(Duration::from_millis(300));
    let status = kill_and_reap(child);
    assert_eq!(
        status.signal(),
        Some(libc_sigkill()),
        "handler must have died from SIGKILL, got {status:?}"
    );
    drop(stdin);
    assert!(
        repo.sessions().is_empty(),
        "a handler killed before EOF must not create a session row"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "a handler killed before EOF must not create a checkpoint"
    );

    // (b) half an envelope written, stdin still open: same invariant — the
    // ingest starts only after the full read, so the truncated payload must
    // never surface as a row.
    let envelope = repo.envelope("SessionStart", "sess-crash-half", json!({}));
    let half = &envelope[..envelope.len() / 2];
    let mut child = repo.spawn_hook("session-start", &[]);
    let mut stdin = child.stdin.take().expect("stdin piped");
    stdin
        .write_all(half.as_bytes())
        .expect("write half an envelope");
    stdin.flush().expect("flush half envelope");
    std::thread::sleep(Duration::from_millis(300));
    kill_and_reap(child);
    drop(stdin);
    assert!(
        repo.sessions().is_empty(),
        "a handler killed after half an envelope must not create a session row"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "a handler killed after half an envelope must not create a checkpoint"
    );
}

/// SIGKILL's signal number without pulling in the libc crate.
fn libc_sigkill() -> i32 {
    9
}

/// A panic injected after the payload is read and validated (but before
/// any DB write, via `LIBRA_TEST_HOOK_PANIC_AFTER_READ`) exits non-zero,
/// proves the path executed (the panic message surfaces), never echoes the
/// raw stdin bytes, and persists nothing.
#[test]
fn hook_handler_panic_leaves_no_partial_write_and_no_stdin_echo() {
    let repo = HookRepo::init();

    let marker = "LIBRA_TEST_CRASH_PROMPT_MARKER_7d20e4";
    let envelope = repo.envelope(
        "SessionStart",
        "sess-crash-panic",
        json!({ "prompt": format!("{marker} please deploy") }),
    );
    let out = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&envelope),
        &[("LIBRA_TEST_HOOK_PANIC_AFTER_READ", "1")],
    );
    assert!(
        !out.status.success(),
        "an injected panic must exit non-zero: {}",
        describe(&out)
    );

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stderr.contains("test-injected hook panic (LIBRA_TEST_HOOK_PANIC_AFTER_READ)"),
        "the panic message must surface, proving the knob fired after read+validate: {}",
        describe(&out)
    );
    assert!(
        !stderr.contains(marker) && !stdout.contains(marker),
        "the raw stdin bytes must not be echoed on a panic: {}",
        describe(&out)
    );

    assert!(
        repo.sessions().is_empty(),
        "a panic before the DB write must not create a session row"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "a panic before the DB write must not create a checkpoint"
    );
}

/// Codex runs its Stop hook in the foreground. If checkpoint publication
/// fails after writer registration, capture must remain advisory: the host
/// callback succeeds, leaks no hook payload, and exposes a retryable session
/// diagnostic. The next Stop must re-own the abandoned attempt and publish.
#[test]
fn codex_stop_exposes_and_retries_checkpoint_publication_failure() {
    let repo = HookRepo::init();
    let user_name = repo.run(&["config", "user.name", "Test"], None, &[]);
    assert!(
        user_name.status.success(),
        "user.name: {}",
        describe(&user_name)
    );
    let user_email = repo.run(&["config", "user.email", "test@example.com"], None, &[]);
    assert!(
        user_email.status.success(),
        "user.email: {}",
        describe(&user_email)
    );
    let initial_commit = repo.run(
        &[
            "commit",
            "--allow-empty",
            "--author",
            "Test <test@example.com>",
            "-m",
            "agent-hook-test",
        ],
        None,
        &[],
    );
    assert!(
        initial_commit.status.success(),
        "initial commit: {}",
        describe(&initial_commit)
    );
    let session = "sess-codex-lock-failure";
    let start = repo.run(
        &["hooks", "codex", "session-start"],
        Some(&repo.envelope("SessionStart", session, json!({}))),
        &[],
    );
    assert!(
        start.status.success(),
        "session-start: {}",
        describe(&start)
    );

    let marker = "LIBRA_TEST_CODEX_STOP_MARKER_1d412a";
    let stop = repo.run(
        &["hooks", "codex", "stop"],
        Some(&repo.envelope("Stop", session, json!({ "last_assistant_message": marker }))),
        &[("LIBRA_TEST_CHECKPOINT_FAIL_AFTER_REGISTRATION", "1")],
    );

    assert!(
        stop.status.success(),
        "Codex Stop must acknowledge capture failure: {}",
        describe(&stop)
    );
    let stderr = String::from_utf8_lossy(&stop.stderr);
    let stdout = String::from_utf8_lossy(&stop.stdout);
    assert!(
        !stderr.contains(marker) && !stdout.contains(marker),
        "the hook payload must not be echoed after capture failure: {}",
        describe(&stop)
    );
    assert!(
        repo.checkpoints().is_empty(),
        "a failed Codex Stop checkpoint publication must not leave a checkpoint"
    );
    let failed = repo.session_show(&format!("codex__{session}"));
    assert_eq!(failed["capture_status"], json!("retryable"));
    assert_eq!(
        failed["capture_error_code"],
        json!("checkpoint_write_failed")
    );

    // The abandoned coverage claim is deliberately re-ownable. A later Stop
    // retries the same full-session capture after the injected failure is
    // removed, matching Entire's deferred recovery behavior.
    let retry = repo.run(
        &["hooks", "codex", "stop"],
        Some(&repo.envelope("Stop", session, json!({}))),
        &[],
    );
    assert!(
        retry.status.success(),
        "Codex Stop retry failed: {}",
        describe(&retry)
    );
    let checkpoint_out = repo.run(&["agent", "checkpoint", "list", "--json"], None, &[]);
    let checkpoints = json_data_rows(&checkpoint_out, "checkpoints");
    let after_retry = repo.session_show(&format!("codex__{session}"));
    assert_eq!(
        checkpoints.len(),
        1,
        "retry should publish the checkpoint; retry={} list={} session={}",
        describe(&retry),
        describe(&checkpoint_out),
        after_retry
    );
    let recovered = repo.session_show(&format!("codex__{session}"));
    assert!(
        recovered.get("capture_status").is_none(),
        "successful retry should clear the failure diagnostic: {recovered}"
    );
}

/// SIGKILL racing a `stop` ingest (which writes a committed checkpoint on
/// `refs/libra/traces` plus an `agent_checkpoint` row) must never leave
/// torn *visible* state: whatever subset of the five racy attempts landed,
/// `agent checkpoint list --json` parses and every listed row is complete
/// (non-empty commit/tree/blob ids, scope `committed`), and
/// `agent session list --json` parses. The kill may land before or after
/// the write — both are acceptable; a half-written visible row is not.
#[test]
fn stop_killed_mid_run_leaves_no_torn_visible_checkpoint_state() {
    let repo = HookRepo::init();
    let session = "sess-crash-stop";

    // A completed session-start so the stop verb has a valid prior session.
    let out = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&repo.envelope("SessionStart", session, json!({}))),
        &[],
    );
    assert!(out.status.success(), "session-start: {}", describe(&out));

    for attempt in 0..5 {
        let envelope = repo.envelope(
            "Stop",
            session,
            json!({ "prompt": format!("turn {attempt} wrap-up") }),
        );
        let mut child = repo.spawn_hook("stop", &[]);
        {
            let mut stdin = child.stdin.take().expect("stdin piped");
            stdin
                .write_all(envelope.as_bytes())
                .expect("write full stop envelope");
            // stdin drops (EOF) here, letting the ingest begin.
        }
        // Racy by design: ~30ms usually lands the SIGKILL somewhere between
        // process startup and the checkpoint write.
        std::thread::sleep(Duration::from_millis(30));
        let _ = child.kill();
        let _ = child.wait().expect("reap the stop handler");

        // Invariant after every attempt: parseable surfaces, complete rows.
        let checkpoints = repo.checkpoints();
        for row in &checkpoints {
            for field in [
                "checkpoint_id",
                "traces_commit",
                "tree_oid",
                "metadata_blob_oid",
            ] {
                let value = row[field].as_str().unwrap_or_default();
                assert!(
                    !value.is_empty(),
                    "attempt {attempt}: checkpoint row has empty '{field}': {row}"
                );
                assert!(
                    value.chars().any(|ch| ch != '0'),
                    "attempt {attempt}: checkpoint row has zero '{field}': {row}"
                );
            }
            assert_eq!(
                row["scope"],
                json!("committed"),
                "attempt {attempt}: unexpected checkpoint scope: {row}"
            );
        }
        let sessions = repo.sessions();
        assert!(
            !sessions.is_empty(),
            "attempt {attempt}: the claimed session row must survive the kill"
        );
    }
}
