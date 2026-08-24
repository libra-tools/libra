//! Wave 5 / PR 5 — Code service code-generation matrix runner.
//!
//! Each `#[test]` here loads
//! `tests/data/code_ui_remote/generation_cases.json` and drives the
//! corresponding apply_patch fixture through a real non-TTY `libra code`
//! Web process, asserting that:
//!
//! 1. the fake provider's `apply_patch` tool call lands a complete
//!    file in the temporary working directory,
//! 2. the produced source compiles + its self-contained tests pass,
//! 3. the SSE event stream observes the tool execution + final
//!    completion concurrently with the generation request, and
//! 4. fault injection (invalid patch) surfaces a transcript-visible
//!    error without leaving a half-written file behind.
//!
//! The matrix re-uses the harness-level `CodeSession`, the data
//! driven `matrix.rs` runner, and the fake provider fixtures
//! shipped with the JSON case file — see
//! `tests/data/code_ui_remote/provider_fixtures/code_generation_*`.
//!
//! Adding a new generation case is one extra `generation_case!`
//! line plus the matching JSON entry; the runner stays untouched.

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
const CASE_FILE_PATH: &str = "tests/data/code_ui_remote/generation_cases.json";

#[cfg(feature = "test-provider")]
fn run_generation_case(case_name: &str) -> Result<()> {
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
macro_rules! generation_case {
    ($name:ident) => {
        #[test]
        #[serial]
        fn $name() -> Result<()> {
            run_generation_case(stringify!($name))
        }
    };
}

// Wave 5 — full P0/P1 generation matrix. Order mirrors
// `generation_cases.json` so a regression maps cleanly back to the
// JSON entry.
#[cfg(feature = "test-provider")]
generation_case!(generation_code_service_creates_complete_rust_file_and_tests_pass);
#[cfg(feature = "test-provider")]
generation_case!(generation_sse_observes_tool_execution_and_final_completion);
#[cfg(feature = "test-provider")]
generation_case!(generation_invalid_patch_surfaces_error_without_partial_file);

#[cfg(not(feature = "test-provider"))]
#[test]
fn generation_matrix_requires_test_provider_feature() {
    eprintln!("skipping generation matrix; enable --features test-provider");
}
