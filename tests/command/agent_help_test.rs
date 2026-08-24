//! Integration tests for the `libra agent --help` surface.
//!
//! **Layer:** L1 — deterministic, no external dependencies. Covers
//! the cross-cutting `--help` EXAMPLES rollout from
//! `docs/development/commands/_general.md` item B for the external Agent capture
//! pipeline operator surface.

use super::*;

/// `libra agent --help` surfaces the EXAMPLES banner so operators see
/// the canonical invocation per visible sub-command (status, list, import, graph,
/// enable, disable, session, checkpoint, clean, doctor, push, rpc) plus the
/// `--all` clean form, the `--remote` push form, and the JSON variant
/// without reading the design doc.
#[test]
fn test_agent_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for agent --help");
    let output = run_libra_command(&["agent", "--help"], repo.path());
    assert!(
        output.status.success(),
        "agent --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "agent --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra agent status",
        "libra agent list --schema-version 2 --json",
        "libra agent import --session <id>",
        "libra --json agent graph <session>",
        "libra --machine agent graph <session>",
        "libra agent enable --agent claude",
        "libra agent disable --agent claude",
        "libra agent session list",
        "libra agent checkpoint list",
        "libra agent checkpoint show <id>",
        "libra agent checkpoint rewind <id>",
        "libra agent clean",
        "libra agent clean --all",
        "libra agent doctor",
        "libra agent push",
        "libra agent push --remote origin",
        "libra agent rpc list",
        "libra agent rpc invoke",
        // `--json` is a global flag, so it precedes the subcommand; the
        // banner was corrected to this form in f742fa25.
        "libra --json agent status",
    ] {
        assert!(
            stdout.contains(invocation),
            "agent --help should include `{invocation}`, stdout: {stdout}"
        );
    }
    // W5-08: the bare interactive `libra agent graph <session>` entry was
    // removed; the EXAMPLES banner must not advertise it.
    assert!(
        !stdout.contains("libra agent graph <session>"),
        "agent --help must not advertise the removed interactive graph entry, stdout: {stdout}"
    );
}

#[test]
fn test_agent_graph_help_pins_session_repo_and_machine_surface() {
    let repo = tempdir().expect("tempdir for agent graph --help");
    let output = run_libra_command(&["agent", "graph", "--help"], repo.path());
    assert!(
        output.status.success(),
        "agent graph --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for text in [
        "<SESSION>",
        "--repo <PATH>",
        "Captured session id from `libra agent session list`",
        "libra --json agent graph <session>",
        "libra --machine agent graph <session>",
    ] {
        assert!(
            stdout.contains(text),
            "agent graph --help should include `{text}`, stdout: {stdout}"
        );
    }
}

#[test]
fn test_agent_import_help_pins_consent_and_selector_surface() {
    let repo = tempdir().expect("tempdir for agent import --help");
    let output = run_libra_command(&["agent", "import", "--help"], repo.path());
    assert!(
        output.status.success(),
        "agent import --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for text in [
        "--session <ID>",
        "--path <PATH>",
        "--since <RFC3339>",
        "--all",
        "--agent <NAME>",
        "--limit <N>",
        "--cursor <CURSOR>",
        "--yes",
        "--restore-erased",
        "Confirm reading/redacting provider session data",
    ] {
        assert!(
            stdout.contains(text),
            "agent import --help should include `{text}`, stdout: {stdout}"
        );
    }
}

#[test]
fn test_agent_checkpoint_rewind_help_mentions_supported_transcript_truncation() {
    let repo = tempdir().expect("tempdir for agent checkpoint rewind --help");
    let output = run_libra_command(&["agent", "checkpoint", "rewind", "--help"], repo.path());
    assert!(
        output.status.success(),
        "agent checkpoint rewind --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("supported agent transcripts"),
        "rewind help should describe the current truncation behavior, stdout: {stdout}"
    );
    assert!(
        !stdout.contains("NOT rewritten") && !stdout.contains("restores worktree only"),
        "rewind help must not claim transcripts are never rewritten, stdout: {stdout}"
    );
}
