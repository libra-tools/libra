//! AG-19 central hook dispatcher contract (plan.md Task A4).
//!
//! Drives the built `libra` binary end-to-end: `libra init` in a tempdir,
//! then `libra agent hooks <agent> <verb>` with a JSON envelope piped via
//! stdin, asserting on exit codes, stderr hygiene, and the observable
//! `agent session list` / `agent checkpoint list` CLI JSON surfaces. The
//! behaviour under test lives in capture ingress plus the typed runtime
//! handoff:
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
    time::{Duration, Instant},
};

use chrono::Utc;
use libra::internal::{
    ai::{
        capture::{
            ingress::lower_in_process_capture_frame_for_test,
            test_support::ingest_agent_traces_ingress_outcome_for_test,
        },
        hooks::{
            HookProvider, LifecycleEvent, LifecycleEventKind, ProviderHookCommand,
            ProviderInstallOptions, SessionHookEnvelope, provider::HookProviderIdentity,
        },
        observed_agents::AgentKind,
    },
    db::migration::{builtin_migrations, builtin_runner},
};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{Value, json};
use serial_test::serial;

async fn ingest_agent_traces_payload(
    payload: &[u8],
    command: ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
    conn: &DatabaseConnection,
    repo_path: Option<&Path>,
) -> anyhow::Result<()> {
    let outcome = lower_in_process_capture_frame_for_test(
        payload,
        command,
        expected_kind,
        provider,
        repo_path,
    );
    ingest_agent_traces_ingress_outcome_for_test(outcome, command, provider, conn, repo_path).await
}

/// One isolated libra repository plus a fake `$HOME` for provider
/// transcript roots (`~/.claude`). Every test builds its own so no state
/// is shared between tests.
struct HookRepo {
    _tempdir: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

/// The three durable lifecycle columns whose relationship consumer code must
/// preserve. `agent session show` intentionally stays metadata-first, so the
/// contract tests read these columns from the repository's real SQLite store.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DurableSessionState {
    state: String,
    stopped_at: Option<i64>,
    sync_revision: i64,
    working_dir: String,
    metadata: Value,
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
            // The hook process canonicalizes its current working directory
            // before deriving the Claude project slug. Keep the fixture's
            // paths canonical too: macOS commonly exposes the temp root
            // through a `/var -> /private/var` alias.
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

    /// Run the built `libra` binary inside the repo with a clean
    /// environment.
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        self.run_in(&self.repo, args, stdin)
    }

    /// Run the built binary from a specific directory while retaining this
    /// fixture's isolated provider home. This lets scope-binding tests invoke
    /// a root worktree but claim a nested or linked worktree in the envelope.
    fn run_in(&self, workdir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
        self.run_bytes_in(workdir, args, stdin.map(str::as_bytes))
    }

    /// Byte-oriented counterpart used to exercise invalid UTF-8 and frame
    /// size rejection without lossy test-side conversion.
    fn run_bytes(&self, args: &[&str], stdin: Option<&[u8]>) -> Output {
        self.run_bytes_in(&self.repo, args, stdin)
    }

    fn command_in(&self, workdir: &Path) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.current_dir(workdir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home);
        if let Some(temp_dir) = std::env::var_os("TMPDIR") {
            cmd.env("TMPDIR", temp_dir);
        }
        cmd
    }

    fn run_bytes_in(&self, workdir: &Path, args: &[&str], stdin: Option<&[u8]>) -> Output {
        let mut cmd = self.command_in(workdir);
        cmd.args(args)
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
                .write_all(payload)
                .expect("write hook envelope to stdin");
        }
        child.wait_with_output().expect("wait for libra binary")
    }

    fn run_file_in(&self, workdir: &Path, args: &[&str], stdin_path: &Path) -> Output {
        let mut cmd = self.command_in(workdir);
        cmd.args(args)
            .stdin(Stdio::from(
                std::fs::File::open(stdin_path).expect("open fixture stdin"),
            ))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.output().expect("run libra with regular-file stdin")
    }

    /// `libra agent hooks <agent> <verb>` with `envelope` piped via stdin.
    fn hook(&self, agent: &str, verb: &str, envelope: &str) -> Output {
        self.run(&["agent", "hooks", agent, verb], Some(envelope))
    }

    /// Spawn `libra agent hooks <agent> <verb>` without waiting, stdin
    /// piped — used by the owner-race test to run two ingests
    /// concurrently.
    fn spawn_hook(&self, agent: &str, verb: &str) -> std::process::Child {
        let mut cmd = self.command_in(&self.repo);
        cmd.args(["agent", "hooks", agent, verb])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.spawn().expect("spawn libra hook handler")
    }

    /// Parsed rows of `libra agent session list --json` (AG-20 paged
    /// payload: rows under `data.sessions`).
    fn sessions(&self) -> Vec<Value> {
        self.sessions_in(&self.repo)
    }

    fn sessions_in(&self, workdir: &Path) -> Vec<Value> {
        json_data_rows(
            &self.run_in(workdir, &["agent", "session", "list", "--json"], None),
            "sessions",
        )
    }

    /// Parsed rows of `libra agent checkpoint list --json` (AG-20 paged
    /// payload: rows under `data.checkpoints`).
    fn checkpoints(&self) -> Vec<Value> {
        self.checkpoints_in(&self.repo)
    }

    fn checkpoints_in(&self, workdir: &Path) -> Vec<Value> {
        json_data_rows(
            &self.run_in(workdir, &["agent", "checkpoint", "list", "--json"], None),
            "checkpoints",
        )
    }

    fn session_show(&self, session_id: &str) -> Value {
        let out = self.run(&["agent", "session", "show", session_id, "--json"], None);
        assert!(
            out.status.success(),
            "session show failed: {}",
            describe(&out)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let parsed: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|error| panic!("session show stdout is not JSON ({error}): {stdout}"));
        assert_eq!(parsed["ok"], json!(true), "session show envelope: {parsed}");
        parsed["data"].clone()
    }

    async fn db(&self) -> DatabaseConnection {
        let mut options = ConnectOptions::new(format!(
            "sqlite://{}",
            self.repo.join(".libra").join("libra.db").display()
        ));
        options.sqlx_logging(false);
        Database::connect(options)
            .await
            .expect("open repository database")
    }

    async fn durable_session(&self, session_id: &str) -> DurableSessionState {
        let conn = self.db().await;
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, stopped_at, sync_revision, working_dir, metadata_json \
                 FROM agent_session WHERE session_id = ? LIMIT 1",
                [session_id.into()],
            ))
            .await
            .expect("query durable agent session")
            .unwrap_or_else(|| panic!("missing durable agent session {session_id}"));
        DurableSessionState {
            state: row.try_get_by("state").expect("decode state"),
            stopped_at: row.try_get_by("stopped_at").expect("decode stopped_at"),
            sync_revision: row
                .try_get_by("sync_revision")
                .expect("decode sync_revision"),
            working_dir: row.try_get_by("working_dir").expect("decode working_dir"),
            metadata: serde_json::from_str(
                &row.try_get_by::<String, _>("metadata_json")
                    .expect("decode metadata_json"),
            )
            .expect("metadata_json is valid JSON"),
        }
    }

    async fn pending_artifact_count(&self) -> i64 {
        let conn = self.db().await;
        conn.query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope IN (
               'agent_capture_pending', 'agent_capture_quarantine',
               'agent_capture_pending_chunk', 'agent_capture_session_alias'
             )"
            .to_string(),
        ))
        .await
        .expect("query pending artifact count")
        .expect("pending artifact count row")
        .try_get_by("n")
        .expect("decode pending artifact count")
    }

    /// Canonical hook envelope with the repo as `cwd`, plus extra fields.
    fn envelope(
        &self,
        hook_event_name: &str,
        session_id: &str,
        transcript_path: Option<&Path>,
        extra: Value,
    ) -> String {
        self.envelope_at(
            &self.repo,
            hook_event_name,
            session_id,
            transcript_path,
            extra,
        )
    }

    fn envelope_at(
        &self,
        cwd: &Path,
        hook_event_name: &str,
        session_id: &str,
        transcript_path: Option<&Path>,
        extra: Value,
    ) -> String {
        let mut obj = json!({
            "hook_event_name": hook_event_name,
            "session_id": session_id,
            "cwd": cwd.to_string_lossy(),
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

    /// Create a real Claude Code layout source for the supplied `(cwd,
    /// session_id)`. The runtime must discover this by the verified cwd and
    /// session id, never by a hook-supplied transcript pointer.
    fn write_claude_transcript(&self, cwd: &Path, session_id: &str, marker: &str) -> PathBuf {
        let cwd = cwd.canonicalize().expect("canonical transcript cwd");
        let slug: String = cwd
            .to_string_lossy()
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        let dir = self.home.join(".claude").join("projects").join(slug);
        std::fs::create_dir_all(&dir).expect("create ~/.claude transcript dir");
        let path = dir.join(format!("{session_id}.jsonl"));
        let transcript = [
            json!({
                "type": "user",
                "uuid": "fixture-user",
                "sessionId": session_id,
                "cwd": cwd,
                "message": {"role": "user", "content": "capture fixture"},
            }),
            json!({
                "type": "assistant",
                "uuid": "fixture-assistant",
                "sessionId": session_id,
                "cwd": cwd,
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": marker}],
                },
            }),
        ]
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        std::fs::write(&path, format!("{transcript}\n")).expect("write transcript fixture");
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

/// Provider-neutral ingress fixture for the reducer's canonical event set.
/// It uses the real capture ingress/catalog/checkpoint runtime but does not
/// rely on any provider's current hook taxonomy, so a provider adding or
/// dropping a spelling cannot leave one reducer arm unconsumed.
struct MatrixHookProvider;

impl HookProviderIdentity for MatrixHookProvider {
    fn agent_kind(&self) -> AgentKind {
        // The closed `codex` catalog kind (see `provider_name`): Codex's
        // source-absent live capture keeps this reducer matrix independent of
        // Claude coverage claims.
        AgentKind::Codex
    }
}

impl HookProvider for MatrixHookProvider {
    fn provider_name(&self) -> &'static str {
        // Reuse the closed `codex` catalog kind while exercising the
        // provider-neutral reducer. Codex's source-absent capture path keeps
        // this state-machine matrix independent of Claude coverage claims.
        "codex"
    }

    fn source_name(&self) -> &'static str {
        "lifecycle_matrix_fixture"
    }

    fn supported_commands(&self) -> &'static [ProviderHookCommand] {
        &[ProviderHookCommand::Prompt]
    }

    fn parse_hook_event(
        &self,
        hook_event_name: &str,
        envelope: &SessionHookEnvelope,
    ) -> anyhow::Result<LifecycleEvent> {
        let kind = match hook_event_name {
            "matrix-session-start" => LifecycleEventKind::SessionStart,
            "matrix-turn-start" => LifecycleEventKind::TurnStart,
            "matrix-tool-use" => LifecycleEventKind::ToolUse,
            "matrix-model-update" => LifecycleEventKind::ModelUpdate,
            "matrix-compaction" => LifecycleEventKind::Compaction,
            "matrix-compaction-completed" => LifecycleEventKind::CompactionCompleted,
            "matrix-permission-request" => LifecycleEventKind::PermissionRequest,
            "matrix-source-enabled" => LifecycleEventKind::SourceEnabled,
            "matrix-source-disabled" => LifecycleEventKind::SourceDisabled,
            "matrix-turn-end" => LifecycleEventKind::TurnEnd,
            "matrix-subagent-start" => LifecycleEventKind::SubagentStart,
            "matrix-subagent-end" => LifecycleEventKind::SubagentEnd,
            other => anyhow::bail!("unknown lifecycle matrix event {other}"),
        };
        Ok(LifecycleEvent {
            kind,
            session_id: envelope.session_id.clone(),
            session_ref: None,
            prompt: None,
            model: None,
            source: None,
            tool_name: None,
            tool_input: None,
            tool_response: None,
            assistant_message: None,
            timestamp: Utc::now(),
        })
    }

    fn recognizes_event(&self, hook_event_name: &str) -> bool {
        hook_event_name.starts_with("matrix-")
    }

    fn dedup_identity_keys(&self) -> &'static [&'static str] {
        &["event_id"]
    }

    fn install_hooks(&self, _options: &ProviderInstallOptions) -> anyhow::Result<()> {
        Ok(())
    }

    fn uninstall_hooks(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn hooks_are_installed(&self) -> anyhow::Result<bool> {
        Ok(false)
    }
}

const MATRIX_HOOK_PROVIDER: MatrixHookProvider = MatrixHookProvider;

async fn ingest_matrix_event(
    repo: &HookRepo,
    conn: &DatabaseConnection,
    session: &str,
    transcript: &Path,
    hook_event_name: &str,
    event_kind: LifecycleEventKind,
    event_id: &str,
) {
    let payload = json!({
        "hook_event_name": hook_event_name,
        "session_id": session,
        "cwd": repo.repo.to_string_lossy(),
        "transcript_path": transcript.to_string_lossy(),
        "event_id": event_id,
    })
    .to_string();
    ingest_agent_traces_payload(
        payload.as_bytes(),
        ProviderHookCommand::Prompt,
        event_kind,
        &MATRIX_HOOK_PROVIDER,
        conn,
        Some(&repo.repo),
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "matrix event {hook_event_name} ({event_kind}) must reach the capture consumer: {error:#}; debug={error:?}"
        )
    });
}

/// Every ingress failure class remains a stable LBR-AGENT-008 reject before
/// any durable mutation, and none may echo raw stdin bytes (which may contain
/// secrets) through stdout or stderr.
#[test]
fn capture_ingress_validation_matrix() {
    let repo = HookRepo::init();

    let non_json_marker = "LIBRA_TEST_GARBAGE_MARKER_93b1f2";
    let json_string_marker = "LIBRA_TEST_JSON_STRING_MARKER_f6c72a";
    let unsafe_session_marker = "LIBRA_TEST_TRAVERSAL_MARKER_51ac07";
    let path_marker = "LIBRA_TEST_PATH_MARKER_d9e7a4";
    let cwd_marker = "LIBRA_TEST_CWD_MARKER_85fcb2";
    let oversized_marker = "LIBRA_TEST_OVERSIZED_MARKER_8c4e30";
    let oversized_payload = format!(
        "{oversized_marker}{}",
        "x".repeat(libra::internal::ai::capture::ingress::MAX_STDIN_BYTES)
    )
    .into_bytes();
    let long_transcript_path = format!(
        "{path_marker}{}",
        "x".repeat(libra::internal::ai::capture::ingress::MAX_TRANSCRIPT_PATH_BYTES)
    );
    let long_reported_cwd = format!(
        "{cwd_marker}{}",
        "x".repeat(libra::internal::ai::capture::ingress::MAX_REPORTED_CWD_BYTES)
    );

    let cases = vec![
        ("empty stdin", Vec::new(), None),
        ("invalid UTF-8", vec![0xff, 0xfe, 0xfd], None),
        (
            "non-JSON stdin",
            format!("{non_json_marker} {{ this is not json").into_bytes(),
            Some(non_json_marker),
        ),
        (
            "top-level JSON string",
            serde_json::to_vec(&json!(json_string_marker)).expect("serialize JSON string"),
            Some(json_string_marker),
        ),
        (
            "typed field scalar",
            serde_json::to_vec(&json!({
                "hook_event_name": 739184026,
                "session_id": "safe-session-id",
                "cwd": "/workspace",
            }))
            .expect("serialize scalar field envelope"),
            Some("739184026"),
        ),
        (
            "unsafe session id",
            repo.envelope(
                "Stop",
                "../../x",
                None,
                json!({ "prompt": unsafe_session_marker }),
            )
            .into_bytes(),
            Some(unsafe_session_marker),
        ),
        (
            "oversized transcript path",
            repo.envelope(
                "Stop",
                "sess-ingress-long-path",
                Some(Path::new(&long_transcript_path)),
                json!({}),
            )
            .into_bytes(),
            Some(path_marker),
        ),
        (
            "oversized reported cwd",
            serde_json::to_vec(&json!({
                "hook_event_name": "Stop",
                "session_id": "sess-ingress-long-cwd",
                "cwd": long_reported_cwd,
            }))
            .expect("serialize long cwd envelope"),
            Some(cwd_marker),
        ),
        (
            "oversized frame",
            oversized_payload.clone(),
            Some(oversized_marker),
        ),
    ];

    for (case, payload, marker) in cases {
        // `libra agent enable --agent claude-code` installs this top-level
        // command, so exercise the actual production hook surface rather
        // than only the hidden diagnostic alias.
        let out = repo.run_bytes(&["hooks", "claude", "stop"], Some(&payload));
        assert!(
            !out.status.success(),
            "{case} must fail: {}",
            describe(&out)
        );
        assert_eq!(
            out.status.code(),
            Some(128),
            "{case} must preserve the envelope-reject exit status: {}",
            describe(&out)
        );
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            stderr.contains("LBR-AGENT-008"),
            "{case} must preserve LBR-AGENT-008 classification: {}",
            describe(&out)
        );
        if case == "oversized frame" {
            assert!(
                stderr.contains("hook input exceeds 1048576 bytes"),
                "pipe oversized wording must match regular-file stdin: {}",
                describe(&out)
            );
        }
        if let Some(marker) = marker {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(
                !stderr.contains(marker) && !stdout.contains(marker),
                "{case} must not echo raw stdin bytes: {}",
                describe(&out)
            );
        }
        assert!(
            repo.sessions().is_empty(),
            "{case} must not create a session row"
        );
        assert!(
            repo.checkpoints().is_empty(),
            "{case} must not create a checkpoint"
        );
    }

    let regular_file = tempfile::NamedTempFile::new().expect("create regular stdin fixture");
    std::fs::write(regular_file.path(), &oversized_payload).expect("write oversized regular stdin");
    let out = repo.run_file_in(
        &repo.repo,
        &["hooks", "claude", "stop"],
        regular_file.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "regular-file oversize: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-AGENT-008"),
        "regular-file oversize classification: {}",
        describe(&out)
    );
    assert!(
        stderr.contains("hook input exceeds 1048576 bytes"),
        "regular-file oversize text: {}",
        describe(&out)
    );
    assert!(
        !stderr.contains(oversized_marker),
        "regular-file oversize leaked payload marker: {}",
        describe(&out)
    );
}

/// A syntactically valid hook must still not be able to claim a cwd outside
/// the worktree that invoked Libra. Without this binding, catalog metadata and
/// provider transcript discovery could attribute another checkout's activity
/// to the current repository.
#[test]
fn forged_reported_cwd_is_rejected_before_capture_side_effects() {
    let repo = HookRepo::init();
    let foreign_worktree = repo.home.join("foreign-worktree");
    std::fs::create_dir_all(&foreign_worktree).expect("create foreign worktree fixture");
    let raw_marker = "LIBRA_TEST_FORGED_CWD_MARKER_6c7e9b";
    let payload = serde_json::to_string(&json!({
        "hook_event_name": "Stop",
        "session_id": "sess-forged-cwd",
        "cwd": foreign_worktree.to_string_lossy(),
        "prompt": raw_marker,
    }))
    .expect("serialize forged cwd payload");

    let out = repo.run(&["hooks", "claude", "stop"], Some(&payload));
    assert!(
        !out.status.success(),
        "forged cwd must fail: {}",
        describe(&out)
    );
    assert_eq!(out.status.code(), Some(128), "{}", describe(&out));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("LBR-AGENT-008"), "{combined}");
    assert!(
        !combined.contains(raw_marker),
        "hook payload must not be echoed: {combined}"
    );
    assert!(repo.sessions().is_empty(), "no session may be written");
    assert!(
        repo.checkpoints().is_empty(),
        "no checkpoint may be written"
    );
}

/// A child directory is still part of the invoking worktree and must retain
/// the root capture scope. This is the positive counterpart to the forged-cwd
/// reject: hooks commonly run from a package or feature subdirectory.
#[test]
fn reported_subdirectory_of_active_worktree_is_accepted() {
    let repo = HookRepo::init();
    let subdirectory = repo.repo.join("nested").join("package");
    std::fs::create_dir_all(&subdirectory).expect("create worktree subdirectory");
    let session = "sess-subdirectory-scope";

    let out = repo.hook(
        "codex",
        "stop",
        &repo.envelope_at(&subdirectory, "Stop", session, None, json!({})),
    );
    assert!(
        out.status.success(),
        "a descendant cwd must bind to the active worktree: {}",
        describe(&out)
    );
    assert!(
        repo.sessions()
            .iter()
            .any(|row| { row["session_id"] == json!(format!("codex__{session}")) }),
        "accepted child cwd must create the active-worktree session: {:?}",
        repo.sessions()
    );
    assert!(
        !repo.checkpoints().is_empty(),
        "accepted child cwd must retain normal checkpoint behavior"
    );
}

/// A nested `.libra` checkout may be physically below the active worktree but
/// is a different repository. It must not be allowed to route a root-process
/// hook into either repository's capture catalog.
#[test]
fn nested_independent_libra_repo_is_rejected_without_capture_side_effects() {
    let repo = HookRepo::init();
    let nested = repo.repo.join("nested-independent-repo");
    std::fs::create_dir_all(&nested).expect("create nested repository dir");
    let init = repo.run_in(&nested, &["init"], None);
    assert!(
        init.status.success(),
        "initialize nested independent repository: {}",
        describe(&init)
    );
    let raw_marker = "LIBRA_TEST_NESTED_SCOPE_MARKER_4c1d42";
    let payload = repo.envelope_at(
        &nested,
        "Stop",
        "sess-nested-independent-scope",
        None,
        json!({"prompt": raw_marker}),
    );

    let out = repo.hook("codex", "stop", &payload);
    assert!(
        !out.status.success(),
        "nested independent repo must be rejected: {}",
        describe(&out)
    );
    assert_eq!(out.status.code(), Some(128), "{}", describe(&out));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("LBR-AGENT-008"), "{combined}");
    assert!(
        !combined.contains(raw_marker),
        "rejection must not echo the hook payload: {combined}"
    );
    assert!(repo.sessions().is_empty(), "root catalog must remain empty");
    assert!(
        repo.checkpoints().is_empty(),
        "root checkpoint catalog must remain empty"
    );
    assert!(
        repo.sessions_in(&nested).is_empty(),
        "nested repository catalog must remain empty"
    );
    assert!(
        repo.checkpoints_in(&nested).is_empty(),
        "nested repository checkpoint catalog must remain empty"
    );
}

/// A linked sibling shares repository object storage but has its own local
/// gitdir and worktree identity. A hook launched from the main worktree must
/// reject a claimed sibling cwd instead of silently attributing it to main.
#[test]
fn sibling_linked_worktree_is_rejected_without_capture_side_effects() {
    let repo = HookRepo::init();
    let sibling = repo
        .repo
        .parent()
        .expect("repo has temporary parent")
        .join("sibling-worktree");
    let sibling_arg = sibling.to_string_lossy().into_owned();
    let added = repo.run(
        &[
            "worktree",
            "add",
            "sibling-worktree",
            "--path",
            &sibling_arg,
        ],
        None,
    );
    assert!(
        added.status.success(),
        "create sibling linked worktree: {}",
        describe(&added)
    );
    let sibling = sibling.canonicalize().expect("canonical sibling worktree");
    let raw_marker = "LIBRA_TEST_SIBLING_SCOPE_MARKER_983f75";
    let payload = repo.envelope_at(
        &sibling,
        "Stop",
        "sess-sibling-worktree-scope",
        None,
        json!({"prompt": raw_marker}),
    );

    let out = repo.hook("codex", "stop", &payload);
    assert!(
        !out.status.success(),
        "sibling linked worktree must be rejected: {}",
        describe(&out)
    );
    assert_eq!(out.status.code(), Some(128), "{}", describe(&out));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("LBR-AGENT-008"), "{combined}");
    assert!(
        !combined.contains(raw_marker),
        "rejection must not echo the hook payload: {combined}"
    );
    assert!(repo.sessions().is_empty(), "main catalog must remain empty");
    assert!(
        repo.checkpoints().is_empty(),
        "main checkpoint catalog must remain empty"
    );
    assert!(
        repo.sessions_in(&sibling).is_empty(),
        "sibling catalog must remain empty"
    );
    assert!(
        repo.checkpoints_in(&sibling).is_empty(),
        "sibling checkpoint catalog must remain empty"
    );
}

/// Claude capture derives its source from the verified worktree cwd and the
/// provider session id. A same-id file for another repository, even when a
/// hook points directly at it, must never enter checkpoint content or metadata.
#[test]
fn claude_session_end_uses_verified_source_not_hook_pointer() {
    let repo = HookRepo::init();
    let foreign_repo = repo
        .repo
        .parent()
        .expect("repo has temporary parent")
        .join("foreign-provider-repository");
    std::fs::create_dir_all(&foreign_repo).expect("create foreign repository dir");
    let foreign_init = repo.run_in(&foreign_repo, &["init"], None);
    assert!(
        foreign_init.status.success(),
        "initialize foreign repository: {}",
        describe(&foreign_init)
    );
    let foreign_repo = foreign_repo
        .canonicalize()
        .expect("canonical foreign repository");
    let session = "sess-verified-claude-source";
    let accepted_marker = "LIBRA_TEST_CLAUDE_ACCEPTED_SOURCE_486ca0";
    let forged_marker = "LIBRA_TEST_CLAUDE_FORGED_SOURCE_a7d9e1";
    repo.write_claude_transcript(&repo.repo, session, accepted_marker);
    let forged = repo.write_claude_transcript(&foreign_repo, session, forged_marker);

    let out = repo.hook(
        "claude-code",
        "session-end",
        &repo.envelope(
            "SessionEnd",
            session,
            Some(&forged),
            json!({"last_assistant_message": "hook pointer is untrusted"}),
        ),
    );
    assert!(
        out.status.success(),
        "verified Claude SessionEnd must write a checkpoint: {}",
        describe(&out)
    );
    let libra_session = format!("claude__{session}");
    let checkpoint_id = repo
        .checkpoints()
        .iter()
        .find(|row| row["session_id"] == json!(libra_session))
        .and_then(|row| row["checkpoint_id"].as_str())
        .unwrap_or_else(|| {
            panic!(
                "missing verified-source checkpoint for {libra_session}: {:?}",
                repo.checkpoints()
            )
        })
        .to_string();

    let show = repo.run(
        &["agent", "checkpoint", "show", &checkpoint_id, "--json"],
        None,
    );
    assert!(
        show.status.success(),
        "checkpoint show: {}",
        describe(&show)
    );
    let show_text = String::from_utf8_lossy(&show.stdout);
    assert!(
        !show_text.contains(forged_marker)
            && !show_text.contains(forged.to_string_lossy().as_ref()),
        "untrusted provider source leaked into checkpoint metadata: {show_text}"
    );

    let export = repo.run(&["agent", "checkpoint", "export", &checkpoint_id], None);
    assert!(
        export.status.success(),
        "redacted checkpoint export: {}",
        describe(&export)
    );
    let export_text = String::from_utf8_lossy(&export.stdout);
    assert!(
        export_text.contains(accepted_marker),
        "checkpoint must contain the redacted, verified Claude source: {export_text}"
    );
    assert!(
        !export_text.contains(forged_marker),
        "forged same-session source must not enter checkpoint content: {export_text}"
    );
}

/// Claude's provider project directory is keyed by the canonical hook cwd,
/// not always by the repository root. A hook invoked from a package directory
/// must therefore capture and later extract that directory's same-id source,
/// never the root project's decoy.
#[tokio::test]
async fn claude_subdirectory_cwd_uses_its_canonical_project_source_and_durable_identity() {
    let repo = HookRepo::init();
    let subdirectory = repo.repo.join("nested").join("package");
    std::fs::create_dir_all(&subdirectory).expect("create nested package directory");
    let subdirectory = subdirectory
        .canonicalize()
        .expect("canonical nested package directory");
    let session = "sess-claude-subdirectory-source";
    let accepted_marker = "LIBRA_TEST_CLAUDE_SUBDIRECTORY_SOURCE_5f737b";
    let root_decoy_marker = "LIBRA_TEST_CLAUDE_ROOT_DECOY_30f99c";
    let accepted_source = repo.write_claude_transcript(&subdirectory, session, accepted_marker);
    let root_decoy = repo.write_claude_transcript(&repo.repo, session, root_decoy_marker);

    let out = repo.hook(
        "claude-code",
        "session-end",
        &repo.envelope_at(
            &subdirectory,
            "SessionEnd",
            session,
            Some(&root_decoy),
            json!({"last_assistant_message": "hook pointer is untrusted"}),
        ),
    );
    assert!(
        out.status.success(),
        "nested-cwd Claude SessionEnd must write a checkpoint: {}",
        describe(&out)
    );

    let libra_session = format!("claude__{session}");
    let durable = repo.durable_session(&libra_session).await;
    assert_eq!(
        durable.working_dir,
        subdirectory.to_string_lossy(),
        "the canonical nested cwd must be the durable source identity"
    );
    let checkpoint_id = repo
        .checkpoints()
        .iter()
        .find(|row| row["session_id"] == json!(libra_session))
        .and_then(|row| row["checkpoint_id"].as_str())
        .unwrap_or_else(|| {
            panic!(
                "missing nested-cwd checkpoint for {libra_session}: {:?}",
                repo.checkpoints()
            )
        })
        .to_string();
    let export = repo.run(&["agent", "checkpoint", "export", &checkpoint_id], None);
    assert!(
        export.status.success(),
        "nested-cwd checkpoint export: {}",
        describe(&export)
    );
    let export_text = String::from_utf8_lossy(&export.stdout);
    assert!(
        export_text.contains(accepted_marker) && !export_text.contains(root_decoy_marker),
        "checkpoint must use the nested Claude source rather than the root decoy: {export_text}"
    );

    let extracted = repo.repo.join("nested-cwd-transcript-copy.jsonl");
    let extracted_arg = extracted.to_string_lossy().into_owned();
    let show = repo.run(
        &[
            "agent",
            "session",
            "show",
            &libra_session,
            "--extract-transcript",
            &extracted_arg,
            "--json",
        ],
        None,
    );
    assert!(
        show.status.success(),
        "nested-cwd transcript extraction: {}",
        describe(&show)
    );
    let show_text = String::from_utf8_lossy(&show.stdout);
    let show_json: Value = serde_json::from_str(show_text.trim()).unwrap_or_else(|error| {
        panic!("nested-cwd extraction output is not JSON ({error}): {show_text}")
    });
    let data = show_json["data"]
        .as_object()
        .expect("extraction JSON data envelope");
    let data_fields = data.keys().map(String::as_str).collect::<Vec<_>>();
    assert_eq!(
        data_fields,
        ["extracted_transcript", "session"],
        "extraction JSON must retain its safe two-field envelope: {show_text}"
    );
    assert_eq!(data["session"]["working_dir"], json!(subdirectory));
    let extraction = data["extracted_transcript"]
        .as_object()
        .expect("safe extracted_transcript object");
    let extraction_fields = extraction.keys().map(String::as_str).collect::<Vec<_>>();
    assert_eq!(
        extraction_fields,
        ["bytes", "output_path"],
        "the extraction projection must not grow beyond safe output metadata: {show_text}"
    );
    assert_eq!(
        extraction["output_path"],
        json!(extracted),
        "the requested destination is the only path exported by extraction"
    );
    assert_eq!(
        extraction["bytes"],
        json!(
            std::fs::metadata(&extracted)
                .expect("extracted transcript metadata")
                .len()
        ),
        "the extraction byte count must match the published file"
    );
    assert!(
        extraction.get("source_path").is_none(),
        "the provider source path must never be serialized: {show_text}"
    );
    assert!(
        !show_text.contains(accepted_source.to_string_lossy().as_ref()),
        "JSON output must not disclose the provider transcript locator: {show_text}"
    );
    let extracted_text = std::fs::read_to_string(&extracted).expect("read extracted transcript");
    assert!(
        extracted_text.contains(accepted_marker) && !extracted_text.contains(root_decoy_marker),
        "operator extraction must not copy the root decoy"
    );
}

/// The installed Claude entry must validate its frame before CLI preflight,
/// storage resolution, or database opening. This runs outside any Libra
/// repository so a regression to the old ordering cannot hide a malformed
/// frame behind an unrelated "not a repository" failure.
#[test]
fn invalid_ingress_is_classified_before_repository_resolution() {
    let tempdir = tempfile::tempdir().expect("create outside-repository tempdir");
    let outside = tempdir.path().join("outside");
    let home = tempdir.path().join("home");
    std::fs::create_dir_all(&outside).expect("create outside directory");
    std::fs::create_dir_all(&home).expect("create fake home");
    let marker = "LIBRA_TEST_PRECHECK_MARKER_4b5cf7";
    let payload = format!("{marker} {{ invalid json");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    cmd.args(["hooks", "claude", "stop"])
        .current_dir(outside)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("LIBRA_TEST_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn outside-repository hook");
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(payload.as_bytes())
        .expect("write malformed ingress");
    let out = child.wait_with_output().expect("wait for hook");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(out.status.code(), Some(128), "{}", describe(&out));
    assert!(
        stderr.contains("LBR-AGENT-008"),
        "invalid ingress must retain its stable classification: {}",
        describe(&out)
    );
    assert!(
        !stderr.contains(marker) && !stdout.contains(marker),
        "invalid ingress must not echo its raw frame: {}",
        describe(&out)
    );
}

/// R86 #4 fixture: a canonical working directory outside every Libra
/// repository plus an isolated provider home. Deliberately independent of
/// `HookRepo` so the frozen byte-contract oracle is unaffected.
fn outside_repository_hook_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tempdir = tempfile::tempdir().expect("create outside-repository tempdir");
    let outside = tempdir.path().join("outside");
    let home = tempdir.path().join("home");
    std::fs::create_dir_all(&outside).expect("create outside directory");
    std::fs::create_dir_all(&home).expect("create fake home");
    let outside = outside.canonicalize().expect("canonical outside directory");
    let home = home.canonicalize().expect("canonical fake home");
    (tempdir, outside, home)
}

/// Run one hook entry from `outside` with a well-formed provider frame whose
/// reported cwd is that same directory, so ingress validation succeeds and
/// only repository discovery can decide the outcome.
fn run_hook_outside_repository(
    outside: &Path,
    home: &Path,
    args: &[&str],
    hook_event_name: &str,
    marker: &str,
) -> Output {
    let payload = json!({
        "hook_event_name": hook_event_name,
        "session_id": "sess-outside-repository",
        "cwd": outside.to_string_lossy(),
        "prompt": marker,
    })
    .to_string();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    cmd.args(args)
        .current_dir(outside)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("LIBRA_TEST_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(temp_dir) = std::env::var_os("TMPDIR") {
        cmd.env("TMPDIR", temp_dir);
    }
    let mut child = cmd.spawn().expect("spawn outside-repository hook");
    // The uninstall-only gemini entries never read stdin, so the child may
    // exit before the frame is written; only a closed pipe is tolerated.
    if let Err(error) = child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(payload.as_bytes())
    {
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe,
            "write outside-repository hook frame: {error}"
        );
    }
    child
        .wait_with_output()
        .expect("wait for outside-repository hook")
}

/// R86 #4: Codex hooks are installed user-level, so they fire in every
/// trusted project, including directories that are not Libra repositories.
/// Such a callback has no scope to bind and is never a trusted terminal
/// boundary: the installed Codex surface must acknowledge it (exit 0) for
/// every event, `SessionEnd` included, on both the managed killable-helper
/// path and the unmanaged in-process path, without creating repository state.
#[test]
fn codex_hooks_outside_repository_are_acknowledged() {
    let (_tempdir, outside, home) = outside_repository_hook_dirs();
    let marker = "LIBRA_TEST_CODEX_OUTSIDE_REPOSITORY_MARKER_51e0c2";
    for (args, hook_event_name, quiet) in [
        (
            &[
                "hooks",
                "codex",
                "session-end",
                "--capture-budget-ms",
                "2000",
            ][..],
            "SessionEnd",
            true,
        ),
        (&["hooks", "codex", "session-end"][..], "SessionEnd", true),
        (
            &["hooks", "codex", "stop", "--capture-budget-ms", "29000"][..],
            "Stop",
            true,
        ),
        (
            &[
                "hooks",
                "codex",
                "session-start",
                "--capture-budget-ms",
                "29000",
            ][..],
            "SessionStart",
            false,
        ),
    ] {
        let out = run_hook_outside_repository(&outside, &home, args, hook_event_name, marker);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: Codex must acknowledge a callback outside any Libra repository: {}",
            describe(&out)
        );
        assert!(
            stdout.is_empty(),
            "{args:?}: an acknowledged Codex callback writes nothing to stdout: {}",
            describe(&out)
        );
        assert!(
            !stderr.contains(marker) && !stderr.contains("LBR-"),
            "{args:?}: the acknowledgement must not leak the frame or report an error: {}",
            describe(&out)
        );
        if quiet {
            assert!(
                stderr.is_empty(),
                "{args:?}: the acknowledged callback must stay silent: {}",
                describe(&out)
            );
        }
        assert!(
            !outside.join(".libra").exists(),
            "{args:?}: a callback outside a repository must not create repository state"
        );
    }
}

/// R86 #4: the installed Claude surface and the hidden `libra agent hooks`
/// alias are fail-closed outside a Libra repository, but they must keep the
/// shipped repository-not-found contract (`LBR-REPO-001`, exit 128, fixed and
/// path-free) rather than reporting an internal capture failure. A malformed
/// frame is still rejected first with `LBR-AGENT-008` (pinned by
/// `invalid_ingress_is_classified_before_repository_resolution`). The
/// uninstall-only gemini entries share the same shipped classification
/// outside a repository; inside one they keep their uninstall hint.
#[test]
fn fail_closed_hooks_outside_repository_report_repository_not_found() {
    let (_tempdir, outside, home) = outside_repository_hook_dirs();
    let marker = "LIBRA_TEST_CLAUDE_OUTSIDE_REPOSITORY_MARKER_8c47ad";
    let outside_text = outside.to_string_lossy().into_owned();
    for (args, hook_event_name) in [
        (
            &["hooks", "claude", "stop", "--capture-budget-ms", "9000"][..],
            "Stop",
        ),
        (&["hooks", "claude", "stop"][..], "Stop"),
        (
            &[
                "hooks",
                "claude",
                "session-end",
                "--capture-budget-ms",
                "9000",
            ][..],
            "SessionEnd",
        ),
        (&["agent", "hooks", "claude-code", "stop"][..], "Stop"),
        (
            &["agent", "hooks", "codex", "session-end"][..],
            "SessionEnd",
        ),
        (&["hooks", "gemini", "stop"][..], "Stop"),
        (
            &["agent", "hooks", "gemini", "session-end"][..],
            "SessionEnd",
        ),
    ] {
        let out = run_hook_outside_repository(&outside, &home, args, hook_event_name, marker);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            out.status.code(),
            Some(128),
            "{args:?}: a fail-closed hook outside a repository keeps exit 128: {}",
            describe(&out)
        );
        assert!(
            stderr.contains("LBR-REPO-001")
                && stderr
                    .contains("not a libra repository (or any of the parent directories): .libra")
                && !stderr.contains("LBR-INTERNAL-001")
                && !stderr.contains("capture could not be completed"),
            "{args:?}: outside-repository hooks must keep the repository-not-found contract: {}",
            describe(&out)
        );
        assert!(
            !stderr.contains(marker) && !stdout.contains(marker),
            "{args:?}: the repository-not-found error must not echo the hook frame: {}",
            describe(&out)
        );
        assert!(
            !stderr.contains(&outside_text),
            "{args:?}: the repository-not-found error must stay path-free: {}",
            describe(&out)
        );
        assert!(
            !outside.join(".libra").exists(),
            "{args:?}: a callback outside a repository must not create repository state"
        );
    }
}

/// A valid frame may reach repository setup, but a missing database must be
/// returned as an ordinary hook error rather than the legacy global-DB panic.
/// This pins the explicit-connection hash configuration path used after
/// ingress validation.
#[test]
fn valid_ingress_with_unavailable_database_fails_without_panic() {
    let repo = HookRepo::init();
    let database = repo.repo.join(".libra").join("libra.db");
    std::fs::remove_file(&database).expect("remove temporary test database");
    let out = repo.run(
        &["hooks", "claude", "stop"],
        Some(&repo.envelope("Stop", "sess-missing-db", None, json!({}))),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "unavailable database must fail the Claude hook: {}",
        describe(&out)
    );
    assert!(
        !stderr.contains("panicked at") && !stderr.contains("Failed to open database"),
        "hook must surface a contextual Result error instead of panicking: {}",
        describe(&out)
    );

    let marker = "LIBRA_TEST_CODEX_MISSING_DB_MARKER_3a9ea5";
    let codex = repo.run(
        &["hooks", "codex", "stop"],
        Some(&repo.envelope(
            "Stop",
            "sess-codex-missing-db",
            None,
            json!({ "prompt": marker }),
        )),
    );
    let codex_stderr = String::from_utf8_lossy(&codex.stderr);
    let codex_stdout = String::from_utf8_lossy(&codex.stdout);
    assert!(
        codex.status.success(),
        "Codex must acknowledge an unavailable capture database: {}",
        describe(&codex)
    );
    assert!(
        !codex_stderr.contains("panicked at")
            && !codex_stderr.contains("Failed to open database")
            && !codex_stderr.contains(marker)
            && !codex_stdout.contains(marker),
        "Codex must acknowledge the failure without panic or payload leakage: {}",
        describe(&codex)
    );
}

/// A trusted Codex SessionEnd must not turn a SQLite writer collision into a
/// successful callback or a false terminal state. The handler gets only its
/// bounded managed slice while the writer holds the catalog lock; once that
/// lock is released, redelivering the same native event must recover exactly
/// one terminal checkpoint.
#[tokio::test]
async fn codex_terminal_database_lock_is_bounded_nonzero_and_replays_once() {
    let repo = HookRepo::init();
    let session = "sess-codex-terminal-database-lock";
    let native_event_id = "codex-terminal-database-lock-v1";

    let start = repo.run(
        &[
            "hooks",
            "codex",
            "session-start",
            "--capture-budget-ms",
            "9000",
        ],
        Some(&repo.envelope("SessionStart", session, None, json!({}))),
    );
    assert!(
        start.status.success(),
        "Codex start fixture failed: {}",
        describe(&start)
    );

    let terminal = repo.envelope(
        "SessionEnd",
        session,
        None,
        json!({ "event_id": native_event_id }),
    );
    let conn = repo.db().await;
    let transaction = libra::internal::db::begin_write_transaction(&conn)
        .await
        .expect("hold a writer lock across the terminal hook");

    let started = Instant::now();
    let locked = repo.run(
        &["hooks", "codex", "session-end", "--capture-budget-ms", "20"],
        Some(&terminal),
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "managed terminal hook must not inherit SQLite's ordinary 30-second wait: {}",
        describe(&locked)
    );
    assert!(
        !locked.status.success(),
        "a trusted terminal without a durable receipt must fail closed: {}",
        describe(&locked)
    );

    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            transaction.get_database_backend(),
            "SELECT state, stopped_at FROM agent_session WHERE provider_session_id = ? LIMIT 1",
            [session.into()],
        ))
        .await
        .expect("read the session while the writer lock remains held")
        .expect("the SessionStart fixture must remain visible");
    assert_eq!(
        row.try_get_by::<String, _>("state")
            .expect("decode locked terminal state"),
        "active",
        "a lock-rejected terminal must not publish stopped state"
    );
    assert_eq!(
        row.try_get_by::<Option<i64>, _>("stopped_at")
            .expect("decode locked terminal stopped_at"),
        None,
        "a lock-rejected terminal must not publish a stop timestamp"
    );
    transaction
        .rollback()
        .await
        .expect("release writer lock before native replay");

    let replay = repo.run(
        &[
            "hooks",
            "codex",
            "session-end",
            "--capture-budget-ms",
            "9000",
        ],
        Some(&terminal),
    );
    assert!(
        replay.status.success(),
        "the same native terminal delivery must recover after lock release: {}",
        describe(&replay)
    );

    let durable = repo.durable_session(&format!("codex__{session}")).await;
    assert_eq!(durable.state, "stopped");
    assert!(
        durable.stopped_at.is_some(),
        "the recovered terminal must have one durable stop timestamp"
    );
    let conn = repo.db().await;
    let checkpoint_count = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS count FROM agent_checkpoint WHERE session_id = ?",
            [format!("codex__{session}").into()],
        ))
        .await
        .expect("count replayed terminal checkpoints")
        .expect("checkpoint count row")
        .try_get_by::<i64, _>("count")
        .expect("decode checkpoint count");
    assert_eq!(
        checkpoint_count, 1,
        "native terminal replay must recover exactly one checkpoint"
    );
}

/// Codex treats a hook callback failure as a task failure, so its installed
/// top-level surface deliberately acknowledges malformed ingress after
/// emitting a sanitized diagnostic. The acknowledgement must still be a
/// zero-side-effect path and must never leak the raw frame.
#[test]
fn installed_codex_hook_acknowledges_invalid_ingress_without_side_effect() {
    let repo = HookRepo::init();
    let marker = "LIBRA_TEST_CODEX_INVALID_INGRESS_MARKER_6d931b";
    let payload = format!("{marker} {{ this is not JSON");
    let out = repo.run_bytes(&["hooks", "codex", "stop"], Some(payload.as_bytes()));
    assert!(
        out.status.success(),
        "Codex's fail-open hook acknowledgement must exit 0: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stderr.contains(marker) && !stdout.contains(marker),
        "Codex acknowledgement must not echo the raw ingress frame: {}",
        describe(&out)
    );
    assert!(
        repo.sessions().is_empty(),
        "invalid Codex ingress must not create a session row"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "invalid Codex ingress must not create a checkpoint"
    );
}

/// A terminal callback with no bound worktree identity remains Codex-advisory
/// even when its managed budget has elapsed. It cannot safely create a
/// recovery receipt because the reported cwd was rejected before scope/key
/// binding; treating that as a terminal persistence failure would turn an
/// untrusted envelope into a task failure.
#[test]
fn codex_expired_unbound_terminal_remains_advisory() {
    let repo = HookRepo::init();
    let payload = repo.envelope_at(
        Path::new("relative-unbound-cwd"),
        "SessionEnd",
        "sess-codex-expired-unbound",
        None,
        json!({"prompt": "LIBRA_TEST_CODEX_EXPIRED_UNBOUND_MARKER"}),
    );
    let out = repo.run(
        &["hooks", "codex", "session-end", "--capture-budget-ms", "1"],
        Some(&payload),
    );
    assert!(
        out.status.success(),
        "an expired unbound Codex terminal must stay advisory: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stderr.contains("LIBRA_TEST_CODEX_EXPIRED_UNBOUND_MARKER")
            && !stdout.contains("LIBRA_TEST_CODEX_EXPIRED_UNBOUND_MARKER"),
        "the unbound terminal must not leak the raw frame: {}",
        describe(&out)
    );
    assert!(
        repo.sessions().is_empty() && repo.checkpoints().is_empty(),
        "an unbound terminal must not create session/checkpoint state"
    );
}

/// An absolute but foreign cwd gets as far as the scope helper's explicit
/// `U` proof. That proof must retain Codex's advisory terminal behavior just
/// like a lexical ingress reject; it is not enough to classify every spawned
/// helper as trusted.
#[test]
fn codex_unbound_scope_helper_phase_remains_advisory() {
    let repo = HookRepo::init();
    let foreign = repo.home.join("foreign-unbound-scope");
    std::fs::create_dir_all(&foreign).expect("create foreign unbound cwd");
    let marker = "LIBRA_TEST_CODEX_UNBOUND_SCOPE_PHASE_MARKER";
    let payload = repo.envelope_at(
        &foreign,
        "SessionEnd",
        "sess-codex-unbound-scope-phase",
        None,
        json!({"prompt": marker}),
    );
    let out = repo.run(
        &[
            "hooks",
            "codex",
            "session-end",
            "--capture-budget-ms",
            "500",
        ],
        Some(&payload),
    );
    assert!(
        out.status.success(),
        "an explicit unverified helper phase must remain advisory: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stderr.contains(marker) && !stdout.contains(marker),
        "the unbound helper phase must not leak raw terminal content: {}",
        describe(&out)
    );
    assert!(
        repo.sessions().is_empty() && repo.checkpoints().is_empty(),
        "an unbound helper phase must not create capture state"
    );
}

/// Once a Codex SessionEnd has a valid active worktree, local capture-key
/// storage is trusted infrastructure. Its failure must remain nonzero rather
/// than being folded into Codex's advisory pre-ingress acknowledgement.
#[test]
fn codex_terminal_active_capture_storage_failure_is_nonzero() {
    let repo = HookRepo::init();
    let private_dir = repo.repo.join(".libra").join("private");
    std::fs::write(&private_dir, "not a directory")
        .expect("replace capture-private directory fixture with a regular file");
    let marker = "LIBRA_TEST_CODEX_ACTIVE_STORAGE_FAILURE_MARKER";
    let out = repo.run(
        &[
            "hooks",
            "codex",
            "session-end",
            "--capture-budget-ms",
            "500",
        ],
        Some(&repo.envelope(
            "SessionEnd",
            "sess-codex-storage-failure",
            None,
            json!({"prompt": marker}),
        )),
    );
    assert!(
        !out.status.success(),
        "a trusted terminal capture-storage failure must fail closed: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stderr.contains("Codex hook ingestion failed: capture could not be completed"),
        "the active capture-storage failure must retain terminal classification: {}",
        describe(&out)
    );
    assert!(
        !stderr.contains(marker) && !stdout.contains(marker),
        "the storage failure must not leak raw terminal content: {}",
        describe(&out)
    );
}

/// The helper emits its trusted phase for an active-worktree infrastructure
/// failure before it can bind the replay key. A valid SessionEnd from a linked
/// worktree whose local `commondir` is damaged must therefore stay nonzero;
/// it must not be folded into the advisory path used for an unbound provider
/// cwd. This is the end-to-end active-infrastructure half of the phase matrix.
#[test]
fn codex_terminal_broken_active_commondir_is_nonzero() {
    let repo = HookRepo::init();
    let linked = repo
        .repo
        .parent()
        .expect("repo has temporary parent")
        .join("broken-active-commondir-worktree");
    let linked_arg = linked.to_string_lossy().into_owned();
    let added = repo.run(
        &["worktree", "add", "linked-worktree", "--path", &linked_arg],
        None,
    );
    assert!(
        added.status.success(),
        "create linked active-worktree fixture: {}",
        describe(&added)
    );
    let linked = linked.canonicalize().expect("canonical linked worktree");
    std::fs::write(
        linked.join(".libra").join("commondir"),
        "../missing-capture-storage\n",
    )
    .expect("break active linked-worktree commondir");

    let marker = "LIBRA_TEST_CODEX_BROKEN_ACTIVE_COMMONDIR_MARKER";
    let payload = repo.envelope_at(
        &linked,
        "SessionEnd",
        "sess-codex-broken-active-commondir",
        None,
        json!({"prompt": marker}),
    );
    let out = repo.run_in(
        &linked,
        &[
            "hooks",
            "codex",
            "session-end",
            "--capture-budget-ms",
            "500",
        ],
        Some(&payload),
    );
    assert!(
        !out.status.success(),
        "a damaged active worktree must fail a trusted Codex terminal: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // R88 #8: the trusted terminal keeps its non-zero exit and reports the
    // restored, path-free repository class (as every repository command
    // does) instead of the generic capture retry diagnostic.
    assert!(
        stderr.contains(
            "Codex hook ingestion failed: could not resolve the active repository storage"
        ) && stderr.contains("Error-Code: LBR-REPO-003")
            && !stderr.contains("capture could not be completed"),
        "the active commondir failure must retain terminal classification: {}",
        describe(&out)
    );
    assert!(
        !stderr.contains(marker) && !stdout.contains(marker),
        "the active commondir failure must not leak raw terminal content: {}",
        describe(&out)
    );
    assert!(
        repo.sessions().is_empty() && repo.checkpoints().is_empty(),
        "a failed active-scope binding must not create root capture state"
    );
}

/// The fail-closed hook entries checked by the R88 #8 damaged-repository
/// regressions: the installed Claude surface on both the managed
/// killable-helper path and the unmanaged in-process path, plus the hidden
/// `libra agent hooks` Claude Code and Codex aliases. Each row is
/// `(argv, context prefix)`.
const FAIL_CLOSED_DAMAGED_REPOSITORY_SURFACES: [(&[&str], &str); 4] = [
    (
        &["hooks", "claude", "stop", "--capture-budget-ms", "9000"],
        "hook ingestion failed",
    ),
    (&["hooks", "claude", "stop"], "hook ingestion failed"),
    (
        &["agent", "hooks", "claude-code", "stop"],
        "agent hook ingestion failed",
    ),
    (
        &["agent", "hooks", "codex", "stop"],
        "agent hook ingestion failed",
    ),
];

/// Assert one damaged-repository hook result keeps the restored public
/// contract: exit 128, the expected stable code and fixed remedy, never the
/// generic internal capture failure, and no path or raw stdin byte.
fn assert_restored_repository_error(
    label: &str,
    out: &Output,
    stable_code: &str,
    required: &[&str],
    forbidden: &[&str],
) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(128),
        "{label}: a damaged active repository must fail the hook with exit 128: {}",
        describe(out)
    );
    assert!(
        stderr.contains(&format!("Error-Code: {stable_code}"))
            && !stderr.contains("LBR-INTERNAL-001")
            && !stderr.contains("capture could not be completed"),
        "{label}: the hook must restore the repository preflight contract ({stable_code}): {}",
        describe(out)
    );
    for needle in required {
        assert!(
            stderr.contains(needle),
            "{label}: the restored repository error must carry {needle:?}: {}",
            describe(out)
        );
    }
    for needle in forbidden {
        assert!(
            !stderr.contains(needle) && !stdout.contains(needle),
            "{label}: the restored repository error must not render {needle:?}: {}",
            describe(out)
        );
    }
}

/// Every spelling of a fixture root that must stay out of a path-free error
/// (including the `/private` alias macOS temp roots carry).
fn path_spellings(paths: &[&Path]) -> Vec<String> {
    let mut spellings = Vec::new();
    for path in paths {
        let spelled = path.to_string_lossy().into_owned();
        if let Some(alias) = spelled.strip_prefix("/private") {
            spellings.push(alias.to_string());
        }
        spellings.push(spelled);
    }
    spellings
}

/// R88 #8: before hook dispatch became lazy, every fail-closed hook entry ran
/// the generic CLI repository preflight, so a linked worktree whose
/// `commondir` is corrupt reported `LBR-REPO-003` with its `libra worktree
/// repair --confirm` remedy — exactly what `libra status` still reports. The
/// installed Claude surface, the hidden `libra agent hooks` aliases and both
/// uninstall-only gemini entries must keep that class (path-free) rather than
/// the generic `LBR-INTERNAL-001` capture failure.
#[test]
fn fail_closed_hooks_keep_repository_state_contract_for_broken_linked_worktree() {
    let repo = HookRepo::init();
    let linked = repo
        .repo
        .parent()
        .expect("repo has temporary parent")
        .join("broken-commondir-fail-closed-worktree");
    let linked_arg = linked.to_string_lossy().into_owned();
    let added = repo.run(
        &["worktree", "add", "linked-worktree", "--path", &linked_arg],
        None,
    );
    assert!(
        added.status.success(),
        "create linked worktree fixture: {}",
        describe(&added)
    );
    let linked = linked.canonicalize().expect("canonical linked worktree");
    std::fs::write(
        linked.join(".libra").join("commondir"),
        "../missing-fail-closed-storage\n",
    )
    .expect("break linked-worktree commondir");

    let status = repo.run_in(&linked, &["status"], None);
    assert!(
        String::from_utf8_lossy(&status.stderr).contains("Error-Code: LBR-REPO-003"),
        "fixture precondition: ordinary commands report LBR-REPO-003 here: {}",
        describe(&status)
    );

    let marker = "LIBRA_TEST_BROKEN_COMMONDIR_FAIL_CLOSED_MARKER_7d21";
    let root = repo.repo.parent().expect("repo has temporary parent");
    let paths = path_spellings(&[&linked, &repo.repo, &repo.home, root]);
    let mut forbidden: Vec<&str> = paths.iter().map(String::as_str).collect();
    forbidden.extend([marker, "missing-fail-closed-storage", "commondir pointer"]);
    let remedy =
        "from the main worktree run `libra worktree repair --confirm --path <worktree-path>`";
    for (args, context) in FAIL_CLOSED_DAMAGED_REPOSITORY_SURFACES {
        let payload = repo.envelope_at(
            &linked,
            "Stop",
            "sess-broken-commondir-fail-closed",
            None,
            json!({ "prompt": marker }),
        );
        let out = repo.run_in(&linked, args, Some(&payload));
        assert_restored_repository_error(
            &format!("{args:?}"),
            &out,
            "LBR-REPO-003",
            &[
                &format!("{context}: could not resolve the active repository storage"),
                remedy,
            ],
            &forbidden,
        );
    }
    for (args, context) in [
        (&["hooks", "gemini", "stop"][..], "hook ingestion failed"),
        (
            &["agent", "hooks", "gemini", "stop"][..],
            "agent hook ingestion failed",
        ),
    ] {
        let out = repo.run_in(&linked, args, None);
        assert_restored_repository_error(
            &format!("{args:?}"),
            &out,
            "LBR-REPO-003",
            &[
                &format!("{context}: could not resolve the active repository storage"),
                remedy,
            ],
            &forbidden,
        );
    }
    // The installed Codex surface keeps its exit policy (a trusted SessionEnd
    // fails, a nonterminal callback is acknowledged) but reports the same
    // repository code and remedy instead of the generic retry diagnostic.
    for budget in [&["--capture-budget-ms", "2000"][..], &[][..]] {
        let mut args = vec!["hooks", "codex", "session-end"];
        args.extend_from_slice(budget);
        let payload = repo.envelope_at(
            &linked,
            "SessionEnd",
            "sess-broken-commondir-codex-terminal",
            None,
            json!({ "prompt": marker }),
        );
        let out = repo.run_in(&linked, &args, Some(&payload));
        assert_restored_repository_error(
            &format!("{args:?}"),
            &out,
            "LBR-REPO-003",
            &[
                "Codex hook ingestion failed: could not resolve the active repository storage",
                remedy,
            ],
            &forbidden,
        );
    }
    let nonterminal = repo.run_in(
        &linked,
        &["hooks", "codex", "stop"],
        Some(&repo.envelope_at(
            &linked,
            "Stop",
            "sess-broken-commondir-codex-nonterminal",
            None,
            json!({ "prompt": marker }),
        )),
    );
    assert!(
        nonterminal.status.success(),
        "installed Codex nonterminal callbacks stay advisory: {}",
        describe(&nonterminal)
    );
    assert!(
        repo.sessions().is_empty() && repo.checkpoints().is_empty(),
        "a damaged linked worktree must not create root capture state"
    );
}

/// R88 #8: with the repository database removed, the retired preflight
/// reported `LBR-REPO-002` (as `libra status` still does), and with a
/// database that exists but cannot be opened, `LBR-IO-001`. The fail-closed
/// hook entries restore both classes without the database path, ahead of
/// the generic internal capture failure.
#[test]
fn fail_closed_hooks_keep_repository_database_contract() {
    let repo = HookRepo::init();
    let database = repo.repo.join(".libra").join("libra.db");
    std::fs::remove_file(&database).expect("remove repository database");

    let status = repo.run(&["status"], None);
    assert!(
        String::from_utf8_lossy(&status.stderr).contains("Error-Code: LBR-REPO-002"),
        "fixture precondition: ordinary commands report LBR-REPO-002 here: {}",
        describe(&status)
    );

    let marker = "LIBRA_TEST_MISSING_DATABASE_FAIL_CLOSED_MARKER_e3b8";
    let root = repo.repo.parent().expect("repo has temporary parent");
    let paths = path_spellings(&[&repo.repo, &repo.home, root]);
    let mut forbidden: Vec<&str> = paths.iter().map(String::as_str).collect();
    forbidden.extend([marker, "libra.db", "Database file does not exist"]);
    let run_all = |code: &str, reason: &str, remedy: &str| {
        for (args, context) in FAIL_CLOSED_DAMAGED_REPOSITORY_SURFACES {
            let payload = repo.envelope(
                "Stop",
                "sess-missing-database-fail-closed",
                None,
                json!({ "prompt": marker }),
            );
            let out = repo.run(args, Some(&payload));
            assert_restored_repository_error(
                &format!("{args:?}"),
                &out,
                code,
                &[&format!("{context}: {reason}"), remedy],
                &forbidden,
            );
        }
        for (args, context) in [
            (&["hooks", "gemini", "stop"][..], "hook ingestion failed"),
            (
                &["agent", "hooks", "gemini", "session-end"][..],
                "agent hook ingestion failed",
            ),
        ] {
            let out = repo.run(args, None);
            assert_restored_repository_error(
                &format!("{args:?}"),
                &out,
                code,
                &[&format!("{context}: {reason}"), remedy],
                &forbidden,
            );
        }
    };
    run_all(
        "LBR-REPO-002",
        "repository database not found",
        "restore the repository's .libra storage",
    );

    // A database path that exists but cannot be opened as SQLite.
    std::fs::create_dir(&database).expect("replace database with a directory");
    run_all(
        "LBR-IO-001",
        "could not open the repository database",
        "otherwise restore repository storage, then retry the hook",
    );
}

/// Every schema object's definition plus every table column of the database,
/// so a migration's physical effect is observable independent of receipts.
async fn physical_repository_schema(conn: &DatabaseConnection) -> Vec<String> {
    let agent_kind_check = regex::Regex::new(r"CHECK\(`agent_kind` IN \(([^)]*)\)\)")
        .expect("fixed agent_kind CHECK pattern");
    conn.query_all_raw(Statement::from_string(
        conn.get_database_backend(),
        "SELECT type || ':' || name || ':' || COALESCE(sql, '') FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' \
         UNION ALL \
         SELECT 'column:' || m.name || '.' || p.name \
         FROM sqlite_master AS m, pragma_table_info(m.name) AS p WHERE m.type = 'table' \
         UNION ALL \
         SELECT 'agent-kind-grok:' || (instr(sql, '''grok''') > 0) FROM sqlite_master \
         WHERE type = 'table' AND name = 'agent_session' \
         ORDER BY 1",
    ))
    .await
    .expect("read physical repository schema")
    .into_iter()
    .map(|row| {
        // SQLite's table rebuild quotes the renamed table with double quotes;
        // fresh init uses backticks and a multiline closed agent-kind list.
        // Normalize those spellings while retaining every CHECK value.
        let definition = row.try_get_by_index::<String>(0)
            .expect("decode schema object")
            .replacen(
                "table:agent_session:CREATE TABLE \"agent_session\"",
                "table:agent_session:CREATE TABLE `agent_session`",
                1,
            );
        agent_kind_check
            .replace_all(&definition, |captures: &regex::Captures<'_>| {
                format!(
                    "CHECK(`agent_kind` IN ({}))",
                    captures[1].split_whitespace().collect::<String>()
                )
            })
            .into_owned()
    })
    .collect()
}

/// Every `schema_versions` receipt (version, name and timestamp).
async fn repository_schema_receipts(conn: &DatabaseConnection) -> Vec<String> {
    conn.query_all_raw(Statement::from_string(
        conn.get_database_backend(),
        "SELECT version || ':' || name || ':' || applied_at FROM schema_versions ORDER BY version",
    ))
    .await
    .expect("read schema receipts")
    .into_iter()
    .map(|row| {
        row.try_get_by_index::<String>(0)
            .expect("decode schema receipt")
    })
    .collect()
}

async fn max_repository_schema_receipt(conn: &DatabaseConnection) -> Option<i64> {
    conn.query_one_raw(Statement::from_string(
        conn.get_database_backend(),
        "SELECT MAX(version) FROM schema_versions",
    ))
    .await
    .expect("query newest schema receipt")
    .expect("newest schema receipt row")
    .try_get_by_index(0)
    .expect("decode newest schema receipt")
}

async fn agent_session_rows(conn: &DatabaseConnection) -> i64 {
    conn.query_one_raw(Statement::from_string(
        conn.get_database_backend(),
        "SELECT COUNT(*) FROM agent_session",
    ))
    .await
    .expect("count agent sessions")
    .expect("agent session count row")
    .try_get_by_index(0)
    .expect("decode agent session count")
}

fn latest_repository_schema_version() -> i64 {
    builtin_runner()
        .expect("built-in migration registry")
        .max_registered_version()
        .expect("at least one built-in migration")
}

/// Physical schema of a freshly initialized repository and of the same
/// disposable repository after its latest reversible migration was rolled back.
struct StaleSchemaFixture {
    current: Vec<String>,
    stale: Vec<String>,
}

/// A disposable repository with the two newest reversible migrations (the
/// `pi` and `hermes` CHECKs) rolled back, both forward-only receipts and their
/// empty physical state removed, then the next older reversible migration
/// rolled back. This models an older
/// repository without asking the production rollback API to undo a
/// compatibility barrier.
async fn stale_hook_repo() -> (HookRepo, StaleSchemaFixture) {
    let repo = HookRepo::init();
    let migrations = builtin_migrations();
    let reversible = migrations.last().expect("at least one built-in migration");
    assert!(
        reversible.down.is_some() && reversible.name == "agent_kind_pi",
        "the disposable fixture must explicitly model the latest reversible migration"
    );
    let hermes = migrations.iter().rev().nth(1).expect("hermes CHECK");
    assert!(
        hermes.down.is_some() && hermes.name == "agent_kind_hermes",
        "the disposable fixture must explicitly model the reversible hermes CHECK"
    );
    let newest = migrations
        .iter()
        .rev()
        .nth(2)
        .expect("session-commit links");
    assert!(
        newest.down.is_none() && newest.name == "agent_session_commit",
        "the disposable fixture must explicitly model the forward-only session links"
    );
    let layout = migrations.iter().rev().nth(3).expect("layout barrier");
    assert!(layout.down.is_none() && layout.name == "worktree_layout_capability");
    let previous = migrations
        .iter()
        .rev()
        .nth(5)
        .map(|migration| migration.version)
        .expect("an older reversible migration");
    let runner = builtin_runner().expect("built-in migration registry");
    let conn = repo.db().await;
    assert_eq!(
        max_repository_schema_receipt(&conn).await,
        Some(latest_repository_schema_version()),
        "fixture precondition: init writes the current schema"
    );
    let current = physical_repository_schema(&conn).await;
    // `rollback_to` refuses any irreversible migration in its range, so the
    // reversible migrations above the forward-only pair go first.
    runner
        .rollback_to(&conn, newest.version)
        .await
        .expect("roll the newest reversible migrations back");
    assert_eq!(reversible.version, latest_repository_schema_version());
    // This fixture is disposable and has no session-commit links. Remove both
    // forward-only migrations' physical effects and receipts before exercising
    // the older reversible schema migration.
    conn.execute_unprepared("DROP TABLE agent_session_commit")
        .await
        .expect("remove disposable empty session-commit table");
    conn.execute_unprepared("DELETE FROM worktree_registry_capability WHERE version = 4")
        .await
        .expect("remove disposable layout capability marker");
    conn.execute_unprepared(&format!(
        "DELETE FROM schema_versions WHERE version IN ({}, {})",
        newest.version, layout.version
    ))
    .await
    .expect("remove disposable forward-only receipts");
    runner
        .rollback_to(&conn, previous)
        .await
        .expect("roll the older reversible migration back");
    assert_eq!(
        max_repository_schema_receipt(&conn).await,
        Some(previous),
        "fixture precondition: the forward-only receipts are gone"
    );
    let stale = physical_repository_schema(&conn).await;
    assert_ne!(
        stale, current,
        "fixture precondition: the newest migration's physical effect is gone"
    );
    conn.close().await.expect("close stale-schema fixture");
    (repo, StaleSchemaFixture { current, stale })
}

/// R89: managed hook callbacks open the repository database through the
/// same migrate-and-fence path as every repository command (and as the
/// retired hook preflight did): a repository last migrated by an older Libra
/// is brought up to date before capture, instead of being written under a
/// stale schema or failing with the generic retry diagnostic. Each surface
/// gets its own stale repository; every problem is collected so a regression
/// names all affected entries at once.
#[tokio::test]
async fn hooks_apply_pending_repository_migrations_before_capture() {
    let latest = latest_repository_schema_version();
    let session = "sess-stale-schema-hook";
    let mut problems = Vec::new();
    for (args, hook_event_name) in [
        (
            &[
                "hooks",
                "claude",
                "session-start",
                "--capture-budget-ms",
                "9000",
            ][..],
            "SessionStart",
        ),
        (
            &["hooks", "claude", "stop", "--capture-budget-ms", "9000"][..],
            "Stop",
        ),
        (
            &[
                "agent",
                "hooks",
                "--capture-budget-ms",
                "9000",
                "claude-code",
                "session-start",
            ][..],
            "SessionStart",
        ),
        (
            &[
                "hooks",
                "codex",
                "session-start",
                "--capture-budget-ms",
                "29000",
            ][..],
            "SessionStart",
        ),
        (
            &[
                "hooks",
                "codex",
                "session-end",
                "--capture-budget-ms",
                "2000",
            ][..],
            "SessionEnd",
        ),
        (&["hooks", "gemini", "stop"][..], ""),
        (&["agent", "hooks", "gemini", "session-end"][..], ""),
    ] {
        let (repo, fixture) = stale_hook_repo().await;
        let gemini = hook_event_name.is_empty();
        let out = if gemini {
            repo.run(args, None)
        } else {
            let transcript = (!args.contains(&"codex"))
                .then(|| repo.write_claude_transcript(&repo.repo, session, "stale schema fixture"));
            let payload = repo.envelope(hook_event_name, session, transcript.as_deref(), json!({}));
            repo.run(args, Some(&payload))
        };
        let stderr = String::from_utf8_lossy(&out.stderr);
        if gemini {
            if out.status.success()
                || !stderr.contains("gemini hook ingestion is disabled")
                || !stderr.contains("libra agent remove gemini")
                || stderr.contains("LBR-IO-001")
            {
                problems.push(format!(
                    "{args:?}: a healthy stale repository must reach the uninstall hint: {}",
                    describe(&out)
                ));
            }
        } else if !out.status.success() {
            problems.push(format!(
                "{args:?}: a stale repository schema must be migrated, not fail the hook: {}",
                describe(&out)
            ));
        }
        let conn = repo.db().await;
        let receipt = max_repository_schema_receipt(&conn).await;
        if receipt != Some(latest) {
            problems.push(format!(
                "{args:?}: the hook must apply pending repository migrations \
                 (newest receipt {receipt:?}, expected {latest})"
            ));
        }
        // The newest migration's physical effect is re-applied, and nothing
        // outside a freshly initialized repository's schema appears.
        let migrated = physical_repository_schema(&conn).await;
        if migrated == fixture.stale {
            problems.push(format!(
                "{args:?}: the pending migration's physical effect must be re-applied"
            ));
        }
        if let Some(alien) = migrated.iter().find(|item| !fixture.current.contains(item)) {
            problems.push(format!(
                "{args:?}: the migrated schema must stay within the current schema, found {alien}"
            ));
        }
        if !gemini && agent_session_rows(&conn).await == 0 {
            problems.push(format!(
                "{args:?}: capture must complete on the migrated schema"
            ));
        }
        conn.close().await.expect("close migrated repository");
    }
    assert!(
        problems.is_empty(),
        "stale repository schemas must be migrated by every hook entry:\n{}",
        problems.join("\n")
    );
}

/// A freshly initialized repository whose schema ledger carries one receipt
/// from a newer Libra. Returns the repository, the newer version, and the
/// receipts plus physical schema that every refusal must leave untouched.
async fn future_schema_hook_repo() -> (HookRepo, i64, Vec<String>, Vec<String>) {
    let repo = HookRepo::init();
    let future = latest_repository_schema_version() + 1;
    let conn = repo.db().await;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO schema_versions (version, name, applied_at) VALUES (?, 'future', 'fixture')",
        [future.into()],
    ))
    .await
    .expect("record a newer-Libra schema receipt");
    let receipts = repository_schema_receipts(&conn).await;
    let schema = physical_repository_schema(&conn).await;
    conn.close().await.expect("close future-schema fixture");
    (repo, future, receipts, schema)
}

const FUTURE_SCHEMA_REMEDY: &str =
    "install a newer Libra binary; otherwise restore repository storage, then retry the hook";
const FUTURE_SCHEMA_MARKER: &str = "LIBRA_TEST_FUTURE_SCHEMA_HOOK_MARKER_7c1d";

/// Strings no newer-schema refusal may render: every fixture path spelling,
/// the raw stdin marker, the database file name and the schema versions or
/// the underlying migration error text.
fn future_schema_forbidden(repo: &HookRepo, future: i64) -> Vec<String> {
    let root = repo.repo.parent().expect("repo has temporary parent");
    let mut forbidden = path_spellings(&[&repo.repo, &repo.home, root]);
    forbidden.extend(
        [
            FUTURE_SCHEMA_MARKER,
            "libra.db",
            "schema version",
            "newer than this Libra binary supports",
        ]
        .map(str::to_string),
    );
    forbidden.push(future.to_string());
    forbidden.push((future - 1).to_string());
    forbidden
}

/// Run fail-closed hook entries against a newer-Libra schema and require the
/// restored path-free `LBR-IO-001` newer-binary contract from each.
fn assert_future_schema_refused(
    repo: &HookRepo,
    future: i64,
    surfaces: &[(&[&str], &str, Option<&str>)],
) {
    let forbidden = future_schema_forbidden(repo, future);
    let forbidden: Vec<&str> = forbidden.iter().map(String::as_str).collect();
    for (args, context, hook_event_name) in surfaces {
        let payload = hook_event_name.map(|name| {
            repo.envelope(
                name,
                "sess-future-schema-fail-closed",
                None,
                json!({ "prompt": FUTURE_SCHEMA_MARKER }),
            )
        });
        let out = repo.run(args, payload.as_deref());
        assert_restored_repository_error(
            &format!("{args:?}"),
            &out,
            "LBR-IO-001",
            &[
                &format!("{context}: could not open the repository database"),
                FUTURE_SCHEMA_REMEDY,
            ],
            &forbidden,
        );
    }
}

/// The refused repository keeps every receipt and schema object, and no hook
/// captured a session under the unknown schema.
async fn assert_future_schema_untouched(repo: &HookRepo, receipts: &[String], schema: &[String]) {
    let conn = repo.db().await;
    assert_eq!(
        repository_schema_receipts(&conn).await,
        receipts,
        "a refused newer-Libra schema must keep every receipt"
    );
    assert_eq!(
        physical_repository_schema(&conn).await,
        schema,
        "a refused newer-Libra schema must not be migrated or rebuilt"
    );
    assert_eq!(
        agent_session_rows(&conn).await,
        0,
        "no hook may capture under a schema written by a newer Libra"
    );
    conn.close().await.expect("close future-schema repository");
}

/// R89: a repository database whose schema was written by a newer Libra is
/// refused by the installed Claude surface and the hidden `libra agent
/// hooks` alias before any capture write — on the managed
/// (`--capture-budget-ms`) path as well as the unmanaged one — with the
/// path-free `LBR-IO-001` newer-binary remedy the retired preflight gave,
/// never the generic capture failure, a path, a version number or raw stdin.
#[tokio::test]
async fn fail_closed_hooks_refuse_a_newer_libra_repository_schema() {
    let (repo, future, receipts, schema) = future_schema_hook_repo().await;
    let mut surfaces: Vec<(&[&str], &str, Option<&str>)> = vec![(
        &[
            "agent",
            "hooks",
            "--capture-budget-ms",
            "9000",
            "claude-code",
            "stop",
        ],
        "agent hook ingestion failed",
        Some("Stop"),
    )];
    surfaces.extend(
        FAIL_CLOSED_DAMAGED_REPOSITORY_SURFACES
            .iter()
            .map(|(args, context)| (*args, *context, Some("Stop"))),
    );
    assert_future_schema_refused(&repo, future, &surfaces);
    assert_future_schema_untouched(&repo, &receipts, &schema).await;
}

/// R89: the installed Codex surface keeps its exit policy against a
/// newer-Libra schema: a trusted managed `SessionEnd` fails with the same
/// path-free `LBR-IO-001` code and remedy, and a nonterminal callback stays
/// advisory (exit `0`) without capturing or leaking anything.
#[tokio::test]
async fn codex_hooks_refuse_a_newer_libra_repository_schema() {
    let (repo, future, receipts, schema) = future_schema_hook_repo().await;
    assert_future_schema_refused(
        &repo,
        future,
        &[
            (
                &[
                    "hooks",
                    "codex",
                    "session-end",
                    "--capture-budget-ms",
                    "2000",
                ],
                "Codex hook ingestion failed",
                Some("SessionEnd"),
            ),
            (
                &["hooks", "codex", "session-end"],
                "Codex hook ingestion failed",
                Some("SessionEnd"),
            ),
        ],
    );
    let nonterminal = repo.run(
        &["hooks", "codex", "stop", "--capture-budget-ms", "29000"],
        Some(&repo.envelope(
            "Stop",
            "sess-future-schema-codex-nonterminal",
            None,
            json!({ "prompt": FUTURE_SCHEMA_MARKER }),
        )),
    );
    assert_eq!(
        nonterminal.status.code(),
        Some(0),
        "installed Codex nonterminal callbacks stay advisory: {}",
        describe(&nonterminal)
    );
    let stderr = String::from_utf8_lossy(&nonterminal.stderr);
    let stdout = String::from_utf8_lossy(&nonterminal.stdout);
    for needle in future_schema_forbidden(&repo, future) {
        assert!(
            !stderr.contains(&needle) && !stdout.contains(&needle),
            "the advisory Codex callback must not render {needle:?}: {}",
            describe(&nonterminal)
        );
    }
    assert_future_schema_untouched(&repo, &receipts, &schema).await;
}

/// R89: the uninstall-only gemini entries restore the retired preflight's
/// newer-schema refusal (`LBR-IO-001`, path-free) instead of the uninstall
/// hint, exactly as the other damaged-repository classes.
#[tokio::test]
async fn gemini_hooks_refuse_a_newer_libra_repository_schema() {
    let (repo, future, receipts, schema) = future_schema_hook_repo().await;
    assert_future_schema_refused(
        &repo,
        future,
        &[
            (&["hooks", "gemini", "stop"], "hook ingestion failed", None),
            (
                &["agent", "hooks", "gemini", "session-end"],
                "agent hook ingestion failed",
                None,
            ),
        ],
    );
    assert_future_schema_untouched(&repo, &receipts, &schema).await;
}

/// First-writer-wins: once claude_code has claimed provider session `S`,
/// a codex `stop` for the same `S` is skipped (exit 0) — no codex row,
/// no extra checkpoint. (Codex is the second provider here because gemini
/// is uninstall-only and its hook entries reject before ingest.)
#[test]
fn owner_claim_prevents_duplicate_checkpoint() {
    let repo = HookRepo::init();
    let session = "sess-owner-claim";
    let transcript = repo.write_claude_transcript(&repo.repo, session, "owner fixture");

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
/// non-owner (the first inserted `rowid` wins) stay skipped. Codex is the
/// second provider because
/// gemini's hook entries are uninstall-only and reject before ingest.
#[tokio::test]
async fn session_start_exempt_allows_second_provider_claim_row() {
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
    assert!(
        kinds.contains(&"codex".to_string()),
        "exempt codex SessionStart must create its own row rather than be silently skipped: {kinds:?}"
    );

    // The owner fence applies only to checkpoint-producing/non-exempt
    // actions. A second provider's TurnStart remains explicitly accepted:
    // prove it changed the codex row, rather than merely returning the
    // success status used by a fenced skip.
    let codex_session = format!("codex__{session}");
    let codex_before = repo.durable_session(&codex_session).await;
    let out = repo.hook(
        "codex",
        "prompt",
        &repo.envelope(
            "UserPromptSubmit",
            session,
            None,
            json!({ "prompt": "exempt turn start", "turn_id": "exempt-turn-start-v1" }),
        ),
    );
    assert!(
        out.status.success(),
        "codex TurnStart must remain exempt from owner fencing: {}",
        describe(&out)
    );
    let codex_after = repo.durable_session(&codex_session).await;
    assert_eq!(codex_after.state, "active");
    assert!(
        codex_after.sync_revision > codex_before.sync_revision,
        "accepted exempt TurnStart must mutate the codex row instead of taking owner-skip fast path"
    );

    // The point to pin: codex's NON-exempt stop is still skipped — the
    // owner is the first inserted row — so it must exit 0 and write no
    // checkpoint and not
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

    // The later exempt row must not reverse ownership. The original
    // rowid-first Claude owner still gets to publish a checkpoint-producing
    // action after the Codex loser was fenced; otherwise a broad foreign-row
    // guard would silently deadlock the legitimate owner too.
    let claude_session = format!("claude__{session}");
    let claude_before = repo.durable_session(&claude_session).await;
    let codex_after_fenced_stop = repo.durable_session(&codex_session).await;
    let out = repo.hook(
        "claude-code",
        "stop",
        &repo.envelope("Stop", session, None, json!({})),
    );
    assert!(
        out.status.success(),
        "first-row Claude owner must checkpoint after fenced Codex stop: {}",
        describe(&out)
    );
    let claude_after = repo.durable_session(&claude_session).await;
    assert!(
        claude_after.sync_revision > claude_before.sync_revision,
        "owner checkpoint action must advance only the Claude row"
    );
    assert_eq!(
        repo.checkpoints()
            .into_iter()
            .filter(|row| row["session_id"] == json!(claude_session))
            .count(),
        1,
        "the original owner must write exactly one checkpoint"
    );
    assert_eq!(
        repo.durable_session(&codex_session).await,
        codex_after_fenced_stop,
        "Claude owner checkpoint must not mutate the later exempt Codex row"
    );
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
/// upsert (first inserted `rowid` wins) and the
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

/// The first-writer owner fence applies to every non-exempt lifecycle
/// mutation, not merely checkpoint-producing actions. ToolUse and Compaction
/// write only session state, but letting a second provider apply either as a
/// first event would create a second owner row (or revise the elected row)
/// before any checkpoint fence could intervene. SessionStart/TurnStart keep
/// their separate explicit exemption coverage above.
#[tokio::test]
async fn simultaneous_nonexempt_state_only_events_fence_loser_before_any_mutation() {
    let repo = HookRepo::init();
    let cases = [
        ("tool-use", "PreToolUse", "PreToolUse", "active"),
        ("compaction", "Compaction", "PreCompact", "condensed"),
    ];

    for (label, claude_event, codex_event, expected_state) in cases {
        for attempt in 0..3 {
            let session = format!("sess-owner-race-{label}-state-only-{attempt}");
            let claude_envelope = repo.envelope(
                claude_event,
                &session,
                None,
                json!({ "event_id": format!("{label}-claude-{attempt}-v1") }),
            );
            let codex_envelope = repo.envelope(
                codex_event,
                &session,
                None,
                json!({ "event_id": format!("{label}-codex-{attempt}-v1") }),
            );
            let mut claude_child = repo.spawn_hook("claude-code", label);
            let mut codex_child = repo.spawn_hook("codex", label);
            claude_child
                .stdin
                .take()
                .expect("Claude stdin piped")
                .write_all(claude_envelope.as_bytes())
                .expect("write Claude first state-only envelope");
            codex_child
                .stdin
                .take()
                .expect("Codex stdin piped")
                .write_all(codex_envelope.as_bytes())
                .expect("write Codex first state-only envelope");
            let claude_out = claude_child
                .wait_with_output()
                .expect("wait for Claude first state-only event");
            let codex_out = codex_child
                .wait_with_output()
                .expect("wait for Codex first state-only event");
            assert!(
                claude_out.status.success(),
                "{label} attempt {attempt}: Claude handler must safely apply/skip: {}",
                describe(&claude_out)
            );
            assert!(
                codex_out.status.success(),
                "{label} attempt {attempt}: Codex handler must safely apply/skip: {}",
                describe(&codex_out)
            );

            let conn = repo.db().await;
            let rows = conn
                .query_all_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "SELECT agent_kind, state, sync_revision, metadata_json \
                     FROM agent_session WHERE provider_session_id = ? ORDER BY rowid",
                    [session.clone().into()],
                ))
                .await
                .expect("query raced state-only rows");
            assert_eq!(
                rows.len(),
                1,
                "{label} attempt {attempt}: loser must create no provider row"
            );
            let state: String = rows[0].try_get_by("state").expect("state");
            let revision: i64 = rows[0].try_get_by("sync_revision").expect("revision");
            let metadata_json: String = rows[0].try_get_by("metadata_json").expect("metadata");
            let metadata: Value =
                serde_json::from_str(&metadata_json).expect("state-only metadata JSON");
            assert_eq!(
                state, expected_state,
                "{label} event kind must drive winner state"
            );
            assert_eq!(
                revision, 1,
                "{label} attempt {attempt}: loser must not revise the elected row"
            );
            assert_eq!(
                metadata["capture_catalog_receipts_v1"]["entries"]
                    .as_array()
                    .map_or(0, Vec::len),
                1,
                "{label} attempt {attempt}: loser must not append a second receipt"
            );
            drop(conn);
            assert!(
                repo.checkpoints().into_iter().all(|row| !row["session_id"]
                    .as_str()
                    .is_some_and(|id| id.ends_with(&session))),
                "{label} is state-only and must not create a checkpoint"
            );
        }
    }
}

/// Terminal owner-race regression: two adapters may race a fresh underlying
/// provider session, but catalog ownership fencing must reject the loser
/// before it can create an `agent_session`, a pending terminal receipt, or a
/// finalizer record.  Replaying the losing native SessionEnd afterwards is a
/// safe acknowledgement only; it must leave the winner byte-for-byte intact.
#[tokio::test]
async fn simultaneous_session_end_race_fences_loser_before_receipt_write() {
    let repo = HookRepo::init();

    for attempt in 0..5 {
        let session = format!("sess-owner-race-terminal-{attempt}");
        let native_event_id = format!("owner-race-session-end-{attempt}-v1");
        let claude_envelope = repo.envelope(
            "SessionEnd",
            &session,
            None,
            json!({ "event_id": native_event_id }),
        );
        let codex_envelope = repo.envelope(
            "SessionEnd",
            &session,
            None,
            json!({ "event_id": native_event_id }),
        );

        let mut claude_child = repo.spawn_hook("claude-code", "session-end");
        let mut codex_child = repo.spawn_hook("codex", "session-end");
        claude_child
            .stdin
            .take()
            .expect("Claude stdin piped")
            .write_all(claude_envelope.as_bytes())
            .expect("write Claude SessionEnd envelope");
        codex_child
            .stdin
            .take()
            .expect("Codex stdin piped")
            .write_all(codex_envelope.as_bytes())
            .expect("write Codex SessionEnd envelope");
        let claude_out = claude_child
            .wait_with_output()
            .expect("wait for Claude SessionEnd");
        let codex_out = codex_child
            .wait_with_output()
            .expect("wait for Codex SessionEnd");
        assert!(
            claude_out.status.success(),
            "attempt {attempt}: fenced Claude SessionEnd acknowledges safely: {}",
            describe(&claude_out)
        );
        assert!(
            codex_out.status.success(),
            "attempt {attempt}: fenced Codex SessionEnd acknowledges safely: {}",
            describe(&codex_out)
        );

        let conn = repo.db().await;
        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT session_id, agent_kind, state, stopped_at, sync_revision, metadata_json \
                 FROM agent_session WHERE provider_session_id = ? ORDER BY rowid",
                [session.clone().into()],
            ))
            .await
            .expect("query raced terminal session rows");
        assert_eq!(
            rows.len(),
            1,
            "attempt {attempt}: terminal loser must not leave a session row"
        );
        let winner_session_id: String = rows[0].try_get_by("session_id").expect("session ID");
        let winner_kind: String = rows[0].try_get_by("agent_kind").expect("agent kind");
        let winner_state: String = rows[0].try_get_by("state").expect("state");
        let winner_stopped_at: Option<i64> = rows[0].try_get_by("stopped_at").expect("stopped");
        let winner_revision: i64 = rows[0].try_get_by("sync_revision").expect("revision");
        let winner_metadata_raw: String =
            rows[0].try_get_by("metadata_json").expect("metadata JSON");
        let winner_metadata: Value =
            serde_json::from_str(&winner_metadata_raw).expect("winner metadata is valid JSON");
        assert_eq!(
            winner_state, "stopped",
            "attempt {attempt}: only a durably completed winner may publish terminal state"
        );
        assert!(
            winner_stopped_at.is_some(),
            "attempt {attempt}: terminal state must retain its stopped timestamp"
        );
        let receipts = winner_metadata["capture_catalog_receipts_v1"]["entries"]
            .as_array()
            .expect("native winner SessionEnd has a receipt ledger");
        assert_eq!(
            receipts.len(),
            1,
            "attempt {attempt}: losing terminal race must not add a second receipt/finalizer"
        );
        assert_eq!(
            receipts[0]["status"],
            json!("complete"),
            "attempt {attempt}: the sole terminal receipt must be settled, not pending"
        );
        drop(conn);

        let winner_checkpoints = repo
            .checkpoints()
            .into_iter()
            .filter(|row| row["session_id"] == json!(winner_session_id))
            .collect::<Vec<_>>();
        assert_eq!(
            winner_checkpoints.len(),
            1,
            "attempt {attempt}: terminal winner must have exactly one checkpoint"
        );
        let (loser_agent, loser_envelope) = match winner_kind.as_str() {
            "claude_code" => ("codex", &codex_envelope),
            "codex" => ("claude-code", &claude_envelope),
            other => panic!("unexpected raced owner kind {other}"),
        };

        // Repeated delivery of the losing native SessionEnd is intentionally
        // harmless. It must stay outside catalog/finalizer mutation entirely.
        for replay_attempt in 0..2 {
            let loser_replay = repo.hook(loser_agent, "session-end", loser_envelope);
            assert!(
                loser_replay.status.success(),
                "attempt {attempt}/{replay_attempt}: fenced loser replay must acknowledge: {}",
                describe(&loser_replay)
            );
            let conn = repo.db().await;
            let after_rows = conn
                .query_all_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "SELECT session_id, state, stopped_at, sync_revision, metadata_json \
                     FROM agent_session WHERE provider_session_id = ? ORDER BY rowid",
                    [session.clone().into()],
                ))
                .await
                .expect("query terminal race after loser replay");
            assert_eq!(
                after_rows.len(),
                1,
                "attempt {attempt}/{replay_attempt}: loser replay must not create a row"
            );
            assert_eq!(
                after_rows[0]
                    .try_get_by::<String, _>("session_id")
                    .expect("session ID after replay"),
                winner_session_id,
                "attempt {attempt}/{replay_attempt}: loser replay must not replace owner"
            );
            assert_eq!(
                after_rows[0]
                    .try_get_by::<String, _>("state")
                    .expect("state after replay"),
                winner_state,
                "attempt {attempt}/{replay_attempt}: loser replay must not alter terminal state"
            );
            assert_eq!(
                after_rows[0]
                    .try_get_by::<Option<i64>, _>("stopped_at")
                    .expect("stopped_at after replay"),
                winner_stopped_at,
                "attempt {attempt}/{replay_attempt}: loser replay must not alter terminal timestamp"
            );
            assert_eq!(
                after_rows[0]
                    .try_get_by::<i64, _>("sync_revision")
                    .expect("revision after replay"),
                winner_revision,
                "attempt {attempt}/{replay_attempt}: loser replay must not mutate revision"
            );
            assert_eq!(
                after_rows[0]
                    .try_get_by::<String, _>("metadata_json")
                    .expect("metadata after replay"),
                winner_metadata_raw,
                "attempt {attempt}/{replay_attempt}: loser replay must not grow receipt/finalizer evidence"
            );
            drop(conn);
            assert_eq!(
                repo.checkpoints()
                    .into_iter()
                    .filter(|row| row["session_id"] == json!(winner_session_id))
                    .count(),
                1,
                "attempt {attempt}/{replay_attempt}: loser replay must not append a checkpoint"
            );
        }
    }
}

/// A duplicate delivery from the *same* provider owns the same receipt, not
/// a second terminal writer. Both processes may reach the pending receipt
/// concurrently, but the catalog must elect one marker/source pair and the
/// eventual replay may only finish that attempt — never append a second
/// checkpoint or quarantine the session as a false marker takeover.
#[tokio::test]
async fn simultaneous_same_provider_session_end_uses_one_terminal_attempt() {
    let repo = HookRepo::init();
    let session = "sess-same-provider-terminal-race";
    let start = repo.hook(
        "claude-code",
        "session-start",
        &repo.envelope("SessionStart", session, None, json!({})),
    );
    assert!(
        start.status.success(),
        "same-provider race setup must create its session: {}",
        describe(&start)
    );

    let terminal = repo.envelope(
        "SessionEnd",
        session,
        None,
        json!({ "event_id": "same-provider-terminal-race-v1" }),
    );
    // More contenders than the bounded finalizer retry budget exercise the
    // live-marker path: duplicate deliveries must be observed as in-flight,
    // not as five retryable failures that quarantine the elected writer.
    let mut children = (0..6)
        .map(|_| repo.spawn_hook("claude-code", "session-end"))
        .collect::<Vec<_>>();
    for child in &mut children {
        child
            .stdin
            .take()
            .expect("same-provider SessionEnd stdin piped")
            .write_all(terminal.as_bytes())
            .expect("write same-provider SessionEnd envelope");
    }
    let outputs = children
        .into_iter()
        .map(|child| {
            child
                .wait_with_output()
                .expect("wait for SessionEnd contender")
        })
        .collect::<Vec<_>>();
    let output_summary = outputs.iter().map(describe).collect::<Vec<_>>().join(" | ");

    // Once both concurrent handlers have returned, their shared native
    // receipt must already be terminal. A live-marker contender may report a
    // retryable foreground error, but it cannot leave a second pending writer
    // whose later delivery would be required to repair the race.
    let before_replay = repo.durable_session(&format!("claude__{session}")).await;
    assert_eq!(
        before_replay.state, "stopped",
        "concurrent terminal delivery must finish one elected attempt; contenders={output_summary}",
    );
    assert_eq!(
        repo.checkpoints()
            .into_iter()
            .filter(|row| row["session_id"] == json!(format!("claude__{session}")))
            .count(),
        1,
        "all same-native contenders must append exactly one checkpoint before replay"
    );
    let conn = repo.db().await;
    let trace_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT c.traces_commit, r.\"commit\" AS ref_commit
             FROM agent_checkpoint c
             JOIN reference r ON r.name = 'traces'
             WHERE c.session_id = ? LIMIT 1",
            [format!("claude__{session}").into()],
        ))
        .await
        .expect("read terminal checkpoint and traces ref")
        .expect("one terminal traces commit");
    let checkpoint_commit: String = trace_row
        .try_get_by("traces_commit")
        .expect("decode terminal checkpoint traces commit");
    let traces_head_before_replay: String = trace_row
        .try_get_by("ref_commit")
        .expect("decode traces ref head");
    assert_eq!(
        traces_head_before_replay, checkpoint_commit,
        "a hidden duplicate append would advance refs/libra/traces beyond the sole catalog checkpoint"
    );
    drop(conn);

    // The same native identity is then an idempotent acknowledgement.
    let settle = repo.hook("claude-code", "session-end", &terminal);
    assert!(
        settle.status.success(),
        "same native replay must settle the elected writer attempt; contenders={output_summary}, replay={}",
        describe(&settle)
    );
    let durable = repo.durable_session(&format!("claude__{session}")).await;
    assert_eq!(
        durable, before_replay,
        "native replay must not mutate the settled terminal row"
    );
    assert_eq!(
        durable.state, "stopped",
        "duplicate terminal deliveries must not leave a quarantine/pending state: {durable:?}"
    );
    assert!(durable.stopped_at.is_some());
    assert_eq!(
        repo.checkpoints()
            .into_iter()
            .filter(|row| row["session_id"] == json!(format!("claude__{session}")))
            .count(),
        1,
        "same native delivery must produce exactly one checkpoint"
    );
    let entries = durable.metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .expect("terminal receipt ledger is present");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["status"], json!("complete"));
    assert_ne!(
        entries[0]["finalizer"]["status"],
        json!("quarantined"),
        "same-event contender may not turn the elected marker into a takeover quarantine"
    );
    let conn = repo.db().await;
    let replay_head: String = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT `commit` AS ref_commit FROM reference
             WHERE name = 'traces' LIMIT 1"
                .to_string(),
        ))
        .await
        .expect("read traces ref after native replay")
        .expect("traces ref exists after terminal checkpoint")
        .try_get_by("ref_commit")
        .expect("decode replay traces ref head");
    assert_eq!(
        replay_head, traces_head_before_replay,
        "settled native replay must not append another traces commit"
    );
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

/// ACF-02's public CLI matrix: the thin hook adapter must consume the reducer
/// plan rather than keep a second state table.  In particular, only the
/// terminal action may publish a `stopped` row with a terminal timestamp.
#[tokio::test]
async fn capture_state_transition_matrix() {
    let repo = HookRepo::init();
    let session = "sess-capture-state-transition-matrix";
    let session_id = format!("claude__{session}");
    let transcript = repo.write_claude_transcript(&repo.repo, session, "matrix source");
    let steps = [
        ("session-start", "SessionStart", "active", false, 0_usize),
        ("compaction", "Compaction", "condensed", false, 0),
        ("prompt", "UserPromptSubmit", "active", false, 0),
        ("stop", "Stop", "active", false, 1),
        ("session-end", "SessionEnd", "stopped", true, 2),
    ];
    let mut previous_revision = 0_i64;
    let mut session_end_envelope = None;
    let mut terminal_state = None;
    let mut terminal_checkpoints = None;

    for (index, (verb, hook_event_name, expected_state, has_stopped_at, checkpoints)) in
        steps.into_iter().enumerate()
    {
        let envelope = repo.envelope(
            hook_event_name,
            session,
            Some(&transcript),
            json!({ "event_id": format!("capture-transition-matrix-{index}-v1") }),
        );
        let out = repo.hook("claude-code", verb, &envelope);
        assert!(
            out.status.success(),
            "{hook_event_name} must consume its lifecycle plan: {}",
            describe(&out)
        );
        let durable = repo.durable_session(&session_id).await;
        assert_eq!(
            durable.state,
            expected_state,
            "{hook_event_name} durable state; row={durable:?}; public={}; checkpoints={:?}",
            repo.session_show(&session_id),
            repo.checkpoints()
        );
        assert_eq!(
            durable.stopped_at.is_some(),
            has_stopped_at,
            "{hook_event_name} stopped_at relationship"
        );
        if hook_event_name == "SessionEnd" {
            // Terminal publication deliberately has two durable edges: the
            // pending receipt reservation and its finalizer-proven completion.
            // It must still advance the lifecycle generation, but consumer
            // contract only requires strict increase rather than a forged
            // single-step revision.
            assert!(
                durable.sync_revision > previous_revision,
                "terminal action must strictly advance sync_revision"
            );
        } else {
            assert_eq!(
                durable.sync_revision,
                previous_revision + 1,
                "each nonterminal lifecycle action must advance sync_revision once"
            );
        }
        assert_eq!(
            repo.checkpoints().len(),
            checkpoints,
            "{hook_event_name} checkpoint action"
        );
        previous_revision = durable.sync_revision;
        if hook_event_name == "SessionEnd" {
            session_end_envelope = Some(envelope);
            terminal_state = Some(durable);
            terminal_checkpoints = Some(repo.checkpoints());
        }
    }

    let session_end_envelope = session_end_envelope.expect("matrix has a terminal action");
    let terminal_state = terminal_state.expect("terminal row recorded");
    let terminal_checkpoints = terminal_checkpoints.expect("terminal checkpoints recorded");
    let replay = repo.hook("claude-code", "session-end", &session_end_envelope);
    assert!(
        replay.status.success(),
        "same SessionEnd replay must be acknowledged: {}",
        describe(&replay)
    );
    assert_eq!(
        repo.durable_session(&session_id).await,
        terminal_state,
        "same SessionEnd must not change the completed terminal row"
    );
    assert_eq!(
        repo.checkpoints(),
        terminal_checkpoints,
        "same SessionEnd must not append a second terminal checkpoint"
    );
}

#[tokio::test]
async fn nonterminal_events_never_create_pending_artifact() {
    let repo = HookRepo::init();
    let session = "sess-no-nonterminal-artifact";
    let transcript = repo.write_claude_transcript(&repo.repo, session, "nonterminal source");
    let cases = [
        ("session-start", "SessionStart"),
        ("prompt", "UserPromptSubmit"),
        ("compaction", "Compaction"),
        ("stop", "Stop"),
    ];
    for (index, (verb, event)) in cases.into_iter().enumerate() {
        let out = repo.hook(
            "claude-code",
            verb,
            &repo.envelope(
                event,
                session,
                Some(&transcript),
                json!({ "event_id": format!("nonterminal-artifact-{index}-v1") }),
            ),
        );
        assert!(out.status.success(), "{event}: {}", describe(&out));
        assert_eq!(
            repo.pending_artifact_count().await,
            0,
            "{event} must not create a durable SessionEnd recovery artifact"
        );
    }

    // Budget exhaustion must not turn a nonterminal or subagent boundary
    // into replayable evidence either. A 1 ms cooperative budget expires
    // inside capture: the event fails with the fixed incomplete message or
    // stays advisory, and never writes an artifact, header or checkpoint.
    let codex_session = "sess-no-nonterminal-artifact-codex";
    let started = repo.hook(
        "codex",
        "session-start",
        &repo.envelope("SessionStart", codex_session, None, json!({})),
    );
    assert!(
        started.status.success(),
        "codex SessionStart: {}",
        describe(&started)
    );
    let checkpoints = repo.checkpoints().len();
    let claude_source = Some(transcript.as_path());
    let exhausted = [
        ("claude-code", "stop", "Stop", session, claude_source),
        (
            "claude-code",
            "compaction",
            "Compaction",
            session,
            claude_source,
        ),
        (
            "claude-code",
            "subagent-end",
            "SubagentStop",
            session,
            claude_source,
        ),
        ("codex", "stop", "Stop", codex_session, None),
        ("codex", "compaction", "Compaction", codex_session, None),
        ("codex", "subagent-end", "SubagentStop", codex_session, None),
    ];
    for (index, (agent, verb, event, sid, source)) in exhausted.into_iter().enumerate() {
        let out = repo.run(
            &["agent", "hooks", "--capture-budget-ms", "1", agent, verb],
            Some(&repo.envelope(
                event,
                sid,
                source,
                json!({ "event_id": format!("exhausted-nonterminal-{index}-v1") }),
            )),
        );
        assert!(
            out.status.success()
                || String::from_utf8_lossy(&out.stderr)
                    .contains("agent hook ingestion failed: capture could not be completed"),
            "{agent} {event} under an exhausted budget must stay advisory or fail as incomplete: {}",
            describe(&out)
        );
        assert_eq!(
            repo.pending_artifact_count().await,
            0,
            "{agent} {event} under an exhausted budget must not create a recovery artifact"
        );
        assert_eq!(
            repo.checkpoints().len(),
            checkpoints,
            "{agent} {event} under an exhausted budget must not write a checkpoint"
        );
    }

    // With no artifact or header there is nothing to hint: the debug hold
    // seam would keep a launched worker on the repository lock, so an
    // unowned (or never created) lock proves no recovery worker started.
    let release = repo.home.join("nonterminal-worker-release");
    let mut cmd = repo.command_in(&repo.repo);
    cmd.args(["agent", "hooks", "claude-code", "session-start"])
        .env("LIBRA_TEST", "1")
        .env("LIBRA_TEST_CAPTURE_WORKER_HOLD_PATH", &release)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn SessionStart hint probe");
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(
            repo.envelope(
                "SessionStart",
                "sess-no-nonterminal-artifact-next",
                None,
                json!({}),
            )
            .as_bytes(),
        )
        .expect("write SessionStart envelope");
    let hinted = child
        .wait_with_output()
        .expect("wait for SessionStart hint");
    assert!(
        hinted.status.success(),
        "SessionStart hint: {}",
        describe(&hinted)
    );
    std::thread::sleep(Duration::from_millis(200));
    let lock_path = repo
        .repo
        .join(".libra")
        .join("private")
        .join("agent-capture-recovery-worker.lock");
    if let Ok(lock) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        assert!(
            lock.try_lock().is_ok(),
            "no recovery worker may be launched without a durable artifact header"
        );
    }
    std::fs::write(&release, b"").expect("release any held worker");
}

/// A failed nonterminal checkpoint write must not leave a durable replay
/// artifact or any of its private alias/chunk/quarantine records.
#[tokio::test]
async fn failed_nonterminal_checkpoint_does_not_create_pending_artifact() {
    let repo = HookRepo::init();
    let session = "sess-failed-nonterminal-artifact";
    let transcript = repo.write_claude_transcript(&repo.repo, session, "nonterminal source");
    let started = repo.hook(
        "claude-code",
        "session-start",
        &repo.envelope(
            "SessionStart",
            session,
            Some(&transcript),
            json!({ "event_id": "failed-nonterminal-start-v1" }),
        ),
    );
    assert!(
        started.status.success(),
        "SessionStart: {}",
        describe(&started)
    );

    let objects = repo.repo.join(".libra").join("objects");
    let backup = repo.repo.join(".libra").join("objects-test-backup");
    assert!(!backup.exists(), "isolated test backup must not exist");
    std::fs::rename(&objects, &backup).expect("move object directory aside");
    std::fs::write(&objects, b"not a directory").expect("block object directory creation");
    let stopped = repo.hook(
        "claude-code",
        "stop",
        &repo.envelope(
            "Stop",
            session,
            Some(&transcript),
            json!({ "event_id": "failed-nonterminal-stop-v1" }),
        ),
    );
    std::fs::remove_file(&objects).expect("remove object-directory blocker");
    std::fs::rename(&backup, &objects).expect("restore object directory");
    assert_eq!(repo.pending_artifact_count().await, 0);
    assert!(
        !stopped.status.success(),
        "the object-store failure must reach the nonterminal checkpoint path: {}",
        describe(&stopped)
    );
    assert!(
        String::from_utf8_lossy(&stopped.stderr)
            .contains("agent hook ingestion failed: capture could not be completed"),
        "the failure must retain the expected hook-ingest classification: {}",
        describe(&stopped)
    );
}

/// A native delivery identity represents one reducer action. Replaying it
/// must reuse both no-checkpoint and checkpoint-producing action receipts,
/// without consuming another revision or writing another traces checkpoint.
#[tokio::test]
async fn duplicate_event_is_action_idempotent() {
    let repo = HookRepo::init();
    let session = "sess-duplicate-lifecycle-action";
    let session_id = format!("claude__{session}");
    let transcript = repo.write_claude_transcript(&repo.repo, session, "dedupe source");
    let started = repo.hook(
        "claude-code",
        "session-start",
        &repo.envelope(
            "SessionStart",
            session,
            Some(&transcript),
            json!({ "event_id": "duplicate-action-start-v1" }),
        ),
    );
    assert!(
        started.status.success(),
        "session start: {}",
        describe(&started)
    );

    let compaction = repo.envelope(
        "Compaction",
        session,
        Some(&transcript),
        json!({ "event_id": "duplicate-action-compaction-v1" }),
    );
    let first_compaction = repo.hook("claude-code", "compaction", &compaction);
    assert!(
        first_compaction.status.success(),
        "first compaction: {}",
        describe(&first_compaction)
    );
    let compaction_state = repo.durable_session(&session_id).await;
    assert_eq!(compaction_state.state, "condensed");
    assert_eq!(compaction_state.stopped_at, None);
    assert_eq!(compaction_state.sync_revision, 2);
    let repeated_compaction = repo.hook("claude-code", "compaction", &compaction);
    assert!(
        repeated_compaction.status.success(),
        "duplicate compaction must be acknowledged: {}",
        describe(&repeated_compaction)
    );
    assert_eq!(
        repo.durable_session(&session_id).await,
        compaction_state,
        "duplicate no-checkpoint action must not mutate the durable row"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "compaction has no checkpoint action"
    );

    let turn_end = repo.envelope(
        "Stop",
        session,
        Some(&transcript),
        json!({ "event_id": "duplicate-action-turn-end-v1" }),
    );
    let first_turn_end = repo.hook("claude-code", "stop", &turn_end);
    assert!(
        first_turn_end.status.success(),
        "first turn end: {}",
        describe(&first_turn_end)
    );
    let turn_end_state = repo.durable_session(&session_id).await;
    let checkpoints = repo.checkpoints();
    assert_eq!(turn_end_state.state, "active");
    assert_eq!(turn_end_state.stopped_at, None);
    assert_eq!(turn_end_state.sync_revision, 3);
    assert_eq!(checkpoints.len(), 1, "TurnEnd writes one checkpoint");

    let repeated_turn_end = repo.hook("claude-code", "stop", &turn_end);
    assert!(
        repeated_turn_end.status.success(),
        "duplicate TurnEnd must be acknowledged: {}",
        describe(&repeated_turn_end)
    );
    assert_eq!(
        repo.durable_session(&session_id).await,
        turn_end_state,
        "duplicate checkpoint action must not advance the durable generation"
    );
    assert_eq!(
        repo.checkpoints(),
        checkpoints,
        "duplicate TurnEnd must reuse its checkpoint instead of appending one"
    );
}

/// ACF-02 consumer gate for every canonical non-SessionEnd transition. A
/// completed terminal row is deliberately reactivated by those actions, but
/// its historical terminal timestamp stays intact; only the CLI resume
/// consumer is allowed to clear it. The fixture goes through ingress,
/// catalog, and checkpoint coordination for every reducer arm.
#[tokio::test]
async fn terminal_consumer_contract_v1() {
    let repo = HookRepo::init();
    let session = "sess-terminal-consumer-contract";
    let session_id = format!("codex__{session}");
    let transcript = repo.write_claude_transcript(&repo.repo, session, "consumer source");
    let started = repo.hook(
        "codex",
        "session-start",
        &repo.envelope(
            "SessionStart",
            session,
            Some(&transcript),
            json!({ "event_id": "terminal-consumer-start-v1" }),
        ),
    );
    assert!(
        started.status.success(),
        "session start: {}",
        describe(&started)
    );
    let ended = repo.hook(
        "codex",
        "session-end",
        &repo.envelope(
            "SessionEnd",
            session,
            Some(&transcript),
            json!({ "event_id": "terminal-consumer-end-v1" }),
        ),
    );
    assert!(
        ended.status.success(),
        "terminal action must complete before consumer assertions: {}",
        describe(&ended)
    );
    let terminal = repo.durable_session(&session_id).await;
    assert_eq!(terminal.state, "stopped");
    let stopped_at = terminal
        .stopped_at
        .expect("completed terminal action must have stopped_at");
    let mut expected_revision = terminal.sync_revision;
    let mut expected_checkpoints = repo.checkpoints().len();
    assert_eq!(expected_checkpoints, 1, "SessionEnd checkpoint");

    let conn = repo.db().await;
    let matrix = [
        (
            "matrix-session-start",
            LifecycleEventKind::SessionStart,
            "active",
            0_usize,
        ),
        (
            "matrix-turn-start",
            LifecycleEventKind::TurnStart,
            "active",
            0,
        ),
        ("matrix-tool-use", LifecycleEventKind::ToolUse, "active", 0),
        (
            "matrix-model-update",
            LifecycleEventKind::ModelUpdate,
            "active",
            0,
        ),
        (
            "matrix-compaction",
            LifecycleEventKind::Compaction,
            "condensed",
            0,
        ),
        (
            "matrix-compaction-completed",
            LifecycleEventKind::CompactionCompleted,
            "active",
            0,
        ),
        (
            "matrix-permission-request",
            LifecycleEventKind::PermissionRequest,
            "active",
            0,
        ),
        (
            "matrix-source-enabled",
            LifecycleEventKind::SourceEnabled,
            "active",
            0,
        ),
        (
            "matrix-source-disabled",
            LifecycleEventKind::SourceDisabled,
            "active",
            0,
        ),
        ("matrix-turn-end", LifecycleEventKind::TurnEnd, "active", 1),
        (
            "matrix-subagent-start",
            LifecycleEventKind::SubagentStart,
            "active",
            1,
        ),
        (
            "matrix-subagent-end",
            LifecycleEventKind::SubagentEnd,
            "active",
            1,
        ),
    ];
    for (index, (hook_event_name, kind, expected_state, checkpoint_delta)) in
        matrix.into_iter().enumerate()
    {
        if let Some((verb, native_event_name)) = match kind {
            // Keep the native checkpoint-producing Codex adapters in this
            // cross-layer contract as well as the provider-neutral fixture
            // below. This proves real hooks can reactivate a completed row
            // and write their committed/subagent checkpoints; the custom
            // fixture covers reducer arms that no installed provider spells.
            LifecycleEventKind::TurnEnd => Some(("stop", "Stop")),
            LifecycleEventKind::SubagentStart => Some(("subagent-start", "SubagentStart")),
            LifecycleEventKind::SubagentEnd => Some(("subagent-end", "SubagentStop")),
            _ => None,
        } {
            let output = repo.hook(
                "codex",
                verb,
                &repo.envelope(
                    native_event_name,
                    session,
                    Some(&transcript),
                    json!({ "event_id": format!("terminal-consumer-{index}-v1") }),
                ),
            );
            assert!(
                output.status.success(),
                "native Codex {kind} must consume the terminal reactivation plan: {}",
                describe(&output)
            );
        } else {
            ingest_matrix_event(
                &repo,
                &conn,
                session,
                &transcript,
                hook_event_name,
                kind,
                &format!("terminal-consumer-{index}-v1"),
            )
            .await;
        }
        expected_revision += 1;
        expected_checkpoints += checkpoint_delta;
        let actual = repo.durable_session(&session_id).await;
        assert_eq!(actual.state, expected_state, "{kind} next state");
        assert_eq!(
            actual.stopped_at,
            Some(stopped_at),
            "{kind} is a live reactivation, not an implicit CLI resume"
        );
        assert_eq!(
            actual.sync_revision, expected_revision,
            "{kind} must advance the durable lifecycle generation"
        );
        assert_eq!(
            repo.checkpoints().len(),
            expected_checkpoints,
            "{kind} checkpoint action"
        );
    }
}

/// The explicit operator resume is intentionally unlike a live lifecycle
/// reactivation: it clears `stopped_at` while switching a completed terminal
/// session back to active, and advances the durable generation exactly once.
#[tokio::test]
async fn explicit_cli_resume_consumer_contract_v1() {
    let repo = HookRepo::init();
    let session = "sess-explicit-cli-resume-consumer";
    let session_id = format!("claude__{session}");
    let transcript = repo.write_claude_transcript(&repo.repo, session, "resume source");
    for (verb, hook_event_name, event_id) in [
        (
            "session-start",
            "SessionStart",
            "explicit-cli-resume-start-v1",
        ),
        ("session-end", "SessionEnd", "explicit-cli-resume-end-v1"),
    ] {
        let out = repo.hook(
            "claude-code",
            verb,
            &repo.envelope(
                hook_event_name,
                session,
                Some(&transcript),
                json!({ "event_id": event_id }),
            ),
        );
        assert!(
            out.status.success(),
            "{hook_event_name} setup: {}",
            describe(&out)
        );
    }
    let before = repo.durable_session(&session_id).await;
    assert_eq!(before.state, "stopped");
    assert!(before.stopped_at.is_some(), "completed terminal state");

    let resumed = repo.run(&["agent", "session", "resume", &session_id, "--json"], None);
    assert!(
        resumed.status.success(),
        "explicit CLI resume: {}",
        describe(&resumed)
    );
    let resumed_json: Value = serde_json::from_slice(&resumed.stdout).unwrap_or_else(|error| {
        panic!(
            "resume stdout is not JSON ({error}): {}",
            describe(&resumed)
        )
    });
    assert_eq!(resumed_json["ok"], json!(true));
    assert_eq!(resumed_json["data"]["action"], json!("resume"));
    assert_eq!(resumed_json["data"]["previous_state"], json!("stopped"));
    assert_eq!(resumed_json["data"]["state"], json!("active"));
    assert!(
        resumed_json["data"]["stopped_at"].is_null(),
        "public resume result must state that it cleared stopped_at: {resumed_json}"
    );
    let after = repo.durable_session(&session_id).await;
    assert_eq!(after.state, "active");
    assert_eq!(after.stopped_at, None);
    assert_eq!(
        after.sync_revision,
        before.sync_revision + 1,
        "explicit resume must advance the durable generation exactly once"
    );

    let repeated = repo.run(&["agent", "session", "resume", &session_id, "--json"], None);
    assert!(
        repeated.status.success(),
        "idempotent explicit resume: {}",
        describe(&repeated)
    );
    let repeated_json: Value = serde_json::from_slice(&repeated.stdout).unwrap_or_else(|error| {
        panic!(
            "repeated resume stdout is not JSON ({error}): {}",
            describe(&repeated)
        )
    });
    assert_eq!(repeated_json["data"]["updated"], json!(false));
    assert_eq!(
        repo.durable_session(&session_id).await,
        after,
        "already-active resume must not advance sync_revision again"
    );
}

/// Locate the one ledger entry for a deferred terminal receipt by its
/// durable action key. Ledger order is an implementation detail; the action
/// key is the stable replay identity.
fn terminal_receipt_entry(durable: &DurableSessionState, action_key: &str) -> Value {
    durable.metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .expect("terminal receipt ledger is present")
        .iter()
        .find(|entry| entry["action_key"] == json!(action_key))
        .unwrap_or_else(|| panic!("ledger lacks terminal receipt {action_key}: {durable:?}"))
        .clone()
}

/// The traces ref head, or `None` before the first checkpoint publishes it.
async fn traces_ref_head(repo: &HookRepo) -> Option<String> {
    let conn = repo.db().await;
    conn.query_one_raw(Statement::from_string(
        conn.get_database_backend(),
        "SELECT `commit` AS ref_commit FROM reference WHERE name = 'traces' LIMIT 1".to_string(),
    ))
    .await
    .expect("read traces ref head")
    .and_then(|row| {
        row.try_get_by::<Option<String>, _>("ref_commit")
            .expect("decode traces ref head")
    })
}

/// A stale terminal finalizer cannot overwrite a takeover. The first
/// SessionEnd leaves a pending receipt bound to its marker generation
/// because checkpoint publication is blocked. A resumed SessionStart then
/// advances the durable generation, and later a newer SessionEnd plus an
/// explicit CLI resume take the session over twice more. Every redelivery of
/// the stale native terminal must be rejected by the receipt's revision
/// fence: it may neither stop the session nor publish its checkpoint, and
/// its persisted receipt and finalizer evidence stay unchanged.
#[tokio::test]
async fn stale_finalizer_cannot_overwrite_takeover() {
    let repo = HookRepo::init();
    let session = "sess-stale-finalizer-takeover";
    let session_id = format!("claude__{session}");
    let sentinel = "must not enter a finalizer diagnostic";
    let start = repo.hook(
        "claude-code",
        "session-start",
        &repo.envelope(
            "SessionStart",
            session,
            None,
            json!({ "event_id": "stale-finalizer-start-v1" }),
        ),
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );

    let stale_terminal = repo.envelope(
        "SessionEnd",
        session,
        None,
        json!({
            "event_id": "stale-finalizer-takeover-event-v1",
            "last_assistant_message": sentinel,
        }),
    );
    // Block checkpoint publication only for the first terminal delivery so
    // its finalizer is elected and persisted, but no checkpoint can become
    // durable and the receipt stays pending.
    let objects = repo.repo.join(".libra").join("objects");
    let backup = repo.repo.join(".libra").join("objects-test-backup");
    assert!(!backup.exists(), "isolated test backup must not exist");
    std::fs::rename(&objects, &backup).expect("move object directory aside");
    std::fs::write(&objects, b"not a directory").expect("block object directory creation");
    let blocked = repo.hook("claude-code", "session-end", &stale_terminal);
    std::fs::remove_file(&objects).expect("remove object-directory blocker");
    std::fs::rename(&backup, &objects).expect("restore object directory");
    assert!(
        !blocked.status.success(),
        "a terminal whose checkpoint cannot publish must not be acknowledged: {}",
        describe(&blocked)
    );
    assert!(
        !describe(&blocked).contains(sentinel),
        "the failed terminal must not echo transcript content: {}",
        describe(&blocked)
    );
    let pending = repo.durable_session(&session_id).await;
    assert_eq!(pending.state, "active", "pending terminal must stay active");
    assert_eq!(pending.stopped_at, None);
    assert!(
        repo.checkpoints().is_empty(),
        "a blocked terminal must not publish a checkpoint"
    );
    assert_eq!(traces_ref_head(&repo).await, None);
    let stale_entry = pending.metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .expect("pending terminal receipt ledger")
        .iter()
        .find(|entry| entry["intent"]["phase"] == json!("stopped"))
        .unwrap_or_else(|| panic!("ledger lacks the pending terminal receipt: {pending:?}"))
        .clone();
    let stale_action_key = stale_entry["action_key"]
        .as_str()
        .expect("stale terminal action key")
        .to_string();
    assert_eq!(stale_entry["status"], json!("pending"));
    assert_eq!(
        stale_entry["reserved_revision"],
        json!(pending.sync_revision),
        "the pending terminal owns the current durable generation"
    );
    assert_eq!(stale_entry["finalizer"]["status"], json!("pending"));
    let stale_marker = stale_entry["finalizer"]["marker_generation"]
        .as_str()
        .expect("the stale writer persisted its marker generation");
    assert!(
        !stale_marker.is_empty() && !stale_marker.starts_with("capture-finalizer-unbound-"),
        "the blocked writer must have elected a concrete marker: {stale_entry}"
    );
    assert!(
        !pending.metadata.to_string().contains(sentinel),
        "the pending receipt must stay content-free: {pending:?}"
    );

    // Takeover 1: a resumed SessionStart advances the durable generation
    // past the stale receipt's reservation.
    let resume = repo.hook(
        "claude-code",
        "session-start",
        &repo.envelope(
            "SessionStart",
            session,
            None,
            json!({ "event_id": "stale-finalizer-resume-v1", "source": "resume" }),
        ),
    );
    assert!(
        resume.status.success(),
        "resumed session start: {}",
        describe(&resume)
    );
    let resumed = repo.durable_session(&session_id).await;
    assert_eq!(resumed.state, "active");
    assert!(
        resumed.sync_revision > pending.sync_revision,
        "the resumed SessionStart must advance the generation: {pending:?} -> {resumed:?}"
    );

    // The stale redelivery is acknowledged only as a fenced skip. Capture
    // the content-free ingest diagnostic to prove the catalog rejected it on
    // the revision fence rather than by any later, accidental failure.
    let fence_log = repo.repo.join("stale-finalizer-fence.log");
    let mut fenced = repo.command_in(&repo.repo);
    fenced
        .args(["agent", "hooks", "claude-code", "session-end"])
        .env("LIBRA_LOG", "agent.hook.ingest=warn")
        .env("LIBRA_LOG_FILE", &fence_log)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut fenced = fenced.spawn().expect("spawn stale terminal redelivery");
    fenced
        .stdin
        .take()
        .expect("stale terminal stdin piped")
        .write_all(stale_terminal.as_bytes())
        .expect("write stale terminal envelope");
    let fenced = fenced
        .wait_with_output()
        .expect("wait for stale terminal redelivery");
    assert!(
        fenced.status.success(),
        "a fenced stale terminal is acknowledged without side effects: {}",
        describe(&fenced)
    );
    let fence_diagnostic = std::fs::read_to_string(&fence_log).expect("read fence diagnostic");
    assert!(
        fence_diagnostic.contains("catalog_conflict")
            && fence_diagnostic.contains("ConditionalWrite"),
        "the stale terminal must be rejected by the receipt revision fence: {fence_diagnostic}"
    );
    assert!(
        !fence_diagnostic.contains(sentinel) && !describe(&fenced).contains(sentinel),
        "the fence diagnostic must stay content-free: {fence_diagnostic}"
    );
    assert_eq!(
        repo.durable_session(&session_id).await,
        resumed,
        "a stale finalizer must not stop or otherwise mutate the resumed session"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "a stale finalizer must not publish its checkpoint after a takeover"
    );
    assert_eq!(traces_ref_head(&repo).await, None);

    // Takeover 2: a newer native terminal completes the current generation.
    let takeover_terminal = repo.envelope(
        "SessionEnd",
        session,
        None,
        json!({ "event_id": "stale-finalizer-takeover-event-v2" }),
    );
    let takeover = repo.hook("claude-code", "session-end", &takeover_terminal);
    assert!(
        takeover.status.success(),
        "the takeover terminal must complete: {}",
        describe(&takeover)
    );
    let stopped = repo.durable_session(&session_id).await;
    assert_eq!(stopped.state, "stopped");
    assert!(stopped.stopped_at.is_some());
    let checkpoints_stopped = repo.checkpoints();
    assert_eq!(
        checkpoints_stopped.len(),
        1,
        "only the takeover terminal may publish a checkpoint: {checkpoints_stopped:?}"
    );
    let traces_head = traces_ref_head(&repo).await;
    assert_eq!(
        traces_head.as_deref(),
        checkpoints_stopped[0]["traces_commit"].as_str(),
        "no hidden stale append may advance refs/libra/traces"
    );
    assert_eq!(
        terminal_receipt_entry(&stopped, &stale_action_key),
        stale_entry,
        "the takeover must not adopt, complete, or re-arm the stale receipt"
    );

    // Takeover 3: an explicit operator resume reopens the stopped session.
    let operator_resume = repo.run(&["agent", "session", "resume", &session_id, "--json"], None);
    assert!(
        operator_resume.status.success(),
        "explicit CLI resume: {}",
        describe(&operator_resume)
    );
    let reopened = repo.durable_session(&session_id).await;
    assert_eq!(reopened.state, "active");
    assert_eq!(reopened.stopped_at, None);

    let late = repo.hook("claude-code", "session-end", &stale_terminal);
    assert!(
        late.status.success(),
        "a late stale terminal is acknowledged without side effects: {}",
        describe(&late)
    );
    let after_late = repo.durable_session(&session_id).await;
    assert_eq!(
        after_late, reopened,
        "a stale finalizer must not re-stop a session reopened by the operator"
    );
    assert_eq!(
        terminal_receipt_entry(&after_late, &stale_action_key),
        stale_entry,
        "the stale receipt stays fenced with its original marker and attempt budget"
    );
    assert_eq!(
        repo.checkpoints(),
        checkpoints_stopped,
        "a late stale finalizer must not publish its checkpoint"
    );
    assert_eq!(traces_ref_head(&repo).await, traces_head);
}

/// Native lifecycle verbs in the order one contract session exercises them.
/// The terminal verb runs last so every earlier verb observes a live session.
const HOOK_CONTRACT_VERBS: [&str; 10] = [
    "session-start",
    "prompt",
    "tool-use",
    "permission-request",
    "model-update",
    "compaction",
    "stop",
    "subagent-start",
    "subagent-end",
    "session-end",
];

/// Native `hook_event_name` per verb (in [`HOOK_CONTRACT_VERBS`] order) for
/// the Claude Code taxonomy; also used for the rejected Gemini surfaces.
const CLAUDE_CONTRACT_EVENTS: [&str; 10] = [
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PermissionRequest",
    "ModelUpdate",
    "Compaction",
    "Stop",
    "SubagentStart",
    "SubagentStop",
    "SessionEnd",
];

/// Codex taxonomy counterpart of [`CLAUDE_CONTRACT_EVENTS`].
const CODEX_CONTRACT_EVENTS: [&str; 10] = [
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PermissionRequest",
    "ModelUpdate",
    "PreCompact",
    "Stop",
    "SubagentStart",
    "SubagentStop",
    "SessionEnd",
];

/// OpenCode plugin taxonomy counterpart of [`CLAUDE_CONTRACT_EVENTS`]; verbs
/// without an OpenCode event keep the canonical spelling.
const OPENCODE_CONTRACT_EVENTS: [&str; 10] = [
    "session.created",
    "message.updated",
    "tool.execute.after",
    "PermissionRequest",
    "ModelUpdate",
    "session.compacted",
    "session.idle",
    "SubagentStart",
    "SubagentStop",
    "session.deleted",
];

/// One public hook surface of [`hook_public_contract_byte_compat_matrix`].
struct HookContractSurface {
    label: &'static str,
    command: &'static [&'static str],
    native_events: [&'static str; 10],
}

const HOOK_CONTRACT_SURFACES: [HookContractSurface; 5] = [
    HookContractSurface {
        label: "hooks-claude",
        command: &["hooks", "claude"],
        native_events: CLAUDE_CONTRACT_EVENTS,
    },
    HookContractSurface {
        label: "hooks-codex",
        command: &["hooks", "codex"],
        native_events: CODEX_CONTRACT_EVENTS,
    },
    HookContractSurface {
        label: "agent-hooks-claude-code",
        command: &["agent", "hooks", "claude-code"],
        native_events: CLAUDE_CONTRACT_EVENTS,
    },
    HookContractSurface {
        label: "agent-hooks-codex",
        command: &["agent", "hooks", "codex"],
        native_events: CODEX_CONTRACT_EVENTS,
    },
    HookContractSurface {
        label: "agent-hooks-opencode",
        command: &["agent", "hooks", "opencode"],
        native_events: OPENCODE_CONTRACT_EVENTS,
    },
];

/// Pinned observations of [`hook_public_contract_byte_compat_matrix`], one
/// line per hook invocation:
/// `<surface> <case> <verb> exit=<code> stdout=<bytes> stderr=<bytes> | <durable>`.
const HOOK_PUBLIC_CONTRACT_MATRIX: &str = r##"hooks-claude envelope-invalid session-start exit=Some(128) stdout="" stderr="fatal: hook ingestion failed: hook callback ended before trusted terminal capture ingress: invalid hook JSON payload (Syntax at line 1 column 2)\nError-Code: LBR-AGENT-008\n{\"ok\":false,\"error_code\":\"LBR-AGENT-008\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"hook ingestion failed: hook callback ended before trusted terminal capture ingress: invalid hook JSON payload (Syntax at line 1 column 2)\"}\n" | sessions=[] artifacts=0
hooks-claude unknown-event stop exit=Some(0) stdout="" stderr="" | sessions=[] artifacts=0
hooks-claude cwd-outside-worktree session-start exit=Some(128) stdout="" stderr="fatal: hook ingestion failed: hook callback ended before trusted terminal capture ingress: hook cwd is outside the active worktree\nError-Code: LBR-AGENT-008\n{\"ok\":false,\"error_code\":\"LBR-AGENT-008\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"hook ingestion failed: hook callback ended before trusted terminal capture ingress: hook cwd is outside the active worktree\"}\n" | sessions=[] artifacts=0
hooks-claude SessionStart session-start exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[]:receipts=1(pending=0)[]] artifacts=0
hooks-claude UserPromptSubmit prompt exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[]:receipts=2(pending=0)[]] artifacts=0
hooks-claude PostToolUse tool-use exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[]:receipts=3(pending=0)[]] artifacts=0
hooks-claude PermissionRequest permission-request exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[]:receipts=4(pending=0)[]] artifacts=0
hooks-claude ModelUpdate model-update exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[]:receipts=5(pending=0)[]] artifacts=0
hooks-claude Compaction compaction exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:condensed:stopped_at=none:checkpoints=[]:receipts=6(pending=0)[]] artifacts=0
hooks-claude Stop stop exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[committed]:receipts=7(pending=0)[committed/active/complete]] artifacts=0
hooks-claude SubagentStart subagent-start exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[committed,subagent]:receipts=8(pending=0)[committed/active/complete,subagent_boundary/active/complete]] artifacts=0
hooks-claude SubagentStop subagent-end exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[committed,subagent,subagent]:receipts=9(pending=0)[committed/active/complete,subagent_boundary/active/complete,subagent_boundary/active/complete]] artifacts=0
hooks-claude SessionEnd session-end exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:stopped:stopped_at=set:checkpoints=[committed,subagent,subagent,committed]:receipts=10(pending=0)[committed/active/complete,subagent_boundary/active/complete,subagent_boundary/active/complete,committed/stopped/complete]] artifacts=0
hooks-codex envelope-invalid session-start exit=Some(0) stdout="" stderr="" | sessions=[] artifacts=0
hooks-codex unknown-event stop exit=Some(0) stdout="" stderr="" | sessions=[] artifacts=0
hooks-codex cwd-outside-worktree session-start exit=Some(0) stdout="" stderr="" | sessions=[] artifacts=0
hooks-codex SessionStart session-start exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-hooks-codex:codex:active:stopped_at=none:checkpoints=[]:receipts=1(pending=0)[]] artifacts=0
hooks-codex UserPromptSubmit prompt exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-hooks-codex:codex:active:stopped_at=none:checkpoints=[]:receipts=2(pending=0)[]] artifacts=0
hooks-codex PostToolUse tool-use exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-hooks-codex:codex:active:stopped_at=none:checkpoints=[]:receipts=3(pending=0)[]] artifacts=0
hooks-codex PermissionRequest permission-request exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-hooks-codex:codex:active:stopped_at=none:checkpoints=[]:receipts=4(pending=0)[]] artifacts=0
hooks-codex ModelUpdate model-update exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-hooks-codex:codex:active:stopped_at=none:checkpoints=[]:receipts=4(pending=0)[]] artifacts=0
hooks-codex PreCompact compaction exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-hooks-codex:codex:condensed:stopped_at=none:checkpoints=[]:receipts=5(pending=0)[]] artifacts=0
hooks-codex Stop stop exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-hooks-codex:codex:active:stopped_at=none:checkpoints=[committed]:receipts=6(pending=0)[committed/active/complete]] artifacts=0
hooks-codex SubagentStart subagent-start exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-hooks-codex:codex:active:stopped_at=none:checkpoints=[committed,subagent]:receipts=7(pending=0)[committed/active/complete,subagent_boundary/active/complete]] artifacts=0
hooks-codex SubagentStop subagent-end exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-hooks-codex:codex:active:stopped_at=none:checkpoints=[committed,subagent,subagent]:receipts=8(pending=0)[committed/active/complete,subagent_boundary/active/complete,subagent_boundary/active/complete]] artifacts=0
hooks-codex SessionEnd session-end exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-hooks-codex:codex:stopped:stopped_at=set:checkpoints=[committed,subagent,subagent,committed]:receipts=9(pending=0)[committed/active/complete,subagent_boundary/active/complete,subagent_boundary/active/complete,committed/stopped/complete]] artifacts=0
agent-hooks-claude-code envelope-invalid session-start exit=Some(128) stdout="" stderr="fatal: agent hook ingestion failed: hook callback ended before trusted terminal capture ingress: invalid hook JSON payload (Syntax at line 1 column 2)\nError-Code: LBR-AGENT-008\n{\"ok\":false,\"error_code\":\"LBR-AGENT-008\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"agent hook ingestion failed: hook callback ended before trusted terminal capture ingress: invalid hook JSON payload (Syntax at line 1 column 2)\"}\n" | sessions=[] artifacts=0
agent-hooks-claude-code unknown-event stop exit=Some(0) stdout="" stderr="" | sessions=[] artifacts=0
agent-hooks-claude-code cwd-outside-worktree session-start exit=Some(128) stdout="" stderr="fatal: agent hook ingestion failed: hook callback ended before trusted terminal capture ingress: hook cwd is outside the active worktree\nError-Code: LBR-AGENT-008\n{\"ok\":false,\"error_code\":\"LBR-AGENT-008\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"agent hook ingestion failed: hook callback ended before trusted terminal capture ingress: hook cwd is outside the active worktree\"}\n" | sessions=[] artifacts=0
agent-hooks-claude-code SessionStart session-start exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[]:receipts=1(pending=0)[]] artifacts=0
agent-hooks-claude-code UserPromptSubmit prompt exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[]:receipts=2(pending=0)[]] artifacts=0
agent-hooks-claude-code PostToolUse tool-use exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[]:receipts=3(pending=0)[]] artifacts=0
agent-hooks-claude-code PermissionRequest permission-request exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[]:receipts=4(pending=0)[]] artifacts=0
agent-hooks-claude-code ModelUpdate model-update exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[]:receipts=5(pending=0)[]] artifacts=0
agent-hooks-claude-code Compaction compaction exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:condensed:stopped_at=none:checkpoints=[]:receipts=6(pending=0)[]] artifacts=0
agent-hooks-claude-code Stop stop exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[committed]:receipts=7(pending=0)[committed/active/complete]] artifacts=0
agent-hooks-claude-code SubagentStart subagent-start exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[committed,subagent]:receipts=8(pending=0)[committed/active/complete,subagent_boundary/active/complete]] artifacts=0
agent-hooks-claude-code SubagentStop subagent-end exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[committed,subagent,subagent]:receipts=9(pending=0)[committed/active/complete,subagent_boundary/active/complete,subagent_boundary/active/complete]] artifacts=0
agent-hooks-claude-code SessionEnd session-end exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:stopped:stopped_at=set:checkpoints=[committed,subagent,subagent,committed]:receipts=10(pending=0)[committed/active/complete,subagent_boundary/active/complete,subagent_boundary/active/complete,committed/stopped/complete]] artifacts=0
agent-hooks-codex envelope-invalid session-start exit=Some(128) stdout="" stderr="fatal: agent hook ingestion failed: hook callback ended before trusted terminal capture ingress: invalid hook JSON payload (Syntax at line 1 column 2)\nError-Code: LBR-AGENT-008\n{\"ok\":false,\"error_code\":\"LBR-AGENT-008\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"agent hook ingestion failed: hook callback ended before trusted terminal capture ingress: invalid hook JSON payload (Syntax at line 1 column 2)\"}\n" | sessions=[] artifacts=0
agent-hooks-codex unknown-event stop exit=Some(0) stdout="" stderr="" | sessions=[] artifacts=0
agent-hooks-codex cwd-outside-worktree session-start exit=Some(128) stdout="" stderr="fatal: agent hook ingestion failed: hook callback ended before trusted terminal capture ingress: hook cwd is outside the active worktree\nError-Code: LBR-AGENT-008\n{\"ok\":false,\"error_code\":\"LBR-AGENT-008\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"agent hook ingestion failed: hook callback ended before trusted terminal capture ingress: hook cwd is outside the active worktree\"}\n" | sessions=[] artifacts=0
agent-hooks-codex SessionStart session-start exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-agent-hooks-codex:codex:active:stopped_at=none:checkpoints=[]:receipts=1(pending=0)[]] artifacts=0
agent-hooks-codex UserPromptSubmit prompt exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-agent-hooks-codex:codex:active:stopped_at=none:checkpoints=[]:receipts=2(pending=0)[]] artifacts=0
agent-hooks-codex PostToolUse tool-use exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-agent-hooks-codex:codex:active:stopped_at=none:checkpoints=[]:receipts=3(pending=0)[]] artifacts=0
agent-hooks-codex PermissionRequest permission-request exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-agent-hooks-codex:codex:active:stopped_at=none:checkpoints=[]:receipts=4(pending=0)[]] artifacts=0
agent-hooks-codex ModelUpdate model-update exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-agent-hooks-codex:codex:active:stopped_at=none:checkpoints=[]:receipts=4(pending=0)[]] artifacts=0
agent-hooks-codex PreCompact compaction exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-agent-hooks-codex:codex:condensed:stopped_at=none:checkpoints=[]:receipts=5(pending=0)[]] artifacts=0
agent-hooks-codex Stop stop exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-agent-hooks-codex:codex:active:stopped_at=none:checkpoints=[committed]:receipts=6(pending=0)[committed/active/complete]] artifacts=0
agent-hooks-codex SubagentStart subagent-start exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-agent-hooks-codex:codex:active:stopped_at=none:checkpoints=[committed,subagent]:receipts=7(pending=0)[committed/active/complete,subagent_boundary/active/complete]] artifacts=0
agent-hooks-codex SubagentStop subagent-end exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-agent-hooks-codex:codex:active:stopped_at=none:checkpoints=[committed,subagent,subagent]:receipts=8(pending=0)[committed/active/complete,subagent_boundary/active/complete,subagent_boundary/active/complete]] artifacts=0
agent-hooks-codex SessionEnd session-end exit=Some(0) stdout="" stderr="" | sessions=[codex__contract-agent-hooks-codex:codex:stopped:stopped_at=set:checkpoints=[committed,subagent,subagent,committed]:receipts=9(pending=0)[committed/active/complete,subagent_boundary/active/complete,subagent_boundary/active/complete,committed/stopped/complete]] artifacts=0
agent-hooks-opencode envelope-invalid session-start exit=Some(128) stdout="" stderr="fatal: agent hook ingestion failed: hook callback ended before trusted terminal capture ingress: invalid hook JSON payload (Syntax at line 1 column 2)\nError-Code: LBR-AGENT-008\n{\"ok\":false,\"error_code\":\"LBR-AGENT-008\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"agent hook ingestion failed: hook callback ended before trusted terminal capture ingress: invalid hook JSON payload (Syntax at line 1 column 2)\"}\n" | sessions=[] artifacts=0
agent-hooks-opencode unknown-event stop exit=Some(0) stdout="" stderr="" | sessions=[] artifacts=0
agent-hooks-opencode cwd-outside-worktree session-start exit=Some(0) stdout="" stderr="" | sessions=[] artifacts=0
agent-hooks-opencode session.created session-start exit=Some(0) stdout="" stderr="" | sessions=[opencode__contract-agent-hooks-opencode:opencode:active:stopped_at=none:checkpoints=[]:receipts=1(pending=0)[]] artifacts=0
agent-hooks-opencode message.updated prompt exit=Some(0) stdout="" stderr="" | sessions=[opencode__contract-agent-hooks-opencode:opencode:active:stopped_at=none:checkpoints=[]:receipts=2(pending=0)[]] artifacts=0
agent-hooks-opencode tool.execute.after tool-use exit=Some(0) stdout="" stderr="" | sessions=[opencode__contract-agent-hooks-opencode:opencode:active:stopped_at=none:checkpoints=[]:receipts=3(pending=0)[]] artifacts=0
agent-hooks-opencode PermissionRequest permission-request exit=Some(0) stdout="" stderr="" | sessions=[opencode__contract-agent-hooks-opencode:opencode:active:stopped_at=none:checkpoints=[]:receipts=3(pending=0)[]] artifacts=0
agent-hooks-opencode ModelUpdate model-update exit=Some(0) stdout="" stderr="" | sessions=[opencode__contract-agent-hooks-opencode:opencode:active:stopped_at=none:checkpoints=[]:receipts=3(pending=0)[]] artifacts=0
agent-hooks-opencode session.compacted compaction exit=Some(0) stdout="" stderr="" | sessions=[opencode__contract-agent-hooks-opencode:opencode:condensed:stopped_at=none:checkpoints=[]:receipts=4(pending=0)[]] artifacts=0
agent-hooks-opencode session.idle stop exit=Some(0) stdout="" stderr="" | sessions=[opencode__contract-agent-hooks-opencode:opencode:active:stopped_at=none:checkpoints=[committed]:receipts=5(pending=0)[committed/active/complete]] artifacts=0
agent-hooks-opencode SubagentStart subagent-start exit=Some(0) stdout="" stderr="" | sessions=[opencode__contract-agent-hooks-opencode:opencode:active:stopped_at=none:checkpoints=[committed]:receipts=5(pending=0)[committed/active/complete]] artifacts=0
agent-hooks-opencode SubagentStop subagent-end exit=Some(0) stdout="" stderr="" | sessions=[opencode__contract-agent-hooks-opencode:opencode:active:stopped_at=none:checkpoints=[committed]:receipts=5(pending=0)[committed/active/complete]] artifacts=0
agent-hooks-opencode session.deleted session-end exit=Some(0) stdout="" stderr="" | sessions=[opencode__contract-agent-hooks-opencode:opencode:stopped:stopped_at=set:checkpoints=[committed,committed]:receipts=6(pending=0)[committed/active/complete,committed/stopped/complete]] artifacts=0
hooks-gemini SessionStart session-start exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
hooks-gemini UserPromptSubmit prompt exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
hooks-gemini PostToolUse tool-use exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
hooks-gemini PermissionRequest permission-request exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
hooks-gemini ModelUpdate model-update exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
hooks-gemini Compaction compaction exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
hooks-gemini Stop stop exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
hooks-gemini SubagentStart subagent-start exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
hooks-gemini SubagentStop subagent-end exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
hooks-gemini SessionEnd session-end exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
agent-hooks-gemini SessionStart session-start exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
agent-hooks-gemini UserPromptSubmit prompt exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
agent-hooks-gemini PostToolUse tool-use exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
agent-hooks-gemini PermissionRequest permission-request exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
agent-hooks-gemini ModelUpdate model-update exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
agent-hooks-gemini Compaction compaction exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
agent-hooks-gemini Stop stop exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
agent-hooks-gemini SubagentStart subagent-start exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
agent-hooks-gemini SubagentStop subagent-end exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
agent-hooks-gemini SessionEnd session-end exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0
"##;

/// Render captured process bytes with the fixture's temporary roots replaced
/// by stable placeholders (including the `/private` alias spelling on macOS).
fn hook_contract_bytes(repo: &HookRepo, bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    let root = repo
        .repo
        .parent()
        .expect("contract repository has a temporary root")
        .to_path_buf();
    for (path, placeholder) in [
        (repo.repo.clone(), "<REPO>"),
        (repo.home.clone(), "<HOME>"),
        (root, "<TMP>"),
    ] {
        let spelled = path.to_string_lossy().into_owned();
        text = text.replace(&spelled, placeholder);
        if let Some(alias) = spelled.strip_prefix("/private") {
            text = text.replace(alias, placeholder);
        }
    }
    format!("{text:?}")
}

/// The durable projection after one invocation: every `agent_session` row's
/// identity, lifecycle columns, checkpoint scopes and capture receipts, plus
/// the repository's pending-artifact count.
async fn hook_contract_projection(repo: &HookRepo) -> String {
    let conn = repo.db().await;
    let backend = conn.get_database_backend();
    let rows = conn
        .query_all_raw(Statement::from_string(
            backend,
            "SELECT session_id, agent_kind, state, stopped_at, metadata_json \
             FROM agent_session ORDER BY session_id"
                .to_string(),
        ))
        .await
        .expect("query contract sessions");
    let mut sessions = Vec::new();
    for row in rows {
        let session_id: String = row.try_get_by("session_id").expect("decode session id");
        let agent_kind: String = row.try_get_by("agent_kind").expect("decode agent kind");
        let state: String = row.try_get_by("state").expect("decode state");
        let stopped_at: Option<i64> = row.try_get_by("stopped_at").expect("decode stopped_at");
        let metadata: Value = serde_json::from_str(
            &row.try_get_by::<String, _>("metadata_json")
                .expect("decode metadata_json"),
        )
        .expect("contract metadata_json is JSON");
        let scopes = conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT scope FROM agent_checkpoint WHERE session_id = ? ORDER BY created_at, rowid",
                [session_id.clone().into()],
            ))
            .await
            .expect("query contract checkpoints")
            .into_iter()
            .map(|row| row.try_get_by::<String, _>("scope").expect("decode scope"))
            .collect::<Vec<_>>();
        let entries = metadata["capture_catalog_receipts_v1"]["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let text = |value: &Value| value.as_str().unwrap_or("-").to_string();
        let checkpoint_receipts = entries
            .iter()
            .filter(|entry| entry["intent"]["checkpoint"] != json!("none"))
            .map(|entry| {
                format!(
                    "{}/{}/{}",
                    text(&entry["intent"]["checkpoint"]),
                    text(&entry["intent"]["phase"]),
                    text(&entry["status"])
                )
            })
            .collect::<Vec<_>>();
        let pending = entries
            .iter()
            .filter(|entry| entry["status"] == json!("pending"))
            .count();
        sessions.push(format!(
            "{session_id}:{agent_kind}:{state}:stopped_at={}:checkpoints=[{}]:receipts={}(pending={pending})[{}]",
            if stopped_at.is_some() { "set" } else { "none" },
            scopes.join(","),
            entries.len(),
            checkpoint_receipts.join(","),
        ));
    }
    format!(
        "sessions=[{}] artifacts={}",
        sessions.join(" "),
        repo.pending_artifact_count().await
    )
}

/// ACF-17 VER1 (ADR-ACF-10 byte compatibility): the public hook surfaces keep
/// their exit status, stdout/stderr bytes and durable projection across the
/// live-runtime extraction. Every verb of `hooks claude|codex` and
/// `agent hooks claude-code|codex|opencode` runs through one native session,
/// preceded by an invalid envelope, an unknown event and an out-of-worktree
/// cwd; every verb of both Gemini entry points must stay rejected. Platform
/// capability and budget-expiry outcomes are timing/platform specific and
/// stay in their dedicated unit tests. ADR-ACF-10 freezes this function's
/// token stream together with the helpers it calls directly.
#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
async fn hook_public_contract_byte_compat_matrix() {
    // One invocation -> one pinned line:
    // `<surface> <case> <verb> exit=<code> stdout=<bytes> stderr=<bytes> | <durable>`.
    macro_rules! observe {
        ($repo:expr, $surface:expr, $case:expr, $args:expr, $stdin:expr) => {{
            let repo: &HookRepo = $repo;
            let args: Vec<&str> = $args;
            let stdin: &str = $stdin;
            let out = repo.run(&args, Some(stdin));
            format!(
                "{} {} {} exit={:?} stdout={} stderr={} | {}",
                $surface,
                $case,
                args.last().copied().unwrap_or_default(),
                out.status.code(),
                hook_contract_bytes(repo, &out.stdout),
                hook_contract_bytes(repo, &out.stderr),
                hook_contract_projection(repo).await,
            )
        }};
    }

    let mut observed = Vec::new();
    for surface in &HOOK_CONTRACT_SURFACES {
        let repo = HookRepo::init();
        let label = surface.label;
        let session = format!("contract-{label}");
        let edge_session = format!("contract-{label}-edge");
        let with_verb = |verb: &'static str| {
            let mut args = surface.command.to_vec();
            args.push(verb);
            args
        };
        observed.push(observe!(
            &repo,
            label,
            "envelope-invalid",
            with_verb("session-start"),
            "not a hook envelope"
        ));
        observed.push(observe!(
            &repo,
            label,
            "unknown-event",
            with_verb("stop"),
            &repo.envelope(
                "LibraContractUnknownEvent",
                &edge_session,
                None,
                json!({ "event_id": format!("{edge_session}-unknown") }),
            )
        ));
        observed.push(observe!(
            &repo,
            label,
            "cwd-outside-worktree",
            with_verb("session-start"),
            &repo.envelope_at(
                &repo.home,
                "SessionStart",
                &edge_session,
                None,
                json!({ "event_id": format!("{edge_session}-outside") }),
            )
        ));
        for (verb, native_event) in HOOK_CONTRACT_VERBS.into_iter().zip(surface.native_events) {
            observed.push(observe!(
                &repo,
                label,
                native_event,
                with_verb(verb),
                &repo.envelope(
                    native_event,
                    &session,
                    None,
                    if surface.label == "agent-hooks-opencode" && native_event == "message.updated"
                    {
                        json!({ "event_id": format!("{session}-{verb}"), "prompt": "" })
                    } else {
                        json!({ "event_id": format!("{session}-{verb}") })
                    },
                )
            ));
        }
    }

    let repo = HookRepo::init();
    for command in [&["hooks", "gemini"][..], &["agent", "hooks", "gemini"][..]] {
        let label = command.join("-");
        for (verb, native_event) in HOOK_CONTRACT_VERBS.into_iter().zip(CLAUDE_CONTRACT_EVENTS) {
            let mut args = command.to_vec();
            args.push(verb);
            observed.push(observe!(
                &repo,
                label,
                native_event,
                args,
                &repo.envelope(
                    native_event,
                    "contract-gemini",
                    None,
                    json!({ "event_id": format!("contract-gemini-{verb}") }),
                )
            ));
        }
    }

    let expected: Vec<&str> = HOOK_PUBLIC_CONTRACT_MATRIX.lines().collect();
    assert_eq!(
        observed,
        expected,
        "public hook contract drifted; observed matrix:\n{}",
        observed.join("\n")
    );
}

/// Keep the four Claude/Codex public hook byte contracts in CI while the
/// OpenCode Session-producing surface is exercised only by the manual matrix.
#[tokio::test]
async fn hook_public_contract_claude_codex_ci() {
    // One invocation -> one pinned line:
    // `<surface> <case> <verb> exit=<code> stdout=<bytes> stderr=<bytes> | <durable>`.
    macro_rules! observe {
        ($repo:expr, $surface:expr, $case:expr, $args:expr, $stdin:expr) => {{
            let repo: &HookRepo = $repo;
            let args: Vec<&str> = $args;
            let stdin: &str = $stdin;
            let out = repo.run(&args, Some(stdin));
            format!(
                "{} {} {} exit={:?} stdout={} stderr={} | {}",
                $surface,
                $case,
                args.last().copied().unwrap_or_default(),
                out.status.code(),
                hook_contract_bytes(repo, &out.stdout),
                hook_contract_bytes(repo, &out.stderr),
                hook_contract_projection(repo).await,
            )
        }};
    }

    let mut observed = Vec::new();
    for surface in &HOOK_CONTRACT_SURFACES[..4] {
        let repo = HookRepo::init();
        let label = surface.label;
        let session = format!("contract-{label}");
        let edge_session = format!("contract-{label}-edge");
        let with_verb = |verb: &'static str| {
            let mut args = surface.command.to_vec();
            args.push(verb);
            args
        };
        observed.push(observe!(
            &repo,
            label,
            "envelope-invalid",
            with_verb("session-start"),
            "not a hook envelope"
        ));
        observed.push(observe!(
            &repo,
            label,
            "unknown-event",
            with_verb("stop"),
            &repo.envelope(
                "LibraContractUnknownEvent",
                &edge_session,
                None,
                json!({ "event_id": format!("{edge_session}-unknown") }),
            )
        ));
        observed.push(observe!(
            &repo,
            label,
            "cwd-outside-worktree",
            with_verb("session-start"),
            &repo.envelope_at(
                &repo.home,
                "SessionStart",
                &edge_session,
                None,
                json!({ "event_id": format!("{edge_session}-outside") }),
            )
        ));
        for (verb, native_event) in HOOK_CONTRACT_VERBS.into_iter().zip(surface.native_events) {
            observed.push(observe!(
                &repo,
                label,
                native_event,
                with_verb(verb),
                &repo.envelope(
                    native_event,
                    &session,
                    None,
                    if surface.label == "agent-hooks-opencode" && native_event == "message.updated"
                    {
                        json!({ "event_id": format!("{session}-{verb}"), "prompt": "" })
                    } else {
                        json!({ "event_id": format!("{session}-{verb}") })
                    },
                )
            ));
        }
    }

    let repo = HookRepo::init();
    for command in [&["hooks", "gemini"][..], &["agent", "hooks", "gemini"][..]] {
        let label = command.join("-");
        for (verb, native_event) in HOOK_CONTRACT_VERBS.into_iter().zip(CLAUDE_CONTRACT_EVENTS) {
            let mut args = command.to_vec();
            args.push(verb);
            observed.push(observe!(
                &repo,
                label,
                native_event,
                args,
                &repo.envelope(
                    native_event,
                    "contract-gemini",
                    None,
                    json!({ "event_id": format!("contract-gemini-{verb}") }),
                )
            ));
        }
    }

    let expected: Vec<&str> = HOOK_PUBLIC_CONTRACT_MATRIX
        .lines()
        .filter(|line| !line.starts_with("agent-hooks-opencode "))
        .collect();
    assert_eq!(
        observed,
        expected,
        "public hook contract drifted; observed matrix:\n{}",
        observed.join("\n")
    );
}

// OG-01 inputs are synthetic Libra envelopes, not upstream/native provenance.
type Og01DurableSnapshot = (
    std::collections::BTreeMap<String, Vec<String>>,
    std::collections::BTreeMap<PathBuf, Vec<u8>>,
);

async fn og01_durable_snapshot(repo: &HookRepo) -> Og01DurableSnapshot {
    let conn = repo.db().await;
    let backend = conn.get_database_backend();
    let tables = conn.query_all_raw(Statement::from_string(backend,
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name".to_owned()
    )).await.expect("list durable tables");
    let mut rows = std::collections::BTreeMap::new();
    for table in tables {
        let name: String = table.try_get_by("name").expect("table name");
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        let columns = conn
            .query_all_raw(Statement::from_string(
                backend,
                format!("PRAGMA table_info({quoted})"),
            ))
            .await
            .expect("table columns");
        let expressions: Vec<_> = columns
            .iter()
            .map(|column| {
                let name: String = column.try_get_by("name").expect("column name");
                format!("quote(\"{}\")", name.replace('"', "\"\""))
            })
            .collect();
        assert!(!expressions.is_empty(), "a table must have columns");
        let image = expressions.join(" || char(31) || ");
        let values = conn
            .query_all_raw(Statement::from_string(
                backend,
                format!("SELECT {image} AS image FROM {quoted} ORDER BY image"),
            ))
            .await
            .expect("durable rows")
            .into_iter()
            .map(|row| row.try_get_by("image").expect("row image"))
            .collect();
        rows.insert(name, values);
    }
    conn.close()
        .await
        .expect("close read-only snapshot connection");
    let mut files = std::collections::BTreeMap::new();
    for subtree in ["objects", "refs", "sessions"] {
        let base = repo.repo.join(".libra").join(subtree);
        if !base.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(base).sort_by_file_name() {
            let entry = entry.expect("walk durable capture files");
            if entry.file_type().is_file() {
                files.insert(
                    entry
                        .path()
                        .strip_prefix(&repo.repo)
                        .expect("repo-relative file")
                        .to_path_buf(),
                    std::fs::read(entry.path()).expect("read durable file"),
                );
            }
        }
    }
    (rows, files)
}

#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
async fn opencode_status_idle_maps_turn_end() {
    let repo = HookRepo::init();
    let start = repo.envelope(
        "session.created",
        "synthetic-idle",
        None,
        json!({"event_id":"synthetic-idle-start"}),
    );
    let started = repo.run(
        &["agent", "hooks", "opencode", "session-start"],
        Some(&start),
    );
    assert!(started.status.success(), "{}", describe(&started));
    let envelope = repo.envelope(
        "session.status",
        "synthetic-idle",
        None,
        json!({"status":{"type":"idle"}, "event_id":"synthetic-idle-stop"}),
    );
    let out = repo.run(&["agent", "hooks", "opencode", "stop"], Some(&envelope));
    assert!(out.status.success(), "{}", describe(&out));
    let state = repo.durable_session("opencode__synthetic-idle").await;
    assert_eq!(state.state, "active");
    assert_eq!(state.sync_revision, 2);
    assert_eq!(
        state.metadata["capture_catalog_receipts_v1"]["entries"]
            .as_array()
            .expect("receipts")
            .len(),
        2
    );
    let checkpoints = repo.checkpoints();
    assert_eq!(
        checkpoints.len(),
        1,
        "turn end writes a metadata-only checkpoint"
    );
    assert_eq!(checkpoints[0]["scope"], "committed");
}

#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
async fn opencode_status_busy_retry_fails_closed() {
    let repo = HookRepo::init();
    let baseline = og01_durable_snapshot(&repo).await;
    for status in [
        json!({"type":"busy"}),
        json!({"type":"retry"}),
        json!(null),
        json!("idle"),
        json!({"type":"PRIVATE-STATUS-CANARY"}),
    ] {
        let payload = repo.envelope(
            "session.status",
            "synthetic-status",
            None,
            json!({"status":status}),
        );
        let out = repo.run(&["agent", "hooks", "opencode", "stop"], Some(&payload));
        assert_eq!(out.status.code(), Some(128), "{}", describe(&out));
        assert!(out.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&out.stderr).contains("PRIVATE-STATUS-CANARY"));
        assert_eq!(
            og01_durable_snapshot(&repo).await,
            baseline,
            "status rejection before durable writes"
        );
    }
}

#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
async fn opencode_message_updated_is_single_envelope_turn_start() {
    for mut extra in [
        json!({"prompt":""}),
        json!({"prompt":"synthetic prompt", "role":"user"}),
        json!({"prompt":"", "message":"SYNTHETIC-FALLBACK"}),
    ] {
        let repo = HookRepo::init();
        extra["event_id"] = json!("synthetic-legacy-prompt");
        let payload = repo.envelope("message.updated", "synthetic-prompt", None, extra);
        let out = repo.run(&["agent", "hooks", "opencode", "prompt"], Some(&payload));
        assert!(out.status.success(), "{}", describe(&out));
        assert!(out.stdout.is_empty());
        assert_eq!(repo.sessions().len(), 1);
        let state = repo.durable_session("opencode__synthetic-prompt").await;
        assert_eq!(state.state, "active");
        assert_eq!(state.sync_revision, 1);
        assert_eq!(
            state.metadata["capture_catalog_receipts_v1"]["entries"]
                .as_array()
                .expect("receipts")
                .len(),
            1,
            "one envelope creates exactly one capture receipt"
        );
        assert!(repo.checkpoints().is_empty());
    }
}

#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
async fn opencode_missing_legacy_prompt_fails_before_writes() {
    for active in [false, true] {
        let repo = HookRepo::init();
        let session = "synthetic-missing-prompt";
        if active {
            let payload = repo.envelope("session.created", session, None, json!({}));
            let out = repo.run(
                &["agent", "hooks", "opencode", "session-start"],
                Some(&payload),
            );
            assert!(out.status.success(), "{}", describe(&out));
        }
        let baseline = og01_durable_snapshot(&repo).await;
        for extra in [
            json!({}),
            json!({"role":"user"}),
            json!({"message":"PRIVATE-PROMPT-CANARY"}),
            json!({"user_prompt":"PRIVATE-PROMPT-CANARY"}),
            json!({"role":"user", "message":"PRIVATE-PROMPT-CANARY"}),
            json!({"prompt":null}),
            json!({"prompt":42}),
            json!({"prompt":"", "role":"assistant"}),
            json!({"prompt":"", "role":null}),
        ] {
            let payload = repo.envelope("message.updated", session, None, extra);
            let out = repo.run(&["agent", "hooks", "opencode", "prompt"], Some(&payload));
            assert_eq!(out.status.code(), Some(128), "{}", describe(&out));
            assert!(out.stdout.is_empty());
            let stderr = String::from_utf8_lossy(&out.stderr);
            const PUBLIC_ERROR: &str = "fatal: agent hook ingestion failed: capture could not be completed; retry the hook or inspect the local repository\nError-Code: LBR-INTERNAL-001\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"agent hook ingestion failed: capture could not be completed; retry the hook or inspect the local repository\"}\n";
            assert_eq!(stderr, PUBLIC_ERROR, "public masked error bytes changed");
            assert!(!stderr.contains("PRIVATE-PROMPT-CANARY"));
            assert!(!stderr.contains("requires a prompt string"));
            assert_eq!(
                og01_durable_snapshot(&repo).await,
                baseline,
                "active={active}: rejection must precede every durable write"
            );
        }
    }
}

#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
async fn opencode_legacy_alias_transition_forwarded() {
    for stable_id in [false, true] {
        let repo = HookRepo::init();
        let session = "synthetic-legacy-alias";
        for (name, verb, mut extra) in [
            ("session.created", "session-start", json!({})),
            ("message.updated", "prompt", json!({"prompt":""})),
            ("session.idle", "stop", json!({})),
        ] {
            // Real older plugin frames may have no dedup ID. Also prove the
            // receipt path with an explicit synthetic stable identity.
            if stable_id {
                extra["event_id"] = json!(format!("synthetic-{name}"));
            }
            let payload = repo.envelope(name, session, None, extra);
            let out = repo.run(&["agent", "hooks", "opencode", verb], Some(&payload));
            assert!(out.status.success(), "{}", describe(&out));
            assert!(out.stdout.is_empty());
        }
        let state = repo
            .durable_session("opencode__synthetic-legacy-alias")
            .await;
        assert_eq!(state.state, "active");
        assert_eq!(state.sync_revision, 3);
        let receipts = state.metadata["capture_catalog_receipts_v1"]["entries"].as_array();
        if stable_id {
            assert_eq!(receipts.expect("stable identity receipts").len(), 3);
        } else {
            assert!(
                receipts.is_none(),
                "no stable event identity creates no receipt ledger"
            );
        }
        let checkpoints = repo.checkpoints();
        assert_eq!(
            checkpoints.len(),
            1,
            "legacy turn end writes one metadata-only checkpoint"
        );
        assert_eq!(checkpoints[0]["scope"], "committed");
    }
}

// OG-02 uses only this target's private harness; the frozen dispatcher/oracle
// helpers above remain unchanged. This is a real local child-process test,
// with a controlled exporter and synthetic events, not a live model proof.
fn og02_run_lifecycle_plugin(hung: bool, exit: bool) -> (Vec<Value>, Duration, String) {
    use std::os::unix::fs::PermissionsExt;
    let version = Command::new("node")
        .args(["-e", "process.stdout.write(process.execPath)"])
        .output()
        .expect("Node required for macOS local acceptance");
    assert!(version.status.success());
    let interpreter = String::from_utf8(version.stdout).unwrap();
    let repo = HookRepo::init();
    let enable = repo.run(&["agent", "enable", "--agent", "opencode"], None);
    assert!(enable.status.success(), "{}", describe(&enable));
    let fake = repo.repo.join("controlled-exporter.mjs");
    let log = repo.repo.join("frames.jsonl");
    let pid_file = repo.repo.join("hung.pid");
    let fake_source = format!(
        r#"#!{interpreter}
import {{ readFileSync, appendFileSync, writeFileSync }} from 'node:fs';
const frame=JSON.parse(readFileSync(0,'utf8'));
appendFileSync(process.env.LIBRA_TEST_FORWARD_LOG,JSON.stringify(frame)+'\n');
if ({hung} && process.argv[5]==='stop') {{
 writeFileSync(process.env.LIBRA_TEST_HUNG_PID,String(process.pid));
 process.on('SIGTERM',()=>{{}});
 Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0);
}}
if(!{hung} && process.argv[5]==='session-start') Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,150);
"#
    );
    std::fs::write(&fake, fake_source).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source =
        std::fs::read_to_string(repo.repo.join(".opencode/plugin/libra-hooks.js")).unwrap();
    let old = format!(
        "const LIBRA_COMMAND = {};",
        serde_json::to_string(&std::fs::canonicalize(env!("CARGO_BIN_EXE_libra")).unwrap())
            .unwrap()
    );
    let new = format!(
        "const LIBRA_COMMAND = {};",
        serde_json::to_string(&fake).unwrap()
    );
    assert_eq!(source.matches(&old).count(), 1);
    std::fs::write(repo.repo.join("plugin.mjs"), source.replacen(&old, &new, 1)).unwrap();
    let end = if exit {
        "process.exit(0);"
    } else {
        "await cleanup();"
    };
    let runner = format!(
        r#"
import plugin from './plugin.mjs';
let finish;const finished=new Promise(resolve=>{{finish=resolve;}});
const cleanup=await plugin.setup({{
 location:{{directory:{cwd}}},tool:{{hook:async()=>({{dispose:async()=>{{}}}})}},
 event:{{subscribe:()=> (async function*(){{ try{{
 yield {{type:'session.created',data:{{sessionID:'synthetic-budget'}}}};
 {terminal}
 }}finally{{finish();}} }})()}}
}});
await finished; {end}
"#,
        cwd = serde_json::to_string(&repo.repo).unwrap(),
        terminal = if hung {
            "yield {type:'session.execution.succeeded',data:{sessionID:'synthetic-budget'}};"
        } else {
            ""
        }
    );
    std::fs::write(repo.repo.join("runner.mjs"), runner).unwrap();
    let start = Instant::now();
    let out = Command::new(interpreter)
        .arg("--no-warnings")
        .arg(repo.repo.join("runner.mjs"))
        .current_dir(&repo.repo)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &repo.home)
        .env("LIBRA_TEST_FORWARD_LOG", &log)
        .env("LIBRA_TEST_HUNG_PID", &pid_file)
        .output()
        .unwrap();
    let elapsed = start.elapsed();
    assert!(out.status.success(), "{}", describe(&out));
    assert!(out.stdout.is_empty(), "{}", describe(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        if hung {
            "libra opencode: forward_timeout\n"
        } else {
            ""
        }
    );
    if hung {
        let pid = std::fs::read_to_string(pid_file)
            .expect("hung stop child actually executed")
            .parse::<i32>()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "hung child still exists after SIGKILL timeout"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
    let frames = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    println!(
        "[executed] OG-02 production timeout/exit runtime: hung={hung}, exit={exit}, elapsed={elapsed:?}"
    );
    (frames, elapsed, source)
}

#[test]
fn opencode_plugin_sync_forward_within_deadline_budget() {
    let (frames, elapsed, source) = og02_run_lifecycle_plugin(false, false);
    assert_eq!(frames.len(), 2);
    assert!(
        elapsed < Duration::from_secs(3),
        "local metadata budget {elapsed:?}"
    );
    let ms = libra::internal::ai::observed_agents::opencode_export::EXPORT_DEADLINE.as_millis();
    assert!(source.contains(&format!("const EXPORT_DEADLINE_MS = {ms};")));
    assert!(source.contains("const FORWARD_TIMEOUT_MS = EXPORT_DEADLINE_MS + 15000;"));
}
#[test]
fn opencode_spawn_sync_timeout_kills_hung_gate() {
    let (frames, elapsed, _) = og02_run_lifecycle_plugin(true, false);
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[1]["hook_event_name"], "session.execution.succeeded");
    let timeout = libra::internal::ai::observed_agents::opencode_export::EXPORT_DEADLINE
        + Duration::from_secs(15);
    assert!(
        elapsed >= timeout,
        "hung gate did not exercise the production timeout"
    );
    assert!(
        elapsed < timeout + Duration::from_secs(4),
        "hung gate not bounded: {elapsed:?}"
    );
}
#[test]
fn exit_flush_orders_after_sync_session_start() {
    let (frames, elapsed, _) = og02_run_lifecycle_plugin(false, true);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["hook_event_name"], "session.created");
    assert_eq!(frames[1]["hook_event_name"], "server.instance.disposed");
    assert_eq!(frames[0]["session_id"], frames[1]["session_id"]);
    assert!(elapsed < Duration::from_secs(3));
}

#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
async fn opencode_produced_frames_reach_real_ingress_and_survive_history_rollover() {
    let repo = HookRepo::init();
    let installed = repo.run(&["agent", "enable", "--agent", "opencode"], None);
    assert!(installed.status.success(), "{}", describe(&installed));
    // The installed real binary literal remains byte-identical: no fake exporter.
    let plugin =
        std::fs::read_to_string(repo.repo.join(".opencode/plugin/libra-hooks.js")).unwrap();
    std::fs::write(repo.repo.join("plugin.mjs"), &plugin).unwrap();
    let runner = format!(
        r#"
import plugin from './plugin.mjs';
let finish;const finished=new Promise(resolve=>{{finish=resolve;}});let tool;
const id='synthetic-real-ingress';
const cleanup=await plugin.setup({{
 location:{{directory:{cwd}}},
 tool:{{hook:async(_name,callback)=>{{tool=callback;return {{dispose:async()=>{{}}}};}}}},
 event:{{subscribe:()=> (async function*(){{try{{
 yield {{type:'session.created',data:{{sessionID:id}}}};
 yield {{type:'session.step.started',data:{{sessionID:id,model:{{id:'synthetic-model'}}}}}};
 for(let i=0;i<260;i++){{
  const inboxID='synthetic-u-'+i;
  yield {{type:'session.inbox.enqueued',data:{{sessionID:id,inboxID,item:{{type:'user',payload:{{text:i===259?'a'.repeat(16385):'Synthetic prompt '+i}}}}}}}};
  yield {{type:'session.inbox.delivered',data:{{sessionID:id,inboxID}}}};
 }}
 // Duplicate inside the bounded live history never opens another turn.
 yield {{type:'session.inbox.delivered',data:{{sessionID:id,inboxID:'synthetic-u-259'}}}};
 yield {{type:'message.updated',data:{{info:{{sessionID:id,id:'synthetic-legacy',role:'user'}}}}}};
 yield {{type:'message.part.updated',data:{{part:{{sessionID:id,messageID:'synthetic-legacy',type:'text',text:'Synthetic legacy'}}}}}};
 yield {{type:'session.execution.interrupted',data:{{sessionID:id,reason:'user'}}}};
 yield {{type:'session.execution.succeeded',data:{{sessionID:id}}}};
 }}finally{{finish();}} }})()}}
}});
await finished;
tool({{sessionID:id,tool:'synthetic-tool',id:'synthetic-call',input:{{private:'PRIVATE-UNOBSERVED'}},status:'completed',result:{{private:'PRIVATE-UNOBSERVED'}}}});
await new Promise(resolve=>setTimeout(resolve,250));
await cleanup();
"#,
        cwd = serde_json::to_string(&repo.repo).unwrap()
    );
    std::fs::write(repo.repo.join("real-ingress.mjs"), runner).unwrap();
    let node = Command::new("node")
        .args(["-e", "process.stdout.write(process.execPath)"])
        .output()
        .expect("Node required for producer ingress");
    assert!(node.status.success());
    let out = Command::new(String::from_utf8(node.stdout).unwrap())
        .arg("--no-warnings")
        .arg(repo.repo.join("real-ingress.mjs"))
        .current_dir(&repo.repo)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &repo.home)
        .env("LIBRA_TEST_HOME", &repo.home)
        .output()
        .expect("Node required for producer-to-real-ingress gate");
    assert!(out.status.success(), "{}", describe(&out));
    assert!(out.stdout.is_empty(), "{}", describe(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "libra opencode: prompt_truncated\n",
        "real ingress must not emit forward_failed/forward_timeout"
    );
    let state = repo
        .durable_session("opencode__synthetic-real-ingress")
        .await;
    assert_eq!(state.state, "stopped");
    assert!(state.stopped_at.is_some());
    // 1 start + 260 user inbox prompts + 1 legacy prompt + 2 terminal events +
    // 1 tool observation + 1 session end. SessionEnd reserves then finalizes
    // the deferred terminal, adding one more revision (catalog completion);
    // no cliff after the 256-entry JS ring.
    assert_eq!(
        state.sync_revision, 267,
        "all unique producer frames must reach the real catalog"
    );
    assert_eq!(state.working_dir, repo.repo.display().to_string());
    let checkpoints = repo.checkpoints();
    assert!(
        !checkpoints.is_empty(),
        "real stop/end ingress creates committed checkpoints"
    );
    assert!(checkpoints.iter().all(|c| c["scope"] == "committed"));
    let receipts = state.metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .expect("stable message IDs reach the existing bounded receipt ledger");
    assert_eq!(
        receipts.len(),
        128,
        "existing catalog receipt ring remains bounded"
    );
    println!(
        "[executed] OG-02 unchanged installed module -> real Libra ingress, 260 prompts, model/reason/truncation/tool fields and durable stopped/checkpoints"
    );
}

fn claude_observe_text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn claude_observe_success_and_controllable_failure_are_silent() {
    let repo = HookRepo::init();
    let canary = "CC02-THINKING-CANARY";
    let payload_canary = "CC02-PAYLOAD-CANARY";
    let session = "cc02-observe";
    let started = repo.hook(
        "claude-code",
        "observe",
        &repo.envelope(
            "SessionStart",
            session,
            None,
            json!({ "thinking": canary, "source": "startup" }),
        ),
    );
    assert!(started.status.success(), "{}", describe(&started));
    let (stdout, stderr) = claude_observe_text(&started);
    assert!(
        stdout.is_empty(),
        "observe stdout must stay empty: {stdout}"
    );
    assert!(!stdout.contains(canary) && !stderr.contains(canary));
    assert!(!stdout.contains(payload_canary) && !stderr.contains(payload_canary));

    let listed = repo.run(&["agent", "session", "list", "--json"], None);
    let listed_text = String::from_utf8_lossy(&listed.stdout);
    assert!(
        !listed_text.contains(canary),
        "session list metadata must not contain the thinking body"
    );
    let missing = repo.hook(
        "claude-code",
        "observe",
        &json!({
            "hook_event_name": "SessionStart",
            "cwd": repo.repo.display().to_string(),
            payload_canary: "secret"
        })
        .to_string(),
    );
    assert!(missing.status.success(), "{}", describe(&missing));
    let (stdout, stderr) = claude_observe_text(&missing);
    assert!(stdout.is_empty(), "partial observe stdout must stay empty");
    assert!(!stdout.contains(payload_canary) && !stderr.contains(payload_canary));
    let still = repo.run(&["agent", "session", "list", "--json"], None);
    assert!(
        String::from_utf8_lossy(&still.stdout).contains(session),
        "a partial observe must not delete the session already recorded"
    );

    let oversize_marker = "CC02-OVERSIZE-CANARY";
    let mut oversize = format!(
        "{{\"hook_event_name\":\"SessionStart\",\"session_id\":\"{session}\",\"cwd\":\"{}\",\"note\":\"",
        repo.repo.display()
    );
    oversize.push_str(oversize_marker);
    oversize.push_str(&"x".repeat(libra::internal::ai::capture::ingress::MAX_STDIN_BYTES));
    oversize.push_str("\"}");
    let oversized = repo.run_bytes(
        &["agent", "hooks", "claude-code", "observe"],
        Some(oversize.as_bytes()),
    );
    assert!(oversized.status.success(), "{}", describe(&oversized));
    let (stdout, stderr) = claude_observe_text(&oversized);
    assert!(stdout.is_empty());
    assert!(!stdout.contains(oversize_marker) && !stderr.contains(oversize_marker));

    let hold = repo.hook(
        "claude-code",
        "observe",
        &repo.envelope("WorktreeCreate", session, None, json!({ "name": "hold" })),
    );
    assert!(hold.status.success(), "{}", describe(&hold));
    let (stdout, _) = claude_observe_text(&hold);
    assert!(
        stdout.is_empty(),
        "WorktreeCreate must not receive a path from observe"
    );
}

#[test]
fn claude_observe_message_display_drops_after_the_coalesce_limit() {
    use libra::internal::ai::{
        capture::ingress::{CaptureIngressOutcome, lower_in_process_capture_frame_for_test},
        hooks::{
            LifecycleEventKind, ProviderHookCommand, claude_provider,
            providers::claude::{
                MESSAGE_DISPLAY_COALESCE_LIMIT, reset_message_display_counts_for_test,
            },
        },
    };

    reset_message_display_counts_for_test();
    let provider = claude_provider();
    let mut kept = 0_u32;
    let mut dropped = 0_u32;
    for index in 0..(MESSAGE_DISPLAY_COALESCE_LIMIT + 1) {
        let payload = json!({
            "hook_event_name": "MessageDisplay",
            "session_id": "cc02-display",
            "cwd": "/tmp/cc02",
            "message_id": "m1",
            "index": index,
            "delta": "CC02-DISPLAY-CANARY"
        })
        .to_string();
        let outcome = lower_in_process_capture_frame_for_test(
            payload.as_bytes(),
            ProviderHookCommand::Observe,
            LifecycleEventKind::ToolUse,
            provider,
            None,
        )
        .expect("observe ingress accepts the frame");
        match outcome {
            CaptureIngressOutcome::Command(_) => kept += 1,
            CaptureIngressOutcome::UnknownEvent { .. } => dropped += 1,
        }
    }
    assert_eq!(kept, MESSAGE_DISPLAY_COALESCE_LIMIT);
    assert_eq!(dropped, 1);
}

/// CC-02 AC5: a malformed frame and a database that refuses writes are both
/// controllable failures. Observe stays silent with exit 0, neither failure
/// echoes the payload to stdout, stderr or the tracing log, and the session
/// recorded before the failures is still listed afterwards.
#[test]
fn claude_observe_parse_and_database_failures_keep_existing_data() {
    use std::os::unix::fs::PermissionsExt;

    let repo = HookRepo::init();
    let session = "cc02-failures";
    let canary = "CC02-FAILURE-CANARY";
    let log_file = repo.home.join("observe.log");
    let observe = |stdin: &[u8]| {
        let mut cmd = repo.command_in(&repo.repo);
        cmd.args(["agent", "hooks", "claude-code", "observe"])
            .env("LIBRA_LOG", "debug")
            .env("LIBRA_LOG_FILE", &log_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn libra observe");
        child
            .stdin
            .take()
            .expect("stdin piped")
            .write_all(stdin)
            .expect("write observe frame");
        child.wait_with_output().expect("wait for libra observe")
    };
    let assert_silent = |out: &Output, case: &str| {
        let (stdout, stderr) = claude_observe_text(out);
        assert!(out.status.success(), "{case}: {}", describe(out));
        assert!(stdout.is_empty(), "{case}: observe stdout must stay empty");
        assert!(
            !stderr.contains(canary) && !stderr.contains("panicked at"),
            "{case}: stderr must not echo the payload or panic: {stderr}"
        );
    };
    let session_listed = || {
        let listed = repo.run(&["agent", "session", "list", "--json"], None);
        String::from_utf8_lossy(&listed.stdout).contains(session)
    };

    let started = observe(
        repo.envelope(
            "SessionStart",
            session,
            None,
            json!({ "source": "startup" }),
        )
        .as_bytes(),
    );
    assert_silent(&started, "baseline");
    assert!(session_listed(), "baseline observe records the session");

    let malformed = format!(
        "{{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"{session}\",\"prompt\":\"{canary}\""
    );
    let parsed = observe(malformed.as_bytes());
    assert_silent(&parsed, "malformed JSON");
    assert!(
        session_listed(),
        "a parse failure must not delete the session"
    );

    let partials = || {
        std::fs::read_to_string(&log_file)
            .unwrap_or_default()
            .matches("claude_observe_partial")
            .count()
    };
    let before_refusal = partials();
    let database = repo.repo.join(".libra").join("libra.db");
    let original = std::fs::metadata(&database)
        .expect("stat repository database")
        .permissions();
    let mut read_only = original.clone();
    read_only.set_mode(0o444);
    std::fs::set_permissions(&database, read_only).expect("make database read-only");
    let refused = observe(
        repo.envelope(
            "UserPromptSubmit",
            session,
            None,
            json!({ "prompt": canary }),
        )
        .as_bytes(),
    );
    std::fs::set_permissions(&database, original).expect("restore database permissions");
    assert_silent(&refused, "read-only database");
    assert!(
        session_listed(),
        "a database write failure must not delete the session"
    );

    assert!(
        partials() > before_refusal,
        "the read-only database must take the controllable failure path"
    );
    let log = std::fs::read_to_string(&log_file).unwrap_or_default();
    assert!(
        !log.contains(canary),
        "the tracing log must not contain the payload body"
    );
}

/// CC-02 AC4 forward fix: every Claude hook call is a new process, so the
/// MessageDisplay cap must survive across processes. Separate observe
/// processes keep the first `MESSAGE_DISPLAY_COALESCE_LIMIT` batches, write no
/// catalog revision for later ones and only count them as dropped. Erase
/// removes the counts with the session.
#[tokio::test]
async fn claude_observe_message_display_limit_holds_across_processes() {
    use libra::internal::ai::hooks::providers::claude::MESSAGE_DISPLAY_COALESCE_LIMIT;

    let repo = HookRepo::init();
    let session = "cc02-display-durable";
    let libra_session = format!("claude__{session}");
    let limit = i64::from(MESSAGE_DISPLAY_COALESCE_LIMIT);
    let total = limit + 2;
    let display = |index: i64| {
        let out = repo.hook(
            "claude-code",
            "observe",
            &repo.envelope(
                "MessageDisplay",
                session,
                None,
                json!({ "message_id": format!("m{index}"), "index": 0 }),
            ),
        );
        assert!(out.status.success(), "batch {index}: {}", describe(&out));
        assert!(
            out.stdout.is_empty(),
            "batch {index}: stdout must stay empty"
        );
    };
    for index in 0..limit {
        display(index);
    }
    let at_limit = repo.durable_session(&libra_session).await.sync_revision;
    for index in limit..total {
        display(index);
    }
    let state = repo.durable_session(&libra_session).await;
    assert_eq!(
        state.sync_revision, at_limit,
        "batches past the budget must not reserve a catalog revision"
    );

    let counts = || async {
        let conn = repo.db().await;
        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT key, value FROM metadata_kv \
                 WHERE scope = 'agent_display_batch_count' AND target = ? ORDER BY key",
                [libra_session.clone().into()],
            ))
            .await
            .expect("query display-batch counts");
        rows.iter()
            .map(|row| {
                (
                    row.try_get_by::<String, _>("key").expect("decode key"),
                    row.try_get_by::<String, _>("value").expect("decode value"),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        counts().await,
        vec![
            ("dropped".to_string(), (total - limit).to_string()),
            ("kept".to_string(), limit.to_string()),
        ],
        "the budget and dropped count must persist across observe processes"
    );

    let storage = repo.repo.join(".libra");
    let conn = repo.db().await;
    libra::internal::ai::history::HistoryManager::new_with_ref(
        std::sync::Arc::new(
            libra::utils::client_storage::ClientStorage::init_local_existing(
                storage.join("objects"),
            ),
        ),
        storage,
        std::sync::Arc::new(conn.clone()),
        libra::internal::branch::TRACES_BRANCH,
    )
    .erase_session_local(&libra_session)
    .await
    .expect("erase the display-batch session");
    conn.close().await.expect("close erasure database");
    assert!(
        counts().await.is_empty(),
        "erase must remove the display-batch counts"
    );
}

fn hermes_observe_frame(repo: &HookRepo, event: &str, outcome: Option<&str>) -> String {
    let mut payload = json!({
        "hook_event_name": event,
        "session_id": "hermes-observe-1",
        "cwd": repo.repo.display().to_string(),
        "source": "cli",
        "profile_name": "default",
        "turn_id": "turn-1",
        "message_id": "msg-1",
        "phase": "turn",
        "event_id": "hermes-observe-1|turn-1|msg-1|turn",
    });
    if let Some(outcome) = outcome {
        payload["outcome"] = json!(outcome);
    }
    serde_json::to_string(&payload).expect("hermes frame")
}

#[test]
fn hermes_observe_turn_end_stays_active_and_replays_once() {
    let repo = HookRepo::init();
    let frame = hermes_observe_frame(&repo, "on_session_end", Some("completed"));
    let first = repo.hook("hermes", "observe", &frame);
    assert!(
        first.status.success(),
        "turn observer failed: {}",
        describe(&first)
    );
    let sessions = repo.sessions();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_ne!(sessions[0]["state"], json!("stopped"), "{sessions:?}");
    let checkpoints = repo.checkpoints().len();
    assert!(checkpoints >= 1, "turn observer wrote no checkpoint");
    let second = repo.hook("hermes", "observe", &frame);
    assert!(
        second.status.success(),
        "replay failed: {}",
        describe(&second)
    );
    assert_eq!(
        repo.checkpoints().len(),
        checkpoints,
        "replay wrote another checkpoint"
    );
    assert_eq!(repo.sessions().len(), 1);
}

#[test]
fn hermes_observe_subdirectory_cwd_does_not_capture() {
    let repo = HookRepo::init();
    let nested = repo.repo.join("nested");
    std::fs::create_dir_all(&nested).expect("nested workdir");
    let mut frame: Value = serde_json::from_str(&hermes_observe_frame(
        &repo,
        "on_session_end",
        Some("completed"),
    ))
    .expect("frame");
    frame["cwd"] = json!(nested.display().to_string());
    let out = repo.run_in(
        &nested,
        &["agent", "hooks", "hermes", "observe"],
        Some(&serde_json::to_string(&frame).unwrap()),
    );
    assert!(
        repo.sessions().is_empty(),
        "subdirectory cwd created a session: {}",
        describe(&out)
    );
    assert!(repo.checkpoints().is_empty());
}

#[test]
fn hermes_observe_finalize_stops_the_session() {
    let repo = HookRepo::init();
    let turn = hermes_observe_frame(&repo, "on_session_end", Some("completed"));
    assert!(repo.hook("hermes", "observe", &turn).status.success());
    let end = hermes_observe_frame(&repo, "on_session_finalize", None);
    let stopped = repo.hook("hermes", "observe", &end);
    assert!(
        stopped.status.success(),
        "finalize failed: {}",
        describe(&stopped)
    );
    let sessions = repo.sessions();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(sessions[0]["state"], json!("stopped"), "{sessions:?}");
}

#[test]
fn hermes_observe_gateway_control_and_body_do_not_capture() {
    let repo = HookRepo::init();
    let mut gateway = serde_json::from_str::<Value>(&hermes_observe_frame(
        &repo,
        "on_session_end",
        Some("completed"),
    ))
    .unwrap();
    gateway["source"] = json!("gateway");
    let refused = repo.hook(
        "hermes",
        "observe",
        &serde_json::to_string(&gateway).unwrap(),
    );
    assert!(
        repo.sessions().is_empty(),
        "gateway created a session: {}",
        describe(&refused)
    );
    assert!(repo.checkpoints().is_empty());

    let control = repo.hook(
        "hermes",
        "observe",
        &json!({
            "hook_event_name": "pre_tool_call",
            "session_id": "hermes-observe-1",
            "cwd": repo.repo.display().to_string(),
            "tool_input": "secret-tool-args",
        })
        .to_string(),
    );
    assert!(control.status.success(), "{}", describe(&control));
    assert!(repo.sessions().is_empty());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&control.stdout),
        String::from_utf8_lossy(&control.stderr)
    );
    assert!(!combined.contains("secret-tool-args"));

    let mut body =
        serde_json::from_str::<Value>(&hermes_observe_frame(&repo, "post_llm_call", None)).unwrap();
    body["history"] = json!("secret-thinking-body");
    let echoed = repo.hook("hermes", "observe", &serde_json::to_string(&body).unwrap());
    assert!(repo.sessions().is_empty(), "{}", describe(&echoed));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&echoed.stdout),
        String::from_utf8_lossy(&echoed.stderr)
    );
    assert!(!combined.contains("secret-thinking-body"));

    let other_verb = repo.hook(
        "hermes",
        "stop",
        &hermes_observe_frame(&repo, "on_session_end", Some("completed")),
    );
    assert!(!other_verb.status.success(), "stop must stay unregistered");
    assert!(repo.sessions().is_empty());
}

#[test]
fn hermes_observe_plugin_forwards_only_the_allowlist() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join("libra-hermes-observer-root");
    std::fs::create_dir_all(root.join(".libra")).expect("plugin worktree marker");
    let script = root.join("libra-stub");
    let captured = root.join("stdin.json");
    let env_log = root.join("child-env.txt");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncat > '{}'\nenv > '{}'\nexit 0\n",
            captured.display(),
            env_log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let payload = json!({
        "event": "on_session_end",
        "session_id": "hermes-observe-1",
        "cwd": root.display().to_string(),
        "source": "cli",
        "profile_name": "default",
        "turn_id": "turn-1",
        "message_id": "msg-1",
        "phase": "turn",
        "outcome": "completed",
        "history": "secret-thinking-body",
        "child_goal": "secret-child",
    });
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/internal/ai/hooks/providers/hermes/libra_observer.py"
        ))
        .arg("--hook")
        .env("LIBRA_BIN", &script)
        .env("PATH", "/usr/bin:/bin")
        .env("HERMES_HOME", root.as_os_str())
        .env("OPENAI_API_KEY", "parent-secret")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("plugin");
    let mut child = output;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let finished = child.wait_with_output().expect("plugin wait");
    assert!(
        finished.status.success(),
        "{}",
        String::from_utf8_lossy(&finished.stderr)
    );
    assert!(
        finished.stdout.is_empty(),
        "observer stdout must stay empty"
    );
    let frame: Value =
        serde_json::from_str(&std::fs::read_to_string(&captured).expect("frame")).unwrap();
    for key in [
        "history",
        "child_goal",
        "secret-thinking-body",
        "secret-child",
    ] {
        assert!(!frame.to_string().contains(key), "{frame}");
    }
    assert_eq!(frame["hook_event_name"], json!("on_session_end"));
    assert_eq!(frame["source"], json!("cli"));
    let child_env = std::fs::read_to_string(&env_log).expect("child env");
    let home = root.display().to_string();
    assert!(
        child_env.contains(&format!("HERMES_HOME={home}")),
        "plugin dropped HERMES_HOME: {child_env}"
    );
    assert!(
        !child_env.contains("OPENAI_API_KEY") && !child_env.contains("parent-secret"),
        "plugin forwarded a parent secret: {child_env}"
    );
}

// ---------------------------------------------------------------------------
// PI-04: Pi observe extension and receiver (`pi_observe_*`).
// ---------------------------------------------------------------------------

const PI_SESSION_ID: &str = "019a0c3e-7b1d-7c2e-9f10-5a6b7c8d9e0f";

/// The absolute path of the Node interpreter on `PATH`.
fn pi_node() -> String {
    let out = Command::new("node")
        .args(["-e", "process.stdout.write(process.execPath)"])
        .output()
        .expect("Node is required to run the Pi extension");
    assert!(out.status.success(), "node -e failed: {}", describe(&out));
    String::from_utf8(out.stdout).expect("node path is UTF-8")
}

/// A fake libra executable that logs its argv and stdin frame.
fn pi_fake_libra(repo: &HookRepo, node: &str, exit_code: i32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let fake = repo.home.join(format!("fake-libra-{exit_code}.mjs"));
    std::fs::write(
        &fake,
        format!(
            "#!{node}\nimport {{ readFileSync, appendFileSync }} from 'node:fs';\n\
             const frame = readFileSync(0, 'utf8');\n\
             appendFileSync(process.env.LIBRA_TEST_PI_LOG, JSON.stringify({{ argv: process.argv.slice(2), frame: JSON.parse(frame) }}) + '\\n');\n\
             process.exit({exit_code});\n"
        ),
    )
    .expect("write fake libra");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700))
        .expect("make fake libra executable");
    fake
}

/// Events, in ADR-PI-06 order, with payloads carrying every field Pi sends,
/// including bodies that must never be forwarded.
fn pi_event_payloads(nested: bool) -> Vec<(&'static str, Value)> {
    let parent = if nested { json!("call-1") } else { Value::Null };
    let tool_id = if nested { "call-1/1" } else { "call-1" };
    vec![
        (
            "session_start",
            if nested {
                json!({"type": "session_start", "reason": "resume", "previousSessionFile": "/tmp/prev.jsonl"})
            } else {
                json!({"type": "session_start", "reason": "startup"})
            },
        ),
        (
            "before_agent_start",
            json!({"type": "before_agent_start", "messageType": "user", "prompt": "PI_CANARY_PROMPT", "systemPrompt": "PI_CANARY_SYSTEM"}),
        ),
        ("tool_call", {
            let mut call = json!({"type": "tool_call", "toolCallId": tool_id, "toolName": "bash", "input": {"command": "PI_CANARY_INPUT"}});
            if nested {
                call["parentToolCallId"] = parent.clone();
            }
            call
        }),
        ("tool_result", {
            let mut result = json!({"type": "tool_result", "toolCallId": tool_id, "toolName": "bash", "input": {}, "content": [{"type": "text", "text": "PI_CANARY_OUTPUT"}], "isError": false});
            if nested {
                result["parentToolCallId"] = parent.clone();
            }
            result
        }),
        (
            "turn_end",
            json!({"type": "turn_end", "turnIndex": 0, "message": {"role": "assistant", "content": [{"type": "thinking", "thinking": "PI_CANARY_THINKING"}, {"type": "text", "text": "PI_CANARY_ANSWER"}]}, "toolResults": [], "messageEntryId": "entry-2", "toolResultEntryIds": ["entry-3"], "outcome": "completed"}),
        ),
        (
            "agent_settled",
            json!({"type": "agent_settled", "aborted": false}),
        ),
        (
            "session_shutdown",
            if nested {
                json!({"type": "session_shutdown", "reason": "new", "targetSessionFile": "/tmp/next.jsonl"})
            } else {
                json!({"type": "session_shutdown", "reason": "quit"})
            },
        ),
    ]
}

struct PiExtensionRun {
    registered: Vec<String>,
    returned_undefined: Vec<bool>,
    forwarded: Vec<Value>,
    output: Output,
}

/// Load the rendered extension in Node with a fake Pi API, fire `events`
/// and collect what each handler returned and what reached `command`.
/// `ctx` is a JavaScript object expression for the extension context.
fn pi_run_extension(
    repo: &HookRepo,
    node: &str,
    command: &Path,
    ctx: &str,
    events: &[(&str, Value)],
) -> PiExtensionRun {
    let asset = libra::internal::ai::hooks::providers::pi::settings::render_pi_asset(command)
        .expect("render the Pi extension");
    let work = repo.home.join("pi-ext");
    std::fs::create_dir_all(&work).expect("extension work dir");
    std::fs::write(work.join("extension.mjs"), asset).expect("write extension module");
    let log = work.join("forwarded.jsonl");
    let _ = std::fs::remove_file(&log);
    let events = serde_json::to_string(
        &events
            .iter()
            .map(|(name, payload)| json!([name, payload]))
            .collect::<Vec<_>>(),
    )
    .expect("events");
    let runner = format!(
        r#"import factory from './extension.mjs';
const handlers = new Map();
factory({{ on(name, handler) {{ handlers.set(name, handler); }} }});
const ctx = {ctx};
const returned = [];
for (const [name, payload] of {events}) {{
  const value = handlers.get(name)(payload, ctx);
  returned.push(value === undefined);
}}
process.stdout.write(JSON.stringify({{ registered: [...handlers.keys()], returned }}));
"#
    );
    std::fs::write(work.join("runner.mjs"), runner).expect("write runner");
    let output = Command::new(node)
        .arg("--no-warnings")
        .arg(work.join("runner.mjs"))
        .current_dir(&work)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &repo.home)
        .env("LIBRA_TEST_PI_LOG", &log)
        .output()
        .expect("run the Pi extension");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    let forwarded = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("forwarded line"))
        .collect();
    PiExtensionRun {
        registered: report["registered"]
            .as_array()
            .map(|names| {
                names
                    .iter()
                    .map(|name| name.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default(),
        returned_undefined: report["returned"]
            .as_array()
            .map(|values| values.iter().map(|value| value == &json!(true)).collect())
            .unwrap_or_default(),
        forwarded,
        output,
    }
}

fn pi_ctx(repo: &HookRepo, trusted: &str, session_file: &str) -> String {
    format!(
        "{{ cwd: {cwd}, isProjectTrusted: () => {trusted}, sessionManager: {{ getSessionId: () => {id}, getSessionFile: () => {session_file} }} }}",
        cwd = serde_json::to_string(&repo.repo).unwrap(),
        id = serde_json::to_string(PI_SESSION_ID).unwrap(),
    )
}

/// A fixed `frameId` for fixture frames; `n` tells frames apart.
fn pi_frame_id(n: u32) -> String {
    format!("5f0c2a7e-9b4d-4c1e-8a3f-{n:012x}")
}

/// One receiver frame per ADR-PI-06 event.
fn pi_frame(repo: &HookRepo, event: &str) -> Value {
    let mut frame = json!({
        "schema": 3,
        "event": event,
        "frameId": pi_frame_id(0),
        "sessionId": PI_SESSION_ID,
        "sessionFile": null,
        "cwd": repo.repo.display().to_string(),
        "emittedAt": "2026-11-02T17:00:00.000Z",
    });
    let fields = match event {
        "session_start" => json!({"reason": "startup", "previousSessionFile": null}),
        "session_shutdown" => json!({"reason": "quit", "targetSessionFile": null}),
        "before_agent_start" => json!({"messageType": "user"}),
        "turn_end" => {
            json!({"turnIndex": 0, "messageEntryId": "entry-2", "toolResultEntryIds": [], "outcome": "completed"})
        }
        "tool_call" | "tool_result" => {
            json!({"toolCallId": "call-1", "parentToolCallId": null, "toolName": "bash"})
        }
        "agent_settled" => json!({"aborted": false, "outcome": "completed"}),
        other => panic!("no fixture for {other}"),
    };
    for (key, value) in fields.as_object().unwrap() {
        frame[key] = value.clone();
    }
    frame
}

/// ADR-PI-06 field sets, written out here rather than taken from the parser
/// so a matching omission in both the extension and the receiver still fails.
fn pi_expected_keys(event: &str) -> std::collections::BTreeSet<String> {
    let common = [
        "schema",
        "event",
        "frameId",
        "sessionId",
        "sessionFile",
        "cwd",
        "emittedAt",
    ];
    let specific: &[&str] = match event {
        "session_start" => &["reason", "previousSessionFile"],
        "session_shutdown" => &["reason", "targetSessionFile"],
        "before_agent_start" => &["messageType"],
        "turn_end" => &[
            "turnIndex",
            "messageEntryId",
            "toolResultEntryIds",
            "outcome",
        ],
        "tool_call" | "tool_result" => &["toolCallId", "parentToolCallId", "toolName"],
        "agent_settled" => &["aborted", "outcome"],
        other => panic!("ADR-PI-06 has no field set for {other}"),
    };
    common
        .iter()
        .chain(specific.iter())
        .map(|key| key.to_string())
        .collect()
}

#[test]
fn pi_observe_forwards_only_while_the_project_is_trusted() {
    let repo = HookRepo::init();
    let node = pi_node();
    let fake = pi_fake_libra(&repo, &node, 0);
    let events = pi_event_payloads(false);
    for ctx in [
        pi_ctx(&repo, "false", "undefined"),
        // A context without the trust API, or one that throws, is untrusted.
        format!(
            "{{ cwd: {}, sessionManager: {{ getSessionId: () => 'x', getSessionFile: () => undefined }} }}",
            serde_json::to_string(&repo.repo).unwrap()
        ),
        pi_ctx(
            &repo,
            "{ throw new Error('trust unavailable'); }",
            "undefined",
        ),
        pi_ctx(&repo, "'yes'", "undefined"),
    ] {
        let run = pi_run_extension(&repo, &node, &fake, &ctx, &events);
        assert!(run.output.status.success(), "{}", describe(&run.output));
        assert!(
            run.forwarded.is_empty(),
            "an untrusted project forwarded {:?}",
            run.forwarded
        );
    }
    let run = pi_run_extension(
        &repo,
        &node,
        &fake,
        &pi_ctx(&repo, "true", "undefined"),
        &events,
    );
    assert!(run.output.status.success(), "{}", describe(&run.output));
    assert_eq!(
        run.forwarded.len(),
        events.len(),
        "a trusted project forwards every observed event"
    );
}

#[test]
fn pi_observe_asset_write_leaves_unmanaged_and_drifted_files() {
    use libra::internal::ai::hooks::providers::pi::settings::{
        PiAssetOwnership, PiAssetWrite, pi_asset_ownership, pi_asset_path, render_pi_asset,
        write_owned_pi_asset,
    };
    let repo = HookRepo::init();
    let binary = std::fs::canonicalize(env!("CARGO_BIN_EXE_libra")).expect("libra binary");
    let path = pi_asset_path(&repo.repo);
    assert_eq!(
        pi_asset_ownership(&repo.repo).unwrap(),
        PiAssetOwnership::Absent
    );
    assert_eq!(
        write_owned_pi_asset(&repo.repo, &binary).unwrap(),
        PiAssetWrite::Created
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        render_pi_asset(&binary).unwrap()
    );
    assert_eq!(
        write_owned_pi_asset(&repo.repo, &binary).unwrap(),
        PiAssetWrite::Unchanged
    );
    // An existing file is never replaced, even an owned asset for another
    // binary: replacing a path cannot prove who owns it at that moment.
    let other = repo.home.join("other-libra");
    let older = render_pi_asset(&other).unwrap();
    std::fs::write(&path, &older).unwrap();
    assert_eq!(
        pi_asset_ownership(&repo.repo).unwrap(),
        PiAssetOwnership::Owned
    );
    let error = write_owned_pi_asset(&repo.repo, &binary)
        .expect_err("an existing owned asset must not be replaced");
    assert!(
        error
            .to_string()
            .contains("does not replace an existing file"),
        "{error:#}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), older);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        write_owned_pi_asset(&repo.repo, &binary).unwrap(),
        PiAssetWrite::Created
    );

    let user_file = "export default function mine(pi) {}\n";
    let mut drifted = render_pi_asset(&binary).unwrap();
    drifted.push_str("// local edit\n");
    for (label, bytes) in [("unmanaged", user_file.to_string()), ("drifted", drifted)] {
        std::fs::write(&path, &bytes).unwrap();
        let error = write_owned_pi_asset(&repo.repo, &binary)
            .expect_err("a user or drifted file must not be overwritten");
        assert!(
            error.to_string().contains("leaves it as it is"),
            "{label}: {error:#}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            bytes,
            "{label} file changed"
        );
    }

    // A symlink in the slot, or a symlinked extensions directory, is never followed.
    #[cfg(unix)]
    {
        let target = repo.home.join("target.ts");
        std::fs::write(&target, user_file).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(write_owned_pi_asset(&repo.repo, &binary).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), user_file);
        std::fs::remove_file(&path).unwrap();
        let extensions = path.parent().unwrap().to_path_buf();
        std::fs::remove_dir(&extensions).unwrap();
        let elsewhere = repo.home.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &extensions).unwrap();
        assert!(write_owned_pi_asset(&repo.repo, &binary).is_err());
        assert!(
            std::fs::read_dir(&elsewhere).unwrap().next().is_none(),
            "nothing may be written through a symlinked directory"
        );

        // A directory swapped for a link after the slot is checked does not
        // redirect the write: the file lands in the directory that was checked.
        use libra::internal::ai::hooks::providers::pi::settings::write_owned_pi_asset_pausing_for_test;
        let project = repo.home.join("swap-project");
        std::fs::create_dir_all(&project).unwrap();
        let outside = repo.home.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let extensions = pi_asset_path(&project).parent().unwrap().to_path_buf();
        let moved = extensions.with_file_name("extensions-checked");
        let written = write_owned_pi_asset_pausing_for_test(&project, &binary, &mut || {
            std::fs::rename(&extensions, &moved).unwrap();
            std::os::unix::fs::symlink(&outside, &extensions).unwrap();
        })
        .expect("the write goes to the checked directory");
        assert_eq!(written, PiAssetWrite::Created);
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "a swapped-in link redirected the write"
        );
        assert_eq!(
            std::fs::read_to_string(moved.join("libra-hooks.ts")).unwrap(),
            render_pi_asset(&binary).unwrap()
        );
    }
}

#[test]
fn pi_observe_callbacks_return_undefined() {
    let repo = HookRepo::init();
    let node = pi_node();
    let fake = pi_fake_libra(&repo, &node, 0);
    let events = pi_event_payloads(false);
    for trusted in ["true", "false"] {
        let run = pi_run_extension(
            &repo,
            &node,
            &fake,
            &pi_ctx(&repo, trusted, "undefined"),
            &events,
        );
        assert!(run.output.status.success(), "{}", describe(&run.output));
        assert_eq!(
            run.registered,
            [
                "session_start",
                "session_shutdown",
                "before_agent_start",
                "turn_end",
                "tool_call",
                "tool_result",
                "agent_settled"
            ],
            "the extension subscribes to exactly the ADR-PI-06 events"
        );
        assert_eq!(
            run.returned_undefined,
            vec![true; events.len()],
            "every handler must return undefined (trusted={trusted})"
        );
    }
}

#[test]
fn pi_observe_delivery_errors_keep_the_provider_result() {
    let repo = HookRepo::init();
    let node = pi_node();
    let events = pi_event_payloads(false);
    let failing = pi_fake_libra(&repo, &node, 3);
    let missing = repo.home.join("no-such-libra");
    for (command, ctx) in [
        (missing.clone(), pi_ctx(&repo, "true", "undefined")),
        (failing.clone(), pi_ctx(&repo, "true", "undefined")),
        (
            failing.clone(),
            format!(
                "{{ cwd: {}, isProjectTrusted: () => true, sessionManager: {{ getSessionId: () => {{ throw new Error('boom'); }}, getSessionFile: () => undefined }} }}",
                serde_json::to_string(&repo.repo).unwrap()
            ),
        ),
        (
            failing.clone(),
            format!(
                "{{ cwd: {}, isProjectTrusted: () => true, sessionManager: {{ getSessionId: () => {}, getSessionFile: () => undefined }} }}",
                serde_json::to_string(&repo.home.join("gone")).unwrap(),
                serde_json::to_string(PI_SESSION_ID).unwrap()
            ),
        ),
    ] {
        let run = pi_run_extension(&repo, &node, &command, &ctx, &events);
        assert!(
            run.output.status.success(),
            "a delivery error must not fail the provider: {}",
            describe(&run.output)
        );
        assert!(
            run.output.stderr.is_empty(),
            "a delivery error must not surface in Pi: {}",
            describe(&run.output)
        );
        assert_eq!(run.returned_undefined, vec![true; events.len()]);
    }
}

#[test]
fn pi_observe_envelope_fields_match_adr_pi_06() {
    let repo = HookRepo::init();
    let node = pi_node();
    let fake = pi_fake_libra(&repo, &node, 0);
    for (nested, session_file) in [(false, "undefined"), (true, "'/tmp/current.jsonl'")] {
        let events = pi_event_payloads(nested);
        let run = pi_run_extension(
            &repo,
            &node,
            &fake,
            &pi_ctx(&repo, "true", session_file),
            &events,
        );
        assert!(run.output.status.success(), "{}", describe(&run.output));
        assert_eq!(run.forwarded.len(), events.len());
        // Children finish in any order; match each frame by its event name.
        for (event, _) in &events {
            let matching: Vec<&Value> = run
                .forwarded
                .iter()
                .filter(|forwarded| forwarded["frame"]["event"] == json!(event))
                .collect();
            assert_eq!(matching.len(), 1, "{event}: {:?}", run.forwarded);
            let forwarded = matching[0];
            assert_eq!(
                forwarded["argv"],
                json!([
                    "agent",
                    "hooks",
                    "--capture-budget-ms",
                    "1500",
                    "pi",
                    "observe"
                ])
            );
            let frame = &forwarded["frame"];
            let keys: std::collections::BTreeSet<String> =
                frame.as_object().unwrap().keys().cloned().collect();
            assert_eq!(keys, pi_expected_keys(event), "{event}: {frame}");
            assert_eq!(frame["event"], json!(event));
            assert_eq!(frame["schema"], json!(3));
            let frame_id = frame["frameId"].as_str().expect("frameId");
            assert_eq!(
                uuid::Uuid::parse_str(frame_id)
                    .map(|id| id.hyphenated().to_string())
                    .ok()
                    .as_deref(),
                Some(frame_id),
                "{event}: {frame}"
            );
            assert_eq!(frame["sessionId"], json!(PI_SESSION_ID));
            assert_eq!(frame["cwd"], json!(repo.repo.display().to_string()));
            if nested {
                assert_eq!(frame["sessionFile"], json!("/tmp/current.jsonl"));
            } else {
                assert_eq!(frame["sessionFile"], Value::Null, "{event}");
            }
            match *event {
                "session_start" => assert_eq!(
                    frame["previousSessionFile"],
                    if nested {
                        json!("/tmp/prev.jsonl")
                    } else {
                        Value::Null
                    }
                ),
                "session_shutdown" => assert_eq!(
                    frame["targetSessionFile"],
                    if nested {
                        json!("/tmp/next.jsonl")
                    } else {
                        Value::Null
                    }
                ),
                "tool_call" | "tool_result" => assert_eq!(
                    frame["parentToolCallId"],
                    if nested { json!("call-1") } else { Value::Null }
                ),
                _ => {}
            }
            // The extension's output is what the receiver accepts.
            let out = repo.hook("pi", "observe", &frame.to_string());
            assert!(out.status.success(), "{event}: {}", describe(&out));
        }
        let frame_ids: std::collections::BTreeSet<String> = run
            .forwarded
            .iter()
            .map(|forwarded| forwarded["frame"]["frameId"].to_string())
            .collect();
        assert_eq!(
            frame_ids.len(),
            events.len(),
            "every frame gets its own frameId"
        );
        let text = serde_json::to_string(&run.forwarded).unwrap();
        for canary in [
            "PI_CANARY_PROMPT",
            "PI_CANARY_SYSTEM",
            "PI_CANARY_INPUT",
            "PI_CANARY_OUTPUT",
            "PI_CANARY_THINKING",
            "PI_CANARY_ANSWER",
        ] {
            assert!(!text.contains(canary), "{canary} was forwarded");
        }
    }
}

#[test]
fn pi_observe_receiver_refuses_frames_outside_adr_pi_06() {
    let repo = HookRepo::init();
    let mut refused = Vec::new();
    let start = pi_frame(&repo, "session_start");
    let mut extra = start.clone();
    extra["prompt"] = json!("hello");
    refused.push(("extra field", extra));
    let mut missing = start.clone();
    missing.as_object_mut().unwrap().remove("reason");
    refused.push(("missing required field", missing));
    let mut no_nullable = start.clone();
    no_nullable
        .as_object_mut()
        .unwrap()
        .remove("previousSessionFile");
    refused.push(("missing nullable key", no_nullable));
    let mut empty = start.clone();
    empty["reason"] = json!("");
    refused.push(("empty required field", empty));
    for version in [1, 2] {
        let mut schema = start.clone();
        schema["schema"] = json!(version);
        refused.push(("other schema", schema));
    }
    let mut bad_outcome = pi_frame(&repo, "agent_settled");
    bad_outcome["outcome"] = json!("interrupted");
    refused.push(("agent_settled outcome outside the envelope", bad_outcome));
    let mut no_frame_id = start.clone();
    no_frame_id.as_object_mut().unwrap().remove("frameId");
    refused.push(("missing frameId", no_frame_id));
    for bad in [
        json!("frame-1"),
        json!(pi_frame_id(0).to_uppercase()),
        json!(pi_frame_id(0).replace('-', "")),
        json!(7),
    ] {
        let mut frame_id = start.clone();
        frame_id["frameId"] = bad;
        refused.push(("frameId not a lowercase hyphenated UUID", frame_id));
    }
    let mut snake = start.clone();
    snake["hook_event_name"] = json!("session_start");
    refused.push(("shared envelope field", snake));
    let mut tool = pi_frame(&repo, "tool_call");
    tool["toolCallId"] = json!(7);
    refused.push(("wrong type", tool));
    for (label, frame) in &refused {
        let out = repo.hook("pi", "observe", &frame.to_string());
        assert!(out.status.success(), "{label}: {}", describe(&out));
        assert!(
            repo.sessions().is_empty(),
            "{label}: a refused frame wrote a session"
        );
    }
    // A repeated key is refused rather than resolved to its last value.
    let text = start.to_string();
    let duplicated = text.replacen("{", "{\"reason\":\"startup\",", 1);
    assert_ne!(duplicated, text);
    let out = repo.hook("pi", "observe", &duplicated);
    assert!(out.status.success(), "repeated key: {}", describe(&out));
    assert!(
        repo.sessions().is_empty(),
        "a frame with a repeated key wrote a session"
    );
    assert!(repo.checkpoints().is_empty());
}

#[tokio::test]
async fn pi_observe_receiver_records_adr_frames() {
    let repo = HookRepo::init();
    let start = pi_frame(&repo, "session_start");
    let out = repo.hook("pi", "observe", &start.to_string());
    assert!(out.status.success(), "{}", describe(&out));
    let sessions = repo.sessions();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(sessions[0]["agent_kind"], json!("pi"), "{sessions:?}");
    // The recorded projection: lifecycle kind, tool name, and no content.
    {
        use libra::internal::ai::{
            capture::ingress::CaptureIngressOutcome, hooks::providers::pi_provider,
        };
        let provider = pi_provider();
        for (event, kind, tool) in [
            ("session_start", LifecycleEventKind::SessionStart, None),
            ("before_agent_start", LifecycleEventKind::TurnStart, None),
            ("tool_call", LifecycleEventKind::ToolUse, Some("bash")),
            ("tool_result", LifecycleEventKind::ToolUse, Some("bash")),
            ("agent_settled", LifecycleEventKind::TurnEnd, None),
        ] {
            let frame = pi_frame(&repo, event).to_string();
            match lower_in_process_capture_frame_for_test(
                frame.as_bytes(),
                ProviderHookCommand::Observe,
                ProviderHookCommand::Observe.lifecycle_event_kind(),
                provider,
                Some(&repo.repo),
            )
            .expect("valid frame passes ingress")
            {
                CaptureIngressOutcome::Command(command) => {
                    assert_eq!(command.event_kind(), kind, "{event}");
                    assert_eq!(command.provider_kind(), "pi");
                }
                CaptureIngressOutcome::UnknownEvent { .. } => {
                    panic!("{event} must be recorded")
                }
            }
            let envelope = provider
                .lower_native_frame(&frame)
                .expect("pi lowers its own frame")
                .expect("valid frame lowers");
            let parsed = provider
                .parse_hook_event(event, &envelope)
                .expect("recorded event parses");
            assert_eq!(parsed.kind, kind, "{event}");
            assert_eq!(parsed.tool_name.as_deref(), tool, "{event}");
            assert!(
                parsed.prompt.is_none()
                    && parsed.tool_input.is_none()
                    && parsed.tool_response.is_none()
                    && parsed.assistant_message.is_none()
                    && parsed.session_ref.is_none(),
                "{event} must carry no content"
            );
        }
        for event in ["session_shutdown", "turn_end"] {
            let frame = pi_frame(&repo, event).to_string();
            assert!(
                matches!(
                    lower_in_process_capture_frame_for_test(
                        frame.as_bytes(),
                        ProviderHookCommand::Observe,
                        ProviderHookCommand::Observe.lifecycle_event_kind(),
                        provider,
                        Some(&repo.repo),
                    )
                    .expect("valid frame passes ingress"),
                    CaptureIngressOutcome::UnknownEvent { .. }
                ),
                "{event} is accepted on the wire but not recorded yet"
            );
        }
    }

    let libra_session = format!("pi__{PI_SESSION_ID}");
    let mut revision = repo.durable_session(&libra_session).await.sync_revision;
    for event in ["tool_call", "tool_result"] {
        let out = repo.hook("pi", "observe", &pi_frame(&repo, event).to_string());
        assert!(out.status.success(), "{event}: {}", describe(&out));
        let after = repo.durable_session(&libra_session).await;
        assert!(
            after.sync_revision > revision,
            "{event} must be recorded on the durable session, not skipped"
        );
        assert_eq!(after.state, "active");
        revision = after.sync_revision;
    }
    // A wire event this receiver does not record leaves the session as it is.
    let out = repo.hook("pi", "observe", &pi_frame(&repo, "turn_end").to_string());
    assert!(out.status.success(), "turn_end: {}", describe(&out));
    assert_eq!(
        repo.durable_session(&libra_session).await.sync_revision,
        revision
    );
    assert_eq!(repo.sessions().len(), 1);
    assert!(
        repo.checkpoints().is_empty(),
        "tool events and session start write no checkpoint"
    );
    let verb = repo.hook("pi", "session-start", &start.to_string());
    assert!(
        !verb.status.success(),
        "pi registers only the observe entry"
    );
}

// ---------------------------------------------------------------------------
// PI-13: the Pi extension's child-process resource contract (`pi_child_contract_*`).
// ---------------------------------------------------------------------------

/// A fake libra that logs `start`/`end` lines (pid, time, argv, own path) and
/// then behaves as `mode` says: `exit:<code>`, `sleep:<ms>:<code>`, `hang`
/// (ignores SIGTERM and never exits on its own), or `retry-then-hang:<ms>:<n>`
/// (attempts before the `n`th sleep `ms` and exit 1; the `n`th hangs).
fn pi_contract_fake(dir: &Path, node: &str, name: &str, mode: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).expect("fake libra dir");
    let fake = dir.join(name);
    std::fs::write(
        &fake,
        format!(
            r#"#!{node}
import {{ readFileSync, appendFileSync }} from 'node:fs';
const log = (phase) => appendFileSync(process.env.LIBRA_TEST_PI_LOG, JSON.stringify({{ phase, pid: process.pid, t: Date.now(), argv: process.argv.slice(2), self: process.argv[1] }}) + '\n');
readFileSync(0, 'utf8');
log('start');
const mode = {mode};
const attempt = readFileSync(process.env.LIBRA_TEST_PI_LOG, 'utf8').split('\n').filter((line) => line.includes('"phase":"start"')).length;
const hang = () => {{ process.on('SIGTERM', () => {{}}); setInterval(() => {{}}, 1000); }};
if (mode === 'hang') {{
  hang();
}} else if (mode.startsWith('retry-then-hang:')) {{
  const [, ms, n] = mode.split(':');
  if (attempt >= Number(n)) {{
    hang();
  }} else {{
    setTimeout(() => {{ log('end'); process.exit(1); }}, Number(ms));
  }}
}} else if (mode.startsWith('sleep:')) {{
  const [, ms, code] = mode.split(':');
  setTimeout(() => {{ log('end'); process.exit(Number(code)); }}, Number(ms));
}} else {{
  log('end');
  process.exit(Number(mode.split(':')[1]));
}}
"#,
            mode = serde_json::to_string(mode).unwrap()
        ),
    )
    .expect("write fake libra");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700))
        .expect("make fake libra executable");
    fake
}

struct PiContractRun {
    lines: Vec<Value>,
    report: Value,
    output: Output,
    /// Child pids still present `grace` after the runner ended, recorded
    /// before the runner's process group is cleaned up.
    survivors: Vec<i64>,
}

/// Run the rendered extension for `command` and fire `count` copies of
/// `tool_call` at once. `tail` is JavaScript run after the handlers return,
/// with `t0` (the time before the first handler) in scope; it may finish the
/// report early (for example by calling `process.exit`).
fn pi_contract_run(
    repo: &HookRepo,
    node: &str,
    command: &Path,
    count: usize,
    tail: &str,
) -> PiContractRun {
    pi_contract_run_with(
        repo,
        node,
        command,
        count,
        "",
        tail,
        Duration::from_secs(2),
        &[],
    )
}

/// [`pi_contract_run`] with `head`, JavaScript run before the first handler
/// (with `report` in scope and `t0p`, the `performance.now()` just before the
/// first handler, readable from closures), extra environment for the runner
/// and its children, and a `grace` to wait for children to disappear after
/// the runner ends. Children are the fake libra pids from the log plus any
/// pids the runner put in `report.kids`.
#[allow(clippy::too_many_arguments)]
fn pi_contract_run_with(
    repo: &HookRepo,
    node: &str,
    command: &Path,
    count: usize,
    head: &str,
    tail: &str,
    grace: Duration,
    env: &[(&str, &str)],
) -> PiContractRun {
    pi_assert_ps_works();
    let asset = libra::internal::ai::hooks::providers::pi::settings::render_pi_asset(command)
        .expect("render the Pi extension");
    let work = repo
        .home
        .join(format!("pi-contract-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work).expect("contract work dir");
    std::fs::write(work.join("extension.mjs"), asset).expect("write extension module");
    let log = work.join("children.jsonl");
    let report = work.join("report.json");
    let runner = format!(
        r#"import {{ writeFileSync }} from 'node:fs';
import {{ execFileSync, spawn as realSpawn, spawnSync }} from 'node:child_process';
import {{ EventEmitter }} from 'node:events';
import factory, {{ __setClockForTest, __setSpawnForTest }} from './extension.mjs';
const handlers = new Map();
factory({{ on(name, handler) {{ handlers.set(name, handler); }} }});
const ctx = {{ cwd: {cwd}, isProjectTrusted: () => true, sessionManager: {{ getSessionId: () => {id}, getSessionFile: () => undefined }} }};
const report = {{ returned: [] }};
const save = () => writeFileSync({report}, JSON.stringify(report));
process.on('exit', () => {{ report.elapsedAtExit = Date.now() - t0; save(); }});
let t0p = 0;
{head}
const t0 = Date.now();
t0p = performance.now();
for (let i = 0; i < {count}; i++) {{
  report.returned.push(handlers.get('tool_call')({{ type: 'tool_call', toolCallId: 'call-' + i, toolName: 'bash', input: {{}} }}, ctx) === undefined);
}}
{tail}
"#,
        cwd = serde_json::to_string(&repo.repo).unwrap(),
        id = serde_json::to_string(PI_SESSION_ID).unwrap(),
        report = serde_json::to_string(&report).unwrap(),
    );
    std::fs::write(work.join("runner.mjs"), runner).expect("write runner");
    // Run Node in its own process group with a hard limit, so a regression
    // that leaves a child running fails here instead of hanging the suite.
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(node)
        .arg("--no-warnings")
        .arg(work.join("runner.mjs"))
        .current_dir(&work)
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", repo.home.join("decoy-bin").display()),
        )
        .env("HOME", &repo.home)
        .env("LIBRA_TEST_PI_LOG", &log)
        .envs(env.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("run the Pi extension");
    // Kill whatever is left of the runner's process group when this run ends,
    // also when a check below panics.
    let group = PiGroupGuard(child.id() as i32);
    let limit = Instant::now() + Duration::from_secs(20);
    while child.try_wait().expect("poll the Pi extension").is_none() {
        if Instant::now() >= limit {
            // SAFETY: signal the process group this test created.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().expect("collect the Pi extension");
    let lines: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("child log line"))
        .collect();
    let report = std::fs::read_to_string(&report)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    let mut pids: Vec<i64> = lines
        .iter()
        .filter(|line| line["phase"] == json!("start"))
        .filter_map(|line| line["pid"].as_i64())
        .collect();
    if let Some(kids) = report["kids"].as_array() {
        pids.extend(kids.iter().filter_map(|kid| {
            kid.as_i64()
                .or_else(|| kid.as_str().and_then(|text| text.trim().parse().ok()))
        }));
    }
    let until = Instant::now() + grace;
    let survivors = loop {
        let alive: Vec<i64> = pids
            .iter()
            .copied()
            .filter(|pid| pi_pid_exists(*pid))
            .collect();
        if alive.is_empty() || Instant::now() >= until {
            break alive;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // Clean up the runner's process group now that the survivors are known.
    drop(group);
    PiContractRun {
        lines,
        report,
        output,
        survivors,
    }
}

fn pi_contract_phase(run: &PiContractRun, phase: &str) -> Vec<Value> {
    run.lines
        .iter()
        .filter(|line| line["phase"] == json!(phase))
        .cloned()
        .collect()
}

/// Kills a test runner's process group when dropped, including on panic.
struct PiGroupGuard(i32);

impl Drop for PiGroupGuard {
    fn drop(&mut self) {
        // SAFETY: signal the process group this test created.
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

/// JavaScript for `head`: a virtual clock the test advances with
/// `advance(to)`, recording every timer the extension sets.
const PI_VIRTUAL_CLOCK: &str = r#"report.timers = [];
const virtual = { t: 0, timers: [], next: 1 };
__setClockForTest({
  now: () => virtual.t,
  setTimeout: (callback, ms) => {
    const id = virtual.next++;
    virtual.timers.push({ id, at: virtual.t + ms, callback });
    report.timers.push({ at: virtual.t, ms });
    return id;
  },
  clearTimeout: (id) => { virtual.timers = virtual.timers.filter((timer) => timer.id !== id); },
});
const advance = (to) => {
  virtual.t = to;
  for (;;) {
    const due = virtual.timers.filter((timer) => timer.at <= to).sort((a, b) => a.at - b.at)[0];
    if (!due) break;
    virtual.timers = virtual.timers.filter((timer) => timer !== due);
    due.callback();
  }
};"#;

/// JavaScript for `head`: fake children that never exit on their own.
/// `plan[i]` says whether the `i`th spawned child started (has a pid);
/// beyond the plan, children start.
const PI_FAKE_CHILDREN: &str = r#"report.spawnedAt = [];
const children = [];
const plan = globalThis.libraTestPlan ?? [];
const fakeSpawn = () => {
  const child = new EventEmitter();
  const started = plan[children.length] ?? true;
  child.pid = started ? 700000 + children.length : undefined;
  child.exitCode = null;
  child.signalCode = null;
  child.killCalls = 0;
  child.stdin = { on() {}, end() {} };
  child.kill = () => { child.killCalls += 1; child.killedAt = typeof virtual === 'undefined' ? null : virtual.t; return true; };
  report.spawnedAt.push(typeof virtual === 'undefined' ? null : virtual.t);
  children.push(child);
  return child;
};
__setSpawnForTest(fakeSpawn);"#;

/// JavaScript for `head`: record the argv of every spawn while still starting
/// real children.
const PI_RECORD_SPAWNS: &str = r#"report.spawns = [];
__setSpawnForTest((command, args, options) => {
  report.spawns.push(args);
  return realSpawn(command, args, options);
});"#;

/// Whether `pid` still names a process (a zombie counts). `ps` exits 0 with
/// the process's state when it exists and 1 with no output when it does
/// not; anything else is a failed query, not an absent process.
fn pi_pid_exists(pid: i64) -> bool {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    let listed = !String::from_utf8_lossy(&out.stdout).trim().is_empty();
    match out.status.code() {
        Some(0) if listed => true,
        Some(1) if !listed => false,
        _ => panic!("ps could not answer for pid {pid}: {}", describe(&out)),
    }
}

/// The process query used by these tests answers for a process that exists.
fn pi_assert_ps_works() {
    assert!(
        pi_pid_exists(i64::from(std::process::id())),
        "ps must list the test process itself"
    );
}

/// JavaScript for `head`: record when each kill is decided, relative to the
/// first handler, while still starting real children.
const PI_RECORD_KILLS: &str = r#"report.kills = [];
__setSpawnForTest((command, args, options) => {
  const child = realSpawn(command, args, options);
  const kill = child.kill.bind(child);
  child.kill = (signal) => { report.kills.push(Math.round(performance.now() - t0p)); return kill(signal); };
  return child;
});"#;

#[test]
fn pi_child_contract_runs_the_fixed_absolute_executable() {
    let repo = HookRepo::init();
    let node = pi_node();
    // A `libra` on PATH must never be the one that runs.
    let decoy = pi_contract_fake(&repo.home.join("decoy-bin"), &node, "libra", "exit:0");
    let fake = pi_contract_fake(&repo.home.join("fixed"), &node, "fixed-libra.mjs", "exit:0");
    assert!(
        libra::internal::ai::hooks::providers::pi::settings::render_pi_asset(Path::new("libra"))
            .is_err(),
        "a relative executable must be refused"
    );
    let run = pi_contract_run(&repo, &node, &fake, 1, "");
    assert!(run.output.status.success(), "{}", describe(&run.output));
    let starts = pi_contract_phase(&run, "start");
    assert_eq!(starts.len(), 1, "{:?}", run.lines);
    assert_eq!(starts[0]["self"], json!(fake.display().to_string()));
    assert_ne!(starts[0]["self"], json!(decoy.display().to_string()));
}

#[test]
fn pi_child_contract_passes_the_fixed_argv() {
    let repo = HookRepo::init();
    let node = pi_node();
    let fixed = json!([
        "agent",
        "hooks",
        "--capture-budget-ms",
        "1500",
        "pi",
        "observe"
    ]);
    // A child that runs receives exactly the fixed argv.
    let fake = pi_contract_fake(&repo.home.join("argv"), &node, "libra.mjs", "exit:0");
    let run = pi_contract_run(&repo, &node, &fake, 1, "");
    let starts = pi_contract_phase(&run, "start");
    assert_eq!(starts.len(), 1, "{:?}", run.lines);
    assert_eq!(starts[0]["argv"], fixed);
    // Every retry of a child that never started is spawned with the same argv.
    let missing = repo.home.join("no-such-libra");
    let run = pi_contract_run_with(
        &repo,
        &node,
        &missing,
        1,
        PI_RECORD_SPAWNS,
        "",
        Duration::from_secs(2),
        &[],
    );
    let spawns = run.report["spawns"].as_array().cloned().unwrap_or_default();
    assert_eq!(spawns.len(), 3, "{}", run.report);
    for args in spawns {
        assert_eq!(args, fixed);
    }
}

#[test]
fn pi_child_contract_starts_without_a_shell() {
    let repo = HookRepo::init();
    let node = pi_node();
    // With a shell, these names would run `touch`; without one they are
    // only a path.
    let dir = repo.home.join("bin $(touch SHELL_RAN) ; touch SHELL_RAN2");
    let fake = pi_contract_fake(&dir, &node, "libra `touch SHELL_RAN3`.mjs", "exit:0");
    let run = pi_contract_run(&repo, &node, &fake, 1, "");
    assert!(run.output.status.success(), "{}", describe(&run.output));
    assert_eq!(pi_contract_phase(&run, "start").len(), 1, "{:?}", run.lines);
    for name in ["SHELL_RAN", "SHELL_RAN2", "SHELL_RAN3"] {
        for root in [&repo.home, &repo.repo] {
            assert!(
                !walkdir_contains(root, name),
                "a shell ran: found {name} under {}",
                root.display()
            );
        }
    }
}

fn walkdir_contains(root: &Path, name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name() == std::ffi::OsStr::new(name) {
            return true;
        }
        if path.is_dir() && !path.is_symlink() && walkdir_contains(&path, name) {
            return true;
        }
    }
    false
}

#[test]
fn pi_child_contract_bounds_each_delivery_to_two_seconds() {
    let repo = HookRepo::init();
    let node = pi_node();
    let unused = repo.home.join("never-run");
    // On a virtual clock: attempts 1 and 2 never start and report so at 900
    // and 1800 ms; attempt 3 starts and is still running at the deadline. One
    // window covers all three, so the third gets only the 200 ms left. A
    // never-started child reported at the deadline gets no further attempt.
    let run = pi_contract_run_with(
        &repo,
        &node,
        &unused,
        0,
        &format!(
            "globalThis.libraTestPlan = [false, false, true, false];\n{PI_VIRTUAL_CLOCK}\n{PI_FAKE_CHILDREN}"
        ),
        r#"handlers.get('tool_call')({ type: 'tool_call', toolCallId: 'a', toolName: 'bash', input: {} }, ctx);
advance(900); children[0].emit('error', new Error('not started'));
advance(1800); children[1].emit('error', new Error('not started'));
advance(1999); report.killsBeforeDeadline = children[2].killCalls;
advance(2000); report.killsAtDeadline = children[2].killCalls; report.killedAt = children[2].killedAt;
children[2].exitCode = 0; children[2].emit('exit', 0);
report.spawnsAfterFirst = children.length;
advance(2000);
handlers.get('tool_call')({ type: 'tool_call', toolCallId: 'b', toolName: 'bash', input: {} }, ctx);
advance(4000); children[3].emit('error', new Error('not started'));
report.spawnsAfterSecond = children.length;
report.inFlight = globalThis[Symbol.for('libra.pi-hooks.delivery')].running.size;"#,
        Duration::from_secs(1),
        &[],
    );
    assert!(run.output.status.success(), "{}", describe(&run.output));
    assert_eq!(
        run.report["spawnedAt"],
        json!([0, 900, 1800, 2000]),
        "{}",
        run.report
    );
    // Each attempt's kill timer gets only what is left of the one window.
    assert_eq!(
        run.report["timers"],
        json!([
            { "at": 0, "ms": 2000 },
            { "at": 900, "ms": 1100 },
            { "at": 1800, "ms": 200 },
            { "at": 2000, "ms": 2000 }
        ]),
        "{}",
        run.report
    );
    assert_eq!(
        run.report["killsBeforeDeadline"],
        json!(0),
        "{}",
        run.report
    );
    assert_eq!(run.report["killsAtDeadline"], json!(1), "{}", run.report);
    assert_eq!(run.report["killedAt"], json!(2000), "{}", run.report);
    assert_eq!(run.report["spawnsAfterFirst"], json!(3), "{}", run.report);
    // The second delivery's child reported "not started" at its deadline:
    // no attempt follows.
    assert_eq!(run.report["spawnsAfterSecond"], json!(4), "{}", run.report);
    assert_eq!(run.report["inFlight"], json!(0), "{}", run.report);
}

#[test]
fn pi_child_contract_kills_a_child_at_the_deadline_or_on_exit() {
    let repo = HookRepo::init();
    let node = pi_node();
    let fake = pi_contract_fake(&repo.home.join("hang"), &node, "libra.mjs", "hang");
    // Timeout: the hung child is killed at the deadline, and the provider
    // process can finish.
    let run = pi_contract_run_with(
        &repo,
        &node,
        &fake,
        1,
        PI_RECORD_KILLS,
        "",
        Duration::from_secs(2),
        &[],
    );
    let starts = pi_contract_phase(&run, "start");
    assert_eq!(starts.len(), 1, "{:?}", run.lines);
    let kills = run.report["kills"].as_array().cloned().unwrap_or_default();
    assert_eq!(kills.len(), 1, "{}", run.report);
    let decided = kills[0].as_i64().unwrap();
    // Exact timing is pinned on the virtual clock; here a real child only
    // has to be killed at the deadline, give or take scheduling.
    assert!(
        (1_995..=2_600).contains(&decided),
        "the hung child must be killed at the deadline: {decided} ms"
    );
    assert!(run.survivors.is_empty(), "{:?}", run.survivors);
    // Cancellation: Pi exits while the child is running.
    let run = pi_contract_run(
        &repo,
        &node,
        &fake,
        1,
        "await new Promise((resolve) => setTimeout(resolve, 300)); process.exit(0);",
    );
    let starts = pi_contract_phase(&run, "start");
    assert_eq!(starts.len(), 1, "{:?}", run.lines);
    assert!(
        run.survivors.is_empty(),
        "the child outlived Pi's exit: {:?}",
        run.survivors
    );
}

#[test]
fn pi_child_contract_caps_children_in_flight_across_reloads() {
    let repo = HookRepo::init();
    let node = pi_node();
    let unused = repo.home.join("never-run");
    // Eight children of the first copy never exit while a reload loads a
    // second copy that fires eight more events: together they stay at eight.
    let run = pi_contract_run_with(
        &repo,
        &node,
        &unused,
        8,
        PI_FAKE_CHILDREN,
        r#"const second = await import('./extension.mjs?reload');
second.__setSpawnForTest(fakeSpawn);
const reloaded = new Map();
second.default({ on(name, handler) { reloaded.set(name, handler); } });
for (let i = 0; i < 8; i++) {
  report.returned.push(reloaded.get('tool_call')({ type: 'tool_call', toolCallId: 'again-' + i, toolName: 'bash', input: {} }, ctx) === undefined);
}
report.spawned = children.length;
report.inFlight = globalThis[Symbol.for('libra.pi-hooks.delivery')].running.size;
report.exitListeners = process.listenerCount('exit');
for (const child of children) { child.exitCode = 0; child.emit('exit', 0); }
report.inFlightAfterExit = globalThis[Symbol.for('libra.pi-hooks.delivery')].running.size;"#,
        Duration::from_secs(1),
        &[],
    );
    assert!(run.output.status.success(), "{}", describe(&run.output));
    assert_eq!(
        run.report["spawned"],
        json!(8),
        "a reload must not get its own eight children: {}",
        run.report
    );
    assert_eq!(run.report["inFlight"], json!(8), "{}", run.report);
    assert_eq!(run.report["inFlightAfterExit"], json!(0), "{}", run.report);
    // The runner's own exit listener plus one shared listener.
    assert_eq!(run.report["exitListeners"], json!(2), "{}", run.report);
}

#[tokio::test]
async fn pi_child_contract_bounds_a_child_after_pi_is_killed() {
    let repo = HookRepo::init();
    let node = pi_node();
    // Hold the repository's write lock, so the real libra child would wait
    // for it (the default busy wait is 30 seconds) if it had no budget.
    let mut options = ConnectOptions::new(format!(
        "sqlite://{}",
        repo.repo.join(".libra").join("libra.db").display()
    ));
    options
        .max_connections(1)
        .min_connections(1)
        .sqlx_logging(false);
    let lock = Database::connect(options)
        .await
        .expect("open repository database");
    lock.execute_unprepared("BEGIN IMMEDIATE")
        .await
        .expect("hold the write lock");
    let libra = std::fs::canonicalize(env!("CARGO_BIN_EXE_libra")).expect("libra binary");
    // Pi dies at once, without running any hook, after the child started.
    let started = Instant::now();
    let run = pi_contract_run_with(
        &repo,
        &node,
        &libra,
        0,
        "",
        r#"handlers.get('session_start')({ type: 'session_start', reason: 'startup' }, ctx);
report.kids = execFileSync('pgrep', ['-P', String(process.pid)]).toString().trim().split(/\s+/);
save();
process.kill(process.pid, 'SIGKILL');"#,
        Duration::from_secs(3),
        &[],
    );
    let lived = started.elapsed();
    let kids = run.report["kids"].as_array().cloned().unwrap_or_default();
    assert_eq!(kids.len(), 1, "{} {}", run.report, describe(&run.output));
    assert!(
        run.survivors.is_empty(),
        "the libra child outlived Pi by more than its budget: {:?}",
        run.survivors
    );
    assert!(
        lived < Duration::from_secs(4),
        "the child must end within its own budget: {lived:?}"
    );
    lock.execute_unprepared("ROLLBACK")
        .await
        .expect("release the write lock");
}

#[test]
fn pi_child_contract_reaps_killed_children() {
    let repo = HookRepo::init();
    let node = pi_node();
    let fake = pi_contract_fake(&repo.home.join("reap"), &node, "libra.mjs", "hang");
    // Stay alive past the deadline and look at the killed child from the
    // provider's own process: a zombie would still be listed.
    let run = pi_contract_run(
        &repo,
        &node,
        &fake,
        1,
        r#"await new Promise((resolve) => setTimeout(resolve, 2600));
const { readFileSync } = await import('node:fs');
const pids = readFileSync(process.env.LIBRA_TEST_PI_LOG, 'utf8').trim().split('\n').map((line) => JSON.parse(line).pid);
const query = (pid) => {
  const out = spawnSync('ps', ['-o', 'stat=', '-p', String(pid)]);
  const listed = out.stdout.toString().trim();
  if (out.status === 0 && listed !== '') return listed;
  if (out.status === 1 && listed === '') return 'absent';
  return 'query-failed';
};
report.self = query(process.pid);
report.listed = pids.map(query);"#,
    );
    assert!(run.output.status.success(), "{}", describe(&run.output));
    // The query works for a process that exists, and the killed child is
    // gone, not a zombie.
    assert_ne!(run.report["self"], json!("absent"), "{}", run.report);
    assert_ne!(run.report["self"], json!("query-failed"), "{}", run.report);
    assert_eq!(run.report["listed"], json!(["absent"]), "{}", run.report);
}

#[test]
fn pi_child_contract_ends_a_stalled_child_after_pi_is_killed() {
    let repo = HookRepo::init();
    let node = pi_node();
    let libra = std::fs::canonicalize(env!("CARGO_BIN_EXE_libra")).expect("libra binary");
    // The child blocks as an operation that ignores its capture deadline
    // would, and Pi dies without running any hook: only the child's own hard
    // lifetime (budget plus grace) can end it.
    let started = Instant::now();
    let run = pi_contract_run_with(
        &repo,
        &node,
        &libra,
        0,
        "",
        r#"handlers.get('session_start')({ type: 'session_start', reason: 'startup' }, ctx);
report.kids = execFileSync('pgrep', ['-P', String(process.pid)]).toString().trim().split(/\s+/);
save();
process.kill(process.pid, 'SIGKILL');"#,
        Duration::from_secs(3),
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_PI_HOOK_STALL_MS", "10000"),
        ],
    );
    let lived = started.elapsed();
    let kids = run.report["kids"].as_array().cloned().unwrap_or_default();
    assert_eq!(kids.len(), 1, "{} {}", run.report, describe(&run.output));
    assert!(
        run.survivors.is_empty(),
        "a stalled child outlived Pi by more than its hard lifetime: {:?}",
        run.survivors
    );
    assert!(
        lived < Duration::from_secs(4),
        "the stalled child must end at its hard lifetime: {lived:?}"
    );
}

#[test]
fn pi_child_contract_retries_a_failed_kill_once() {
    let repo = HookRepo::init();
    let node = pi_node();
    let unused = repo.home.join("never-started");
    for mode in ["throw", "error"] {
        // A fake child that never exits and whose kill fails, either by
        // throwing or by reporting an error event.
        let head = format!(
            r#"const fakes = [];
__setSpawnForTest(() => {{
  const child = new EventEmitter();
  child.pid = 424242 + fakes.length;
  child.exitCode = null;
  child.signalCode = null;
  child.killCalls = 0;
  child.stdin = {{ on() {{}}, end() {{}} }};
  child.kill = () => {{
    child.killCalls += 1;
    if ({mode} === 'throw') throw new Error('kill failed');
    setImmediate(() => child.emit('error', new Error('kill failed')));
    return false;
  }};
  fakes.push(child);
  return child;
}});
report.fakes = fakes;"#,
            mode = serde_json::to_string(mode).unwrap()
        );
        let run = pi_contract_run_with(
            &repo,
            &node,
            &unused,
            1,
            &head,
            r#"await new Promise((resolve) => setTimeout(resolve, 2600));
const shared = globalThis[Symbol.for('libra.pi-hooks.delivery')];
report.killsAtDeadline = fakes.map((child) => child.killCalls);
report.heldAtDeadline = shared.running.size;
await new Promise((resolve) => setTimeout(resolve, 500));
report.killsLater = fakes.map((child) => child.killCalls);
for (const child of fakes) { child.exitCode = 0; child.emit('exit', 0); }
report.heldAfterExit = shared.running.size;
delete report.fakes;"#,
            Duration::from_secs(1),
            &[],
        );
        assert!(
            run.output.status.success(),
            "{mode}: {}",
            describe(&run.output)
        );
        // One kill at the deadline and exactly one retry; no further retries.
        assert_eq!(
            run.report["killsAtDeadline"],
            json!([2]),
            "{mode}: {}",
            run.report
        );
        assert_eq!(
            run.report["killsLater"],
            json!([2]),
            "{mode}: {}",
            run.report
        );
        // The slot stays taken until the child's exit event, then frees.
        assert_eq!(
            run.report["heldAtDeadline"],
            json!(1),
            "{mode}: {}",
            run.report
        );
        assert_eq!(
            run.report["heldAfterExit"],
            json!(0),
            "{mode}: {}",
            run.report
        );
    }
}

#[test]
fn pi_child_contract_caps_children_in_flight_at_eight() {
    let repo = HookRepo::init();
    let node = pi_node();
    let fake = pi_contract_fake(&repo.home.join("cap"), &node, "libra.mjs", "sleep:400:0");
    let run = pi_contract_run(&repo, &node, &fake, 20, "");
    assert!(run.output.status.success(), "{}", describe(&run.output));
    let mut points: Vec<(i64, i32)> = run
        .lines
        .iter()
        .map(|line| {
            let delta = if line["phase"] == json!("start") {
                1
            } else {
                -1
            };
            (line["t"].as_i64().unwrap(), delta)
        })
        .collect();
    // Count an end before a start at the same millisecond.
    points.sort();
    let mut running = 0;
    let mut peak = 0;
    for (_, delta) in points {
        running += delta;
        peak = peak.max(running);
    }
    assert!(peak <= 8, "{peak} children ran at once");
    assert_eq!(
        pi_contract_phase(&run, "start").len(),
        8,
        "deliveries over the cap are dropped, not queued"
    );
}

#[test]
fn pi_child_contract_stops_after_three_attempts() {
    let repo = HookRepo::init();
    let node = pi_node();
    let unused = repo.home.join("never-run");
    // Children that never start are retried up to three attempts in all.
    let run = pi_contract_run_with(
        &repo,
        &node,
        &unused,
        1,
        &format!("globalThis.libraTestPlan = [false, false, false, false];\n{PI_FAKE_CHILDREN}"),
        r#"for (let i = 0; i < 4; i++) { if (children[i]) children[i].emit('error', new Error('not started')); }
report.spawned = children.length;"#,
        Duration::from_secs(1),
        &[],
    );
    assert_eq!(run.report["spawned"], json!(3), "{}", run.report);
    // The same with a real missing executable.
    let missing = repo.home.join("no-such-libra");
    let run = pi_contract_run_with(
        &repo,
        &node,
        &missing,
        1,
        PI_RECORD_SPAWNS,
        "",
        Duration::from_secs(2),
        &[],
    );
    assert_eq!(
        run.report["spawns"].as_array().map(Vec::len),
        Some(3),
        "{}",
        run.report
    );
}

#[test]
fn pi_child_contract_never_reruns_a_child_that_ran() {
    let repo = HookRepo::init();
    let node = pi_node();
    // A child that ran and then failed may already have recorded the event,
    // so it is never run again.
    // The fake logs its start (standing in for a recorded event) and then
    // exits 1.
    let failing = pi_contract_fake(&repo.home.join("ran"), &node, "libra.mjs", "exit:1");
    let run = pi_contract_run(&repo, &node, &failing, 1, "");
    assert_eq!(pi_contract_phase(&run, "start").len(), 1, "{:?}", run.lines);
    assert_eq!(pi_contract_phase(&run, "end").len(), 1, "{:?}", run.lines);
    // The same for a child killed at the deadline.
    let hung = pi_contract_fake(&repo.home.join("ran-hung"), &node, "libra.mjs", "hang");
    let run = pi_contract_run(&repo, &node, &hung, 1, "");
    assert_eq!(pi_contract_phase(&run, "start").len(), 1, "{:?}", run.lines);
}

// ---------------------------------------------------------------------------
// PI-12: Pi live event correlation (`pi_event_correlation_*`).
// ---------------------------------------------------------------------------

fn pi_libra_session() -> String {
    format!("pi__{PI_SESSION_ID}")
}

/// A receiver frame for `event` with `fields` replacing the fixture values.
fn pi_frame_with(repo: &HookRepo, event: &str, fields: Value) -> Value {
    let mut frame = pi_frame(repo, event);
    for (key, value) in fields.as_object().expect("override object") {
        frame[key] = value.clone();
    }
    frame
}

/// Deliver one frame through the real `agent hooks pi observe` entry, a
/// new receiver process each time, as after a restart.
fn pi_observe(repo: &HookRepo, frame: &Value) {
    let out = repo.hook("pi", "observe", &frame.to_string());
    assert!(out.status.success(), "{frame}: {}", describe(&out));
}

/// The in-process ingress outcome for one frame: the lifecycle kind it is
/// recorded as, or `None` when the receiver skips it.
fn pi_recorded_kind(repo: &HookRepo, frame: &Value) -> Option<LifecycleEventKind> {
    use libra::internal::ai::{
        capture::ingress::CaptureIngressOutcome, hooks::providers::pi_provider,
    };
    match lower_in_process_capture_frame_for_test(
        frame.to_string().as_bytes(),
        ProviderHookCommand::Observe,
        ProviderHookCommand::Observe.lifecycle_event_kind(),
        pi_provider(),
        Some(&repo.repo),
    )
    .expect("a Pi frame never fails ingress")
    {
        CaptureIngressOutcome::Command(command) => Some(command.event_kind()),
        CaptureIngressOutcome::UnknownEvent { .. } => None,
    }
}

/// Deliver `frame` and assert how far the durable session moved: one
/// revision for a new action, none for a replay or a skipped frame.
async fn pi_observe_advances(repo: &HookRepo, frame: &Value, by: i64) -> DurableSessionState {
    let session = pi_libra_session();
    let before = repo.durable_session(&session).await;
    pi_observe(repo, frame);
    let after = repo.durable_session(&session).await;
    if by == 0 {
        assert_eq!(after, before, "{frame} must leave the session as it was");
    } else {
        assert_eq!(
            after.sync_revision,
            before.sync_revision + by,
            "{frame} must be recorded once"
        );
        assert_eq!(after.state, "active", "{frame}");
    }
    after
}

#[tokio::test]
async fn pi_event_correlation_replays_a_session_start_once() {
    let repo = HookRepo::init();
    let start = pi_frame(&repo, "session_start");
    pi_observe(&repo, &start);
    assert_eq!(repo.sessions().len(), 1);
    // The same start again, from a new receiver process, is the same action.
    pi_observe_advances(&repo, &start, 0).await;
    // Pi restarted and resumed this session, then reloaded it: each is a new
    // start on the same session, and each is recorded once.
    for (n, reason, previous, at) in [
        (
            1,
            "resume",
            json!("/tmp/prev.jsonl"),
            "2026-11-02T17:05:00.000Z",
        ),
        (2, "reload", Value::Null, "2026-11-02T17:06:00.000Z"),
    ] {
        let restart = pi_frame_with(
            &repo,
            "session_start",
            json!({"frameId": pi_frame_id(n), "reason": reason, "previousSessionFile": previous, "emittedAt": at, "sessionFile": "/tmp/session.jsonl"}),
        );
        pi_observe_advances(&repo, &restart, 1).await;
        pi_observe_advances(&repo, &restart, 0).await;
    }
    // Two Pi processes start the session in the same millisecond, or a later
    // process starts it after the clock stepped back: the frames differ
    // only in frameId, and both starts are recorded.
    for n in [3, 4] {
        let start = pi_frame_with(
            &repo,
            "session_start",
            json!({"frameId": pi_frame_id(n), "reason": "resume", "emittedAt": "2026-11-02T17:07:00.000Z"}),
        );
        pi_observe_advances(&repo, &start, 1).await;
        pi_observe_advances(&repo, &start, 0).await;
    }
    // Events after the restart land on that one session.
    let call = pi_frame_with(
        &repo,
        "tool_call",
        json!({"toolCallId": "call-after-resume"}),
    );
    pi_observe_advances(&repo, &call, 1).await;
    let sessions = repo.sessions();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(
        sessions[0]["session_id"],
        json!(pi_libra_session()),
        "{sessions:?}"
    );
    assert!(repo.checkpoints().is_empty());
}

#[tokio::test]
async fn pi_event_correlation_replays_a_tool_record_once() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    let call = pi_frame_with(&repo, "tool_call", json!({"toolCallId": "call-7"}));
    let recorded = pi_observe_advances(&repo, &call, 1).await;
    pi_observe_advances(&repo, &call, 0).await;
    // Pi sends the same call again after a restart: a new frame, a later
    // emittedAt and a known session file do not make it a new record.
    let again = pi_frame_with(
        &repo,
        "tool_call",
        json!({"frameId": pi_frame_id(7), "toolCallId": "call-7", "emittedAt": "2026-11-02T18:00:00.000Z", "sessionFile": "/tmp/session.jsonl"}),
    );
    assert_eq!(pi_observe_advances(&repo, &again, 0).await, recorded);
    // Another tool call is another record.
    let other = pi_frame_with(&repo, "tool_call", json!({"toolCallId": "call-8"}));
    pi_observe_advances(&repo, &other, 1).await;
}

#[tokio::test]
async fn pi_event_correlation_keeps_call_and_result_phases_apart() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    let call = pi_frame_with(&repo, "tool_call", json!({"toolCallId": "call-9"}));
    let result = pi_frame_with(&repo, "tool_result", json!({"toolCallId": "call-9"}));
    // One record, two phases: the result is not a replay of the call.
    pi_observe_advances(&repo, &call, 1).await;
    let both = pi_observe_advances(&repo, &result, 1).await;
    // Each phase replays to itself.
    pi_observe_advances(&repo, &call, 0).await;
    assert_eq!(pi_observe_advances(&repo, &result, 0).await, both);
    use libra::internal::ai::hooks::providers::pi_provider;
    for event in ["tool_call", "tool_result"] {
        assert_eq!(
            pi_provider().dedup_identity_keys_for_event(event, LifecycleEventKind::ToolUse),
            &["toolCallId"],
            "{event}"
        );
    }
}

#[tokio::test]
async fn pi_event_correlation_records_a_result_before_its_call() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    // The result arrives first. It is recorded at once; nothing waits for
    // the call, and the call is recorded when it comes.
    let result = pi_frame_with(&repo, "tool_result", json!({"toolCallId": "call-3"}));
    let call = pi_frame_with(&repo, "tool_call", json!({"toolCallId": "call-3"}));
    pi_observe_advances(&repo, &result, 1).await;
    let paired = pi_observe_advances(&repo, &call, 1).await;
    pi_observe_advances(&repo, &result, 0).await;
    assert_eq!(pi_observe_advances(&repo, &call, 0).await, paired);
    // A result whose call never comes leaves nothing open: later events
    // are recorded as usual.
    let lone = pi_frame_with(&repo, "tool_result", json!({"toolCallId": "call-4"}));
    pi_observe_advances(&repo, &lone, 1).await;
    let next = pi_frame_with(&repo, "tool_call", json!({"toolCallId": "call-5"}));
    pi_observe_advances(&repo, &next, 1).await;
    assert_eq!(repo.sessions().len(), 1);
    assert!(repo.checkpoints().is_empty());

    // The extension keeps no pairing state either: it forwards a result
    // that precedes its call as it comes, and both reach the receiver.
    let node = pi_node();
    let fake = pi_fake_libra(&repo, &node, 0);
    let events = pi_event_payloads(false);
    let tool = |name: &str| {
        events
            .iter()
            .find(|(event, _)| *event == name)
            .map(|(event, payload)| (*event, payload.clone()))
            .expect("tool payload")
    };
    let run = pi_run_extension(
        &repo,
        &node,
        &fake,
        &pi_ctx(&repo, "true", "undefined"),
        &[tool("tool_result"), tool("tool_call")],
    );
    assert!(run.output.status.success(), "{}", describe(&run.output));
    let mut forwarded: Vec<String> = run
        .forwarded
        .iter()
        .map(|line| {
            line["frame"]["event"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    forwarded.sort();
    assert_eq!(
        forwarded,
        ["tool_call", "tool_result"],
        "{:?}",
        run.forwarded
    );
}

#[tokio::test]
async fn pi_event_correlation_treats_a_repeated_result_as_the_first() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    pi_observe(
        &repo,
        &pi_frame_with(&repo, "tool_call", json!({"toolCallId": "call-11"})),
    );
    let result = pi_frame_with(&repo, "tool_result", json!({"toolCallId": "call-11"}));
    let first = pi_observe_advances(&repo, &result, 1).await;
    // The same bytes again, and the same result sent later with a known
    // session file, end in the state the first result left.
    let later = pi_frame_with(
        &repo,
        "tool_result",
        json!({"frameId": pi_frame_id(11), "toolCallId": "call-11", "emittedAt": "2026-11-02T18:30:00.000Z", "sessionFile": "/tmp/session.jsonl"}),
    );
    for repeated in [&result, &later, &result] {
        assert_eq!(pi_observe_advances(&repo, repeated, 0).await, first);
    }
}

#[tokio::test]
async fn pi_event_correlation_accepts_nested_events_only_under_their_parent() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    let nested = |event: &str, id: &str, parent: &str| {
        pi_frame_with(
            &repo,
            event,
            json!({"toolCallId": id, "parentToolCallId": parent, "toolName": "read"}),
        )
    };
    // The parent call and a call it made are separate records; the nested
    // call pairs with its own result, and a call the nested tool made in
    // turn belongs to the nested call.
    let parent = pi_frame_with(&repo, "tool_call", json!({"toolCallId": "call-1"}));
    pi_observe_advances(&repo, &parent, 1).await;
    let mut recorded = Vec::new();
    for frame in [
        nested("tool_call", "call-1/1", "call-1"),
        nested("tool_result", "call-1/1", "call-1"),
        nested("tool_call", "call-1/1/1", "call-1/1"),
        nested("tool_call", "call-1/2", "call-1"),
    ] {
        assert_eq!(
            pi_recorded_kind(&repo, &frame),
            Some(LifecycleEventKind::ToolUse),
            "{frame}"
        );
        pi_observe_advances(&repo, &frame, 1).await;
        recorded.push(frame);
    }
    for frame in &recorded {
        pi_observe_advances(&repo, frame, 0).await;
    }
    // A frame whose ID is not a direct child of the parent it names is
    // refused without a write.
    for frame in [
        nested("tool_call", "call-1/1", "call-2"),
        nested("tool_result", "call-1/1/2", "call-1"),
        nested("tool_call", "call-1/", "call-1"),
        nested("tool_call", "call-1", "call-1"),
        nested("tool_call", "call-10/1", "call-1"),
        nested("tool_call", "other", "call-1"),
    ] {
        assert_eq!(pi_recorded_kind(&repo, &frame), None, "{frame}");
        pi_observe_advances(&repo, &frame, 0).await;
    }
    assert_eq!(repo.sessions().len(), 1);
}

#[tokio::test]
async fn pi_event_correlation_expects_no_transcript_entry_for_nested_events() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    // Nested events with no parent event seen, result first: each is
    // recorded as it arrives, without a diagnostic.
    for (event, id, parent) in [
        ("tool_result", "call-5/1", "call-5"),
        ("tool_call", "call-5/1", "call-5"),
        ("tool_result", "call-5/1/1", "call-5/1"),
    ] {
        let frame = pi_frame_with(
            &repo,
            event,
            json!({"toolCallId": id, "parentToolCallId": parent}),
        );
        let out = repo.hook("pi", "observe", &frame.to_string());
        assert!(out.status.success(), "{frame}: {}", describe(&out));
        assert!(
            out.stdout.is_empty() && out.stderr.is_empty(),
            "a nested event must not print a diagnostic: {}",
            describe(&out)
        );
        pi_observe_advances(&repo, &frame, 0).await;
    }
    let session = repo.durable_session(&pi_libra_session()).await;
    assert_eq!(session.sync_revision, 4, "start plus three nested events");
    // Doctor finds no orphan or missing entry.
    let doctor = repo.run(&["--json", "agent", "doctor"], None);
    assert!(doctor.status.success(), "{}", describe(&doctor));
    let report: Value = serde_json::from_slice(&doctor.stdout).expect("doctor json");
    assert_eq!(report["data"]["orphan_checkpoints"], json!(0), "{report}");
    assert_eq!(
        report["data"]["checkpoint_store"]["findings"],
        json!([]),
        "{report}"
    );
    assert!(repo.checkpoints().is_empty());
}

#[tokio::test]
async fn pi_event_correlation_counts_only_user_messages_as_prompts() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    // A user message starts a turn, once.
    let user = pi_frame(&repo, "before_agent_start");
    assert_eq!(
        pi_recorded_kind(&repo, &user),
        Some(LifecycleEventKind::TurnStart)
    );
    pi_observe_advances(&repo, &user, 1).await;
    pi_observe_advances(&repo, &user, 0).await;
    // A run a custom message started is not a prompt and is not recorded.
    for (n, at) in [
        (20, "2026-11-02T17:10:00.000Z"),
        (21, "2026-11-02T17:11:00.000Z"),
    ] {
        let custom = pi_frame_with(
            &repo,
            "before_agent_start",
            json!({"frameId": pi_frame_id(n), "messageType": "custom", "emittedAt": at}),
        );
        assert_eq!(pi_recorded_kind(&repo, &custom), None, "{custom}");
        pi_observe_advances(&repo, &custom, 0).await;
    }
    // The next user message is another turn, also when it carries the same
    // emittedAt as the first, as from another Pi process.
    for (n, at) in [
        (22, "2026-11-02T17:12:00.000Z"),
        (23, "2026-11-02T17:00:00.000Z"),
    ] {
        let next = pi_frame_with(
            &repo,
            "before_agent_start",
            json!({"frameId": pi_frame_id(n), "emittedAt": at}),
        );
        pi_observe_advances(&repo, &next, 1).await;
        pi_observe_advances(&repo, &next, 0).await;
    }
    assert!(
        repo.checkpoints().is_empty(),
        "a turn start writes no checkpoint"
    );
}

/// Deliver `count` new tool calls in process, each its own action.
async fn pi_ingest_tool_calls(
    repo: &HookRepo,
    conn: &DatabaseConnection,
    prefix: &str,
    count: usize,
) {
    use libra::internal::ai::hooks::providers::pi_provider;
    for n in 0..count {
        let frame = pi_frame_with(
            repo,
            "tool_call",
            json!({"toolCallId": format!("{prefix}-{n}")}),
        );
        ingest_agent_traces_payload(
            frame.to_string().as_bytes(),
            ProviderHookCommand::Observe,
            ProviderHookCommand::Observe.lifecycle_event_kind(),
            pi_provider(),
            conn,
            Some(&repo.repo),
        )
        .await
        .expect("a later tool call is recorded");
    }
}

#[tokio::test]
async fn pi_event_correlation_bounds_the_replay_window() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    let first = pi_frame_with(
        &repo,
        "tool_call",
        json!({"toolCallId": "call-window-first"}),
    );
    pi_observe_advances(&repo, &first, 1).await;
    // The shared receipt ledger keeps at most 128 receipts per session and,
    // when full, evicts the completed receipt with the smallest
    // (observed second, key). Let every later action be observed in a later
    // second than the first frame, so its receipt goes before theirs.
    std::thread::sleep(Duration::from_millis(1100));
    let conn = repo.db().await;
    // 102 actions in all: no receipt is evicted, so the replay is the same
    // action.
    pi_ingest_tool_calls(&repo, &conn, "call-window-a", 100).await;
    pi_observe_advances(&repo, &first, 0).await;
    // 202 actions: the receipts of the first second are gone, so the replay
    // is recorded once more as a new action on the same session.
    pi_ingest_tool_calls(&repo, &conn, "call-window-b", 100).await;
    pi_observe_advances(&repo, &first, 1).await;
    pi_observe_advances(&repo, &first, 0).await;
    assert_eq!(repo.sessions().len(), 1);
    assert!(repo.checkpoints().is_empty());
}

/// JavaScript for `head`: freeze the wall clock (`Date.now()` and
/// `new Date()`), and record the frame each
/// spawned fake child receives. `plan[i]` says whether the `i`th child
/// started; beyond the plan, children start. `fake` is also installed in
/// any reloaded copy the tail imports.
const PI_FROZEN_FRAMES: &str = r#"const frozenAt = Date.UTC(2026, 10, 3, 1, 0, 0, 0);
const RealDate = Date;
globalThis.Date = class extends RealDate {
  constructor(...args) { super(...(args.length ? args : [frozenAt])); }
  static now() { return frozenAt; }
};
report.frames = [];
report.raw = [];
const kids = [];
const plan = globalThis.libraTestPlan ?? [];
const fake = () => {
  const child = new EventEmitter();
  child.pid = (plan[kids.length] ?? true) ? 710000 + kids.length : undefined;
  child.exitCode = null;
  child.signalCode = null;
  child.kill = () => true;
  child.stdin = { on() {}, end(data) { report.raw.push(data); report.frames.push(JSON.parse(data)); } };
  kids.push(child);
  return child;
};
__setSpawnForTest(fake);
const done = () => { for (const kid of kids) { if (kid.pid !== undefined) kid.emit('exit', 0, null); } save(); };"#;

#[tokio::test]
async fn pi_event_correlation_gives_every_frame_its_own_frame_id() {
    let repo = HookRepo::init();
    let node = pi_node();
    let unused = repo.home.join("never-run");
    // Two Pi processes with the clock frozen at the same millisecond, the
    // first also reloading its extension: each fires a session start and a
    // user run start.
    let tail = r#"handlers.get('session_start')({ type: 'session_start', reason: 'startup' }, ctx);
const reloaded = await import('./extension.mjs?reload=1');
reloaded.__setSpawnForTest(fake);
const again = new Map();
reloaded.default({ on(name, handler) { again.set(name, handler); } });
again.get('before_agent_start')({ type: 'before_agent_start', messageType: 'user', prompt: 'one' }, ctx);
done();"#;
    let mut frames: Vec<Value> = Vec::new();
    for _ in 0..2 {
        let run = pi_contract_run_with(
            &repo,
            &node,
            &unused,
            0,
            PI_FROZEN_FRAMES,
            tail,
            Duration::from_secs(1),
            &[],
        );
        let produced = run.report["frames"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            produced.len(),
            2,
            "{} {}",
            run.report,
            describe(&run.output)
        );
        frames.extend(produced);
    }
    let stamps: std::collections::BTreeSet<String> = frames
        .iter()
        .map(|frame| frame["emittedAt"].to_string())
        .collect();
    assert_eq!(stamps.len(), 1, "the clock is frozen: {frames:?}");
    let ids: std::collections::BTreeSet<String> = frames
        .iter()
        .map(|frame| frame["frameId"].to_string())
        .collect();
    assert_eq!(ids.len(), 4, "every frame gets its own frameId: {frames:?}");
    // The receiver records all four starts, once each.
    pi_observe(&repo, &frames[0]);
    for frame in &frames[1..] {
        pi_observe_advances(&repo, frame, 1).await;
    }
    for frame in &frames {
        pi_observe_advances(&repo, frame, 0).await;
    }
    assert_eq!(repo.sessions().len(), 1);

    // A frame delivered again, after a child that never started, is the
    // same frame, with the same frameId.
    let run = pi_contract_run_with(
        &repo,
        &node,
        &unused,
        0,
        &format!("globalThis.libraTestPlan = [false, true];\n{PI_FROZEN_FRAMES}"),
        r#"handlers.get('session_start')({ type: 'session_start', reason: 'startup' }, ctx);
kids[0].emit('error', new Error('not started'));
done();"#,
        Duration::from_secs(1),
        &[],
    );
    let raw: Vec<Value> = run.report["raw"].as_array().cloned().unwrap_or_default();
    assert_eq!(raw.len(), 2, "{} {}", run.report, describe(&run.output));
    assert_eq!(raw[0], raw[1], "a redelivered frame is the same bytes");
}

// ---------------------------------------------------------------------------
// PI-19: Pi run terminal mapping (`pi_run_terminal_*`).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pi_run_terminal_waits_for_agent_settled() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    pi_observe(&repo, &pi_frame(&repo, "before_agent_start"));
    // A turn ending is not the run terminal: nothing is committed yet.
    pi_observe(&repo, &pi_frame(&repo, "turn_end"));
    assert!(
        repo.checkpoints().is_empty(),
        "turn_end must not commit a checkpoint"
    );
    // agent_end is not an observed event and writes nothing.
    let mut agent_end = pi_frame(&repo, "session_start");
    agent_end["event"] = json!("agent_end");
    pi_observe(&repo, &agent_end);
    assert!(repo.checkpoints().is_empty(), "agent_end is not a terminal");
    // The settle commits exactly one checkpoint.
    let settled = pi_frame(&repo, "agent_settled");
    pi_observe(&repo, &settled);
    assert_eq!(repo.checkpoints().len(), 1);
    // Replaying the same frame is the same action.
    pi_observe(&repo, &settled);
    assert_eq!(repo.checkpoints().len(), 1);
}

#[tokio::test]
async fn pi_run_terminal_reports_an_error_run() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    pi_observe(&repo, &pi_frame(&repo, "before_agent_start"));
    let turn = pi_frame_with(&repo, "turn_end", json!({"outcome": "error"}));
    pi_observe(&repo, &turn);
    let settled = pi_frame_with(
        &repo,
        "agent_settled",
        json!({"aborted": false, "outcome": "error"}),
    );
    let out = repo.hook("pi", "observe", &settled.to_string());
    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("pi run ended with an error; its checkpoint was kept"),
        "an error run must not look like a clean completion: {}",
        describe(&out)
    );
    assert_eq!(
        repo.checkpoints().len(),
        1,
        "an error run keeps the data it captured"
    );
    // The recorded kind is still the checkpoint-committing turn end.
    assert_eq!(
        pi_recorded_kind(&repo, &settled),
        Some(LifecycleEventKind::TurnEnd)
    );
    let envelope = libra::internal::ai::hooks::providers::pi_provider()
        .lower_native_frame(&settled.to_string())
        .expect("pi lowers its frame")
        .expect("valid frame lowers");
    let parsed = libra::internal::ai::hooks::providers::pi_provider()
        .parse_hook_event("agent_settled", &envelope)
        .expect("the terminal parses");
    assert_eq!(parsed.kind, LifecycleEventKind::TurnEnd);
    let source = parsed.source.expect("terminal source");
    assert_eq!(source["outcome"], json!("error"));
    assert_eq!(source["partial"], json!(true));
}

#[tokio::test]
async fn pi_run_terminal_interrupts_without_a_checkpoint() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    let interrupted = pi_frame_with(
        &repo,
        "agent_settled",
        json!({"aborted": true, "outcome": "error"}),
    );
    pi_observe(&repo, &interrupted);
    assert!(
        repo.checkpoints().is_empty(),
        "an aborted run does not commit a checkpoint"
    );
    assert_eq!(
        pi_recorded_kind(&repo, &interrupted),
        Some(LifecycleEventKind::TurnInterrupted)
    );
    let session = pi_libra_session();
    assert_eq!(repo.durable_session(&session).await.state, "active");
}

#[tokio::test]
async fn pi_run_terminal_reports_an_unknown_outcome() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    pi_observe(&repo, &pi_frame(&repo, "before_agent_start"));
    // The run produced no turn_end at all.
    let settled = pi_frame_with(
        &repo,
        "agent_settled",
        json!({"aborted": false, "outcome": null}),
    );
    let out = repo.hook("pi", "observe", &settled.to_string());
    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("pi run ended without a completed turn; its checkpoint is partial"),
        "an unknown outcome must not look like a clean completion: {}",
        describe(&out)
    );
    assert_eq!(repo.checkpoints().len(), 1);
    let envelope = libra::internal::ai::hooks::providers::pi_provider()
        .lower_native_frame(&settled.to_string())
        .expect("pi lowers its frame")
        .expect("valid frame lowers");
    let parsed = libra::internal::ai::hooks::providers::pi_provider()
        .parse_hook_event("agent_settled", &envelope)
        .expect("the terminal parses");
    let source = parsed.source.expect("terminal source");
    assert_eq!(source["outcome"], json!("unknown"));
    assert_eq!(source["partial"], json!(true));
}

#[tokio::test]
async fn pi_run_terminal_keeps_previously_captured_data() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    pi_observe(&repo, &pi_frame(&repo, "before_agent_start"));
    pi_observe(&repo, &pi_frame(&repo, "agent_settled"));
    let first = repo.checkpoints();
    assert_eq!(first.len(), 1);
    // The next run is aborted: it must not delete or rewrite the earlier
    // checkpoint.
    pi_observe(
        &repo,
        &pi_frame_with(
            &repo,
            "agent_settled",
            json!({"frameId": pi_frame_id(20), "aborted": true, "outcome": "error"}),
        ),
    );
    assert_eq!(
        repo.checkpoints(),
        first,
        "an aborted run keeps earlier capture data unchanged"
    );
}

#[test]
fn pi_run_terminal_tracks_each_run_outcome() {
    let repo = HookRepo::init();
    let node = pi_node();
    let fake = pi_fake_libra(&repo, &node, 0);
    let events = vec![
        ("session_start", json!({"reason": "startup"})),
        (
            "before_agent_start",
            json!({"messageType": "user", "prompt": "one"}),
        ),
        (
            "turn_end",
            json!({"outcome": "completed", "turnIndex": 0, "messageEntryId": "entry-1", "toolResultEntryIds": []}),
        ),
        ("agent_settled", json!({"aborted": false})),
        (
            "before_agent_start",
            json!({"messageType": "custom", "message": "two"}),
        ),
        (
            "turn_end",
            json!({"outcome": "error", "turnIndex": 0, "messageEntryId": "entry-2", "toolResultEntryIds": []}),
        ),
        ("agent_settled", json!({"aborted": false})),
        // A settle with no observed run in this process reports unknown.
        ("agent_settled", json!({"aborted": false})),
    ];
    let run = pi_run_extension(
        &repo,
        &node,
        &fake,
        &pi_ctx(&repo, "true", "undefined"),
        &events,
    );
    assert!(run.output.status.success(), "{}", describe(&run.output));
    let outcomes: Vec<Value> = run
        .forwarded
        .iter()
        .filter(|forwarded| forwarded["frame"]["event"] == json!("agent_settled"))
        .map(|forwarded| forwarded["frame"]["outcome"].clone())
        .collect();
    // Deliveries run as independent children, so their log order is not
    // the event order. Three runs must still yield all three distinct
    // outcomes: completed, error and the unknown null. Reusing a previous
    // run's outcome would repeat one value and drop another.
    let mut labels: Vec<&str> = outcomes
        .iter()
        .map(|outcome| outcome.as_str().unwrap_or("null"))
        .collect();
    labels.sort_unstable();
    assert_eq!(
        labels,
        vec!["completed", "error", "null"],
        "each run reports its own outcome, never the previous run's: {outcomes:?}"
    );
}

// Draft tests for tests/agent_lifecycle_event_test.rs (PI-14/PI-17).

#[test]
fn pi_asset_install_writes_only_a_verified_owned_asset() {
    use libra::internal::ai::hooks::providers::pi::settings::{
        PiAssetOwnership, PiAssetWrite, pi_asset_ownership, write_owned_pi_asset,
    };

    let temp = tempfile::tempdir().expect("temp project");
    let binary = temp.path().join("bin").join("libra");
    assert_eq!(
        write_owned_pi_asset(temp.path(), &binary).expect("install"),
        PiAssetWrite::Created
    );
    assert_eq!(
        pi_asset_ownership(temp.path()).expect("ownership"),
        PiAssetOwnership::Owned
    );

    let user = tempfile::tempdir().expect("user project");
    let slot = user
        .path()
        .join(".pi")
        .join("extensions")
        .join("libra-hooks.ts");
    std::fs::create_dir_all(slot.parent().expect("slot parent")).expect("mkdir");
    std::fs::write(&slot, "the user's own extension").expect("user file");
    let error = write_owned_pi_asset(user.path(), &binary)
        .expect_err("an unmanaged file is never replaced");
    assert!(
        error.to_string().contains("not a libra-managed"),
        "{error:#}"
    );
    assert_eq!(
        std::fs::read_to_string(&slot).expect("user bytes"),
        "the user's own extension"
    );
    assert_eq!(
        pi_asset_ownership(user.path()).expect("ownership"),
        PiAssetOwnership::Unmanaged
    );
}

#[test]
fn pi_asset_install_is_idempotent() {
    use libra::internal::ai::hooks::providers::pi::settings::{
        PiAssetWrite, pi_asset_path, write_owned_pi_asset,
    };

    let temp = tempfile::tempdir().expect("temp project");
    let binary = temp.path().join("libra");
    assert_eq!(
        write_owned_pi_asset(temp.path(), &binary).expect("first install"),
        PiAssetWrite::Created
    );
    let first = std::fs::read(pi_asset_path(temp.path())).expect("asset");
    assert_eq!(
        write_owned_pi_asset(temp.path(), &binary).expect("second install"),
        PiAssetWrite::Unchanged
    );
    assert_eq!(
        std::fs::read(pi_asset_path(temp.path())).expect("asset"),
        first
    );
}

#[test]
fn pi_asset_uninstall_is_idempotent() {
    use libra::internal::ai::hooks::providers::pi::settings::{
        PiAssetOwnership, PiAssetRemove, pi_asset_ownership, remove_owned_pi_asset,
        write_owned_pi_asset,
    };

    let temp = tempfile::tempdir().expect("temp project");
    let binary = temp.path().join("libra");
    write_owned_pi_asset(temp.path(), &binary).expect("install");
    assert_eq!(
        remove_owned_pi_asset(temp.path()).expect("first remove"),
        PiAssetRemove::Removed
    );
    assert_eq!(
        pi_asset_ownership(temp.path()).expect("ownership"),
        PiAssetOwnership::Absent
    );
    assert_eq!(
        remove_owned_pi_asset(temp.path()).expect("second remove"),
        PiAssetRemove::Absent
    );
}

#[test]
fn pi_asset_uninstall_removes_only_the_owned_asset() {
    use libra::internal::ai::hooks::providers::pi::settings::{
        PI_ASSET_FILE, PiAssetOwnership, remove_owned_pi_asset, write_owned_pi_asset,
    };

    let temp = tempfile::tempdir().expect("temp project");
    write_owned_pi_asset(temp.path(), &temp.path().join("libra")).expect("install");
    let slot = temp
        .path()
        .join(".pi")
        .join("extensions")
        .join(PI_ASSET_FILE);
    assert!(slot.exists());
    remove_owned_pi_asset(temp.path()).expect("remove");
    assert!(!slot.exists(), "the owned asset is removed");
    assert!(
        slot.parent().expect("parent").is_dir(),
        "the extension directory is not removed"
    );
    assert_eq!(
        libra::internal::ai::hooks::providers::pi::settings::pi_asset_ownership(temp.path())
            .expect("ownership"),
        PiAssetOwnership::Absent
    );
}

#[test]
fn pi_asset_uninstall_leaves_non_owned_or_drifted_files() {
    use libra::internal::ai::hooks::providers::pi::settings::{
        PI_ASSET_FILE, PiAssetOwnership, pi_asset_ownership, pi_asset_path, remove_owned_pi_asset,
        write_owned_pi_asset,
    };

    let user = tempfile::tempdir().expect("user project");
    let user_slot = pi_asset_path(user.path());
    std::fs::create_dir_all(user_slot.parent().expect("parent")).expect("mkdir");
    std::fs::write(&user_slot, "the user's own extension").expect("user file");
    let error = remove_owned_pi_asset(user.path()).expect_err("an unmanaged file is kept");
    assert!(
        error.to_string().contains("not a libra-managed"),
        "{error:#}"
    );
    assert_eq!(
        std::fs::read_to_string(&user_slot).expect("user bytes"),
        "the user's own extension"
    );

    let drifted = tempfile::tempdir().expect("drifted project");
    write_owned_pi_asset(drifted.path(), &drifted.path().join("libra")).expect("install");
    let drifted_slot = drifted
        .path()
        .join(".pi")
        .join("extensions")
        .join(PI_ASSET_FILE);
    let mut bytes = std::fs::read(&drifted_slot).expect("asset");
    bytes.extend_from_slice(b"\n// local edit\n");
    std::fs::write(&drifted_slot, &bytes).expect("drift");
    let error = remove_owned_pi_asset(drifted.path()).expect_err("a drifted file is kept");
    assert!(
        error.to_string().contains("changed after libra wrote it"),
        "{error:#}"
    );
    assert_eq!(std::fs::read(&drifted_slot).expect("drifted bytes"), bytes);
    assert_eq!(
        pi_asset_ownership(drifted.path()).expect("ownership"),
        PiAssetOwnership::Drifted
    );
}

#[test]
fn pi_asset_install_never_writes_pi_trust_state() {
    use libra::internal::ai::hooks::providers::pi::settings::{
        PI_ASSET_FILE, pi_asset_path, remove_owned_pi_asset, write_owned_pi_asset,
    };

    let temp = tempfile::tempdir().expect("temp project");
    write_owned_pi_asset(temp.path(), &temp.path().join("libra")).expect("install");
    let asset = std::fs::read_to_string(pi_asset_path(temp.path())).expect("asset");
    assert!(
        !asset.contains("trust.json") && !asset.contains("--approve"),
        "the asset must not manage Pi trust state"
    );
    let trust = temp.path().join(".pi").join("agent").join("trust.json");
    assert!(!trust.exists(), "install wrote a Pi trust file");
    remove_owned_pi_asset(temp.path()).expect("remove");
    assert!(!trust.exists(), "uninstall wrote a Pi trust file");
    assert!(
        !temp
            .path()
            .join(".pi")
            .join("extensions")
            .join(PI_ASSET_FILE)
            .exists()
    );
}

#[test]
fn pi_asset_install_detects_a_missing_pi_binary() {
    use libra::internal::ai::hooks::providers::pi::settings::pi_binary_on_path;

    let empty = tempfile::tempdir().expect("empty dir");
    assert!(
        pi_binary_on_path(Some(empty.path().as_os_str())).is_none(),
        "an empty PATH directory has no Pi binary"
    );
    assert!(pi_binary_on_path(None).is_none());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let bin = tempfile::tempdir().expect("bin dir");
        let pi = bin.path().join("pi");
        std::fs::write(&pi, b"#!/bin/sh\necho 1.4.0\n").expect("fake pi");
        let mut permissions = std::fs::metadata(&pi).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&pi, permissions).expect("chmod");
        assert_eq!(pi_binary_on_path(Some(bin.path().as_os_str())), Some(pi));

        // A plain file named `pi` is not an executable binary.
        let plain_dir = tempfile::tempdir().expect("plain dir");
        let plain = plain_dir.path().join("pi");
        std::fs::write(&plain, b"not executable").expect("plain file");
        let mut permissions = std::fs::metadata(&plain).expect("metadata").permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&plain, permissions).expect("chmod");
        assert!(pi_binary_on_path(Some(plain_dir.path().as_os_str())).is_none());
    }
}

#[test]
fn pi_asset_doctor_reports_a_missing_asset() {
    use libra::internal::ai::hooks::providers::pi::settings::{
        PiDiagnostic, PiProjectTrust, diagnose_pi_project,
    };

    let temp = tempfile::tempdir().expect("temp project");
    let diagnosis =
        diagnose_pi_project(temp.path(), None, PiProjectTrust::Granted).expect("diagnose");
    assert!(diagnosis.findings.contains(&PiDiagnostic::AssetMissing));
    assert!(!diagnosis.findings.contains(&PiDiagnostic::TrustNotGranted));
}

#[test]
fn pi_asset_doctor_reports_asset_drift() {
    use libra::internal::ai::hooks::providers::pi::settings::{
        PiDiagnostic, PiProjectTrust, diagnose_pi_project, pi_asset_path, write_owned_pi_asset,
    };

    let temp = tempfile::tempdir().expect("temp project");
    write_owned_pi_asset(temp.path(), &temp.path().join("libra")).expect("install");
    let slot = pi_asset_path(temp.path());
    let mut bytes = std::fs::read(&slot).expect("asset");
    bytes.extend_from_slice(b"\n// local edit\n");
    std::fs::write(&slot, bytes).expect("drift");
    let diagnosis =
        diagnose_pi_project(temp.path(), None, PiProjectTrust::Granted).expect("diagnose");
    assert!(diagnosis.findings.contains(&PiDiagnostic::AssetDrifted));
}

#[test]
fn pi_asset_doctor_reports_a_trust_coverage_gap() {
    use libra::internal::ai::hooks::providers::pi::settings::{
        PiDiagnostic, PiProjectTrust, diagnose_pi_project, write_owned_pi_asset,
    };

    let temp = tempfile::tempdir().expect("temp project");
    write_owned_pi_asset(temp.path(), &temp.path().join("libra")).expect("install");
    for trust in [PiProjectTrust::Denied, PiProjectTrust::Unknown] {
        let diagnosis = diagnose_pi_project(temp.path(), None, trust).expect("diagnose");
        assert!(
            diagnosis.findings.contains(&PiDiagnostic::TrustNotGranted),
            "{trust:?}: {:?}",
            diagnosis.findings
        );
    }
    let diagnosis =
        diagnose_pi_project(temp.path(), None, PiProjectTrust::Granted).expect("diagnose");
    assert!(!diagnosis.findings.contains(&PiDiagnostic::TrustNotGranted));
    assert!(
        diagnosis
            .findings
            .contains(&PiDiagnostic::SubdirectoryLaunchGap),
        "the subdirectory launch gap is always reported"
    );
}

#[test]
#[cfg(unix)]
fn pi_asset_doctor_reports_a_pi_version_drift() {
    use std::os::unix::fs::PermissionsExt;

    use libra::internal::ai::hooks::providers::pi::settings::{
        PiDiagnostic, PiProjectTrust, diagnose_pi_project, write_owned_pi_asset,
    };

    let temp = tempfile::tempdir().expect("temp project");
    write_owned_pi_asset(temp.path(), &temp.path().join("libra")).expect("install");
    let bin = tempfile::tempdir().expect("bin dir");
    let fake = bin.path().join("pi");
    std::fs::write(&fake, b"#!/bin/sh\necho 1.5.0\n").expect("fake pi");
    let mut permissions = std::fs::metadata(&fake).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake, permissions).expect("chmod");
    let diagnosis =
        diagnose_pi_project(temp.path(), Some(&fake), PiProjectTrust::Granted).expect("diagnose");
    assert!(
        diagnosis.findings.iter().any(|finding| matches!(
            finding,
            PiDiagnostic::VersionDrift { installed, audited }
                if installed == "1.5.0" && *audited == "1.4.0"
        )),
        "{:?}",
        diagnosis.findings
    );

    std::fs::write(&fake, b"#!/bin/sh\necho 1.4.0\n").expect("fake audited pi");
    let diagnosis =
        diagnose_pi_project(temp.path(), Some(&fake), PiProjectTrust::Granted).expect("diagnose");
    assert!(
        !diagnosis
            .findings
            .iter()
            .any(|finding| matches!(finding, PiDiagnostic::VersionDrift { .. }))
    );
}

// ---------------------------------------------------------------------------
// PI-35: bounded Pi session source selection (`pi_session_source_*`).
// ---------------------------------------------------------------------------

// PI-35: bounded Pi session source selection (`pi_session_source_*`).

struct PiSourceFixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
    sessions: PathBuf,
}

impl PiSourceFixture {
    fn create(_cwd: &str) -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let home = temp.path().join("home");
        let cwd = temp.path().join("repo");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let encoded = cwd
            .to_str()
            .expect("utf-8 cwd")
            .trim_start_matches('/')
            .replace(['/', '\\', ':'], "-");
        let sessions = home
            .join(".pi")
            .join("agent")
            .join("sessions")
            .join(format!("--{encoded}--"));
        std::fs::create_dir_all(&sessions).expect("sessions");
        let _ = cwd;
        Self {
            _temp: temp,
            home,
            cwd,
            sessions,
        }
    }

    fn write_session(&self, id: &str, _cwd: &Path, bytes: &[u8]) -> PathBuf {
        let name = format!("2026-11-04T00-00-00-000Z_{id}.jsonl");
        let path = self.sessions.join(name);
        std::fs::write(&path, bytes).expect("session file");
        path
    }

    fn session_bytes(&self, id: &str, cwd: &Path) -> Vec<u8> {
        format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-11-04T00:00:00.000Z\",\"cwd\":\"{}\"}}\n{{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"timestamp\":\"2026-11-04T00:00:01.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n",
            cwd.display()
        )
        .into_bytes()
    }
}

fn with_pi_source_home<T>(fixture: &PiSourceFixture, run: impl FnOnce() -> T) -> T {
    let previous = std::env::var_os("LIBRA_TEST_HOME");
    // SAFETY: serialized tests set and restore their own environment.
    unsafe { std::env::set_var("LIBRA_TEST_HOME", &fixture.home) };
    let result = run();
    match previous {
        Some(value) => {
            // SAFETY: serialized test restoring its own mutation.
            unsafe { std::env::set_var("LIBRA_TEST_HOME", value) };
        }
        None => {
            // SAFETY: serialized test restoring its own mutation.
            unsafe { std::env::remove_var("LIBRA_TEST_HOME") };
        }
    }
    result
}

#[test]
#[serial(env)]
fn pi_session_source_encodes_the_default_directory() {
    use libra::internal::ai::observed_agents::pi_live::default_pi_session_directory;

    let fixture = PiSourceFixture::create("/repo/sub");
    with_pi_source_home(&fixture, || {
        let directory =
            default_pi_session_directory(&fixture.cwd).expect("default session directory");
        let encoded = fixture
            .cwd
            .to_str()
            .expect("utf-8")
            .trim_start_matches('/')
            .replace(['/', '\\', ':'], "-");
        assert!(
            directory.ends_with(format!("--{encoded}--")),
            "{}",
            directory.display()
        );
        assert_eq!(
            directory,
            fixture
                .home
                .join(".pi/agent/sessions")
                .join(format!("--{encoded}--"))
        );
    });
}

#[test]
#[serial(env)]
fn pi_session_source_selects_the_matching_session() {
    use libra::internal::ai::observed_agents::pi_live::{
        pi_live_counters, pi_source_control_for_test, pi_source_status,
        select_pi_session_source_for_test,
    };

    let fixture = PiSourceFixture::create("/repo");
    let bytes = fixture.session_bytes("sess-1", &fixture.cwd);
    fixture.write_session("sess-1", &fixture.cwd, &bytes);
    with_pi_source_home(&fixture, || {
        pi_live_counters::reset();
        let control = pi_source_control_for_test(
            &fixture.sessions,
            "sess-1",
            &fixture.cwd,
            libra::internal::ai::observed_agents::builtin::pi::PiDiscoveryLimits::default(),
            5_000,
        );
        let (status, selected) = select_pi_session_source_for_test(&control, 1024 * 1024);
        assert_eq!(status, pi_source_status::COMPLETE);
        assert_eq!(selected, bytes);
        assert_eq!(pi_live_counters::opened_count(), 1);
    });
}

#[test]
#[serial(env)]
fn pi_session_source_reads_only_the_named_candidate() {
    use libra::internal::ai::observed_agents::pi_live::{
        pi_live_counters, pi_source_control_for_test, pi_source_status,
        select_pi_session_source_for_test,
    };

    let fixture = PiSourceFixture::create("/repo");
    let bytes = fixture.session_bytes("sess-1", &fixture.cwd);
    fixture.write_session("sess-1", &fixture.cwd, &bytes);
    fixture.write_session(
        "sess-2",
        &fixture.cwd,
        &fixture.session_bytes("sess-2", &fixture.cwd),
    );
    with_pi_source_home(&fixture, || {
        pi_live_counters::reset();
        let control = pi_source_control_for_test(
            &fixture.sessions,
            "sess-1",
            &fixture.cwd,
            libra::internal::ai::observed_agents::builtin::pi::PiDiscoveryLimits::default(),
            5_000,
        );
        let (status, selected) = select_pi_session_source_for_test(&control, 1024 * 1024);
        assert_eq!(status, pi_source_status::COMPLETE);
        assert_eq!(selected, bytes);
        assert_eq!(
            pi_live_counters::opened_count(),
            1,
            "only the file name carrying the session id is opened"
        );
    });
}

#[test]
#[serial(env)]
fn pi_session_source_refuses_unusable_candidates() {
    use libra::internal::ai::observed_agents::{
        builtin::pi::PiDiscoveryLimits,
        pi_live::{
            pi_source_control_for_test, pi_source_status, select_pi_session_source_for_test,
        },
    };

    let fixture = PiSourceFixture::create("/repo");
    with_pi_source_home(&fixture, || {
        let default_limits = PiDiscoveryLimits::default();
        let control = |directory: &Path, id: &str, limits| {
            pi_source_control_for_test(directory, id, &fixture.cwd, limits, 5_000)
        };
        let status = |control: String, cap: u64| select_pi_session_source_for_test(&control, cap).0;

        // Missing file.
        assert_eq!(
            status(
                control(&fixture.sessions, "sess-1", default_limits),
                1024 * 1024
            ),
            pi_source_status::ABSENT
        );
        // Ambiguous: two valid files with the same id.
        let bytes = fixture.session_bytes("sess-1", &fixture.cwd);
        fixture.write_session("sess-1", &fixture.cwd, &bytes);
        std::fs::write(
            fixture
                .sessions
                .join("2026-11-04T00-00-01-000Z_sess-1.jsonl"),
            &bytes,
        )
        .expect("second session file");
        assert_eq!(
            status(
                control(&fixture.sessions, "sess-1", default_limits),
                1024 * 1024
            ),
            pi_source_status::ABSENT
        );
        std::fs::remove_dir_all(&fixture.sessions).expect("reset sessions");
        std::fs::create_dir_all(&fixture.sessions).expect("sessions");

        // Header id mismatch.
        fixture.write_session(
            "sess-1",
            &fixture.cwd,
            &fixture.session_bytes("other", &fixture.cwd),
        );
        assert_eq!(
            status(
                control(&fixture.sessions, "sess-1", default_limits),
                1024 * 1024
            ),
            pi_source_status::ABSENT
        );
        std::fs::remove_dir_all(&fixture.sessions).expect("reset sessions");
        std::fs::create_dir_all(&fixture.sessions).expect("sessions");

        // Header cwd mismatch.
        fixture.write_session(
            "sess-1",
            &fixture.cwd,
            &fixture.session_bytes("sess-1", Path::new("/somewhere/else")),
        );
        assert_eq!(
            status(
                control(&fixture.sessions, "sess-1", default_limits),
                1024 * 1024
            ),
            pi_source_status::ABSENT
        );

        // Entry limit, header limit and source cap.
        fixture.write_session("sess-1", &fixture.cwd, &bytes);
        fixture.write_session(
            "sess-2",
            &fixture.cwd,
            &fixture.session_bytes("sess-2", &fixture.cwd),
        );
        let mut one_entry = default_limits;
        one_entry.max_entries = 1;
        assert_eq!(
            status(control(&fixture.sessions, "sess-1", one_entry), 1024 * 1024),
            pi_source_status::OVERSIZE
        );
        let mut short_header = default_limits;
        short_header.max_header_bytes = 8;
        assert_eq!(
            status(
                control(&fixture.sessions, "sess-1", short_header),
                1024 * 1024
            ),
            pi_source_status::OVERSIZE
        );
        assert_eq!(
            status(control(&fixture.sessions, "sess-1", default_limits), 8),
            pi_source_status::OVERSIZE
        );

        // A directory outside the configured provider root.
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(
            outside.path().join("2026-11-04T00-00-00-000Z_sess-1.jsonl"),
            &bytes,
        )
        .expect("outside file");
        assert_eq!(
            status(
                control(outside.path(), "sess-1", default_limits),
                1024 * 1024
            ),
            pi_source_status::UNTRUSTED
        );
    });
}

#[tokio::test]
#[serial(env)]
async fn pi_session_source_stops_at_the_deadline() {
    use std::time::{Duration, Instant};

    use libra::internal::ai::observed_agents::{
        builtin::pi::PiDiscoveryLimits,
        pi_live::{PiSessionSourceRead, read_pi_session_source_until},
    };

    let fixture = PiSourceFixture::create("/repo");
    let bytes = fixture.session_bytes("sess-1", &fixture.cwd);
    fixture.write_session("sess-1", &fixture.cwd, &bytes);
    let previous_home = std::env::var_os("LIBRA_TEST_HOME");
    // SAFETY: serialized test; the helper child inherits these values.
    unsafe { std::env::set_var("LIBRA_TEST_HOME", &fixture.home) };
    // SAFETY: serialized test setting a test-only stall hook and helper.
    unsafe { std::env::set_var("LIBRA_TEST", "1") };
    // SAFETY: serialized test; the integration test binary is not libra.
    unsafe { std::env::set_var("LIBRA_TEST_PI_SOURCE_HELPER", env!("CARGO_BIN_EXE_libra")) };
    for stage in ["list", "header", "body"] {
        // SAFETY: serialized test; the helper child inherits this value.
        unsafe { std::env::set_var("LIBRA_TEST_PI_SOURCE_STALL", stage) };
        let started = Instant::now();
        let result = read_pi_session_source_until(
            &fixture.sessions,
            "sess-1",
            &fixture.cwd,
            PiDiscoveryLimits::default(),
            Instant::now() + Duration::from_millis(400),
        )
        .await;
        assert_eq!(
            result,
            PiSessionSourceRead::DeadlineExceeded,
            "stage {stage}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "stage {stage} exceeded the deadline by too much: {:?}",
            started.elapsed()
        );
    }
    // SAFETY: serialized test restoring its own mutation.
    unsafe { std::env::remove_var("LIBRA_TEST_PI_SOURCE_STALL") };
    // SAFETY: serialized test restoring its own mutation.
    unsafe { std::env::remove_var("LIBRA_TEST") };
    // SAFETY: serialized test restoring its own mutation.
    unsafe { std::env::remove_var("LIBRA_TEST_PI_SOURCE_HELPER") };
    match previous_home {
        Some(value) => {
            // SAFETY: serialized test restoring its own mutation.
            unsafe { std::env::set_var("LIBRA_TEST_HOME", value) };
        }
        None => {
            // SAFETY: serialized test restoring its own mutation.
            unsafe { std::env::remove_var("LIBRA_TEST_HOME") };
        }
    }
}

// ---------------------------------------------------------------------------
// PI-36: the observe checkpoint's bounded Pi session source (`pi_observe_source_*`).
// ---------------------------------------------------------------------------

fn pi_live_source_file(repo: &HookRepo, session: &str, thinking: &str, answer: &str) -> PathBuf {
    let encoded = repo
        .repo
        .to_str()
        .expect("utf-8 repo")
        .trim_start_matches('/')
        .replace(['/', '\\', ':'], "-");
    let directory = repo
        .home
        .join(".pi")
        .join("agent")
        .join("sessions")
        .join(format!("--{encoded}--"));
    std::fs::create_dir_all(&directory).expect("Pi session directory");
    let cwd = serde_json::to_string(&repo.repo.display().to_string()).expect("cwd json");
    let file = directory.join(format!("2026-11-04T00-00-00-000Z_{session}.jsonl"));
    let bytes = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"{session}\",\"timestamp\":\"2026-11-04T00:00:00.000Z\",\"cwd\":{cwd}}}\n{{\"type\":\"message\",\"id\":\"u1\",\"parentId\":null,\"timestamp\":\"2026-11-04T00:00:01.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n{{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"timestamp\":\"2026-11-04T00:00:02.000Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"thinking\",\"thinking\":{thinking_json}}},{{\"type\":\"text\",\"text\":{answer_json}}}]}}}}\n",
        thinking_json = serde_json::to_string(thinking).expect("thinking json"),
        answer_json = serde_json::to_string(answer).expect("answer json"),
    );
    std::fs::write(&file, bytes).expect("Pi session file");
    file
}

#[tokio::test]
async fn pi_observe_source_writes_a_checkpoint_with_thinking() {
    let repo = HookRepo::init();
    let session = "pi-live-source";
    pi_live_source_file(&repo, session, "PI36-THINKING-CANARY", "PI36-ANSWER-CANARY");
    let start = pi_frame_with(
        &repo,
        "session_start",
        json!({"sessionId": session, "frameId": pi_frame_id(30)}),
    );
    pi_observe(&repo, &start);
    let run = pi_frame_with(
        &repo,
        "before_agent_start",
        json!({"sessionId": session, "frameId": pi_frame_id(31)}),
    );
    pi_observe(&repo, &run);
    let settled = pi_frame_with(
        &repo,
        "agent_settled",
        json!({"sessionId": session, "frameId": pi_frame_id(32), "outcome": "completed"}),
    );
    let out = repo.hook("pi", "observe", &settled.to_string());
    assert!(out.status.success(), "{}", describe(&out));
    let checkpoints = repo.checkpoints();
    assert_eq!(checkpoints.len(), 1, "{checkpoints:?}");
    let checkpoint_id = checkpoints[0]["checkpoint_id"]
        .as_str()
        .expect("checkpoint id");
    let export = repo.run(&["agent", "checkpoint", "export", checkpoint_id], None);
    assert!(export.status.success(), "{}", describe(&export));
    let text = String::from_utf8_lossy(&export.stdout);
    assert!(
        text.contains("PI36-THINKING-CANARY"),
        "the live checkpoint must keep the session thinking: {text}"
    );
    assert!(
        text.contains("PI36-ANSWER-CANARY"),
        "the live checkpoint must keep the assistant answer: {text}"
    );
    let metadata_oid = checkpoints[0]["metadata_blob_oid"]
        .as_str()
        .expect("metadata blob oid");
    let metadata = repo.run(&["cat-file", "-p", metadata_oid], None);
    assert!(metadata.status.success(), "{}", describe(&metadata));
    let metadata_text = String::from_utf8_lossy(&metadata.stdout);
    assert!(
        metadata_text.contains("\"completeness\": \"partial\"")
            && metadata_text.contains("\"partial_reason\": \"unproven_ordinary\""),
        "the Pi snapshot must stay partial and unproven: {metadata_text}"
    );
    assert!(
        repo.durable_session(&format!("pi__{session}")).await.state == "active",
        "the observe checkpoint leaves the session active"
    );
}

#[tokio::test]
async fn pi_observe_source_without_a_session_file_keeps_the_metadata_checkpoint() {
    let repo = HookRepo::init();
    let session = "pi-live-missing";
    let start = pi_frame_with(
        &repo,
        "session_start",
        json!({"sessionId": session, "frameId": pi_frame_id(40)}),
    );
    pi_observe(&repo, &start);
    let settled = pi_frame_with(
        &repo,
        "agent_settled",
        json!({"sessionId": session, "frameId": pi_frame_id(41), "outcome": "completed"}),
    );
    let out = repo.hook("pi", "observe", &settled.to_string());
    assert!(out.status.success(), "{}", describe(&out));
    let checkpoints = repo.checkpoints();
    assert_eq!(checkpoints.len(), 1, "{checkpoints:?}");
    let checkpoint_id = checkpoints[0]["checkpoint_id"]
        .as_str()
        .expect("checkpoint id");
    let export = repo.run(&["agent", "checkpoint", "export", checkpoint_id], None);
    assert!(export.status.success(), "{}", describe(&export));
    let text = String::from_utf8_lossy(&export.stdout);
    assert!(
        !text.contains("PI36-"),
        "a metadata-only checkpoint carries no transcript content: {text}"
    );
}

#[tokio::test]
async fn pi_observe_source_refuses_a_bad_frame() {
    let repo = HookRepo::init();
    let session = "pi-live-bad-frame";
    let encoded = repo
        .repo
        .to_str()
        .expect("utf-8 repo")
        .trim_start_matches('/')
        .replace(['/', '\\', ':'], "-");
    let directory = repo
        .home
        .join(".pi")
        .join("agent")
        .join("sessions")
        .join(format!("--{encoded}--"));
    std::fs::create_dir_all(&directory).expect("Pi session directory");
    let cwd = serde_json::to_string(&repo.repo.display().to_string()).expect("cwd json");
    std::fs::write(
        directory.join(format!("2026-11-04T00-00-00-000Z_{session}.jsonl")),
        format!("{{\"type\":\"session\",\"version\":2,\"id\":\"{session}\",\"cwd\":{cwd}}}\n"),
    )
    .expect("v2 session file");
    let start = pi_frame_with(
        &repo,
        "session_start",
        json!({"sessionId": session, "frameId": pi_frame_id(50)}),
    );
    pi_observe(&repo, &start);
    let settled = pi_frame_with(
        &repo,
        "agent_settled",
        json!({"sessionId": session, "frameId": pi_frame_id(51), "outcome": "completed"}),
    );
    let out = repo.hook("pi", "observe", &settled.to_string());
    assert!(
        !out.status.success(),
        "a Pi reader refusal fails closed: {}",
        describe(&out)
    );
    assert!(
        repo.checkpoints().is_empty(),
        "a Pi reader refusal must not commit a checkpoint: {:?}",
        repo.checkpoints()
    );
}

#[test]
fn pi_observe_source_live_capture_contract() {
    use libra::internal::ai::observed_agents::{
        AgentKind, live_capture_matches_hook_installable, registry::registration_for,
    };

    assert!(registration_for(AgentKind::Pi).hook_installable);
    assert!(live_capture_matches_hook_installable(AgentKind::Pi));
    for kind in AgentKind::all() {
        assert!(
            live_capture_matches_hook_installable(*kind),
            "{kind:?} broke the live-capture/hook-installable contract"
        );
    }
}

// ---------------------------------------------------------------------------
// PI-37: durable links between Pi live tool events (`pi_tool_correlation_*`).
// ---------------------------------------------------------------------------

/// Deliver one frame through the bounded helper path a managed Pi hook uses
/// (`--capture-budget-ms`), as opposed to [`pi_observe`]'s in-process path.
fn pi_observe_budgeted(repo: &HookRepo, frame: &Value) {
    let out = repo.run(
        &[
            "agent",
            "hooks",
            "--capture-budget-ms",
            "1500",
            "pi",
            "observe",
        ],
        Some(&frame.to_string()),
    );
    assert!(out.status.success(), "{frame}: {}", describe(&out));
}

/// The session's persisted tool links, oldest first.
async fn pi_links_of(repo: &HookRepo, session_id: &str) -> Vec<Value> {
    repo.durable_session(&format!("pi__{session_id}"))
        .await
        .metadata
        .get("capture_tool_links_v1")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

async fn pi_links(repo: &HookRepo) -> Vec<Value> {
    pi_links_of(repo, PI_SESSION_ID).await
}

fn pi_tool_frame(repo: &HookRepo, event: &str, id: &str, parent: Option<&str>) -> Value {
    pi_frame_with(
        repo,
        event,
        json!({"toolCallId": id, "parentToolCallId": parent}),
    )
}

fn pi_link_identity(value: &Value) -> String {
    let text = value.as_str().expect("link identity is a string");
    let hex = text
        .strip_prefix("capture-tool-link-v1:")
        .expect("link identity prefix");
    assert!(
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{text}"
    );
    text.to_string()
}

#[tokio::test]
async fn pi_tool_correlation_pairs_a_call_with_its_result() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    // The result first, through the helper path; the call later, in process.
    pi_observe_budgeted(&repo, &pi_tool_frame(&repo, "tool_result", "call-1", None));
    pi_observe(&repo, &pi_tool_frame(&repo, "tool_call", "call-1", None));
    let links = pi_links(&repo).await;
    assert_eq!(links.len(), 2, "{links:?}");
    assert_eq!(
        pi_link_identity(&links[0]["tool"]),
        pi_link_identity(&links[1]["tool"]),
        "a call and its result share one tool identity: {links:?}"
    );
    assert_ne!(links[0]["action"], links[1]["action"], "{links:?}");
}

#[tokio::test]
async fn pi_tool_correlation_gives_each_call_its_own_identity() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    pi_observe(&repo, &pi_tool_frame(&repo, "tool_call", "call-1", None));
    pi_observe_budgeted(&repo, &pi_tool_frame(&repo, "tool_call", "call-2", None));
    let links = pi_links(&repo).await;
    assert_eq!(links.len(), 2, "{links:?}");
    assert_ne!(
        pi_link_identity(&links[0]["tool"]),
        pi_link_identity(&links[1]["tool"]),
        "{links:?}"
    );
}

#[tokio::test]
async fn pi_tool_correlation_attributes_a_nested_event_to_its_parent() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    // The nested call arrives before its parent call.
    pi_observe_budgeted(
        &repo,
        &pi_tool_frame(&repo, "tool_call", "call-1/1", Some("call-1")),
    );
    pi_observe(&repo, &pi_tool_frame(&repo, "tool_call", "call-1", None));
    let links = pi_links(&repo).await;
    assert_eq!(links.len(), 2, "{links:?}");
    let (nested, parent) = (&links[0], &links[1]);
    assert_eq!(
        pi_link_identity(&nested["parent"]),
        pi_link_identity(&parent["tool"]),
        "the nested event names its parent call's identity: {links:?}"
    );
    assert_ne!(nested["tool"], parent["tool"], "{links:?}");
    assert!(parent.get("parent").is_none(), "{links:?}");
}

#[tokio::test]
async fn pi_tool_correlation_keeps_the_parent_when_it_never_arrives() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    pi_observe(
        &repo,
        &pi_tool_frame(&repo, "tool_call", "call-9/1", Some("call-9")),
    );
    pi_observe_budgeted(
        &repo,
        &pi_tool_frame(&repo, "tool_result", "call-9/1", Some("call-9")),
    );
    let links = pi_links(&repo).await;
    assert_eq!(links.len(), 2, "{links:?}");
    let parent = pi_link_identity(&links[0]["parent"]);
    assert_eq!(parent, pi_link_identity(&links[1]["parent"]), "{links:?}");
    assert_ne!(parent, pi_link_identity(&links[0]["tool"]), "{links:?}");
}

#[tokio::test]
async fn pi_tool_correlation_stores_no_raw_ids() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    pi_observe(
        &repo,
        &pi_tool_frame(&repo, "tool_call", "PI_RAW_PARENT_ID", None),
    );
    pi_observe_budgeted(
        &repo,
        &pi_tool_frame(
            &repo,
            "tool_call",
            "PI_RAW_PARENT_ID/1",
            Some("PI_RAW_PARENT_ID"),
        ),
    );
    let session = repo.durable_session(&pi_libra_session()).await;
    let text = session.metadata.to_string();
    assert!(!text.contains("PI_RAW_PARENT_ID"), "{text}");
    let links = pi_links(&repo).await;
    assert_eq!(links.len(), 2, "{links:?}");
    for link in &links {
        pi_link_identity(&link["tool"]);
    }
    pi_link_identity(&links[1]["parent"]);
}

#[tokio::test]
async fn pi_tool_correlation_differs_between_sessions() {
    const OTHER: &str = "019a0c3e-7b1d-7c2e-9f10-5a6b7c8d9e10";
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    pi_observe(
        &repo,
        &pi_frame_with(
            &repo,
            "session_start",
            json!({"sessionId": OTHER, "frameId": pi_frame_id(1)}),
        ),
    );
    pi_observe(&repo, &pi_tool_frame(&repo, "tool_call", "call-1", None));
    pi_observe(
        &repo,
        &pi_frame_with(
            &repo,
            "tool_call",
            json!({"sessionId": OTHER, "toolCallId": "call-1"}),
        ),
    );
    let here = pi_links(&repo).await;
    let there = pi_links_of(&repo, OTHER).await;
    assert_eq!((here.len(), there.len()), (1, 1), "{here:?} {there:?}");
    assert_ne!(
        pi_link_identity(&here[0]["tool"]),
        pi_link_identity(&there[0]["tool"]),
        "the same tool call ID in two sessions gives two identities"
    );
}

#[tokio::test]
async fn pi_tool_correlation_leaves_links_unchanged_on_replay() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    let call = pi_tool_frame(&repo, "tool_call", "call-4/1", Some("call-4"));
    pi_observe(&repo, &call);
    let before = serde_json::to_string(&pi_links(&repo).await).unwrap();
    // The same frame again, through either path.
    pi_observe(&repo, &call);
    pi_observe_budgeted(&repo, &call);
    let after = serde_json::to_string(&pi_links(&repo).await).unwrap();
    assert_eq!(after, before);
}

#[tokio::test]
async fn pi_tool_correlation_evicts_the_oldest_link_at_capacity() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    let conn = repo.db().await;
    pi_ingest_tool_calls(&repo, &conn, "call-cap", 128).await;
    let full = pi_links(&repo).await;
    assert_eq!(full.len(), 128);
    pi_observe(
        &repo,
        &pi_tool_frame(&repo, "tool_call", "call-cap-next", None),
    );
    let after = pi_links(&repo).await;
    assert_eq!(after.len(), 128);
    assert_eq!(
        after[..127],
        full[1..],
        "the earliest link goes and the rest keep their order"
    );
    assert!(!full.contains(&after[127]), "the new link is added last");
}

#[tokio::test]
async fn pi_tool_correlation_leaves_other_providers_unlinked() {
    let repo = HookRepo::init();
    let session = "sess-pi37-claude-tool";
    let envelope = repo.envelope(
        "PreToolUse",
        session,
        None,
        json!({"event_id": "pi37-claude-tool-v1", "tool_use_id": "toolu_pi37", "tool_name": "Bash"}),
    );
    let out = repo.hook("claude-code", "tool-use", &envelope);
    assert!(out.status.success(), "{}", describe(&out));
    let state = repo.durable_session(&format!("claude__{session}")).await;
    assert!(
        state.metadata.get("capture_tool_links_v1").is_none(),
        "{}",
        state.metadata
    );
}

#[tokio::test]
async fn pi_tool_correlation_reads_sessions_with_and_without_links() {
    let repo = HookRepo::init();
    // A session written before any link, then one with links: both read
    // back through the CLI, and doctor finds nothing.
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    for _ in 0..2 {
        let show = repo.run(
            &["agent", "session", "show", &pi_libra_session(), "--json"],
            None,
        );
        assert!(show.status.success(), "{}", describe(&show));
        pi_observe(&repo, &pi_tool_frame(&repo, "tool_call", "call-r", None));
    }
    assert_eq!(pi_links(&repo).await.len(), 1);
    let doctor = repo.run(&["--json", "agent", "doctor"], None);
    assert!(doctor.status.success(), "{}", describe(&doctor));
    let report: Value = serde_json::from_slice(&doctor.stdout).expect("doctor json");
    assert_eq!(report["data"]["orphan_checkpoints"], json!(0), "{report}");
    assert_eq!(
        report["data"]["checkpoint_store"]["findings"],
        json!([]),
        "{report}"
    );
}

/// Replace one session's `metadata_json` member (test-only fault injection).
async fn pi_set_metadata_member(repo: &HookRepo, session_id: &str, key: &str, value: Value) {
    let conn = repo.db().await;
    let mut metadata = repo.durable_session(session_id).await.metadata;
    metadata[key] = value;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_session SET metadata_json = ? WHERE session_id = ?",
        [metadata.to_string().into(), session_id.into()],
    ))
    .await
    .expect("rewrite session metadata");
}

#[tokio::test]
async fn pi_tool_correlation_replaces_a_malformed_link_list() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    let session = pi_libra_session();
    pi_set_metadata_member(
        &repo,
        &session,
        "capture_tool_links_v1",
        json!([{"tool": "call-raw", "action": 7}]),
    )
    .await;
    let before = repo.durable_session(&session).await;
    pi_observe(&repo, &pi_tool_frame(&repo, "tool_call", "call-m", None));
    let after = repo.durable_session(&session).await;
    assert_eq!(
        after.sync_revision,
        before.sync_revision + 1,
        "capture goes on"
    );
    let links = pi_links(&repo).await;
    assert_eq!(links.len(), 1, "{links:?}");
    pi_link_identity(&links[0]["tool"]);
    let receipts = after.metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        receipts
            .iter()
            .any(|receipt| receipt["action_key"] == links[0]["action"]),
        "the link names the receipt written with it: {links:?} {receipts:?}"
    );
}

#[tokio::test]
async fn pi_tool_correlation_survives_metadata_rewrites() {
    let repo = HookRepo::init();
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    pi_observe(&repo, &pi_tool_frame(&repo, "tool_call", "call-w", None));
    let links = serde_json::to_string(&pi_links(&repo).await).unwrap();
    // Another action rewrites the session metadata.
    pi_observe(&repo, &pi_frame(&repo, "before_agent_start"));
    assert_eq!(
        serde_json::to_string(&pi_links(&repo).await).unwrap(),
        links
    );
    // The checkpoint and terminal paths rewrite metadata through the shared
    // catalog for every provider: links already in a session survive them.
    let claude = "sess-pi37-rewrite";
    let claude_session = format!("claude__{claude}");
    let transcript = repo.write_claude_transcript(&repo.repo, claude, "pi37 rewrite source");
    let out = repo.hook(
        "claude-code",
        "session-start",
        &repo.envelope(
            "SessionStart",
            claude,
            Some(&transcript),
            json!({"event_id": "pi37-rewrite-start-v1"}),
        ),
    );
    assert!(out.status.success(), "{}", describe(&out));
    let seeded = pi_links(&repo).await;
    pi_set_metadata_member(
        &repo,
        &claude_session,
        "capture_tool_links_v1",
        json!(seeded),
    )
    .await;
    let seeded_text = serde_json::to_string(&seeded).unwrap();
    for (verb, name, id) in [
        ("stop", "Stop", "pi37-rewrite-stop-v1"),
        ("session-end", "SessionEnd", "pi37-rewrite-end-v1"),
    ] {
        let out = repo.hook(
            "claude-code",
            verb,
            &repo.envelope(name, claude, Some(&transcript), json!({"event_id": id})),
        );
        assert!(out.status.success(), "{verb}: {}", describe(&out));
        let kept = repo
            .durable_session(&claude_session)
            .await
            .metadata
            .get("capture_tool_links_v1")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(serde_json::to_string(&kept).unwrap(), seeded_text, "{verb}");
    }
    assert!(!repo.checkpoints().is_empty(), "the checkpoint paths ran");
}

#[tokio::test]
async fn pi_tool_correlation_skips_sessions_adopted_by_an_import() {
    let repo = HookRepo::init();
    // An explicit import (PI-10) stores this Pi session as a closed V2
    // import record.
    let dir = repo.home.join("pi-import");
    std::fs::create_dir_all(&dir).expect("pi session dir");
    let file = dir.join(format!("2026-10-31T00-00-00-000Z_{PI_SESSION_ID}.jsonl"));
    let lines = [
        json!({"type": "session", "version": 3, "id": PI_SESSION_ID, "timestamp": "2026-10-31T00:00:00.000Z", "cwd": repo.repo.to_str().expect("utf-8 repo")}),
        json!({"type": "message", "id": "u1", "parentId": null, "timestamp": "2026-10-31T00:00:01.000Z", "message": {"role": "user", "content": "pi37-prompt"}}),
        json!({"type": "message", "id": "a1", "parentId": "u1", "timestamp": "2026-10-31T00:00:01.000Z", "message": {"role": "assistant", "content": [{"type": "text", "text": "pi37-answer"}]}}),
    ];
    std::fs::write(
        &file,
        lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>(),
    )
    .expect("pi session file");
    let path = file.to_str().expect("utf-8 path");
    let import = repo.run(
        &["agent", "import", "--agent", "pi", "--path", path, "--yes"],
        None,
    );
    assert!(import.status.success(), "{}", describe(&import));
    let session = pi_libra_session();
    let before = repo.durable_session(&session).await;
    assert_eq!(
        before.metadata["import_source_schema_version"],
        json!(2),
        "{}",
        before.metadata
    );
    // A live tool event on that session is captured without a link.
    pi_observe(&repo, &pi_tool_frame(&repo, "tool_call", "call-v2", None));
    let after = repo.durable_session(&session).await;
    assert_eq!(
        after.sync_revision,
        before.sync_revision + 1,
        "{}",
        after.metadata
    );
    assert!(
        after.metadata.get("capture_tool_links_v1").is_none(),
        "{}",
        after.metadata
    );
    assert_eq!(after.metadata["import_source_schema_version"], json!(2));
    let show = repo.run(&["agent", "session", "show", &session, "--json"], None);
    assert!(show.status.success(), "{}", describe(&show));
}

#[tokio::test]
async fn pi_tool_correlation_survives_a_doctor_repair() {
    use libra::internal::ai::hooks::providers::claude_provider;
    // A real repairable fault: a terminal receipt with no checkpoint whose
    // finalizer window has passed. `agent doctor --repair` quarantines it,
    // rewriting the session's metadata through the shared catalog.
    let repo = HookRepo::init();
    let session = "sess-pi37-doctor-repair";
    let session_id = format!("claude__{session}");
    let transcript = repo.write_claude_transcript(&repo.repo, session, "pi37 repair source");
    let conn = repo.db().await;
    let start = repo.envelope("SessionStart", session, Some(&transcript), json!({}));
    ingest_agent_traces_payload(
        start.as_bytes(),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("session start without a repository binding");
    let end = repo.envelope(
        "SessionEnd",
        session,
        Some(&transcript),
        json!({"event_id": "pi37-doctor-repair-terminal-v1"}),
    );
    ingest_agent_traces_payload(
        end.as_bytes(),
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("terminal capture leaves a pending finalizer receipt");

    // Seed links, then age the finalizer past its 30-minute window.
    pi_observe(&repo, &pi_frame(&repo, "session_start"));
    pi_observe(&repo, &pi_tool_frame(&repo, "tool_call", "call-dr", None));
    let seeded = pi_links(&repo).await;
    assert_eq!(seeded.len(), 1);
    pi_set_metadata_member(&repo, &session_id, "capture_tool_links_v1", json!(seeded)).await;
    let mut metadata = repo.durable_session(&session_id).await.metadata;
    let finalizer = metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array_mut()
        .and_then(|entries| entries.first_mut())
        .and_then(|entry| entry.get_mut("finalizer"))
        .and_then(Value::as_object_mut)
        .expect("the terminal receipt has a pending finalizer");
    finalizer.insert(
        "first_attempt_millis".to_string(),
        json!(Utc::now().timestamp_millis() - 1_800_001),
    );
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_session SET metadata_json = ? WHERE session_id = ?",
        [metadata.to_string().into(), session_id.clone().into()],
    ))
    .await
    .expect("age the finalizer");
    let before = repo.durable_session(&session_id).await;
    assert_ne!(before.state, "quarantined");
    let before_links = serde_json::to_string(&before.metadata["capture_tool_links_v1"]).unwrap();

    let repair = repo.run(&["--json", "agent", "doctor", "--repair"], None);
    assert!(repair.status.success(), "{}", describe(&repair));
    let report: Value = serde_json::from_slice(&repair.stdout).expect("doctor json");
    assert!(
        report["data"]["checkpoint_store"]["findings"]
            .as_array()
            .is_some_and(|findings| findings
                .iter()
                .any(|finding| finding["inconsistency_type"] == "expired_inflight_marker")),
        "{report}"
    );
    let after = repo.durable_session(&session_id).await;
    assert_eq!(after.state, "quarantined", "doctor repaired the receipt");
    assert_ne!(
        after.metadata, before.metadata,
        "the repair rewrote the metadata"
    );
    assert_eq!(
        serde_json::to_string(&after.metadata["capture_tool_links_v1"]).unwrap(),
        before_links,
        "the links survive the repair byte for byte"
    );
}
