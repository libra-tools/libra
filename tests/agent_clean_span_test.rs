//! Fake-sink span assertions for the AG-20 prune span
//! (`agent.clean.prune`, plan.md Task A5 / agent.md §6).
//!
//! Lives in its own integration-test binary for the same reason as
//! `tests/agent_checkpoint_span_test.rs`: the assertions install a
//! thread-local `tracing` subscriber, and tracing's per-callsite interest
//! cache can flap when sibling threads in the same process evaluate the
//! same callsites without a subscriber — a single-test binary removes that
//! concurrency by construction.
//!
//! Contract under test (§6 table): the span carries `deleted_objects`,
//! `deleted_sessions`, `window_guard`, and `duration_ms`; no raw
//! filesystem path (the repo tempdir here) may reach the sink.

use std::sync::{Arc, Mutex};

use libra::{
    internal::ai::{
        history::{CheckpointCommitParams, CheckpointScope, HistoryManager},
        observed_agents::Redactor,
    },
    utils::client_storage::ClientStorage,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, Value};

/// Shared in-memory sink handed to the fmt subscriber (identical to the
/// pattern in `tests/agent_checkpoint_span_test.rs`).
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
    type Writer = Sink;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn capture_spans<F: FnOnce()>(f: F) -> String {
    let sink = Sink::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(sink.clone())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let captured = sink.0.lock().unwrap();
    String::from_utf8_lossy(&captured).to_string()
}

/// Single-threaded runtime so every task (including the sqlx pool's) polls
/// on this thread, where the thread-local subscriber is installed.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread tokio runtime")
}

async fn seed_temporary_checkpoint(
    conn: &DatabaseConnection,
    repo_path: &std::path::Path,
    checkpoint_id: &str,
) {
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, stopped_at
         ) VALUES ('span-session', 'claude_code', 'provider-span', 'stopped',
                   '/tmp/libra-agent-clean-span-test', '{}', '{}', 10, 20, 30)",
        [],
    ))
    .await
    .expect("insert agent_session");

    let storage = Arc::new(ClientStorage::init(repo_path.join("objects")));
    let history = HistoryManager::new_with_ref(
        storage,
        repo_path.to_path_buf(),
        Arc::new(conn.clone()),
        libra::internal::branch::TRACES_BRANCH,
    );
    let redactor = Redactor::new_default();
    let (redacted, _) = redactor.redact(b"transcript for the prune span test");
    let (meta_redacted, _) = redactor.redact(br#"{"checkpoint_id":"span"}"#);
    let (events_redacted, _) = redactor.redact(b"{}\n");
    let (report_redacted, _) = redactor.redact(b"{}");
    let written = history
        .append_checkpoint_commit(CheckpointCommitParams {
            checkpoint_id,
            session_id: "span-session",
            agent_kind: "claude_code",
            parent_commit: None,
            scope: CheckpointScope::Temporary,
            tool_use_id: None,
            metadata_json: &meta_redacted,
            transcript_redacted: &redacted,
            lifecycle_events_jsonl: &events_redacted,
            redaction_report_json: &report_redacted,
            txn_extra: None,
        })
        .await
        .expect("append temporary checkpoint commit");

    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, parent_commit, tree_oid,
            metadata_blob_oid, traces_commit, created_at
         ) VALUES (?, 'span-session', 'temporary', NULL, ?, ?, ?, 40)",
        vec![
            Value::from(checkpoint_id),
            Value::from(written.tree_oid.to_string()),
            Value::from(written.metadata_blob_oid.to_string()),
            Value::from(written.commit_hash.to_string()),
        ],
    ))
    .await
    .expect("insert agent_checkpoint row");
}

/// A prune that removes a temporary checkpoint emits one
/// `agent.clean.prune` span carrying the §6 required fields
/// (`deleted_objects`, `deleted_sessions`, `window_guard`, `duration_ms`),
/// with the window guards verified — and never leaks the repository path
/// into the sink.
#[test]
fn clean_prune_span_carries_required_fields_without_raw_paths() {
    let rt = runtime();
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_path = dir.path().to_path_buf();
    let db_path = repo_path.join("libra.db");
    let conn = rt.block_on(async {
        libra::internal::db::create_database(&db_path.display().to_string())
            .await
            .expect("create fresh libra database")
    });

    let checkpoint_id = "aa000000-0000-4000-8000-0000000000aa";
    rt.block_on(seed_temporary_checkpoint(&conn, &repo_path, checkpoint_id));

    let captured = capture_spans(|| {
        rt.block_on(async {
            let storage = Arc::new(ClientStorage::init(repo_path.join("objects")));
            let history = HistoryManager::new_with_ref(
                storage,
                repo_path.clone(),
                Arc::new(conn.clone()),
                libra::internal::branch::TRACES_BRANCH,
            );
            let outcome = history
                .prune_checkpoint_commits(&[checkpoint_id.to_string()])
                .await
                .expect("prune succeeds");
            assert_eq!(outcome.removed_checkpoints, 1);
            assert_eq!(outcome.window_guard, "markers_and_catalog_verified");
        });
    });

    assert!(
        captured.contains("agent.clean.prune"),
        "prune span missing: {captured}"
    );
    for field in [
        "deleted_objects=",
        "deleted_sessions=0",
        "window_guard=\"markers_and_catalog_verified\"",
        "duration_ms=",
    ] {
        assert!(
            captured.contains(field),
            "prune span missing `{field}`: {captured}"
        );
    }

    // Forbidden content: no raw filesystem path may reach the sink.
    let repo_path_text = repo_path.display().to_string();
    assert!(
        !captured.contains(&repo_path_text),
        "the repository path must never reach the span sink: {captured}"
    );
}
