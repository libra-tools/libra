//! Wave 6 / PR 6 — `/api/code/interactions/{id}` end-to-end matrix.
//!
//! Each `#[test]` here loads
//! `tests/data/code_ui_remote/approval_cases.json` and drives the
//! shell-tool approval lifecycle through a real non-TTY `libra code`
//! Web process, asserting that:
//!
//! 1. a fixture-driven `shell` tool call enters the
//!    `awaiting_interaction` state with a pending interaction
//!    matching the runtime's `call_id`,
//! 2. POSTing `/api/code/interactions/{id}` with `approved: true`
//!    drains the turn back to idle, the tool runs, and the
//!    fixture's follow-up assistant text lands in the transcript,
//! 3. POSTing the same route with `approved: false` surfaces the
//!    rejection as a `failed` `tool_call` entry without leaving the
//!    runtime stuck in awaiting_interaction.
//!
//! The matrix re-uses `CodeSession::matrix_respond_interaction()`
//! and the new `Step::WaitInteractionPending` /
//! `Step::RespondInteraction` variants so adding a new approval
//! case is one extra `approval_case!` line plus the matching JSON
//! entry; the runner stays untouched.

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
const CASE_FILE_PATH: &str = "tests/data/code_ui_remote/approval_cases.json";

#[cfg(feature = "test-provider")]
fn run_approval_case(case_name: &str) -> Result<()> {
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
macro_rules! approval_case {
    ($name:ident) => {
        #[test]
        #[serial]
        fn $name() -> Result<()> {
            run_approval_case(stringify!($name))
        }
    };
}

// Wave 6 — P0 accept / reject / apply_to_future caching. Concurrent
// pending interactions are tracked as P1 in
// `docs/development/commands/_general.md` §5.11 because the fake provider only
// emits one tool call per turn, so a true two-pending case would
// need a parallel-tool-call extension that is out of scope here.
#[cfg(feature = "test-provider")]
approval_case!(approval_accept_path_runs_shell_and_completes_assistant);
#[cfg(feature = "test-provider")]
approval_case!(approval_reject_path_propagates_rejection_to_transcript);
#[cfg(feature = "test-provider")]
approval_case!(approval_apply_to_future_caches_decision_for_subsequent_calls);
#[cfg(feature = "test-provider")]
approval_case!(approval_policy_never_blocks_shell_without_prompt);
#[cfg(feature = "test-provider")]
approval_case!(approval_policy_on_failure_prompts_for_needs_human_shell);
#[cfg(feature = "test-provider")]
approval_case!(approval_policy_untrusted_prompts_for_needs_human_shell);
#[cfg(feature = "test-provider")]
approval_case!(approval_policy_allow_all_runs_shell_without_prompt);

#[cfg(not(feature = "test-provider"))]
#[test]
fn approval_matrix_requires_test_provider_feature() {
    eprintln!("skipping approval matrix; enable --features test-provider");
}
