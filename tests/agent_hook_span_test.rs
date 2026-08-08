//! Fake-sink span assertions for the AG-19 hook-ingest spans
//! (`agent.hook.ingest` / `agent.redaction.apply`, plan.md Task A4).
//!
//! Lives in its own integration-test binary for the same reason as
//! `tests/agent_rpc_span_test.rs`: the assertions install a thread-local
//! `tracing` subscriber, and tracing's per-callsite interest cache can flap
//! when sibling threads in the same process evaluate the same callsites
//! without a subscriber — a single-test binary removes that concurrency by
//! construction.
//!
//! Spans do not cross process boundaries, so these tests drive
//! `libra::internal::ai::hooks::runtime::ingest_agent_traces_payload`
//! in-process (exported `pub` for exactly this purpose; not a stable API)
//! against a fresh on-disk SQLite database bootstrapped through the same
//! `create_database` path `libra init` uses.

use std::sync::Mutex;

use libra::internal::{
    ai::hooks::{
        LifecycleEventKind, ProviderHookCommand, claude_provider,
        runtime::ingest_agent_traces_payload,
    },
    config::ConfigKv,
};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use serde_json::json;

/// Shared in-memory sink handed to the fmt subscriber (identical to the
/// pattern in `tests/agent_rpc_span_test.rs`).
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

/// Run `f` under a thread-local fmt subscriber that records span-close
/// events (so fields recorded after span creation are visible) and return
/// everything the subscriber wrote.
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

/// Fresh production-shaped database: `create_database` runs the same
/// bootstrap SQL + built-in migrations `libra init` applies, so the
/// `agent_session` table the ingest writes to exists.
async fn fresh_conn(dir: &std::path::Path) -> sea_orm::DatabaseConnection {
    let db_path = dir.join("libra.db");
    let conn = libra::internal::db::create_database(&db_path.display().to_string())
        .await
        .expect("create fresh libra database");
    ConfigKv::set_with_conn(&conn, "libra.repoid", "agent-hook-span-test", false)
        .await
        .expect("seed repository identity");
    conn
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

/// Collect every `rules_hit=<n>` value in the captured output.
fn rules_hit_values(captured: &str) -> Vec<u64> {
    captured
        .match_indices("rules_hit=")
        .map(|(index, needle)| {
            let digits: String = captured[index + needle.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse().unwrap_or(0)
        })
        .collect()
}

/// A successful claude SessionStart ingest emits one `agent.hook.ingest`
/// span carrying the required AG-19 fields (provider, verb, event_kind,
/// frame_bytes, validated, partial) and one `agent.redaction.apply` span
/// whose `rules_hit` reflects the AWS-key-shaped secret in the prompt —
/// while the raw prompt text never reaches the sink.
#[test]
fn ingest_span_carries_required_fields_without_raw_input() {
    let rt = runtime();
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = rt.block_on(fresh_conn(dir.path()));

    let marker = "RAW-PROMPT-MARKER-4c1d9e";
    let payload = envelope(
        "SessionStart",
        "sess-span-fields",
        json!({ "prompt": format!("{marker} deploy with AKIAIOSFODNN7EXAMPLE please") }),
    );

    let captured = capture_spans(|| {
        rt.block_on(async {
            ingest_agent_traces_payload(
                &payload,
                ProviderHookCommand::SessionStart,
                LifecycleEventKind::SessionStart,
                claude_provider(),
                &conn,
                None,
            )
            .await
            .expect("session-start ingest succeeds");
        });
    });

    assert!(captured.contains("agent.hook.ingest"), "{captured}");
    for field in [
        "provider=\"claude\"",
        "verb=session-start",
        "event_kind=session_start",
        "validated=true",
        "partial=false",
        "frame_bytes=",
    ] {
        assert!(
            captured.contains(field),
            "ingest span missing `{field}`: {captured}"
        );
    }
    assert!(
        !captured.contains("frame_bytes=0"),
        "frame_bytes must reflect the non-empty payload: {captured}"
    );

    for field in [
        "agent.redaction.apply",
        "size_cap_triggered=false",
        "fail_closed=false",
    ] {
        assert!(
            captured.contains(field),
            "redaction span missing `{field}`: {captured}"
        );
    }
    let rules_hit = rules_hit_values(&captured);
    assert!(
        rules_hit.iter().any(|&hits| hits >= 1),
        "redaction span must report rules_hit>=1 for the AWS-key prompt, got {rules_hit:?}: {captured}"
    );

    assert!(
        !captured.contains(marker),
        "raw prompt text must never reach the span sink: {captured}"
    );
    assert!(
        !captured.contains("AKIAIOSFODNN7EXAMPLE"),
        "the raw secret must never reach the span sink: {captured}"
    );
}

/// An event name this build does not recognize is skipped-and-logged: the
/// ingest span records `partial=true` and a warn event carries
/// `reason="unknown_event_type"` — no error, no panic.
#[test]
fn unknown_event_records_partial_true_with_warn_reason() {
    let rt = runtime();
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = rt.block_on(fresh_conn(dir.path()));

    let payload = envelope("FutureFancyEvent", "sess-span-unknown", json!({}));

    let captured = capture_spans(|| {
        rt.block_on(async {
            ingest_agent_traces_payload(
                &payload,
                ProviderHookCommand::Stop,
                LifecycleEventKind::TurnEnd,
                claude_provider(),
                &conn,
                None,
            )
            .await
            .expect("unknown event name must skip, not fail");
        });
    });

    for field in [
        "agent.hook.ingest",
        "validated=true",
        "partial=true",
        "WARN",
        "reason=\"unknown_event_type\"",
        "hook_event_name=FutureFancyEvent",
    ] {
        assert!(
            captured.contains(field),
            "unknown-event capture missing `{field}`: {captured}"
        );
    }
    assert!(
        !captured.contains("partial=false"),
        "the skipped ingest must not also record partial=false: {captured}"
    );
}

/// An invalid envelope (path-traversal session id) fails validation before
/// any write; the ingest span records `validated=false` and never flips to
/// `validated=true`, and the offending session id is not echoed.
#[test]
fn invalid_envelope_records_validated_false() {
    let rt = runtime();
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = rt.block_on(fresh_conn(dir.path()));

    let payload = envelope("SessionStart", "../../evil", json!({}));

    let captured = capture_spans(|| {
        rt.block_on(async {
            ingest_agent_traces_payload(
                &payload,
                ProviderHookCommand::SessionStart,
                LifecycleEventKind::SessionStart,
                claude_provider(),
                &conn,
                None,
            )
            .await
            .expect_err("path-traversal session id must fail validation");
        });
    });

    assert!(captured.contains("agent.hook.ingest"), "{captured}");
    assert!(
        captured.contains("validated=false"),
        "rejected envelope must record validated=false: {captured}"
    );
    assert!(
        !captured.contains("validated=true"),
        "rejected envelope must never record validated=true: {captured}"
    );
    assert!(
        !captured.contains("../../evil"),
        "the invalid session id must not be echoed into the span sink: {captured}"
    );
}

/// A provider id is a global external identity, not an adapter-local key. An
/// export claim left by another scope must therefore prevent hook capture
/// before it can create a second, cross-workspace `agent_session` claim.
#[test]
fn foreign_export_claim_blocks_hook_capture() {
    let rt = runtime();
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = rt.block_on(fresh_conn(dir.path()));
    let provider_session_id = "sess-span-foreign-export";

    rt.block_on(async {
        conn.execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "INSERT INTO agent_export_job (
                job_id, agent_kind, provider_session_id, observed_generation,
                processed_generation, state, created_at, updated_at, ttl_expires_at,
                repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES (
                'foreign-export-job', 'opencode', 'sess-span-foreign-export', 0,
                0, 'idle', 1, 1, 9999999999999,
                'other-repo', 'foreign-worktree', NULL, NULL, 'scoped'
             )",
        ))
        .await
        .expect("seed foreign export claim");

        let error = ingest_agent_traces_payload(
            &envelope("SessionStart", provider_session_id, json!({})),
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect_err("foreign provider claim must reject hook capture");
        assert!(
            format!("{error:#}").contains("already claimed by another"),
            "unexpected ownership error: {error:#}"
        );

        let session = conn
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT 1 FROM agent_session
                 WHERE provider_session_id = 'sess-span-foreign-export'",
            ))
            .await
            .expect("query hook session after refused capture");
        assert!(
            session.is_none(),
            "a rejected hook must not create a competing session claim"
        );
    });
}
