use super::*;

fn dirty_context_repo(fixture: &Fixture, name: &str) -> PathBuf {
    let repo = fixture.path(name);
    fixture.init_repo(&repo);
    let body = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n";
    fixture.commit_file(&repo, "ctx.txt", body, "base");
    fs::write(repo.join("ctx.txt"), body.replace("l5", "L5")).expect("modify context file");
    repo
}

fn staged_rename_repo(fixture: &Fixture, name: &str) -> PathBuf {
    let repo = fixture.path(name);
    fixture.init_repo(&repo);
    let body = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
    fixture.commit_file(&repo, "old-name.txt", body, "base");
    fs::rename(repo.join("old-name.txt"), repo.join("new-name.txt")).expect("rename file");
    fixture.success(&repo, &["add", "-A"]);
    repo
}

fn assert_no_progress(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("Scanning working tree"), "{stderr}");
}

#[test]
fn diff_context_config_cascades_accepts_git_integers_and_cli_wins() {
    let fixture = Fixture::new();
    let repo = dirty_context_repo(&fixture, "diff-context-cascade");

    fixture.success(&repo, &["config", "--system", "diff.Context", "1k"]);
    let system = stdout_trim(&fixture.success(&repo, &["diff"]));
    assert!(system.contains(" l1"), "system 1k context: {system}");

    fixture.success(&repo, &["config", "--global", "diff.context", "1"]);
    let global = stdout_trim(&fixture.success(&repo, &["diff"]));
    assert!(global.contains(" l4"), "global context 1: {global}");
    assert!(
        !global.contains(" l3"),
        "global must override system: {global}"
    );

    fixture.success(&repo, &["config", "diff.context", "0"]);
    let local_zero = stdout_trim(&fixture.success(&repo, &["diff"]));
    assert!(!local_zero.contains(" l4"), "zero context: {local_zero}");
    fixture.success(&repo, &["config", "diff.context", "2"]);
    let local = stdout_trim(&fixture.success(&repo, &["diff"]));
    assert!(local.contains(" l3"), "local context 2: {local}");
    assert!(
        !local.contains(" l2"),
        "local must override global: {local}"
    );

    fixture.success(&repo, &["config", "diff.context", "2147483647"]);
    let int_max = stdout_trim(&fixture.success(&repo, &["diff"]));
    assert!(int_max.contains(" l1"), "Git int max context: {int_max}");
    fixture.success(&repo, &["config", "diff.context", "2097151k"]);
    let suffixed_int_max = stdout_trim(&fixture.success(&repo, &["diff"]));
    assert!(
        suffixed_int_max.contains(" l1"),
        "largest in-range k suffix: {suffixed_int_max}"
    );
    fixture.success(&repo, &["config", "diff.context", "2"]);

    let cli = stdout_trim(&fixture.success(&repo, &["diff", "-U3"]));
    assert!(cli.contains(" l2"), "-U3 must override config: {cli}");
}

#[test]
fn diff_context_errors_fail_before_progress_with_stable_codes() {
    let fixture = Fixture::new();
    let repo = dirty_context_repo(&fixture, "diff-context-errors");

    for invalid in [
        "-1",
        "abc",
        "2147483648",
        "2097152k",
        "9223372036854775807g",
    ] {
        fixture.success(&repo, &["config", "--", "diff.context", invalid]);
        let rejected = fixture.run(&repo, &["diff"]);
        assert_eq!(rejected.status.code(), Some(129), "value: {invalid}");
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
        assert!(stderr.contains("diff.context"), "{stderr}");
        assert!(rejected.stdout.is_empty());
        assert_no_progress(&rejected);
        fixture.success(&repo, &["config", "--unset", "diff.context"]);
    }

    fs::create_dir_all(fixture.home.join(".libra").join("config.db"))
        .expect("create unreadable global config path");
    let unreadable = fixture.run(&repo, &["diff"]);
    assert_eq!(unreadable.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&unreadable.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("diff.context"), "{stderr}");
    assert!(unreadable.stdout.is_empty());
    assert_no_progress(&unreadable);
}

#[test]
fn diff_renames_defaults_to_git_and_config_cascades_with_cli_precedence() {
    let fixture = Fixture::new();
    let repo = staged_rename_repo(&fixture, "diff-renames-cascade");

    let default = stdout_trim(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(
        default.contains("rename from old-name.txt"),
        "Git default: {default}"
    );

    fixture.success(&repo, &["config", "--system", "diff.ReNames", "false"]);
    let system = stdout_trim(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(!system.contains("rename from"), "system false: {system}");

    fixture.success(&repo, &["config", "--global", "diff.renames", "true"]);
    let global = stdout_trim(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(
        global.contains("rename from old-name.txt"),
        "global true: {global}"
    );

    fixture.success(&repo, &["config", "diff.renames", "false"]);
    let local = stdout_trim(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(!local.contains("rename from"), "local false: {local}");
    let cli_on = stdout_trim(&fixture.success(&repo, &["diff", "--staged", "-M"]));
    assert!(
        cli_on.contains("rename from old-name.txt"),
        "-M wins: {cli_on}"
    );

    fixture.success(&repo, &["config", "diff.renames", "copies"]);
    let copies = stdout_trim(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(
        copies.contains("rename from old-name.txt"),
        "copies degrades to rename detection: {copies}"
    );
    fixture.success(&repo, &["config", "diff.renames", "copy"]);
    let copy = stdout_trim(&fixture.success(&repo, &["diff", "--staged"]));
    assert!(
        copy.contains("rename from old-name.txt"),
        "copy degrades to rename detection: {copy}"
    );
    let cli_off = stdout_trim(&fixture.success(&repo, &["diff", "--staged", "--no-renames"]));
    assert!(
        !cli_off.contains("rename from"),
        "--no-renames wins: {cli_off}"
    );
}

#[test]
fn diff_renames_errors_fail_before_progress_with_stable_codes() {
    let fixture = Fixture::new();
    let repo = staged_rename_repo(&fixture, "diff-renames-errors");

    for invalid in ["", "sideways"] {
        fixture.success(&repo, &["config", "diff.renames", invalid]);
        let rejected = fixture.run(&repo, &["diff"]);
        assert_eq!(rejected.status.code(), Some(129), "value: {invalid:?}");
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
        assert!(stderr.contains("diff.renames"), "{stderr}");
        assert!(rejected.stdout.is_empty());
        assert_no_progress(&rejected);
        fixture.success(&repo, &["config", "--unset", "diff.renames"]);
    }

    fixture.success(&repo, &["config", "diff.context", "3"]);
    fs::create_dir_all(fixture.home.join(".libra").join("config.db"))
        .expect("create unreadable global config path");
    let unreadable = fixture.run(&repo, &["diff"]);
    assert_eq!(unreadable.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&unreadable.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("diff.renames"), "{stderr}");
    assert!(unreadable.stdout.is_empty());
    assert_no_progress(&unreadable);
}
