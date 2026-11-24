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
fn var_committer_ident_ignores_env_until_pm37() {
    let repo = init_identity_repo();
    // PM-10 AC1: the committer side still reports the configured identity and
    // "now" (PM-37 rewires it); the author side is covered separately below.
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
    assert_cli_success(&out, "PM-10 AC1 libra var GIT_COMMITTER_IDENT");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        ident_regex("C O Mitter").is_match(&text),
        "PM-10 AC1: {text}"
    );
}

#[test]
fn var_author_ident_uses_author_env() {
    let repo = init_identity_repo();
    let run = |pairs: &[(&'static str, &'static str)]| {
        let mut envs = vec![("TZ", TX)];
        envs.extend_from_slice(pairs);
        let out = run_libra_command_with_env(&["var", "GIT_AUTHOR_IDENT"], repo.path(), &envs);
        assert_cli_success(&out, "libra var GIT_AUTHOR_IDENT");
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    // AC1: the author-side name wins over the committer side.
    let text = run(&[
        ("GIT_AUTHOR_NAME", "A U Thor"),
        ("GIT_COMMITTER_NAME", "C O Mitter"),
    ]);
    assert!(ident_regex("A U Thor").is_match(&text), "AC1: {text}");

    // AC2: the author-side email wins over the committer side.
    let text = run(&[("GIT_AUTHOR_EMAIL", "a@b"), ("GIT_COMMITTER_EMAIL", "c@d")]);
    assert!(
        Regex::new(r"^C O Mitter <a@b> \d+ \+0800\n$")
            .expect("AC2 regex")
            .is_match(&text),
        "AC2: {text}"
    );

    // AC3: GIT_AUTHOR_DATE is honored verbatim.
    let text = run(&[("GIT_AUTHOR_DATE", "1700000000 +0800")]);
    assert_eq!(text, "C O Mitter <c@d> 1700000000 +0800\n", "AC3");

    // AC4: an invalid author date is a usage error.
    let out = run_libra_command_with_env(
        &["var", "GIT_AUTHOR_IDENT"],
        repo.path(),
        &[("TZ", TX), ("GIT_AUTHOR_DATE", "garbage")],
    );
    assert!(!out.status.success(), "AC4 must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002"),
        "AC4 code"
    );

    // AC5: only the author-side date is validated.
    let text = run(&[
        ("GIT_AUTHOR_DATE", "1700000000 +0800"),
        ("GIT_COMMITTER_DATE", "garbage"),
    ]);
    assert_eq!(text, "C O Mitter <c@d> 1700000000 +0800\n", "AC5");

    // AC6: Libra's cross-role fallback.
    let text = run(&[("GIT_COMMITTER_NAME", "C O Mitter")]);
    assert!(ident_regex("C O Mitter").is_match(&text), "AC6: {text}");

    // AC7: EMAIL is consulted before the configured email.
    let text = run(&[("EMAIL", "e@f")]);
    assert!(
        Regex::new(r"^C O Mitter <e@f> \d+ \+0800\n$")
            .expect("AC7 regex")
            .is_match(&text),
        "AC7: {text}"
    );

    // AC8: user.useConfigOnly suppresses only the identity env vars.
    assert_cli_success(
        &run_libra_command(
            &["config", "set", "user.useConfigOnly", "true"],
            repo.path(),
        ),
        "AC8 config",
    );
    let text = run(&[
        ("GIT_AUTHOR_NAME", "A U Thor"),
        ("GIT_AUTHOR_DATE", "1700000000 +0800"),
    ]);
    assert_eq!(text, "C O Mitter <c@d> 1700000000 +0800\n", "AC8");
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
