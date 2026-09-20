//! RC-13: `libra code` is no longer a public command. This target pins
//! the unknown-command / usage-error surface and keeps the leftover
//! capture-graph checks that still apply (`agent graph`).

use std::process::Command;

fn libra_bin() -> &'static str {
    env!("CARGO_BIN_EXE_libra")
}

fn diag_of(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(libra_bin())
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", "/tmp")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .output()
        .expect("failed to spawn libra")
}

fn assert_unknown_code(output: &std::process::Output, context: &str) {
    assert_ne!(
        output.status.code(),
        Some(0),
        "{context} must not succeed: {}",
        diag_of(output)
    );
    let diag = diag_of(output);
    assert!(
        !diag.contains("Web Code UI"),
        "{context} must not advertise the removed Web Code UI: {diag}"
    );
}

#[test]
fn libra_code_is_unknown() {
    let output = run(&["code"]);
    assert_unknown_code(&output, "libra code");
    let diag = diag_of(&output);
    assert!(
        diag.contains("libra agent") || diag.contains("not a libra command"),
        "unknown `code` should mention libra agent or unknown-command: {diag}"
    );
}

#[test]
fn libra_code_control_stdio_is_unknown() {
    let output = run(&["code", "--control", "stdio"]);
    assert_unknown_code(&output, "libra code --control stdio");
}

#[test]
fn libra_code_help_is_unknown() {
    let output = run(&["code", "--help"]);
    assert_unknown_code(&output, "libra code --help");
}

#[test]
fn code_control_command_removed() {
    let output = run(&["code-control"]);
    assert_unknown_code(&output, "libra code-control");
}

/// Aggregate RC-13 / leftover W5 surface: `code` is gone, `code-control`
/// stays gone, top-level `graph` is gone, `agent graph` keeps the W5-08
/// interactive refusal.
#[test]
fn breaking_code_surface_migration() {
    let code = run(&["code"]);
    assert_unknown_code(&code, "aggregate libra code");
    let code_diag = diag_of(&code);
    assert!(
        code_diag.contains("libra agent") || code_diag.contains("not a libra command"),
        "aggregate code refusal should mention libra agent: {code_diag}"
    );

    let help = run(&["--help"]);
    assert!(help.status.success(), "root --help should succeed");
    let help_diag = diag_of(&help);
    assert!(
        !help_diag.contains("code-control"),
        "root --help must not list removed code-control"
    );
    // Command Groups must not advertise the removed `code` token as its own
    // command. The word can still appear inside "Claude Code" on `agent`.
    assert!(
        !help_diag.contains(" AI And Automation       code,")
            && !help_diag.contains(" code, automation"),
        "root --help Command Groups must not list `code`: {help_diag}"
    );

    let graph_dir = tempfile::tempdir().expect("tempdir for aggregate graph probe");
    let top_level = Command::new(libra_bin())
        .args(["graph", "11111111-1111-4111-8111-111111111111"])
        .current_dir(graph_dir.path())
        .output()
        .expect("run aggregate top-level graph probe");
    let top_level_diag = diag_of(&top_level);
    assert_ne!(
        top_level.status.code(),
        Some(0),
        "top-level libra graph must not succeed; got:\n{top_level_diag}"
    );

    let bare = Command::new(libra_bin())
        .args(["agent", "graph", "11111111-1111-4111-8111-111111111111"])
        .current_dir(graph_dir.path())
        .output()
        .expect("run aggregate bare agent graph probe");
    let bare_diag = diag_of(&bare);
    assert!(
        bare_diag.contains("no longer opens an interactive TUI"),
        "bare agent graph must surface the W5-08 removal diagnostic; got:\n{bare_diag}"
    );
}
