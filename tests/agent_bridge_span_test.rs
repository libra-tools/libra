//! `tests/agent_bridge_span_test.rs` — fake-sink span assertion for
//! `libra agent bridge --stdio` (plan-20260818 LB-03, GC-LB-10).
//!
//! One tracing span per bridge request must carry stable, low-cardinality
//! fields (method, request id, repository scope) and must never carry the raw
//! request payload, prompt, token, or response body (GC-LB-08). Lives in its
//! own integration-test binary so the span sink is not shared with other
//! suites.
//!
//! The sink is installed as the **process-global** subscriber and the two cases
//! are serialized. A thread-local default (`subscriber::with_default`) made the
//! assertions flaky: `tracing` caches each callsite's interest, and when that
//! cache was populated before a thread-local default existed — or from a worker
//! thread without one — the span events were skipped everywhere and the capture
//! came back empty (`span name missing on error path: `). Registering a global
//! default rebuilds the interest cache, so the events are always recorded.

#![cfg(unix)]

use std::{
    io::Cursor,
    sync::{Mutex, OnceLock},
};

use libra::{
    command::agent::bridge::IngressBridgeHandler,
    internal::{
        ai::agent_bridge::{ingress::BridgeContext, transport::run},
        db::migration::run_builtin_migrations,
    },
};
use sea_orm::Database;

/// Captured `tracing` output for the case currently running.
static SINK: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// Serializes the cases so neither observes the other's span events.
static CASE_LOCK: Mutex<()> = Mutex::new(());

struct Sink;
impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        SINK.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
    type Writer = Sink;
    fn make_writer(&'a self) -> Self::Writer {
        Sink
    }
}

/// Install the process-global subscriber exactly once.
fn install_subscriber() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(Sink)
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("install the process-global span sink");
    });
}

/// Build an in-memory store and drive one request batch through the bridge
/// handler under the fake `tracing` subscriber, returning the captured output.
fn capture(input: &str, repo_id: &str) -> (String, String) {
    let _case = CASE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    install_subscriber();
    SINK.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();

    let mut out = Vec::new();
    let rt = tokio::runtime::Runtime::new().expect("rt");
    // Dispatch every request through the handler before reading the sink back.
    rt.block_on(async {
        let db = Database::connect("sqlite::memory:").await.expect("connect");
        run_builtin_migrations(&db).await.expect("apply migrations");

        let handler = IngressBridgeHandler::new(BridgeContext {
            repo: db.clone(),
            conn: db.clone(),
            repository_id: repo_id.to_string(),
            worktree_id: None,
        });
        run(
            Cursor::new(input.as_bytes()),
            &mut out,
            &handler,
            std::time::Duration::from_secs(30),
        )
        .await
        .expect("transport runs");
    });

    let captured =
        String::from_utf8_lossy(&SINK.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
            .to_string();
    let stdout = String::from_utf8(out).expect("stdout utf8");
    (captured, stdout)
}

/// The span emitted for a handled request carries method + id + repository
/// scope and never the raw payload.
#[test]
fn request_span_carries_scope_but_never_payload() {
    // A real event.append with a payload that must not leak into the span.
    let input = r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}
{"jsonrpc":"2.0","method":"event.append","params":{"session_id":"s1","events":[{"seq":1,"type":"tool/result","payload":"TOP-SECRET-TOKEN-abc123"}]},"id":2}
{"jsonrpc":"2.0","method":"status.get","id":3}
"#;
    let (captured, stdout) = capture(input, "repo-1");

    // Sanity: the transport actually served the requests.
    assert!(
        stdout.contains("\"id\":2"),
        "event.append answered: {stdout}"
    );

    assert!(
        captured.contains("agent.bridge.request"),
        "span name missing: {captured}"
    );
    for field in [
        "method=session.open",
        "method=event.append",
        "method=status.get",
        "id=Some(Number(1))",
        "id=Some(Number(2))",
        "id=Some(Number(3))",
        "repository_id=repo-1",
    ] {
        assert!(
            captured.contains(field),
            "span missing `{field}`: {captured}"
        );
    }
    assert!(
        !captured.contains("TOP-SECRET-TOKEN-abc123"),
        "span must not leak the raw payload: {captured}"
    );
}

/// A rejected/errored request still emits (and closes) its span — the tracing
/// guard is released even on failure (GC-LB-10).
#[test]
fn failed_request_still_closes_its_span() {
    // event.append on a session that is not open -> error, but the span must
    // still be emitted and closed.
    let input = r#"{"jsonrpc":"2.0","method":"event.append","params":{"session_id":"nope","events":[]},"id":5}
"#;
    let (captured, stdout) = capture(input, "repo-1");

    assert!(
        stdout.contains("\"error\""),
        "failed request returns an error frame: {stdout}"
    );
    assert!(
        captured.contains("agent.bridge.request"),
        "span name missing on error path: {captured}"
    );
    assert!(
        captured.contains("method=event.append"),
        "span method missing on error path: {captured}"
    );
}
