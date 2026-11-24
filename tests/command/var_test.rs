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
fn var_committer_ident_uses_committer_env() {
    let repo = init_identity_repo();
    let run = |pairs: &[(&'static str, &'static str)]| {
        let mut envs = vec![("TZ", TX)];
        envs.extend_from_slice(pairs);
        let out = run_libra_command_with_env(&["var", "GIT_COMMITTER_IDENT"], repo.path(), &envs);
        assert_cli_success(&out, "libra var GIT_COMMITTER_IDENT");
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    // AC1: the committer-side name wins over the author side.
    let text = run(&[
        ("GIT_COMMITTER_NAME", "C O Mitter"),
        ("GIT_AUTHOR_NAME", "A U Thor"),
    ]);
    assert!(ident_regex("C O Mitter").is_match(&text), "AC1: {text}");

    // AC2: the committer-side email wins over the author side.
    let text = run(&[("GIT_COMMITTER_EMAIL", "c@d"), ("GIT_AUTHOR_EMAIL", "a@b")]);
    assert!(
        Regex::new(r"^C O Mitter <c@d> \d+ \+0800\n$")
            .expect("AC2 regex")
            .is_match(&text),
        "AC2: {text}"
    );

    // AC3: GIT_COMMITTER_DATE is honored verbatim.
    let text = run(&[("GIT_COMMITTER_DATE", "1700000000 -0700")]);
    assert_eq!(text, "C O Mitter <c@d> 1700000000 -0700\n", "AC3");

    // AC4: an invalid committer date is a usage error.
    let out = run_libra_command_with_env(
        &["var", "GIT_COMMITTER_IDENT"],
        repo.path(),
        &[("TZ", TX), ("GIT_COMMITTER_DATE", "garbage")],
    );
    assert!(!out.status.success(), "AC4 must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002"),
        "AC4 code"
    );

    // AC5: only the committer-side date is validated.
    let text = run(&[
        ("GIT_COMMITTER_DATE", "1700000000 -0700"),
        ("GIT_AUTHOR_DATE", "garbage"),
    ]);
    assert_eq!(text, "C O Mitter <c@d> 1700000000 -0700\n", "AC5");

    // AC6: Libra's cross-role fallback.
    let text = run(&[("GIT_AUTHOR_NAME", "A U Thor")]);
    assert!(ident_regex("A U Thor").is_match(&text), "AC6: {text}");

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
        ("GIT_COMMITTER_NAME", "C O Mitter"),
        ("GIT_COMMITTER_DATE", "1700000000 -0700"),
    ]);
    assert_eq!(text, "C O Mitter <c@d> 1700000000 -0700\n", "AC8");
    let _ = run_libra_command(
        &["config", "set", "user.useConfigOnly", "false"],
        repo.path(),
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
fn var_list_orders_supported_variables() {
    let repo = init_identity_repo();
    // The identities stay first and in order; the editor rows follow (they are
    // skipped when TERM is unusable, so this asserts the prefix, not the count).
    let out = run_libra_command_with_env(&["var", "-l"], repo.path(), &[("TZ", TX)]);
    assert_cli_success(&out, "libra var -l");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() >= 2, "line count: {text}");
    assert!(
        lines[0].starts_with("GIT_COMMITTER_IDENT=C O Mitter <c@d> "),
        "first line: {text}"
    );
    assert!(
        lines[1].starts_with("GIT_AUTHOR_IDENT=C O Mitter <c@d> "),
        "second line: {text}"
    );
    for line in &lines[2..] {
        assert!(
            [
                "GIT_EDITOR=",
                "GIT_SEQUENCE_EDITOR=",
                "GIT_PAGER=",
                "GIT_DEFAULT_BRANCH=",
                "GIT_ATTR_GLOBAL="
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix)),
            "unexpected listing row: {line}"
        );
    }
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

#[test]
fn var_editor_variable() {
    let repo = init_identity_repo();
    let set = |key: &str, value: &str| {
        assert_cli_success(
            &run_libra_command(&["config", "set", key, value], repo.path()),
            "config set",
        );
    };
    let unset = |key: &str| {
        let _ = run_libra_command(&["config", "unset", key], repo.path());
    };
    let var = |envs: &[(&'static str, &'static str)]| {
        run_libra_command_with_env(&["var", "GIT_EDITOR"], repo.path(), envs)
    };

    // AC1: GIT_EDITOR wins over core.editor.
    set("core.editor", "emacs");
    let out = var(&[("GIT_EDITOR", "nano"), ("TERM", "xterm")]);
    assert_cli_success(&out, "AC1");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "nano\n", "AC1");

    // AC2: core.editor is used when GIT_EDITOR is unset.
    let out = var(&[("TERM", "xterm")]);
    assert_cli_success(&out, "AC2");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "emacs\n", "AC2");
    unset("core.editor");

    // AC3: VISUAL precedes EDITOR.
    let out = var(&[("TERM", "xterm"), ("VISUAL", "vim"), ("EDITOR", "ed")]);
    assert_cli_success(&out, "AC3");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "vim\n", "AC3");

    // AC4: with nothing configured, a usable TERM falls back to vi.
    let out = var(&[("TERM", "xterm")]);
    assert_cli_success(&out, "AC4");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "vi\n", "AC4");

    // AC5/AC6: an unusable TERM exits 1 with empty stdout.
    for envs in [&[][..], &[("TERM", "dumb")][..]] {
        let out = var(envs);
        assert_eq!(out.status.code(), Some(1), "AC5/6 exit");
        assert!(out.stdout.is_empty(), "AC5/6 stdout");
    }
}

#[test]
fn var_sequence_editor_variable() {
    let repo = init_identity_repo();
    let set = |key: &str, value: &str| {
        assert_cli_success(
            &run_libra_command(&["config", "set", key, value], repo.path()),
            "config set",
        );
    };
    let unset = |key: &str| {
        let _ = run_libra_command(&["config", "unset", key], repo.path());
    };
    let var = |envs: &[(&'static str, &'static str)]| {
        run_libra_command_with_env(&["var", "GIT_SEQUENCE_EDITOR"], repo.path(), envs)
    };

    // AC1: GIT_SEQUENCE_EDITOR wins over sequence.editor.
    set("sequence.editor", "other");
    let out = var(&[("GIT_SEQUENCE_EDITOR", "seq"), ("TERM", "xterm")]);
    assert_cli_success(&out, "AC1");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "seq\n", "AC1");

    // AC2: sequence.editor is used when the variable is unset.
    let out = var(&[("TERM", "xterm")]);
    assert_cli_success(&out, "AC2");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "other\n", "AC2");
    unset("sequence.editor");

    // AC3: the sequence chain falls back to the resolved editor.
    let out = var(&[("GIT_EDITOR", "nano"), ("TERM", "xterm")]);
    assert_cli_success(&out, "AC3");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "nano\n", "AC3");

    // AC4: neither configured and TERM is usable -> vi.
    let out = var(&[("TERM", "xterm")]);
    assert_cli_success(&out, "AC4");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "vi\n", "AC4");

    // AC5: no TERM -> exit 1, no output.
    let out = var(&[]);
    assert_eq!(out.status.code(), Some(1), "AC5 exit");
    assert!(out.stdout.is_empty(), "AC5 stdout");
}

#[test]
fn var_misc_variables_and_list_order() {
    let repo = init_identity_repo();
    let home = repo.path().join(".libra-test-home");
    let set = |key: &str, value: &str| {
        assert_cli_success(
            &run_libra_command(&["config", "set", key, value], repo.path()),
            "config set",
        );
    };

    // AC1: Git's pager variables are deliberately ignored.
    let out = run_libra_command_with_env(
        &["var", "GIT_PAGER"],
        repo.path(),
        &[("TZ", TX), ("GIT_PAGER", "most"), ("PAGER", "more")],
    );
    assert_cli_success(&out, "AC1");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "less -R -F\n", "AC1");

    // AC2: paging disabled reports cat.
    let out = run_libra_command_with_env(
        &["var", "GIT_PAGER"],
        repo.path(),
        &[("TZ", TX), ("LIBRA_PAGER", "never")],
    );
    assert_cli_success(&out, "AC2");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "cat\n", "AC2");

    // AC3/AC4: init.defaultBranch is honored, else main.
    let out = run_libra_command(&["var", "GIT_DEFAULT_BRANCH"], repo.path());
    assert_cli_success(&out, "AC3");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "main\n", "AC3");
    set("init.defaultBranch", "trunk");
    let out = run_libra_command(&["var", "GIT_DEFAULT_BRANCH"], repo.path());
    assert_cli_success(&out, "AC4");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "trunk\n", "AC4");
    let _ = run_libra_command(&["config", "unset", "init.defaultBranch"], repo.path());

    // AC5: core.attributesFile is expanded against HOME.
    set("core.attributesFile", "~/my_attrs");
    let out = run_libra_command(&["var", "GIT_ATTR_GLOBAL"], repo.path());
    assert_cli_success(&out, "AC5");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        home.join("my_attrs").display().to_string(),
        "AC5"
    );
    let _ = run_libra_command(&["config", "unset", "core.attributesFile"], repo.path());

    // AC6: without configuration the XDG default path is reported.
    let xdg = repo.path().join("xdg");
    let out = run_libra_command_with_env(
        &["var", "GIT_ATTR_GLOBAL"],
        repo.path(),
        &[("XDG_CONFIG_HOME", xdg.to_str().expect("utf8 xdg path"))],
    );
    assert_cli_success(&out, "AC6");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        xdg.join("git").join("attributes").display().to_string(),
        "AC6"
    );

    // AC7: there is no system attributes file, so the variable is unavailable.
    let out = run_libra_command(&["var", "GIT_ATTR_SYSTEM"], repo.path());
    assert_eq!(out.status.code(), Some(1), "AC7 exit");
    assert!(out.stdout.is_empty(), "AC7 stdout");

    // AC8: `-l` lists every resolvable variable in Git's order. Pin the dates so
    // the two ident rows are stable.
    let out = run_libra_command_with_env(
        &["var", "-l"],
        repo.path(),
        &[
            ("TZ", TX),
            ("TERM", "xterm"),
            ("GIT_AUTHOR_DATE", "1700000000 +0000"),
            ("GIT_COMMITTER_DATE", "1700000000 +0000"),
        ],
    );
    assert_cli_success(&out, "AC8");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        text,
        "GIT_COMMITTER_IDENT=C O Mitter <c@d> 1700000000 +0000\n\
GIT_AUTHOR_IDENT=C O Mitter <c@d> 1700000000 +0000\n\
GIT_EDITOR=vi\n\
GIT_SEQUENCE_EDITOR=vi\n\
GIT_PAGER=less -R -F\n\
GIT_DEFAULT_BRANCH=main\n\
GIT_ATTR_GLOBAL=<HOME>\n"
            .replace(
                "<HOME>",
                &home
                    .join(".config")
                    .join("git")
                    .join("attributes")
                    .display()
                    .to_string()
            ),
        "AC8 listing"
    );
}
