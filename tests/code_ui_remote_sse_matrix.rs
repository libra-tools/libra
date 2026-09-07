//! Data-driven SSE matrix runner (Wave 1 closing case).
//!
//! Each `#[test]` here loads `tests/data/code_ui_remote/sse_cases.json`
//! and runs the named case end-to-end against a fresh non-TTY `libra code`
//! Web process, subscribing to `/api/code/events` through the
//! `tests/harness/event_stream.rs` blocking client.
//!
//! Wave 1's exit criteria from `docs/development/commands/_general.md` is to prove
//! one SSE case end-to-end so the harness, matrix step variants, and
//! event-stream client are all wired correctly. Subsequent Waves
//! (PR 4) flesh out the remaining six cases without changing this
//! runner — adding a new case becomes one extra `sse_case!` line.

#[cfg(feature = "test-provider")]
mod harness;

#[cfg(feature = "test-provider")]
use anyhow::Result;
#[cfg(feature = "test-provider")]
use harness::CodeSession;
#[cfg(feature = "test-provider")]
use harness::matrix::{
    Case, CaseFile, DEFAULT_SSE_WIRE_VERSION, Step, build_session_options, find_case,
    load_case_file,
};
#[cfg(feature = "test-provider")]
use serial_test::serial;

#[cfg(feature = "test-provider")]
const CASE_FILE_PATH: &str = "tests/data/code_ui_remote/sse_cases.json";

#[cfg(feature = "test-provider")]
fn run_sse_case(case_name: &str) -> Result<()> {
    let file_path = harness::matrix::data_path(CASE_FILE_PATH);
    let file: CaseFile = load_case_file(&file_path)?;
    let case: Case = find_case(&file, case_name)?;
    let options = build_session_options(&file, &case);
    let mut session = CodeSession::spawn(options)?;
    let outcome = harness::matrix::run_case(&mut session, &case);
    let shutdown = session.shutdown();
    outcome?;
    shutdown
}

#[cfg(feature = "test-provider")]
#[test]
fn sse_matrix_consumes_only_wire_v2() -> Result<()> {
    // DF-08: wire v1 was removed in 0.22.0 — the fixture must not carry a
    // single v1 case any more, and every openEvents step names v2.
    let file_path = harness::matrix::data_path(CASE_FILE_PATH);
    let file: CaseFile = load_case_file(&file_path)?;
    assert_eq!(DEFAULT_SSE_WIRE_VERSION, 2);

    let v2_case = find_case(
        &file,
        "sse_emits_status_changed_when_submit_starts_thinking",
    )?;
    assert!(
        v2_case
            .steps
            .iter()
            .any(|step| matches!(step, Step::OpenEvents { wire: 2, .. }))
    );
    for case in &file.cases {
        let name = case["name"].as_str().unwrap_or("<unnamed>");
        for step in case["steps"].as_array().into_iter().flatten() {
            if step["op"] == "openEvents"
                && let Some(wire) = step.get("wire")
            {
                // An omitted wire defaults to v2 in the harness; an
                // explicit value may only name v2.
                assert_eq!(
                    wire, 2,
                    "case '{name}' must not consume the removed wire v1"
                );
            }
        }
    }
    Ok(())
}

#[cfg(feature = "test-provider")]
macro_rules! sse_case {
    ($name:ident) => {
        #[test]
        #[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
        fn $name() -> Result<()> {
            run_sse_case(stringify!($name))
        }
    };
}

// Wave 1 / Wave 4 — full SSE matrix coverage. Wave 1 wired the
// initial-replay case as a proof-of-life and added the
// `Step::OpenEvents` / `Step::ExpectEvent` variants + the
// `event_data_*` assertion vocabulary. Wave 4 (this commit)
// landed the remaining variants
// (`Step::CollectEventsUntil`, `Step::CollectSessionUpdates`,
// `Step::SubmitAndWaitIdle`) plus the multi-event
// `assistant_content_monotonic` assertion, which lets every case
// in `sse_cases.json` run end-to-end.
// Wave 4 — remaining six P0/P1 cases.
#[cfg(feature = "test-provider")]
sse_case!(sse_emits_status_changed_when_submit_starts_thinking);
#[cfg(feature = "test-provider")]
sse_case!(sse_emits_code_workflow_after_assistant_completion);
#[cfg(feature = "test-provider")]
sse_case!(sse_two_concurrent_subscribers_receive_code_workflow);
#[cfg(feature = "test-provider")]
sse_case!(sse_reconnect_initial_replay_contains_latest_transcript);
#[cfg(feature = "test-provider")]
sse_case!(sse_streaming_fixture_transcript_content_grows_monotonically);

/// W3-08: slow consumer past the transport broadcast budget is disconnected
/// with `event: resync`; tip-cursor reconnect continues without dup/loss.
#[cfg(feature = "test-provider")]
#[tokio::test(flavor = "multi_thread")]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn sse_slow_consumer() -> Result<()> {
    libra::internal::ai::web::assert_sse_slow_consumer_contract().await
}

#[cfg(not(feature = "test-provider"))]
#[test]
fn sse_matrix_requires_test_provider_feature() {
    eprintln!("skipping SSE matrix; enable --features test-provider");
}
