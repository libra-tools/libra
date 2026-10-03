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
    str::FromStr,
};

use git_internal::hash::ObjectHash;
use libra::internal::ai::observed_agents::claude_project_slug;
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
        // Hook ingress canonicalizes its working directory before it derives
        // Claude's project slug. Keep the fixture on the same identity:
        // macOS commonly exposes its temporary root through the
        // `/var -> /private/var` alias.
        let this = Self {
            _tempdir: tempdir,
            repo: repo.canonicalize().expect("canonical repo dir"),
            home: home.canonicalize().expect("canonical fake home"),
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
        self.hook_for_agent("claude-code", verb, envelope)
    }

    fn hook_for_agent(&self, agent: &str, verb: &str, envelope: &str) -> Output {
        self.run(&["agent", "hooks", agent, verb], Some(envelope))
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

    /// Match `claude_session_dir(&verified_cwd)` under the fake home passed
    /// to the hook process as `LIBRA_TEST_HOME`.
    fn claude_transcript_path(&self, session_id: &str) -> PathBuf {
        self.home
            .join(".claude")
            .join("projects")
            .join(claude_project_slug(&self.repo))
            .join(format!("{session_id}.jsonl"))
    }

    fn untrusted_hook_pointer(&self) -> String {
        self.home
            .join("untrusted-hook-pointer.jsonl")
            .to_string_lossy()
            .into_owned()
    }

    fn db_path(&self) -> PathBuf {
        self.repo.join(".libra").join("libra.db")
    }

    fn checkpoint_id_for_session(&self, session_id: &str) -> String {
        let list = self.run(&["agent", "checkpoint", "list", "--json"], None);
        assert!(
            list.status.success(),
            "checkpoint list: {}",
            describe(&list)
        );
        let text = String::from_utf8_lossy(&list.stdout);
        let body: Value = serde_json::from_str(text.trim()).expect("checkpoint list JSON");
        body["data"]["checkpoints"]
            .as_array()
            .expect("checkpoint list array")
            .iter()
            .find(|row| row["session_id"] == json!(session_id))
            .and_then(|row| row["checkpoint_id"].as_str())
            .unwrap_or_else(|| panic!("checkpoint for {session_id} missing: {body}"))
            .to_string()
    }

    fn checkpoint_show(&self, checkpoint_id: &str) -> Value {
        let show = self.run(
            &["agent", "checkpoint", "show", checkpoint_id, "--json"],
            None,
        );
        assert!(
            show.status.success(),
            "checkpoint show: {}",
            describe(&show)
        );
        serde_json::from_slice(&show.stdout).expect("checkpoint show JSON")
    }

    /// Read a checkpoint metadata object directly for tests that validate the
    /// durable redaction invariant.  Default `checkpoint show` deliberately
    /// withholds this untrusted internal document, so these assertions must
    /// not accidentally turn its former CLI representation into a contract.
    async fn checkpoint_metadata(&self, checkpoint_id: &str) -> Value {
        let url = format!("sqlite://{}?mode=ro", self.db_path().display());
        let mut opts = ConnectOptions::new(url);
        opts.sqlx_logging(false);
        let conn = Database::connect(opts)
            .await
            .expect("open checkpoint catalog for metadata assertion");
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT metadata_blob_oid FROM agent_checkpoint WHERE checkpoint_id = ? LIMIT 1",
                [checkpoint_id.into()],
            ))
            .await
            .expect("query checkpoint metadata object")
            .unwrap_or_else(|| panic!("no checkpoint metadata row for {checkpoint_id}"));
        let oid: String = row
            .try_get_by("metadata_blob_oid")
            .expect("checkpoint metadata object id");
        drop(conn);
        let hash = ObjectHash::from_str(&oid).expect("valid checkpoint metadata object id");
        let bytes = libra::utils::object::read_git_object(&self.repo.join(".libra"), &hash)
            .expect("read checkpoint metadata object");
        serde_json::from_slice(&bytes).expect("checkpoint metadata JSON")
    }

    fn checkpoint_export(&self, checkpoint_id: &str) -> String {
        let export = self.run(&["agent", "checkpoint", "export", checkpoint_id], None);
        assert!(
            export.status.success(),
            "checkpoint export: {}",
            describe(&export)
        );
        String::from_utf8_lossy(&export.stdout).into_owned()
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

/// Read the catalog fields that make a lifecycle receipt replayable.  The
/// hook's public session JSON intentionally does not expose this private
/// recovery ledger, so this test-only observer validates the durable
/// idempotency boundary directly without widening the CLI contract.
async fn durable_session_receipts(repo: &HookRepo, session_id: &str) -> (i64, Value) {
    let mut options = ConnectOptions::new(format!("sqlite://{}?mode=ro", repo.db_path().display()));
    options.sqlx_logging(false);
    let conn = Database::connect(options)
        .await
        .expect("open catalog for receipt observation");
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT sync_revision, COALESCE(metadata_json, '{}') AS metadata_json \
             FROM agent_session WHERE session_id = ?",
            [session_id.into()],
        ))
        .await
        .expect("query catalog receipt state")
        .unwrap_or_else(|| panic!("missing session for receipt observation: {session_id}"));
    let revision = row
        .try_get_by("sync_revision")
        .expect("read catalog sync revision");
    let raw: String = row
        .try_get_by("metadata_json")
        .expect("read catalog receipt metadata");
    let metadata = serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("catalog metadata is not JSON ({error}): {raw}"));
    (revision, metadata)
}

fn receipt_entries(metadata: &Value) -> &[Value] {
    metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("capture receipt ledger missing from metadata: {metadata}"))
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
    let transcript_path = repo.claude_transcript_path(provider_session);
    let transcript_dir = transcript_path.parent().expect("transcript parent");
    std::fs::create_dir_all(transcript_dir).expect("mkdir transcript dir");
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
            json!({ "transcript_path": repo.untrusted_hook_pointer() }),
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
                "transcript_path": repo.untrusted_hook_pointer(),
                "last_assistant_message": "done",
            }),
        ),
    );
    assert!(out.status.success(), "stop hook: {}", describe(&out));

    // Default CLI show must withhold the metadata body and therefore carry
    // neither the raw token nor prompt text. The durable object is inspected
    // directly below for the extraction/redaction invariant.
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

    let metadata = repo.checkpoint_metadata(&checkpoint_id).await;
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
            .any(|w| w.as_str() == Some("transcript parser reported partial output")),
        "count-only warning present: {warnings:?}"
    );
    assert!(
        !warnings
            .iter()
            .any(|w| { w.as_str() == Some("subagent aggregate extraction reported warnings") }),
        "a parent-only parse failure must not be labeled as a subagent aggregate warning: {warnings:?}"
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
    let transcript_path = repo.claude_transcript_path(provider_session);
    let transcript_dir = transcript_path.parent().expect("transcript parent");
    std::fs::create_dir_all(transcript_dir).expect("mkdir transcript dir");
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
            json!({ "transcript_path": repo.untrusted_hook_pointer() }),
        ),
    );
    assert!(out.status.success(), "session-start: {}", describe(&out));
    let out = repo.hook(
        "stop",
        &repo.envelope(
            "Stop",
            provider_session,
            json!({
                "transcript_path": repo.untrusted_hook_pointer(),
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
    let metadata = repo.checkpoint_metadata(&checkpoint_id).await;
    let extraction = &metadata["extraction"];
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

/// ACF-03: the live snapshot crosses the durable checkpoint boundary only
/// after redaction. This exercises the real provider-root-derived source,
/// checkpoint writer, catalog, and reader rather than only inspecting an
/// in-memory snapshot projection.
#[tokio::test]
async fn snapshot_redacts_before_durable_projection() {
    let repo = HookRepo::init();
    let token = aws_token();
    let provider_session = "sess-snapshot-durable-redaction";
    let libra_session = format!("claude__{provider_session}");
    let transcript_path = repo.claude_transcript_path(provider_session);
    std::fs::create_dir_all(transcript_path.parent().expect("transcript parent"))
        .expect("create provider transcript directory");
    std::fs::write(
        &transcript_path,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "uuid": "snapshot-redaction-user",
                "message": {"role": "user", "content": format!("please use {token}")},
            })
        ),
    )
    .expect("write provider transcript");

    let start = repo.hook(
        "session-start",
        &repo.envelope("SessionStart", provider_session, json!({})),
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );
    let end = repo.hook(
        "session-end",
        &repo.envelope(
            "SessionEnd",
            provider_session,
            json!({
                "event_id": "snapshot-durable-redaction-end-v1",
                // Keep the session-level redaction report observable too;
                // the export assertions below independently prove that the
                // provider transcript crossed the durable seam redacted.
                "last_assistant_message": format!("same source secret {token}"),
            }),
        ),
    );
    assert!(end.status.success(), "session end: {}", describe(&end));

    let checkpoint_id = repo.checkpoint_id_for_session(&libra_session);
    let show = repo.checkpoint_show(&checkpoint_id);
    let show_text = show.to_string();
    let export = repo.checkpoint_export(&checkpoint_id);
    assert!(
        !show_text.contains(&token) && !export.contains(&token),
        "neither default checkpoint summary nor transcript export may reveal the source secret; \\
         show={show_text}; export={export}"
    );
    let metadata = repo.checkpoint_metadata(&checkpoint_id).await;
    let snapshot = &metadata["transcript_snapshot"];
    assert_eq!(snapshot["completeness"], json!("complete"));
    assert_eq!(snapshot["source"]["kind"], json!("provider_file"));
    assert_eq!(
        snapshot["source"]["identity"],
        json!("not_retained:v1"),
        "durable source identity must not retain a correlating source key: {snapshot}"
    );
    assert!(
        snapshot["source"]["digest_sha256"]
            .as_str()
            .is_some_and(|digest| {
                digest.strip_prefix("source/hmac-v2/").is_some_and(|hex| {
                    hex.len() == 64
                        && hex
                            .bytes()
                            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                })
            }),
        "durable source commitment must be repository-keyed HMAC v2: {snapshot}"
    );
    assert_persisted_row_redacted(&repo, &libra_session, &token).await;
}

/// ACF-03: source read failures and empty authorized sources are durable,
/// typed partial outcomes. They must not be silently represented as a
/// complete empty transcript when a terminal checkpoint falls back to the
/// redacted event projection.
#[tokio::test]
async fn snapshot_partial_reason_matrix() {
    let cases = [
        ("read-error", "source_read_error", "unreadable"),
        ("empty", "source_empty", "empty"),
        ("absent", "source_absent", "absent"),
        ("untrusted", "source_untrusted", "symlink"),
        ("oversize", "source_oversize", "oversize"),
    ];

    for (suffix, expected_reason, source_state) in cases {
        let repo = HookRepo::init();
        let provider_session = format!("sess-snapshot-partial-{suffix}");
        let libra_session = format!("claude__{provider_session}");
        let source = repo.claude_transcript_path(&provider_session);
        if source_state == "unreadable" {
            std::fs::create_dir_all(source.parent().expect("transcript parent"))
                .expect("create provider transcript directory");
            std::fs::create_dir(&source)
                .expect("use a directory where a transcript file is required");
        } else if source_state == "empty" {
            std::fs::create_dir_all(source.parent().expect("transcript parent"))
                .expect("create provider transcript directory");
            std::fs::write(&source, []).expect("write empty provider transcript");
        } else if source_state == "symlink" {
            std::fs::create_dir_all(source.parent().expect("transcript parent"))
                .expect("create provider transcript directory");
            let outside = repo.home.join("outside-transcript.jsonl");
            std::fs::write(&outside, b"outside source must not be authorized")
                .expect("write outside source fixture");
            std::os::unix::fs::symlink(outside, &source)
                .expect("link provider source outside root");
        } else if source_state == "oversize" {
            use libra::internal::ai::observed_agents::TRANSCRIPT_READ_HARD_CAP_BYTES;

            std::fs::create_dir_all(source.parent().expect("transcript parent"))
                .expect("create provider transcript directory");
            let oversize = vec![b'x'; TRANSCRIPT_READ_HARD_CAP_BYTES as usize + 1];
            std::fs::write(&source, oversize).expect("write oversize provider source");
        }

        let start = repo.hook(
            "session-start",
            &repo.envelope("SessionStart", &provider_session, json!({})),
        );
        assert!(
            start.status.success(),
            "{suffix}: session start: {}",
            describe(&start)
        );
        let end = repo.hook(
            "session-end",
            &repo.envelope(
                "SessionEnd",
                &provider_session,
                json!({ "event_id": format!("snapshot-partial-{suffix}-v1") }),
            ),
        );
        assert!(
            end.status.success(),
            "{suffix}: session end: {}",
            describe(&end)
        );
        let checkpoint_id = repo.checkpoint_id_for_session(&libra_session);
        let checkpoint = repo.checkpoint_show(&checkpoint_id);
        assert!(
            checkpoint["data"].get("metadata").is_none(),
            "default show must not expose metadata: {checkpoint}"
        );
        let metadata = repo.checkpoint_metadata(&checkpoint_id).await;
        let snapshot = &metadata["transcript_snapshot"];
        assert_eq!(
            snapshot["completeness"],
            json!("partial"),
            "{suffix}: snapshot must not claim a complete empty source: {snapshot}"
        );
        assert_eq!(
            snapshot["partial_reason"],
            json!(expected_reason),
            "{suffix}: partial cause must survive the durable projection: {snapshot}"
        );
    }
}

/// ACF-04: a catalog DML failure must prevent the checkpoint port from
/// observing the action. The trigger preserves the table so the hook passes
/// its early schema check and fails at the actual `agent_session` write
/// boundary, then the test checks the independent checkpoint table directly.
#[tokio::test]
async fn catalog_failure_leaves_no_false_checkpoint_row() {
    let repo = HookRepo::init();
    let mut options = ConnectOptions::new(format!("sqlite://{}?mode=rw", repo.db_path().display()));
    options.sqlx_logging(false);
    let conn = Database::connect(options)
        .await
        .expect("open temporary catalog database");
    conn.execute_unprepared(
        "CREATE TRIGGER fail_capture_catalog_insert BEFORE INSERT ON agent_session
         BEGIN SELECT RAISE(ABORT, 'injected capture catalog write failure'); END",
    )
    .await
    .expect("install deterministic failure at the catalog DML boundary");
    drop(conn);

    let failed = repo.hook(
        "session-end",
        &repo.envelope(
            "SessionEnd",
            "sess-catalog-prewrite-failure",
            json!({ "event_id": "catalog-prewrite-failure-v1" }),
        ),
    );
    assert!(
        !failed.status.success(),
        "catalog failure must not be acknowledged as a checkpoint success: {}",
        describe(&failed)
    );

    let conn = Database::connect(format!("sqlite://{}?mode=ro", repo.db_path().display()))
        .await
        .expect("reopen fault fixture database");
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS count FROM agent_checkpoint".to_string(),
        ))
        .await
        .expect("count checkpoint rows despite unavailable catalog")
        .expect("checkpoint count row");
    let count: i64 = row.try_get_by("count").expect("decode checkpoint count");
    assert_eq!(
        count, 0,
        "a failed catalog reservation must never leave a false checkpoint row"
    );
}

/// ACF-05: the default checkpoint reader never exposes raw source data, while
/// this test separately inspects the durable redacted payload. This pins the
/// storage façade's accepted input at its durable output boundary.
#[tokio::test]
async fn checkpoint_store_accepts_only_redacted_snapshot() {
    let repo = HookRepo::init();
    let token = aws_token();
    let provider_session = "sess-checkpoint-redacted-input";
    let libra_session = format!("claude__{provider_session}");
    let transcript_path = repo.claude_transcript_path(provider_session);
    std::fs::create_dir_all(transcript_path.parent().expect("transcript parent"))
        .expect("create provider transcript directory");
    std::fs::write(
        &transcript_path,
        format!(
            "{}\n",
            json!({
                "type": "assistant",
                "uuid": "checkpoint-redacted-assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": format!("secret {token}")}]},
            })
        ),
    )
    .expect("write secret-bearing transcript");
    let start = repo.hook(
        "session-start",
        &repo.envelope("SessionStart", provider_session, json!({})),
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );
    let stop = repo.hook(
        "stop",
        &repo.envelope(
            "Stop",
            provider_session,
            json!({ "event_id": "checkpoint-redacted-input-stop-v1" }),
        ),
    );
    assert!(stop.status.success(), "stop: {}", describe(&stop));

    let checkpoint_id = repo.checkpoint_id_for_session(&libra_session);
    let checkpoint = repo.checkpoint_show(&checkpoint_id);
    let export = repo.checkpoint_export(&checkpoint_id);
    assert!(
        !checkpoint.to_string().contains(&token) && !export.contains(&token),
        "the checkpoint store must reject raw source bytes before durable persistence; \
         checkpoint={checkpoint}; export={export}"
    );
    let metadata = repo.checkpoint_metadata(&checkpoint_id).await;
    let snapshot = &metadata["transcript_snapshot"];
    assert_eq!(snapshot["completeness"], json!("complete"));
    assert!(
        snapshot["redaction_match_count"].as_u64().unwrap_or(0) > 0,
        "the durable snapshot must retain only redaction evidence: {snapshot}"
    );
}

/// ACF-08: the shared capture foundation must retain the privacy distinction
/// between ordinary redacted content and Codex's opaque encrypted reasoning.
/// `encrypted_content` is intentionally neither decrypted nor preserved as a
/// placeholder in the generic capture projection.
#[tokio::test]
async fn capture_foundation_preserves_redaction_and_reasoning_boundaries() {
    let repo = HookRepo::init();
    let provider_session = "sess-codex-encrypted-content-boundary";
    let libra_session = format!("codex__{provider_session}");
    let encrypted_marker = "ciphertext-only-marker-6df0e9d4";
    let start = repo.hook_for_agent(
        "codex",
        "session-start",
        &repo.envelope(
            "SessionStart",
            provider_session,
            json!({ "encrypted_content": encrypted_marker }),
        ),
    );
    assert!(
        start.status.success(),
        "Codex session start: {}",
        describe(&start)
    );
    let end = repo.hook_for_agent(
        "codex",
        "session-end",
        &repo.envelope(
            "SessionEnd",
            provider_session,
            json!({
                "event_id": "codex-encrypted-content-boundary-end-v1",
                "encrypted_content": encrypted_marker,
            }),
        ),
    );
    assert!(
        end.status.success(),
        "Codex session end: {}",
        describe(&end)
    );

    let checkpoint_id = repo.checkpoint_id_for_session(&libra_session);
    let session_show = repo.run(
        &["agent", "session", "show", &libra_session, "--json"],
        None,
    );
    assert!(
        session_show.status.success(),
        "Codex session show: {}",
        describe(&session_show)
    );
    let checkpoint = repo.checkpoint_show(&checkpoint_id);
    let export = repo.checkpoint_export(&checkpoint_id);
    let visible = format!(
        "{}{}{}",
        String::from_utf8_lossy(&session_show.stdout),
        checkpoint,
        export
    );
    assert!(
        !visible.contains(encrypted_marker),
        "opaque Codex encrypted reasoning must be dropped before all durable/readable capture projections: {visible}"
    );
    let metadata = repo.checkpoint_metadata(&checkpoint_id).await;
    let snapshot = &metadata["transcript_snapshot"];
    assert_eq!(snapshot["completeness"], json!("partial"));
    assert_eq!(snapshot["partial_reason"], json!("source_absent"));
}

/// Every blob below one checkpoint's `checkpoint/<xx>/<rest>` subtree, keyed
/// by its slash-separated path, so a privacy assertion covers each durable
/// role rather than only the metadata document.
async fn checkpoint_tree_blobs(repo: &HookRepo, checkpoint_id: &str) -> Vec<(String, Vec<u8>)> {
    fn read_object(storage: &std::path::Path, oid: &str) -> Vec<u8> {
        let hash = ObjectHash::from_str(oid).expect("valid checkpoint object id");
        libra::utils::object::read_git_object(storage, &hash).expect("read checkpoint object")
    }
    fn tree_entries(tree: &[u8]) -> Vec<(bool, String, String)> {
        let mut entries = Vec::new();
        let mut cursor = 0usize;
        while cursor < tree.len() {
            let mode_end = cursor
                + tree[cursor..]
                    .iter()
                    .position(|byte| *byte == b' ')
                    .expect("tree entry mode delimiter");
            let name_end = mode_end
                + 1
                + tree[mode_end + 1..]
                    .iter()
                    .position(|byte| *byte == 0)
                    .expect("tree entry name delimiter");
            let oid_end = name_end + 1 + 20;
            assert!(oid_end <= tree.len(), "tree entry object id is truncated");
            entries.push((
                &tree[cursor..mode_end] == b"40000",
                String::from_utf8(tree[mode_end + 1..name_end].to_vec()).expect("utf-8 tree name"),
                hex::encode(&tree[name_end + 1..oid_end]),
            ));
            cursor = oid_end;
        }
        entries
    }

    let url = format!("sqlite://{}?mode=ro", repo.db_path().display());
    let mut opts = ConnectOptions::new(url);
    opts.sqlx_logging(false);
    let conn = Database::connect(opts)
        .await
        .expect("open checkpoint catalog for tree walk");
    let tree_oid: String = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT tree_oid FROM agent_checkpoint WHERE checkpoint_id = ?",
            [checkpoint_id.into()],
        ))
        .await
        .expect("query checkpoint tree")
        .unwrap_or_else(|| panic!("no checkpoint row for {checkpoint_id}"))
        .try_get_by("tree_oid")
        .expect("checkpoint tree oid");
    drop(conn);

    let storage = repo.repo.join(".libra");
    let mut oid = tree_oid;
    for component in ["checkpoint", &checkpoint_id[..2], &checkpoint_id[2..]] {
        oid = tree_entries(&read_object(&storage, &oid))
            .into_iter()
            .find(|(_, name, _)| name == component)
            .map(|(_, _, oid)| oid)
            .unwrap_or_else(|| panic!("checkpoint tree component {component} missing"));
    }
    let mut blobs = Vec::new();
    let mut pending = vec![(String::new(), oid)];
    while let Some((prefix, tree)) = pending.pop() {
        for (is_tree, name, oid) in tree_entries(&read_object(&storage, &tree)) {
            let path = format!("{prefix}{name}");
            if is_tree {
                pending.push((format!("{path}/"), oid));
            } else {
                blobs.push((path, read_object(&storage, &oid)));
            }
        }
    }
    blobs.sort();
    blobs
}

async fn session_checkpoint_ids(repo: &HookRepo, session_id: &str) -> Vec<String> {
    let url = format!("sqlite://{}?mode=ro", repo.db_path().display());
    let mut opts = ConnectOptions::new(url);
    opts.sqlx_logging(false);
    let conn = Database::connect(opts)
        .await
        .expect("open checkpoint catalog");
    conn.query_all_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT checkpoint_id FROM agent_checkpoint WHERE session_id = ? \
         ORDER BY created_at, checkpoint_id",
        [session_id.into()],
    ))
    .await
    .expect("query session checkpoints")
    .into_iter()
    .map(|row| row.try_get_by("checkpoint_id").expect("checkpoint id"))
    .collect()
}

/// ACF-08: Codex's opaque reasoning ciphertext and ordinary secrets keep one
/// privacy boundary on both entries into the capture foundation. The rollout
/// carries the ciphertext as a `reasoning` item's `encrypted_content` next to
/// a secret in user and assistant text. Live hooks never follow a
/// provider-supplied rollout pointer (Codex is source-absent), so they see
/// the same ciphertext and secret only through envelope fields; historical
/// import reads the rollout itself. Every durable checkpoint role, CLI
/// projection, catalog row and stored object must drop the ciphertext and
/// keep the secret only as a redaction placeholder.
#[tokio::test]
async fn capture_foundation_codex_reasoning_boundaries_live_and_import() {
    const REDACTED: &str = "<REDACTED:aws-access-key-id>";
    let repo = HookRepo::init();
    let token = aws_token();
    let encrypted_marker = "gAAAAB-codex-reasoning-ciphertext-7c1e52a0";
    let provider_session = "123e4567-e89b-12d3-a456-4266141740e8";
    let libra_session = format!("codex__{provider_session}");
    let rollout = repo.home.join(".codex/sessions/2026/07/15").join(format!(
        "rollout-2026-07-15T01-00-00-{provider_session}.jsonl"
    ));
    std::fs::create_dir_all(rollout.parent().expect("rollout parent"))
        .expect("create Codex sessions directory");
    let rollout_lines = [
        json!({
            "type": "session_meta", "timestamp": "2026-07-15T01:00:00Z",
            "payload": {"id": provider_session, "cwd": repo.repo}
        }),
        json!({
            "type": "response_item", "timestamp": "2026-07-15T01:00:01Z",
            "payload": {"type": "message", "role": "user", "id": "turn-reasoning-1",
                "content": [{"type": "input_text",
                    "text": format!("deploy with access key {token}")}]}
        }),
        json!({
            "type": "response_item", "timestamp": "2026-07-15T01:00:02Z",
            "payload": {"type": "reasoning", "id": "rs-reasoning-1",
                "summary": [{"type": "summary_text", "text": "planning the deploy"}],
                "content": null, "encrypted_content": encrypted_marker}
        }),
        json!({
            "type": "response_item", "timestamp": "2026-07-15T01:00:03Z",
            "payload": {"type": "message", "role": "assistant", "id": "reply-reasoning-1",
                "content": [{"type": "output_text",
                    "text": format!("configured the deploy with {token}")}]}
        }),
        json!({
            "type": "session_end", "timestamp": "2026-07-15T01:00:04Z",
            "payload": {"type": "session_end", "cwd": repo.repo}
        }),
    ];
    std::fs::write(
        &rollout,
        format!(
            "{}\n",
            rollout_lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .expect("write Codex rollout fixture");
    let rollout_pointer = rollout.to_string_lossy().into_owned();

    for (verb, event, extra) in [
        (
            "session-start",
            "SessionStart",
            json!({ "transcript_path": rollout_pointer, "encrypted_content": encrypted_marker }),
        ),
        (
            "prompt",
            "UserPromptSubmit",
            json!({
                "turn_id": "turn-reasoning-1",
                "prompt": format!("deploy with access key {token}"),
                "encrypted_content": encrypted_marker,
            }),
        ),
        (
            "stop",
            "Stop",
            json!({
                "turn_id": "turn-reasoning-1",
                "last_assistant_message": format!("configured the deploy with {token}"),
                "encrypted_content": encrypted_marker,
            }),
        ),
        (
            "session-end",
            "SessionEnd",
            json!({
                "event_id": "codex-reasoning-boundary-end-v1",
                "transcript_path": rollout_pointer,
                "encrypted_content": encrypted_marker,
            }),
        ),
    ] {
        let out = repo.hook_for_agent(
            "codex",
            verb,
            &repo.envelope(event, provider_session, extra),
        );
        assert!(out.status.success(), "Codex {verb}: {}", describe(&out));
    }
    let live_checkpoints = session_checkpoint_ids(&repo, &libra_session).await;
    assert!(
        !live_checkpoints.is_empty(),
        "live Codex hooks wrote no checkpoint"
    );

    let imported = repo.run(
        &[
            "agent",
            "import",
            "--path",
            &rollout_pointer,
            "--agent",
            "codex",
            "--yes",
            "--json",
        ],
        None,
    );
    assert!(
        imported.status.success(),
        "Codex import: {}",
        describe(&imported)
    );
    let import_checkpoints = session_checkpoint_ids(&repo, &libra_session)
        .await
        .into_iter()
        .filter(|checkpoint_id| !live_checkpoints.contains(checkpoint_id))
        .collect::<Vec<_>>();
    let import_json: Value = serde_json::from_slice(&imported.stdout).expect("import JSON");
    assert_eq!(
        import_json["data"]["results"][0]["status"],
        json!("imported")
    );
    assert_eq!(
        import_json["data"]["results"][0]["checkpoints_written"],
        json!(import_checkpoints.len()),
        "every imported turn must be checked below"
    );
    assert!(
        !import_checkpoints.is_empty(),
        "Codex import wrote no checkpoint"
    );

    let forbidden = [encrypted_marker, token.as_str()];
    let assert_free = |surface: &str, bytes: &[u8]| {
        for needle in forbidden {
            assert!(
                !bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes()),
                "{surface} retained {}: {}",
                if needle == encrypted_marker {
                    "Codex reasoning ciphertext"
                } else {
                    "a raw secret"
                },
                String::from_utf8_lossy(bytes)
            );
        }
    };
    let report_names_secret_rule = |report: &Value| {
        report["matches"].as_array().is_some_and(|matches| {
            matches
                .iter()
                .any(|matched| matched["rule_id"] == json!("aws-access-key-id"))
        })
    };

    // Live: the provider-supplied rollout pointer is never followed, so the
    // reasoning item cannot enter; the envelope secret survives only as a
    // placeholder plus redaction evidence.
    let mut live_placeholder = false;
    let mut live_secret_evidence = false;
    for checkpoint_id in &live_checkpoints {
        for (path, blob) in checkpoint_tree_blobs(&repo, checkpoint_id).await {
            assert_free(&format!("live checkpoint {path}"), &blob);
            live_placeholder |= String::from_utf8_lossy(&blob)
                .contains(&format!("configured the deploy with {REDACTED}"));
            if path == "redaction_report.json" {
                let report: Value = serde_json::from_slice(&blob).expect("live report JSON");
                live_secret_evidence |= report_names_secret_rule(&report);
            }
        }
        let snapshot = &repo.checkpoint_metadata(checkpoint_id).await["transcript_snapshot"];
        assert_eq!(snapshot["completeness"], json!("partial"));
        assert_eq!(
            snapshot["partial_reason"],
            json!("source_absent"),
            "live Codex capture must not follow the provider-supplied rollout pointer"
        );
    }
    assert!(
        live_placeholder && live_secret_evidence,
        "the live secret must be redacted in place, not silently dropped"
    );

    // Import: the rollout is read whole, but its checkpoint keeps only the
    // allowlisted turn projection with each secret replaced in place.
    for checkpoint_id in &import_checkpoints {
        let blobs = checkpoint_tree_blobs(&repo, checkpoint_id).await;
        for (path, blob) in &blobs {
            assert_free(&format!("imported checkpoint {path}"), blob);
        }
        let blob = |wanted: &str| {
            blobs
                .iter()
                .find(|(path, _)| path == wanted)
                .map(|(_, blob)| String::from_utf8_lossy(blob).into_owned())
                .unwrap_or_else(|| panic!("imported checkpoint has no {wanted}"))
        };
        let transcript = blob("transcript/codex.jsonl");
        for expected in [
            format!("deploy with access key {REDACTED}"),
            format!("configured the deploy with {REDACTED}"),
        ] {
            assert!(
                transcript.contains(&expected),
                "imported turn lost its redacted content: {transcript}"
            );
        }
        assert!(
            !transcript.contains("encrypted_content"),
            "imported turn projection carried a reasoning ciphertext field: {transcript}"
        );
        let report: Value =
            serde_json::from_str(&blob("redaction_report.json")).expect("import report JSON");
        assert!(
            report_names_secret_rule(&report),
            "imported checkpoint lacks redaction evidence: {report}"
        );
        let metadata: Value = serde_json::from_str(&blob("metadata.json")).expect("metadata");
        let snapshot = &metadata["transcript_snapshot"];
        assert_eq!(snapshot["completeness"], json!("complete"));
        assert_eq!(snapshot["source"]["kind"], json!("trusted_export"));
        assert_eq!(
            snapshot["redaction_match_count"],
            json!(2),
            "the snapshot boundary must redact both rollout secrets: {snapshot}"
        );
    }

    // Readable projections and every durable store.
    let session_show = repo.run(
        &["agent", "session", "show", &libra_session, "--json"],
        None,
    );
    assert!(
        session_show.status.success(),
        "Codex session show: {}",
        describe(&session_show)
    );
    assert_free("agent session show", &session_show.stdout);
    for checkpoint_id in live_checkpoints.iter().chain(&import_checkpoints) {
        assert_free(
            "agent checkpoint show",
            repo.checkpoint_show(checkpoint_id).to_string().as_bytes(),
        );
        assert_free(
            "agent checkpoint export",
            repo.checkpoint_export(checkpoint_id).as_bytes(),
        );
    }
    let mut options = ConnectOptions::new(format!("sqlite://{}?mode=ro", repo.db_path().display()));
    options.sqlx_logging(false);
    let conn = Database::connect(options)
        .await
        .expect("open catalog for session redaction evidence");
    let session_report: String = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT COALESCE(redaction_report, '{}') AS report FROM agent_session \
             WHERE session_id = ?",
            [libra_session.clone().into()],
        ))
        .await
        .expect("query session redaction report")
        .expect("Codex session row")
        .try_get_by("report")
        .expect("session redaction report");
    drop(conn);
    let session_report: Value =
        serde_json::from_str(&session_report).expect("session redaction report JSON");
    // An adopted live session carries the importer's closed `{ "import": .. }`
    // evidence record; a hook-only session carries the flat hook report.
    assert!(
        report_names_secret_rule(&session_report)
            || report_names_secret_rule(&session_report["import"]),
        "session row lacks redaction evidence: {session_report}"
    );
    for path in [
        repo.db_path(),
        PathBuf::from(format!("{}-wal", repo.db_path().display())),
    ] {
        if let Ok(bytes) = std::fs::read(&path) {
            assert_free("repository catalog file", &bytes);
        }
    }
    let storage = repo.repo.join(".libra");
    let mut scanned_objects = 0usize;
    for fanout in std::fs::read_dir(storage.join("objects")).expect("read object store") {
        let fanout = fanout.expect("object fan-out entry");
        let prefix = fanout.file_name().to_string_lossy().into_owned();
        if prefix.len() != 2 || !fanout.path().is_dir() {
            continue;
        }
        for object in std::fs::read_dir(fanout.path()).expect("read object fan-out") {
            let rest = object
                .expect("loose object entry")
                .file_name()
                .to_string_lossy()
                .into_owned();
            let hash = ObjectHash::from_str(&format!("{prefix}{rest}")).expect("loose object id");
            let payload =
                libra::utils::object::read_git_object(&storage, &hash).expect("read loose object");
            assert_free(&format!("object {prefix}{rest}"), &payload);
            scanned_objects += 1;
        }
    }
    assert!(
        scanned_objects > 0,
        "the object store scan inspected nothing"
    );
}

/// ACF-08: adapter-specific ingress must converge on the same capture
/// foundation. Claude contributes an authorized provider file while Codex
/// deliberately has no generic transcript source; both must still produce a
/// coherent checkpoint and typed snapshot outcome through the shared path.
#[tokio::test]
async fn capture_foundation_provider_matrix() {
    let repo = HookRepo::init();

    let claude_provider_session = "sess-foundation-provider-claude";
    let claude_session = format!("claude__{claude_provider_session}");
    let claude_source = repo.claude_transcript_path(claude_provider_session);
    std::fs::create_dir_all(claude_source.parent().expect("Claude source parent"))
        .expect("create Claude provider directory");
    std::fs::write(
        &claude_source,
        concat!(
            r#"{"type":"user","uuid":"foundation-user","message":{"role":"user","content":"capture this turn"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"foundation-assistant","message":{"role":"assistant","content":[{"type":"text","text":"captured"}]}}"#,
            "\n",
        ),
    )
    .expect("write authorized Claude transcript");
    let claude_start = repo.hook(
        "session-start",
        &repo.envelope("SessionStart", claude_provider_session, json!({})),
    );
    assert!(
        claude_start.status.success(),
        "Claude session start: {}",
        describe(&claude_start)
    );
    let claude_end = repo.hook(
        "session-end",
        &repo.envelope(
            "SessionEnd",
            claude_provider_session,
            json!({ "event_id": "foundation-claude-end-v1" }),
        ),
    );
    assert!(
        claude_end.status.success(),
        "Claude session end: {}",
        describe(&claude_end)
    );

    let codex_provider_session = "sess-foundation-provider-codex";
    let codex_session = format!("codex__{codex_provider_session}");
    let codex_start = repo.hook_for_agent(
        "codex",
        "session-start",
        &repo.envelope("SessionStart", codex_provider_session, json!({})),
    );
    assert!(
        codex_start.status.success(),
        "Codex session start: {}",
        describe(&codex_start)
    );
    let codex_end = repo.hook_for_agent(
        "codex",
        "session-end",
        &repo.envelope(
            "SessionEnd",
            codex_provider_session,
            json!({ "event_id": "foundation-codex-end-v1" }),
        ),
    );
    assert!(
        codex_end.status.success(),
        "Codex session end: {}",
        describe(&codex_end)
    );

    let claude_checkpoint_id = repo.checkpoint_id_for_session(&claude_session);
    let codex_checkpoint_id = repo.checkpoint_id_for_session(&codex_session);
    let claude_checkpoint = repo.checkpoint_show(&claude_checkpoint_id);
    let codex_checkpoint = repo.checkpoint_show(&codex_checkpoint_id);
    assert!(
        claude_checkpoint["data"].get("metadata").is_none()
            && codex_checkpoint["data"].get("metadata").is_none(),
        "default show must not expose provider-specific metadata: \\
         claude={claude_checkpoint}; codex={codex_checkpoint}"
    );
    let claude_metadata = repo.checkpoint_metadata(&claude_checkpoint_id).await;
    let codex_metadata = repo.checkpoint_metadata(&codex_checkpoint_id).await;
    let claude_snapshot = &claude_metadata["transcript_snapshot"];
    let codex_snapshot = &codex_metadata["transcript_snapshot"];
    assert_eq!(claude_snapshot["completeness"], json!("complete"));
    assert_eq!(claude_snapshot["source"]["kind"], json!("provider_file"));
    assert_eq!(codex_snapshot["completeness"], json!("partial"));
    assert_eq!(codex_snapshot["partial_reason"], json!("source_absent"));
}

/// A native TurnEnd with a *new* delivery identity can be fully covered by a
/// preceding checkpoint.  The coverage fast path may skip object/ref work,
/// but it must settle the catalog receipt it pre-reserved; otherwise every
/// replay resumes the same pending action forever and fills the bounded
/// ledger.  This deliberately uses two different `turn_id`s so the second
/// callback reaches coverage rather than being dismissed by ingress dedup.
#[tokio::test]
async fn all_covered_native_turn_end_completes_its_catalog_receipt() {
    let repo = HookRepo::init();
    let provider_session = "sess-all-covered-native-turn-end";
    let libra_session = format!("claude__{provider_session}");
    let source = repo.claude_transcript_path(provider_session);
    std::fs::create_dir_all(source.parent().expect("Claude transcript parent"))
        .expect("create Claude transcript directory");
    std::fs::write(
        &source,
        concat!(
            r#"{"type":"user","uuid":"covered-user","message":{"role":"user","content":"run it"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"covered-assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
            "\n",
        ),
    )
    .expect("write coverage-normalizable transcript");
    let start = repo.hook(
        "session-start",
        &repo.envelope("SessionStart", provider_session, json!({})),
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );

    let first = repo.envelope(
        "Stop",
        provider_session,
        json!({ "turn_id": "covered-turn-initial-v1" }),
    );
    let first_out = repo.hook("stop", &first);
    assert!(
        first_out.status.success(),
        "first TurnEnd writes the coverage baseline: {}",
        describe(&first_out)
    );
    assert!(
        !repo.checkpoint_id_for_session(&libra_session).is_empty(),
        "first covered turn must publish a checkpoint"
    );

    let (_, before_second) = durable_session_receipts(&repo, &libra_session).await;
    let entries_before = receipt_entries(&before_second).len();
    let all_covered = repo.envelope(
        "Stop",
        provider_session,
        json!({ "turn_id": "covered-turn-noop-v1" }),
    );
    let second_out = repo.hook("stop", &all_covered);
    assert!(
        second_out.status.success(),
        "all-covered TurnEnd must acknowledge its no-op: {}",
        describe(&second_out)
    );
    let checkpoints_after_noop = repo.run(&["agent", "checkpoint", "list", "--json"], None);
    assert!(
        checkpoints_after_noop.status.success(),
        "checkpoint list after all-covered TurnEnd: {}",
        describe(&checkpoints_after_noop)
    );
    let checkpoint_rows: Value = serde_json::from_slice(&checkpoints_after_noop.stdout)
        .expect("checkpoint list JSON after all-covered TurnEnd");
    assert_eq!(
        checkpoint_rows["data"]["checkpoints"]
            .as_array()
            .expect("checkpoint rows")
            .len(),
        1,
        "an all-covered TurnEnd must not append a second checkpoint"
    );
    let (revision_after_noop, metadata_after_noop) =
        durable_session_receipts(&repo, &libra_session).await;
    let entries_after_noop = receipt_entries(&metadata_after_noop);
    assert!(
        entries_after_noop.len() > entries_before,
        "the distinct native delivery must have its own durable receipt: {metadata_after_noop}"
    );
    assert!(
        entries_after_noop
            .iter()
            .all(|entry| entry["status"] == json!("complete")),
        "the all-covered fast path must settle every receipt it owns: {metadata_after_noop}"
    );

    let replay_out = repo.hook("stop", &all_covered);
    assert!(
        replay_out.status.success(),
        "identical all-covered delivery must be AlreadyApplied: {}",
        describe(&replay_out)
    );
    let (revision_after_replay, metadata_after_replay) =
        durable_session_receipts(&repo, &libra_session).await;
    assert_eq!(
        revision_after_replay, revision_after_noop,
        "AlreadyApplied replay must not mutate session revision"
    );
    assert_eq!(
        metadata_after_replay, metadata_after_noop,
        "AlreadyApplied replay must not append another pending receipt"
    );
}

/// Replace the checkpoint object directory with a regular file so that the
/// live SessionEnd producer reaches its durable recovery-artifact transaction
/// and then fails at checkpoint object publication (before the ref CAS).
fn block_checkpoint_object_directory(repo: &HookRepo) -> Option<PathBuf> {
    let objects = repo.repo.join(".libra").join("objects");
    let backup = repo.repo.join(".libra").join("objects-test-backup");
    assert!(
        !backup.exists(),
        "test object-directory backup must not already exist: {}",
        backup.display()
    );
    let moved = if objects.exists() {
        std::fs::rename(&objects, &backup).expect("move object directory aside");
        Some(backup)
    } else {
        None
    };
    std::fs::write(&objects, b"not a directory").expect("block object directory creation");
    moved
}

fn restore_checkpoint_object_directory(repo: &HookRepo, backup: Option<PathBuf>) {
    let objects = repo.repo.join(".libra").join("objects");
    std::fs::remove_file(&objects).expect("remove object-directory blocker");
    if let Some(backup) = backup {
        std::fs::rename(backup, objects).expect("restore object directory");
    }
}

/// Read every private capture-recovery row (`scope`, `key`, `value`) in a
/// stable order. Values are read as text: headers/aliases are canonical JSON
/// and chunk rows are standard base64 of the sealed envelope bytes.
async fn private_capture_rows(repo: &HookRepo) -> Vec<(String, String, String)> {
    let url = format!("sqlite://{}?mode=ro", repo.db_path().display());
    let mut opts = ConnectOptions::new(url);
    opts.sqlx_logging(false);
    let conn = Database::connect(opts)
        .await
        .expect("open repository metadata for pending artifact assertion");
    let rows = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT scope, key, CAST(value AS TEXT) AS value FROM metadata_kv \
             WHERE scope IN ('agent_capture_pending', 'agent_capture_quarantine', \
             'agent_capture_pending_chunk', 'agent_capture_session_alias') \
             ORDER BY scope, key"
                .to_string(),
        ))
        .await
        .expect("query private capture recovery rows");
    rows.into_iter()
        .map(|row| {
            (
                row.try_get_by::<String, _>("scope")
                    .expect("decode private capture scope"),
                row.try_get_by::<String, _>("key")
                    .expect("decode private capture key"),
                row.try_get_by::<String, _>("value")
                    .expect("decode private capture value"),
            )
        })
        .collect()
}

/// ACF-13: a real SessionEnd producer persists its recovery artifact before
/// the checkpoint ref CAS. Every durable private row (header, chunks, alias)
/// must contain only the redacted transcript and safe projection: no source
/// secret, no provider-native session id in the artifact control identity,
/// and no hook/catalog working directory.
#[tokio::test]
async fn pending_snapshot_contains_only_redacted_content() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let repo = HookRepo::init();
    let token = aws_token();
    let provider_session = "sess-pending-redacted-artifact";
    let libra_session = format!("claude__{provider_session}");
    let transcript_path = repo.claude_transcript_path(provider_session);
    std::fs::create_dir_all(transcript_path.parent().expect("transcript parent"))
        .expect("create provider transcript directory");
    std::fs::write(
        &transcript_path,
        format!(
            "{}\n{}\n",
            json!({
                "type": "user",
                "uuid": "pending-redaction-user",
                "message": {"role": "user", "content": format!("deploy with {token}")},
            }),
            json!({
                "type": "assistant",
                "uuid": "pending-redaction-assistant",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": format!("stored {token}")}],
                },
            }),
        ),
    )
    .expect("write provider transcript");

    let start = repo.hook(
        "session-start",
        &repo.envelope("SessionStart", provider_session, json!({})),
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );

    let terminal = repo.envelope(
        "SessionEnd",
        provider_session,
        json!({
            "event_id": "pending-redacted-artifact-end-v1",
            "last_assistant_message": format!("final answer mentions {token}"),
        }),
    );
    let blocker = block_checkpoint_object_directory(&repo);
    let failed = repo.run(
        &[
            "agent",
            "hooks",
            "--capture-budget-ms",
            "60000",
            "claude-code",
            "session-end",
        ],
        Some(&terminal),
    );
    restore_checkpoint_object_directory(&repo, blocker);
    assert!(
        !failed.status.success(),
        "blocked checkpoint publication must fail after the durable artifact prepare: {}",
        describe(&failed)
    );
    let failed_text = format!(
        "{}{}",
        String::from_utf8_lossy(&failed.stdout),
        String::from_utf8_lossy(&failed.stderr)
    );
    assert!(
        !failed_text.contains(&token),
        "hook failure output must stay content-free: {failed_text}"
    );

    let rows = private_capture_rows(&repo).await;
    let headers: Vec<_> = rows
        .iter()
        .filter(|(scope, _, _)| scope == "agent_capture_pending")
        .collect();
    assert_eq!(
        headers.len(),
        1,
        "exactly one terminal artifact header must be retained: {rows:?}"
    );
    let chunks: Vec<_> = rows
        .iter()
        .filter(|(scope, _, _)| scope == "agent_capture_pending_chunk")
        .collect();
    assert!(!chunks.is_empty(), "artifact chunks must be retained");
    let aliases: Vec<_> = rows
        .iter()
        .filter(|(scope, _, _)| scope == "agent_capture_session_alias")
        .collect();
    assert_eq!(aliases.len(), 1, "the artifact publishes exactly one alias");

    let repo_path = repo.repo.to_string_lossy().into_owned();
    // No private row may carry the source secret in any encoding layer.
    for (scope, key, value) in &rows {
        assert!(
            !value.contains(&token) && !key.contains(&token),
            "private capture row {scope}/{key} leaked the source secret"
        );
    }
    // Header and chunk rows are the artifact control identity + sealed
    // payload: they must not carry the provider-native session id or any
    // working-directory locator. (The alias registry row is the only place
    // the local catalog PK may appear, by ADR-ACF-09b.)
    let (_, _, header) = headers[0];
    assert!(
        !header.contains(provider_session) && !header.contains(&repo_path),
        "artifact header must bind only opaque identity: {header}"
    );
    let envelope_bytes: Vec<u8> = chunks
        .iter()
        .flat_map(|(_, _, value)| STANDARD.decode(value).expect("chunk is standard base64"))
        .collect();
    let envelope_text = String::from_utf8(envelope_bytes).expect("sealed envelope is UTF-8 JSON");
    for forbidden in [token.as_str(), provider_session, repo_path.as_str()] {
        assert!(
            !envelope_text.contains(forbidden),
            "sealed envelope must not contain {forbidden:?}"
        );
    }
    let envelope: Value = serde_json::from_str(&envelope_text).expect("sealed envelope JSON");
    let transcript = STANDARD
        .decode(
            envelope["payload"]["transcript"]
                .as_str()
                .expect("envelope transcript is base64 text"),
        )
        .expect("decode envelope transcript");
    let transcript = String::from_utf8(transcript).expect("redacted transcript is UTF-8");
    assert!(
        transcript.contains("<REDACTED:aws-access-key-id>") && !transcript.contains(&token),
        "the artifact must hold the redacted (not raw, not empty) transcript: {transcript}"
    );
    for pointer in ["/session_id", "/provider_session_id", "/working_dir"] {
        let slot = envelope["payload"]["metadata"].pointer(pointer);
        assert!(
            slot.is_none_or(|value| {
                !value.to_string().contains(provider_session)
                    && !value.to_string().contains(&repo_path)
            }),
            "metadata control position {pointer} must be an opaque slot: {slot:?}"
        );
    }
    let (_, _, alias_row) = aliases[0];
    assert!(
        alias_row.contains(&libra_session) && !alias_row.contains(&repo_path),
        "alias registry holds the local catalog PK only, never a path locator"
    );
}
