//! Integration tests for `ls-files`.
//!
//! **Layer:** L1 — deterministic local repositories, no network.

use std::{fs, process::Output};

use super::*;

fn setup_ls_files_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::create_dir_all(repo.path().join("tracked-dir")).expect("create tracked dir");
    fs::create_dir_all(repo.path().join("中文目录")).expect("create chinese tracked dir");
    fs::create_dir_all(repo.path().join("others-dir")).expect("create untracked dir");

    fs::write(
        repo.path().join(".libraignore"),
        "ignored.tmp\nothers-dir/*.tmp\n",
    )
    .expect("write ignore file");
    fs::write(repo.path().join("tracked.txt"), "tracked\n").expect("write tracked file");
    fs::write(repo.path().join("delete.txt"), "delete me\n").expect("write delete fixture");
    fs::write(repo.path().join("tracked-dir").join("alpha.txt"), "alpha\n")
        .expect("write tracked dir alpha");
    fs::write(repo.path().join("tracked-dir").join("bravo.txt"), "bravo\n")
        .expect("write tracked dir bravo");
    fs::write(repo.path().join("中文目录").join("条目.txt"), "unicode\n")
        .expect("write chinese tracked file");
    fs::write(repo.path().join("special [name].txt"), "special\n")
        .expect("write special tracked file");

    let add = run_libra_command(
        &[
            "add",
            ".libraignore",
            "tracked.txt",
            "delete.txt",
            "tracked-dir",
            "中文目录",
            "special [name].txt",
        ],
        repo.path(),
    );
    assert_cli_success(&add, "failed to add ls-files fixture files");

    let commit = run_libra_command(
        &["commit", "-m", "ls-files fixture", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&commit, "failed to commit ls-files fixture");

    fs::write(repo.path().join("tracked.txt"), "tracked and modified\n")
        .expect("modify tracked file");
    fs::remove_file(repo.path().join("delete.txt")).expect("delete tracked file");
    fs::write(repo.path().join("untracked.txt"), "untracked\n").expect("write untracked file");
    fs::write(repo.path().join("ignored.tmp"), "ignored\n").expect("write ignored file");
    fs::write(
        repo.path().join("others-dir").join("untracked.txt"),
        "nested untracked\n",
    )
    .expect("write nested untracked file");
    fs::write(
        repo.path().join("others-dir").join("ignored.tmp"),
        "nested ignored\n",
    )
    .expect("write nested ignored file");

    repo
}

fn stdout_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.to_string())
        .collect()
}

fn stdout_nul_fields(output: &Output) -> Vec<String> {
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8(field.to_vec()).expect("expected UTF-8 field"))
        .collect()
}

#[test]
fn ls_files_help_is_visible_and_renders_examples() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["ls-files", "--help"], repo.path());
    assert_cli_success(&output, "ls-files --help should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "ls-files --help should render examples, stdout={stdout}"
    );
}

#[test]
fn ls_files_defaults_to_cached_listing() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files"], repo.path());
    assert_cli_success(&output, "ls-files should succeed");

    assert_eq!(
        stdout_lines(&output),
        vec![
            ".libraignore".to_string(),
            "delete.txt".to_string(),
            "special [name].txt".to_string(),
            "tracked-dir/alpha.txt".to_string(),
            "tracked-dir/bravo.txt".to_string(),
            "tracked.txt".to_string(),
            "中文目录/条目.txt".to_string(),
        ]
    );
}

#[test]
fn ls_files_modified_lists_only_modified_tracked_paths() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "--modified"], repo.path());
    assert_cli_success(&output, "ls-files --modified should succeed");

    assert_eq!(stdout_lines(&output), vec!["tracked.txt".to_string()]);
}

#[test]
fn ls_files_deleted_lists_only_missing_tracked_paths() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "--deleted"], repo.path());
    assert_cli_success(&output, "ls-files --deleted should succeed");

    assert_eq!(stdout_lines(&output), vec!["delete.txt".to_string()]);
}

#[test]
fn ls_files_others_lists_untracked_paths_without_ignore_filtering() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "--others"], repo.path());
    assert_cli_success(&output, "ls-files --others should succeed");

    assert_eq!(
        stdout_lines(&output),
        vec![
            "ignored.tmp".to_string(),
            "others-dir/ignored.tmp".to_string(),
            "others-dir/untracked.txt".to_string(),
            "untracked.txt".to_string(),
        ]
    );
}

#[test]
fn ls_files_exclude_standard_honors_libraignore_for_others() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "--others", "--exclude-standard"], repo.path());
    assert_cli_success(
        &output,
        "ls-files --others --exclude-standard should succeed",
    );

    assert_eq!(
        stdout_lines(&output),
        vec![
            "others-dir/untracked.txt".to_string(),
            "untracked.txt".to_string()
        ]
    );
}

#[test]
fn ls_files_stage_and_short_alias_render_same_stage_output() {
    let repo = setup_ls_files_repo();

    let stage = run_libra_command(&["ls-files", "--stage"], repo.path());
    assert_cli_success(&stage, "ls-files --stage should succeed");

    let short = run_libra_command(&["ls-files", "-s"], repo.path());
    assert_cli_success(&short, "ls-files -s should succeed");

    let stage_stdout = String::from_utf8_lossy(&stage.stdout);
    assert!(
        stage_stdout
            .lines()
            .any(|line| line.contains(" 0\ttracked.txt")),
        "--stage output should include stage 0 tracked.txt entry, stdout={stage_stdout}"
    );
    assert_eq!(stage.stdout, short.stdout, "--stage and -s should match");
}

#[test]
fn ls_files_json_uses_standard_envelope() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["--json", "ls-files", "--modified"], repo.path());
    assert_cli_success(&output, "json ls-files --modified should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "ls-files");

    let data = json["data"]
        .as_array()
        .expect("ls-files data should be an array");
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["path"], "tracked.txt");
    assert_eq!(data[0]["status"], "modified");
    assert_eq!(data[0]["stage"], 0);
    assert!(data[0]["hash"].is_string());
    assert!(data[0]["mode"].is_string());
}

#[test]
fn ls_files_pathspec_filters_to_an_exact_file() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "tracked-dir/alpha.txt"], repo.path());
    assert_cli_success(&output, "ls-files <file> should succeed");

    assert_eq!(
        stdout_lines(&output),
        vec!["tracked-dir/alpha.txt".to_string()]
    );
}

#[test]
fn ls_files_pathspec_filters_to_a_directory_prefix() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "tracked-dir"], repo.path());
    assert_cli_success(&output, "ls-files <dir> should succeed");

    assert_eq!(
        stdout_lines(&output),
        vec![
            "tracked-dir/alpha.txt".to_string(),
            "tracked-dir/bravo.txt".to_string(),
        ]
    );
}

#[test]
fn ls_files_others_pathspec_lists_untracked_paths_under_directory() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "--others", "others-dir"], repo.path());
    assert_cli_success(&output, "ls-files --others <dir> should succeed");

    assert_eq!(
        stdout_lines(&output),
        vec![
            "others-dir/ignored.tmp".to_string(),
            "others-dir/untracked.txt".to_string(),
        ]
    );
}

#[test]
fn ls_files_others_exclude_standard_honors_libraignore_for_directory_pathspec() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(
        &["ls-files", "--others", "--exclude-standard", "others-dir"],
        repo.path(),
    );
    assert_cli_success(
        &output,
        "ls-files --others --exclude-standard <dir> should succeed",
    );

    assert_eq!(
        stdout_lines(&output),
        vec!["others-dir/untracked.txt".to_string()]
    );
}

#[test]
fn ls_files_pathspec_is_resolved_from_nested_current_dir() {
    let repo = setup_ls_files_repo();
    let nested_cwd = repo.path().join("nested-cwd");
    fs::create_dir_all(&nested_cwd).expect("create nested cwd");

    let output = run_libra_command(&["ls-files", "../tracked-dir"], &nested_cwd);
    assert_cli_success(&output, "ls-files should resolve pathspecs from cwd");

    assert_eq!(
        stdout_lines(&output),
        vec![
            "tracked-dir/alpha.txt".to_string(),
            "tracked-dir/bravo.txt".to_string(),
        ]
    );
}

#[test]
fn ls_files_pathspec_accepts_chinese_names() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "中文目录/条目.txt"], repo.path());
    assert_cli_success(&output, "ls-files should accept chinese pathspecs");

    assert_eq!(stdout_lines(&output), vec!["中文目录/条目.txt".to_string()]);
}

#[test]
fn ls_files_pathspec_accepts_special_character_names() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "special [name].txt"], repo.path());
    assert_cli_success(
        &output,
        "ls-files should accept special-character pathspecs",
    );

    assert_eq!(
        stdout_lines(&output),
        vec!["special [name].txt".to_string()]
    );
}

#[test]
fn ls_files_stage_output_respects_pathspecs() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "--stage", "tracked-dir"], repo.path());
    assert_cli_success(&output, "ls-files --stage <dir> should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .all(|line| line.ends_with("tracked-dir/alpha.txt")
                || line.ends_with("tracked-dir/bravo.txt")),
        "stage output should be limited to tracked-dir entries, stdout={stdout}"
    );
}

#[test]
fn ls_files_empty_pathspec_result_is_allowed_without_error_unmatch() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "missing.txt"], repo.path());
    assert_cli_success(
        &output,
        "ls-files without --error-unmatch should allow empty pathspec results",
    );

    assert!(
        output.stdout.is_empty(),
        "stdout should be empty: {:?}",
        output.stdout
    );
}

#[test]
fn ls_files_z_outputs_nul_delimited_records() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "-z", "tracked-dir"], repo.path());
    assert_cli_success(&output, "ls-files -z should succeed");

    assert_eq!(
        stdout_nul_fields(&output),
        vec![
            "tracked-dir/alpha.txt".to_string(),
            "tracked-dir/bravo.txt".to_string(),
        ]
    );
    assert!(
        !output.stdout.contains(&b'\n'),
        "nul output should not contain newlines: {:?}",
        output.stdout
    );
    assert_eq!(
        output.stdout.last(),
        Some(&0),
        "nul output should end with a NUL byte"
    );
}

#[test]
fn ls_files_error_unmatch_fails_for_missing_pathspec() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["ls-files", "--error-unmatch", "missing.txt"], repo.path());
    assert_eq!(output.status.code(), Some(1));

    let (_, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        report
            .message
            .contains("pathspec 'missing.txt' did not match any files"),
        "message was: {}",
        report.message
    );
}

#[test]
fn ls_files_error_unmatch_fails_when_any_pathspec_is_missing() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(
        &["ls-files", "--error-unmatch", "tracked.txt", "missing.txt"],
        repo.path(),
    );
    assert_eq!(output.status.code(), Some(1));

    let (_, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        report
            .message
            .contains("pathspec 'missing.txt' did not match any files"),
        "message was: {}",
        report.message
    );
}

#[test]
fn ls_files_pathspec_rejects_paths_outside_repo() {
    let repo = setup_ls_files_repo();
    let nested_cwd = repo.path().join("nested-cwd");
    fs::create_dir_all(&nested_cwd).expect("create nested cwd");

    let output = run_libra_command(
        &["ls-files", "--error-unmatch", "../../outside.txt"],
        &nested_cwd,
    );
    assert_eq!(output.status.code(), Some(129));

    let (_, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        report.message.contains("outside repository"),
        "message was: {}",
        report.message
    );
}

#[test]
fn ls_files_rejects_z_with_json_output() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["--json", "ls-files", "-z"], repo.path());
    assert_eq!(output.status.code(), Some(129));

    let (_, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        report.message.contains("ls-files -z cannot be combined"),
        "message was: {}",
        report.message
    );
}

#[test]
fn ls_files_rejects_z_with_machine_output() {
    let repo = setup_ls_files_repo();

    let output = run_libra_command(&["--machine", "ls-files", "-z"], repo.path());
    assert_eq!(output.status.code(), Some(129));

    let (_, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        report.message.contains("ls-files -z cannot be combined"),
        "message was: {}",
        report.message
    );
}

#[test]
fn ls_files_t_prefixes_status_tags() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Cached files are tagged H.
    let out = run_libra_command(&["ls-files", "-t"], p);
    assert_cli_success(&out, "ls-files -t");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        s.lines().any(|l| l == "H tracked.txt"),
        "cached -> H: {s:?}"
    );
    assert!(
        s.lines().all(|l| l.starts_with("H ")),
        "all cached -> H: {s:?}"
    );

    // Untracked files are tagged ?.
    fs::write(p.join("untracked.txt"), "x\n").unwrap();
    let out = run_libra_command(&["ls-files", "-t", "--others", "--exclude-standard"], p);
    assert_cli_success(&out, "ls-files -t --others");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        s.lines().any(|l| l == "? untracked.txt"),
        "untracked -> ?: {s:?}"
    );

    // Modified files are tagged C.
    fs::write(p.join("tracked.txt"), "tracked changed\n").unwrap();
    let out = run_libra_command(&["ls-files", "-t", "--modified"], p);
    assert_cli_success(&out, "ls-files -t --modified");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        s.lines().any(|l| l == "C tracked.txt"),
        "modified -> C: {s:?}"
    );

    // Deleted files are tagged R.
    fs::remove_file(p.join("tracked.txt")).unwrap();
    let out = run_libra_command(&["ls-files", "-t", "--deleted"], p);
    assert_cli_success(&out, "ls-files -t --deleted");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        s.lines().any(|l| l == "R tracked.txt"),
        "deleted -> R: {s:?}"
    );
}

#[test]
fn ls_files_u_shows_unmerged_conflict_entries() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Build a merge conflict on conf.txt via two divergent branches.
    fs::write(p.join("conf.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "conf.txt"], p), "add conf");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base-conf", "--no-verify"], p),
        "commit base-conf",
    );
    assert_cli_success(&run_libra_command(&["branch", "other"], p), "branch other");
    fs::write(p.join("conf.txt"), "main-change\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "conf.txt"], p), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-c", "--no-verify"], p),
        "commit main-c",
    );
    assert_cli_success(&run_libra_command(&["switch", "other"], p), "switch other");
    fs::write(p.join("conf.txt"), "other-change\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "conf.txt"], p), "add other");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "other-c", "--no-verify"], p),
        "commit other-c",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    // The merge conflicts (non-zero exit expected); the conflict stays in the index.
    let _ = run_libra_command(&["merge", "other"], p);

    // -u lists the three conflict stages for conf.txt in stage format.
    let out = run_libra_command(&["ls-files", "-u"], p);
    assert_cli_success(&out, "ls-files -u");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        s.lines().any(|l| l.contains(" 1\tconf.txt")),
        "stage 1: {s:?}"
    );
    assert!(
        s.lines().any(|l| l.contains(" 2\tconf.txt")),
        "stage 2: {s:?}"
    );
    assert!(
        s.lines().any(|l| l.contains(" 3\tconf.txt")),
        "stage 3: {s:?}"
    );
    // -u shows ONLY unmerged entries, not cleanly-staged files.
    assert!(
        !s.contains("tracked.txt"),
        "clean entries excluded from -u: {s:?}"
    );
}

#[test]
fn ls_files_full_name_accepted_as_noop() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // `--full-name` is accepted (Git compatibility) and produces the same
    // repo-root-relative output Libra emits by default.
    let plain = run_libra_command(&["ls-files"], p);
    assert_cli_success(&plain, "ls-files");
    let with_flag = run_libra_command(&["ls-files", "--full-name"], p);
    assert_cli_success(&with_flag, "ls-files --full-name");
    assert_eq!(
        String::from_utf8_lossy(&plain.stdout),
        String::from_utf8_lossy(&with_flag.stdout),
        "--full-name is a no-op matching default output"
    );
    // Paths are repo-root-relative (the `git --full-name` form).
    assert!(
        String::from_utf8_lossy(&with_flag.stdout)
            .lines()
            .any(|l| l == "tracked.txt"),
        "root-relative path present"
    );
}

#[test]
fn test_ls_files_abbrev_shortens_object_name() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("f.txt"), "content\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c", "--no-verify"], p),
        "commit",
    );

    // -s shows the full 40-char object name.
    let full = run_libra_command(&["ls-files", "-s", "f.txt"], p);
    assert_cli_success(&full, "ls-files -s");
    let full_out = String::from_utf8_lossy(&full.stdout);
    let full_hash = full_out.split_whitespace().nth(1).expect("hash field");
    assert_eq!(full_hash.len(), 40, "full hash: {full_out:?}");

    // --abbrev=8 truncates the object name to 8 digits.
    let ab8 = run_libra_command(&["ls-files", "-s", "--abbrev=8", "f.txt"], p);
    assert_cli_success(&ab8, "ls-files -s --abbrev=8");
    let ab8_out = String::from_utf8_lossy(&ab8.stdout);
    let ab8_hash = ab8_out.split_whitespace().nth(1).expect("hash field");
    assert_eq!(ab8_hash.len(), 8, "abbrev=8 hash: {ab8_out:?}");
    assert!(
        full_hash.starts_with(ab8_hash),
        "abbrev is a prefix of the full hash"
    );

    // Bare --abbrev defaults to 7.
    let ab = run_libra_command(&["ls-files", "-s", "--abbrev", "f.txt"], p);
    assert_cli_success(&ab, "ls-files -s --abbrev");
    let ab_out = String::from_utf8_lossy(&ab.stdout);
    assert_eq!(
        ab_out.split_whitespace().nth(1).expect("hash").len(),
        7,
        "bare --abbrev = 7: {ab_out:?}"
    );
}

#[test]
fn test_ls_files_ignored() {
    // `-i`/`--ignored` lists the ignored set: `-i -o` shows ignored UNTRACKED files
    // (inverse of `-o`), `-i -c` shows tracked files matching an exclude pattern.
    // `-i` requires `-o`/`-c` plus an exclude source — `--exclude-standard` or an
    // explicit `-x`/`-X` pattern (the latter covered by
    // `test_ls_files_ignored_with_custom_exclude`) — matching git.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join(".libraignore"), "build/\n*.log\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", ".libraignore"], p),
        "add ignore",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ignore", "--no-verify"], p),
        "commit ignore",
    );
    std::fs::create_dir_all(p.join("build")).unwrap();
    std::fs::write(p.join("build/out.o"), "x\n").unwrap();
    std::fs::write(p.join("debug.log"), "log\n").unwrap();
    std::fs::write(p.join("keep.txt"), "normal\n").unwrap();

    let lines = |args: &[&str]| -> Vec<String> {
        let out = run_libra_command(args, p);
        assert_cli_success(&out, "ls-files -i");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    };

    // -i -o: ignored untracked files (NOT the non-ignored keep.txt).
    let ignored = lines(&["ls-files", "-i", "-o", "--exclude-standard"]);
    assert!(
        ignored.iter().any(|l| l == "build/out.o") && ignored.iter().any(|l| l == "debug.log"),
        "-i -o lists ignored untracked files: {ignored:?}"
    );
    assert!(
        !ignored.iter().any(|l| l == "keep.txt"),
        "-i -o excludes the non-ignored file: {ignored:?}"
    );
    // Plain -o (no -i) is the inverse: shows keep.txt, not the ignored files.
    let others = lines(&["ls-files", "-o", "--exclude-standard"]);
    assert!(
        others.iter().any(|l| l == "keep.txt") && !others.iter().any(|l| l == "debug.log"),
        "-o (without -i) lists only non-ignored untracked files: {others:?}"
    );

    // -i -c: a tracked file matching an exclude pattern (force-added) is listed.
    std::fs::write(p.join("tracked.log"), "data\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "-f", "tracked.log"], p),
        "force-add ignored file",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "track ignored", "--no-verify"], p),
        "commit tracked.log",
    );
    let cached_ignored = lines(&["ls-files", "-i", "-c", "--exclude-standard"]);
    assert!(
        cached_ignored.iter().any(|l| l == "tracked.log"),
        "-i -c lists tracked files matching an exclude pattern: {cached_ignored:?}"
    );

    // `-i -o` stays others-only even with a stage-style display flag (`-s`): the
    // force-added ignored TRACKED file must NOT leak in (the cached block only runs
    // under an explicit `-c` in ignored mode).
    let io_stage = lines(&["ls-files", "-i", "-o", "-s", "--exclude-standard"]);
    assert!(
        !io_stage.iter().any(|l| l.contains("tracked.log")),
        "-i -o -s must not list the tracked ignored file: {io_stage:?}"
    );

    // -i requires -o/-c (exit 128) and an exclude source (exit 128), with Git's
    // exact fatal messages.
    let no_mode = run_libra_command(&["ls-files", "-i", "--exclude-standard"], p);
    assert_eq!(
        no_mode.status.code(),
        Some(128),
        "-i without -o/-c exits 128"
    );
    assert!(
        String::from_utf8_lossy(&no_mode.stderr)
            .contains("ls-files -i must be used with either -o or -c"),
        "expected the -o/-c requirement message: {}",
        String::from_utf8_lossy(&no_mode.stderr)
    );
    let no_exclude = run_libra_command(&["ls-files", "-i", "-o"], p);
    assert_eq!(
        no_exclude.status.code(),
        Some(128),
        "-i without exclude source exits 128"
    );
    assert!(
        String::from_utf8_lossy(&no_exclude.stderr)
            .contains("ls-files --ignored needs some exclude pattern"),
        "expected the exclude-source requirement message: {}",
        String::from_utf8_lossy(&no_exclude.stderr)
    );
}

#[test]
fn modified_and_deleted_short_flags_alias_long_forms() {
    let repo = setup_ls_files_repo();
    let p = repo.path();

    // `-m` is `--modified`: the fixture modifies `tracked.txt`.
    let m_short = run_libra_command(&["ls-files", "-m"], p);
    assert_cli_success(&m_short, "ls-files -m");
    let m_long = run_libra_command(&["ls-files", "--modified"], p);
    assert_cli_success(&m_long, "ls-files --modified");
    assert_eq!(
        stdout_lines(&m_short),
        stdout_lines(&m_long),
        "-m must match --modified"
    );
    assert!(
        stdout_lines(&m_short)
            .iter()
            .any(|candidate| candidate.as_str() == "tracked.txt"),
        "-m lists the modified file: {:?}",
        stdout_lines(&m_short)
    );

    // `-d` is `--deleted`: the fixture removes `delete.txt`.
    let d_short = run_libra_command(&["ls-files", "-d"], p);
    assert_cli_success(&d_short, "ls-files -d");
    let d_long = run_libra_command(&["ls-files", "--deleted"], p);
    assert_cli_success(&d_long, "ls-files --deleted");
    assert_eq!(
        stdout_lines(&d_short),
        stdout_lines(&d_long),
        "-d must match --deleted"
    );
    assert!(
        stdout_lines(&d_short)
            .iter()
            .any(|candidate| candidate.as_str() == "delete.txt"),
        "-d lists the deleted file: {:?}",
        stdout_lines(&d_short)
    );
}

/// `-o -x <pattern>` excludes untracked files matching the pattern; `-X <file>`
/// reads additional patterns from a file (gitignore syntax, comments/blanks
/// skipped). Both supplement the `--others` listing.
#[test]
fn test_ls_files_exclude_pattern_and_file() {
    let repo = tempdir().expect("repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("keep.rs"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "keep.rs"], p), "add keep.rs");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    fs::write(p.join("a.log"), "l\n").unwrap();
    fs::write(p.join("b.log"), "l\n").unwrap();
    fs::write(p.join("c.dat"), "d\n").unwrap();
    fs::write(p.join("note.txt"), "n\n").unwrap();

    // -x '*.log' drops the logs from the others listing.
    let out = run_libra_command(&["ls-files", "-o", "-x", "*.log"], p);
    assert_cli_success(&out, "ls-files -o -x");
    let listed = String::from_utf8(out.stdout).unwrap();
    assert!(
        !listed.contains("a.log") && !listed.contains("b.log"),
        "logs excluded: {listed}"
    );
    assert!(
        listed.contains("c.dat") && listed.contains("note.txt"),
        "others kept: {listed}"
    );

    // -X file: combine *.log + *.dat from a file.
    fs::write(p.join("ex.txt"), "# comment\n\n*.log\n*.dat\n").unwrap();
    let out = run_libra_command(&["ls-files", "-o", "-X", "ex.txt"], p);
    assert_cli_success(&out, "ls-files -o -X");
    let listed = String::from_utf8(out.stdout).unwrap();
    assert!(
        !listed.contains("a.log") && !listed.contains("c.dat"),
        "file patterns excluded logs and dat: {listed}"
    );
    assert!(
        listed.contains("note.txt"),
        "unmatched other kept: {listed}"
    );
}

/// `-i -o -x <pattern>` lists ONLY the untracked files matching the explicit
/// pattern (the custom exclude defines the ignored set; `--exclude-standard` is
/// not required when an explicit pattern is supplied).
#[test]
fn test_ls_files_ignored_with_custom_exclude() {
    let repo = tempdir().expect("repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("keep.rs"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "keep.rs"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    fs::write(p.join("a.log"), "l\n").unwrap();
    fs::write(p.join("note.txt"), "n\n").unwrap();

    let out = run_libra_command(&["ls-files", "-i", "-o", "-x", "*.log"], p);
    assert_cli_success(&out, "ls-files -i -o -x (no --exclude-standard)");
    let listed = String::from_utf8(out.stdout).unwrap();
    assert!(
        listed.contains("a.log"),
        "the matched (ignored) file is listed: {listed}"
    );
    assert!(
        !listed.contains("note.txt"),
        "non-matching others omitted: {listed}"
    );
}

/// A directory exclude pattern (`-x dir/`, or the same via `-X`) excludes every
/// untracked file beneath the directory (gitignore parent-directory semantics),
/// not just an entry literally named `dir`.
#[test]
fn test_ls_files_exclude_directory_pattern() {
    let repo = tempdir().expect("repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("keep.rs"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "keep.rs"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    fs::create_dir_all(p.join("build")).unwrap();
    fs::write(p.join("build/a.o"), "o\n").unwrap();
    fs::write(p.join("build/b.o"), "o\n").unwrap();
    fs::write(p.join("top.txt"), "t\n").unwrap();

    // -x 'build/' drops everything under build/.
    let out = run_libra_command(&["ls-files", "-o", "-x", "build/"], p);
    assert_cli_success(&out, "ls-files -o -x build/");
    let listed = String::from_utf8(out.stdout).unwrap();
    assert!(
        !listed.contains("build/"),
        "files under build/ excluded: {listed}"
    );
    assert!(listed.contains("top.txt"), "sibling kept: {listed}");

    // -i -o -x 'build/' lists ONLY the files under build/.
    let out = run_libra_command(&["ls-files", "-i", "-o", "-x", "build/"], p);
    assert_cli_success(&out, "ls-files -i -o -x build/");
    let listed = String::from_utf8(out.stdout).unwrap();
    assert!(
        listed.contains("build/a.o") && listed.contains("build/b.o"),
        "build files listed: {listed}"
    );
    assert!(
        !listed.contains("top.txt"),
        "non-matching omitted: {listed}"
    );

    // Same directory pattern via -X file.
    fs::write(p.join("ex.txt"), "build/\n").unwrap();
    let out = run_libra_command(&["ls-files", "-o", "-X", "ex.txt"], p);
    assert_cli_success(&out, "ls-files -o -X (dir pattern)");
    let listed = String::from_utf8(out.stdout).unwrap();
    assert!(
        !listed.contains("build/a.o"),
        "dir pattern from file excludes subtree: {listed}"
    );
}

/// Git parent-directory dominance: once a directory is excluded, a later
/// whitelist for a child cannot re-include it. `-x build/ -x !build/keep.txt`
/// must NOT surface `build/keep.txt`.
#[test]
fn test_ls_files_exclude_parent_dominance() {
    let repo = tempdir().expect("repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("tracked.txt"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "tracked.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    fs::create_dir_all(p.join("build")).unwrap();
    fs::write(p.join("build/a.o"), "o\n").unwrap();
    fs::write(p.join("build/keep.txt"), "k\n").unwrap();
    fs::write(p.join("top.txt"), "t\n").unwrap();

    let out = run_libra_command(
        &["ls-files", "-o", "-x", "build/", "-x", "!build/keep.txt"],
        p,
    );
    assert_cli_success(&out, "ls-files -o -x build/ -x !build/keep.txt");
    let listed = String::from_utf8(out.stdout).unwrap();
    assert!(
        !listed.contains("build/"),
        "a child cannot be re-included once its parent dir is excluded: {listed}"
    );
    assert!(listed.contains("top.txt"), "unrelated other kept: {listed}");
}

/// Git source precedence: command-line `-x` patterns rank above `-X` files, so a
/// later inline `-x !pattern` re-includes a file excluded by an `-X` file.
#[test]
fn test_ls_files_exclude_inline_overrides_file() {
    let repo = tempdir().expect("repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("tracked.txt"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "tracked.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    fs::write(p.join("a.log"), "a\n").unwrap();
    fs::write(p.join("b.log"), "b\n").unwrap();
    fs::write(p.join("ex.txt"), "*.log\n").unwrap();

    // -X excludes all *.log; the higher-precedence inline -x re-includes a.log.
    let out = run_libra_command(&["ls-files", "-o", "-X", "ex.txt", "-x", "!a.log"], p);
    assert_cli_success(&out, "ls-files -o -X ex -x !a.log");
    let listed = String::from_utf8(out.stdout).unwrap();
    assert!(
        listed.contains("a.log"),
        "inline -x re-includes over -X file: {listed}"
    );
    assert!(
        !listed.contains("b.log"),
        "other *.log stays excluded: {listed}"
    );
}

/// Git cross-source precedence: command-line `-x` outranks `.libraignore`, so an
/// inline negation re-includes a file the standard excludes would drop.
#[test]
fn test_ls_files_exclude_inline_overrides_standard() {
    let repo = tempdir().expect("repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("tracked.txt"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "tracked.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    fs::write(p.join(".libraignore"), "*.log\n").unwrap();
    fs::write(p.join("a.log"), "a\n").unwrap();
    fs::write(p.join("b.log"), "b\n").unwrap();

    // .libraignore excludes *.log; the higher-precedence -x negation re-includes a.log.
    let out = run_libra_command(&["ls-files", "-o", "--exclude-standard", "-x", "!a.log"], p);
    assert_cli_success(&out, "ls-files -o --exclude-standard -x !a.log");
    let listed = String::from_utf8(out.stdout).unwrap();
    assert!(
        listed.contains("a.log"),
        "inline -x negation overrides .libraignore: {listed}"
    );
    assert!(
        !listed.contains("b.log"),
        "other *.log stays excluded by .libraignore: {listed}"
    );
}

#[test]
fn test_ls_files_eol_classifies_line_endings() {
    let repo = tempdir().expect("repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);

    fs::write(p.join("lf.txt"), b"a\nb\n").unwrap();
    fs::write(p.join("crlf.txt"), b"a\r\nb\r\n").unwrap();
    fs::write(p.join("mixed.txt"), b"a\nb\r\n").unwrap();
    fs::write(p.join("noeol.txt"), b"noeol").unwrap();
    fs::write(p.join("bin.bin"), b"\x00\x01\x02\n").unwrap();
    fs::write(p.join("lonecr.txt"), b"a\rb\r").unwrap(); // bare CR -> binary
    fs::write(p.join("ctrl.txt"), b"a\x01\x02\x03\x04\x05b").unwrap(); // control-heavy -> binary
    let files = [
        "lf.txt",
        "crlf.txt",
        "mixed.txt",
        "noeol.txt",
        "bin.bin",
        "lonecr.txt",
        "ctrl.txt",
    ];
    let mut add = vec!["add"];
    add.extend_from_slice(&files);
    assert_cli_success(&run_libra_command(&add, p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c", "--no-verify"], p),
        "commit",
    );

    let mut cmd = vec!["ls-files", "--eol"];
    cmd.extend_from_slice(&files);
    let out = run_libra_command(&cmd, p);
    assert_cli_success(&out, "ls-files --eol");
    let text = String::from_utf8_lossy(&out.stdout);

    // Byte-exact field layout matches `git ls-files --eol`
    // (`i/%-5s w/%-5s attr/%-17s\t<path>`), with empty attr. Binary detection
    // covers NUL, a bare CR, and a non-printable-heavy buffer (Git's heuristic).
    for (path, eol) in [
        ("lf.txt", "lf"),
        ("crlf.txt", "crlf"),
        ("mixed.txt", "mixed"),
        ("noeol.txt", "none"),
        ("bin.bin", "-text"),
        ("lonecr.txt", "-text"),
        ("ctrl.txt", "-text"),
    ] {
        let expected = format!("i/{eol:<5} w/{eol:<5} attr/{:<17}\t{path}", "");
        assert!(
            text.lines().any(|l| l == expected),
            "expected line {expected:?} in:\n{text}"
        );
    }

    // `--eol` composes with `-t` (tag prefix) and `-s` (mode/oid/stage prefix):
    // the eol column sits before the path, after the tag/stage prefix (Git).
    let t = run_libra_command(&["ls-files", "-t", "--eol", "lf.txt"], p);
    assert_cli_success(&t, "ls-files -t --eol");
    let expected_t = format!("H i/{e:<5} w/{e:<5} attr/{:<17}\tlf.txt", "", e = "lf");
    assert_eq!(
        String::from_utf8_lossy(&t.stdout).trim_end(),
        expected_t,
        "-t --eol composes the tag prefix before the eol column"
    );

    let s = run_libra_command(&["ls-files", "-s", "--eol", "lf.txt"], p);
    assert_cli_success(&s, "ls-files -s --eol");
    let s_out = String::from_utf8_lossy(&s.stdout);
    assert!(
        s_out.contains(&format!(
            "\ti/{e:<5} w/{e:<5} attr/{:<17}\tlf.txt",
            "",
            e = "lf"
        )),
        "-s --eol inserts the eol column after the stage record: {s_out}"
    );
}

#[test]
fn test_literal_pathspecs_disables_magic() {
    let repo = tempdir().expect("tempdir");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("x.txt"), "x\n").unwrap();
    fs::write(p.join("*.txt"), "star\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "x.txt", "*.txt"], p),
        "add both",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "files", "--no-verify"], p),
        "commit",
    );
    let out = run_libra_command(
        &["--literal-pathspecs", "ls-files", "--", ":(glob)*.txt"],
        p,
    );
    assert_cli_success(&out, "ls-files literal magic");
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        ":(glob)*.txt is a literal name: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// SW-07 (M-CLI C4, plan issues/490): a skip-worktree entry tags `S` under
/// `-t`/`-v`, is never reported deleted/modified, and is excluded from the
/// `-d`/`-m` listings (git's `if (ce_skip_worktree(ce)) continue;`).
#[test]
fn test_ls_files_tags_skip_worktree() {
    let repo = setup_ls_files_repo();
    let root = repo.path();

    super::mark_skip_worktree(root, "tracked.txt");
    assert!(
        super::skip_worktree_set(root, "tracked.txt"),
        "precondition: the bit must be set through the index API"
    );

    // `S` wins over the worktree-state tags: even with the file deleted or
    // modified the entry tags `S` and never lists under `-d`/`-m`.
    let deleted = fs::rename(root.join("tracked.txt"), root.join("tracked.bak"));
    assert!(deleted.is_ok(), "delete the worktree copy");
    let list = run_libra_command(&["ls-files", "-t"], root);
    assert_cli_success(&list, "-t on a marked entry");
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("S tracked.txt"),
        "deleted marked entry must tag S: {}",
        String::from_utf8_lossy(&list.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&list.stdout).contains("R tracked.txt"),
        "a marked entry must never tag R: {}",
        String::from_utf8_lossy(&list.stdout)
    );
    let deleted_list = run_libra_command(&["ls-files", "-d"], root);
    assert!(
        !String::from_utf8_lossy(&deleted_list.stdout).contains("tracked.txt"),
        "-d must not list marked entries: {}",
        String::from_utf8_lossy(&deleted_list.stdout)
    );

    // Restore the file, modify it: same `S` rule, `-m` stays silent for it.
    fs::rename(root.join("tracked.bak"), root.join("tracked.txt")).expect("restore");
    fs::write(root.join("tracked.txt"), "modified\n").expect("modify");
    let list = run_libra_command(&["ls-files", "-t"], root);
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("S tracked.txt"),
        "modified marked entry must tag S: {}",
        String::from_utf8_lossy(&list.stdout)
    );
    let modified_list = run_libra_command(&["ls-files", "-m"], root);
    assert!(
        !String::from_utf8_lossy(&modified_list.stdout).contains("tracked.txt"),
        "-m must not list marked entries: {}",
        String::from_utf8_lossy(&modified_list.stdout)
    );

    // Ordinary entries still tag `H`, and `-v` equals `-t` output (Libra has
    // no assume-unchanged lowercase spelling).
    let list = run_libra_command(&["ls-files", "-t"], root);
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("H tracked-dir/alpha.txt"),
        "plain entries tag H: {}",
        String::from_utf8_lossy(&list.stdout)
    );
    let t = run_libra_command(&["ls-files", "-t"], root);
    let v = run_libra_command(&["ls-files", "-v"], root);
    assert_eq!(
        String::from_utf8_lossy(&t.stdout),
        String::from_utf8_lossy(&v.stdout),
        "-v must match -t while assume-unchanged is unsupported"
    );

    // The JSON shape is unchanged (the extended flag is not serialized).
    let json = run_libra_command(&["--json", "ls-files"], root);
    assert_cli_success(&json, "json ls-files");
    let parsed = parse_json_stdout(&json);
    assert!(
        parsed["data"]
            .as_array()
            .expect("entries")
            .iter()
            .any(|e| e["path"] == "tracked.txt"),
        "json must still list the marked entry: {parsed}"
    );
}

/// WT-06 / issue #476 M-ITA-VIEW V7: `ls-files -s` shows the intent-to-add
/// entry as the canonical empty blob at stage 0.
#[test]
fn ls_files_short_shows_intent_to_add_empty_blob() {
    const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("tracked.txt"), "tracked\n").expect("write tracked");
    assert_cli_success(&run_libra_command(&["add", "tracked.txt"], root), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );
    fs::write(root.join("new.txt"), "new\n").expect("write new");
    assert_cli_success(
        &run_libra_command(&["add", "-N", "new.txt"], root),
        "add -N",
    );

    let output = run_libra_command(&["ls-files", "-s"], root);
    assert_cli_success(&output, "ls-files -s");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.lines()
            .any(|line| line == format!("100644 {EMPTY_BLOB} 0\tnew.txt")),
        "V7 expected empty-blob entry: {text:?}"
    );
}

/// Issue #490 / TC-0046 support surface: `ls-files --debug` prints Git's cached
/// stat block after each record, so a caller can prove that `add --refresh`
/// never rewrites a skip-worktree entry's cached stat.
#[test]
fn ls_files_debug_prints_cached_stat_block() {
    let repo = super::create_committed_repo_via_cli();
    let root = repo.path();

    let out = run_libra_command(&["ls-files", "--debug", "tracked.txt"], root);
    super::assert_cli_success(&out, "ls-files --debug");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("tracked.txt"),
        "path line first: {text}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start().starts_with("ctime: ")),
        "ctime line: {text}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start().starts_with("mtime: ")),
        "mtime line: {text}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start().starts_with("dev: ") && line.contains("\tino: ")),
        "dev/ino line: {text}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start().starts_with("uid: ") && line.contains("\tgid: ")),
        "uid/gid line: {text}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start().starts_with("size: ") && line.contains("\tflags: ")),
        "size/flags line: {text}"
    );
    // The stat pair is `<seconds>:<nanoseconds>` on both time lines.
    for key in ["ctime: ", "mtime: "] {
        let line = lines
            .iter()
            .find(|line| line.trim_start().starts_with(key))
            .expect("time line present");
        let value = line.trim_start().trim_start_matches(key).trim();
        assert_eq!(
            value.split(':').count(),
            2,
            "{key} must render seconds:nanos, got {value}"
        );
    }

    // `-s` composes: the stage record comes first, then the debug block.
    let staged = run_libra_command(&["ls-files", "-s", "--debug", "tracked.txt"], root);
    super::assert_cli_success(&staged, "ls-files -s --debug");
    let staged_text = String::from_utf8_lossy(&staged.stdout).to_string();
    assert!(
        staged_text.starts_with("100644 "),
        "the stage record keeps its shape: {staged_text}"
    );
    assert!(
        staged_text.contains("  mtime: "),
        "the debug block follows the stage record: {staged_text}"
    );

    // Untracked rows (`-o`) carry no cached stat, so they print only the path.
    fs::write(root.join("untracked.txt"), "u\n").expect("write untracked file");
    let others = run_libra_command(&["ls-files", "-o", "--debug", "untracked.txt"], root);
    super::assert_cli_success(&others, "ls-files -o --debug");
    assert_eq!(
        String::from_utf8_lossy(&others.stdout).trim(),
        "untracked.txt",
        "an untracked row has no cached stat block"
    );
}

/// Issue #490 / TC-0046: `add --refresh` must not rewrite a skip-worktree
/// entry's cached stat (Git never refreshes sparse entries).
#[test]
fn add_refresh_leaves_skip_worktree_stat_untouched() {
    let repo = super::create_committed_repo_via_cli();
    let root = repo.path();
    fs::write(root.join("sparse_entry"), "sparse\n").expect("write sparse_entry");
    super::assert_cli_success(
        &run_libra_command(&["add", "sparse_entry"], root),
        "stage sparse_entry",
    );
    super::assert_cli_success(
        &run_libra_command(&["update-index", "--skip-worktree", "sparse_entry"], root),
        "mark skip-worktree",
    );
    super::assert_cli_success(
        &run_libra_command(&["commit", "-m", "sparse", "--no-verify"], root),
        "commit sparse_entry",
    );
    fs::remove_file(root.join("sparse_entry")).expect("delete the worktree copy");

    let before = run_libra_command(&["ls-files", "--debug", "sparse_entry"], root);
    super::assert_cli_success(&before, "ls-files --debug before");
    let before_stat: Vec<String> = String::from_utf8_lossy(&before.stdout)
        .lines()
        .filter(|line| line.trim_start().starts_with("mtime: "))
        .map(str::to_string)
        .collect();

    let refresh = run_libra_command(&["add", "--refresh", "sparse_entry"], root);
    assert_eq!(
        refresh.status.code(),
        Some(1),
        "a sparse pathspec must be refused (t3705#10): {}",
        String::from_utf8_lossy(&refresh.stderr)
    );

    let after = run_libra_command(&["ls-files", "--debug", "sparse_entry"], root);
    super::assert_cli_success(&after, "ls-files --debug after");
    let after_stat: Vec<String> = String::from_utf8_lossy(&after.stdout)
        .lines()
        .filter(|line| line.trim_start().starts_with("mtime: "))
        .map(str::to_string)
        .collect();
    assert_eq!(
        before_stat, after_stat,
        "add --refresh must not update a skip-worktree entry's cached stat"
    );

    let staged = run_libra_command(&["diff", "--staged", "--", "sparse_entry"], root);
    assert!(
        String::from_utf8_lossy(&staged.stdout).trim().is_empty(),
        "the sparse entry must stay unstaged"
    );
}

/// PM-11: `-o` excludes every index path, including unmerged conflict stages.
#[test]
fn ls_files_others_excludes_unmerged_paths() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let p = repo.path();
    std::fs::write(p.join("f"), "base\n").unwrap();
    std::fs::write(p.join("g.log"), "base\n").unwrap();
    std::fs::write(p.join("k"), "keep\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "f", "g.log", "k"], p),
        "add base",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    assert_cli_success(&run_libra_command(&["branch", "side"], p), "branch side");
    assert_cli_success(&run_libra_command(&["switch", "side"], p), "switch side");
    std::fs::write(p.join("f"), "side\n").unwrap();
    std::fs::write(p.join("g.log"), "side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f", "g.log"], p), "add side");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "side", "--no-verify"], p),
        "commit side",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("f"), "main\n").unwrap();
    std::fs::write(p.join("g.log"), "main\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f", "g.log"], p), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main", "--no-verify"], p),
        "commit main",
    );
    // The merge fails with conflicts; the fixture then asserts both are unmerged.
    let _ = run_libra_command(&["merge", "side"], p);
    let unmerged = run_libra_command(&["ls-files", "-u"], p);
    assert_cli_success(&unmerged, "ls-files -u");
    let merged_paths: std::collections::BTreeSet<String> =
        String::from_utf8_lossy(&unmerged.stdout)
            .lines()
            .filter_map(|line| line.split('\t').nth(1).map(str::to_string))
            .collect();
    assert_eq!(
        merged_paths,
        ["f".to_string(), "g.log".to_string()]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        "fixture conflict stages"
    );

    std::fs::write(p.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(p.join("u.txt"), "u\n").unwrap();
    std::fs::write(p.join("ign.log"), "ign\n").unwrap();

    // AC1: untracked files include the conflict paths' neighbours only.
    let out = run_libra_command(&["ls-files", "-o"], p);
    assert_cli_success(&out, "AC1");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        ".gitignore\nign.log\nu.txt\n",
        "AC1"
    );

    // AC2: the ignored file is filtered by the standard excludes.
    let out = run_libra_command(&["ls-files", "-o", "--exclude-standard"], p);
    assert_cli_success(&out, "AC2");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        ".gitignore\nu.txt\n",
        "AC2"
    );

    // AC3: `-i -o` lists only the ignored file; the unmerged `g.log` is not untracked.
    let out = run_libra_command(&["ls-files", "-i", "-o", "--exclude-standard"], p);
    assert_cli_success(&out, "AC3");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ign.log\n", "AC3");

    // AC4: an add/add conflict in a clean tree lists nothing.
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let p = repo.path();
    std::fs::write(p.join("r"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "r"], p), "add r");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit r",
    );
    assert_cli_success(&run_libra_command(&["branch", "side"], p), "branch side");
    assert_cli_success(&run_libra_command(&["switch", "side"], p), "switch side");
    std::fs::write(p.join("b"), "side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b"], p), "add side b");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "side-b", "--no-verify"], p),
        "commit side b",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("b"), "main\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b"], p), "add main b");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-b", "--no-verify"], p),
        "commit main b",
    );
    let _ = run_libra_command(&["merge", "side"], p);
    let out = run_libra_command(&["ls-files", "-o"], p);
    assert_cli_success(&out, "AC4");
    assert!(
        out.stdout.is_empty(),
        "AC4: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
