//! Data-driven controller-lease matrix.
//!
//! Each `#[test]` here loads `tests/data/code_ui_remote/lease_cases.json`
//! and runs the named case end-to-end against a fresh non-TTY `libra code`
//! Web process. Adding a new lease scenario is a data-only change in the JSON
//! file plus one one-line entry in this test module.

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
const CASE_FILE_PATH: &str = "tests/data/code_ui_remote/lease_cases.json";

#[cfg(feature = "test-provider")]
fn run_lease_case(case_name: &str) -> Result<()> {
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
macro_rules! lease_case {
    ($name:ident) => {
        #[test]
        #[serial]
        fn $name() -> Result<()> {
            run_lease_case(stringify!($name))
        }
    };
}

#[cfg(feature = "test-provider")]
lease_case!(lease_attach_automation_succeeds_with_control_token);

#[cfg(feature = "test-provider")]
lease_case!(lease_attach_automation_without_control_token_is_403_missing);

#[cfg(feature = "test-provider")]
lease_case!(lease_attach_automation_with_wrong_control_token_is_403_invalid);

#[cfg(feature = "test-provider")]
lease_case!(lease_attach_invalid_kind_returns_400);

// Wave 3 / PR 3 — bring the matrix to 9/9 case coverage. The
// JSON in `tests/data/code_ui_remote/lease_cases.json` has had
// these case bodies since Phase 0; Wave 1's lazy-case loader
// keeps the runner from rejecting them on parse, and the
// matrix Step variants used here (`Attach`, `Detach`, `Submit`,
// `Sleep`, `WaitSnapshot`) are all already implemented.

#[cfg(feature = "test-provider")]
lease_case!(lease_attach_renew_with_same_client_id_extends_expiry);

#[cfg(feature = "test-provider")]
lease_case!(lease_attach_conflict_when_other_client_holds);

#[cfg(feature = "test-provider")]
lease_case!(lease_detach_releases_to_none);

#[cfg(feature = "test-provider")]
lease_case!(lease_detach_with_wrong_controller_token_is_rejected);

#[cfg(feature = "test-provider")]
lease_case!(lease_expiry_releases_and_rejects_stale_token);

// Wave 3 / PR 3 §5.4 — `--control observe` automation attach must
// resolve to 403 / CONTROL_DISABLED. Brings the matrix to 10/10
// (the original 9 plus this observe-mode case).
#[cfg(feature = "test-provider")]
lease_case!(lease_attach_observe_mode_rejects_automation_with_control_disabled);

#[cfg(not(feature = "test-provider"))]
#[test]
fn lease_matrix_requires_test_provider_feature() {
    eprintln!("skipping lease matrix; enable --features test-provider");
}
