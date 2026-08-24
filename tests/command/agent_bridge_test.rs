//! Integration tests for the `libra agent bridge --stdio` CLI surface
//! (plan-20260818 LB-01).
//!
//! **Layer:** L1 — deterministic, no external dependencies. Verifies clap
//! registration, the `agent --help` EXAMPLES banner, and the fail-closed
//! requirement that the bridge requires `--stdio`.

use super::*;

/// `libra agent bridge` is registered as an agent subcommand and its help is
/// discoverable.
#[test]
fn agent_bridge_help_and_dispatch() {
    let repo = tempdir().expect("tempdir for agent bridge --help");
    let output = run_libra_command(&["agent", "bridge", "--help"], repo.path());
    assert!(
        output.status.success(),
        "agent bridge --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--stdio"),
        "agent bridge --help should document --stdio, stdout: {stdout}"
    );
}

/// `libra agent --help` surfaces the bridge example in the EXAMPLES banner.
#[test]
fn agent_help_banner_advertises_bridge() {
    let repo = tempdir().expect("tempdir for agent --help");
    let output = run_libra_command(&["agent", "--help"], repo.path());
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("libra agent bridge --stdio"),
        "agent --help should advertise the bridge example, stdout: {stdout}"
    );
}

/// The bridge refuses to run without `--stdio` (fail-closed; no other
/// transport exists).
#[test]
fn agent_bridge_requires_stdio() {
    let repo = tempdir().expect("tempdir for agent bridge");
    // `libra agent bridge` with no sub-args is a clap usage failure because
    // `--stdio` is required; we only assert the subcommand is reachable and
    // that a missing `--stdio` does not silently start a transport.
    let output = run_libra_command(&["agent", "bridge"], repo.path());
    assert!(
        !output.status.success(),
        "agent bridge without --stdio must fail closed"
    );
}
