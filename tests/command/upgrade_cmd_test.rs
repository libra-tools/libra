//! `libra upgrade` CLI surface (manual signed-channel upgrade).
//!
//! The test binary is a dev build without the official install marker, so
//! every invocation deterministically resolves to the "not an official
//! install" (or, off the release matrix, "unsupported platform") outcome
//! BEFORE any network I/O — which is exactly the surface these tests pin:
//! friendly report-only `--check`, fail-closed install attempts, JSON shape,
//! and flag validation. The verified decide/install pipeline itself is
//! covered by `upgrade_auto_test` at the library level.

use serde_json::Value;
use tempfile::tempdir;

use super::{parse_cli_error_stderr, run_libra_command};

/// `--check` is report-only: it must succeed (exit 0) on a dev binary and
/// say WHY nothing can be reported for it.
#[test]
fn upgrade_check_on_a_dev_binary_reports_and_exits_zero() {
    let dir = tempdir().expect("tempdir");
    let output = run_libra_command(&["upgrade", "--check"], dir.path());
    assert!(
        output.status.success(),
        "--check must be informational: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("not an official signed install")
            || stdout.contains("outside the signed release matrix"),
        "stdout must explain the state: {stdout}"
    );
}

/// An install attempt from a dev binary fails closed with the stable
/// unsupported code and an actionable install-script hint.
#[test]
fn upgrade_install_on_a_dev_binary_fails_closed_with_hint() {
    let dir = tempdir().expect("tempdir");
    let output = run_libra_command(&["upgrade", "--yes"], dir.path());
    assert!(
        !output.status.success(),
        "installing from a dev binary must be refused"
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001", "stderr: {stderr}");
    if stderr.contains("not an official signed install") {
        assert!(
            stderr.contains("install.sh"),
            "the refusal must point at the install script: {stderr}"
        );
    }
}

/// The interactive form behaves identically when it cannot even check
/// (no prompt is reachable before the official-install gate).
#[test]
fn upgrade_interactive_on_a_dev_binary_fails_closed_too() {
    let dir = tempdir().expect("tempdir");
    let output = run_libra_command(&["upgrade"], dir.path());
    assert!(!output.status.success());
    let (_, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
}

/// `--json upgrade --check` emits the machine envelope with a closed status
/// vocabulary and exits 0.
#[test]
fn upgrade_check_json_reports_machine_status() {
    let dir = tempdir().expect("tempdir");
    let output = run_libra_command(&["--json", "upgrade", "--check"], dir.path());
    assert!(
        output.status.success(),
        "json --check must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("one JSON document");
    let status = value["data"]["status"].as_str().unwrap_or_default();
    assert!(
        matches!(status, "not_official_install" | "unsupported_platform"),
        "unexpected status {status}: {value}"
    );
}

/// `--check` and `--yes` are mutually exclusive at the clap layer.
#[test]
fn upgrade_check_conflicts_with_yes() {
    let dir = tempdir().expect("tempdir");
    let output = run_libra_command(&["upgrade", "--check", "--yes"], dir.path());
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--check") || stderr.contains("cannot be used"),
        "clap must report the conflict: {stderr}"
    );
}

/// The command needs no repository: running it outside any repo must not
/// produce a repository error (the tempdir has no .libra).
#[test]
fn upgrade_works_outside_a_repository() {
    let dir = tempdir().expect("tempdir");
    let output = run_libra_command(&["upgrade", "--check"], dir.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("not a libra repository"),
        "upgrade must not require a repository: {stderr}"
    );
}

/// The install scripts write the official-install marker with hand-rolled
/// JSON; this pins that byte shape to the Rust validator's schema so the
/// two can never drift apart silently.
#[test]
fn installer_marker_json_shape_matches_the_rust_schema() {
    let shell_shaped = r#"{"schema_version":1,"installed_at":"2026-09-02T00:00:00Z","install_source":"official_signed_manifest","platform":"linux-arm64","version":"0.22.10","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size":12345,"manifest_key_id":"libra-release-1"}"#;
    let marker: libra::internal::upgrade::marker::InstallMarker =
        serde_json::from_str(shell_shaped).expect("installer-written marker must parse");
    assert_eq!(marker.schema_version, 1);
    assert_eq!(
        marker.install_source,
        libra::internal::upgrade::marker::OFFICIAL_INSTALL_SOURCE
    );
    assert_eq!(marker.version, "0.22.10");
    assert_eq!(marker.size, 12345);
    // Round-trips losslessly (the txn writer uses serde on the same struct).
    let reserialized = serde_json::to_string(&marker).expect("serialize");
    let reparsed: libra::internal::upgrade::marker::InstallMarker =
        serde_json::from_str(&reserialized).expect("round-trip");
    assert_eq!(marker, reparsed);
}
