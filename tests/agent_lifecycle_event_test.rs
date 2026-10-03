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
    let added = repo.run(&["worktree", "add", &sibling_arg], None);
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
    let added = repo.run(&["worktree", "add", &linked_arg], None);
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
    let added = repo.run(&["worktree", "add", &linked_arg], None);
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
    let remedy = "from the main worktree run `libra worktree repair --confirm <worktree-path>`";
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

/// Every schema object plus every table column of the repository database,
/// so a migration's physical effect is observable independent of receipts.
async fn physical_repository_schema(conn: &DatabaseConnection) -> Vec<String> {
    conn.query_all_raw(Statement::from_string(
        conn.get_database_backend(),
        "SELECT type || ':' || name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' \
         UNION ALL \
         SELECT 'column:' || m.name || '.' || p.name \
         FROM sqlite_master AS m, pragma_table_info(m.name) AS p WHERE m.type = 'table' \
         ORDER BY 1",
    ))
    .await
    .expect("read physical repository schema")
    .into_iter()
    .map(|row| {
        row.try_get_by_index::<String>(0)
            .expect("decode schema object")
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

/// Physical schema of a freshly initialized (current) repository and of the
/// same repository after its newest migration was rolled back.
struct StaleSchemaFixture {
    current: Vec<String>,
    stale: Vec<String>,
}

/// A freshly initialized repository whose newest built-in migration has been
/// rolled back — effect and ledger receipt — exactly as a repository last
/// migrated by the previous Libra release looks.
async fn stale_hook_repo() -> (HookRepo, StaleSchemaFixture) {
    let repo = HookRepo::init();
    let migrations = builtin_migrations();
    let newest = migrations.last().expect("at least one built-in migration");
    assert!(
        newest.down.is_some(),
        "the stale-schema fixture rolls back the newest built-in migration ({}); \
         give it a down migration or roll back to an older reversible tip",
        newest.name
    );
    let previous = migrations
        .iter()
        .rev()
        .nth(1)
        .map(|migration| migration.version)
        .expect("a previous built-in migration");
    let conn = repo.db().await;
    assert_eq!(
        max_repository_schema_receipt(&conn).await,
        Some(latest_repository_schema_version()),
        "fixture precondition: init writes the current schema"
    );
    let current = physical_repository_schema(&conn).await;
    builtin_runner()
        .expect("built-in migration registry")
        .rollback_to(&conn, previous)
        .await
        .expect("roll the newest built-in migration back");
    assert_eq!(
        max_repository_schema_receipt(&conn).await,
        Some(previous),
        "fixture precondition: the newest receipt is gone"
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
hooks-claude PermissionRequest permission-request exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[]:receipts=3(pending=0)[]] artifacts=0
hooks-claude ModelUpdate model-update exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[]:receipts=4(pending=0)[]] artifacts=0
hooks-claude Compaction compaction exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:condensed:stopped_at=none:checkpoints=[]:receipts=5(pending=0)[]] artifacts=0
hooks-claude Stop stop exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[committed]:receipts=6(pending=0)[committed/active/complete]] artifacts=0
hooks-claude SubagentStart subagent-start exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[committed]:receipts=6(pending=0)[committed/active/complete]] artifacts=0
hooks-claude SubagentStop subagent-end exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:active:stopped_at=none:checkpoints=[committed]:receipts=6(pending=0)[committed/active/complete]] artifacts=0
hooks-claude SessionEnd session-end exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-hooks-claude:claude_code:stopped:stopped_at=set:checkpoints=[committed,committed]:receipts=7(pending=0)[committed/active/complete,committed/stopped/complete]] artifacts=0
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
agent-hooks-claude-code PermissionRequest permission-request exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[]:receipts=3(pending=0)[]] artifacts=0
agent-hooks-claude-code ModelUpdate model-update exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[]:receipts=4(pending=0)[]] artifacts=0
agent-hooks-claude-code Compaction compaction exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:condensed:stopped_at=none:checkpoints=[]:receipts=5(pending=0)[]] artifacts=0
agent-hooks-claude-code Stop stop exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[committed]:receipts=6(pending=0)[committed/active/complete]] artifacts=0
agent-hooks-claude-code SubagentStart subagent-start exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[committed]:receipts=6(pending=0)[committed/active/complete]] artifacts=0
agent-hooks-claude-code SubagentStop subagent-end exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:active:stopped_at=none:checkpoints=[committed]:receipts=6(pending=0)[committed/active/complete]] artifacts=0
agent-hooks-claude-code SessionEnd session-end exit=Some(0) stdout="" stderr="" | sessions=[claude__contract-agent-hooks-claude-code:claude_code:stopped:stopped_at=set:checkpoints=[committed,committed]:receipts=7(pending=0)[committed/active/complete,committed/stopped/complete]] artifacts=0
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
agent-hooks-gemini SessionEnd session-end exit=Some(128) stdout="" stderr="fatal: gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\nError-Code: LBR-INTERNAL-001\n\nHint: remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\n{\"ok\":false,\"error_code\":\"LBR-INTERNAL-001\",\"category\":\"internal\",\"exit_code\":128,\"severity\":\"fatal\",\"message\":\"gemini hook ingestion is disabled: gemini is uninstall-only (not in the supported agent roster)\",\"hints\":[\"remove the stale hook config with 'libra agent remove gemini'; previously captured gemini sessions stay readable\"]}\n" | sessions=[] artifacts=0"##;

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
                    json!({ "event_id": format!("{session}-{verb}") }),
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
