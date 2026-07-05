//! Fake-sink span assertions for the AG-22 review-run span
//! (`agent.review.run`, plan.md Task A7 / `agent.md` §6 row :1334).
//!
//! Lives in its own integration-test binary for the same reason as
//! `tests/agent_checkpoint_span_test.rs`: the assertions install a
//! thread-local `tracing` subscriber, and tracing's per-callsite interest
//! cache can flap when sibling threads in the same process evaluate the
//! same callsites without a subscriber — a single-test binary removes
//! that concurrency by construction.
//!
//! Contract under test (§6 table): the span carries `run_id`,
//! `agent_count`, `terminal_state`, `duration_ms`; reviewer raw stdout is
//! a FORBIDDEN field and must NEVER reach the sink.

#![cfg(unix)]

use std::{path::PathBuf, sync::Mutex, time::Duration};

use libra::internal::ai::review::{
    ReviewCancelHandle, ReviewRunRequest, ReviewRunStore, ReviewTerminalState, ReviewerCommand,
    ReviewerSource, run_review,
};

/// Shared in-memory sink handed to the fmt subscriber (identical to the
/// pattern in `tests/agent_checkpoint_span_test.rs`).
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

/// Single-threaded runtime so the run loop polls on this thread, where
/// the thread-local subscriber is installed. (`spawn_blocking` work —
/// workspace materialization — runs on blocking threads whose events
/// fall to the no-op global default; the `agent.review.run` span itself
/// opens, records, and closes on this thread.)
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread tokio runtime")
}

/// A completed review run emits one `agent.review.run` span carrying the
/// §6 required fields (`run_id`, `agent_count`, `terminal_state`,
/// `duration_ms`); the reviewer's stdout text — which demonstrably
/// flowed through the run into `findings.md` — never reaches the sink.
#[test]
fn review_run_span_carries_required_fields_without_reviewer_stdout() {
    let rt = runtime();
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    std::fs::write(repo.join("README.md"), "span test repo\n").expect("seed file");
    let store = ReviewRunStore::new(dir.path().join(".libra").join("sessions"));

    // Unique marker: it reaching the sink would mean the span (or an
    // event in the run loop) leaked reviewer stdout.
    let marker = "REVIEWER-STDOUT-MARKER-55zz";
    let reviewer = ReviewerSource::Custom(ReviewerCommand {
        slug: "fake-span".to_string(),
        program: PathBuf::from("/bin/sh"),
        args: vec!["-c".to_string(), format!("printf '%s\\n' '{marker}'")],
        env: Vec::new(),
        timeout: Duration::from_secs(30),
    });
    let request = ReviewRunRequest::new(
        &repo,
        "review the changes",
        "HEAD~1..HEAD",
        "sha-span",
        vec![reviewer],
    );

    let mut outcome_slot = None;
    let captured = capture_spans(|| {
        let outcome = rt
            .block_on(run_review(&store, request, ReviewCancelHandle::new()))
            .expect("review run completes");
        outcome_slot = Some(outcome);
    });
    let outcome = outcome_slot.expect("run produced an outcome");
    assert_eq!(outcome.terminal_state, ReviewTerminalState::Success);

    // The marker really flowed through the run (so its absence from the
    // span sink below is meaningful, not vacuous).
    let findings = store
        .read_findings(&outcome.run_id)
        .expect("read findings")
        .expect("findings exist");
    assert!(
        findings.contains(marker),
        "reviewer stdout must reach findings.md: {findings}"
    );

    assert!(
        captured.contains("agent.review.run"),
        "review run span missing: {captured}"
    );
    for field in [
        format!("run_id=\"{}\"", outcome.run_id),
        "agent_count=1".to_string(),
        "terminal_state=\"success\"".to_string(),
        "duration_ms=".to_string(),
    ] {
        assert!(
            captured.contains(&field),
            "review run span missing `{field}`: {captured}"
        );
    }

    // Forbidden content: reviewer raw stdout.
    assert!(
        !captured.contains(marker),
        "reviewer stdout must never reach the span sink: {captured}"
    );
}
