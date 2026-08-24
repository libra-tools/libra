//! Wave 7 / PR 7 — `code_ui_remote_security` matrix runner.
//!
//! Loads `tests/data/code_ui_remote/security_cases.json` and runs
//! the L2-driven P1 cases through a real non-TTY `libra code` Web process:
//!
//! 1. diagnostics body never echoes either the harness's
//!    `X-Libra-Control-Token` value or the issued
//!    `controllerToken`,
//! 2. diagnostics redacts secret-like substrings from
//!    `LIBRA_LOG_FILE` (driven via per-case `extraEnv`),
//! 3. `--control observe` rejects an automation attach with
//!    403 / `CONTROL_DISABLED`,
//! 4. `/threads?limit=abc` returns 400 / `INVALID_QUERY_PARAM`,
//! 5. `/threads?limit=99999` clamps to ≤200 items,
//! 6. control audit log redacts secret-like client ids on attach.
//!
//! The two `testKind: inline` cases in the JSON file
//! (`security_non_loopback_session_route_is_inline_unit` and
//! `security_non_loopback_messages_route_rejects_before_controller_token`)
//! are intentionally NOT mapped here — they are inline `#[test]`s
//! in `src/internal/ai/web/mod.rs` (added by PR 2 / Wave 2) and
//! the JSON entry is only a marker that those scenarios exist
//! under inline coverage.

#[cfg(feature = "test-provider")]
mod harness;

#[cfg(feature = "test-provider")]
use anyhow::Result;
#[cfg(feature = "test-provider")]
use harness::CodeSession;
#[cfg(feature = "test-provider")]
use harness::matrix::{Case, CaseFile, build_session_options, find_case, load_case_file};
#[cfg(feature = "test-provider")]
use serial_test::serial;

#[cfg(feature = "test-provider")]
const CASE_FILE_PATH: &str = "tests/data/code_ui_remote/security_cases.json";

#[cfg(feature = "test-provider")]
fn run_security_case(case_name: &str) -> Result<()> {
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
macro_rules! security_case {
    ($name:ident) => {
        #[test]
        #[serial]
        fn $name() -> Result<()> {
            run_security_case(stringify!($name))
        }
    };
}

// Wave 7 — six L2 P1 cases. The two inline-only entries in the
// JSON file are covered by the existing route-level inline tests
// landed in Wave 2 (`src/internal/ai/web/mod.rs mod tests`).
#[cfg(feature = "test-provider")]
security_case!(security_diagnostics_does_not_expose_control_or_controller_token);
#[cfg(feature = "test-provider")]
security_case!(security_diagnostics_redacts_secret_like_log_file_path);
#[cfg(feature = "test-provider")]
security_case!(security_attach_with_control_observe_is_403_control_disabled);
#[cfg(feature = "test-provider")]
security_case!(security_threads_invalid_limit_returns_invalid_query_param);
#[cfg(feature = "test-provider")]
security_case!(security_threads_limit_clamped_to_200_max);
#[cfg(feature = "test-provider")]
security_case!(security_audit_log_records_attach_with_redacted_client_id);

/// W3-05 Origin gate: missing Origin on browser attach fails closed.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn security_browser_write_missing_origin_is_403_origin_required() -> Result<()> {
    let options = harness::CodeSessionOptions::new(
        "security-origin-missing",
        harness::matrix::data_path("tests/fixtures/code_ui/basic_chat.json"),
    )
    .with_browser_control_loopback();
    let mut session = CodeSession::spawn(options)?;
    let (status, body) = session.attach_browser_without_origin("security-origin-missing")?;
    let shutdown = session.shutdown();
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        body.pointer("/error/code").and_then(|v| v.as_str()),
        Some("ORIGIN_REQUIRED"),
        "missing Origin must fail closed; got {body}"
    );
    shutdown
}

/// W3-05 Origin gate: cross-site Origin is rejected.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn security_browser_write_cross_site_origin_is_403_origin_required() -> Result<()> {
    let options = harness::CodeSessionOptions::new(
        "security-origin-cross",
        harness::matrix::data_path("tests/fixtures/code_ui/basic_chat.json"),
    )
    .with_browser_control_loopback();
    let mut session = CodeSession::spawn(options)?;
    let (status, body) =
        session.attach_browser_with_origin("security-origin-cross", "https://evil.example")?;
    let shutdown = session.shutdown();
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        body.pointer("/error/code").and_then(|v| v.as_str()),
        Some("ORIGIN_REQUIRED"),
        "cross-site Origin must fail closed; got {body}"
    );
    shutdown
}

/// W3-05 Origin gate: trusted loopback Origin is accepted for browser attach.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn security_browser_write_trusted_loopback_origin_succeeds() -> Result<()> {
    let options = harness::CodeSessionOptions::new(
        "security-origin-ok",
        harness::matrix::data_path("tests/fixtures/code_ui/basic_chat.json"),
    )
    .with_browser_control_loopback();
    let mut session = CodeSession::spawn(options)?;
    let token = session.attach_browser("security-origin-ok")?;
    let shutdown = session.shutdown();
    assert!(!token.is_empty());
    shutdown
}

/// W3-05 Origin gate: post-attach `/messages` without Origin fails closed.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn security_browser_message_missing_origin_is_403_origin_required() -> Result<()> {
    let options = harness::CodeSessionOptions::new(
        "security-origin-msg-missing",
        harness::matrix::data_path("tests/fixtures/code_ui/basic_chat.json"),
    )
    .with_browser_control_loopback();
    let mut session = CodeSession::spawn(options)?;
    let token = session.attach_browser("security-origin-msg-missing")?;
    let (status, body) =
        session.browser_submit_message_with_origin(&token, "/chat csrf-missing", None)?;
    let shutdown = session.shutdown();
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        body.pointer("/error/code").and_then(|v| v.as_str()),
        Some("ORIGIN_REQUIRED"),
        "browser /messages without Origin must fail closed; got {body}"
    );
    shutdown
}

/// W3-05 Origin gate: post-attach `/messages` with cross-site Origin fails closed.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn security_browser_message_cross_site_origin_is_403_origin_required() -> Result<()> {
    let options = harness::CodeSessionOptions::new(
        "security-origin-msg-cross",
        harness::matrix::data_path("tests/fixtures/code_ui/basic_chat.json"),
    )
    .with_browser_control_loopback();
    let mut session = CodeSession::spawn(options)?;
    let token = session.attach_browser("security-origin-msg-cross")?;
    let (status, body) = session.browser_submit_message_with_origin(
        &token,
        "/chat csrf-cross",
        Some("https://evil.example"),
    )?;
    let shutdown = session.shutdown();
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        body.pointer("/error/code").and_then(|v| v.as_str()),
        Some("ORIGIN_REQUIRED"),
        "browser /messages with cross-site Origin must fail closed; got {body}"
    );
    shutdown
}

/// W3-05: omitted attach `kind` with control token stays automation (code-control shim).
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn security_automation_attach_omitted_kind_without_origin_succeeds() -> Result<()> {
    let options = harness::CodeSessionOptions::new(
        "security-attach-omit-kind",
        harness::matrix::data_path("tests/fixtures/code_ui/basic_chat.json"),
    );
    let mut session = CodeSession::spawn(options)?;
    let (status, body) = session.attach_automation_omitted_kind("security-attach-omit-kind")?;
    let shutdown = session.shutdown();
    assert!(
        status.is_success(),
        "omitted-kind automation attach must succeed without Origin; status={status} body={body}"
    );
    assert!(
        body.get("controllerToken")
            .and_then(|v| v.as_str())
            .is_some_and(|token| !token.is_empty()),
        "expected controllerToken; got {body}"
    );
    shutdown
}

/// W3-05 per-session rate limit trips and recovers after the window.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn security_session_rate_limit_triggers_and_recovers() -> Result<()> {
    use std::time::Duration;

    let mut options = harness::CodeSessionOptions::new(
        "security-rate-limit",
        harness::matrix::data_path("tests/fixtures/code_ui/basic_chat.json"),
    );
    options.extra_env.push((
        "LIBRA_CODE_SESSION_WRITE_RATE_LIMIT".to_string(),
        "2".to_string(),
    ));
    options.extra_env.push((
        "LIBRA_CODE_SESSION_WRITE_RATE_WINDOW_SECS".to_string(),
        "2".to_string(),
    ));
    let mut session = CodeSession::spawn(options)?;
    let _token = session.attach_automation("security-rate-limit")?;
    // attach consumed budget slot 1; first submit is slot 2 (still within max=2).
    let status1 = session.submit_message("/chat rate-1")?;
    let (status2, headers2, body2) =
        session.submit_message_expect_error_with_headers("/chat rate-2")?;
    std::thread::sleep(Duration::from_millis(2100));
    let status3 = session.submit_message("/chat rate-3")?;
    let shutdown = session.shutdown();
    assert!(
        status1.is_success(),
        "first submit within budget should succeed, got {status1}"
    );
    assert_eq!(status2, reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        body2.pointer("/error/code").and_then(|v| v.as_str()),
        Some("RATE_LIMITED"),
        "over-budget write must return RATE_LIMITED; got {body2}"
    );
    let retry_after = headers2
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.parse::<u64>().ok());
    assert!(
        retry_after.is_some_and(|secs| secs >= 1),
        "RATE_LIMITED must advertise Retry-After >= 1s; headers={headers2:?}"
    );
    assert!(
        status3.is_success(),
        "write after window must recover, got {status3}"
    );
    shutdown
}

/// W3-05 body limit on browser write path.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn security_browser_write_body_size_too_large_is_413_payload_too_large() -> Result<()> {
    let options = harness::CodeSessionOptions::new(
        "security-body-browser",
        harness::matrix::data_path("tests/fixtures/code_ui/basic_chat.json"),
    )
    .with_browser_control_loopback();
    let mut session = CodeSession::spawn(options)?;
    let token = session.attach_browser("security-body-browser")?;
    let (status, body) = session.browser_submit_large_message(&token, 262_145)?;
    let shutdown = session.shutdown();
    assert_eq!(status, reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        body.pointer("/error/code").and_then(|v| v.as_str()),
        Some("PAYLOAD_TOO_LARGE"),
        "oversized browser body must fail closed; got {body}"
    );
    shutdown
}

/// W3-05 body limit on automation write path.
#[cfg(feature = "test-provider")]
#[test]
#[serial]
fn security_automation_write_body_size_too_large_is_413_payload_too_large() -> Result<()> {
    let options = harness::CodeSessionOptions::new(
        "security-body-automation",
        harness::matrix::data_path("tests/fixtures/code_ui/basic_chat.json"),
    );
    let mut session = CodeSession::spawn(options)?;
    let _token = session.attach_automation("security-body-automation")?;
    let (status, body) = session.submit_large_message(262_145)?;
    let shutdown = session.shutdown();
    assert_eq!(status, reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        body.pointer("/error/code").and_then(|v| v.as_str()),
        Some("PAYLOAD_TOO_LARGE"),
        "oversized automation body must fail closed; got {body}"
    );
    shutdown
}

/// W3-11 host posture: non-loopback peers only get the static remote notice;
/// snapshot/SSE/approval/write API surfaces stay LOOPBACK_REQUIRED.
#[cfg(feature = "test-provider")]
#[tokio::test]
async fn security_host_posture_non_loopback_surfaces_fail_closed() -> anyhow::Result<()> {
    libra::internal::ai::web::assert_host_posture_non_loopback_contract().await
}

#[cfg(not(feature = "test-provider"))]
#[test]
fn security_matrix_requires_test_provider_feature() {
    eprintln!("skipping security matrix; enable --features test-provider");
}
