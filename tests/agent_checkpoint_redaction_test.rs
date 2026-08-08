//! AG-19 redaction-before-persist for hook ingest (plan.md Task A4).
//!
//! The central dispatcher (`src/internal/ai/hooks/runtime.rs::
//! ingest_agent_traces_payload`) must redact every free-form envelope
//! field — prompt, assistant_message, tool_input AND tool_response —
//! *before* anything reaches durable storage, and stamp the aggregated
//! report onto `agent_session.redaction_report`.
//!
//! Each test drives the built binary end-to-end (`libra init` → `libra
//! agent hooks claude-code <verb>` with an envelope carrying a canonical
//! AWS access-key-id shape) and then asserts:
//! - the hook exits 0;
//! - the raw token is absent from ALL CLI JSON output (`agent session
//!   list/show --json` — note: those surfaces do not expose
//!   `redaction_report`, so the report shape is verified directly on the
//!   persisted `agent_session` row);
//! - the persisted row carries a `redaction_report` whose `matches` name
//!   the `aws-access-key-id` rule, and no column of the row (nor the raw
//!   SQLite file) contains the token.

#![cfg(unix)]

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

use sea_orm::{ConnectOptions, ConnectionTrait, Database, Statement};
use serde_json::{Value, json};

/// Canonical AWS access-key-id fixture (AWS docs example key), composed
/// at runtime so the literal shape never sits in source where secret
/// scanners would flag it.
fn aws_token() -> String {
    format!("AKIA{}", "IOSFODNN7EXAMPLE")
}

/// One isolated libra repository plus a fake `$HOME`. Mirrors the harness
/// in `tests/agent_lifecycle_event_test.rs` (top-level targets cannot
/// share `tests/command/mod.rs` helpers).
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

    fn hook(&self, verb: &str, envelope: &str) -> Output {
        self.run(&["agent", "hooks", "claude-code", verb], Some(envelope))
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

    fn db_path(&self) -> PathBuf {
        self.repo.join(".libra").join("libra.db")
    }

    /// Assert the token never surfaces in the JSON CLI output of `agent
    /// session list` and `agent session show <id>`, and that the session
    /// row actually exists.
    fn assert_cli_json_free_of(&self, session_id: &str, token: &str) {
        let list = self.run(&["agent", "session", "list", "--json"], None);
        assert!(list.status.success(), "session list: {}", describe(&list));
        let list_stdout = String::from_utf8_lossy(&list.stdout).to_string();
        assert!(
            !list_stdout.contains(token),
            "raw token leaked into `agent session list --json`:\n{list_stdout}"
        );
        assert!(
            list_stdout.contains(session_id),
            "expected session '{session_id}' in list output:\n{list_stdout}"
        );

        let show = self.run(&["agent", "session", "show", session_id, "--json"], None);
        assert!(show.status.success(), "session show: {}", describe(&show));
        let show_stdout = String::from_utf8_lossy(&show.stdout).to_string();
        assert!(
            !show_stdout.contains(token),
            "raw token leaked into `agent session show --json`:\n{show_stdout}"
        );
        let parsed: Value = serde_json::from_str(show_stdout.trim()).expect("show output is JSON");
        assert_eq!(parsed["data"]["session_id"], json!(session_id));
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

/// Read the persisted `agent_session` row straight from the repo's SQLite
/// store and assert redaction-before-persist observably happened:
/// no text column carries the raw token, and `redaction_report.matches`
/// names the `aws-access-key-id` rule.
async fn assert_persisted_row_redacted(repo: &HookRepo, session_id: &str, token: &str) {
    let url = format!("sqlite://{}", repo.db_path().display());
    let mut opts = ConnectOptions::new(url);
    opts.sqlx_logging(false);
    let conn = Database::connect(opts).await.expect("open repo libra.db");
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT session_id, agent_kind, provider_session_id, state, working_dir, \
                    COALESCE(metadata_json, '') AS metadata_json, \
                    COALESCE(redaction_report, '') AS redaction_report \
             FROM agent_session WHERE session_id = ? LIMIT 1",
            [session_id.into()],
        ))
        .await
        .expect("query agent_session")
        .unwrap_or_else(|| panic!("no agent_session row for '{session_id}'"));

    for column in [
        "session_id",
        "agent_kind",
        "provider_session_id",
        "state",
        "working_dir",
        "metadata_json",
        "redaction_report",
    ] {
        let value: String = row.try_get_by(column).expect("read text column");
        assert!(
            !value.contains(token),
            "raw token persisted in agent_session.{column}: {value}"
        );
    }

    let report_raw: String = row
        .try_get_by("redaction_report")
        .expect("read redaction_report");
    let report: Value = serde_json::from_str(&report_raw)
        .unwrap_or_else(|err| panic!("redaction_report is not JSON ({err}): {report_raw}"));
    let matches = report["matches"]
        .as_array()
        .unwrap_or_else(|| panic!("redaction_report.matches is not an array: {report_raw}"));
    assert!(
        !matches.is_empty(),
        "redaction_report must record at least one match: {report_raw}"
    );
    assert!(
        matches
            .iter()
            .any(|m| m["rule_id"] == json!("aws-access-key-id")),
        "redaction_report must attribute the aws-access-key-id rule: {report_raw}"
    );
    assert!(
        report["bytes_redacted"].as_u64().unwrap_or(0) > 0,
        "redaction_report.bytes_redacted must be positive: {report_raw}"
    );
    drop(conn);

    // Belt and suspenders: the raw SQLite file (and its WAL, if any) must
    // not contain the token bytes anywhere — not just in the columns the
    // SELECT above named.
    let token_bytes = token.as_bytes();
    for path in [
        repo.db_path(),
        repo.db_path().with_extension("db-wal"),
        PathBuf::from(format!("{}-wal", repo.db_path().display())),
    ] {
        if let Ok(bytes) = std::fs::read(&path) {
            assert!(
                !bytes
                    .windows(token_bytes.len())
                    .any(|window| window == token_bytes),
                "raw token bytes found in {}",
                path.display()
            );
        }
    }
}

/// `prompt` verb (UserPromptSubmit → TurnStart): a secret in the
/// envelope's `prompt` field must be redacted before the session row is
/// persisted and must never appear in CLI JSON output.
#[tokio::test]
async fn raw_hook_input_is_redacted_before_persist() {
    let repo = HookRepo::init();
    let token = aws_token();
    let provider_session = "sess-redact-prompt";
    let libra_session = format!("claude__{provider_session}");

    let out = repo.hook(
        "session-start",
        &repo.envelope("SessionStart", provider_session, json!({})),
    );
    assert!(out.status.success(), "session-start: {}", describe(&out));

    let out = repo.hook(
        "prompt",
        &repo.envelope(
            "UserPromptSubmit",
            provider_session,
            json!({ "prompt": format!("deploy with access key {token} please") }),
        ),
    );
    assert!(out.status.success(), "prompt hook: {}", describe(&out));

    repo.assert_cli_json_free_of(&libra_session, &token);
    assert_persisted_row_redacted(&repo, &libra_session, &token).await;
}

/// `tool-use` verb (PostToolUse → ToolUse): a secret inside the
/// `tool_response` payload must be redacted too — AG-19 extended
/// redaction-before-persist beyond prompt/tool_input to tool_response
/// and assistant_message.
#[tokio::test]
async fn tool_response_is_redacted_too() {
    let repo = HookRepo::init();
    let token = aws_token();
    let provider_session = "sess-redact-tool-response";
    let libra_session = format!("claude__{provider_session}");

    let out = repo.hook(
        "tool-use",
        &repo.envelope(
            "PostToolUse",
            provider_session,
            json!({
                "tool_name": "Bash",
                "tool_input": { "command": "aws sts get-caller-identity" },
                "tool_response": { "output": format!("AccessKeyId: {token}") },
            }),
        ),
    );
    assert!(out.status.success(), "tool-use hook: {}", describe(&out));

    repo.assert_cli_json_free_of(&libra_session, &token);
    assert_persisted_row_redacted(&repo, &libra_session, &token).await;
}

/// `stop` verb (Stop → TurnEnd): a secret inside the envelope's
/// `last_assistant_message` field (the key `build_lifecycle_event` maps to
/// `assistant_message`) must be redacted before anything is persisted —
/// closing the fourth free-form field alongside prompt / tool_input /
/// tool_response.
#[tokio::test]
async fn assistant_message_is_redacted_too() {
    let repo = HookRepo::init();
    let token = aws_token();
    let provider_session = "sess-redact-assistant-message";
    let libra_session = format!("claude__{provider_session}");

    let out = repo.hook(
        "session-start",
        &repo.envelope("SessionStart", provider_session, json!({})),
    );
    assert!(out.status.success(), "session-start: {}", describe(&out));

    let out = repo.hook(
        "stop",
        &repo.envelope(
            "Stop",
            provider_session,
            json!({
                "last_assistant_message":
                    format!("configured the deploy with access key {token} for you"),
            }),
        ),
    );
    assert!(out.status.success(), "stop hook: {}", describe(&out));

    repo.assert_cli_json_free_of(&libra_session, &token);
    assert_persisted_row_redacted(&repo, &libra_session, &token).await;

    // The Stop verb also materialises a committed checkpoint; its CLI JSON
    // must be token-free as well.
    let list = repo.run(&["agent", "checkpoint", "list", "--json"], None);
    assert!(
        list.status.success(),
        "checkpoint list: {}",
        describe(&list)
    );
    let list_stdout = String::from_utf8_lossy(&list.stdout).to_string();
    assert!(
        !list_stdout.contains(&token),
        "raw token leaked into `agent checkpoint list --json`:\n{list_stdout}"
    );
}

/// AG-21: extractor warnings recorded in the checkpoint's `extraction`
/// metadata carry no secret, no session-owner detail and no prompt text —
/// a transcript whose broken line embeds an AWS key yields a count-only
/// warning, `partial:true`, and a metadata document free of the raw
/// values (extraction failures never block the checkpoint write).
#[tokio::test]
async fn extractor_warning_does_not_include_secret_owner_or_prompt() {
    let repo = HookRepo::init();
    let token = aws_token();
    let provider_session = "sess-extract-warning";
    let libra_session = format!("claude__{provider_session}");
    let secret_prompt = "top-secret business plan phrase";

    // Transcript inside the provider's trusted root (~/.claude), with one
    // valid user line carrying a distinctive prompt phrase and one
    // undecodable line embedding the token — extraction must degrade to a
    // count-only warning without echoing either.
    let transcript_dir = repo.home.join(".claude").join("projects").join("p");
    std::fs::create_dir_all(&transcript_dir).expect("mkdir transcript dir");
    let transcript_path = transcript_dir.join("session.jsonl");
    std::fs::write(
        &transcript_path,
        format!(
            "{}\nBROKEN {token} BROKEN\n",
            serde_json::json!({
                "type": "user",
                "uuid": "u-x",
                "message": {"role": "user", "content": secret_prompt}
            })
        ),
    )
    .expect("write transcript fixture");

    let out = repo.hook(
        "session-start",
        &repo.envelope(
            "SessionStart",
            provider_session,
            json!({ "transcript_path": transcript_path.to_string_lossy() }),
        ),
    );
    assert!(out.status.success(), "session-start: {}", describe(&out));

    // Stop materialises the committed checkpoint (metadata + extraction).
    let out = repo.hook(
        "stop",
        &repo.envelope(
            "Stop",
            provider_session,
            json!({
                "transcript_path": transcript_path.to_string_lossy(),
                "last_assistant_message": "done",
            }),
        ),
    );
    assert!(out.status.success(), "stop hook: {}", describe(&out));

    // The checkpoint's metadata (via CLI show --json) must carry the
    // extraction block with partial=true + warnings, and must NOT carry
    // the raw token or the prompt text.
    let list = repo.run(&["agent", "checkpoint", "list", "--json"], None);
    assert!(
        list.status.success(),
        "checkpoint list: {}",
        describe(&list)
    );
    let list_json: Value =
        serde_json::from_str(String::from_utf8_lossy(&list.stdout).trim()).expect("list JSON");
    let checkpoints = list_json["data"]["checkpoints"]
        .as_array()
        .or_else(|| list_json["data"].as_array())
        .unwrap_or_else(|| panic!("unexpected checkpoint list shape: {list_json}"));
    let checkpoint_id = checkpoints
        .iter()
        .find_map(|row| {
            let sid = row.get("session_id").and_then(Value::as_str)?;
            (sid == libra_session).then(|| {
                row.get("checkpoint_id")
                    .or_else(|| row.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })?
        })
        .unwrap_or_else(|| panic!("no checkpoint for {libra_session}: {list_json}"));

    let show = repo.run(
        &["agent", "checkpoint", "show", &checkpoint_id, "--json"],
        None,
    );
    assert!(
        show.status.success(),
        "checkpoint show: {}",
        describe(&show)
    );
    let show_stdout = String::from_utf8_lossy(&show.stdout).to_string();
    assert!(
        !show_stdout.contains(&token),
        "raw token leaked into checkpoint show: {show_stdout}"
    );
    assert!(
        !show_stdout.contains(secret_prompt),
        "prompt text leaked into checkpoint metadata: {show_stdout}"
    );

    let show_json: Value = serde_json::from_str(show_stdout.trim()).expect("show JSON");
    let metadata = &show_json["data"]["metadata"];
    let extraction = metadata
        .get("extraction")
        .unwrap_or_else(|| panic!("metadata missing extraction block: {metadata}"));
    assert_eq!(extraction["present"], json!(true));
    assert_eq!(
        extraction["partial"],
        json!(true),
        "undecodable transcript line must mark extraction partial: {extraction}"
    );
    let warnings = extraction["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|w| w.contains("not valid JSON"))),
        "count-only warning present: {warnings:?}"
    );
    // prompt_count is derived, but the prompt text itself must be absent
    // (asserted above) — only the count may appear.
    assert_eq!(extraction["prompt_count"], json!(1));
}

/// AG-21 security regression: a secret embedded in transcript-DERIVED
/// extraction fields (`model`, tool_use `file_path`) must be redacted in
/// the checkpoint metadata — not just the transcript blob. Codex review
/// P1 (2026-07-05): extraction strings were persisted verbatim.
#[tokio::test]
async fn extraction_derived_strings_are_redacted_in_metadata() {
    let repo = HookRepo::init();
    let token = aws_token();
    let provider_session = "sess-extract-derived";
    let libra_session = format!("claude__{provider_session}");

    // Transcript with the token hidden in BOTH a model id and a Write
    // tool's file_path (adversarial — real agents would not, but a hostile
    // or buggy transcript could).
    let transcript_dir = repo.home.join(".claude").join("projects").join("p2");
    std::fs::create_dir_all(&transcript_dir).expect("mkdir transcript dir");
    let transcript_path = transcript_dir.join("session.jsonl");
    let line = serde_json::json!({
        "type": "assistant",
        "uuid": "a-x",
        "message": {
            "role": "assistant",
            "model": format!("claude-{token}"),
            "content": [{
                "type": "tool_use",
                "name": "Write",
                "input": { "file_path": format!("secrets/{token}/out.rs") }
            }],
            "usage": { "input_tokens": 5, "output_tokens": 2 }
        }
    });
    std::fs::write(&transcript_path, format!("{line}\n")).expect("write transcript");

    let out = repo.hook(
        "session-start",
        &repo.envelope(
            "SessionStart",
            provider_session,
            json!({ "transcript_path": transcript_path.to_string_lossy() }),
        ),
    );
    assert!(out.status.success(), "session-start: {}", describe(&out));
    let out = repo.hook(
        "stop",
        &repo.envelope(
            "Stop",
            provider_session,
            json!({
                "transcript_path": transcript_path.to_string_lossy(),
                "last_assistant_message": "done",
            }),
        ),
    );
    assert!(out.status.success(), "stop hook: {}", describe(&out));

    let list = repo.run(&["agent", "checkpoint", "list", "--json"], None);
    let list_json: Value =
        serde_json::from_str(String::from_utf8_lossy(&list.stdout).trim()).expect("list JSON");
    let checkpoints = list_json["data"]["checkpoints"]
        .as_array()
        .or_else(|| list_json["data"].as_array())
        .unwrap_or_else(|| panic!("unexpected shape: {list_json}"));
    let checkpoint_id = checkpoints
        .iter()
        .find_map(|row| {
            let sid = row.get("session_id").and_then(Value::as_str)?;
            (sid == libra_session).then(|| {
                row.get("checkpoint_id")
                    .or_else(|| row.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })?
        })
        .unwrap_or_else(|| panic!("no checkpoint for {libra_session}"));

    let show = repo.run(
        &["agent", "checkpoint", "show", &checkpoint_id, "--json"],
        None,
    );
    assert!(
        show.status.success(),
        "checkpoint show: {}",
        describe(&show)
    );
    let show_stdout = String::from_utf8_lossy(&show.stdout).to_string();
    assert!(
        !show_stdout.contains(&token),
        "token leaked through an extraction-derived field: {show_stdout}"
    );
    let show_json: Value = serde_json::from_str(show_stdout.trim()).expect("show JSON");
    let extraction = &show_json["data"]["metadata"]["extraction"];
    // The fields are still present (redacted), proving they went through
    // the scrubber rather than being dropped.
    let model = extraction["model"].as_str().unwrap_or_default();
    assert!(
        model.starts_with("claude-") && !model.contains(&token),
        "model redacted: {model}"
    );
    let files = extraction["modified_files"]
        .as_array()
        .expect("files array");
    assert!(
        files
            .iter()
            .all(|f| !f.as_str().unwrap_or_default().contains(&token)),
        "file paths redacted: {files:?}"
    );
}
