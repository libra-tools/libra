//! `libra ui` CLI surface.
//!
//! The window itself needs a display and runs on the process main thread, so
//! these cases cover help text, argument validation, and the headless Linux
//! refusal without opening a window.

use tempfile::tempdir;

use super::run_libra_command;

/// Compile-time guard: the desktop module is part of the default build, so a
/// future feature gate that removes it fails this test target at compile time.
#[test]
fn gui_module_is_available_in_default_builds() {
    let _run = libra::gui::run;
}

#[test]
fn ui_help_shows_examples() {
    let dir = tempdir().expect("temp dir");
    let output = run_libra_command(&["ui", "--help"], dir.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "`libra ui --help` should show examples, got: {stdout}"
    );
    assert!(
        stdout.contains("libra ui"),
        "examples should include the launch command, got: {stdout}"
    );
}

#[test]
fn ui_rejects_extra_arguments_without_opening_a_window() {
    let dir = tempdir().expect("temp dir");
    let output = run_libra_command(&["ui", "extra"], dir.path());
    assert_eq!(
        output.status.code(),
        Some(129),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected argument"),
        "extra arguments should be a usage error, got: {stderr}"
    );
}

/// The test harness clears the environment, so Linux has no display. A bare
/// `libra --json ui` must refuse before GPUI starts instead of hanging or
/// panicking, and must keep the structured error envelope.
#[cfg(target_os = "linux")]
#[test]
fn ui_without_display_refuses_before_opening_a_window() {
    let dir = tempdir().expect("temp dir");
    let output = run_libra_command(&["--json", "ui"], dir.path());
    assert_eq!(
        output.status.code(),
        Some(128),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LBR-UNSUPPORTED-001"),
        "headless `libra ui` should keep its stable error code, got: {stderr}"
    );
    assert!(
        stderr.contains("graphical display"),
        "headless `libra ui` should name the missing display, got: {stderr}"
    );
}
