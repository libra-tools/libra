//! Static guard for Phase 2 Task 2.7 of `docs/development/tracing/agent.md` Part B
//! (merged from the original TUI improvement plan per the 2026-05-02
//! agent.md consolidation), updated for plan-20260715 **W4-01** / **W5-06** /
//! **W5-07**.
//!
//! `libra code --provider codex` must route through the managed Codex
//! app-server / Code UI runtime — the legacy standalone Codex stdin loop
//! (`agent_codex::execute`) is deprecated and must not be reachable from the
//! `libra code` command path. After W4-01 the **default** entry is Web Code UI
//! (`execute_web_only`); W5-07 removed the hidden `LIBRA_CODE_LEGACY_TUI=1`
//! rollback env and the deprecated `--web`/`--web-only` aliases, and W5-06
//! removed the TUI startup path together with the bare
//! `--provider codex --resume` legacy TUI resume driver (it now fails closed
//! with a usage error). Spinning up a real Codex app-server inside CI is
//! prohibitively heavy, so we rely on source-level invariants instead:
//!
//! 1. `src/command/code.rs` must not call `agent_codex::execute`.
//! 2. Default `execute()` must prefer `execute_web_only` via
//!    `code_uses_web_launch`, and the removed rollback env must not reappear
//!    (mirrors the W5-07 guard).
//! 3. The W5-06-removed TUI startup path (`execute_tui`,
//!    `codex_resume_uses_legacy_tui`, `run_tui_with_managed_code_runtime`)
//!    must not reappear; bare `--provider codex --resume` must fail closed
//!    with a migration-hint usage error, and `CodeProvider::Codex` hands off
//!    to `start_codex_code_ui_runtime` (Web default).
//! 4. `agent_codex::execute` must keep the `#[deprecated]` marker.
//! 5. Legacy stdin/stdout primitives must not appear inside
//!    `src/command/code.rs` (the TUI module was removed in W5-03).
//!
//! These checks complement the runtime scenarios in
//! `tests/code_ui_scenarios.rs`; they fail fast and don't need the
//! `test-provider` feature.

use std::{fs, path::PathBuf};

const COMMAND_CODE_PATH: &str = "src/command/code.rs";
const CODEX_MOD_PATH: &str = "src/internal/ai/codex/mod.rs";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// Panic if any line in `haystack` matches the predicate. Useful for guard
/// invariants where we want a precise diagnostic instead of a boolean.
fn assert_no_line_matches<P>(haystack: &str, label: &str, predicate: P)
where
    P: Fn(&str) -> bool,
{
    let offenders: Vec<(usize, &str)> = haystack
        .lines()
        .enumerate()
        .filter(|(_, line)| predicate(line))
        .map(|(idx, line)| (idx + 1, line.trim()))
        .collect();
    assert!(
        offenders.is_empty(),
        "{label} regression: {} offending line(s):\n{}",
        offenders.len(),
        offenders
            .iter()
            .map(|(line_no, line)| format!("  L{line_no}: {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn command_code_does_not_call_legacy_codex_execute() {
    let source = read_file(COMMAND_CODE_PATH);
    // Allow the substring inside comments/docs but not as a function call.
    assert_no_line_matches(&source, "agent_codex::execute call site", |line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("*") {
            return false;
        }
        line.contains("agent_codex::execute(")
    });
}

#[test]
fn default_execute_routes_to_web_code_ui() {
    let source = read_file(COMMAND_CODE_PATH);
    // W5-07: the hidden rollback env is removed from the CLI surface; this
    // pin mirrors the plan guard so it cannot silently return.
    assert!(
        !source.contains("LIBRA_CODE_LEGACY_TUI"),
        "W5-07 removed the LIBRA_CODE_LEGACY_TUI rollback env from {COMMAND_CODE_PATH}"
    );
    assert!(
        source.contains("fn code_uses_web_launch"),
        "expected code_uses_web_launch helper for default Web routing"
    );
    assert!(
        source.contains("execute_web_only(&args).await"),
        "default execute path must call execute_web_only"
    );
    // W5-06: the TUI startup path and the bare `--provider codex --resume`
    // legacy resume driver are deleted; these symbols must not reappear.
    assert!(
        !source.contains("execute_tui"),
        "W5-06 removed the execute_tui startup path from {COMMAND_CODE_PATH}"
    );
    assert!(
        !source.contains("codex_resume_uses_legacy_tui"),
        "W5-06 removed the codex_resume_uses_legacy_tui dispatch from {COMMAND_CODE_PATH}"
    );
    assert!(
        source.contains("--resume is not supported with --provider=codex"),
        "bare --provider codex --resume must fail closed with a migration-hint usage error"
    );
}

#[test]
fn codex_arm_routes_through_managed_runtime() {
    let source = read_file(COMMAND_CODE_PATH);
    assert!(
        source.contains("CodeProvider::Codex =>"),
        "expected `CodeProvider::Codex` match arm in {COMMAND_CODE_PATH}"
    );
    // W5-06: the shared managed-TUI driver went away with the legacy resume
    // driver; it must not come back.
    assert!(
        !source.contains("run_tui_with_managed_code_runtime"),
        "W5-06 removed `run_tui_with_managed_code_runtime` from {COMMAND_CODE_PATH}"
    );
    // Default Web and Codex web construct the runtime via the documented helper.
    assert!(
        source.contains("start_codex_code_ui_runtime"),
        "Codex arm must construct the runtime via `start_codex_code_ui_runtime`"
    );
}

#[test]
fn legacy_codex_execute_is_deprecated() {
    let source = read_file(CODEX_MOD_PATH);
    let exec_idx = source
        .find("pub async fn execute(")
        .expect("agent_codex::execute should still exist (legacy)");
    let preamble = &source[..exec_idx];
    let last_attr_window = preamble.rfind("#[deprecated").unwrap_or(usize::MAX);
    let last_blank_line = preamble.rfind("\n\n").unwrap_or(0);
    assert!(
        last_attr_window > last_blank_line,
        "agent_codex::execute must keep the `#[deprecated(...)]` attribute attached \
         (so any new caller fails compilation with -D warnings)"
    );
}

#[test]
fn libra_code_path_has_no_stdin_or_codex_print_loops() {
    // Inside src/command/code.rs the orchestrator should never read stdin or
    // drive a Codex-style approval print loop.
    let cmd_source = read_file(COMMAND_CODE_PATH);
    assert_no_line_matches(&cmd_source, "stdin reader in command/code.rs", |line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("///") {
            return false;
        }
        line.contains("std::io::stdin") || line.contains("io::stdin(")
    });
}

/// W5-02 (Codex r18): the managed Codex adapter shares the turn-admission
/// wire contract — busy submit and idle cancel are `SESSION_BUSY`, never a
/// Codex-private code. `INTERACTION_NOT_ACTIVE` stays scoped to pending
/// interaction responses.
#[test]
fn codex_adapter_uses_shared_session_busy_contract() {
    let source = read_file("src/internal/ai/codex/mod.rs");
    assert!(
        !source.contains("TURN_ALREADY_ACTIVE"),
        "managed Codex must not mint a private busy code; use SESSION_BUSY"
    );
    // Branch 1: busy submit — the SESSION_BUSY code adjacent to the
    // in-progress message.
    let busy_submit = source
        .find("a Codex turn is already in progress")
        .expect("busy-submit message present");
    let submit_window = &source[busy_submit.saturating_sub(200)..busy_submit];
    assert!(
        submit_window.contains("\"SESSION_BUSY\""),
        "busy submit must map to SESSION_BUSY"
    );
    // Branch 2: idle cancel — the SESSION_BUSY code adjacent to the
    // nothing-to-cancel message.
    let idle_cancel = source
        .find("no active Codex turn to cancel")
        .expect("idle-cancel message present");
    let cancel_window = &source[idle_cancel.saturating_sub(200)..idle_cancel];
    assert!(
        cancel_window.contains("\"SESSION_BUSY\""),
        "idle cancel must map to SESSION_BUSY, not INTERACTION_NOT_ACTIVE"
    );
    assert!(
        !cancel_window.contains("INTERACTION_NOT_ACTIVE"),
        "idle cancel must not be reported as INTERACTION_NOT_ACTIVE"
    );
}
