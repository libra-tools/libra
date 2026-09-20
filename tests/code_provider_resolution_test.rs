//! RC-16: `libra code` provider/boot flags are no longer a public surface.

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", "/tmp")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .output()
        .expect("failed to spawn libra")
}

fn diag_of(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
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
        diag.contains("not a libra command") || diag.contains("removed") || diag.contains("agent"),
        "{context} must refuse the retired Code CLI: {diag}"
    );
}

#[test]
fn libra_code_provider_flags_are_unknown() {
    let output = run(&["code", "--port", "0"]);
    assert_unknown_code(&output, "libra code --port");
}

#[test]
fn libra_code_resume_is_unknown() {
    let output = run(&["code", "--resume"]);
    assert_unknown_code(&output, "libra code --resume");
}
