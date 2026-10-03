//! AG-20 `libra agent doctor [--repair]` three-class detection/repair
//! contract (plan.md Task A5, `agent.md` doctor repair matrix).
//!
//! Drives the built `libra` binary end-to-end: `libra init` in a tempdir,
//! real checkpoint ingestion through `libra agent hooks claude-code …`,
//! then direct DB/object-store manipulation to fabricate each inconsistency
//! class, and finally `libra agent doctor [--json] [--repair]` assertions:
//!
//! - **class 2** (`missing_catalog_row`): DELETE the `agent_checkpoint` row
//!   of a ref-reachable checkpoint → detected; `--repair` re-INSERTs it with
//!   the original key fields (reconstructed from the commit's metadata.json
//!   + `Libra-*` trailers); a second run is clean (idempotent).
//! - **class 1** (`stale_catalog_row` / `missing_objects`): corrupting the
//!   row's OID columns → repaired back from `refs/libra/traces`; deleting
//!   the objects themselves (ref gone) → `missing_objects`, manual only,
//!   row untouched by `--repair`. E4 sidecar coverage: deleting a single
//!   sidecar blob (`redaction_report.json`) or the `manifest.json` blob
//!   itself → `missing_objects` naming the sidecar (never `legacy-v1`),
//!   with the remaining sidecars still checked, and a healthy report only
//!   after the object returns.
//! - **class 3** (`missing_object_index`): DELETE the `object_index` rows
//!   of checkpoint objects → `--repair` re-inserts them idempotently with
//!   the writer's row semantics (payload size is proven by bounded streaming
//!   validation of the descriptor-pinned content-addressed object, never by
//!   a manifest `byte_len`; o_type commit/tree/blob and `agent_transcript`
//!   for the transcript blob), compared against the writer-enqueued baseline rows — for the
//!   row-column OIDs and for sidecar blobs alike. A checkpoint with BOTH a
//!   stale row and missing index rows is fully fixed by one `--repair`
//!   run (auto-repairable class-1 findings do not suppress class 3).
//! - **legacy-v1**: the committed fixture
//!   (`tests/fixtures/agent_checkpoints/v1_claude_code`) seeded as a real
//!   traces commit is classified `legacy_v1_checkpoints`, never enters the
//!   three classes, and is byte-identical after `--repair`.
//! - **orphan rule fidelity**: session-without-checkpoint is legal and
//!   never flagged.
//! - **gemini**: leftover hook config yields the uninstall-channel hint;
//!   captured gemini rows are read-only data and never flagged.
//! - **span coverage**: `--repair` emits `agent.doctor.repair` with the §6
//!   required fields (asserted via `LIBRA_LOG_FILE`); detection-only runs
//!   emit none, and no transcript content ever reaches the sink.

#![cfg(unix)]

use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use git_internal::{
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        tree::{Tree, TreeItem, TreeItemMode},
    },
};
use libra::internal::ai::{
    capture::{
        ingress::lower_in_process_capture_frame_for_test,
        test_support::ingest_agent_traces_ingress_outcome_for_test,
    },
    hooks::{LifecycleEventKind, ProviderHookCommand, claude_provider},
    traces::{TracesInflightMarker, write_traces_inflight_marker},
};
use ring::{digest, hmac};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{Value, json};
use serial_test::serial;
use uuid::Uuid;

async fn ingest_agent_traces_payload(
    payload: &[u8],
    command: ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn libra::internal::ai::hooks::HookProvider,
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

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One isolated libra repository plus a fake `$HOME` for the Claude Code
/// transcript root (`~/.claude`). Mirrors `tests/agent_lifecycle_event_test.rs`.
struct DoctorRepo {
    _tempdir: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

/// The in-process fixture must use the same provider root as the child hook
/// process. This is a test-only environment adaptation, kept RAII-scoped so
/// a failed assertion cannot leak it into an unrelated test.
struct ClaudeHomeGuard(Option<OsString>);

impl ClaudeHomeGuard {
    fn install(home: &Path) -> Self {
        let prior = std::env::var_os("LIBRA_TEST_HOME");
        // SAFETY: the sole caller is serialised on the process-wide env lane
        // and `Drop` restores the prior value before the test returns.
        unsafe {
            std::env::set_var("LIBRA_TEST_HOME", home);
        }
        Self(prior)
    }
}

impl Drop for ClaudeHomeGuard {
    fn drop(&mut self) {
        // SAFETY: restores the variable installed by `ClaudeHomeGuard`.
        unsafe {
            match self.0.take() {
                Some(value) => std::env::set_var("LIBRA_TEST_HOME", value),
                None => std::env::remove_var("LIBRA_TEST_HOME"),
            }
        }
    }
}

impl DoctorRepo {
    fn init() -> Self {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let home = tempdir.path().join("home");
        let repo = tempdir.path().join("repo");
        std::fs::create_dir_all(&home).expect("create fake home");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        let this = Self {
            _tempdir: tempdir,
            // Hook ingress canonicalizes its current working directory
            // before resolving provider sources. Keep the test's repository
            // and fake home canonical too: on macOS `/var` and `/private/var`
            // otherwise produce different Claude project identities.
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
    /// environment (plus optional extra env vars, e.g. LIBRA_LOG*).
    fn run_env(&self, args: &[&str], stdin: Option<&str>, envs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .env("LIBRA_COMMITTER_NAME", "Doctor Test")
            .env("LIBRA_COMMITTER_EMAIL", "doctor@test.libra")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in envs {
            cmd.env(key, value);
        }
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

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        self.run_env(args, stdin, &[])
    }

    /// `libra agent hooks <agent> <verb>` with `envelope` piped via stdin.
    fn hook(&self, agent: &str, verb: &str, envelope: &str) -> Output {
        self.run(&["agent", "hooks", agent, verb], Some(envelope))
    }

    fn envelope(&self, hook_event_name: &str, session_id: &str, transcript: &Path) -> String {
        json!({
            "hook_event_name": hook_event_name,
            "session_id": session_id,
            "cwd": self.repo.to_string_lossy(),
            "transcript_path": transcript.to_string_lossy(),
        })
        .to_string()
    }

    /// Write a Claude Code transcript under the fake home so the writer's
    /// provider-root trust gate accepts it.
    fn write_claude_transcript(&self, content: &str) -> PathBuf {
        let dir = self.home.join(".claude").join("projects").join("x");
        std::fs::create_dir_all(&dir).expect("create ~/.claude transcript dir");
        let path = dir.join("transcript.jsonl");
        std::fs::write(&path, content).expect("write transcript fixture");
        path
    }

    /// Create the provider-discoverable Claude source selected from the
    /// verified repository cwd and provider session id. The hook deliberately
    /// ignores the envelope's transcript pointer, so source-fence tests must
    /// use this canonical layout rather than the generic fixture above.
    fn write_claude_session_transcript(&self, session_id: &str, content: &str) -> PathBuf {
        let slug: String = self
            .repo
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
        std::fs::create_dir_all(&dir).expect("create discovered Claude transcript dir");
        let path = dir.join(format!("{session_id}.jsonl"));
        std::fs::write(&path, content).expect("write discovered Claude transcript fixture");
        path
    }

    /// SessionStart + Stop for one provider session id — ingests exactly
    /// one committed checkpoint through the real writer.
    fn ingest_checkpoint(&self, session: &str, transcript_content: &str) {
        let transcript = self.write_claude_transcript(transcript_content);
        let out = self.hook(
            "claude-code",
            "session-start",
            &self.envelope("SessionStart", session, &transcript),
        );
        assert!(out.status.success(), "session-start: {}", describe(&out));
        let out = self.hook(
            "claude-code",
            "stop",
            &self.envelope("Stop", session, &transcript),
        );
        assert!(out.status.success(), "stop: {}", describe(&out));
    }

    /// Fresh sea-orm connection to the repo database. Callers drop it
    /// before the next CLI invocation (sequential access only).
    async fn db(&self) -> DatabaseConnection {
        let db_url = format!(
            "sqlite://{}",
            self.repo.join(".libra").join("libra.db").display()
        );
        let mut opts = sea_orm::ConnectOptions::new(db_url);
        opts.sqlx_logging(false);
        sea_orm::Database::connect(opts)
            .await
            .expect("open repo db")
    }

    async fn exec_sql(&self, sql: &str, values: Vec<sea_orm::Value>) {
        let conn = self.db().await;
        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_sql_and_values(backend, sql, values))
            .await
            .expect("execute test SQL");
    }

    /// All `agent_checkpoint` rows, oldest first.
    async fn checkpoint_rows(&self) -> Vec<RowSnapshot> {
        let conn = self.db().await;
        let backend = conn.get_database_backend();
        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT checkpoint_id, session_id, scope, parent_commit, tree_oid, \
                        metadata_blob_oid, traces_commit, created_at \
                 FROM agent_checkpoint ORDER BY created_at ASC, checkpoint_id ASC",
                [],
            ))
            .await
            .expect("query agent_checkpoint");
        rows.into_iter()
            .map(|row| RowSnapshot {
                checkpoint_id: row.try_get_by("checkpoint_id").unwrap(),
                session_id: row.try_get_by("session_id").unwrap(),
                scope: row.try_get_by("scope").unwrap(),
                parent_commit: row.try_get_by("parent_commit").ok().flatten(),
                tree_oid: row.try_get_by("tree_oid").unwrap(),
                metadata_blob_oid: row.try_get_by("metadata_blob_oid").unwrap(),
                traces_commit: row.try_get_by("traces_commit").unwrap(),
                created_at: row.try_get_by("created_at").unwrap(),
            })
            .collect()
    }

    /// `object_index` rows for the given OIDs as `(o_id, o_type, o_size,
    /// repo_id)`, ordered by o_id.
    async fn object_index_rows(&self, oids: &[&str]) -> Vec<(String, String, i64, String)> {
        let conn = self.db().await;
        let backend = conn.get_database_backend();
        let placeholders = vec!["?"; oids.len()].join(", ");
        let sql = format!(
            "SELECT o_id, o_type, o_size, repo_id FROM object_index \
             WHERE o_id IN ({placeholders}) ORDER BY o_id ASC"
        );
        let values: Vec<sea_orm::Value> = oids.iter().map(|oid| (*oid).into()).collect();
        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(backend, sql, values))
            .await
            .expect("query object_index");
        rows.into_iter()
            .map(|row| {
                (
                    row.try_get_by("o_id").unwrap(),
                    row.try_get_by("o_type").unwrap(),
                    row.try_get_by("o_size").unwrap(),
                    row.try_get_by("repo_id").unwrap(),
                )
            })
            .collect()
    }

    /// Run `libra agent doctor [--repair] --json` and return the `data`
    /// object of the CLI envelope.
    fn doctor_json(&self, repair: bool) -> Value {
        let mut args = vec!["agent", "doctor"];
        if repair {
            args.push("--repair");
        }
        args.push("--json");
        let out = self.run(&args, None);
        assert!(out.status.success(), "doctor failed: {}", describe(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let parsed: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|err| panic!("doctor stdout is not JSON ({err}): {stdout}"));
        assert_eq!(parsed["ok"], json!(true), "envelope not ok: {parsed}");
        parsed["data"].clone()
    }

    /// Read the durable session state through the public CLI surface, rather
    /// than coupling recovery assertions to the private receipt ledger.
    fn session_show(&self, session: &str) -> Value {
        let out = self.run(&["agent", "session", "show", session, "--json"], None);
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

    /// Bytes of one loose object file in the repo store.
    fn loose_object_bytes(&self, oid: &str) -> Vec<u8> {
        let path = self
            .repo
            .join(".libra")
            .join("objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read loose object {oid}: {e}"))
    }
}

/// Drive the same typed hook runtime that the CLI uses, but deliberately
/// without a repository checkpoint store. This creates a real durable
/// terminal receipt with no checkpoint, which is the only valid way to
/// exercise doctor’s pending/quarantine recovery path without fabricating the
/// private receipt ledger.
async fn persist_no_repository_terminal_receipt(repo: &DoctorRepo, session: &str) {
    persist_no_repository_terminal_receipt_with_transcript(repo, session, TRANSCRIPT).await;
}

/// The variant used by source-fence recovery tests: the initial no-repository
/// receipt has the same authorized fixture bytes whose digest is later bound
/// into its terminal attempt.
async fn persist_no_repository_terminal_receipt_with_transcript(
    repo: &DoctorRepo,
    session: &str,
    transcript_content: &str,
) {
    let transcript = repo.write_claude_session_transcript(session, transcript_content);
    let conn = repo.db().await;
    ingest_agent_traces_payload(
        repo.envelope("SessionStart", session, &transcript)
            .as_bytes(),
        ProviderHookCommand::SessionStart,
        LifecycleEventKind::SessionStart,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("no-repository session start succeeds");

    let mut terminal: Value =
        serde_json::from_str(&repo.envelope("SessionEnd", session, &transcript))
            .expect("hook envelope is JSON");
    terminal["event_id"] = json!(format!("{session}-no-repository-terminal-v1"));
    let terminal = terminal.to_string();
    ingest_agent_traces_payload(
        terminal.as_bytes(),
        ProviderHookCommand::SessionEnd,
        LifecycleEventKind::SessionEnd,
        claude_provider(),
        &conn,
        None,
    )
    .await
    .expect("no-repository terminal capture persists a pending finalizer receipt");
}

/// Bind the real pending no-repository terminal receipt to an expired exact
/// marker. This models the narrow crash window after catalog election and
/// marker registration, before the checkpoint writer can publish a ref. The
/// fixture changes only durable, content-free fence metadata; it never
/// fabricates a checkpoint or a source payload.
async fn bind_expired_terminal_marker(
    repo: &DoctorRepo,
    provider_session_id: &str,
    event_id: Uuid,
    receipt_key: &str,
    source_digest: &str,
) -> (String, String, String, String) {
    let conn = repo.db().await;
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT session_id, metadata_json FROM agent_session WHERE provider_session_id = ?",
            [provider_session_id.into()],
        ))
        .await
        .expect("read pending terminal receipt")
        .expect("pending terminal session exists");
    let session_id: String = row
        .try_get_by("session_id")
        .expect("decode pending terminal session id");
    let metadata_json: String = row
        .try_get_by("metadata_json")
        .expect("decode pending terminal metadata");
    let mut metadata: Value =
        serde_json::from_str(&metadata_json).expect("pending terminal metadata is JSON");
    let receipt = metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array_mut()
        .and_then(|entries| {
            entries
                .iter_mut()
                .find(|entry| entry["intent"]["phase"] == json!("stopped"))
        })
        .expect("typed runtime created one deferred terminal receipt");
    receipt["event_id"] = json!(event_id);
    receipt["action_key"] = json!(format!("capture-lifecycle-v1:{event_id}"));
    receipt["receipt_key"] = json!(receipt_key);
    let generation = Uuid::new_v4().to_string();
    let finalizer = receipt["finalizer"]
        .as_object_mut()
        .expect("no-repository terminal receipt has a finalizer");
    assert!(
        source_digest
            .strip_prefix("source/hmac-v2/")
            .is_some_and(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            }),
        "fixture source fence must be a repository-keyed HMAC v2"
    );
    let source_digest = source_digest.to_string();
    finalizer.insert("source_digest".to_string(), json!(&source_digest));
    finalizer.insert("marker_generation".to_string(), json!(generation));
    finalizer.insert(
        "replay_key".to_string(),
        json!(format!("capture-lifecycle-v1:{event_id}")),
    );
    finalizer.insert("stage".to_string(), json!("marker"));
    finalizer.insert("status".to_string(), json!("pending"));
    finalizer.remove("quarantine_reason");

    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_session SET metadata_json = ? WHERE provider_session_id = ?",
        [
            serde_json::to_string(&metadata)
                .expect("serialize bound terminal marker receipt")
                .into(),
            provider_session_id.into(),
        ],
    ))
    .await
    .expect("persist bound terminal marker receipt");

    let checkpoint_id = committed_capture_checkpoint_id(event_id);
    let mut marker = TracesInflightMarker::new(&session_id, &checkpoint_id, 0);
    marker.generation = Some(generation.clone());
    marker.ttl_ms = 0;
    write_traces_inflight_marker(&conn, &marker)
        .await
        .expect("persist expired terminal marker");
    (session_id, checkpoint_id, generation, source_digest)
}

/// The no-repository fixture deliberately cannot access a repository-private
/// key, so its real pending receipt has no source fence. Model the narrow
/// post-election crash window with the same scoped snapshot-content HMAC the
/// normal runtime would have written before the checkpoint backend vanished.
/// Runtime source-binding itself is covered by the source-fence hook tests;
/// this helper keeps the doctor test focused on recovery of valid durable
/// evidence.
fn fixture_snapshot_source_commitment(repo: &DoctorRepo, source: &str) -> String {
    let secret = std::fs::read(
        repo.repo
            .join(".libra")
            .join("private")
            .join("agent-capture-dedup-v1.key"),
    )
    .expect("read repository-private capture dedup key");
    let secret: [u8; 32] = secret
        .try_into()
        .expect("capture dedup key has fixed 32-byte length");
    let redacted = libra::internal::ai::observed_agents::Redactor::new_default()
        .redact(source.as_bytes())
        .0;
    let preimage = digest::digest(&digest::SHA256, redacted.bytes());
    let mut commitment = hmac::Context::with_key(&hmac::Key::new(hmac::HMAC_SHA256, &secret));
    commitment.update(b"libra-agent-snapshot-content-hmac-v2\0");
    commitment.update(preimage.as_ref());
    format!("source/hmac-v2/{}", hex::encode(commitment.sign().as_ref()))
}

/// Reconstruct the external hook's opaque native-event identity from the
/// repository-private key so the in-process crash fixture and the later CLI
/// replay address the *same* receipt. This copies the documented current
/// ingress HMAC shape, not any transcript content.
fn cli_claude_terminal_identity(
    repo: &DoctorRepo,
    provider_session_id: &str,
    native_event_id: &str,
) -> (Uuid, String) {
    let secret = std::fs::read(
        repo.repo
            .join(".libra")
            .join("private")
            .join("agent-capture-dedup-v1.key"),
    )
    .expect("read repository-private capture dedup key");
    let secret: [u8; 32] = secret
        .try_into()
        .expect("capture dedup key has fixed 32-byte length");
    let mut preimage = digest::Context::new(&digest::SHA256);
    preimage.update(b"libra-capture-ingress-native-preimage-v1\0");
    let native_component = format!("string:{native_event_id}");
    for value in [
        "claude",
        "SessionEnd",
        provider_session_id,
        "event_id",
        native_component.as_str(),
    ] {
        preimage.update(&(value.len() as u64).to_be_bytes());
        preimage.update(value.as_bytes());
    }
    let preimage = preimage.finish();
    let mut hmac = hmac::Context::with_key(&hmac::Key::new(hmac::HMAC_SHA256, &secret));
    hmac.update(b"libra-capture-ingress-dedup-v2\0");
    hmac.update(preimage.as_ref());
    let mac = hmac.sign();
    let mac_bytes = mac.as_ref();
    let mut event_bytes = [0u8; 16];
    event_bytes.copy_from_slice(&mac_bytes[..16]);
    event_bytes[6] = (event_bytes[6] & 0x0f) | 0x50;
    event_bytes[8] = (event_bytes[8] & 0x3f) | 0x80;
    (
        Uuid::from_bytes(event_bytes),
        format!("capture-dedup-v2:{}", hex::encode(mac_bytes)),
    )
}

/// `capture::test_support` intentionally uses the connection's main scope,
/// while the external CLI resolves the repository's verified worktree scope.
/// Make the fixture receipt belong to that real CLI scope before replaying it,
/// so this test exercises terminal source fencing rather than a deliberate
/// cross-scope rejection.
async fn align_pending_session_with_cli_scope(
    repo: &DoctorRepo,
    provider_session_id: &str,
    transcript_content: &str,
) {
    let probe_session = format!("{provider_session_id}-scope-probe");
    let transcript = repo.write_claude_session_transcript(&probe_session, transcript_content);
    let output = repo.hook(
        "claude-code",
        "session-start",
        &repo.envelope("SessionStart", &probe_session, &transcript),
    );
    assert!(
        output.status.success(),
        "create real CLI scope probe: {}",
        describe(&output)
    );
    let conn = repo.db().await;
    let scope = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT repo_id, worktree_id, workspace_id, workspace_fence
             FROM agent_session WHERE provider_session_id = ?",
            [probe_session.into()],
        ))
        .await
        .expect("read CLI capture scope probe")
        .expect("CLI scope probe row exists");
    let repo_id: String = scope.try_get_by("repo_id").expect("decode CLI repo id");
    let worktree_id: String = scope
        .try_get_by("worktree_id")
        .expect("decode CLI worktree id");
    let workspace_id: Option<String> = scope
        .try_get_by("workspace_id")
        .expect("decode CLI workspace id");
    let workspace_fence: Option<i64> = scope
        .try_get_by("workspace_fence")
        .expect("decode CLI workspace fence");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_session
         SET working_dir = ?, scope_state = 'scoped', repo_id = ?, worktree_id = ?,
             workspace_id = ?, workspace_fence = ?
         WHERE provider_session_id = ?",
        [
            repo.repo.to_string_lossy().to_string().into(),
            repo_id.into(),
            worktree_id.into(),
            workspace_id.into(),
            workspace_fence.into(),
            provider_session_id.into(),
        ],
    ))
    .await
    .expect("align pending terminal receipt with CLI capture scope");
}

/// Keep the integration fixture aligned with the capture boundary's stable
/// committed-checkpoint name without making its crate-private helper public.
fn committed_capture_checkpoint_id(event_id: Uuid) -> String {
    const NAMESPACE: Uuid = Uuid::from_bytes([
        0x46, 0x10, 0x71, 0x1d, 0x6e, 0x76, 0x49, 0x2f, 0xa1, 0x24, 0x11, 0x98, 0x52, 0x4d, 0x2c,
        0x87,
    ]);
    let mut name = Vec::with_capacity(48);
    name.extend_from_slice(b"libra-capture-checkpoint-v1\0committed\0");
    name.extend_from_slice(event_id.as_bytes());
    Uuid::new_v5(&NAMESPACE, &name).to_string()
}

/// Age the timestamp on a receipt that was first created by the real typed
/// runtime. This is a fixture-level database inconsistency, not a hook-host
/// clock override: the test preserves every identity/fence field and changes
/// only the historical fact doctor must classify.
async fn age_existing_pending_finalizer_receipt(
    repo: &DoctorRepo,
    session: &str,
    first_attempt_millis: i64,
) {
    let conn = repo.db().await;
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT metadata_json FROM agent_session WHERE provider_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("read terminal receipt metadata")
        .expect("terminal session exists");
    let metadata_json: String = row
        .try_get_by("metadata_json")
        .expect("decode terminal receipt metadata");
    let mut metadata: Value =
        serde_json::from_str(&metadata_json).expect("terminal receipt metadata is JSON");
    let finalizer = metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array_mut()
        .and_then(|entries| entries.first_mut())
        .and_then(|entry| entry.get_mut("finalizer"))
        .and_then(Value::as_object_mut)
        .expect("typed no-repository terminal created one pending finalizer");
    finalizer.insert(
        "first_attempt_millis".to_string(),
        json!(first_attempt_millis),
    );
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_session SET metadata_json = ? WHERE provider_session_id = ?",
        [
            serde_json::to_string(&metadata)
                .expect("serialize aged terminal receipt metadata")
                .into(),
            session.into(),
        ],
    ))
    .await
    .expect("persist aged terminal receipt metadata");
}

#[derive(Debug, Clone, PartialEq)]
struct RowSnapshot {
    checkpoint_id: String,
    session_id: String,
    scope: String,
    parent_commit: Option<String>,
    tree_oid: String,
    metadata_blob_oid: String,
    traces_commit: String,
    created_at: i64,
}

fn describe(out: &Output) -> String {
    format!(
        "status: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

fn findings(report: &Value) -> Vec<Value> {
    report["checkpoint_store"]["findings"]
        .as_array()
        .unwrap_or_else(|| panic!("checkpoint_store.findings missing: {report}"))
        .clone()
}

fn assert_store_clean(report: &Value) {
    assert_eq!(
        findings(report).len(),
        0,
        "expected a clean checkpoint store: {report}"
    );
    assert_eq!(report["checkpoint_store"]["repaired"], json!(0));
    assert_eq!(report["checkpoint_store"]["manual_required"], json!(0));
}

const TRANSCRIPT: &str =
    "{\"type\":\"user\",\"text\":\"hello doctor\"}\n{\"type\":\"assistant\",\"text\":\"done\"}\n";

#[tokio::test]
async fn doctor_reports_and_repairs_expired_empty_inflight_marker() {
    let repo = DoctorRepo::init();
    repo.exec_sql(
        "INSERT INTO metadata_kv (
            scope, target, `key`, value, value_type, created_at, updated_at
         ) VALUES ('agent_traces_inflight', ?, ?, ?, 'text', '0', '0')",
        vec![
            "doctor-session".into(),
            "doctor-expired-attempt".into(),
            json!({
                "schema_version": 1,
                "session_id": "doctor-session",
                "attempt_id": "doctor-expired-attempt",
                "started_at_ms": 0,
                "ttl_ms": 0
            })
            .to_string()
            .into(),
        ],
    )
    .await;

    let detected = repo.doctor_json(false);
    let finding = findings(&detected)
        .into_iter()
        .find(|finding| finding["inconsistency_type"] == "expired_inflight_marker")
        .expect("expired marker finding");
    assert_eq!(finding["manual_required"], json!(false));
    assert_eq!(finding["repaired"], json!(false));

    let repaired = repo.doctor_json(true);
    let finding = findings(&repaired)
        .into_iter()
        .find(|finding| finding["inconsistency_type"] == "expired_inflight_marker")
        .expect("expired marker repair finding");
    assert_eq!(finding["repaired"], json!(true));
    assert_store_clean(&repo.doctor_json(false));
}

/// An expired marker repair can be blocked by another live writer. Its
/// session and attempt are private writer identities, so neither doctor
/// output format may render the guard error that carries them.
#[tokio::test]
async fn expired_marker_repair_redacts_live_writer_identity_in_human_and_json_output() {
    const LIVE_SESSION: &str = "live-provider-session-secret-doctor-output";
    const LIVE_ATTEMPT: &str = "live-attempt-secret-doctor-output";
    const RETRY_GUIDANCE: &str =
        "reachability repair failed closed; retry after active writers finish or rerun doctor";

    let repo = DoctorRepo::init();
    let conn = repo.db().await;
    let expired = TracesInflightMarker::new("expired-doctor-session", "expired-attempt", 0);
    let live = TracesInflightMarker::new(
        LIVE_SESSION,
        LIVE_ATTEMPT,
        chrono::Utc::now().timestamp_millis(),
    );
    write_traces_inflight_marker(&conn, &expired)
        .await
        .expect("write expired marker");
    write_traces_inflight_marker(&conn, &live)
        .await
        .expect("write live marker");
    drop(conn);

    let human = repo.run(&["agent", "doctor", "--repair"], None);
    assert!(human.status.success(), "doctor: {}", describe(&human));
    let human_output = format!(
        "{}{}",
        String::from_utf8_lossy(&human.stdout),
        String::from_utf8_lossy(&human.stderr)
    );
    assert!(
        human_output.contains(RETRY_GUIDANCE),
        "human report must give safe retry guidance: {human_output}"
    );
    for private_identity in [LIVE_SESSION, LIVE_ATTEMPT] {
        assert!(
            !human_output.contains(private_identity),
            "human report leaked live writer identity: {human_output}"
        );
    }

    let json = repo.run(&["agent", "doctor", "--repair", "--json"], None);
    assert!(json.status.success(), "doctor JSON: {}", describe(&json));
    let json_output = format!(
        "{}{}",
        String::from_utf8_lossy(&json.stdout),
        String::from_utf8_lossy(&json.stderr)
    );
    assert!(
        json_output.contains(RETRY_GUIDANCE),
        "JSON report must give safe retry guidance: {json_output}"
    );
    for private_identity in [LIVE_SESSION, LIVE_ATTEMPT] {
        assert!(
            !json_output.contains(private_identity),
            "JSON report leaked live writer identity: {json_output}"
        );
    }
    let report: Value = serde_json::from_str(String::from_utf8_lossy(&json.stdout).trim())
        .expect("doctor JSON output parses");
    let finding = report["data"]["checkpoint_store"]["findings"]
        .as_array()
        .and_then(|findings| {
            findings
                .iter()
                .find(|finding| finding["inconsistency_type"] == "expired_inflight_marker")
        })
        .expect("expired marker finding present");
    assert_eq!(finding["manual_required"], json!(true));
    assert_eq!(finding["repaired"], json!(false));
    assert!(
        finding["detail"]
            .as_str()
            .is_some_and(|detail| detail.ends_with(RETRY_GUIDANCE)),
        "serialized finding must retain safe retry guidance: {finding}"
    );
}

/// An expired terminal marker is recovery evidence, not authority to bind a
/// later transcript to the earlier receipt. Doctor may retire the stale
/// marker, but a replay whose source differs from the elected source must
/// remain repair-required: it must not publish a mixed checkpoint/ref or a
/// false terminal state.
#[tokio::test]
#[serial(env)]
async fn doctor_repair_never_rebinds_an_expired_terminal_marker_to_changed_source() {
    let repo = DoctorRepo::init();
    let _claude_home = ClaudeHomeGuard::install(&repo.home);
    let session = "sess-doctor-expired-terminal-source-fence";
    let native_event_id = format!("{session}-no-repository-terminal-v1");
    let source_x = concat!(
        r#"{"type":"user","text":"terminal source x"}"#,
        "\n",
        r#"{"type":"assistant","text":"first durable source"}"#,
        "\n"
    );
    let source_y = concat!(
        r#"{"type":"user","text":"terminal source y"}"#,
        "\n",
        r#"{"type":"assistant","text":"later incompatible source"}"#,
        "\n"
    );
    assert_ne!(source_x, source_y, "fixture must exercise distinct sources");

    // The receipt comes from the real hook runtime. The fixture then records
    // an exact, already-expired marker as if the elected X writer crashed
    // immediately after registration and before object/ref publication.
    persist_no_repository_terminal_receipt_with_transcript(&repo, session, source_x).await;
    align_pending_session_with_cli_scope(&repo, session, source_x).await;
    let (event_id, receipt_key) = cli_claude_terminal_identity(&repo, session, &native_event_id);
    let source_x_digest = fixture_snapshot_source_commitment(&repo, source_x);
    let (session_id, checkpoint_id, generation, source_x_digest) =
        bind_expired_terminal_marker(&repo, session, event_id, &receipt_key, &source_x_digest)
            .await;

    let detected = repo.doctor_json(false);
    let finding = findings(&detected)
        .into_iter()
        .find(|finding| finding["inconsistency_type"] == "expired_inflight_marker")
        .expect("expired terminal marker finding");
    assert_eq!(finding["repaired"], json!(false));
    assert_eq!(finding["manual_required"], json!(false));

    let repaired = repo.doctor_json(true);
    let finding = findings(&repaired)
        .into_iter()
        .find(|finding| finding["inconsistency_type"] == "expired_inflight_marker")
        .expect("expired terminal marker repair finding");
    assert_eq!(finding["repaired"], json!(true));
    assert_eq!(finding["manual_required"], json!(false));
    let conn = repo.db().await;
    let marker_count = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS count FROM metadata_kv
             WHERE scope = 'agent_traces_inflight' AND target = ? AND `key` = ?",
            [session_id.clone().into(), checkpoint_id.clone().into()],
        ))
        .await
        .expect("read retired terminal marker")
        .expect("marker count row");
    assert_eq!(
        marker_count
            .try_get_by::<i64, _>("count")
            .expect("decode retired marker count"),
        0,
        "doctor repair must retire the exact stale marker rather than replace it"
    );
    drop(conn);

    // A later native replay has the same event identity but a different
    // authorized source. It must quarantine explicitly instead of using the
    // repaired marker slot to rebind Y under X's receipt/fence.
    let transcript = repo.write_claude_session_transcript(session, source_y);
    let mut terminal: Value =
        serde_json::from_str(&repo.envelope("SessionEnd", session, &transcript))
            .expect("terminal hook envelope is JSON");
    terminal["event_id"] = json!(native_event_id);
    let output = repo.hook("claude-code", "session-end", &terminal.to_string());
    assert!(
        output.status.success(),
        "Claude hook must acknowledge a durable repair-required result: {}",
        describe(&output)
    );

    let conn = repo.db().await;
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT state, stopped_at, metadata_json,
                    (SELECT COUNT(*) FROM agent_checkpoint
                     WHERE session_id = agent_session.session_id) AS checkpoints,
                    (SELECT COUNT(*) FROM metadata_kv
                     WHERE scope = 'agent_traces_inflight'
                       AND target = agent_session.session_id) AS markers,
                    (SELECT COUNT(*) FROM reference
                     WHERE name = ? AND kind = 'Branch' AND remote IS NULL
                       AND \"commit\" IS NOT NULL) AS trace_heads
             FROM agent_session WHERE provider_session_id = ?",
            [
                libra::internal::branch::TRACES_BRANCH.into(),
                session.into(),
            ],
        ))
        .await
        .expect("read source-conflict terminal result")
        .expect("terminal session remains durable");
    let metadata: Value = serde_json::from_str(
        &row.try_get_by::<String, _>("metadata_json")
            .expect("decode source-conflict metadata"),
    )
    .expect("source-conflict metadata is JSON");
    assert_eq!(
        row.try_get_by::<String, _>("state")
            .expect("decode source-conflict state"),
        "quarantined",
        "changed-source replay must quarantine the retired exact attempt: {metadata}\n{}",
        describe(&output)
    );
    assert!(
        row.try_get_by::<Option<i64>, _>("stopped_at")
            .expect("decode source-conflict stopped_at")
            .is_none(),
        "a changed-source replay must not publish terminal success"
    );
    assert_eq!(
        row.try_get_by::<i64, _>("checkpoints")
            .expect("decode source-conflict checkpoint count"),
        0,
        "repair must not publish a checkpoint with a mixed source fence"
    );
    assert_eq!(
        row.try_get_by::<i64, _>("markers")
            .expect("decode source-conflict marker count"),
        0,
        "changed-source replay must not recreate/rebind the retired marker"
    );
    assert_eq!(
        row.try_get_by::<i64, _>("trace_heads")
            .expect("decode source-conflict trace-head count"),
        0,
        "changed-source replay must not advance refs/libra/traces"
    );
    let finalizer = &metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["intent"]["phase"] == json!("stopped"))
        })
        .expect("terminal receipt remains present")["finalizer"];
    assert_eq!(finalizer["marker_generation"], json!(generation));
    assert_eq!(finalizer["source_digest"], json!(source_x_digest));
    assert_eq!(finalizer["status"], json!("quarantined"));
    assert_eq!(
        finalizer["quarantine_reason"],
        json!("source_digest_conflict"),
        "the changed source is explicit manual recovery evidence"
    );
}

#[tokio::test]
async fn foreign_scoped_pending_session_does_not_abort_current_repo_diagnosis() {
    let repo = DoctorRepo::init();
    let foreign_session = "sess-doctor-foreign-repo-pending-finalizer";
    let current_session = "sess-doctor-current-repo-pending-finalizer";
    persist_no_repository_terminal_receipt(&repo, foreign_session).await;
    persist_no_repository_terminal_receipt(&repo, current_session).await;

    let conn = repo.db().await;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_session SET repo_id = ? WHERE provider_session_id = ?",
        [
            "foreign-repository-identity".into(),
            format!("claude__{foreign_session}").into(),
        ],
    ))
    .await
    .expect("mark one scoped legacy session as belonging to a different repo identity");
    drop(conn);

    let report = repo.doctor_json(false);
    let finalizer_findings = findings(&report)
        .into_iter()
        .filter(|finding| finding["inconsistency_type"] == "expired_inflight_marker")
        .count();
    assert_eq!(
        finalizer_findings, 2,
        "a foreign scoped receipt must not hide current-repository pending finalizers: {report}"
    );
}

/// Doctor leaves a fresh no-checkpoint terminal receipt as manual pending
/// evidence, then quarantines it only after its persisted finalizer window
/// expires. Both fixtures first drive the typed runtime, then the expired
/// fixture ages only its existing finalizer timestamp. Durable-checkpoint replay is covered by the
/// in-process coordinator fault matrix, where the test-only typed failure
/// seam is available without shipping a CLI environment switch.
#[tokio::test]
async fn pending_finalize_is_classified_and_repairable() {
    // A no-repository terminal path has no checkpoint to prove. A fresh
    // receipt therefore remains explicit manual/replayable evidence even on
    // `--repair`; doctor must neither mark it stopped nor spend its retry
    // budget merely by inspecting it.
    let fresh_repo = DoctorRepo::init();
    let fresh_session = "sess-doctor-fresh-no-checkpoint-finalizer";
    persist_no_repository_terminal_receipt(&fresh_repo, fresh_session).await;
    let fresh_session_id = format!("claude__{fresh_session}");
    assert!(
        fresh_repo.checkpoint_rows().await.is_empty(),
        "a no-repository terminal receipt must not fabricate a checkpoint"
    );
    let fresh_detected = fresh_repo.doctor_json(false);
    let fresh_finding = findings(&fresh_detected)
        .into_iter()
        .find(|finding| finding["inconsistency_type"] == "expired_inflight_marker")
        .expect("fresh no-checkpoint finalizer finding");
    assert_eq!(fresh_finding["repaired"], json!(false));
    assert_eq!(fresh_finding["manual_required"], json!(true));
    let fresh_repaired = fresh_repo.doctor_json(true);
    let fresh_repair_finding = findings(&fresh_repaired)
        .into_iter()
        .find(|finding| finding["inconsistency_type"] == "expired_inflight_marker")
        .expect("fresh no-checkpoint repair finding");
    assert_eq!(fresh_repair_finding["repaired"], json!(false));
    assert_eq!(fresh_repair_finding["manual_required"], json!(true));
    let fresh_state = fresh_repo.session_show(&fresh_session_id);
    assert_ne!(
        fresh_state["state"],
        json!("stopped"),
        "fresh no-checkpoint repair must not publish a false terminal state: {fresh_state}"
    );
    assert_ne!(
        fresh_state["state"],
        json!("quarantined"),
        "doctor inspection must not prematurely quarantine a fresh replayable receipt: {fresh_state}"
    );

    // Drive the real public lifecycle writers rather than replacing the old
    // receipt with a hand-seeded generation. Its evidence must remain visible
    // after stop/resume, while doctor leaves the newer active state untouched.
    for verb in ["stop", "resume"] {
        let out = fresh_repo.run(&["agent", "session", verb, &fresh_session_id], None);
        assert!(out.status.success(), "{verb} failed: {}", describe(&out));
    }
    let database = fresh_repo.repo.join(".libra/libra.db");
    let resumed_bytes = std::fs::read(&database).unwrap();
    for repair in [false, true] {
        let report = fresh_repo.doctor_json(repair);
        let finding = findings(&report)
            .into_iter()
            .find(|f| f["inconsistency_type"] == "expired_inflight_marker")
            .expect("superseded pending_source remains visible");
        assert_eq!(finding["repaired"], json!(false));
        assert_eq!(finding["manual_required"], json!(true));
        assert!(finding["detail"].as_str().unwrap().contains("superseded"));
        assert_eq!(std::fs::read(&database).unwrap(), resumed_bytes);
        assert_eq!(
            fresh_repo.session_show(&fresh_session_id)["state"],
            json!("active")
        );
        assert!(fresh_repo.checkpoint_rows().await.is_empty());
    }

    // The same no-checkpoint receipt becomes repair-required after the
    // persisted 30-minute finalizer window. Age the real typed receipt's
    // timestamp in the fixture database rather than exposing a runtime clock
    // switch; doctor must make the *persisted* quarantine transition rather
    // than silently succeeding or publishing `stopped`.
    let exhausted_repo = DoctorRepo::init();
    let exhausted_session = "sess-doctor-expired-no-checkpoint-finalizer";
    let expired_first_attempt = chrono::Utc::now()
        .timestamp_millis()
        .saturating_sub(1_800_001);
    persist_no_repository_terminal_receipt(&exhausted_repo, exhausted_session).await;
    age_existing_pending_finalizer_receipt(
        &exhausted_repo,
        exhausted_session,
        expired_first_attempt,
    )
    .await;
    let exhausted_session_id = format!("claude__{exhausted_session}");
    assert!(
        exhausted_repo.checkpoint_rows().await.is_empty(),
        "an exhausted no-checkpoint finalizer must still have no durable checkpoint"
    );
    let exhausted_detected = exhausted_repo.doctor_json(false);
    let exhausted_finding = findings(&exhausted_detected)
        .into_iter()
        .find(|finding| finding["inconsistency_type"] == "expired_inflight_marker")
        .expect("exhausted no-checkpoint finalizer finding");
    assert_eq!(exhausted_finding["repaired"], json!(false));
    assert_eq!(exhausted_finding["manual_required"], json!(true));
    let exhausted_repaired = exhausted_repo.doctor_json(true);
    let exhausted_repair_finding = findings(&exhausted_repaired)
        .into_iter()
        .find(|finding| finding["inconsistency_type"] == "expired_inflight_marker")
        .expect("exhausted no-checkpoint repair finding");
    assert_eq!(
        exhausted_repair_finding["repaired"],
        json!(false),
        "quarantine retains evidence but is not a completed checkpoint+receipt repair"
    );
    assert_eq!(exhausted_repair_finding["manual_required"], json!(true));
    let exhausted_state = exhausted_repo.session_show(&exhausted_session_id);
    assert_eq!(
        exhausted_state["state"],
        json!("quarantined"),
        "an expired no-checkpoint receipt must become visibly repair-required: {exhausted_state}"
    );
    assert_ne!(
        exhausted_state["state"],
        json!("stopped"),
        "quarantine must never claim a checkpoint-less terminal capture completed: {exhausted_state}"
    );
    assert!(
        exhausted_repo.checkpoint_rows().await.is_empty(),
        "doctor quarantine must not fabricate a checkpoint"
    );
    // Quarantine is evidence, not a clean/complete terminal capture. Repeat
    // repair must still expose it, without advancing the receipt again.
    let quarantined_bytes = std::fs::read(exhausted_repo.repo.join(".libra/libra.db")).unwrap();
    for repair in [false, true] {
        let report = exhausted_repo.doctor_json(repair);
        let finding = findings(&report)
            .into_iter()
            .find(|f| f["inconsistency_type"] == "expired_inflight_marker")
            .expect("quarantined finalizer stays manual-required");
        assert_eq!(finding["repaired"], json!(false));
        assert_eq!(finding["manual_required"], json!(true));
        assert_eq!(
            std::fs::read(exhausted_repo.repo.join(".libra/libra.db")).unwrap(),
            quarantined_bytes
        );
    }
}

#[tokio::test]
async fn doctor_surfaces_malformed_inflight_marker_as_manual_required() {
    let repo = DoctorRepo::init();
    repo.exec_sql(
        "INSERT INTO metadata_kv (
            scope, target, `key`, value, value_type, created_at, updated_at
         ) VALUES ('agent_traces_inflight', ?, ?, ?, 'text', '0', '0')",
        vec![
            "doctor-damaged-session".into(),
            "doctor-damaged-attempt".into(),
            "{not-json".into(),
        ],
    )
    .await;

    for repair in [false, true] {
        let report = repo.doctor_json(repair);
        let finding = findings(&report)
            .into_iter()
            .find(|finding| finding["inconsistency_type"] == "invalid_inflight_marker")
            .expect("invalid marker finding");
        assert_eq!(finding["manual_required"], json!(true));
        assert_eq!(finding["repaired"], json!(false));
        assert!(
            finding["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("automatic removal is unsafe"))
        );
        // A non-canonical key is never echoed; it gets the opaque label.
        assert!(
            finding["checkpoint_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("inflight-marker-")),
            "non-canonical marker key must use the per-report label: {finding}"
        );
    }
}

/// Marker findings must stay actionable: a canonical writer attempt id (a
/// checkpoint UUID) is reported as-is for both expired and malformed
/// markers, while a non-canonical stored key falls back to the opaque
/// per-report label and never reaches either output format.
#[tokio::test]
async fn doctor_marker_findings_report_canonical_checkpoint_ids() {
    const HOSTILE_KEY: &str = "hostile-attempt-key";

    let repo = DoctorRepo::init();
    let expired_attempt = Uuid::new_v4().to_string();
    let malformed_attempt = Uuid::new_v4().to_string();
    let conn = repo.db().await;
    write_traces_inflight_marker(
        &conn,
        &TracesInflightMarker::new("doctor-marker-session", &expired_attempt, 0),
    )
    .await
    .expect("write expired marker");
    drop(conn);
    for key in [malformed_attempt.as_str(), HOSTILE_KEY] {
        repo.exec_sql(
            "INSERT INTO metadata_kv (
                scope, target, `key`, value, value_type, created_at, updated_at
             ) VALUES ('agent_traces_inflight', ?, ?, ?, 'text', '0', '0')",
            vec![
                "doctor-marker-damaged-session".into(),
                key.into(),
                "{not-json".into(),
            ],
        )
        .await;
    }

    let report = repo.doctor_json(false);
    let all = findings(&report);
    let expired = all
        .iter()
        .find(|finding| finding["inconsistency_type"] == "expired_inflight_marker")
        .unwrap_or_else(|| panic!("expired marker finding: {report}"));
    assert_eq!(
        expired["checkpoint_id"],
        json!(expired_attempt),
        "expired marker must report its canonical attempt checkpoint id: {report}"
    );
    let invalid: Vec<&Value> = all
        .iter()
        .filter(|finding| finding["inconsistency_type"] == "invalid_inflight_marker")
        .collect();
    assert_eq!(invalid.len(), 2, "two malformed markers: {report}");
    assert!(
        invalid
            .iter()
            .any(|finding| finding["checkpoint_id"] == json!(malformed_attempt)),
        "canonical malformed marker key must be reported as-is: {report}"
    );
    assert!(
        invalid.iter().any(|finding| finding["checkpoint_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("inflight-marker-"))),
        "non-canonical malformed marker key must use the per-report label: {report}"
    );

    let human = repo.run(&["agent", "doctor"], None);
    assert!(human.status.success(), "doctor: {}", describe(&human));
    let human_output = format!(
        "{}{}",
        String::from_utf8_lossy(&human.stdout),
        String::from_utf8_lossy(&human.stderr)
    );
    assert!(
        human_output.contains(&expired_attempt) && human_output.contains(&malformed_attempt),
        "human report must name canonical marker checkpoint ids: {human_output}"
    );
    assert!(
        !human_output.contains(HOSTILE_KEY) && !report.to_string().contains(HOSTILE_KEY),
        "non-canonical marker key must never be echoed: {human_output} / {report}"
    );
}

#[tokio::test]
async fn doctor_surfaces_conflicted_coverage_claim_without_raw_identity() {
    let repo = DoctorRepo::init();
    let session = "conflict-doctor-sensitive-session";
    repo.ingest_checkpoint(session, TRANSCRIPT);
    for schema_version in [1_i64, 2] {
        repo.exec_sql(
            "INSERT INTO agent_coverage_claim (
                session_id, logical_turn_key, coverage_schema_version,
                coverage_digest, completeness, revision, state, source_channel,
                created_at, updated_at
             )
             SELECT session_id, 'doctor-conflict-turn', ?, ?, 'complete', 1,
                    'conflicted', 'import', 1, 1
             FROM agent_session WHERE provider_session_id = ?",
            vec![schema_version.into(), "a".repeat(64).into(), session.into()],
        )
        .await;
        repo.exec_sql(
            "INSERT INTO agent_coverage_conflict (
                session_id, logical_turn_key, coverage_schema_version,
                incumbent_revision, incumbent_digest, incumbent_checkpoint_id,
                incoming_digest, incoming_source_channel, incoming_observed_at,
                incoming_canonical_json, incoming_redaction_report_json
             )
             SELECT session_id, 'doctor-conflict-turn', ?, 1, ?, NULL, ?,
                    'import', ?, '[{\"role\":\"user\",\"text\":\"redacted\"}]',
                    '{\"matches\":[],\"bytes_scanned\":0,\"bytes_redacted\":0}'
             FROM agent_session WHERE provider_session_id = ?",
            vec![
                schema_version.into(),
                "a".repeat(64).into(),
                if schema_version == 1 {
                    "b".repeat(64).into()
                } else {
                    "c".repeat(64).into()
                },
                schema_version.into(),
                session.into(),
            ],
        )
        .await;
    }

    for repair in [false, true] {
        let report = repo.doctor_json(repair);
        let conflict_findings = findings(&report)
            .into_iter()
            .filter(|finding| finding["inconsistency_type"] == "conflicted_coverage_claim")
            .collect::<Vec<_>>();
        assert_eq!(conflict_findings.len(), 2);
        assert_ne!(
            conflict_findings[0]["checkpoint_id"], conflict_findings[1]["checkpoint_id"],
            "schema versions need distinct sanitized finding identities"
        );
        for (index, finding) in conflict_findings.iter().enumerate() {
            assert_eq!(finding["manual_required"], json!(true));
            assert_eq!(finding["repaired"], json!(false));
            let rendered = finding.to_string();
            assert!(
                !rendered.contains(session),
                "raw session identity leaked: {rendered}"
            );
            assert!(rendered.contains(&format!("coverage schema {}", index + 1)));
            assert!(rendered.contains("incumbent revision 1"));
            assert!(
                !rendered.contains("incoming digest="),
                "doctor must not render an incoming durable digest: {rendered}"
            );
            assert!(
                rendered.contains("incoming redacted evidence and its redaction report"),
                "doctor should retain a content-free recovery direction: {rendered}"
            );
            assert!(rendered.contains("are stored in agent_coverage_conflict"));
        }
    }
}

// ---------------------------------------------------------------------------
// Loose-object reading helpers (E4 tree navigation for sidecar tests)
// ---------------------------------------------------------------------------

/// Read + zlib-decode one loose object, returning `(type, body)`.
fn read_loose(repo: &DoctorRepo, oid: &str) -> (String, Vec<u8>) {
    let raw = repo.loose_object_bytes(oid);
    let mut decoder = flate2::read::ZlibDecoder::new(&raw[..]);
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decoded).expect("zlib decode");
    let header_end = decoded
        .iter()
        .position(|&b| b == 0)
        .expect("object header terminator");
    let header = std::str::from_utf8(&decoded[..header_end]).expect("utf8 header");
    let object_type = header
        .split(' ')
        .next()
        .expect("object type in header")
        .to_string();
    (object_type, decoded[header_end + 1..].to_vec())
}

/// Parse a (SHA-1) tree object's entries as `(mode, name, oid)`.
fn tree_entries(repo: &DoctorRepo, oid: &str) -> Vec<(String, String, String)> {
    let (object_type, body) = read_loose(repo, oid);
    assert_eq!(object_type, "tree", "object {oid} must be a tree");
    let mut entries = Vec::new();
    let mut cursor = 0;
    while cursor < body.len() {
        let space = cursor
            + body[cursor..]
                .iter()
                .position(|&b| b == b' ')
                .expect("mode terminator");
        let mode = std::str::from_utf8(&body[cursor..space])
            .expect("utf8 mode")
            .to_string();
        let name_start = space + 1;
        let null = name_start
            + body[name_start..]
                .iter()
                .position(|&b| b == 0)
                .expect("name terminator");
        let name = std::str::from_utf8(&body[name_start..null])
            .expect("utf8 name")
            .to_string();
        let hash_start = null + 1;
        let entry_oid = hex::encode(&body[hash_start..hash_start + 20]);
        entries.push((mode, name, entry_oid));
        cursor = hash_start + 20;
    }
    entries
}

fn subtree_oid(repo: &DoctorRepo, tree: &str, name: &str) -> String {
    tree_entries(repo, tree)
        .into_iter()
        .find(|(mode, entry_name, _)| entry_name == name && mode == "40000")
        .unwrap_or_else(|| panic!("tree entry '{name}' missing from tree {tree}"))
        .2
}

/// All blob entries of the checkpoint's inner E4 tree as
/// `path → oid` pairs (top-level sidecars plus `events/…` and
/// `transcript/…` leaves).
fn checkpoint_sidecars(repo: &DoctorRepo, row: &RowSnapshot) -> Vec<(String, String)> {
    let checkpoint = subtree_oid(repo, &row.tree_oid, "checkpoint");
    let prefix = subtree_oid(repo, &checkpoint, &row.checkpoint_id[..2]);
    let inner = subtree_oid(repo, &prefix, &row.checkpoint_id[2..]);
    let mut out = Vec::new();
    for (mode, name, oid) in tree_entries(repo, &inner) {
        if mode == "40000" {
            for (_, leaf_name, leaf_oid) in tree_entries(repo, &oid) {
                out.push((format!("{name}/{leaf_name}"), leaf_oid));
            }
        } else {
            out.push((name, oid));
        }
    }
    out
}

fn sidecar_oid(repo: &DoctorRepo, row: &RowSnapshot, path: &str) -> String {
    checkpoint_sidecars(repo, row)
        .into_iter()
        .find(|(entry_path, _)| entry_path == path)
        .unwrap_or_else(|| panic!("sidecar '{path}' missing from checkpoint tree"))
        .1
}

fn delete_loose_object(repo: &DoctorRepo, oid: &str) -> Vec<u8> {
    let path = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read loose object {oid}: {e}"));
    std::fs::remove_file(&path).unwrap_or_else(|e| panic!("delete loose object {oid}: {e}"));
    bytes
}

fn restore_loose_object(repo: &DoctorRepo, oid: &str, bytes: &[u8]) {
    let path = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    std::fs::write(&path, bytes).unwrap_or_else(|e| panic!("restore loose object {oid}: {e}"));
}

// ---------------------------------------------------------------------------
// Class 2 — ref-reachable commit without catalog row (crash window B)
// ---------------------------------------------------------------------------

/// Ingest a checkpoint, DELETE its `agent_checkpoint` row: doctor detects
/// `missing_catalog_row`; `--repair` re-INSERTs the row with the original
/// key fields (including `parent_commit`, reconstructed from the
/// `Libra-Parent-Commit` trailer); a second run of either mode is clean.
#[tokio::test]
async fn class2_missing_catalog_row_detected_and_repaired() {
    let repo = DoctorRepo::init();

    // A user commit first, so the checkpoint's parent_commit is Some(head)
    // and the class-2 trailer reconstruction path is exercised end-to-end.
    std::fs::write(repo.repo.join("seed.txt"), "seed\n").expect("write seed file");
    let out = repo.run(&["add", "seed.txt"], None);
    assert!(out.status.success(), "libra add: {}", describe(&out));
    let out = repo.run(&["commit", "-m", "seed"], None);
    assert!(out.status.success(), "libra commit: {}", describe(&out));

    repo.ingest_checkpoint("sess-class2", TRANSCRIPT);
    let rows = repo.checkpoint_rows().await;
    assert_eq!(rows.len(), 1, "expected exactly one checkpoint: {rows:?}");
    let original = rows[0].clone();
    assert!(
        original.parent_commit.is_some(),
        "with a user commit the checkpoint must record a parent_commit"
    );

    // Baseline: healthy store.
    assert_store_clean(&repo.doctor_json(false));

    // Fabricate window B: ref advanced, catalog row missing.
    repo.exec_sql(
        "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
        vec![original.checkpoint_id.clone().into()],
    )
    .await;

    // Detection-only: reported, not repaired, nothing written back.
    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(found[0]["inconsistency_type"], json!("missing_catalog_row"));
    assert_eq!(found[0]["checkpoint_id"], json!(original.checkpoint_id));
    assert_eq!(found[0]["repaired"], json!(false));
    assert_eq!(found[0]["manual_required"], json!(false));
    assert_eq!(report["checkpoint_store"]["repair_applied"], json!(false));
    assert!(
        repo.checkpoint_rows().await.is_empty(),
        "detection-only must not write the row back"
    );

    // Repair: the row comes back, equal on every key field.
    let report = repo.doctor_json(true);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(found[0]["repaired"], json!(true));
    assert_eq!(report["checkpoint_store"]["repaired"], json!(1));
    let rows = repo.checkpoint_rows().await;
    assert_eq!(rows.len(), 1, "repair must reinsert exactly one row");
    assert_eq!(
        rows[0], original,
        "repaired row must match the original on all key fields"
    );

    // Idempotency: both modes are now clean no-ops.
    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
    assert_eq!(repo.checkpoint_rows().await, vec![original]);
}

/// A checkpoint tree entry is not a trusted catalog key. A malformed name can
/// still point at otherwise valid metadata/session state, so class 2 must keep
/// it out of both the reachable map and the repair INSERT path.
#[tokio::test]
async fn class2_noncanonical_checkpoint_identity_is_manual_and_never_inserted() {
    const MALFORMED_CHECKPOINT_ID: &str = "zznot-a-canonical-uuid";
    const MANUAL_DETAIL: &str = "invalid checkpoint identity in refs/libra/traces; manual repair required because automatic catalog reconstruction is unsafe";

    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-class2-noncanonical", TRANSCRIPT);
    let original = repo.checkpoint_rows().await.remove(0);
    assert_store_clean(&repo.doctor_json(false));

    // Reuse the real checkpoint's valid inner tree and metadata, but publish
    // it under an attacker-shaped `checkpoint/<prefix>/<rest>` tree entry.
    // Before the identity gate, this made class 2 reconstruct and INSERT a
    // second catalog row whose primary key was the malformed entry name.
    let libra_dir = repo.repo.join(".libra");
    let read_tree = |oid: &ObjectHash| -> Tree {
        let bytes = libra::utils::object::read_git_object(&libra_dir, oid)
            .unwrap_or_else(|error| panic!("read test tree {oid}: {error}"));
        Tree::from_bytes(&bytes, *oid)
            .unwrap_or_else(|error| panic!("parse test tree {oid}: {error}"))
    };
    let root_oid: ObjectHash = original
        .tree_oid
        .parse()
        .expect("parse original checkpoint root tree OID");
    let root = read_tree(&root_oid);
    let checkpoint_tree_oid = root
        .tree_items
        .iter()
        .find(|item| item.name == "checkpoint" && item.mode == TreeItemMode::Tree)
        .expect("checkpoint root entry")
        .id;
    let checkpoint_tree = read_tree(&checkpoint_tree_oid);
    let prefix_tree_oid = checkpoint_tree
        .tree_items
        .iter()
        .find(|item| item.name == original.checkpoint_id[..2] && item.mode == TreeItemMode::Tree)
        .expect("checkpoint prefix entry")
        .id;
    let prefix_tree = read_tree(&prefix_tree_oid);
    let inner_tree_oid = prefix_tree
        .tree_items
        .iter()
        .find(|item| item.name == original.checkpoint_id[2..] && item.mode == TreeItemMode::Tree)
        .expect("checkpoint inner tree entry")
        .id;

    let write_tree = |items: Vec<TreeItem>| -> ObjectHash {
        let tree = Tree::from_tree_items(items).expect("build malformed checkpoint tree");
        let data = tree.to_data().expect("serialize malformed checkpoint tree");
        libra::utils::object::write_git_object(&libra_dir, "tree", &data)
            .expect("write malformed checkpoint tree")
    };
    let malformed_prefix_tree = write_tree(vec![TreeItem::new(
        TreeItemMode::Tree,
        inner_tree_oid,
        MALFORMED_CHECKPOINT_ID[2..].to_string(),
    )]);
    let malformed_checkpoint_tree = write_tree(vec![TreeItem::new(
        TreeItemMode::Tree,
        malformed_prefix_tree,
        MALFORMED_CHECKPOINT_ID[..2].to_string(),
    )]);
    let malformed_root_tree = write_tree(vec![TreeItem::new(
        TreeItemMode::Tree,
        malformed_checkpoint_tree,
        "checkpoint".to_string(),
    )]);
    let parent_commit: ObjectHash = original
        .traces_commit
        .parse()
        .expect("parse original traces commit OID");
    let author = Signature::new(
        SignatureType::Author,
        "Libra".to_string(),
        "traces@libra".to_string(),
    );
    let committer = Signature::new(
        SignatureType::Committer,
        "Libra".to_string(),
        "traces@libra".to_string(),
    );
    let commit = Commit::new(
        author,
        committer,
        malformed_root_tree,
        vec![parent_commit],
        "traces: malformed checkpoint identity\n\nLibra-Scope: committed\n",
    );
    let commit_data = commit.to_data().expect("serialize malformed traces commit");
    let commit_oid = libra::utils::object::write_git_object(&libra_dir, "commit", &commit_data)
        .expect("write malformed traces commit");
    repo.exec_sql(
        "UPDATE reference SET \"commit\" = ? \
         WHERE name = 'traces' AND kind = 'Branch' AND remote IS NULL",
        vec![commit_oid.to_string().into()],
    )
    .await;

    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one manual finding expected: {report}");
    assert_eq!(found[0]["inconsistency_type"], json!("missing_catalog_row"));
    assert_eq!(found[0]["checkpoint_id"], json!("checkpoint-id-redacted"));
    assert_eq!(found[0]["detail"], json!(MANUAL_DETAIL));
    assert_eq!(found[0]["repaired"], json!(false));
    assert_eq!(found[0]["manual_required"], json!(true));
    assert_eq!(
        report["checkpoint_store"]["ref_reachable_checkpoints"],
        json!(1),
        "only the valid parent checkpoint may enter the reachability map: {report}"
    );
    assert!(
        !report.to_string().contains(MALFORMED_CHECKPOINT_ID),
        "doctor JSON must not reflect the malformed tree entry: {report}"
    );
    assert_eq!(
        repo.checkpoint_rows().await,
        vec![original.clone()],
        "detection-only must not persist the malformed identity"
    );
    let human = repo.run(&["agent", "doctor"], None);
    assert!(human.status.success(), "doctor: {}", describe(&human));
    let human_output = format!(
        "{}{}",
        String::from_utf8_lossy(&human.stdout),
        String::from_utf8_lossy(&human.stderr)
    );
    assert!(
        human_output.contains(MANUAL_DETAIL),
        "human report must retain the fixed manual guidance: {human_output}"
    );
    assert!(
        !human_output.contains(MALFORMED_CHECKPOINT_ID),
        "human report must not reflect the malformed tree entry: {human_output}"
    );

    let report = repo.doctor_json(true);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one manual finding expected: {report}");
    assert_eq!(found[0]["repaired"], json!(false));
    assert_eq!(found[0]["manual_required"], json!(true));
    assert_eq!(report["checkpoint_store"]["repaired"], json!(0));
    assert_eq!(report["checkpoint_store"]["manual_required"], json!(1));
    assert!(
        !report.to_string().contains(MALFORMED_CHECKPOINT_ID),
        "repair report must not reflect the malformed tree entry: {report}"
    );
    assert_eq!(
        repo.checkpoint_rows().await,
        vec![original],
        "--repair must never persist a catalog row for the malformed identity"
    );
}

/// A failed scoped catalog repair must not relay an underlying error chain to
/// either presentation format. In particular, provider session identifiers
/// can occur in SQLite/ownership failures, but are never safe to expose from
/// doctor output.
#[tokio::test]
async fn scoped_catalog_repair_failure_redacts_provider_identity_in_human_and_json_output() {
    const PROVIDER_SESSION_ID: &str = "provider-session-secret-doctor-output";
    const RETRY_GUIDANCE: &str = "scoped catalog repair failed closed; rerun doctor from the current workspace after resolving the ownership record";

    let repo = DoctorRepo::init();
    repo.ingest_checkpoint(PROVIDER_SESSION_ID, TRANSCRIPT);
    let original = repo.checkpoint_rows().await.remove(0);
    repo.exec_sql(
        "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
        vec![original.checkpoint_id.clone().into()],
    )
    .await;
    // Force the repair's INSERT to fail with the actual provider identity in
    // the lower error. The public doctor report must use its fixed guidance
    // instead of rendering that error chain.
    repo.exec_sql(
        "CREATE TRIGGER doctor_repair_provider_identity_failure
         BEFORE INSERT ON agent_checkpoint
         BEGIN
             SELECT RAISE(FAIL, 'provider-session-secret-doctor-output');
         END",
        vec![],
    )
    .await;

    let human = repo.run(&["agent", "doctor", "--repair"], None);
    assert!(human.status.success(), "doctor: {}", describe(&human));
    let human_output = format!(
        "{}{}",
        String::from_utf8_lossy(&human.stdout),
        String::from_utf8_lossy(&human.stderr)
    );
    assert!(
        human_output.contains(RETRY_GUIDANCE),
        "human report must give safe retry guidance: {human_output}"
    );
    assert!(
        !human_output.contains(PROVIDER_SESSION_ID),
        "human report leaked provider identity: {human_output}"
    );

    let json = repo.run(&["agent", "doctor", "--repair", "--json"], None);
    assert!(json.status.success(), "doctor JSON: {}", describe(&json));
    let json_output = format!(
        "{}{}",
        String::from_utf8_lossy(&json.stdout),
        String::from_utf8_lossy(&json.stderr)
    );
    assert!(
        json_output.contains(RETRY_GUIDANCE),
        "JSON report must give safe retry guidance: {json_output}"
    );
    assert!(
        !json_output.contains(PROVIDER_SESSION_ID),
        "JSON report leaked provider identity: {json_output}"
    );
    let report: Value = serde_json::from_str(String::from_utf8_lossy(&json.stdout).trim())
        .expect("doctor JSON output parses");
    let finding = report["data"]["checkpoint_store"]["findings"]
        .as_array()
        .and_then(|findings| findings.first())
        .expect("failed repair finding present");
    assert_eq!(finding["manual_required"], json!(true));
    assert_eq!(finding["repaired"], json!(false));
    let detail = finding["detail"].as_str().expect("finding detail string");
    assert!(
        detail.ends_with(RETRY_GUIDANCE) && !detail.contains(PROVIDER_SESSION_ID),
        "serialized finding must retain only safe retry guidance: {detail}"
    );
}

// ---------------------------------------------------------------------------
// Class 1 — DB row vs object store / ref truth
// ---------------------------------------------------------------------------

/// Corrupt all three OID columns of a ref-reachable row: doctor reports
/// `stale_catalog_row`; `--repair` rebuilds the columns from
/// `refs/libra/traces`; second run clean.
#[tokio::test]
async fn class1_stale_row_repaired_from_ref() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-class1", TRANSCRIPT);
    let original = repo.checkpoint_rows().await.remove(0);

    repo.exec_sql(
        "UPDATE agent_checkpoint SET tree_oid = ?, metadata_blob_oid = ?, traces_commit = ? \
         WHERE checkpoint_id = ?",
        vec![
            "a".repeat(40).into(),
            "b".repeat(40).into(),
            "c".repeat(40).into(),
            original.checkpoint_id.clone().into(),
        ],
    )
    .await;

    // Detection-only: reported as auto-repairable, row left corrupt.
    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(found[0]["inconsistency_type"], json!("stale_catalog_row"));
    assert_eq!(found[0]["checkpoint_id"], json!(original.checkpoint_id));
    assert_eq!(found[0]["manual_required"], json!(false));
    assert_eq!(found[0]["repaired"], json!(false));
    let still_corrupt = repo.checkpoint_rows().await.remove(0);
    assert_eq!(still_corrupt.tree_oid, "a".repeat(40));

    // Repair restores every OID column from the ref-reachable commit.
    let report = repo.doctor_json(true);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(found[0]["repaired"], json!(true));
    let repaired = repo.checkpoint_rows().await.remove(0);
    assert_eq!(
        repaired, original,
        "repair must restore tree_oid/metadata_blob_oid/traces_commit from the ref"
    );
    let conn = repo.db().await;
    let generation = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT sync_revision FROM agent_checkpoint WHERE checkpoint_id = ?",
            [repaired.checkpoint_id.clone().into()],
        ))
        .await
        .expect("query repaired checkpoint generation")
        .expect("repaired checkpoint row")
        .try_get_by::<i64, _>("sync_revision")
        .expect("decode repaired checkpoint generation");
    assert_eq!(
        generation, 2,
        "verified doctor repair must advance the cloud checkpoint generation"
    );
    drop(conn);

    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
}

/// A checkpoint suffering BOTH a stale catalog row (class 1,
/// auto-repairable) and missing `object_index` rows (class 3) must be
/// fully fixed by a SINGLE `doctor --repair` run — an auto-repairable
/// stale finding must not suppress the ref-side class-3 check, or
/// cloud-sync visibility would stay broken until a second invocation.
#[tokio::test]
async fn stale_row_and_missing_object_index_fixed_in_single_repair() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-stale-and-index", TRANSCRIPT);
    let original = repo.checkpoint_rows().await.remove(0);
    let oids = [
        original.traces_commit.as_str(),
        original.tree_oid.as_str(),
        original.metadata_blob_oid.as_str(),
    ];
    let index_before = repo.object_index_rows(&oids).await;
    assert_eq!(index_before.len(), 3, "writer baseline: {index_before:?}");

    // Fabricate both inconsistencies on the same checkpoint: corrupt the
    // row's OID columns AND drop the object_index rows of the real
    // (ref-side) objects.
    repo.exec_sql(
        "UPDATE agent_checkpoint SET tree_oid = ?, metadata_blob_oid = ?, traces_commit = ? \
         WHERE checkpoint_id = ?",
        vec![
            "a".repeat(40).into(),
            "b".repeat(40).into(),
            "c".repeat(40).into(),
            original.checkpoint_id.clone().into(),
        ],
    )
    .await;
    repo.exec_sql(
        "DELETE FROM object_index WHERE o_id IN (?, ?, ?)",
        oids.iter().map(|oid| (*oid).into()).collect(),
    )
    .await;

    // Detection sees both findings, both auto-repairable, and the class-3
    // finding names the REF-side (real) OIDs, not the corrupt columns.
    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 2, "both findings expected: {report}");
    let stale = found
        .iter()
        .find(|f| f["inconsistency_type"] == json!("stale_catalog_row"))
        .unwrap_or_else(|| panic!("stale finding missing: {report}"));
    let index = found
        .iter()
        .find(|f| f["inconsistency_type"] == json!("missing_object_index"))
        .unwrap_or_else(|| panic!("class-3 finding missing: {report}"));
    assert_eq!(stale["checkpoint_id"], json!(original.checkpoint_id));
    assert_eq!(index["checkpoint_id"], json!(original.checkpoint_id));
    assert_eq!(report["checkpoint_store"]["manual_required"], json!(0));
    let detail = index["detail"].as_str().unwrap_or_default();
    for oid in &oids {
        assert!(
            detail.contains(*oid),
            "class-3 detail must name the ref-side OID {oid}: {detail}"
        );
    }
    assert!(
        !detail.contains(&"a".repeat(40)),
        "class-3 must not target the corrupt column values: {detail}"
    );

    // ONE --repair run fixes both.
    let report = repo.doctor_json(true);
    let found = findings(&report);
    assert_eq!(found.len(), 2, "both findings expected: {report}");
    assert!(
        found.iter().all(|f| f["repaired"] == json!(true)),
        "a single --repair must fix the stale row AND the index rows: {report}"
    );
    assert_eq!(report["checkpoint_store"]["repaired"], json!(2));
    assert_eq!(
        repo.checkpoint_rows().await,
        vec![original.clone()],
        "row restored from the ref"
    );
    let mut index_after = repo.object_index_rows(&oids).await;
    index_after.sort();
    let mut expected = index_before.clone();
    expected.sort();
    assert_eq!(
        index_after, expected,
        "object_index rows restored to the writer baseline in the same run"
    );

    // Second runs are clean no-ops.
    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
}

/// Genuinely missing objects (and no ref to rebuild from) are reported as
/// `missing_objects`, require manual action, and are never "repaired" by
/// destructive means — the row survives `--repair` untouched.
#[tokio::test]
async fn class1_missing_objects_reported_manual_only() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-missing", TRANSCRIPT);
    let original = repo.checkpoint_rows().await.remove(0);

    // Drop the traces ref (nothing ref-reachable any more) and delete the
    // metadata blob object from the store.
    repo.exec_sql(
        "DELETE FROM reference WHERE name = ? AND kind = 'Branch'",
        vec!["traces".into()],
    )
    .await;
    let blob_path = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&original.metadata_blob_oid[..2])
        .join(&original.metadata_blob_oid[2..]);
    std::fs::remove_file(&blob_path).expect("delete metadata blob object");

    for repair in [false, true] {
        let report = repo.doctor_json(repair);
        let found = findings(&report);
        assert_eq!(found.len(), 1, "one finding expected: {report}");
        assert_eq!(found[0]["inconsistency_type"], json!("missing_objects"));
        assert_eq!(found[0]["checkpoint_id"], json!(original.checkpoint_id));
        assert_eq!(found[0]["manual_required"], json!(true));
        assert_eq!(found[0]["repaired"], json!(false));
        assert!(
            found[0]["detail"]
                .as_str()
                .unwrap_or_default()
                .contains(&original.metadata_blob_oid),
            "detail must name the missing object: {report}"
        );
        assert_eq!(report["checkpoint_store"]["manual_required"], json!(1));
        // No destructive action: the row is still exactly as written.
        assert_eq!(repo.checkpoint_rows().await, vec![original.clone()]);
    }
}

// ---------------------------------------------------------------------------
// Class 3 — object_index rows missing for catalog-known OIDs
// ---------------------------------------------------------------------------

/// DELETE the `object_index` rows of a checkpoint's three OIDs: doctor
/// detects `missing_object_index`; `--repair` re-inserts rows equivalent
/// to the writer's enqueue (same o_type/o_size/repo_id); second run clean.
#[tokio::test]
async fn class3_missing_object_index_reinserted() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-class3", TRANSCRIPT);
    let row = repo.checkpoint_rows().await.remove(0);
    let oids = [
        row.traces_commit.as_str(),
        row.tree_oid.as_str(),
        row.metadata_blob_oid.as_str(),
    ];

    // Baseline: the writer's background indexer catalogued all three
    // (the CLI drains its index queue before exiting).
    let before = repo.object_index_rows(&oids).await;
    assert_eq!(
        before.len(),
        3,
        "expected object_index rows for commit/tree/metadata: {before:?}"
    );

    repo.exec_sql(
        "DELETE FROM object_index WHERE o_id IN (?, ?, ?)",
        oids.iter().map(|oid| (*oid).into()).collect(),
    )
    .await;

    // Detection-only.
    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(
        found[0]["inconsistency_type"],
        json!("missing_object_index")
    );
    assert_eq!(found[0]["checkpoint_id"], json!(row.checkpoint_id));
    assert_eq!(found[0]["manual_required"], json!(false));
    let detail = found[0]["detail"].as_str().unwrap_or_default();
    for oid in &oids {
        assert!(detail.contains(*oid), "detail must list {oid}: {detail}");
    }
    assert!(
        repo.object_index_rows(&oids).await.is_empty(),
        "detection-only must not reinsert object_index rows"
    );

    // Repair: rows come back with the writer's semantics.
    let report = repo.doctor_json(true);
    assert_eq!(findings(&report)[0]["repaired"], json!(true));
    let mut after = repo.object_index_rows(&oids).await;
    after.sort();
    let mut expected = before.clone();
    expected.sort();
    assert_eq!(
        after, expected,
        "repaired rows must match the writer-enqueued rows on o_id/o_type/o_size/repo_id"
    );

    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
    assert_eq!(
        repo.object_index_rows(&oids).await.len(),
        3,
        "second repair run must not duplicate rows"
    );
}

/// A readable loose-object pathname is not enough evidence to repair its
/// cloud index. If a later transcript object fails descriptor-pinned zlib/hash
/// validation, doctor must mark the whole checkpoint manual and leave even
/// earlier missing rows untouched rather than publishing a partial capture.
#[tokio::test]
async fn class3_corrupt_object_requires_manual_without_partial_index_repair() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-class3-corrupt", TRANSCRIPT);
    let row = repo.checkpoint_rows().await.remove(0);
    let manifest_oid = sidecar_oid(&repo, &row, "manifest.json");
    let transcript_oid = sidecar_oid(&repo, &row, "transcript/claude_code.jsonl");
    let missing_oids = [row.traces_commit.as_str(), manifest_oid.as_str()];

    assert_eq!(
        repo.object_index_rows(&missing_oids).await.len(),
        2,
        "fixture needs writer-created rows to prove no partial repair"
    );
    repo.exec_sql(
        "DELETE FROM object_index WHERE o_id IN (?, ?)",
        missing_oids.iter().map(|oid| (*oid).into()).collect(),
    )
    .await;

    // Keep the named loose object in place so the class-1 existence sweep
    // succeeds; its malformed compressed content is discovered only by the
    // class-3 descriptor-pinned integrity validation.
    let transcript_path = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&transcript_oid[..2])
        .join(&transcript_oid[2..]);
    std::fs::write(&transcript_path, b"not a zlib stream")
        .expect("corrupt transcript loose object");

    for repair in [false, true] {
        let report = repo.doctor_json(repair);
        let found = findings(&report);
        assert_eq!(found.len(), 1, "one manual finding expected: {report}");
        assert_eq!(
            found[0]["inconsistency_type"],
            json!("missing_object_index")
        );
        assert_eq!(found[0]["checkpoint_id"], json!(row.checkpoint_id));
        assert_eq!(found[0]["manual_required"], json!(true));
        assert_eq!(found[0]["repaired"], json!(false));
        assert!(
            found[0]["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("no object-index rows were changed"),
            "the fail-closed reason must be explicit: {report}"
        );
        assert!(
            repo.object_index_rows(&missing_oids).await.is_empty(),
            "a corrupt later object must not restore an earlier index row"
        );
    }
}

/// A single missing E4 sidecar blob (`redaction_report.json`) — the row
/// columns are all intact — is a class-1 `missing_objects` finding naming
/// the sidecar; `--repair` cannot resurrect a lost blob (manual only, no
/// destructive action), and the store reports healthy again only once the
/// object returns.
#[tokio::test]
async fn class1_missing_sidecar_blob_detected_manual_only() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-sidecar", TRANSCRIPT);
    let original = repo.checkpoint_rows().await.remove(0);
    let report_oid = sidecar_oid(&repo, &original, "redaction_report.json");
    let saved = delete_loose_object(&repo, &report_oid);

    for repair in [false, true] {
        let report = repo.doctor_json(repair);
        let found = findings(&report);
        assert_eq!(found.len(), 1, "one finding expected: {report}");
        assert_eq!(found[0]["inconsistency_type"], json!("missing_objects"));
        assert_eq!(found[0]["checkpoint_id"], json!(original.checkpoint_id));
        assert_eq!(found[0]["manual_required"], json!(true));
        assert_eq!(found[0]["repaired"], json!(false));
        let detail = found[0]["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("redaction_report.json") && detail.contains(&report_oid),
            "detail must name the missing sidecar and its OID: {detail}"
        );
        // Not a legacy misclassification, and no destructive action.
        assert_eq!(
            report["checkpoint_store"]["legacy_v1_checkpoints"],
            json!(0)
        );
        assert_eq!(repo.checkpoint_rows().await, vec![original.clone()]);
    }

    // Healthy again only after the object is restored.
    restore_loose_object(&repo, &report_oid, &saved);
    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
}

/// A missing `manifest.json` blob is class-1 `missing_objects` — NOT
/// legacy-v1 (the tree entry still exists; legacy means the entry is
/// absent) — and one missing manifest must not hide other missing
/// sidecars: a simultaneously deleted `content_hash.txt` is named too.
#[tokio::test]
async fn class1_missing_manifest_is_not_legacy_and_other_sidecars_still_checked() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-manifest", TRANSCRIPT);
    let original = repo.checkpoint_rows().await.remove(0);
    let manifest_oid = sidecar_oid(&repo, &original, "manifest.json");
    let hash_oid = sidecar_oid(&repo, &original, "content_hash.txt");
    delete_loose_object(&repo, &manifest_oid);
    delete_loose_object(&repo, &hash_oid);

    for repair in [false, true] {
        let report = repo.doctor_json(repair);
        assert_eq!(
            report["checkpoint_store"]["legacy_v1_checkpoints"],
            json!(0),
            "a missing manifest blob must never classify as legacy-v1: {report}"
        );
        let found = findings(&report);
        assert_eq!(found.len(), 1, "one finding expected: {report}");
        assert_eq!(found[0]["inconsistency_type"], json!("missing_objects"));
        assert_eq!(found[0]["manual_required"], json!(true));
        assert_eq!(found[0]["repaired"], json!(false));
        let detail = found[0]["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("manifest.json") && detail.contains(&manifest_oid),
            "detail must name the missing manifest: {detail}"
        );
        assert!(
            detail.contains("content_hash.txt") && detail.contains(&hash_oid),
            "a missing manifest must not hide other missing sidecars: {detail}"
        );
        assert_eq!(repo.checkpoint_rows().await, vec![original.clone()]);
    }
}

/// DELETE the `object_index` rows of E4 sidecar objects (manifest,
/// lifecycle events, transcript): doctor detects `missing_object_index`
/// and `--repair` re-inserts rows equal to the writer-enqueued baseline —
/// in particular the transcript blob keeps the writer's distinguished
/// `agent_transcript` o_type.
#[tokio::test]
async fn class3_missing_sidecar_object_index_rows_reinserted() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-sidecar-index", TRANSCRIPT);
    let row = repo.checkpoint_rows().await.remove(0);
    let manifest_oid = sidecar_oid(&repo, &row, "manifest.json");
    let events_oid = sidecar_oid(&repo, &row, "events/lifecycle.jsonl");
    let transcript_oid = sidecar_oid(&repo, &row, "transcript/claude_code.jsonl");
    let oids = [
        manifest_oid.as_str(),
        events_oid.as_str(),
        transcript_oid.as_str(),
    ];

    // Writer-enqueued baseline (the CLI drains its index queue on exit).
    let before = repo.object_index_rows(&oids).await;
    assert_eq!(
        before.len(),
        3,
        "expected object_index rows for the sidecar blobs: {before:?}"
    );
    let transcript_row = before
        .iter()
        .find(|(o_id, ..)| *o_id == transcript_oid)
        .expect("transcript object_index row");
    assert_eq!(
        transcript_row.1, "agent_transcript",
        "writer tags the transcript blob as agent_transcript: {before:?}"
    );

    repo.exec_sql(
        "DELETE FROM object_index WHERE o_id IN (?, ?, ?)",
        oids.iter().map(|oid| (*oid).into()).collect(),
    )
    .await;

    // Detection-only names every missing sidecar OID.
    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(
        found[0]["inconsistency_type"],
        json!("missing_object_index")
    );
    assert_eq!(found[0]["checkpoint_id"], json!(row.checkpoint_id));
    let detail = found[0]["detail"].as_str().unwrap_or_default();
    for oid in &oids {
        assert!(detail.contains(*oid), "detail must list {oid}: {detail}");
    }

    // Repair restores the exact writer baseline (o_id/o_type/o_size/repo_id).
    let report = repo.doctor_json(true);
    assert_eq!(findings(&report)[0]["repaired"], json!(true));
    let mut after = repo.object_index_rows(&oids).await;
    after.sort();
    let mut expected = before.clone();
    expected.sort();
    assert_eq!(
        after, expected,
        "repaired sidecar rows must match the writer-enqueued baseline"
    );

    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
    assert_eq!(
        repo.object_index_rows(&oids).await.len(),
        3,
        "second repair run must not duplicate rows"
    );
}

// ---------------------------------------------------------------------------
// Legacy-v1 — exempt from all classes, byte-identical across --repair
// ---------------------------------------------------------------------------

/// Everything seeded for the v1 fixture checkpoint: object OIDs (for the
/// byte-identity check) plus its catalog row.
struct V1Seed {
    checkpoint_id: String,
    object_oids: Vec<String>,
}

/// Seed the committed v1-layout fixture
/// (`tests/fixtures/agent_checkpoints/v1_claude_code`) into `repo` as a
/// real root commit on `refs/libra/traces`: byte-identical blobs (OIDs
/// re-verified against the fixture README pins), reconstructed v1 trees
/// (`metadata.json` + `transcript/claude_code`, NO manifest.json), a
/// traces commit with `Libra-*` trailers, the `reference` row, and the
/// `agent_session` / `agent_checkpoint` rows the v1 writer would have
/// written. Deliberately does NOT seed `object_index` rows — legacy-v1
/// checkpoints are exempt from class 3 too.
fn seed_v1_fixture(repo: &DoctorRepo) -> V1Seed {
    let libra_dir = repo.repo.join(".libra");
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "tests/fixtures/agent_checkpoints/v1_claude_code/85/ae75d2-4c53-465a-b890-a9f861a50cc7",
    );
    let checkpoint_id = "85ae75d2-4c53-465a-b890-a9f861a50cc7".to_string();
    let session_id = "claude__fixture-v1-claude";

    let metadata_bytes =
        std::fs::read(fixture_root.join("metadata.json")).expect("fixture metadata");
    let transcript_bytes =
        std::fs::read(fixture_root.join("transcript/claude_code")).expect("fixture transcript");

    let write_blob = |bytes: &[u8]| -> ObjectHash {
        libra::utils::object::write_git_object(&libra_dir, "blob", bytes)
            .expect("write fixture blob")
    };
    let metadata_oid = write_blob(&metadata_bytes);
    assert_eq!(
        metadata_oid.to_string(),
        "b0265e8c5249c53dc588913554cdebdb82b984ec",
        "fixture metadata.json must rehash to the README-pinned OID"
    );
    let transcript_oid = write_blob(&transcript_bytes);
    assert_eq!(
        transcript_oid.to_string(),
        "2c43a69258d78142464f074e4c050bd9c7f0325f",
        "fixture transcript must rehash to the README-pinned OID"
    );

    let write_tree = |items: Vec<TreeItem>| -> ObjectHash {
        let tree = Tree::from_tree_items(items).expect("build tree");
        let data = tree.to_data().expect("serialize tree");
        libra::utils::object::write_git_object(&libra_dir, "tree", &data)
            .expect("write fixture tree")
    };
    // v1 inner layout: metadata.json + transcript/<provider> (no
    // extension), and crucially NO manifest.json.
    let transcript_tree = write_tree(vec![TreeItem::new(
        TreeItemMode::Blob,
        transcript_oid,
        "claude_code".to_string(),
    )]);
    let inner_tree = write_tree(vec![
        TreeItem::new(
            TreeItemMode::Blob,
            metadata_oid,
            "metadata.json".to_string(),
        ),
        TreeItem::new(
            TreeItemMode::Tree,
            transcript_tree,
            "transcript".to_string(),
        ),
    ]);
    let prefix_tree = write_tree(vec![TreeItem::new(
        TreeItemMode::Tree,
        inner_tree,
        checkpoint_id[2..].to_string(),
    )]);
    let checkpoint_tree = write_tree(vec![TreeItem::new(
        TreeItemMode::Tree,
        prefix_tree,
        checkpoint_id[..2].to_string(),
    )]);
    let root_tree = write_tree(vec![TreeItem::new(
        TreeItemMode::Tree,
        checkpoint_tree,
        "checkpoint".to_string(),
    )]);

    let message = format!(
        "traces: committed checkpoint {checkpoint_id}\n\n\
         Libra-Session: {session_id}\n\
         Libra-Agent: claude_code\n\
         Libra-Checkpoint-ID: {checkpoint_id}\n\
         Libra-Scope: committed\n"
    );
    let author = Signature::new(
        SignatureType::Author,
        "Libra".to_string(),
        "traces@libra".to_string(),
    );
    let committer = Signature::new(
        SignatureType::Committer,
        "Libra".to_string(),
        "traces@libra".to_string(),
    );
    let commit = Commit::new(author, committer, root_tree, vec![], &message);
    let commit_data = commit.to_data().expect("serialize fixture commit");
    let commit_oid = libra::utils::object::write_git_object(&libra_dir, "commit", &commit_data)
        .expect("write fixture commit");

    V1Seed {
        checkpoint_id,
        object_oids: vec![
            metadata_oid.to_string(),
            transcript_oid.to_string(),
            transcript_tree.to_string(),
            inner_tree.to_string(),
            prefix_tree.to_string(),
            checkpoint_tree.to_string(),
            root_tree.to_string(),
            commit_oid.to_string(),
        ],
    }
}

async fn seed_v1_rows(repo: &DoctorRepo, seed: &V1Seed) {
    let session_id = "claude__fixture-v1-claude";
    let root_tree = &seed.object_oids[6];
    let commit_oid = &seed.object_oids[7];
    // `libra init` pre-seeds the traces branch row (commit NULL); the
    // unique index on (name, kind) is partial (WHERE remote IS NULL), so
    // update-then-insert instead of ON CONFLICT.
    repo.exec_sql(
        "UPDATE reference SET \"commit\" = ? \
         WHERE name = 'traces' AND kind = 'Branch' AND remote IS NULL",
        vec![commit_oid.clone().into()],
    )
    .await;
    repo.exec_sql(
        "INSERT INTO reference (name, kind, \"commit\") \
         SELECT 'traces', 'Branch', ? \
         WHERE NOT EXISTS (SELECT 1 FROM reference \
                           WHERE name = 'traces' AND kind = 'Branch' AND remote IS NULL)",
        vec![commit_oid.clone().into()],
    )
    .await;
    repo.exec_sql(
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, \
         working_dir, started_at, last_event_at) VALUES (?, 'claude_code', ?, 'stopped', ?, 1, 1)",
        vec![
            session_id.into(),
            "fixture-v1-claude".into(),
            repo.repo.display().to_string().into(),
        ],
    )
    .await;
    repo.exec_sql(
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, \
         tree_oid, metadata_blob_oid, traces_commit, created_at) \
         VALUES (?, ?, 'committed', NULL, ?, ?, ?, 1783206712)",
        vec![
            seed.checkpoint_id.clone().into(),
            session_id.into(),
            root_tree.clone().into(),
            seed.object_oids[0].clone().into(),
            commit_oid.clone().into(),
        ],
    )
    .await;
}

/// The v1 fixture (seeded as a real traces root commit, with a v2
/// checkpoint ingested on top of it) is classified `legacy_v1_checkpoints`,
/// never enters the three classes even though its `object_index` rows are
/// absent, and survives `--repair` byte-identical (objects, catalog row,
/// and ref all unchanged).
#[tokio::test]
async fn legacy_v1_fixture_classified_and_never_repaired() {
    let repo = DoctorRepo::init();
    let seed = seed_v1_fixture(&repo);
    seed_v1_rows(&repo, &seed).await;

    // A real v2 checkpoint on top — the writer splices onto the v1 parent,
    // proving mixed v1/v2 chains classify correctly.
    repo.ingest_checkpoint("sess-v2-on-legacy", TRANSCRIPT);
    let rows = repo.checkpoint_rows().await;
    assert_eq!(rows.len(), 2, "v1 fixture + v2 ingest expected: {rows:?}");
    let v1_row = rows
        .iter()
        .find(|r| r.checkpoint_id == seed.checkpoint_id)
        .expect("v1 row present")
        .clone();

    let object_bytes_before: Vec<Vec<u8>> = seed
        .object_oids
        .iter()
        .map(|oid| repo.loose_object_bytes(oid))
        .collect();

    for repair in [false, true] {
        let report = repo.doctor_json(repair);
        assert_eq!(
            report["checkpoint_store"]["legacy_v1_checkpoints"],
            json!(1),
            "v1 fixture must be classified legacy-v1: {report}"
        );
        assert_eq!(
            report["checkpoint_store"]["ref_reachable_checkpoints"],
            json!(2),
            "both checkpoints are ref-reachable: {report}"
        );
        let found = findings(&report);
        assert!(
            found.is_empty(),
            "legacy-v1 must not enter the three classes (and the v2 \
             checkpoint is healthy): {report}"
        );
    }

    // Byte-identity: --repair must not have rewritten anything of the v1
    // checkpoint — objects, catalog row, and its object_index absence.
    for (oid, before) in seed.object_oids.iter().zip(&object_bytes_before) {
        assert_eq!(
            &repo.loose_object_bytes(oid),
            before,
            "fixture object {oid} must be byte-identical after --repair"
        );
    }
    let rows = repo.checkpoint_rows().await;
    let v1_after = rows
        .iter()
        .find(|r| r.checkpoint_id == seed.checkpoint_id)
        .expect("v1 row still present");
    assert_eq!(
        *v1_after, v1_row,
        "the legacy-v1 catalog row must be untouched by --repair"
    );
    let v1_oids: Vec<&str> = seed.object_oids.iter().map(String::as_str).collect();
    assert!(
        repo.object_index_rows(&v1_oids).await.is_empty(),
        "legacy-v1 objects are exempt from class-3 re-enqueue"
    );
}

// ---------------------------------------------------------------------------
// Orphan rule fidelity + gemini hint
// ---------------------------------------------------------------------------

/// A session without any checkpoint is a LEGAL intermediate state (active
/// session before its first Stop/TurnEnd): doctor must not flag it in any
/// category.
#[tokio::test]
async fn session_without_checkpoint_is_never_flagged() {
    let repo = DoctorRepo::init();
    let transcript = repo.write_claude_transcript(TRANSCRIPT);
    let out = repo.hook(
        "claude-code",
        "session-start",
        &repo.envelope("SessionStart", "sess-legal-orphan", &transcript),
    );
    assert!(out.status.success(), "session-start: {}", describe(&out));

    let report = repo.doctor_json(false);
    assert_eq!(report["active_sessions"], json!(1));
    assert_eq!(
        report["orphan_checkpoints"],
        json!(0),
        "session-without-checkpoint must not count as orphan: {report}"
    );
    assert_store_clean(&report);
    assert_eq!(report["gemini_hooks_remnant"], json!(false));
}

/// Replicate the exact settings shape `libra agent enable gemini` used to
/// write (`hooksConfig.enabled` + the seven Libra-managed hook entries
/// pointing at the current binary), so `hooks_are_installed()` reports
/// remnants.
fn write_gemini_remnant_settings(repo: &DoctorRepo) {
    let binary =
        std::fs::canonicalize(env!("CARGO_BIN_EXE_libra")).expect("canonicalize libra binary path");
    let binary = binary.display();
    let entry = |matcher: Option<&str>, name: &str, subcommand: &str| -> Value {
        let mut obj = json!({
            "hooks": [{
                "name": name,
                "type": "command",
                "command": format!("{binary} hooks gemini {subcommand}"),
            }],
        });
        if let Some(matcher) = matcher {
            obj["matcher"] = json!(matcher);
        }
        json!([obj])
    };
    let settings = json!({
        "hooksConfig": { "enabled": true },
        "hooks": {
            "SessionStart": entry(None, "libra-session-start", "session-start"),
            "BeforeAgent": entry(None, "libra-before-agent", "prompt"),
            "AfterTool": entry(Some("*"), "libra-after-tool", "tool-use"),
            "AfterAgent": entry(None, "libra-after-agent", "stop"),
            "SessionEnd": entry(None, "libra-session-end", "session-end"),
            "BeforeModel": entry(None, "libra-before-model", "model-update"),
            "PreCompress": entry(None, "libra-pre-compress", "compaction"),
        },
    });
    let gemini_dir = repo.repo.join(".gemini");
    std::fs::create_dir_all(&gemini_dir).expect("create .gemini dir");
    std::fs::write(
        gemini_dir.join("settings.json"),
        serde_json::to_vec_pretty(&settings).expect("serialize gemini settings"),
    )
    .expect("write gemini settings remnant");
}

/// Leftover gemini hook configuration triggers the uninstall-channel hint
/// (`libra agent remove gemini`); existing gemini `agent_session` rows are
/// legal read-only data and produce no findings.
#[tokio::test]
async fn gemini_remnant_hint_and_readonly_rows() {
    let repo = DoctorRepo::init();
    write_gemini_remnant_settings(&repo);
    // Legal read-only capture data from the gemini era.
    repo.exec_sql(
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, \
         working_dir, started_at, last_event_at) VALUES (?, 'gemini', ?, 'stopped', ?, 1, 1)",
        vec![
            "gemini__legacy-1".into(),
            "legacy-1".into(),
            repo.repo.display().to_string().into(),
        ],
    )
    .await;

    let report = repo.doctor_json(false);
    assert_eq!(
        report["gemini_hooks_remnant"],
        json!(true),
        "remnant gemini hooks must be surfaced: {report}"
    );
    let gemini_hook = report["provider_hooks"]
        .as_array()
        .expect("provider_hooks array")
        .iter()
        .find(|ph| ph["name"] == json!("gemini"))
        .expect("gemini provider row")
        .clone();
    assert_eq!(gemini_hook["installed"], json!(true));
    assert_store_clean(&report);
    assert_eq!(
        report["orphan_checkpoints"],
        json!(0),
        "gemini rows are legal read-only data: {report}"
    );

    // Human output carries the actionable uninstall hint.
    let out = repo.run(&["agent", "doctor"], None);
    assert!(out.status.success(), "doctor: {}", describe(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("libra agent remove gemini"),
        "human output must hint the gemini uninstall channel:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// Span coverage — agent.doctor.repair (agent.md §6)
// ---------------------------------------------------------------------------

/// `--repair` emits one `agent.doctor.repair` span per repair attempt with
/// the §6 required fields; detection-only runs emit none; transcript
/// content never reaches the sink. Asserted through the CLI's own tracing
/// stack via `LIBRA_LOG` + `LIBRA_LOG_FILE` (span fields render as the
/// event's span scope), so no in-process fake sink is needed.
#[tokio::test]
async fn repair_span_carries_required_fields_without_transcript() {
    let repo = DoctorRepo::init();
    let marker = "SPAN-FORBIDDEN-TRANSCRIPT-MARKER-42x9";
    let transcript = format!(
        "{{\"type\":\"user\",\"text\":\"{marker}\"}}\n{{\"type\":\"assistant\",\"text\":\"done\"}}\n"
    );
    repo.ingest_checkpoint("sess-span", &transcript);
    let original = repo.checkpoint_rows().await.remove(0);
    repo.exec_sql(
        "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
        vec![original.checkpoint_id.clone().into()],
    )
    .await;

    // Detection-only: no repair attempted → no repair span.
    let detect_log = repo.repo.join("doctor-detect.log");
    let out = repo.run_env(
        &["agent", "doctor", "--json"],
        None,
        &[
            ("LIBRA_LOG", "libra=info"),
            ("LIBRA_LOG_FILE", &detect_log.display().to_string()),
        ],
    );
    assert!(out.status.success(), "doctor: {}", describe(&out));
    let detect_captured = std::fs::read_to_string(&detect_log).unwrap_or_default();
    assert!(
        !detect_captured.contains("agent.doctor.repair"),
        "detection-only must not emit repair spans:\n{detect_captured}"
    );

    // Repair: span present with required fields, transcript body absent.
    let repair_log = repo.repo.join("doctor-repair.log");
    let out = repo.run_env(
        &["agent", "doctor", "--repair", "--json"],
        None,
        &[
            ("LIBRA_LOG", "libra=info"),
            ("LIBRA_LOG_FILE", &repair_log.display().to_string()),
        ],
    );
    assert!(out.status.success(), "doctor --repair: {}", describe(&out));
    let captured = std::fs::read_to_string(&repair_log).expect("read doctor span log");
    assert!(
        captured.contains("agent.doctor.repair"),
        "repair span missing:\n{captured}"
    );
    for field in [
        "inconsistency_type=missing_catalog_row",
        "repaired=true",
        "manual_required=false",
    ] {
        assert!(
            captured.contains(field),
            "repair span missing `{field}`:\n{captured}"
        );
    }
    assert!(
        !captured.contains(marker),
        "transcript content must never reach the span sink:\n{captured}"
    );

    // The repair itself worked (row equality on key fields).
    assert_eq!(repo.checkpoint_rows().await, vec![original]);
}

/// Class 3 also repairs rows that EXIST but drifted from the writer's
/// semantics (codex A5 review R5): a transcript blob mis-indexed as a
/// generic `blob` (or with a wrong `o_size`) breaks cloud-sync
/// classification exactly like a missing row. Doctor detects the drift after
/// bounded streaming validation of the descriptor-pinned object (it never
/// trusts a manifest `byte_len` or retains transcript bytes) and `--repair`
/// UPDATEs the row in place back to the writer baseline.
#[tokio::test]
async fn class3_drifted_object_index_row_updated_in_place() {
    let repo = DoctorRepo::init();
    repo.ingest_checkpoint("sess-drift-index", TRANSCRIPT);
    let row = repo.checkpoint_rows().await.remove(0);
    let transcript_oid = sidecar_oid(&repo, &row, "transcript/claude_code.jsonl");
    let oids = [transcript_oid.as_str()];

    let before = repo.object_index_rows(&oids).await;
    assert_eq!(before.len(), 1, "baseline transcript row: {before:?}");
    assert_eq!(before[0].1, "agent_transcript");

    // Drift the row: wrong o_type/size and an already-synced flag that must
    // be reset when repair changes the cloud-visible classification.
    repo.exec_sql(
        "UPDATE object_index SET o_type = 'blob', o_size = 1, is_synced = 1 WHERE o_id = ?",
        vec![transcript_oid.clone().into()],
    )
    .await;

    // Detection reports the drift, naming the old shape.
    let report = repo.doctor_json(false);
    let found = findings(&report);
    assert_eq!(found.len(), 1, "one finding expected: {report}");
    assert_eq!(
        found[0]["inconsistency_type"],
        json!("missing_object_index")
    );
    let detail = found[0]["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("drifted") && detail.contains("was blob/1"),
        "detail must describe the drift: {detail}"
    );

    // Repair restores the writer baseline in place; second runs clean.
    let report = repo.doctor_json(true);
    assert_eq!(findings(&report)[0]["repaired"], json!(true));
    assert_eq!(
        repo.object_index_rows(&oids).await,
        before,
        "drifted row must be updated back to the writer baseline"
    );
    let synced: i64 = repo
        .db()
        .await
        .query_one_raw(Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT is_synced FROM object_index WHERE o_id = ?",
            [transcript_oid.clone().into()],
        ))
        .await
        .expect("read repaired sync flag")
        .expect("repaired row exists")
        .try_get_by("is_synced")
        .expect("decode repaired sync flag");
    assert_eq!(
        synced, 0,
        "repaired row must be offered to cloud sync again"
    );
    assert_store_clean(&repo.doctor_json(false));
    assert_store_clean(&repo.doctor_json(true));
}

// ---------------------------------------------------------------------------
// A0-02 — subagent-scope crash-window-B repair parity
// ---------------------------------------------------------------------------

async fn subagent_parent_link(repo: &DoctorRepo, checkpoint_id: &str) -> Option<String> {
    let conn = repo.db().await;
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT parent_checkpoint_id FROM agent_checkpoint WHERE checkpoint_id = ?",
            vec![checkpoint_id.into()],
        ))
        .await
        .expect("query parent_checkpoint_id")
        .expect("subagent row present");
    row.try_get_by::<Option<String>, _>("parent_checkpoint_id")
        .unwrap()
}

/// A subagent-scope orphan (traces commit present, catalog row deleted) is
/// auto-repaired by `doctor --repair`, which rebuilds a first-class
/// `scope='subagent'` row — including its `parent_checkpoint_id` linkage —
/// from the commit's metadata.json, at parity with the committed class-2 path.
#[tokio::test]
async fn class2_missing_subagent_catalog_row_repaired() {
    let repo = DoctorRepo::init();

    // User commit so the checkpoint's parent_commit is Some(head).
    std::fs::write(repo.repo.join("seed.txt"), "seed\n").expect("write seed file");
    assert!(repo.run(&["add", "seed.txt"], None).status.success());
    assert!(repo.run(&["commit", "-m", "seed"], None).status.success());

    let transcript = repo.write_claude_transcript(TRANSCRIPT);
    let session = "sess-subagent-doctor";
    // codex owns the session; Stop writes a committed parent…
    assert!(
        repo.hook(
            "codex",
            "session-start",
            &repo.envelope("SessionStart", session, &transcript),
        )
        .status
        .success(),
        "codex session-start"
    );
    assert!(
        repo.hook(
            "codex",
            "stop",
            &repo.envelope("Stop", session, &transcript)
        )
        .status
        .success(),
        "codex stop"
    );
    // …and a SubagentStop boundary materialises the subagent checkpoint.
    let out = repo.hook(
        "codex",
        "subagent-end",
        &repo.envelope("SubagentStop", session, &transcript),
    );
    assert!(out.status.success(), "subagent-end: {}", describe(&out));

    let rows = repo.checkpoint_rows().await;
    let subagent = rows
        .iter()
        .find(|r| r.scope == "subagent")
        .cloned()
        .expect("a scope='subagent' checkpoint row");
    let parent_link_before = subagent_parent_link(&repo, &subagent.checkpoint_id).await;
    assert!(
        parent_link_before.is_some(),
        "subagent checkpoint must link back to the committed parent"
    );

    // Fabricate window B for the subagent row.
    repo.exec_sql(
        "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
        vec![subagent.checkpoint_id.clone().into()],
    )
    .await;

    // Detection-only: reported, not repaired.
    let report = repo.doctor_json(false);
    assert!(
        findings(&report)
            .iter()
            .any(|f| f["checkpoint_id"] == json!(subagent.checkpoint_id)
                && f["inconsistency_type"] == json!("missing_catalog_row")
                && f["repaired"] == json!(false)),
        "subagent window B must be detected without repair: {report}"
    );

    // Repair: a scope='subagent' row comes back with its linkage intact.
    let report = repo.doctor_json(true);
    assert!(
        findings(&report)
            .iter()
            .any(|f| f["checkpoint_id"] == json!(subagent.checkpoint_id)
                && f["repaired"] == json!(true)),
        "subagent row must be auto-repaired: {report}"
    );
    let rows = repo.checkpoint_rows().await;
    let repaired = rows
        .iter()
        .find(|r| r.checkpoint_id == subagent.checkpoint_id)
        .expect("subagent row reinserted");
    assert_eq!(
        repaired.scope, "subagent",
        "repaired row must keep scope='subagent'"
    );
    assert_eq!(
        subagent_parent_link(&repo, &subagent.checkpoint_id).await,
        parent_link_before,
        "parent_checkpoint_id linkage must survive the repair"
    );

    // Idempotent.
    assert_store_clean(&repo.doctor_json(true));
}

// ---------------------------------------------------------------------------
// A0-02 — subagent checkpoint sidecar must not leak the transcript path
// ---------------------------------------------------------------------------

/// Resolve the OID of a named entry among parsed `(mode, name, oid)` tree rows.
fn subagent_entry_oid(entries: &[(String, String, String)], name: &str) -> String {
    entries
        .iter()
        .find(|(_, n, _)| n == name)
        .map(|(_, _, oid)| oid.clone())
        .unwrap_or_else(|| panic!("tree entry '{name}' missing"))
}

/// A0-02 (Codex re-review): a `SubagentStop` whose envelope carries a
/// transcript path must NOT persist that local path in the subagent
/// checkpoint's syncable `events/lifecycle.jsonl` blob — the writer clears
/// `session_ref` on the sidecar event before serialization.
#[tokio::test]
async fn subagent_checkpoint_sidecar_omits_transcript_path() {
    let repo = DoctorRepo::init();
    let transcript = repo.write_claude_transcript(TRANSCRIPT);
    let transcript_str = transcript.to_string_lossy().to_string();
    let session = "sess-subagent-sidecar";

    assert!(
        repo.hook(
            "codex",
            "session-start",
            &repo.envelope("SessionStart", session, &transcript),
        )
        .status
        .success(),
        "codex session-start"
    );
    let out = repo.hook(
        "codex",
        "subagent-end",
        &repo.envelope("SubagentStop", session, &transcript),
    );
    assert!(out.status.success(), "subagent-end: {}", describe(&out));

    let subagent = repo
        .checkpoint_rows()
        .await
        .into_iter()
        .find(|r| r.scope == "subagent")
        .expect("a scope='subagent' checkpoint row");

    // Walk root → checkpoint/<id[:2]>/<id[2:]> → events → lifecycle.jsonl.
    let id = &subagent.checkpoint_id;
    let root = tree_entries(&repo, &subagent.tree_oid);
    let checkpoint = tree_entries(&repo, &subagent_entry_oid(&root, "checkpoint"));
    let prefix = tree_entries(&repo, &subagent_entry_oid(&checkpoint, &id[..2]));
    let inner = tree_entries(&repo, &subagent_entry_oid(&prefix, &id[2..]));
    let events = tree_entries(&repo, &subagent_entry_oid(&inner, "events"));
    let blob_oid = subagent_entry_oid(&events, "lifecycle.jsonl");
    let (blob_type, blob) = read_loose(&repo, &blob_oid);
    assert_eq!(blob_type, "blob");
    let content = String::from_utf8_lossy(&blob);

    assert!(
        !content.contains(&transcript_str),
        "events/lifecycle.jsonl must not leak the transcript path '{transcript_str}':\n{content}"
    );
    assert!(
        !content.contains("session_ref"),
        "the subagent sidecar event must not carry session_ref:\n{content}"
    );
}

/// A0-03: a checkpoint operation on an inconsistent store (a catalog row whose
/// `parent_commit` points at an object missing from the store) fails closed
/// with the stable `LBR-AGENT-009` (`AgentCheckpointStoreInconsistent`) code,
/// not a bare fatal.
#[tokio::test]
async fn checkpoint_store_inconsistent_emits_lbr_agent_009() {
    let repo = DoctorRepo::init();

    // A user commit so the checkpoint records a real parent_commit.
    std::fs::write(repo.repo.join("seed.txt"), "seed\n").expect("write seed file");
    assert!(repo.run(&["add", "seed.txt"], None).status.success());
    assert!(repo.run(&["commit", "-m", "seed"], None).status.success());

    repo.ingest_checkpoint("sess-inconsistent", TRANSCRIPT);
    let cp = repo.checkpoint_rows().await.remove(0);

    // Corrupt the catalog: point parent_commit at a non-existent object.
    let bogus = "b".repeat(40);
    repo.exec_sql(
        "UPDATE agent_checkpoint SET parent_commit = ? WHERE checkpoint_id = ?",
        vec![bogus.into(), cp.checkpoint_id.clone().into()],
    )
    .await;

    let out = repo.run(
        &[
            "agent",
            "checkpoint",
            "rewind",
            &cp.checkpoint_id,
            "--dry-run",
        ],
        None,
    );
    assert!(
        !out.status.success(),
        "rewind on a corrupted store must fail: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-AGENT-009"),
        "an inconsistent store must carry LBR-AGENT-009: {stderr}"
    );
}

/// A0-03 (Codex re-review): a rewind whose parent tree's ROOT is present but a
/// nested subtree object is missing must fail closed with `LBR-AGENT-009`, not
/// panic through `Tree::load`. Regression for the recursive tree-walk path.
#[tokio::test]
async fn checkpoint_store_missing_nested_tree_emits_lbr_agent_009() {
    let repo = DoctorRepo::init();

    // A commit whose tree contains a subdirectory (a nested subtree object).
    std::fs::create_dir_all(repo.repo.join("nested")).expect("mkdir nested");
    std::fs::write(repo.repo.join("nested").join("f.txt"), "x\n").expect("write nested file");
    assert!(repo.run(&["add", "nested/f.txt"], None).status.success());
    assert!(repo.run(&["commit", "-m", "nested"], None).status.success());

    repo.ingest_checkpoint("sess-nested", TRANSCRIPT);
    let cp = repo.checkpoint_rows().await.remove(0);
    let parent_commit = cp
        .parent_commit
        .clone()
        .expect("checkpoint records a parent_commit");

    // Resolve the root tree from the commit object, find the "nested" subtree,
    // and delete that object — leaving the root tree readable.
    let (_, commit_body) = read_loose(&repo, &parent_commit);
    let commit_text = String::from_utf8_lossy(&commit_body);
    let root_tree = commit_text
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("tree "))
        .expect("commit starts with a tree line")
        .trim()
        .to_string();
    let subtree_oid = subagent_entry_oid(&tree_entries(&repo, &root_tree), "nested");
    let obj = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&subtree_oid[..2])
        .join(&subtree_oid[2..]);
    std::fs::remove_file(&obj).expect("delete nested subtree object");

    let out = repo.run(
        &[
            "agent",
            "checkpoint",
            "rewind",
            &cp.checkpoint_id,
            "--dry-run",
        ],
        None,
    );
    assert!(
        !out.status.success(),
        "rewind with a missing nested tree must fail: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-AGENT-009"),
        "a missing nested tree must carry LBR-AGENT-009: {stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "rewind must not panic on a missing subtree: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// A0-06 — findings-object repair (review/investigate objectized findings)
// ---------------------------------------------------------------------------

/// Kills and reaps a helper process on drop, including on an assertion
/// unwind — a leaked holder would keep an exclusive lock (and a 600-second
/// sleep) alive for every later test in the process.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// §C.4.3 writer-vs-deleter: `agent doctor --repair` REPUBLISHES a findings
/// blob, so it must hold the repository maintenance lock across the rewrite
/// and the `object_index` insert.
///
/// `agent` is excluded from the command-level shared hold (its runs are
/// long), so this seam is where the exclusion happens for the repair path.
/// The test drives it from the outside: a second PROCESS holds the lock
/// exclusively, and the repair must not get through the publication while
/// that hold lasts.
///
/// The synchronisation is a PROOF, not a sleep: `doctor` writes its marker
/// only after observing that a shared acquisition would block, so the marker
/// existing IS the evidence that it waited. On a loaded machine a sleep would
/// prove nothing — a descheduled process looks exactly like a blocked one —
/// and removing the acquisition would make the marker never appear.
#[tokio::test]
async fn findings_object_repair_waits_for_the_maintenance_lock() {
    use std::io::{BufRead, BufReader};

    use libra::internal::ai::review::{
        ReviewRunStore, ReviewTerminalState, store::RedactionReportSummary,
    };

    let repo = DoctorRepo::init();
    let store = ReviewRunStore::new(repo.repo.join(".libra").join("sessions"));
    store
        .create_run("lock-run", &["codex".to_string()], "sha", "HEAD~1..HEAD")
        .expect("create run");
    store
        .write_findings("lock-run", "review finding body line\n")
        .expect("write findings");
    store
        .finalize_run(
            "lock-run",
            ReviewTerminalState::Success,
            &[],
            RedactionReportSummary::default(),
        )
        .expect("finalize objectizes findings");
    let findings_oid = store
        .load_manifest("lock-run")
        .expect("load manifest")
        .expect("manifest")
        .findings_oid
        .expect("findings_oid");
    let obj_path = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&findings_oid[..2])
        .join(&findings_oid[2..]);
    std::fs::remove_file(&obj_path).expect("delete findings object");

    // A deletion phase, in another process, holding the lock exclusively.
    let lock_path = repo.repo.join(".libra").join("maintenance.lock");
    let script = format!(
        "import fcntl, sys, time\n\
         f = open({path:?}, 'a+')\n\
         fcntl.flock(f, fcntl.LOCK_EX)\n\
         sys.stdout.write('locked\\n')\n\
         sys.stdout.flush()\n\
         time.sleep(600)\n",
        path = lock_path.to_string_lossy().to_string()
    );
    let mut deleter = ChildGuard(
        std::process::Command::new("python3")
            .args(["-c", &script])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the deleter holding the lock"),
    );
    let mut ready = String::new();
    BufReader::new(deleter.0.stdout.take().expect("stdout"))
        .read_line(&mut ready)
        .expect("deleter ready");
    assert_eq!(ready.trim(), "locked");

    // The repair must reach its lock attempt and then WAIT there.
    let barrier = repo.repo.join(".libra").join("publication-barrier");
    let mut repair = ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(["agent", "doctor", "--repair", "--json"])
            .current_dir(&repo.repo)
            .env("LIBRA_TEST", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn doctor --repair"),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !barrier.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "doctor never reached the publication barrier"
        );
        assert!(
            repair.0.try_wait().expect("poll").is_none(),
            "doctor exited before reaching the publication"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // The marker is written only AFTER the repair observed that a shared
    // acquisition would block, so its existence already proves the wait —
    // there is no sleep here to be wrong about.
    assert!(
        repair.0.try_wait().expect("poll").is_none(),
        "the repair must wait for the deletion phase to finish, not republish underneath it"
    );
    assert!(
        !obj_path.exists(),
        "and it must not have rewritten the blob while the deleter holds the lock"
    );

    // Released: the repair completes and the blob is back.
    drop(deleter);
    let status = repair
        .0
        .wait()
        .expect("repair finishes once the lock is free");
    assert!(status.success(), "doctor --repair failed after the wait");
    assert!(
        obj_path.exists(),
        "the findings object must be restored once the lock is released"
    );
}

/// The contention marker is created NO-FOLLOW.
///
/// The marker path is fixed and inside `.libra`, but a planted symlink there
/// would redirect a following write anywhere the process can reach. `O_EXCL`
/// refuses a symlink outright; this pins that, because a regression to
/// `fs::write` would follow it and the contention test alone would not
/// notice.
#[cfg(unix)]
#[tokio::test]
async fn the_contention_marker_never_follows_a_planted_symlink() {
    use std::io::{BufRead, BufReader};

    use libra::internal::ai::review::{
        ReviewRunStore, ReviewTerminalState, store::RedactionReportSummary,
    };

    let repo = DoctorRepo::init();
    let store = ReviewRunStore::new(repo.repo.join(".libra").join("sessions"));
    store
        .create_run("symlink-run", &["codex".to_string()], "sha", "HEAD~1..HEAD")
        .expect("create run");
    store
        .write_findings("symlink-run", "review finding body line\n")
        .expect("write findings");
    store
        .finalize_run(
            "symlink-run",
            ReviewTerminalState::Success,
            &[],
            RedactionReportSummary::default(),
        )
        .expect("finalize");
    let findings_oid = store
        .load_manifest("symlink-run")
        .expect("load")
        .expect("manifest")
        .findings_oid
        .expect("findings_oid");
    let obj_path = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&findings_oid[..2])
        .join(&findings_oid[2..]);
    std::fs::remove_file(&obj_path).expect("delete findings object");

    // A sentinel OUTSIDE the repository, and a symlink at the marker path
    // pointing at it.
    let outside = tempfile::tempdir().expect("outside");
    let sentinel = outside.path().join("sentinel");
    std::fs::write(&sentinel, b"untouched").expect("write sentinel");
    let marker = repo.repo.join(".libra").join("publication-barrier");
    std::os::unix::fs::symlink(&sentinel, &marker).expect("plant the symlink");

    // Force contention so the marker path is exercised.
    let lock_path = repo.repo.join(".libra").join("maintenance.lock");
    let script = format!(
        "import fcntl, sys, time\n\
         f = open({path:?}, 'a+')\n\
         fcntl.flock(f, fcntl.LOCK_EX)\n\
         sys.stdout.write('locked\\n')\n\
         sys.stdout.flush()\n\
         time.sleep(600)\n",
        path = lock_path.to_string_lossy().to_string()
    );
    let mut deleter = ChildGuard(
        std::process::Command::new("python3")
            .args(["-c", &script])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the deleter"),
    );
    let mut ready = String::new();
    BufReader::new(deleter.0.stdout.take().expect("stdout"))
        .read_line(&mut ready)
        .expect("deleter ready");
    assert_eq!(ready.trim(), "locked");

    let mut repair = ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(["agent", "doctor", "--repair", "--json"])
            .current_dir(&repo.repo)
            .env("LIBRA_TEST", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn doctor --repair"),
    );
    // Wait for PROOF that the contention branch ran — the `.attempted`
    // signal, which is written on the same path as the marker attempt and
    // has no symlink planted on it. Without this the sentinel could be
    // untouched merely because the child had not got there yet.
    let attempted = repo
        .repo
        .join(".libra")
        .join("publication-barrier.attempted");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !attempted.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "doctor never reached the contention branch"
        );
        assert!(
            repair.0.try_wait().expect("poll").is_none(),
            "doctor exited before reaching the contention branch"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        std::fs::read(&sentinel).expect("sentinel readable"),
        b"untouched",
        "the marker must never be written through a symlink"
    );
    assert!(
        marker.symlink_metadata().expect("marker path").is_symlink(),
        "the planted symlink itself must be left alone"
    );
    drop(deleter);
    let _ = repair.0.wait();
}

/// A0-06: a review run's objectized findings blob that goes missing while
/// `findings.md` remains is detected as `missing_findings_object` and
/// auto-repaired by `doctor --repair` (content-addressed rewrite from
/// findings.md), idempotently.
#[tokio::test]
async fn findings_object_repair() {
    use libra::internal::ai::review::{
        ReviewRunStore, ReviewTerminalState, store::RedactionReportSummary,
    };

    let repo = DoctorRepo::init();
    let store = ReviewRunStore::new(repo.repo.join(".libra").join("sessions"));
    store
        .create_run(
            "findings-run",
            &["codex".to_string()],
            "sha",
            "HEAD~1..HEAD",
        )
        .expect("create run");
    store
        .write_findings("findings-run", "review finding body line\n")
        .expect("write findings");
    store
        .finalize_run(
            "findings-run",
            ReviewTerminalState::Success,
            &[],
            RedactionReportSummary::default(),
        )
        .expect("finalize objectizes findings");

    let manifest = store
        .load_manifest("findings-run")
        .expect("load manifest")
        .expect("manifest");
    let findings_oid = manifest
        .findings_oid
        .expect("A0-06 populated findings_oid at finalize");
    let obj_path = repo
        .repo
        .join(".libra")
        .join("objects")
        .join(&findings_oid[..2])
        .join(&findings_oid[2..]);
    assert!(
        obj_path.exists(),
        "findings object must be written at finalize"
    );

    // Fabricate the missing-object case (findings.md stays on disk).
    std::fs::remove_file(&obj_path).expect("delete findings object");

    // Detection-only: reported, auto-repairable, not yet repaired.
    let report = repo.doctor_json(false);
    let found = report["findings_store"]["findings"]
        .as_array()
        .expect("findings_store.findings array");
    assert!(
        found
            .iter()
            .any(|f| f["inconsistency_type"] == "missing_findings_object"
                && f["run_id"] == "findings-run"
                && f["repaired"] == false
                && f["manual_required"] == false),
        "missing findings object must be detected auto-repairable: {report}"
    );
    assert!(
        !obj_path.exists(),
        "detection-only must not rewrite the object"
    );

    // Repair: object rewritten from findings.md.
    let report = repo.doctor_json(true);
    assert!(
        report["findings_store"]["repaired"].as_u64().unwrap_or(0) >= 1,
        "--repair must rewrite the findings object: {report}"
    );
    assert!(
        obj_path.exists(),
        "the findings object must be restored by --repair"
    );

    // Idempotent: a second run finds no missing_findings_object.
    let report = repo.doctor_json(true);
    assert!(
        report["findings_store"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["inconsistency_type"] != "missing_findings_object"),
        "second repair run must be clean: {report}"
    );
}

/// A0-06 (codex P1-1): a findings blob already indexed under a different
/// `agent_*` tag (e.g. `agent_transcript`, because identical bytes were first
/// seen as a transcript) with the correct size must NOT be flagged as
/// `missing_findings_object_index` drift — otherwise doctor and the index
/// writer (which refuses to retag an `agent_*` row) fight a tag-war forever.
#[tokio::test]
async fn findings_object_index_tolerates_agent_transcript_tag() {
    use libra::internal::ai::review::{
        ReviewRunStore, ReviewTerminalState, store::RedactionReportSummary,
    };

    let repo = DoctorRepo::init();
    let store = ReviewRunStore::new(repo.repo.join(".libra").join("sessions"));
    let findings_body = "review finding body line\n";
    store
        .create_run("tag-run", &["codex".to_string()], "sha", "HEAD~1..HEAD")
        .expect("create run");
    store
        .write_findings("tag-run", findings_body)
        .expect("write findings");
    store
        .finalize_run(
            "tag-run",
            ReviewTerminalState::Success,
            &[],
            RedactionReportSummary::default(),
        )
        .expect("finalize objectizes findings");
    let manifest = store
        .load_manifest("tag-run")
        .expect("load manifest")
        .expect("manifest");
    let findings_oid = manifest.findings_oid.expect("findings_oid populated");

    // Resolve doctor's repo_id and pre-index the findings blob under the
    // agent_transcript tag with the correct payload size.
    let repo_id = {
        let conn = repo.db().await;
        let backend = conn.get_database_backend();
        conn.query_one_raw(Statement::from_string(
            backend,
            "SELECT value FROM config_kv WHERE key='libra.repoid'",
        ))
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get_by::<String, _>("value").ok())
        .unwrap_or_else(|| "unknown-repo".to_string())
    };
    repo.exec_sql(
        "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced) \
         VALUES (?, ?, ?, ?, ?, 0)
         ON CONFLICT(repo_id, o_id) DO UPDATE SET
           o_type = excluded.o_type, o_size = excluded.o_size",
        vec![
            findings_oid.clone().into(),
            "agent_transcript".into(),
            (findings_body.len() as i64).into(),
            repo_id.into(),
            0i64.into(),
        ],
    )
    .await;

    // A --repair run must NOT flag the size-matching agent_transcript row.
    let report = repo.doctor_json(true);
    let findings = report["findings_store"]["findings"]
        .as_array()
        .expect("findings_store.findings array");
    assert!(
        findings
            .iter()
            .all(|f| f["inconsistency_type"] != "missing_findings_object_index"),
        "a size-matching agent_* index row must not be flagged as findings drift: {report}"
    );
    // The tag is preserved — no tag-war rewrite.
    let rows = repo.object_index_rows(&[&findings_oid]).await;
    assert!(
        rows.iter()
            .any(|(_, o_type, _, _)| o_type == "agent_transcript"),
        "the pre-existing agent_transcript tag must be left intact: {rows:?}"
    );
}

// ---------------------------------------------------------------------------
// Repository-resolution and database-open error contract
// ---------------------------------------------------------------------------

/// A clean, isolated working directory that is NOT inside any Libra
/// repository, plus a fake `$HOME`. The environment mirrors
/// [`DoctorRepo::run_env`] so doctor and its sibling commands see identical
/// inputs.
struct OutsideRepo {
    _tempdir: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
}

impl OutsideRepo {
    fn new() -> Self {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let home = tempdir.path().join("home");
        let cwd = tempdir.path().join("work");
        std::fs::create_dir_all(&home).expect("create fake home");
        std::fs::create_dir_all(&cwd).expect("create work dir");
        Self {
            home: home.canonicalize().expect("canonical fake home"),
            cwd: cwd.canonicalize().expect("canonical work dir"),
            _tempdir: tempdir,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(args)
            .current_dir(&self.cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .stdin(Stdio::null())
            .output()
            .expect("spawn libra binary")
    }
}

/// Parse the structured `--json` error envelope (stderr only; stdout empty).
fn json_error_report(out: &Output) -> Value {
    assert!(
        !out.status.success(),
        "expected a failing command: {}",
        describe(out)
    );
    assert!(
        out.stdout.is_empty(),
        "structured JSON errors must not contaminate stdout: {}",
        describe(out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    serde_json::from_str(stderr.trim())
        .unwrap_or_else(|err| panic!("expected a JSON error on stderr ({err}): {stderr}"))
}

/// Doctor must report repository-resolution failures through the same
/// contract as every sibling repository command: the stable code, message
/// and remedies must be identical to `agent session list`'s.
fn assert_doctor_resolution_error_matches_sibling(
    doctor: &Value,
    sibling: &Value,
    expected_code: &str,
) {
    assert_eq!(doctor["ok"], json!(false), "doctor envelope: {doctor}");
    assert_eq!(doctor["error_code"], json!(expected_code), "{doctor}");
    assert_eq!(doctor["exit_code"], json!(128), "{doctor}");
    for field in ["error_code", "category", "exit_code", "message", "hints"] {
        assert_eq!(
            doctor[field], sibling[field],
            "doctor must keep the shared repository-resolution `{field}`:\ndoctor: {doctor}\nsibling: {sibling}"
        );
    }
}

#[test]
fn doctor_outside_repository_matches_repo_not_found_contract() {
    let outside = OutsideRepo::new();

    let human = outside.run(&["agent", "doctor"]);
    assert_eq!(human.status.code(), Some(128), "{}", describe(&human));
    assert!(human.stdout.is_empty(), "{}", describe(&human));
    let stderr = String::from_utf8_lossy(&human.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "{}",
        describe(&human)
    );
    assert!(stderr.contains("LBR-REPO-001"), "{}", describe(&human));
    assert!(stderr.contains("run 'libra init'"), "{}", describe(&human));
    assert!(!stderr.contains("LBR-INTERNAL-001"), "{}", describe(&human));
    assert!(
        !stderr.contains("could not resolve repository storage"),
        "{}",
        describe(&human)
    );

    let doctor = json_error_report(&outside.run(&["agent", "doctor", "--json"]));
    let sibling = json_error_report(&outside.run(&["agent", "session", "list", "--json"]));
    assert_doctor_resolution_error_matches_sibling(&doctor, &sibling, "LBR-REPO-001");
    assert_eq!(
        doctor["message"],
        json!("not a libra repository (or any of the parent directories): .libra")
    );
}

#[test]
fn doctor_in_git_repository_keeps_conversion_hint() {
    let outside = OutsideRepo::new();
    let git = outside.cwd.join(".git");
    std::fs::create_dir_all(git.join("objects")).expect("create .git/objects");
    std::fs::write(git.join("HEAD"), b"ref: refs/heads/main\n").expect("write .git/HEAD");
    std::fs::write(
        git.join("config"),
        b"[core]\n\trepositoryformatversion = 0\n",
    )
    .expect("write .git/config");

    let doctor = json_error_report(&outside.run(&["agent", "doctor", "--json"]));
    let sibling = json_error_report(&outside.run(&["agent", "session", "list", "--json"]));
    assert_doctor_resolution_error_matches_sibling(&doctor, &sibling, "LBR-REPO-001");
    assert_eq!(
        doctor["hints"][0],
        json!("run 'libra init --from-git-repository .' to convert this Git repository to Libra."),
        "{doctor}"
    );
}

#[test]
fn doctor_in_detached_linked_worktree_keeps_repo_state_remedy() {
    let outside = OutsideRepo::new();
    let gitdir = outside.cwd.join(".libra");
    std::fs::create_dir_all(&gitdir).expect("create linked gitdir");
    std::fs::write(gitdir.join("detached_from_registry"), b"").expect("write detached marker");

    let doctor = json_error_report(&outside.run(&["agent", "doctor", "--json"]));
    let sibling = json_error_report(&outside.run(&["agent", "session", "list", "--json"]));
    assert_doctor_resolution_error_matches_sibling(&doctor, &sibling, "LBR-REPO-003");
    let message = doctor["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("removed from the registry (detached)")
            && message.contains("libra worktree add"),
        "doctor must keep the verbatim detached-worktree remedy: {doctor}"
    );
}

#[test]
fn doctor_missing_repository_database_is_repo_corrupt_without_path() {
    let repo = DoctorRepo::init();
    std::fs::remove_file(repo.repo.join(".libra").join("libra.db"))
        .expect("remove repository database");

    let out = repo.run(&["agent", "doctor", "--json"], None);
    let report = json_error_report(&out);
    assert_eq!(report["error_code"], json!("LBR-REPO-002"), "{report}");
    assert_eq!(report["exit_code"], json!(128), "{report}");
    assert!(
        report["message"]
            .as_str()
            .is_some_and(|message| message.contains("repository database not found")),
        "{report}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains(repo.repo.to_str().expect("utf8 repo path")),
        "doctor's database error must not reveal the repository path: {stderr}"
    );
}

#[tokio::test]
async fn doctor_unsupported_object_format_is_repo_corrupt_without_value() {
    const CANARY: &str = "doctor-object-format-canary";
    let repo = DoctorRepo::init();
    repo.exec_sql(
        "DELETE FROM config_kv WHERE key = 'core.objectformat'",
        vec![],
    )
    .await;
    repo.exec_sql(
        "INSERT INTO config_kv (`key`, `value`, `encrypted`) VALUES ('core.objectformat', ?, 0)",
        vec![CANARY.into()],
    )
    .await;

    let out = repo.run(&["agent", "doctor", "--json"], None);
    let report = json_error_report(&out);
    assert_eq!(report["error_code"], json!("LBR-REPO-002"), "{report}");
    assert_eq!(report["exit_code"], json!(128), "{report}");
    assert!(
        report["message"]
            .as_str()
            .is_some_and(|message| message.contains("unsupported object format")),
        "{report}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains(CANARY),
        "doctor must not echo the stored object-format value: {stderr}"
    );
}

#[tokio::test]
async fn doctor_unopenable_repository_database_is_io_read_failed_without_path() {
    let repo = DoctorRepo::init();
    repo.exec_sql(
        "INSERT INTO schema_versions (version, name, applied_at) VALUES (?, ?, ?)",
        vec![
            9_999_999_999i64.into(),
            "future_schema_for_doctor_test".into(),
            "2026-10-03T00:00:00Z".into(),
        ],
    )
    .await;

    let out = repo.run(&["agent", "doctor", "--json"], None);
    let report = json_error_report(&out);
    assert_eq!(report["error_code"], json!("LBR-IO-001"), "{report}");
    assert_eq!(report["exit_code"], json!(128), "{report}");
    assert!(
        report["message"].as_str().is_some_and(|message| {
            message.contains("could not open the repository database")
                && message.contains("install a newer Libra binary")
        }),
        "doctor must keep the actionable newer-schema remedy: {report}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains(repo.repo.to_str().expect("utf8 repo path")),
        "doctor's database error must not reveal the repository path: {stderr}"
    );
}
