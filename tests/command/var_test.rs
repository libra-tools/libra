//! Integration tests for `libra var`.

use regex::Regex;

use super::*;

const TX: &str = "Asia/Shanghai";

fn init_identity_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    assert_cli_success(
        &run_libra_command(&["config", "set", "user.name", "C O Mitter"], repo.path()),
        "config user.name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "set", "user.email", "c@d"], repo.path()),
        "config user.email",
    );
    repo
}

fn ident_regex(prefix: &str) -> Regex {
    Regex::new(&format!(r"^{prefix} <[^>]+> \d+ \+0800\n$")).expect("ident regex")
}

#[test]
fn var_ident_ignores_identity_and_date_env() {
    let repo = init_identity_repo();
    // AC1: the committer-side variables do not leak into this card's output.
    let out = run_libra_command_with_env(
        &["var", "GIT_COMMITTER_IDENT"],
        repo.path(),
        &[
            ("TZ", TX),
            ("GIT_COMMITTER_NAME", "Env Name"),
            ("GIT_AUTHOR_NAME", "A U Thor"),
            ("GIT_COMMITTER_EMAIL", "env@x"),
            ("GIT_AUTHOR_EMAIL", "a@b"),
            ("GIT_COMMITTER_DATE", "1700000000 +0000"),
            ("GIT_AUTHOR_DATE", "1700000000 +0000"),
        ],
    );
    assert_cli_success(&out, "AC1 libra var GIT_COMMITTER_IDENT");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(ident_regex("C O Mitter").is_match(&text), "AC1: {text}");

    // AC2: same for the author identity.
    let out = run_libra_command_with_env(
        &["var", "GIT_AUTHOR_IDENT"],
        repo.path(),
        &[
            ("TZ", TX),
            ("GIT_AUTHOR_NAME", "Env Name"),
            ("GIT_AUTHOR_EMAIL", "env@x"),
            ("GIT_AUTHOR_DATE", "1700000000 +0000"),
        ],
    );
    assert_cli_success(&out, "AC2 libra var GIT_AUTHOR_IDENT");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(ident_regex("C O Mitter").is_match(&text), "AC2: {text}");
}

#[test]
fn var_list_reports_both_idents_in_order() {
    let repo = init_identity_repo();
    // AC3: exactly two NAME=value lines, in Git's order.
    let out = run_libra_command_with_env(&["var", "-l"], repo.path(), &[("TZ", TX)]);
    assert_cli_success(&out, "AC3 libra var -l");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "AC3 line count: {text}");
    assert!(
        lines[0].starts_with("GIT_COMMITTER_IDENT=C O Mitter <c@d> "),
        "AC3 first line: {text}"
    );
    assert!(
        lines[1].starts_with("GIT_AUTHOR_IDENT=C O Mitter <c@d> "),
        "AC3 second line: {text}"
    );
}

#[test]
fn var_rejects_unknown_and_missing_repo() {
    let repo = init_identity_repo();
    // AC4: unknown names are invalid arguments.
    let out = run_libra_command(&["var", "BOGUS"], repo.path());
    assert!(!out.status.success(), "AC4 must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002"),
        "AC4 code"
    );

    // AC5: the command requires a repository.
    let outside = tempdir().expect("failed to create empty directory");
    let out = run_libra_command(&["var", "GIT_COMMITTER_IDENT"], outside.path());
    assert!(!out.status.success(), "AC5 must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-REPO-001"),
        "AC5 code"
    );
}

#[test]
fn var_json_and_missing_identity() {
    let repo = init_identity_repo();
    // AC6: the JSON envelope carries exactly {name, value}.
    let out = run_libra_command_with_env(
        &["--json", "var", "GIT_COMMITTER_IDENT"],
        repo.path(),
        &[("TZ", TX)],
    );
    assert_cli_success(&out, "AC6 libra --json var");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("AC6 JSON parses");
    assert_eq!(json["data"]["name"], "GIT_COMMITTER_IDENT", "AC6 name");
    assert!(
        ident_regex("C O Mitter").is_match(&format!(
            "{}\n",
            json["data"]["value"].as_str().unwrap_or("")
        )),
        "AC6 value: {json}"
    );

    // AC7: an unconfigured identity is a missing-credentials failure.
    let bare = tempdir().expect("failed to create repository root");
    init_repo_via_cli(bare.path());
    let out = run_libra_command(&["var", "GIT_COMMITTER_IDENT"], bare.path());
    assert!(!out.status.success(), "AC7 must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-AUTH-001"),
        "AC7 code"
    );
}
