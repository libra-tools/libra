//! plan-20260713 DR-05c-0 — live coverage-gate integration tests.
//!
//! Drives the real CLI hook entry (`libra agent hooks claude-code stop`)
//! end-to-end against a tempdir repo + fake `LIBRA_TEST_HOME`, then inspects
//! the checkpoint catalog (CLI JSON) and the `agent_coverage_claim` /
//! `agent_coverage_revision` tables (direct read-only SQLite) to pin the
//! gate's externally observable guarantees:
//!
//! - a repeated TurnEnd over unchanged content appends NO second checkpoint
//!   (`live_repeat_turn_noops_via_coverage_gate`);
//! - a truncated turn later completed advances ONE claim through two
//!   revisions — both checkpoints stay visible, supersede lives in the
//!   revision table, never on `agent_checkpoint`
//!   (`coverage_same_turn_truncated_then_complete_single_current_revision`);
//! - concurrent writers on the same content produce exactly one append
//!   (`coverage_gate_concurrent_writers_single_append`);
//! - a still-live foreign reservation is a visible replayable error, never a
//!   silent successful no-op (`coverage_gate_inflight_only_fails_visibly`);
//! - a missing/broken claim gate fails the write CLOSED — no ungated append
//!   (`coverage_gate_db_error_does_not_append`).

use std::{
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{Value, json};
use tempfile::TempDir;

struct HookRepo {
    _tmp: TempDir,
    repo: PathBuf,
    home: PathBuf,
}

impl HookRepo {
    fn init() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let this = Self {
            _tmp: tmp,
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

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .env_remove("CODEX_HOME");
        cmd
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        let mut cmd = self.command();
        cmd.args(args);
        if let Some(input) = stdin {
            cmd.stdin(Stdio::piped());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
            let mut child = cmd.spawn().expect("spawn libra");
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .expect("stdin piped")
                .write_all(input.as_bytes())
                .expect("write stdin");
            child.wait_with_output().expect("wait libra")
        } else {
            cmd.output().expect("run libra")
        }
    }

    fn hook(&self, envelope: &str) -> Output {
        self.run(&["agent", "hooks", "claude-code", "stop"], Some(envelope))
    }

    fn hook_with_env(&self, envelope: &str, key: &str, value: &str) -> Output {
        let mut cmd = self.command();
        cmd.env(key, value)
            .args(["agent", "hooks", "claude-code", "stop"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn hook with test environment");
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin piped")
            .write_all(envelope.as_bytes())
            .expect("write hook envelope");
        child.wait_with_output().expect("wait hook")
    }

    fn spawn_hook(&self) -> std::process::Child {
        let mut cmd = self.command();
        cmd.args(["agent", "hooks", "claude-code", "stop"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.spawn().expect("spawn hook")
    }

    fn checkpoints(&self) -> Vec<Value> {
        let out = self.run(&["agent", "checkpoint", "list", "--json"], None);
        assert!(out.status.success(), "checkpoint list: {}", describe(&out));
        let parsed: Value =
            serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
        parsed["data"]["checkpoints"]
            .as_array()
            .expect("checkpoints array")
            .clone()
    }

    fn envelope(&self, session_id: &str, transcript_path: &Path) -> String {
        json!({
            "hook_event_name": "Stop",
            "session_id": session_id,
            "cwd": self.repo.to_string_lossy(),
            "transcript_path": transcript_path.to_string_lossy(),
        })
        .to_string()
    }

    /// Write a coverage-v1-parseable Claude transcript under the fake
    /// `~/.claude` (provider-root trust gate accepts it).
    fn write_transcript(&self, name: &str, content: &str) -> PathBuf {
        let dir = self.home.join(".claude").join("projects").join("x");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    async fn db(&self) -> DatabaseConnection {
        let url = format!(
            "sqlite://{}?mode=ro",
            self.repo.join(".libra").join("libra.db").display()
        );
        Database::connect(url).await.expect("open libra.db")
    }

    async fn query_rows(&self, sql: &str) -> Vec<sea_orm::QueryResult> {
        let conn = self.db().await;
        conn.query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("query")
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

const TURN_COMPLETE: &str = concat!(
    r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"run it"}}"#,
    "\n",
    r#"{"type":"assistant","uuid":"a1","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
    "\n",
);

/// Same logical turn (`u1`) but with a truncated assistant line — parses as
/// `incomplete` with a different digest.
const TURN_TRUNCATED: &str = concat!(
    r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"run it"}}"#,
    "\n",
    r#"{"type":"assistant","uuid":"a1","message":{"role":"assistant","content":[{"type":"te"#,
);

#[tokio::test]
async fn live_repeat_turn_noops_via_coverage_gate() {
    let repo = HookRepo::init();
    let transcript = repo.write_transcript("s1.jsonl", TURN_COMPLETE);
    let envelope = repo.envelope("sess-repeat", &transcript);

    let first = repo.hook(&envelope);
    assert!(first.status.success(), "first stop: {}", describe(&first));
    assert_eq!(repo.checkpoints().len(), 1, "first stop appends once");

    // Identical content again: the gate must no-op, not append a duplicate.
    let second = repo.hook(&envelope);
    assert!(
        second.status.success(),
        "second stop: {}",
        describe(&second)
    );
    assert_eq!(
        repo.checkpoints().len(),
        1,
        "repeated TurnEnd over unchanged content must not append a second checkpoint"
    );

    // One committed claim, revision 1, and exactly one revision row.
    let claims = repo
        .query_rows(
            "SELECT state, revision, completeness FROM agent_coverage_claim \
             WHERE logical_turn_key = 'u1'",
        )
        .await;
    assert_eq!(claims.len(), 1);
    let state: String = claims[0].try_get_by("state").unwrap();
    let revision: i64 = claims[0].try_get_by("revision").unwrap();
    assert_eq!(state, "catalog_committed");
    assert_eq!(revision, 1);
    let revisions = repo
        .query_rows("SELECT revision FROM agent_coverage_revision WHERE logical_turn_key = 'u1'")
        .await;
    assert_eq!(revisions.len(), 1);
}

#[tokio::test]
async fn coverage_same_turn_truncated_then_complete_single_current_revision() {
    let repo = HookRepo::init();
    let transcript = repo.write_transcript("s2.jsonl", TURN_TRUNCATED);
    let envelope = repo.envelope("sess-upgrade", &transcript);

    let first = repo.hook(&envelope);
    assert!(first.status.success(), "first stop: {}", describe(&first));
    assert_eq!(repo.checkpoints().len(), 1);

    // The agent finishes flushing: same logical turn, now complete.
    repo.write_transcript("s2.jsonl", TURN_COMPLETE);
    let second = repo.hook(&envelope);
    assert!(
        second.status.success(),
        "second stop: {}",
        describe(&second)
    );

    // Both checkpoints stay visible (ADR-DR-16: no checkpoint-level
    // supersede) …
    assert_eq!(
        repo.checkpoints().len(),
        2,
        "upgrade appends a new checkpoint without hiding the old one"
    );
    // … while the CLAIM advanced through two revisions to a single current
    // complete one.
    let claims = repo
        .query_rows(
            "SELECT state, revision, completeness FROM agent_coverage_claim \
             WHERE logical_turn_key = 'u1'",
        )
        .await;
    assert_eq!(claims.len(), 1, "one claim row per logical turn");
    let state: String = claims[0].try_get_by("state").unwrap();
    let revision: i64 = claims[0].try_get_by("revision").unwrap();
    let completeness: String = claims[0].try_get_by("completeness").unwrap();
    assert_eq!(state, "catalog_committed");
    assert_eq!(revision, 2, "incomplete→complete advanced the revision");
    assert_eq!(completeness, "complete");
    let revisions = repo
        .query_rows(
            "SELECT revision, completeness FROM agent_coverage_revision \
             WHERE logical_turn_key = 'u1' ORDER BY revision",
        )
        .await;
    assert_eq!(revisions.len(), 2, "append-only history keeps both");
    let r1: String = revisions[0].try_get_by("completeness").unwrap();
    let r2: String = revisions[1].try_get_by("completeness").unwrap();
    assert_eq!((r1.as_str(), r2.as_str()), ("incomplete", "complete"));

    // A third stop with the same complete content: no third append.
    let third = repo.hook(&envelope);
    assert!(third.status.success(), "third stop: {}", describe(&third));
    assert_eq!(repo.checkpoints().len(), 2);
}

#[tokio::test]
async fn coverage_gate_concurrent_writers_single_append() {
    let repo = HookRepo::init();
    let transcript = repo.write_transcript("s3.jsonl", TURN_COMPLETE);
    // Session must exist before the race so both writers contend on the
    // claim gate rather than on session creation.
    let start_envelope = json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-race",
        "cwd": repo.repo.to_string_lossy(),
        "transcript_path": transcript.to_string_lossy(),
    })
    .to_string();
    let start = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&start_envelope),
    );
    assert!(
        start.status.success(),
        "session-start: {}",
        describe(&start)
    );

    let envelope = repo.envelope("sess-race", &transcript);
    let mut children = Vec::new();
    for _ in 0..2 {
        let mut child = repo.spawn_hook();
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(envelope.as_bytes())
            .expect("write envelope");
        drop(child.stdin.take());
        children.push(child);
    }
    let mut successes = 0;
    for child in children {
        let out = child.wait_with_output().expect("wait hook");
        if out.status.success() {
            successes += 1;
        } else {
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("another live writer"),
                "loser must fail with a replayable in-flight diagnostic: {}",
                describe(&out)
            );
        }
    }
    assert!(successes >= 1, "one concurrent writer must commit");

    assert_eq!(
        repo.checkpoints().len(),
        1,
        "exactly one checkpoint for the same content under concurrency"
    );
    let claims = repo
        .query_rows("SELECT state FROM agent_coverage_claim WHERE logical_turn_key = 'u1'")
        .await;
    assert_eq!(claims.len(), 1);
    let state: String = claims[0].try_get_by("state").unwrap();
    assert_eq!(state, "catalog_committed");
}

#[tokio::test]
async fn coverage_gate_inflight_only_fails_visibly() {
    let repo = HookRepo::init();
    let transcript = repo.write_transcript("s-inflight.jsonl", TURN_COMPLETE);
    let start_envelope = json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-inflight",
        "cwd": repo.repo.to_string_lossy(),
        "transcript_path": transcript.to_string_lossy(),
    })
    .to_string();
    let start = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&start_envelope),
    );
    assert!(
        start.status.success(),
        "session-start: {}",
        describe(&start)
    );

    let url = format!(
        "sqlite://{}?mode=rw",
        repo.repo.join(".libra").join("libra.db").display()
    );
    let conn = Database::connect(url).await.expect("open rw");
    let inserted = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_coverage_claim (
                session_id, logical_turn_key, coverage_schema_version,
                coverage_digest, completeness, revision, state, owner,
                lease_expires_at, fence_token, source_channel, created_at, updated_at
             ) SELECT session_id, 'u1', 1, 'foreign-digest', 'complete', 0,
                      'reserved_live', 'foreign-owner', 4102444800000, 1, 'live', 0, 0
               FROM agent_session WHERE provider_session_id = ?",
            ["sess-inflight".into()],
        ))
        .await
        .expect("insert foreign reservation");
    assert_eq!(inserted.rows_affected(), 1);

    let blocked = repo.hook(&repo.envelope("sess-inflight", &transcript));
    assert!(
        !blocked.status.success(),
        "an uncovered in-flight turn must not report success: {}",
        describe(&blocked)
    );
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("another live writer"),
        "failure must tell the caller to retry: {}",
        describe(&blocked)
    );
    assert!(
        repo.checkpoints().is_empty(),
        "no checkpoint is durably covered while the foreign lease is active"
    );
}

/// ADR-DR-09: a live/export writer may preempt an import reservation even
/// while its lease is nominally live. The fence increments, so the stale
/// importer cannot later publish its already-constructed ref/catalog plan.
#[tokio::test]
async fn live_writer_preempts_reserved_import_and_fences_stale_owner() {
    let repo = HookRepo::init();
    let transcript = repo.write_transcript("s-import-preempt.jsonl", TURN_COMPLETE);
    let start_envelope = json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-import-preempt",
        "cwd": repo.repo.to_string_lossy(),
        "transcript_path": transcript.to_string_lossy(),
    })
    .to_string();
    let start = repo.run(
        &["agent", "hooks", "claude-code", "session-start"],
        Some(&start_envelope),
    );
    assert!(
        start.status.success(),
        "session-start: {}",
        describe(&start)
    );

    let url = format!(
        "sqlite://{}?mode=rw",
        repo.repo.join(".libra").join("libra.db").display()
    );
    let conn = Database::connect(url).await.expect("open rw");
    let inserted = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_coverage_claim (
                session_id, logical_turn_key, coverage_schema_version,
                coverage_digest, completeness, revision, state, owner,
                lease_expires_at, fence_token, source_channel, created_at, updated_at
             ) SELECT session_id, 'u1', 1, 'import-digest', 'complete', 0,
                      'reserved_import', 'stale-import', 4102444800000, 7, 'import', 0, 0
               FROM agent_session WHERE provider_session_id = ?",
            ["sess-import-preempt".into()],
        ))
        .await
        .expect("insert import reservation");
    assert_eq!(inserted.rows_affected(), 1);
    drop(conn);

    let live = repo.hook(&repo.envelope("sess-import-preempt", &transcript));
    assert!(
        live.status.success(),
        "live preemption: {}",
        describe(&live)
    );
    assert_eq!(repo.checkpoints().len(), 1);
    let claims = repo
        .query_rows(
            "SELECT state, fence_token, source_channel FROM agent_coverage_claim \
             WHERE logical_turn_key = 'u1'",
        )
        .await;
    assert_eq!(claims.len(), 1);
    assert_eq!(
        claims[0].try_get_by::<String, _>("state").unwrap(),
        "catalog_committed"
    );
    assert_eq!(claims[0].try_get_by::<i64, _>("fence_token").unwrap(), 8);
    assert_eq!(
        claims[0].try_get_by::<String, _>("source_channel").unwrap(),
        "live"
    );
}

#[tokio::test]
async fn coverage_gate_db_error_does_not_append() {
    let repo = HookRepo::init();
    let transcript = repo.write_transcript("s4.jsonl", TURN_COMPLETE);
    let envelope = repo.envelope("sess-failclosed", &transcript);

    // Healthy first write.
    let first = repo.hook(&envelope);
    assert!(first.status.success(), "first stop: {}", describe(&first));
    assert_eq!(repo.checkpoints().len(), 1);

    // Break the gate: drop the claim table (simulates schema/gate
    // unavailability). The next gated write must fail CLOSED — error out,
    // no ungated append.
    {
        let url = format!(
            "sqlite://{}?mode=rw",
            repo.repo.join(".libra").join("libra.db").display()
        );
        let conn = Database::connect(url).await.expect("open rw");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "DROP TABLE agent_coverage_claim".to_string(),
        ))
        .await
        .expect("drop claim table");
    }

    let new_content = TURN_COMPLETE.replace("run it", "run it again");
    repo.write_transcript("s4.jsonl", &new_content);
    let broken = repo.hook(&envelope);
    assert!(
        !broken.status.success(),
        "gate unavailable must fail the write closed, got: {}",
        describe(&broken)
    );
    assert_eq!(
        repo.checkpoints().len(),
        1,
        "no checkpoint may be appended without passing the coverage gate"
    );
}

#[tokio::test]
async fn live_failure_after_marker_registration_releases_claim_and_marker() {
    let repo = HookRepo::init();
    let transcript = repo.write_transcript("registered-failure.jsonl", TURN_COMPLETE);
    let envelope = repo.envelope("sess-registered-failure", &transcript);
    let failed = repo.hook_with_env(
        &envelope,
        "LIBRA_TEST_CHECKPOINT_FAIL_AFTER_REGISTRATION",
        "1",
    );
    assert!(!failed.status.success(), "{}", describe(&failed));
    assert!(repo.checkpoints().is_empty());
    let claims = repo
        .query_rows("SELECT state, owner, lease_expires_at FROM agent_coverage_claim")
        .await;
    assert_eq!(claims.len(), 1);
    assert_eq!(
        claims[0].try_get_by::<String, _>("state").unwrap(),
        "abandoned"
    );
    assert!(
        claims[0]
            .try_get_by::<Option<String>, _>("owner")
            .unwrap()
            .is_none()
    );
    assert!(
        repo.query_rows("SELECT `key` FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await
            .is_empty(),
        "registered live failure leaked its marker"
    );
}
