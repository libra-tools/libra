//! Fake-sink span assertions for the AG-20 checkpoint-writer span
//! (`agent.checkpoint.write`, plan.md Task A5 / agent.md §6).
//!
//! Lives in its own integration-test binary for the same reason as
//! `tests/agent_hook_span_test.rs`: the assertions install a thread-local
//! `tracing` subscriber, and tracing's per-callsite interest cache can flap
//! when sibling threads in the same process evaluate the same callsites
//! without a subscriber — a single-test binary removes that concurrency by
//! construction.
//!
//! Contract under test (§6 table): the span carries `checkpoint_id`,
//! `session_id`, `stage` (progression; recorded through to `done`),
//! `cas_retries`, and `object_count`; the transcript body must NEVER reach
//! the sink.

use std::sync::Mutex;

use libra::internal::{
    ai::hooks::{
        LifecycleEventKind, ProviderHookCommand, claude_provider,
        runtime::ingest_agent_traces_payload,
    },
    config::ConfigKv,
};
use serde_json::json;

/// Shared in-memory sink handed to the fmt subscriber (identical to the
/// pattern in `tests/agent_hook_span_test.rs`).
#[derive(Clone, Default)]
struct Sink(std::sync::Arc<Mutex<Vec<u8>>>);

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

fn envelope(hook_event_name: &str, session_id: &str, extra: serde_json::Value) -> Vec<u8> {
    let mut base = json!({
        "hook_event_name": hook_event_name,
        "session_id": session_id,
        "cwd": "/tmp/repo",
    });
    if let (serde_json::Value::Object(extra_map), Some(base_map)) = (extra, base.as_object_mut()) {
        for (key, value) in extra_map {
            base_map.insert(key, value);
        }
    }
    serde_json::to_vec(&base).expect("serialize envelope")
}

/// A SessionEnd ingest that materialises a checkpoint emits one
/// `agent.checkpoint.write` span with the §6 required fields; the stage
/// progression ends at `done`, and neither the transcript body (here the
/// fallback redacted prompt) nor the raw secret leaks into the sink.
#[test]
fn checkpoint_write_span_carries_required_fields_without_transcript_body() {
    let rt = runtime();
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_path = dir.path().to_path_buf();
    let db_path = repo_path.join("libra.db");
    let conn = rt.block_on(async {
        let conn = libra::internal::db::create_database(&db_path.display().to_string())
            .await
            .expect("create fresh libra database");
        ConfigKv::set_with_conn(&conn, "libra.repoid", "agent-checkpoint-span-test", false)
            .await
            .expect("seed repository identity");
        conn
    });

    // Unique marker: with no trusted transcript path, the writer falls
    // back to the redacted prompt as the transcript body — so the marker
    // reaching the sink would mean the span leaked transcript content.
    let body_marker = "TRANSCRIPT-BODY-MARKER-77aa";
    let start = envelope("SessionStart", "sess-cp-span", json!({}));
    let end = envelope(
        "SessionEnd",
        "sess-cp-span",
        json!({ "prompt": format!("{body_marker} finish with AKIAIOSFODNN7EXAMPLE") }),
    );

    let captured = capture_spans(|| {
        rt.block_on(async {
            ingest_agent_traces_payload(
                &start,
                ProviderHookCommand::SessionStart,
                LifecycleEventKind::SessionStart,
                claude_provider(),
                &conn,
                Some(&repo_path),
            )
            .await
            .expect("session-start ingest succeeds");
            ingest_agent_traces_payload(
                &end,
                ProviderHookCommand::SessionEnd,
                LifecycleEventKind::SessionEnd,
                claude_provider(),
                &conn,
                Some(&repo_path),
            )
            .await
            .expect("session-end ingest succeeds");
        });
    });

    assert!(
        captured.contains("agent.checkpoint.write"),
        "checkpoint write span missing: {captured}"
    );
    for field in [
        "checkpoint_id=",
        "session_id=claude__sess-cp-span",
        // Stage progression: recorded values are quoted by the fmt layer.
        "stage=\"marker\"",
        "stage=\"append\"",
        "stage=\"ref_cas_done\"",
        "stage=\"catalog\"",
        "stage=\"done\"",
        "cas_retries=0",
        "object_count=",
    ] {
        assert!(
            captured.contains(field),
            "checkpoint write span missing `{field}`: {captured}"
        );
    }
    assert!(
        !captured.contains("object_count=0"),
        "object_count must reflect the written objects: {captured}"
    );

    // Forbidden content: transcript body and raw secret.
    assert!(
        !captured.contains(body_marker),
        "transcript body must never reach the span sink: {captured}"
    );
    assert!(
        !captured.contains("AKIAIOSFODNN7EXAMPLE"),
        "the raw secret must never reach the span sink: {captured}"
    );
}
