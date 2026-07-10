//! AG-19 central hook dispatcher contract (plan.md Task A4).
//!
//! Drives the built `libra` binary end-to-end: `libra init` in a tempdir,
//! then `libra agent hooks <agent> <verb>` with a JSON envelope piped via
//! stdin, asserting on exit codes, stderr hygiene, and the observable
//! `agent session list` / `agent checkpoint list` CLI JSON surfaces. The
//! behaviour under test lives in
//! `src/internal/ai/hooks/runtime.rs::ingest_agent_traces_payload`:
//!
//! - invalid envelopes (non-JSON stdin, path-traversal session ids) are
//!   rejected before any session/checkpoint write and never echo raw
//!   stdin bytes to stderr;
//! - owner filtering is first-writer-wins by agent kind per provider
//!   session id (SessionStart/TurnStart exempt); non-owner events are
//!   skipped with exit 0, never a hard error;
//! - an unrecognized `hook_event_name` (newer upstream agent) is
//!   skipped-and-logged (`unknown_event_type`) with exit 0, no writes;
//! - a recognized name that maps to the wrong lifecycle kind for the CLI
//!   verb fails closed with a non-zero exit;
//! - gemini is uninstall-only: both hook entry points reject its verbs
//!   with a hint and never ingest;
//! - the checkpoint writer re-confirms ownership after the upsert, so two
//!   agents racing the same fresh provider session id can never both
//!   write checkpoints (owner-race closure).

#![cfg(unix)]

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use serde_json::{Value, json};

/// One isolated libra repository plus a fake `$HOME` for provider
/// transcript roots (`~/.claude`). Every test builds its own so no state
/// is shared between tests.
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
        let out = this.run(&["init"], None);
        assert!(
            out.status.success(),
            "libra init failed: {}",
            describe(&out)
        );
        this
    }

    /// Run the built `libra` binary inside the repo with a clean
    /// environment. `LIBRA_TEST_HOME` mirrors `HOME` so the hook runtime's
    /// provider-root check (`transcript_path_within_provider_root`)
    /// resolves `~/.claude` under the tempdir.
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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

    /// `libra agent hooks <agent> <verb>` with `envelope` piped via stdin.
    fn hook(&self, agent: &str, verb: &str, envelope: &str) -> Output {
        self.run(&["agent", "hooks", agent, verb], Some(envelope))
    }

    /// Spawn `libra agent hooks <agent> <verb>` without waiting, stdin
    /// piped — used by the owner-race test to run two ingests
    /// concurrently.
    fn spawn_hook(&self, agent: &str, verb: &str) -> std::process::Child {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.args(["agent", "hooks", agent, verb])
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.spawn().expect("spawn libra hook handler")
    }

    /// Parsed rows of `libra agent session list --json` (AG-20 paged
    /// payload: rows under `data.sessions`).
    fn sessions(&self) -> Vec<Value> {
        json_data_rows(
            &self.run(&["agent", "session", "list", "--json"], None),
            "sessions",
        )
    }

    /// Parsed rows of `libra agent checkpoint list --json` (AG-20 paged
    /// payload: rows under `data.checkpoints`).
    fn checkpoints(&self) -> Vec<Value> {
        json_data_rows(
            &self.run(&["agent", "checkpoint", "list", "--json"], None),
            "checkpoints",
        )
    }

    /// Canonical hook envelope with the repo as `cwd`, plus extra fields.
    fn envelope(
        &self,
        hook_event_name: &str,
        session_id: &str,
        transcript_path: Option<&Path>,
        extra: Value,
    ) -> String {
        let mut obj = json!({
            "hook_event_name": hook_event_name,
            "session_id": session_id,
            "cwd": self.repo.to_string_lossy(),
        });
        if let Some(path) = transcript_path {
            obj["transcript_path"] = json!(path.to_string_lossy());
        }
        if let Value::Object(fields) = extra {
            for (key, value) in fields {
                obj[key.as_str()] = value;
            }
        }
        obj.to_string()
    }

    /// Create a plausible transcript under the fake home's `~/.claude`
    /// (the Claude Code provider's protected dir) so the checkpoint
    /// writer's provider-root trust gate accepts it.
    fn write_claude_transcript(&self) -> PathBuf {
        let dir = self.home.join(".claude").join("projects").join("x");
        std::fs::create_dir_all(&dir).expect("create ~/.claude transcript dir");
        let path = dir.join("transcript.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"user\",\"text\":\"hello\"}\n{\"type\":\"assistant\",\"text\":\"done\"}\n",
        )
        .expect("write transcript fixture");
        path
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

/// Parse a `{"ok":true,"command":…,"data":[…]}` CLI JSON envelope and
/// return the `data` array.
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

fn agent_kinds(sessions: &[Value]) -> Vec<String> {
    sessions
        .iter()
        .map(|row| row["agent_kind"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Invalid envelopes must be rejected before anything is persisted, and
/// the error path must not echo the raw stdin bytes (they may contain
/// secrets — the whole point of ingesting through a redaction pipeline).
#[test]
fn invalid_hook_envelopes_are_rejected_before_checkpoint() {
    let repo = HookRepo::init();

    // (a) Non-JSON stdin. The marker must not leak into stderr/stdout.
    let marker = "LIBRA_TEST_GARBAGE_MARKER_93b1f2";
    let garbage = format!("{marker} {{ this is not json");
    let out = repo.hook("claude-code", "stop", &garbage);
    assert!(
        !out.status.success(),
        "non-JSON stdin must fail: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !stderr.contains(marker) && !stdout.contains(marker),
        "raw stdin bytes must not be echoed on rejection: {}",
        describe(&out)
    );
    assert!(
        repo.sessions().is_empty(),
        "rejected envelope must not create a session row"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "rejected envelope must not create a checkpoint"
    );

    // (b) Path-traversal session_id — rejected by envelope validation the
    // same way, again without echoing payload fields.
    let traversal_marker = "LIBRA_TEST_TRAVERSAL_MARKER_51ac07";
    let envelope = repo.envelope(
        "Stop",
        "../../x",
        None,
        json!({ "prompt": traversal_marker }),
    );
    let out = repo.hook("claude-code", "stop", &envelope);
    assert!(
        !out.status.success(),
        "path-traversal session_id must fail: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !stderr.contains(traversal_marker) && !stdout.contains(traversal_marker),
        "payload fields must not be echoed on rejection: {}",
        describe(&out)
    );
    assert!(
        repo.sessions().is_empty(),
        "path-traversal envelope must not create a session row"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "path-traversal envelope must not create a checkpoint"
    );
}

/// First-writer-wins: once claude_code has claimed provider session `S`,
/// a codex `stop` for the same `S` is skipped (exit 0) — no codex row,
/// no extra checkpoint. (Codex is the second provider here because gemini
/// is uninstall-only and its hook entries reject before ingest.)
#[test]
fn owner_claim_prevents_duplicate_checkpoint() {
    let repo = HookRepo::init();
    let transcript = repo.write_claude_transcript();
    let session = "sess-owner-claim";

    // claude_code claims S…
    let out = repo.hook(
        "claude-code",
        "session-start",
        &repo.envelope("SessionStart", session, Some(&transcript), json!({})),
    );
    assert!(out.status.success(), "session-start: {}", describe(&out));

    // …and stops a turn, which writes a committed checkpoint.
    let out = repo.hook(
        "claude-code",
        "stop",
        &repo.envelope("Stop", session, Some(&transcript), json!({})),
    );
    assert!(out.status.success(), "claude stop: {}", describe(&out));

    let sessions = repo.sessions();
    assert_eq!(
        sessions.len(),
        1,
        "exactly one claimed session expected, got {sessions:?}"
    );
    assert_eq!(sessions[0]["agent_kind"], json!("claude_code"));
    assert_eq!(
        sessions[0]["session_id"],
        json!(format!("claude__{session}"))
    );
    let checkpoints_before = repo.checkpoints();
    assert!(
        !checkpoints_before.is_empty(),
        "claude stop with a valid transcript must write at least one checkpoint"
    );

    // A codex adapter forwarding the SAME provider session id must be
    // skipped: exit 0 (not an error), no new session row, no new
    // checkpoint. ("Stop" maps to TurnEnd in the codex parser.)
    let out = repo.hook(
        "codex",
        "stop",
        &repo.envelope("Stop", session, None, json!({})),
    );
    assert!(
        out.status.success(),
        "non-owner codex stop must skip with exit 0, not fail: {}",
        describe(&out)
    );

    let sessions = repo.sessions();
    assert_eq!(
        agent_kinds(&sessions),
        vec!["claude_code".to_string()],
        "non-owner stop must not create a codex session row: {sessions:?}"
    );
    assert_eq!(
        repo.checkpoints().len(),
        checkpoints_before.len(),
        "non-owner stop must not add checkpoints"
    );
}

/// SessionStart is exempt from owner filtering (it may establish a
/// claim), so a second provider's SessionStart for the same provider
/// session id may create a row — but non-exempt events from the
/// non-owner (earliest `started_at` wins, `session_id` ASC tiebreak →
/// claude_code) stay skipped. Codex is the second provider because
/// gemini's hook entries are uninstall-only and reject before ingest.
#[test]
fn session_start_exempt_allows_second_provider_claim_row() {
    let repo = HookRepo::init();
    let session = "sess-exempt-claim";

    let out = repo.hook(
        "claude-code",
        "session-start",
        &repo.envelope("SessionStart", session, None, json!({})),
    );
    assert!(
        out.status.success(),
        "claude session-start: {}",
        describe(&out)
    );

    // Exempt event: allowed through even though claude_code holds the
    // claim. A second (codex) row MAY now exist.
    let out = repo.hook(
        "codex",
        "session-start",
        &repo.envelope("SessionStart", session, None, json!({})),
    );
    assert!(
        out.status.success(),
        "codex session-start (exempt) must exit 0: {}",
        describe(&out)
    );
    let kinds = agent_kinds(&repo.sessions());
    assert!(
        kinds.contains(&"claude_code".to_string()),
        "claude_code claim row must survive the exempt codex SessionStart: {kinds:?}"
    );

    // The point to pin: codex's NON-exempt stop is still skipped — the
    // owner is the earliest started_at (tiebreak `claude__…` <
    // `codex__…`) — so it must exit 0 and write no checkpoint and not
    // stop the codex row.
    let out = repo.hook(
        "codex",
        "stop",
        &repo.envelope("Stop", session, None, json!({})),
    );
    assert!(
        out.status.success(),
        "non-owner codex stop must skip with exit 0: {}",
        describe(&out)
    );
    assert!(
        repo.checkpoints().is_empty(),
        "the skipped codex stop must not write a checkpoint (claude never stopped here)"
    );
    for row in repo.sessions() {
        if row["agent_kind"] == json!("codex") {
            assert_ne!(
                row["state"],
                json!("stopped"),
                "skipped codex stop must not mutate the codex exemption row: {row}"
            );
        }
    }
}

/// Gemini is uninstall-only (AG-17 demotion): BOTH hook entry points —
/// the top-level `libra hooks gemini <verb>` and the hidden
/// `libra agent hooks gemini <verb>` — reject with the uninstall-only
/// hint before reading anything, and never write a session row or
/// checkpoint.
#[test]
fn gemini_hook_entries_reject_with_uninstall_only_hint() {
    let repo = HookRepo::init();
    let envelope = repo.envelope("Stop", "sess-gemini-reject", None, json!({}));

    for args in [
        ["hooks", "gemini", "stop"].as_slice(),
        ["agent", "hooks", "gemini", "stop"].as_slice(),
    ] {
        let out = repo.run(args, Some(&envelope));
        assert!(
            !out.status.success(),
            "`libra {}` must reject: {}",
            args.join(" "),
            describe(&out)
        );
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            stderr.contains("uninstall-only"),
            "`libra {}` must name the uninstall-only state: {}",
            args.join(" "),
            describe(&out)
        );
        assert!(
            stderr.contains("libra agent remove gemini"),
            "`libra {}` must hint at the removal command: {}",
            args.join(" "),
            describe(&out)
        );
    }

    assert!(
        repo.sessions().is_empty(),
        "rejected gemini hooks must not create a session row"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "rejected gemini hooks must not create a checkpoint"
    );
}

/// Owner-race closure: the pre-upsert owner check is a read-then-write
/// window, so two agents racing the same FRESH provider session id can
/// both pass it. The checkpoint writer re-confirms ownership after the
/// upsert (earliest `started_at`, `session_id` ASC tiebreak) and the
/// loser skips fail-closed — so however the race lands, every checkpoint
/// for that session belongs to exactly one agent kind. The winner may
/// vary by timing; the invariant may not.
#[test]
fn simultaneous_stop_race_yields_single_owner_checkpoints() {
    let repo = HookRepo::init();

    for attempt in 0..5 {
        let session = format!("sess-owner-race-{attempt}");
        let claude_envelope = repo.envelope("Stop", &session, None, json!({}));
        let codex_envelope = repo.envelope("Stop", &session, None, json!({}));

        // Spawn both handlers before feeding either stdin so the two
        // ingests overlap as much as the scheduler allows.
        let mut claude_child = repo.spawn_hook("claude-code", "stop");
        let mut codex_child = repo.spawn_hook("codex", "stop");
        claude_child
            .stdin
            .take()
            .expect("claude stdin piped")
            .write_all(claude_envelope.as_bytes())
            .expect("write claude stop envelope");
        codex_child
            .stdin
            .take()
            .expect("codex stdin piped")
            .write_all(codex_envelope.as_bytes())
            .expect("write codex stop envelope");

        let claude_out = claude_child
            .wait_with_output()
            .expect("wait for claude stop");
        let codex_out = codex_child.wait_with_output().expect("wait for codex stop");
        assert!(
            claude_out.status.success(),
            "attempt {attempt}: racing claude stop must exit 0 (skip, never error): {}",
            describe(&claude_out)
        );
        assert!(
            codex_out.status.success(),
            "attempt {attempt}: racing codex stop must exit 0 (skip, never error): {}",
            describe(&codex_out)
        );

        // Single-owner invariant for this session id: all checkpoints
        // carry the same `<kind>__<session>` prefix, whichever kind won.
        let suffix = format!("__{session}");
        let owners: std::collections::HashSet<String> = repo
            .checkpoints()
            .iter()
            .filter_map(|row| row["session_id"].as_str())
            .filter(|session_id| session_id.ends_with(&suffix))
            .map(|session_id| {
                session_id
                    .split("__")
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            owners.len(),
            1,
            "attempt {attempt}: exactly one agent kind may own the session's checkpoints, \
             got {owners:?}"
        );
    }
}

/// Forward compatibility: an event name this build does not recognize is
/// skipped-and-logged (`unknown_event_type`) with exit 0 — no parse
/// error, no session row, no checkpoint.
#[test]
fn unknown_event_type_is_skipped_not_fatal() {
    let repo = HookRepo::init();
    let out = repo.hook(
        "claude-code",
        "stop",
        &repo.envelope("FutureFancyEvent", "sess-future-event", None, json!({})),
    );
    assert!(
        out.status.success(),
        "unknown hook_event_name must skip with exit 0: {}",
        describe(&out)
    );
    assert!(
        repo.sessions().is_empty(),
        "unknown event must not create a session row"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "unknown event must not create a checkpoint"
    );
}

/// A *recognized* event name that maps to a different lifecycle kind than
/// the CLI verb expects is NOT the unknown-name case: it fails closed
/// with a non-zero exit (mis-wired hook configs must surface loudly).
#[test]
fn kind_mismatch_still_fails_closed() {
    let repo = HookRepo::init();
    // `stop` expects TurnEnd; "SessionStart" is recognized but parses to
    // SessionStart.
    let out = repo.hook(
        "claude-code",
        "stop",
        &repo.envelope("SessionStart", "sess-kind-mismatch", None, json!({})),
    );
    assert!(
        !out.status.success(),
        "recognized-but-mismatched event kind must fail closed: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("kind mismatch"),
        "diagnostic should name the kind mismatch: {}",
        describe(&out)
    );
    assert!(
        repo.sessions().is_empty(),
        "kind mismatch must not create a session row"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "kind mismatch must not create a checkpoint"
    );
}

/// A0-02: a `SubagentStop` (→ `SubagentEnd`) boundary materialises an
/// independent `scope='subagent'` checkpoint, and it stays distinguishable
/// from the session's `committed` checkpoints in `checkpoint list`.
#[test]
fn subagent_end_materializes_distinct_subagent_scope_checkpoint() {
    let repo = HookRepo::init();
    let session = "sess-subagent";

    // Establish a codex-owned session (codex exposes native subagent hooks).
    let out = repo.hook(
        "codex",
        "session-start",
        &repo.envelope("SessionStart", session, None, json!({})),
    );
    assert!(
        out.status.success(),
        "codex session-start: {}",
        describe(&out)
    );

    // A turn Stop writes a `committed` checkpoint the subagent links back to.
    let out = repo.hook(
        "codex",
        "stop",
        &repo.envelope("Stop", session, None, json!({})),
    );
    assert!(out.status.success(), "codex stop: {}", describe(&out));

    // A SubagentStop boundary materialises a distinct subagent checkpoint.
    let out = repo.hook(
        "codex",
        "subagent-end",
        &repo.envelope("SubagentStop", session, None, json!({})),
    );
    assert!(
        out.status.success(),
        "codex subagent-end must materialise a subagent checkpoint: {}",
        describe(&out)
    );

    let checkpoints = repo.checkpoints();
    let scopes: Vec<String> = checkpoints
        .iter()
        .map(|c| c["scope"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        scopes.iter().any(|s| s == "subagent"),
        "SubagentStop must produce a scope='subagent' checkpoint, got {scopes:?}"
    );
    assert!(
        scopes.iter().any(|s| s == "committed"),
        "the committed turn checkpoint must remain distinguishable, got {scopes:?}"
    );
}

/// A0-03: a malformed (non-JSON) or schema-invalid hook envelope is rejected
/// with the stable `LBR-AGENT-008` (`AgentHookEnvelopeInvalid`) code and a
/// non-zero exit — not a bare fatal — so automation can distinguish an
/// envelope reject from a genuine runtime failure.
#[test]
fn hook_envelope_invalid_emits_lbr_agent_008() {
    let repo = HookRepo::init();

    // Malformed JSON: fails at the JSON parse gate.
    let out = repo.hook("codex", "session-start", "{ this is not valid json");
    assert!(
        !out.status.success(),
        "a malformed envelope must fail: {}",
        describe(&out)
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "an envelope reject exits 128: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-AGENT-008"),
        "malformed envelope must carry LBR-AGENT-008: {stderr}"
    );

    // Well-formed JSON but schema-invalid (missing required fields) also maps
    // to LBR-AGENT-008.
    let out = repo.hook("codex", "session-start", "{}");
    assert!(!out.status.success(), "schema-invalid envelope must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-AGENT-008"),
        "schema-invalid envelope must carry LBR-AGENT-008: {stderr}"
    );
}
