//! 强制补强项 #10 crash regression for the AG-19 hook ingest path
//! (`src/internal/ai/hooks/runtime.rs::ingest_agent_traces_payload`).
//!
//! Drives the built `libra` binary end-to-end (mirroring the harness in
//! `tests/agent_lifecycle_event_test.rs`) and proves that a hook handler
//! dying mid-flight — SIGKILL before/while reading stdin or SIGKILL racing a
//! `stop` ingest — never leaves partial `agent_session` / `agent_checkpoint`
//! state visible through the CLI JSON surfaces, and never echoes raw stdin
//! bytes to stderr.

#![cfg(unix)]

use std::{
    io::Write,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

use libra::internal::ai::observed_agents::claude_project_slug;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
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
        // Hook ingress canonicalizes cwd before resolving the native Claude
        // source. Match that identity in this fixture so `/var` and
        // `/private/var` cannot produce different provider project slugs.
        let this = Self {
            _tempdir: tempdir,
            repo: repo.canonicalize().expect("canonical repo dir"),
            home: home.canonicalize().expect("canonical fake home"),
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

    fn traces_head(&self) -> String {
        // The managed traces writer uses the locked internal `traces` branch
        // in Libra's reference table. `refs/libra/traces` is its transport
        // spelling; the CLI resolves the local branch spelling here.
        let out = self.run(&["rev-parse", "traces"], None, &[]);
        assert!(
            out.status.success(),
            "resolve traces ref: {}",
            describe(&out)
        );
        String::from_utf8(out.stdout)
            .expect("traces ref output is utf-8")
            .trim()
            .to_string()
    }

    /// `None` until a checkpoint has published the traces branch.
    fn traces_head_if_present(&self) -> Option<String> {
        let out = self.run(&["rev-parse", "traces"], None, &[]);
        out.status.success().then(|| {
            String::from_utf8(out.stdout)
                .expect("traces ref output is utf-8")
                .trim()
                .to_string()
        })
    }

    fn db_path(&self) -> PathBuf {
        self.repo.join(".libra").join("libra.db")
    }

    /// Put a coverage-v1-parseable Claude source at the provider-derived
    /// location.  The hook ignores a caller-supplied source pointer, so this
    /// is the only path that can exercise the real authorized snapshot and
    /// coverage/store transaction together.
    fn write_claude_transcript(&self, session_id: &str, content: &str) {
        let dir = self
            .home
            .join(".claude")
            .join("projects")
            .join(claude_project_slug(&self.repo));
        std::fs::create_dir_all(&dir).expect("create Claude provider transcript directory");
        std::fs::write(dir.join(format!("{session_id}.jsonl")), content)
            .expect("write Claude provider transcript");
    }

    async fn db(&self) -> DatabaseConnection {
        let url = format!("sqlite://{}?mode=ro", self.db_path().display());
        Database::connect(url).await.expect("open libra catalog")
    }

    async fn query_rows(&self, sql: &str) -> Vec<sea_orm::QueryResult> {
        let conn = self.db().await;
        conn.query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("query catalog")
    }

    /// Every retained artifact header/chunk row, byte-exact, in stable order.
    async fn artifact_rows(&self) -> Vec<(String, String, String, String)> {
        self.query_rows(
            "SELECT scope, target, key, hex(CAST(value AS BLOB)) AS payload \
             FROM metadata_kv WHERE scope IN \
             ('agent_capture_pending', 'agent_capture_pending_chunk', 'agent_capture_quarantine') \
             ORDER BY scope, target, key",
        )
        .await
        .into_iter()
        .map(|row| {
            (
                row.try_get_by::<String, _>("scope")
                    .expect("decode artifact scope"),
                row.try_get_by::<String, _>("target")
                    .expect("decode artifact target"),
                row.try_get_by::<String, _>("key")
                    .expect("decode artifact key"),
                row.try_get_by::<String, _>("payload")
                    .expect("decode artifact payload canary"),
            )
        })
        .collect()
    }

    /// Ownership of every coverage claim: (turn, state, owner, fence,
    /// revision, lease). A takeover advances owner/fence; replay commits.
    async fn coverage_claims(
        &self,
    ) -> Vec<(String, String, Option<String>, i64, i64, Option<i64>)> {
        self.query_rows(
            "SELECT logical_turn_key, state, owner, fence_token, revision, lease_expires_at \
             FROM agent_coverage_claim ORDER BY session_id, logical_turn_key",
        )
        .await
        .into_iter()
        .map(|row| {
            (
                row.try_get_by::<String, _>("logical_turn_key")
                    .expect("decode claim turn"),
                row.try_get_by::<String, _>("state")
                    .expect("decode claim state"),
                row.try_get_by::<Option<String>, _>("owner")
                    .expect("decode claim owner"),
                row.try_get_by::<i64, _>("fence_token")
                    .expect("decode claim fence"),
                row.try_get_by::<i64, _>("revision")
                    .expect("decode claim revision"),
                row.try_get_by::<Option<i64>, _>("lease_expires_at")
                    .expect("decode claim lease"),
            )
        })
        .collect()
    }

    /// Durable lifecycle evidence a native redelivery must never revise:
    /// (state, stopped_at, sync_revision, metadata_json).
    async fn session_ledger(&self, provider_session: &str) -> (String, Option<i64>, i64, String) {
        let conn = self.db().await;
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT state, stopped_at, sync_revision, metadata_json \
                 FROM agent_session WHERE provider_session_id = ?",
                [provider_session.into()],
            ))
            .await
            .expect("query session ledger")
            .expect("session ledger row");
        (
            row.try_get_by("state").expect("decode session state"),
            row.try_get_by("stopped_at").expect("decode stopped_at"),
            row.try_get_by("sync_revision").expect("decode revision"),
            row.try_get_by("metadata_json").expect("decode metadata"),
        )
    }

    /// Every checkpoint-bearing capture receipt of one provider session as
    /// `(checkpoint class, intent phase, status)`; opaque keys are omitted.
    async fn checkpoint_receipts(&self, provider_session: &str) -> Vec<(String, String, String)> {
        let (_, _, _, metadata_json) = self.session_ledger(provider_session).await;
        let metadata: Value =
            serde_json::from_str(&metadata_json).expect("capture receipt metadata is JSON");
        metadata["capture_catalog_receipts_v1"]["entries"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry["intent"]["checkpoint"] != json!("none"))
                    .map(|entry| {
                        let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
                        (
                            text(&entry["intent"]["checkpoint"]),
                            text(&entry["intent"]["phase"]),
                            text(&entry["status"]),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn inflight_marker_count(&self) -> usize {
        self.query_rows("SELECT `key` FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await
            .len()
    }

    /// Decoded writer markers keyed by one checkpoint (attempt) id.
    async fn inflight_markers_for(&self, checkpoint_id: &str) -> Vec<Value> {
        let conn = self.db().await;
        conn.query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT value FROM metadata_kv WHERE scope = 'agent_traces_inflight' AND `key` = ?",
            [checkpoint_id.into()],
        ))
        .await
        .expect("query checkpoint writer markers")
        .into_iter()
        .map(|row| {
            let value: String = row.try_get_by("value").expect("decode marker value");
            serde_json::from_str(&value).expect("writer marker is JSON")
        })
        .collect()
    }

    /// Rewrite one historical field of the single pending terminal finalizer
    /// created by the real runtime, preserving every identity/fence field.
    /// This models elapsed wall-clock time or spent automatic attempts.
    async fn rewrite_pending_terminal_finalizer(
        &self,
        provider_session: &str,
        field: &str,
        value: Value,
    ) {
        let (_, _, _, metadata_json) = self.session_ledger(provider_session).await;
        let mut metadata: Value =
            serde_json::from_str(&metadata_json).expect("terminal receipt metadata is JSON");
        let finalizer = metadata["capture_catalog_receipts_v1"]["entries"]
            .as_array_mut()
            .and_then(|entries| {
                entries
                    .iter_mut()
                    .find(|entry| entry["intent"]["phase"] == json!("stopped"))
            })
            .and_then(|entry| entry.get_mut("finalizer"))
            .and_then(Value::as_object_mut)
            .expect("blocked terminal persisted one pending finalizer");
        finalizer.insert(field.to_string(), value);
        let conn = Database::connect(format!("sqlite://{}", self.db_path().display()))
            .await
            .expect("open writable catalog for finalizer aging");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_session SET metadata_json = ? WHERE provider_session_id = ?",
            [
                serde_json::to_string(&metadata)
                    .expect("serialize aged terminal receipt metadata")
                    .into(),
                provider_session.into(),
            ],
        ))
        .await
        .expect("persist aged terminal receipt metadata");
    }

    async fn execute_sql(&self, sql: &str) {
        let conn = Database::connect(format!("sqlite://{}", self.db_path().display()))
            .await
            .expect("open writable catalog for fault injection");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("execute catalog fault-injection statement");
    }

    async fn seed_unrelated_sessions_before(&self, anchor_session: &str, count: usize) {
        let conn = Database::connect(format!("sqlite://{}", self.db_path().display()))
            .await
            .expect("open writable catalog for bounded worker regression");
        let anchor = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT working_dir, worktree_id, repo_id, workspace_id, workspace_fence \
                 FROM agent_session WHERE session_id = ? LIMIT 1",
                [anchor_session.into()],
            ))
            .await
            .expect("query anchor session")
            .expect("anchor session exists");
        let working_dir: String = anchor
            .try_get_by("working_dir")
            .expect("decode working dir");
        let worktree_id: String = anchor.try_get_by("worktree_id").expect("decode worktree");
        let repo_id: String = anchor.try_get_by("repo_id").expect("decode repository");
        let workspace_id: Option<String> =
            anchor.try_get_by("workspace_id").expect("decode workspace");
        let workspace_fence: Option<i64> = anchor
            .try_get_by("workspace_fence")
            .expect("decode workspace fence");
        for index in 0..count {
            let session_id = format!("a-worker-scan-noise-{index:04}");
            let provider_id = format!("worker-scan-noise-{index:04}");
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, \
                 working_dir, worktree_id, metadata_json, redaction_report, started_at, last_event_at, \
                 sync_revision, repo_id, workspace_id, workspace_fence, scope_state) \
                 VALUES (?, 'claude_code', ?, 'active', ?, ?, '{}', '{}', 1, 1, 1, ?, ?, ?, 'scoped')",
                [
                    session_id.into(),
                    provider_id.into(),
                    working_dir.clone().into(),
                    worktree_id.clone().into(),
                    repo_id.clone().into(),
                    workspace_id.clone().into(),
                    workspace_fence.into(),
                ],
            ))
            .await
            .expect("insert bounded unrelated session row");
        }
    }

    async fn seed_unresolvable_pending_headers(&self, count: usize) {
        let conn = Database::connect(format!("sqlite://{}", self.db_path().display()))
            .await
            .expect("open writable catalog for queue starvation regression");
        let existing = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT target, value FROM metadata_kv WHERE scope = 'agent_capture_pending' LIMIT 1",
                Vec::<sea_orm::Value>::new(),
            ))
            .await
            .expect("query durable pending header")
            .expect("pending header exists");
        let target: String = existing.try_get_by("target").expect("decode repo id");
        let encoded: String = existing.try_get_by("value").expect("decode pending header");
        let mut header: Value = serde_json::from_str(&encoded).expect("parse pending header");
        for index in 0..count {
            let checkpoint_id = format!("00000000-0000-4000-8000-{index:012}");
            header["binding"]["checkpoint_id"] = json!(checkpoint_id);
            let value = serde_json::to_string(&header).expect("encode bounded pending header");
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO metadata_kv (scope, target, key, value, value_type, created_at, updated_at) \
                 VALUES ('agent_capture_pending', ?, ?, ?, 'text', 1, 1)",
                [target.clone().into(), checkpoint_id.into(), value.into()],
            ))
            .await
            .expect("insert unresolvable pending header ahead of valid artifact");
        }
    }

    async fn corrupt_earliest_pending_header_mac(&self) {
        let conn = Database::connect(format!("sqlite://{}", self.db_path().display()))
            .await
            .expect("open writable catalog for envelope authentication regression");
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT target, key, value FROM metadata_kv WHERE scope = 'agent_capture_pending' ORDER BY key LIMIT 1".to_string(),
            ))
            .await
            .expect("query earliest pending header")
            .expect("pending header exists");
        let target: String = row.try_get_by("target").expect("decode repo id");
        let key: String = row.try_get_by("key").expect("decode checkpoint key");
        let encoded: String = row.try_get_by("value").expect("decode pending header");
        let mut header: Value = serde_json::from_str(&encoded).expect("parse pending header");
        header["mac"] = json!(format!("pending-envelope/hmac-v1/{}", "0".repeat(64)));
        let encoded = serde_json::to_string(&header).expect("encode tampered header");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE metadata_kv SET value = ?, updated_at = updated_at WHERE scope = 'agent_capture_pending' AND target = ? AND key = ?",
            [encoded.into(), target.into(), key.into()],
        ))
        .await
        .expect("tamper only the pending envelope MAC");
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

    /// ACF-17 per-provider durable oracle of one completed native replay.
    /// Asserts the catalog row carries the provider-prefixed session id, then
    /// returns `(agent_kind, transcript_snapshot.partial_reason, claim
    /// channels)` read from the catalog, the checkpoint's `metadata.json`
    /// blob, and `agent_coverage_claim.source_channel`.
    async fn replay_durable_oracle(
        &self,
        provider_session: &str,
        provider_prefix: &str,
        checkpoint_id: &str,
    ) -> (String, Option<String>, Vec<String>) {
        let conn = self.db().await;
        let backend = conn.get_database_backend();
        let session = conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT session_id, agent_kind FROM agent_session WHERE provider_session_id = ?",
                [provider_session.into()],
            ))
            .await
            .expect("query replay catalog identity")
            .expect("replay catalog identity row");
        let session_id: String = session.try_get_by("session_id").expect("decode session id");
        assert_eq!(
            session_id,
            format!("{provider_prefix}__{provider_session}"),
            "the catalog session id keeps its provider prefix"
        );
        let metadata_oid: String = conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT metadata_blob_oid FROM agent_checkpoint WHERE checkpoint_id = ?",
                [checkpoint_id.into()],
            ))
            .await
            .expect("query replay checkpoint metadata blob")
            .expect("replay checkpoint row")
            .try_get_by("metadata_blob_oid")
            .expect("decode metadata blob oid");
        let blob = self.run(&["cat-file", "-p", &metadata_oid], None, &[]);
        assert!(
            blob.status.success(),
            "read checkpoint metadata.json: {}",
            describe(&blob)
        );
        let metadata: Value =
            serde_json::from_slice(&blob.stdout).expect("checkpoint metadata.json is JSON");
        let channels = conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT source_channel FROM agent_coverage_claim \
                 WHERE session_id = ? ORDER BY logical_turn_key",
                [session_id.into()],
            ))
            .await
            .expect("query replay coverage claim channels")
            .into_iter()
            .map(|row| {
                row.try_get_by::<String, _>("source_channel")
                    .expect("decode claim channel")
            })
            .collect();
        (
            session
                .try_get_by::<String, _>("agent_kind")
                .expect("decode agent kind"),
            metadata["transcript_snapshot"]["partial_reason"]
                .as_str()
                .map(str::to_string),
            channels,
        )
    }

    /// Whether some other process currently owns the repository's detached
    /// capture-recovery worker lock. A successful probe is dropped at once, so
    /// it never displaces a worker still inside its lock-handoff retry.
    fn capture_worker_lock_owned(&self) -> bool {
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(
                self.repo
                    .join(".libra")
                    .join("private")
                    .join("agent-capture-recovery-worker.lock"),
            )
            .expect("open capture recovery worker lock");
        match lock.try_lock() {
            Ok(()) => false,
            Err(std::fs::TryLockError::WouldBlock) => true,
            Err(std::fs::TryLockError::Error(error)) => {
                panic!("probe capture recovery worker lock: {error}")
            }
        }
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

/// Minimal normalizable Claude transcript used by the concrete store-fault
/// tests below.  It deliberately has a stable logical turn (`u1`) so the
/// coverage claim, ref-CAS extension, and catalog row are one transaction.
const TURN_COMPLETE: &str = concat!(
    r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"run it"}}"#,
    "\n",
    r#"{"type":"assistant","uuid":"a1","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
    "\n",
);

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

/// Force the concrete checkpoint writer's object-directory creation to fail
/// after it has registered its marker, without teaching the shipped binary a
/// test environment knob. The isolated repository lets the test restore the
/// original object store before exercising native replay.
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

/// Redeliver a native SessionEnd under the managed capture budget and return
/// its content-free `agent.capture.recovery` diagnostics, which name the
/// stage that stopped it (the CLI deliberately renders a fixed message).
fn redeliver_terminal_with_recovery_log(
    repo: &HookRepo,
    terminal: &str,
    label: &str,
) -> (Output, String) {
    let log = repo.home.join(format!("redelivery-{label}.log"));
    let log_path = log.to_string_lossy().to_string();
    let out = repo.run(
        &[
            "agent",
            "hooks",
            "--capture-budget-ms",
            "60000",
            "claude-code",
            "session-end",
        ],
        Some(terminal),
        &[
            ("LIBRA_LOG", "agent.capture.recovery=warn"),
            ("LIBRA_LOG_FILE", log_path.as_str()),
        ],
    );
    let diagnostics = std::fs::read_to_string(&log).unwrap_or_default();
    (out, diagnostics)
}

fn restore_checkpoint_object_directory(repo: &HookRepo, backup: Option<PathBuf>) {
    let objects = repo.repo.join(".libra").join("objects");
    std::fs::remove_file(&objects).expect("remove object-directory blocker");
    if let Some(backup) = backup {
        std::fs::rename(backup, objects).expect("restore object directory");
    }
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

/// A hook manager may redirect its JSON envelope from a regular file instead
/// of a pipe. Linux epoll refuses to register such an fd (`EPERM`), so this
/// exercises the managed-deadline regular-file fallback through the actual
/// installed top-level hook command rather than only its in-process reader.
#[test]
fn managed_hook_accepts_regular_file_stdin() {
    let repo = HookRepo::init();
    let session = "sess-regular-file-stdin";
    let input = repo.repo.join("regular-hook-input.json");
    std::fs::write(
        &input,
        repo.envelope(
            "SessionStart",
            session,
            json!({ "event_id": "regular-file-v1" }),
        ),
    )
    .expect("write regular hook input");

    let mut command = repo.command(
        &[
            "hooks",
            "claude",
            "session-start",
            "--capture-budget-ms",
            "1000",
        ],
        &[],
    );
    command.stdin(Stdio::from(
        std::fs::File::open(&input).expect("open regular hook input"),
    ));
    let output = command.output().expect("run regular-file hook command");
    assert!(
        output.status.success(),
        "regular-file managed hook must succeed: {}",
        describe(&output)
    );
    assert!(
        repo.sessions()
            .iter()
            .any(|row| row["session_id"] == json!(format!("claude__{session}"))),
        "regular-file hook must reach durable capture"
    );
}

/// SIGKILL's signal number without pulling in the libc crate.
fn libc_sigkill() -> i32 {
    9
}

/// Codex runs its Stop hook in the foreground. A real object-store failure
/// occurs after marker registration, but capture remains advisory: the host
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
    let blocker = block_checkpoint_object_directory(&repo);
    let stop = repo.run(
        &["hooks", "codex", "stop"],
        Some(&repo.envelope("Stop", session, json!({ "last_assistant_message": marker }))),
        &[],
    );
    restore_checkpoint_object_directory(&repo, blocker);

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
    // retries the same full-session capture after the physical failure is
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

/// The external hook surface must preserve a stable native replay identity:
/// repeating the same Stop passes through the concrete
/// `TracesCheckpointStore` replay probe without appending a second trace
/// commit or advancing the traces ref. The deliberate crash window is
/// covered by the in-process coordinator/runtime test, where its typed seam
/// cannot be reached by a production hook environment.
#[test]
fn checkpoint_store_replay_does_not_advance_traces_ref_twice() {
    let repo = HookRepo::init();
    let session = "sess-store-stable-replay";
    let start = repo.run(
        &["hooks", "claude", "session-start"],
        Some(&repo.envelope("SessionStart", session, json!({}))),
        &[],
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );
    let stop_envelope = repo.envelope(
        "Stop",
        session,
        json!({
            "prompt": "stable checkpoint replay fixture",
            // A native callback identity distinguishes a retry from a later
            // valid Stop in the same provider session.
            "turn_id": "turn-store-stable-replay",
        }),
    );
    let first = repo.run(&["hooks", "claude", "stop"], Some(&stop_envelope), &[]);
    assert!(
        first.status.success(),
        "initial checkpoint hook must succeed: {}",
        describe(&first)
    );
    let first_rows = repo.checkpoints();
    assert_eq!(
        first_rows.len(),
        1,
        "the initial hook must durably publish one checkpoint: {}",
        describe(&first)
    );
    let first_commit = first_rows[0]["traces_commit"]
        .as_str()
        .expect("checkpoint row has traces_commit")
        .to_string();
    let first_head = repo.traces_head();
    assert_eq!(
        first_head, first_commit,
        "traces ref must name the checkpoint commit"
    );

    let retry = repo.run(&["hooks", "claude", "stop"], Some(&stop_envelope), &[]);
    assert!(retry.status.success(), "replay hook: {}", describe(&retry));
    let replay_rows = repo.checkpoints();
    assert_eq!(
        replay_rows.len(),
        1,
        "stable replay must not create a second checkpoint: {}",
        describe(&retry)
    );
    assert_eq!(
        replay_rows[0]["traces_commit"].as_str(),
        Some(first_commit.as_str()),
        "stable replay must retain the original durable checkpoint commit"
    );
    assert_eq!(
        repo.traces_head(),
        first_head,
        "stable replay must not advance refs/libra/traces"
    );
}

/// A successful ordinary marker cleanup must leave the native replay stable:
/// the identical Stop is an acknowledgement, not a second ref write. Unlike
/// `checkpoint_store_replay_does_not_advance_traces_ref_twice`, this pins the
/// cleanup itself: no writer marker survives either delivery. The typed
/// cleanup-failure branch is covered in the in-process runtime test, where no
/// hook environment can inject it.
#[tokio::test]
async fn checkpoint_store_replay_after_successful_cleanup_does_not_append() {
    let repo = HookRepo::init();
    let session = "sess-store-successful-cleanup";
    let start = repo.run(
        &["hooks", "claude", "session-start"],
        Some(&repo.envelope("SessionStart", session, json!({}))),
        &[],
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );

    let stop_envelope = repo.envelope(
        "Stop",
        session,
        json!({
            "prompt": "successful marker cleanup replay fixture",
            "turn_id": "turn-store-successful-cleanup",
        }),
    );
    let first = repo.run(&["hooks", "claude", "stop"], Some(&stop_envelope), &[]);
    assert!(
        first.status.success(),
        "initial marker cleanup must succeed: {}",
        describe(&first)
    );
    let first_rows = repo.checkpoints();
    assert_eq!(
        first_rows.len(),
        1,
        "the initial ref/catalog transaction is durable"
    );
    let first_head = repo.traces_head();
    assert!(
        repo.query_rows("SELECT `key` FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await
            .is_empty(),
        "a successful checkpoint write must retire its ordinary writer marker"
    );

    let retry = repo.run(&["hooks", "claude", "stop"], Some(&stop_envelope), &[]);
    assert!(
        retry.status.success(),
        "the matching replay must settle ordinary marker cleanup: {}",
        describe(&retry)
    );
    assert_eq!(
        repo.checkpoints().len(),
        1,
        "cleanup replay must not append"
    );
    assert_eq!(
        repo.traces_head(),
        first_head,
        "cleanup replay must retain the originally committed traces ref"
    );
    assert!(
        repo.query_rows("SELECT `key` FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await
            .is_empty(),
        "an acknowledged replay must not register a new writer marker"
    );
}

#[test]
fn capture_recovery_worker_rejects_payload_arguments_before_repository_access() {
    let repo = HookRepo::init();
    let before = std::fs::read(repo.db_path()).expect("snapshot repository database");
    let rejected = repo.run(
        &["__capture-recovery-worker", "unexpected-payload"],
        None,
        &[],
    );
    assert!(
        !rejected.status.success(),
        "the internal worker must reject every caller-provided argument"
    );
    // The raw-argv gate, not clap's unknown-subcommand path, must reject it,
    // and its fixed message must not echo the caller's payload.
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(
        stderr.contains("capture recovery worker does not accept arguments"),
        "the hidden worker gate must reject extra argv: {}",
        describe(&rejected)
    );
    assert!(
        !stderr.contains("unexpected-payload"),
        "the rejection must stay content-free: {}",
        describe(&rejected)
    );
    assert_eq!(
        std::fs::read(repo.db_path()).expect("read repository database after rejection"),
        before,
        "invalid worker argv must be rejected before opening or mutating repository state"
    );
}

/// A terminal native replay remains an acknowledgement after strict
/// completion: it must neither reopen the session nor append a second
/// checkpoint. The deliberate marker-cleanup and post-write interruption
/// paths run in the library test build through typed seams, rather than
/// accepting an environment-controlled production hook fault.
///
/// ACF-13 VER3: when the terminal's complete snapshot is instead retained as
/// a recovery artifact, a native redelivery after its receipt window expired
/// or its automatic attempts were spent must leave the original session
/// state, revision, receipt, artifact, and claims unchanged; the untouched
/// artifact then still completes the original receipt via doctor repair.
#[tokio::test]
async fn terminal_native_replay_is_complete_and_idempotent() {
    let repo = HookRepo::init();
    let session = "sess-terminal-native-replay";
    let start = repo.run(
        &["hooks", "claude", "session-start"],
        Some(&repo.envelope("SessionStart", session, json!({}))),
        &[],
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );

    let terminal = repo.envelope(
        "SessionEnd",
        session,
        json!({"event_id": "terminal-native-replay-v1"}),
    );
    let first = repo.run(&["hooks", "claude", "session-end"], Some(&terminal), &[]);
    assert!(
        first.status.success(),
        "initial terminal delivery: {}",
        describe(&first)
    );
    let before = repo.session_show(&format!("claude__{session}"));
    assert_eq!(before["state"], json!("stopped"));
    assert_eq!(repo.checkpoints().len(), 1);

    let replay = repo.run(&["hooks", "claude", "session-end"], Some(&terminal), &[]);
    assert!(
        replay.status.success(),
        "completed terminal replay: {}",
        describe(&replay)
    );
    assert_eq!(
        repo.session_show(&format!("claude__{session}")),
        before,
        "native terminal replay must not reopen or revise the settled row"
    );
    assert_eq!(repo.checkpoints().len(), 1);

    let retained = "sess-terminal-native-retained";
    repo.write_claude_transcript(retained, TURN_COMPLETE);
    let start = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&repo.envelope("SessionStart", retained, json!({}))),
        &[],
    );
    assert!(
        start.status.success(),
        "retained session start: {}",
        describe(&start)
    );
    let retained_terminal = repo.envelope(
        "SessionEnd",
        retained,
        json!({"event_id": "terminal-native-retained-v1"}),
    );
    let blocker = block_checkpoint_object_directory(&repo);
    let blocked = repo.run(
        &[
            "agent",
            "hooks",
            "--capture-budget-ms",
            "60000",
            "claude-code",
            "session-end",
        ],
        Some(&retained_terminal),
        &[],
    );
    restore_checkpoint_object_directory(&repo, blocker);
    assert!(
        !blocked.status.success(),
        "blocked publication leaves the retained terminal pending: {}",
        describe(&blocked)
    );
    let artifact = repo.artifact_rows().await;
    assert!(
        artifact
            .iter()
            .any(|(scope, ..)| scope == "agent_capture_pending"),
        "the complete terminal snapshot must be retained: {}",
        describe(&blocked)
    );
    // Every redelivery below arrives after the ordinary claim lease.
    repo.execute_sql(
        "UPDATE agent_coverage_claim SET lease_expires_at = 0 WHERE state = 'reserved_live'",
    )
    .await;
    let claims = repo.coverage_claims().await;
    assert!(
        !claims.is_empty(),
        "the retained terminal reserved coverage"
    );
    let (_, _, _, original_metadata) = repo.session_ledger(retained).await;
    let original_metadata: Value =
        serde_json::from_str(&original_metadata).expect("terminal receipt metadata is JSON");
    let first_attempt_millis = original_metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["intent"]["phase"] == json!("stopped"))
        })
        .and_then(|entry| entry["finalizer"]["first_attempt_millis"].as_i64())
        .expect("pending terminal finalizer records its first attempt");
    let published = repo.checkpoints().len();

    for (label, first_attempt, attempts) in [
        // Past the original 30-minute automatic replay window.
        (
            "window-expired",
            first_attempt_millis - 31 * 60 * 1000,
            None,
        ),
        // Inside the window, but every automatic attempt already spent.
        ("attempt-capped", first_attempt_millis, Some(5)),
    ] {
        repo.rewrite_pending_terminal_finalizer(
            retained,
            "first_attempt_millis",
            json!(first_attempt),
        )
        .await;
        if let Some(attempts) = attempts {
            repo.rewrite_pending_terminal_finalizer(retained, "attempts", json!(attempts))
                .await;
        }
        let ledger = repo.session_ledger(retained).await;
        assert_eq!(ledger.0, "active", "{label}: terminal is still pending");
        let (out, stage) = redeliver_terminal_with_recovery_log(&repo, &retained_terminal, label);
        assert!(
            !out.status.success() && stage.contains("terminal_artifact_retained"),
            "{label}: redelivery must stop at the retained artifact: {}\n{stage}",
            describe(&out)
        );
        assert_eq!(
            repo.session_ledger(retained).await,
            ledger,
            "{label}: native redelivery must not change the original state, revision, or receipt"
        );
        assert_eq!(
            repo.artifact_rows().await,
            artifact,
            "{label}: native redelivery must not move or rewrite the retained artifact"
        );
        assert_eq!(
            repo.coverage_claims().await,
            claims,
            "{label}: native redelivery must not take over artifact-owned claims"
        );
        assert_eq!(repo.checkpoints().len(), published);
    }

    let repaired = repo.run(&["agent", "doctor", "--repair", "--json"], None, &[]);
    assert!(
        repaired.status.success(),
        "the untouched artifact must stay repairable: {}",
        describe(&repaired)
    );
    assert_eq!(
        repo.session_ledger(retained).await.0,
        "stopped",
        "doctor repair completes the original retained receipt: {}",
        describe(&repaired)
    );
    assert_eq!(repo.checkpoints().len(), published + 1);
    assert!(
        repo.artifact_rows().await.is_empty(),
        "successful repair atomically removes the retained artifact"
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

/// ACF-05 pre-CAS failure: the checkpoint store fails creating its object
/// directory after marker registration, before any object, ref CAS, or
/// companion transaction runs. It must leave neither a checkpoint row nor a
/// traces ref, retire its marker, release the coverage lease, and let the
/// exact native replay commit once. The third delivery pins that replay goes
/// through the stable durable result rather than reopening a second
/// transaction. The in-transaction rollback is covered by
/// `checkpoint_store_cas_and_catalog_are_atomic`.
#[tokio::test]
async fn checkpoint_store_pre_cas_object_failure_is_replayable() {
    let repo = HookRepo::init();
    let session = "sess-pre-cas-object-failure";
    repo.write_claude_transcript(session, TURN_COMPLETE);
    let start = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&repo.envelope("SessionStart", session, json!({}))),
        &[],
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );

    let turn_end = repo.envelope(
        "Stop",
        session,
        json!({ "turn_id": "turn-pre-cas-object-failure-v1" }),
    );
    let blocker = block_checkpoint_object_directory(&repo);
    let failed = repo.run(
        &["agent", "hooks", "claude-code", "stop"],
        Some(&turn_end),
        &[],
    );
    restore_checkpoint_object_directory(&repo, blocker);
    assert!(
        !failed.status.success(),
        "post-registration object-store failure must fail the hook: {}",
        describe(&failed)
    );
    assert!(
        repo.checkpoints().is_empty(),
        "a failed pre-CAS write must not leave a catalog checkpoint row"
    );
    let traces_after_failure = repo.run(&["rev-parse", "traces"], None, &[]);
    assert!(
        !traces_after_failure.status.success(),
        "a failed pre-CAS write must not publish refs/libra/traces: {}",
        describe(&traces_after_failure)
    );
    let claims = repo
        .query_rows(
            "SELECT state, owner, lease_expires_at FROM agent_coverage_claim \
             WHERE logical_turn_key = 'u1'",
        )
        .await;
    assert_eq!(
        claims.len(),
        1,
        "the attempted turn must retain one claim row"
    );
    assert_eq!(
        claims[0].try_get_by::<String, _>("state").unwrap(),
        "abandoned",
        "the failed transaction must release its coverage claim"
    );
    assert!(
        claims[0]
            .try_get_by::<Option<String>, _>("owner")
            .unwrap()
            .is_none(),
        "the released claim must not retain a live owner"
    );
    assert!(
        claims[0]
            .try_get_by::<Option<i64>, _>("lease_expires_at")
            .unwrap()
            .is_none(),
        "the released claim must not retain a live lease"
    );
    assert!(
        repo.query_rows("SELECT `key` FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await
            .is_empty(),
        "pre-CAS failure must remove its inflight marker"
    );

    let replay = repo.run(
        &["agent", "hooks", "claude-code", "stop"],
        Some(&turn_end),
        &[],
    );
    assert!(
        replay.status.success(),
        "the exact native replay must retry the abandoned transaction: {}",
        describe(&replay)
    );
    let committed = repo.checkpoints();
    assert_eq!(
        committed.len(),
        1,
        "the successful replay must publish one checkpoint: {committed:?}"
    );
    let commit = committed[0]["traces_commit"]
        .as_str()
        .expect("checkpoint has traces commit")
        .to_string();
    assert_eq!(
        repo.traces_head(),
        commit,
        "the published ref must agree with the catalog checkpoint"
    );
    let committed_claims = repo
        .query_rows("SELECT state FROM agent_coverage_claim WHERE logical_turn_key = 'u1'")
        .await;
    assert_eq!(
        committed_claims[0]
            .try_get_by::<String, _>("state")
            .unwrap(),
        "catalog_committed",
        "the successful ref/catalog transaction commits its coverage claim"
    );

    let duplicate = repo.run(
        &["agent", "hooks", "claude-code", "stop"],
        Some(&turn_end),
        &[],
    );
    assert!(
        duplicate.status.success(),
        "completed native replay must acknowledge: {}",
        describe(&duplicate)
    );
    let after_duplicate = repo.checkpoints();
    assert_eq!(
        after_duplicate.len(),
        1,
        "completed replay must not create a second catalog row"
    );
    assert_eq!(
        after_duplicate[0]["traces_commit"].as_str(),
        Some(commit.as_str()),
        "completed replay must retain the original durable commit"
    );
    assert_eq!(
        repo.traces_head(),
        commit,
        "completed replay must not advance refs/libra/traces"
    );
}

/// ACF-05 AC3: the traces ref CAS and its catalog/coverage companion are one
/// SQLite transaction. A trigger aborts the companion's `agent_checkpoint`
/// insert after every checkpoint object was written and the ref row was
/// updated inside the transaction. The whole transaction must roll back: no
/// traces ref, no catalog row, a released coverage claim, and the written
/// objects handed to doctor/GC through a cleanup-pending marker. After that
/// deterministic repair the exact native replay commits once, and a further
/// delivery is a pure acknowledgement.
#[tokio::test]
async fn checkpoint_store_cas_and_catalog_are_atomic() {
    let repo = HookRepo::init();
    let session = "sess-cas-catalog-atomic";
    repo.write_claude_transcript(session, TURN_COMPLETE);
    let start = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&repo.envelope("SessionStart", session, json!({}))),
        &[],
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );

    let turn_end = repo.envelope(
        "Stop",
        session,
        json!({ "turn_id": "turn-cas-catalog-atomic-v1" }),
    );
    repo.execute_sql(
        "CREATE TRIGGER test_abort_checkpoint_companion \
         BEFORE INSERT ON agent_checkpoint \
         BEGIN SELECT RAISE(ABORT, 'injected checkpoint companion failure'); END",
    )
    .await;
    let failed = repo.run(
        &["agent", "hooks", "claude-code", "stop"],
        Some(&turn_end),
        &[],
    );
    repo.execute_sql("DROP TRIGGER test_abort_checkpoint_companion")
        .await;
    assert!(
        !failed.status.success(),
        "a companion transaction failure must fail the hook: {}",
        describe(&failed)
    );
    let failure_stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        !failure_stderr.contains("injected checkpoint companion failure")
            && !failure_stderr.contains(session),
        "the hook failure must stay content-free: {failure_stderr}"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "the rolled-back companion must not leave a catalog checkpoint row"
    );
    let traces_after_failure = repo.run(&["rev-parse", "traces"], None, &[]);
    assert!(
        !traces_after_failure.status.success(),
        "the rolled-back ref CAS must not publish refs/libra/traces: {}",
        describe(&traces_after_failure)
    );
    let claims = repo
        .query_rows(
            "SELECT state, owner, lease_expires_at FROM agent_coverage_claim \
             WHERE logical_turn_key = 'u1'",
        )
        .await;
    assert_eq!(
        claims.len(),
        1,
        "the attempted turn must retain one claim row"
    );
    assert_eq!(
        claims[0].try_get_by::<String, _>("state").unwrap(),
        "abandoned",
        "the rolled-back companion must not commit its coverage claim"
    );
    assert!(
        claims[0]
            .try_get_by::<Option<String>, _>("owner")
            .unwrap()
            .is_none(),
        "the released claim must not retain a live owner"
    );
    let markers = repo
        .query_rows("SELECT value FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
        .await;
    assert_eq!(
        markers.len(),
        1,
        "the rejected append must keep exactly one ownership marker"
    );
    let marker: Value = serde_json::from_str(
        &markers[0]
            .try_get_by::<String, _>("value")
            .expect("decode marker value"),
    )
    .expect("marker value is JSON");
    assert_eq!(
        marker["cleanup_pending"],
        json!(true),
        "objects written before the rolled-back CAS must be handed to doctor/GC: {marker}"
    );
    assert!(
        marker["created_oids"]
            .as_array()
            .is_some_and(|oids| !oids.is_empty()),
        "the failure must happen after checkpoint objects were written: {marker}"
    );

    let repaired = repo.run(&["agent", "doctor", "--repair", "--json"], None, &[]);
    assert!(
        repaired.status.success(),
        "doctor must retire the rejected append's ownership marker: {}",
        describe(&repaired)
    );
    assert!(
        repo.query_rows("SELECT `key` FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await
            .is_empty(),
        "doctor repair must retire the cleanup-pending marker"
    );

    let replay = repo.run(
        &["agent", "hooks", "claude-code", "stop"],
        Some(&turn_end),
        &[],
    );
    assert!(
        replay.status.success(),
        "the exact native replay must commit after repair: {}",
        describe(&replay)
    );
    let committed = repo.checkpoints();
    assert_eq!(
        committed.len(),
        1,
        "the successful replay must publish one checkpoint: {committed:?}"
    );
    let commit = committed[0]["traces_commit"]
        .as_str()
        .expect("checkpoint has traces commit")
        .to_string();
    assert_eq!(
        repo.traces_head(),
        commit,
        "the published ref must agree with the catalog checkpoint"
    );
    let committed_claims = repo
        .query_rows("SELECT state FROM agent_coverage_claim WHERE logical_turn_key = 'u1'")
        .await;
    assert_eq!(
        committed_claims[0]
            .try_get_by::<String, _>("state")
            .unwrap(),
        "catalog_committed",
        "the successful ref/catalog transaction commits its coverage claim"
    );
    assert!(
        repo.query_rows("SELECT `key` FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await
            .is_empty(),
        "the committed replay must retire its writer marker"
    );

    let duplicate = repo.run(
        &["agent", "hooks", "claude-code", "stop"],
        Some(&turn_end),
        &[],
    );
    assert!(
        duplicate.status.success(),
        "completed native replay must acknowledge: {}",
        describe(&duplicate)
    );
    let after_duplicate = repo.checkpoints();
    assert_eq!(
        after_duplicate.len(),
        1,
        "completed replay must not create a second catalog row"
    );
    assert_eq!(
        after_duplicate[0]["traces_commit"].as_str(),
        Some(commit.as_str()),
        "completed replay must retain the original durable commit"
    );
    assert_eq!(
        repo.traces_head(),
        commit,
        "completed replay must not advance refs/libra/traces"
    );
}

/// A complete SessionEnd source is sealed into the local recovery namespace
/// before checkpoint object publication. If publication then fails, deleting
/// the provider source must not lose the snapshot: doctor repairs it from the
/// authenticated artifact and completes the original terminal receipt.
#[tokio::test]
async fn terminal_snapshot_survives_provider_source_removal_and_doctor_repair() {
    let repo = HookRepo::init();
    let session = "sess-terminal-artifact-repair";
    repo.write_claude_transcript(session, TURN_COMPLETE);
    let start = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&repo.envelope("SessionStart", session, json!({}))),
        &[],
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );

    let terminal = repo.envelope(
        "SessionEnd",
        session,
        json!({"event_id": "terminal-artifact-repair-v1"}),
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
        &[],
    );
    restore_checkpoint_object_directory(&repo, blocker);
    assert!(
        !failed.status.success(),
        "blocked checkpoint publication must fail after durable artifact prepare: {}",
        describe(&failed)
    );

    let pending = repo
        .query_rows("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_capture_pending'")
        .await;
    assert_eq!(
        pending[0]
            .try_get_by::<i64, _>("n")
            .expect("decode pending artifact count"),
        1,
        "the complete terminal snapshot must remain locally recoverable; hook result: {}",
        describe(&failed)
    );
    let pending_bytes = repo.artifact_rows().await;
    assert!(!pending_bytes.is_empty(), "artifact rows must be present");
    // The artifact transaction hands its verified claims to the artifact:
    // the ordinary 60 s lease becomes the non-expiring retention sentinel.
    let retained_claims = repo.coverage_claims().await;
    assert!(
        !retained_claims.is_empty()
            && retained_claims
                .iter()
                .all(|(_, state, owner, _, _, lease)| {
                    state == "reserved_live" && owner.is_some() && *lease == Some(i64::MAX)
                }),
        "the persisted artifact must own non-expiring coverage claims: {retained_claims:?}"
    );

    // An immediate native redelivery observes the retained artifact before
    // reading the provider source or reserving coverage. The object store is
    // writable, so a regression that let it continue could publish a second
    // payload; instead it must stop at the artifact-owned stage.
    let (duplicate, stage) = redeliver_terminal_with_recovery_log(&repo, &terminal, "immediate");
    assert!(
        !duplicate.status.success(),
        "a retained terminal is retryable, not acknowledged: {}",
        describe(&duplicate)
    );
    assert!(
        stage.contains("terminal_artifact_retained"),
        "the redelivery must stop at the retained-artifact stage: {stage}"
    );
    assert!(
        !stage.contains(session) && !describe(&duplicate).contains(session),
        "the retained-artifact diagnostic must stay content-free: {stage}"
    );
    assert_eq!(
        repo.artifact_rows().await,
        pending_bytes,
        "native redelivery must not rewrite the authenticated artifact payload"
    );
    assert_eq!(repo.coverage_claims().await, retained_claims);
    assert!(
        repo.checkpoints().is_empty(),
        "no second payload is published"
    );

    // Model a claim lease the gate would consider expired (for example an
    // artifact retained before claim retention existed). The redelivery must
    // still not fence the artifact's claims out: owner/fence/revision stay
    // exactly those sealed into the envelope.
    repo.execute_sql(
        "UPDATE agent_coverage_claim SET lease_expires_at = 0 WHERE state = 'reserved_live'",
    )
    .await;
    let expired_claims = repo.coverage_claims().await;
    let (expired, stage) = redeliver_terminal_with_recovery_log(&repo, &terminal, "expired");
    assert!(
        !expired.status.success() && stage.contains("terminal_artifact_retained"),
        "an expired-lease redelivery must stop at the retained-artifact stage: {}\n{stage}",
        describe(&expired)
    );
    assert_eq!(
        repo.coverage_claims().await,
        expired_claims,
        "an expired-lease redelivery must not take over artifact-owned claims"
    );
    assert_eq!(
        repo.artifact_rows().await,
        pending_bytes,
        "an expired-lease redelivery must not rewrite the authenticated artifact payload"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "no second payload is published"
    );
    let source = repo
        .home
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(&repo.repo))
        .join(format!("{session}.jsonl"));
    std::fs::remove_file(&source).expect("remove provider source before recovery");

    let repaired = repo.run(&["agent", "doctor", "--repair", "--json"], None, &[]);
    assert!(
        repaired.status.success(),
        "doctor must replay the authenticated local artifact without the provider source: {}",
        describe(&repaired)
    );
    let report: Value =
        serde_json::from_slice(&repaired.stdout).expect("doctor JSON report must parse");
    assert!(
        report["data"]["checkpoint_store"]["findings"]
            .as_array()
            .is_some_and(|findings| {
                findings.iter().any(|finding| {
                    finding["repaired"] == json!(true)
                        && finding["detail"]
                            .as_str()
                            .is_some_and(|detail| detail.contains("authenticated local checkpoint"))
                })
            }),
        "doctor must report the authenticated artifact repair: {report}"
    );
    assert_eq!(repo.checkpoints().len(), 1, "one checkpoint is published");
    assert_eq!(
        repo.session_show(&format!("claude__{session}"))["state"],
        json!("stopped")
    );
    let remaining = repo
        .query_rows("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_capture_pending'")
        .await;
    assert_eq!(
        remaining[0]
            .try_get_by::<i64, _>("n")
            .expect("decode remaining artifact count"),
        0,
        "successful replay atomically cleans the artifact"
    );
}

/// A definite pre-commit artifact failure (here: the 16-artifact capacity is
/// full) rolls the artifact transaction back, so the terminal's fresh
/// coverage reservations must be released at once rather than left on a
/// 60 s lease that misreports the next redelivery as "held by another live
/// writer". Retained evidence is never discarded to make room.
#[tokio::test]
async fn terminal_artifact_capacity_failure_releases_fresh_coverage_claims() {
    let repo = HookRepo::init();
    let first = "sess-terminal-capacity-first";
    repo.write_claude_transcript(first, TURN_COMPLETE);
    let blocker = block_checkpoint_object_directory(&repo);
    let blocked = repo.run(
        &[
            "agent",
            "hooks",
            "--capture-budget-ms",
            "60000",
            "claude-code",
            "session-end",
        ],
        Some(&repo.envelope(
            "SessionEnd",
            first,
            json!({"event_id": "terminal-capacity-first-v1"}),
        )),
        &[],
    );
    restore_checkpoint_object_directory(&repo, blocker);
    assert!(
        !blocked.status.success(),
        "blocked publication leaves the first terminal retained: {}",
        describe(&blocked)
    );
    // Fill the remaining per-repository artifact capacity.
    repo.seed_unresolvable_pending_headers(15).await;
    let retained = repo.artifact_rows().await;
    assert_eq!(
        retained
            .iter()
            .filter(|(scope, ..)| scope == "agent_capture_pending")
            .count(),
        16,
        "the artifact capacity must be exactly full"
    );

    // No SessionStart: its recovery hint would race a detached worker.
    let second = "sess-terminal-capacity-second";
    repo.write_claude_transcript(second, TURN_COMPLETE);
    let refused = repo.run(
        &[
            "agent",
            "hooks",
            "--capture-budget-ms",
            "60000",
            "claude-code",
            "session-end",
        ],
        Some(&repo.envelope(
            "SessionEnd",
            second,
            json!({"event_id": "terminal-capacity-second-v1"}),
        )),
        &[],
    );
    assert!(
        !refused.status.success(),
        "a terminal that cannot be retained must fail closed: {}",
        describe(&refused)
    );
    assert_eq!(
        repo.artifact_rows().await,
        retained,
        "capacity exhaustion must not overwrite or discard retained evidence"
    );
    assert!(repo.checkpoints().is_empty(), "nothing is published");
    // The refused terminal elected a concrete marker/source before failing,
    // so the failure was the artifact transaction itself — not an earlier
    // stage whose own cleanup would release the claims regardless.
    let (state, _, _, metadata_json) = repo.session_ledger(second).await;
    assert_ne!(state, "stopped", "the refused terminal must not settle");
    let metadata: Value = serde_json::from_str(&metadata_json).expect("receipt metadata is JSON");
    let finalizer = metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["intent"]["phase"] == json!("stopped"))
        })
        .map(|entry| entry["finalizer"].clone())
        .expect("the refused terminal persisted its finalizer");
    assert!(
        finalizer["marker_generation"]
            .as_str()
            .is_some_and(|marker| !marker.starts_with("capture-finalizer-unbound-"))
            && finalizer["source_digest"].is_string(),
        "the refused terminal must have elected its attempt before the artifact stage: {finalizer}"
    );
    let second_claims = repo
        .query_rows(&format!(
            "SELECT state, owner, lease_expires_at FROM agent_coverage_claim \
             WHERE session_id = 'claude__{second}'"
        ))
        .await
        .into_iter()
        .map(|row| {
            (
                row.try_get_by::<String, _>("state")
                    .expect("decode refused claim state"),
                row.try_get_by::<Option<String>, _>("owner")
                    .expect("decode refused claim owner"),
                row.try_get_by::<Option<i64>, _>("lease_expires_at")
                    .expect("decode refused claim lease"),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        !second_claims.is_empty()
            && second_claims
                .iter()
                .all(|claim| *claim == ("abandoned".to_string(), None, None)),
        "a rolled-back artifact transaction must release its fresh claims: {second_claims:?}"
    );
}

/// SessionStart only emits an indexed pending hint and launches the bounded
/// detached worker. The parent hook does not wait for it; the worker recovers
/// a prior session after its provider transcript has been deleted. A
/// debug-build hold keeps the first worker alive until the test releases it,
/// proving it outlives the parent and its piped stdout/stderr.
#[tokio::test]
async fn session_start_hint_launches_source_free_capture_recovery_worker() {
    let repo = HookRepo::init();
    let session = "sess-start-hint-worker";
    repo.write_claude_transcript(session, TURN_COMPLETE);
    let start = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&repo.envelope("SessionStart", session, json!({}))),
        &[],
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );

    let second_session = "sess-start-hint-worker-second";
    repo.write_claude_transcript(second_session, TURN_COMPLETE);
    let second_start = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&repo.envelope("SessionStart", second_session, json!({}))),
        &[],
    );
    assert!(
        second_start.status.success(),
        "second SessionStart: {}",
        describe(&second_start)
    );

    let terminal = repo.envelope(
        "SessionEnd",
        session,
        json!({"event_id": "session-start-worker-v1"}),
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
        &[],
    );
    restore_checkpoint_object_directory(&repo, blocker);
    assert!(
        !failed.status.success(),
        "the injected object-store failure must leave a pending artifact: {}",
        describe(&failed)
    );
    let second_blocker = block_checkpoint_object_directory(&repo);
    let second_failed = repo.run(
        &[
            "agent",
            "hooks",
            "--capture-budget-ms",
            "60000",
            "claude-code",
            "session-end",
        ],
        Some(&repo.envelope(
            "SessionEnd",
            second_session,
            json!({"event_id": "session-start-worker-second-v1"}),
        )),
        &[],
    );
    restore_checkpoint_object_directory(&repo, second_blocker);
    assert!(
        !second_failed.status.success(),
        "second blocked SessionEnd: {}",
        describe(&second_failed)
    );
    repo.corrupt_earliest_pending_header_mac().await;
    let source = repo
        .home
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(&repo.repo))
        .join(format!("{session}.jsonl"));
    repo.seed_unrelated_sessions_before(&format!("claude__{session}"), 520)
        .await;
    repo.seed_unresolvable_pending_headers(5).await;
    std::fs::remove_file(source).expect("remove provider source before detached replay");
    let second_source = repo
        .home
        .join(".claude")
        .join("projects")
        .join(claude_project_slug(&repo.repo))
        .join(format!("{second_session}.jsonl"));
    std::fs::remove_file(second_source).expect("remove second provider source before replay");

    // Debug-build hold point: the detached child takes the worker lock, then
    // waits for `release` before any recovery work, so it is provably alive
    // after its parent has exited.
    let release = repo.home.join("capture-worker-release");
    let quarantine_count_sql =
        "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_capture_quarantine'";
    let quarantined_before = repo.query_rows(quarantine_count_sql).await[0]
        .try_get_by::<i64, _>("n")
        .expect("decode quarantine count before trigger");
    let next_session = "sess-start-hint-trigger";
    let started = Instant::now();
    let trigger = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&repo.envelope("SessionStart", next_session, json!({}))),
        &[
            ("LIBRA_TEST", "1"),
            (
                "LIBRA_TEST_CAPTURE_WORKER_HOLD_PATH",
                release.to_str().expect("release path is UTF-8"),
            ),
        ],
    );
    // `run` returns only once the parent has exited AND its piped stdout and
    // stderr reached EOF. A parent that waited for the child, or a child
    // holding either pipe, would block here until the child's 30-second hold
    // cap expires.
    let parent_elapsed = started.elapsed();
    assert!(
        trigger.status.success(),
        "hint trigger: {}",
        describe(&trigger)
    );
    assert!(
        parent_elapsed < Duration::from_secs(2),
        "the SessionStart parent must return within the 2s worker batch budget without waiting for its detached child; took {parent_elapsed:?}"
    );
    let owned_deadline = Instant::now() + Duration::from_secs(10);
    while !repo.capture_worker_lock_owned() {
        assert!(
            Instant::now() < owned_deadline,
            "a detached worker must own the repository lock after its parent exited"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        repo.query_rows(quarantine_count_sql).await[0]
            .try_get_by::<i64, _>("n")
            .expect("decode quarantine count while held"),
        quarantined_before,
        "the held worker must not start recovery before release"
    );
    std::fs::write(&release, b"").expect("release the held detached worker");

    let mut quarantined = false;
    for _ in 0..80 {
        let count = repo
            .query_rows(
                "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_capture_quarantine'",
            )
            .await;
        if count[0]
            .try_get_by::<i64, _>("n")
            .expect("decode quarantine count")
            == 6
        {
            quarantined = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        quarantined,
        "the worker must quarantine five unresolvable headers and the tampered envelope after refilling"
    );
    let released_deadline = Instant::now() + Duration::from_secs(10);
    while repo.capture_worker_lock_owned() {
        assert!(
            Instant::now() < released_deadline,
            "the detached worker must exit and release its OS lock"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The worker refills its queue after quarantining, so it may replay the
    // valid artifact within the same 2s batch. Once it has exited, that
    // artifact is either recovered or still pending for the next hint, never
    // quarantined or lost.
    assert_eq!(
        repo.query_rows(quarantine_count_sql).await[0]
            .try_get_by::<i64, _>("n")
            .expect("decode quarantine count after worker exit"),
        6,
        "the worker must not quarantine the valid artifact"
    );
    let still_pending = repo
        .query_rows("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_capture_pending'")
        .await[0]
        .try_get_by::<i64, _>("n")
        .expect("decode remaining pending count");
    let recovered_by_first_worker = repo.checkpoints().len();
    assert!(
        matches!((still_pending, recovered_by_first_worker), (1, 0) | (0, 1)),
        "the valid artifact must be either recovered or retained for a subsequent hint; pending={still_pending}, checkpoints={recovered_by_first_worker}"
    );
    let retry = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&repo.envelope("SessionStart", "sess-start-hint-retry", json!({}))),
        &[],
    );
    assert!(retry.status.success(), "retry hint: {}", describe(&retry));

    let mut quarantined_envelope = false;
    for _ in 0..80 {
        let count = repo
            .query_rows(
                "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_capture_quarantine'",
            )
            .await;
        if count[0]
            .try_get_by::<i64, _>("n")
            .expect("decode quarantine count")
            == 6
        {
            quarantined_envelope = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        quarantined_envelope,
        "the tampered envelope must be quarantined"
    );

    let mut recovered = false;
    for _ in 0..80 {
        if repo.checkpoints().len() == 1 {
            recovered = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        recovered,
        "the detached worker must replay authenticated artifacts without provider sources; checkpoints={}, pending={:?}, quarantine={:?}",
        repo.checkpoints().len(),
        repo.query_rows(
            "SELECT key FROM metadata_kv WHERE scope = 'agent_capture_pending' ORDER BY key"
        )
        .await
        .iter()
        .map(|row| row
            .try_get_by::<String, _>("key")
            .expect("decode pending key"))
        .collect::<Vec<_>>(),
        repo.query_rows(
            "SELECT key FROM metadata_kv WHERE scope = 'agent_capture_quarantine' ORDER BY key"
        )
        .await
        .iter()
        .map(|row| row
            .try_get_by::<String, _>("key")
            .expect("decode quarantine key"))
        .collect::<Vec<_>>()
    );
    assert!(
        repo.checkpoints().len() == 1,
        "the corrupt artifact must be quarantined while the later valid artifact is recovered"
    );
    let remaining = repo
        .query_rows("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_capture_pending'")
        .await;
    assert_eq!(
        remaining[0]
            .try_get_by::<i64, _>("n")
            .expect("decode pending count after recovery"),
        0,
        "successful detached recovery must remove the pending artifact"
    );
    let first_stopped =
        repo.session_show(&format!("claude__{session}"))["state"] == json!("stopped");
    let second_stopped =
        repo.session_show(&format!("claude__{second_session}"))["state"] == json!("stopped");
    assert_ne!(
        first_stopped, second_stopped,
        "one valid artifact must complete while the tampered earlier artifact remains quarantined"
    );
}

/// One CLI-observable fault armed around the first native delivery of a
/// [`capture_coordinator_native_replay_matrix`] case.
#[derive(Clone, Copy, Debug)]
enum ReplayFault {
    /// The object directory is unusable after the writer registered its
    /// marker: nothing becomes durable and the marker is retired.
    PostRegistration,
    /// The ref/catalog transaction commits, then ordinary marker retirement
    /// fails: the checkpoint is durable but its receipt stays pending.
    MarkerCleanup,
    /// A foreign live writer holds the turn's coverage claim, so the
    /// delivery stops before any checkpoint work.
    CoverageHeld,
}

impl ReplayFault {
    async fn arm(self, repo: &HookRepo, provider_session: &str) -> Option<PathBuf> {
        match self {
            Self::PostRegistration => block_checkpoint_object_directory(repo),
            Self::MarkerCleanup => {
                repo.execute_sql(
                    "CREATE TRIGGER test_block_marker_cleanup \
                     BEFORE DELETE ON metadata_kv WHEN OLD.scope = 'agent_traces_inflight' \
                     BEGIN SELECT RAISE(ABORT, 'injected marker cleanup failure'); END",
                )
                .await;
                None
            }
            Self::CoverageHeld => {
                repo.execute_sql(&format!(
                    "INSERT INTO agent_coverage_claim (
                        session_id, logical_turn_key, coverage_schema_version,
                        coverage_digest, completeness, revision, state, owner,
                        lease_expires_at, fence_token, source_channel, created_at, updated_at
                     ) SELECT session_id, 'u1', 1, 'foreign-digest', 'complete', 0,
                              'reserved_live', 'foreign-owner', 4102444800000, 1, 'live', 0, 0
                       FROM agent_session WHERE provider_session_id = '{provider_session}'"
                ))
                .await;
                None
            }
        }
    }

    async fn disarm(self, repo: &HookRepo, blocker: Option<PathBuf>) {
        match self {
            Self::PostRegistration => restore_checkpoint_object_directory(repo, blocker),
            Self::MarkerCleanup => {
                repo.execute_sql("DROP TRIGGER test_block_marker_cleanup")
                    .await;
            }
            // The foreign writer finishes without committing: its claim is
            // abandoned and becomes re-ownable by the native replay.
            Self::CoverageHeld => {
                repo.execute_sql(
                    "UPDATE agent_coverage_claim \
                     SET state = 'abandoned', owner = NULL, lease_expires_at = NULL \
                     WHERE owner = 'foreign-owner'",
                )
                .await;
            }
        }
    }
}

/// One row of [`capture_coordinator_native_replay_matrix`].
struct ReplayCase {
    label: &'static str,
    provider: &'static str,
    provider_prefix: &'static str,
    /// Native `hook_event_name` of the provider's SessionStart.
    start_event: &'static str,
    verb: &'static str,
    hook_event_name: &'static str,
    /// Native replay identity field carried by the envelope.
    identity_key: &'static str,
    /// Whether a coverage-parseable provider transcript exists.
    transcript: bool,
    fault: ReplayFault,
    /// `agent_checkpoint.scope` of the one durable checkpoint.
    scope: &'static str,
    /// Durable receipt intent `(checkpoint class, phase)`.
    receipt: (&'static str, &'static str),
    terminal: bool,
    /// ACF-17 per-provider durable oracle: `agent_session.agent_kind`,
    /// the checkpoint's `transcript_snapshot.partial_reason`, and the
    /// `source_channel` of every coverage claim the session owns.
    durable: ReplayDurableOracle,
}

/// Provider-specific durable facts every completed replay must reproduce.
struct ReplayDurableOracle {
    agent_kind: &'static str,
    partial_reason: Option<&'static str>,
    claim_channels: &'static [&'static str],
}

/// ACF-06 AC4/AC7: the failure outcome of a first native delivery is the
/// only state a replay resumes from. Each case differs by fault stage and/or
/// lifecycle kind; for every case the identical native replay must complete
/// the original receipt with exactly one durable checkpoint, and a further
/// duplicate must change neither the checkpoint, the traces ref, nor the
/// session ledger. Faults are armed through CLI-observable repository state
/// so no production hook environment can select them. ACF-17 extends every
/// case with a per-provider durable oracle (provider-prefixed session id,
/// `agent_kind`, `transcript_snapshot.partial_reason`, claim channels) and
/// adds OpenCode rows; ADR-ACF-10 freezes this function's token stream.
#[tokio::test]
async fn capture_coordinator_native_replay_matrix() {
    let cases = [
        ReplayCase {
            label: "turn-end-post-registration",
            provider: "claude-code",
            provider_prefix: "claude",
            start_event: "SessionStart",
            verb: "stop",
            hook_event_name: "Stop",
            identity_key: "turn_id",
            transcript: true,
            fault: ReplayFault::PostRegistration,
            scope: "committed",
            receipt: ("committed", "active"),
            terminal: false,
            durable: ReplayDurableOracle {
                agent_kind: "claude_code",
                partial_reason: None,
                claim_channels: &["live"],
            },
        },
        ReplayCase {
            label: "turn-end-coverage-held",
            provider: "claude-code",
            provider_prefix: "claude",
            start_event: "SessionStart",
            verb: "stop",
            hook_event_name: "Stop",
            identity_key: "turn_id",
            transcript: true,
            fault: ReplayFault::CoverageHeld,
            scope: "committed",
            receipt: ("committed", "active"),
            terminal: false,
            durable: ReplayDurableOracle {
                agent_kind: "claude_code",
                partial_reason: None,
                claim_channels: &["live"],
            },
        },
        ReplayCase {
            label: "turn-end-marker-cleanup",
            provider: "claude-code",
            provider_prefix: "claude",
            start_event: "SessionStart",
            verb: "stop",
            hook_event_name: "Stop",
            identity_key: "turn_id",
            transcript: false,
            fault: ReplayFault::MarkerCleanup,
            scope: "committed",
            receipt: ("committed", "active"),
            terminal: false,
            durable: ReplayDurableOracle {
                agent_kind: "claude_code",
                partial_reason: Some("source_absent"),
                claim_channels: &[],
            },
        },
        // The first delivery commits the turn's coverage claim with its
        // checkpoint, so the replay reaches the coverage-gate no-op rather
        // than the checkpoint store's write. That settlement must still let
        // the store's replay retire the leftover ordinary marker.
        ReplayCase {
            label: "turn-end-covered-marker-cleanup",
            provider: "claude-code",
            provider_prefix: "claude",
            start_event: "SessionStart",
            verb: "stop",
            hook_event_name: "Stop",
            identity_key: "turn_id",
            transcript: true,
            fault: ReplayFault::MarkerCleanup,
            scope: "committed",
            receipt: ("committed", "active"),
            terminal: false,
            durable: ReplayDurableOracle {
                agent_kind: "claude_code",
                partial_reason: None,
                claim_channels: &["live"],
            },
        },
        ReplayCase {
            label: "session-end-marker-cleanup",
            provider: "claude-code",
            provider_prefix: "claude",
            start_event: "SessionStart",
            verb: "session-end",
            hook_event_name: "SessionEnd",
            identity_key: "event_id",
            transcript: false,
            fault: ReplayFault::MarkerCleanup,
            scope: "committed",
            receipt: ("committed", "stopped"),
            terminal: true,
            durable: ReplayDurableOracle {
                agent_kind: "claude_code",
                partial_reason: Some("source_absent"),
                claim_channels: &[],
            },
        },
        ReplayCase {
            label: "subagent-end-post-registration",
            provider: "codex",
            provider_prefix: "codex",
            start_event: "SessionStart",
            verb: "subagent-end",
            hook_event_name: "SubagentStop",
            identity_key: "event_id",
            transcript: false,
            fault: ReplayFault::PostRegistration,
            scope: "subagent",
            receipt: ("subagent_boundary", "active"),
            terminal: false,
            durable: ReplayDurableOracle {
                agent_kind: "codex",
                partial_reason: None,
                claim_channels: &[],
            },
        },
        ReplayCase {
            label: "turn-end-opencode-post-registration",
            provider: "opencode",
            provider_prefix: "opencode",
            start_event: "session.created",
            verb: "stop",
            hook_event_name: "session.idle",
            identity_key: "event_id",
            transcript: false,
            fault: ReplayFault::PostRegistration,
            scope: "committed",
            receipt: ("committed", "active"),
            terminal: false,
            durable: ReplayDurableOracle {
                agent_kind: "opencode",
                partial_reason: Some("source_read_error"),
                claim_channels: &[],
            },
        },
        ReplayCase {
            label: "session-end-opencode-marker-cleanup",
            provider: "opencode",
            provider_prefix: "opencode",
            start_event: "session.created",
            verb: "session-end",
            hook_event_name: "session.deleted",
            identity_key: "event_id",
            transcript: false,
            fault: ReplayFault::MarkerCleanup,
            scope: "committed",
            receipt: ("committed", "stopped"),
            terminal: true,
            durable: ReplayDurableOracle {
                agent_kind: "opencode",
                partial_reason: Some("source_read_error"),
                claim_channels: &[],
            },
        },
    ];

    for case in cases {
        let label = case.label;
        let repo = HookRepo::init();
        let session = format!("sess-replay-matrix-{label}");
        if case.transcript {
            repo.write_claude_transcript(&session, TURN_COMPLETE);
        }
        let start = repo.run(
            &["agent", "hooks", case.provider, "session-start"],
            Some(&repo.envelope(case.start_event, &session, json!({}))),
            &[],
        );
        assert!(
            start.status.success(),
            "{label}: session start: {}",
            describe(&start)
        );
        let hook = ["agent", "hooks", case.provider, case.verb];
        let mut identity = serde_json::Map::new();
        identity.insert(
            case.identity_key.to_string(),
            json!(format!("replay-matrix-{label}-v1")),
        );
        let delivery = repo.envelope(case.hook_event_name, &session, Value::Object(identity));
        let pending = vec![(
            case.receipt.0.to_string(),
            case.receipt.1.to_string(),
            "pending".to_string(),
        )];
        let complete = vec![(
            case.receipt.0.to_string(),
            case.receipt.1.to_string(),
            "complete".to_string(),
        )];

        // First delivery fails at the case's stage.
        let blocker = case.fault.arm(&repo, &session).await;
        let failed = repo.run(&hook, Some(&delivery), &[]);
        let claims_under_fault = repo.coverage_claims().await;
        case.fault.disarm(&repo, blocker).await;
        assert!(
            !failed.status.success(),
            "{label}: the faulted delivery must not acknowledge: {}",
            describe(&failed)
        );
        let stderr = String::from_utf8_lossy(&failed.stderr);
        assert!(
            !stderr.contains(&session)
                && !stderr.contains("injected")
                && !stderr.contains("foreign"),
            "{label}: the failure must stay content-free: {}",
            describe(&failed)
        );
        assert_eq!(
            repo.checkpoint_receipts(&session).await,
            pending,
            "{label}: the failed delivery must leave its receipt pending"
        );
        assert_ne!(
            repo.session_ledger(&session).await.0,
            "stopped",
            "{label}: a pending receipt must not publish a terminal state"
        );
        let faulted_head = repo.traces_head_if_present();
        match case.fault {
            ReplayFault::PostRegistration | ReplayFault::CoverageHeld => {
                assert!(
                    repo.checkpoints().is_empty() && faulted_head.is_none(),
                    "{label}: nothing may become durable before the fault clears"
                );
                assert_eq!(
                    repo.inflight_marker_count().await,
                    0,
                    "{label}: a pre-durable failure must retire its writer marker"
                );
            }
            ReplayFault::MarkerCleanup => {
                let durable = repo.checkpoints();
                assert_eq!(
                    durable.len(),
                    1,
                    "{label}: the ref/catalog transaction committed before cleanup failed"
                );
                assert_eq!(
                    faulted_head.as_deref(),
                    durable[0]["traces_commit"].as_str(),
                    "{label}: the durable checkpoint is already the traces head"
                );
                assert_eq!(
                    repo.inflight_marker_count().await,
                    1,
                    "{label}: the failed cleanup must leave its ordinary marker"
                );
                let checkpoint_id = durable[0]["checkpoint_id"]
                    .as_str()
                    .expect("durable checkpoint has an id");
                let leftover = repo.inflight_markers_for(checkpoint_id).await;
                assert!(
                    leftover.len() == 1 && leftover[0].get("cleanup_pending").is_none(),
                    "{label}: the leftover marker is this action's ordinary marker: {leftover:?}"
                );
            }
        }
        if matches!(case.fault, ReplayFault::CoverageHeld) {
            assert_eq!(
                claims_under_fault,
                vec![(
                    "u1".to_string(),
                    "reserved_live".to_string(),
                    Some("foreign-owner".to_string()),
                    1,
                    0,
                    Some(4_102_444_800_000),
                )],
                "{label}: the delivery must not take over a live foreign claim"
            );
        }

        // The identical native replay resumes the failure outcome.
        let replay = repo.run(&hook, Some(&delivery), &[]);
        assert!(
            replay.status.success(),
            "{label}: the native replay must complete: {}",
            describe(&replay)
        );
        let committed = repo.checkpoints();
        assert_eq!(
            committed.len(),
            1,
            "{label}: replay must leave exactly one durable checkpoint: {committed:?}"
        );
        assert_eq!(
            committed[0]["scope"].as_str(),
            Some(case.scope),
            "{label}: checkpoint scope"
        );
        let commit = committed[0]["traces_commit"]
            .as_str()
            .expect("checkpoint has traces commit")
            .to_string();
        assert_eq!(
            repo.traces_head(),
            commit,
            "{label}: the traces ref must name the catalog checkpoint"
        );
        if let Some(faulted_head) = faulted_head {
            assert_eq!(
                commit, faulted_head,
                "{label}: replay over a durable checkpoint must not advance the traces ref"
            );
        }
        let checkpoint_id = committed[0]["checkpoint_id"]
            .as_str()
            .expect("checkpoint has an id")
            .to_string();
        assert!(
            repo.inflight_markers_for(&checkpoint_id).await.is_empty(),
            "{label}: replay must retire this action's ordinary writer marker"
        );
        assert_eq!(
            repo.inflight_marker_count().await,
            0,
            "{label}: replay must leave no writer marker behind"
        );
        assert_eq!(
            repo.checkpoint_receipts(&session).await,
            complete,
            "{label}: replay must complete the original receipt"
        );
        let ledger = repo.session_ledger(&session).await;
        assert_eq!(
            ledger.0 == "stopped",
            case.terminal,
            "{label}: only a terminal receipt publishes `stopped`: {ledger:?}"
        );
        assert_eq!(
            case.terminal,
            ledger.1.is_some(),
            "{label}: stopped_at is set exactly for a terminal completion"
        );

        // A completed receipt turns every further delivery into a pure
        // acknowledgement.
        let duplicate = repo.run(&hook, Some(&delivery), &[]);
        assert!(
            duplicate.status.success(),
            "{label}: completed replay acknowledges: {}",
            describe(&duplicate)
        );
        assert_eq!(
            repo.checkpoints(),
            committed,
            "{label}: duplicate must not append or rewrite a checkpoint"
        );
        assert_eq!(
            repo.traces_head(),
            commit,
            "{label}: duplicate must not advance refs/libra/traces"
        );
        assert_eq!(
            repo.session_ledger(&session).await,
            ledger,
            "{label}: duplicate must not revise state, revision, or receipts"
        );
        assert!(
            repo.session_show(&format!("{}__{session}", case.provider_prefix))
                .get("capture_status")
                .is_none(),
            "{label}: successful replay clears the retry diagnostic"
        );
        let durable = repo
            .replay_durable_oracle(&session, case.provider_prefix, &checkpoint_id)
            .await;
        assert_eq!(
            durable,
            (
                case.durable.agent_kind.to_string(),
                case.durable.partial_reason.map(str::to_string),
                case.durable
                    .claim_channels
                    .iter()
                    .map(|channel| channel.to_string())
                    .collect::<Vec<_>>(),
            ),
            "{label}: per-provider durable oracle (agent kind, snapshot partial reason, claim channels)"
        );
    }
}

/// ACF-05/ACF-06 AC4: a covered TurnEnd replay settles its receipt through
/// the checkpoint store's replay authority, not around it. When this
/// action's leftover marker still owns objects for doctor/GC, the covered
/// replay must neither erase that ownership nor acknowledge; after repair
/// the identical replay completes the original receipt without a second
/// append.
#[tokio::test]
async fn covered_turn_end_replay_keeps_cleanup_owned_marker_until_repair() {
    let repo = HookRepo::init();
    let session = "sess-covered-cleanup-owned";
    repo.write_claude_transcript(session, TURN_COMPLETE);
    let start = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&repo.envelope("SessionStart", session, json!({}))),
        &[],
    );
    assert!(
        start.status.success(),
        "session start: {}",
        describe(&start)
    );
    let hook = ["agent", "hooks", "claude-code", "stop"];
    let delivery = repo.envelope(
        "Stop",
        session,
        json!({"turn_id": "covered-cleanup-owned-v1"}),
    );
    let pending = vec![(
        "committed".to_string(),
        "active".to_string(),
        "pending".to_string(),
    )];

    ReplayFault::MarkerCleanup.arm(&repo, session).await;
    let failed = repo.run(&hook, Some(&delivery), &[]);
    ReplayFault::MarkerCleanup.disarm(&repo, None).await;
    assert!(
        !failed.status.success(),
        "the faulted delivery must not acknowledge: {}",
        describe(&failed)
    );
    let durable = repo.checkpoints();
    assert_eq!(durable.len(), 1, "the checkpoint committed before cleanup");
    let checkpoint_id = durable[0]["checkpoint_id"]
        .as_str()
        .expect("checkpoint has an id")
        .to_string();
    let head = repo.traces_head();
    repo.execute_sql(&format!(
        "UPDATE metadata_kv SET value = json_set(value, '$.cleanup_pending', json('true')) \
         WHERE scope = 'agent_traces_inflight' AND `key` = '{checkpoint_id}'"
    ))
    .await;

    let blocked = repo.run(&hook, Some(&delivery), &[]);
    assert!(
        !blocked.status.success(),
        "a covered replay must not acknowledge over cleanup-owned objects: {}",
        describe(&blocked)
    );
    let stderr = String::from_utf8_lossy(&blocked.stderr);
    assert!(
        !stderr.contains(session) && !stderr.contains(&checkpoint_id),
        "the blocked replay must stay content-free: {}",
        describe(&blocked)
    );
    let retained = repo.inflight_markers_for(&checkpoint_id).await;
    assert!(
        retained.len() == 1 && retained[0]["cleanup_pending"] == json!(true),
        "the covered replay must never retire cleanup ownership: {retained:?}"
    );
    assert_eq!(
        repo.checkpoint_receipts(session).await,
        pending,
        "the receipt stays pending until cleanup is resolved"
    );
    assert_eq!(repo.checkpoints(), durable, "no second checkpoint");
    assert_eq!(repo.traces_head(), head, "no second traces append");

    let repaired = repo.run(&["agent", "doctor", "--repair", "--json"], None, &[]);
    assert!(
        repaired.status.success(),
        "doctor must retire the cleanup-owned marker: {}",
        describe(&repaired)
    );
    assert_eq!(repo.inflight_marker_count().await, 0);

    let replay = repo.run(&hook, Some(&delivery), &[]);
    assert!(
        replay.status.success(),
        "the identical replay completes after repair: {}",
        describe(&replay)
    );
    assert_eq!(
        repo.checkpoint_receipts(session).await,
        vec![(
            "committed".to_string(),
            "active".to_string(),
            "complete".to_string(),
        )],
        "the replay completes the original receipt"
    );
    assert_eq!(repo.checkpoints(), durable, "replay must not append");
    assert_eq!(repo.traces_head(), head, "replay must not advance the ref");
    assert_eq!(repo.inflight_marker_count().await, 0);
}

/// ACF-06 AC7: routing capture through the coordinator keeps the public hook
/// commands' exit/output contract byte-for-byte. Success and Codex's
/// advisory nonterminal failure print nothing on either stream and exit 0;
/// a terminal failure exits 128 with one fixed, content-free fatal line, its
/// stable error code, and the structured envelope (stderr is not a TTY).
#[test]
fn hook_command_exit_and_output_contract_is_byte_stable() {
    const TERMINAL_FAILURE_STDERR: &str = concat!(
        "fatal: hook ingestion failed: capture could not be completed; ",
        "retry the hook or inspect the local repository\n",
        "Error-Code: LBR-INTERNAL-001\n",
        r#"{"ok":false,"error_code":"LBR-INTERNAL-001","category":"internal","exit_code":128,"#,
        r#""severity":"fatal","message":"hook ingestion failed: capture could not be completed; "#,
        r#"retry the hook or inspect the local repository"}"#,
        "\n",
    );
    let repo = HookRepo::init();
    let claude = "sess-hook-contract-claude";
    let codex = "sess-hook-contract-codex";
    for (provider, session) in [("claude", claude), ("codex", codex)] {
        let started = repo.run(
            &["hooks", provider, "session-start"],
            Some(&repo.envelope("SessionStart", session, json!({}))),
            &[],
        );
        assert_eq!(
            (
                started.status.code(),
                started.stdout.as_slice(),
                started.stderr.as_slice()
            ),
            (Some(0), &b""[..], &b""[..]),
            "{provider} success contract: {}",
            describe(&started)
        );
    }

    let marker = "LIBRA_TEST_HOOK_CONTRACT_MARKER_7c1e";
    let blocker = block_checkpoint_object_directory(&repo);
    let advisory = repo.run(
        &["hooks", "codex", "stop"],
        Some(&repo.envelope(
            "Stop",
            codex,
            json!({ "event_id": "hook-contract-stop-v1", "last_assistant_message": marker }),
        )),
        &[],
    );
    let terminal = repo.run(
        &["hooks", "claude", "session-end"],
        Some(&repo.envelope(
            "SessionEnd",
            claude,
            json!({ "event_id": "hook-contract-end-v1", "prompt": marker }),
        )),
        &[],
    );
    restore_checkpoint_object_directory(&repo, blocker);

    assert_eq!(
        (
            advisory.status.code(),
            advisory.stdout.as_slice(),
            advisory.stderr.as_slice()
        ),
        (Some(0), &b""[..], &b""[..]),
        "Codex advisory nonterminal contract: {}",
        describe(&advisory)
    );
    // The acknowledgement above must stand for a real capture failure.
    let failed = repo.session_show(&format!("codex__{codex}"));
    assert_eq!(failed["capture_status"], json!("retryable"), "{failed}");
    assert_eq!(
        failed["capture_error_code"],
        json!("checkpoint_write_failed"),
        "{failed}"
    );

    assert_eq!(
        terminal.status.code(),
        Some(128),
        "terminal failure exit code: {}",
        describe(&terminal)
    );
    assert!(
        terminal.stdout.is_empty(),
        "terminal failure must keep stdout empty: {}",
        describe(&terminal)
    );
    assert_eq!(
        String::from_utf8(terminal.stderr.clone()).expect("hook stderr is utf-8"),
        TERMINAL_FAILURE_STDERR,
        "terminal failure stderr bytes: {}",
        describe(&terminal)
    );
    assert_ne!(
        repo.session_show(&format!("claude__{claude}"))["state"],
        json!("stopped"),
        "the failed terminal delivery must not publish `stopped`"
    );
    assert!(
        repo.checkpoints().is_empty(),
        "neither failed delivery may leave a checkpoint"
    );
}
