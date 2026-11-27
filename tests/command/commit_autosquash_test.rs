//! Autosquash (`--fixup`/`--squash`), `--dry-run`/`--porcelain`, and `--verbose`
//! behavior for `libra commit`.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{fs, path::Path, process::Command};

use serial_test::serial;
use tempfile::tempdir;

fn run_libra(args: &[&str], cwd: &Path) -> std::process::Output {
    run_libra_env(args, cwd, &[])
}

fn run_libra_env(args: &[&str], cwd: &Path, env: &[(&str, &str)]) -> std::process::Output {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    fs::create_dir_all(&config_home).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .args(args)
        .current_dir(cwd)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env_remove("RUST_LOG")
        .env_remove("LIBRA_LOG")
        .env_remove("EDITOR")
        .env_remove("VISUAL")
        .env_remove("GIT_EDITOR");
    for (k, v) in env {
        command.env(k, v);
    }
    command.output().unwrap()
}

fn init_repo(repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    assert!(run_libra(&["init"], repo).status.success(), "init failed");
    assert!(
        run_libra(&["config", "user.name", "Test User"], repo)
            .status
            .success()
    );
    assert!(
        run_libra(&["config", "user.email", "test@example.com"], repo)
            .status
            .success()
    );
}

fn write_and_add(repo: &Path, name: &str, content: &str) {
    fs::write(repo.join(name), content).unwrap();
    assert!(run_libra(&["add", name], repo).status.success(), "add");
}

fn commit(repo: &Path, message: &str) {
    let out = run_libra(&["commit", "--no-verify", "-m", message], repo);
    assert_eq!(
        out.status.code(),
        Some(0),
        "commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn last_commit_message(repo: &Path) -> String {
    String::from_utf8_lossy(&run_libra(&["log", "-1"], repo).stdout).into_owned()
}

fn head_commit(repo: &Path) -> String {
    String::from_utf8_lossy(&run_libra(&["rev-parse", "HEAD"], repo).stdout)
        .trim()
        .to_string()
}

#[cfg(unix)]
fn write_editor_script(dir: &Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// Autosquash: --fixup / --squash
// ---------------------------------------------------------------------------

#[test]
fn fixup_generates_fixup_subject() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write_and_add(&repo, "a.txt", "x\n");
    commit(&repo, "base subject");

    write_and_add(&repo, "a.txt", "y\n");
    let out = run_libra(&["commit", "--fixup", "HEAD"], &repo);
    assert_eq!(
        out.status.code(),
        Some(0),
        "fixup commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        last_commit_message(&repo).contains("fixup! base subject"),
        "expected fixup! subject, got: {}",
        last_commit_message(&repo)
    );
}

#[test]
fn squash_generates_squash_subject() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write_and_add(&repo, "a.txt", "x\n");
    commit(&repo, "base subject");

    write_and_add(&repo, "a.txt", "y\n");
    // Libra derives the `squash!` subject and commits directly — unlike Git, it
    // does not seed an editor for `--squash`, so no message source is required.
    let out = run_libra(&["commit", "--squash", "HEAD"], &repo);
    assert_eq!(
        out.status.code(),
        Some(0),
        "squash commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        last_commit_message(&repo).contains("squash! base subject"),
        "expected squash! subject, got: {}",
        last_commit_message(&repo)
    );
}

#[test]
fn fixup_unknown_target_returns_repo_error() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write_and_add(&repo, "a.txt", "x\n");
    commit(&repo, "base");

    write_and_add(&repo, "a.txt", "y\n");
    // An unresolvable --fixup target fails when the parent commit cannot be
    // loaded (a repository error, exit 128), not as a CLI usage error.
    let out = run_libra(&["commit", "--fixup", "no-such-ref"], &repo);
    assert_eq!(
        out.status.code(),
        Some(128),
        "unknown --fixup target should fail to resolve (128): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no-such-ref"),
        "error should name the unresolvable target: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// Dry-run / porcelain
// ---------------------------------------------------------------------------

#[test]
fn dry_run_leaves_head_unchanged() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write_and_add(&repo, "a.txt", "x\n");
    commit(&repo, "base");

    write_and_add(&repo, "a.txt", "y\n");
    let before = head_commit(&repo);
    let out = run_libra(&["commit", "--dry-run", "-m", "preview"], &repo);
    assert_eq!(
        out.status.code(),
        Some(0),
        "dry-run with staged changes should exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(before, head_commit(&repo), "dry-run must not move HEAD");
}

#[test]
fn dry_run_exit_code_matches_committability() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write_and_add(&repo, "a.txt", "x\n");
    commit(&repo, "base");

    // Nothing staged → 128 (nothing to commit).
    let clean = run_libra(&["commit", "--dry-run", "-m", "preview"], &repo);
    assert_eq!(
        clean.status.code(),
        Some(128),
        "dry-run with nothing to commit should exit 128"
    );

    // Staged change → 0.
    write_and_add(&repo, "a.txt", "y\n");
    let dirty = run_libra(&["commit", "--dry-run", "-m", "preview"], &repo);
    assert_eq!(
        dirty.status.code(),
        Some(0),
        "dry-run with a staged change should exit 0"
    );
}

#[test]
fn dry_run_porcelain_matches_status_format() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write_and_add(&repo, "a.txt", "x\n");
    commit(&repo, "base");

    write_and_add(&repo, "a.txt", "y\n");
    let dry = run_libra(
        &["commit", "--dry-run", "--porcelain", "-m", "preview"],
        &repo,
    );
    assert_eq!(dry.status.code(), Some(0));
    let dry_out = String::from_utf8_lossy(&dry.stdout);
    let status = run_libra(&["status", "--porcelain"], &repo);
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert!(
        dry_out
            .lines()
            .any(|l| l.contains("a.txt") && l.starts_with('M')),
        "dry-run porcelain should report a modified a.txt: {dry_out}"
    );
    // The would-commit line should appear verbatim in `status --porcelain`.
    let dry_line = dry_out.lines().find(|l| l.contains("a.txt")).unwrap();
    assert!(
        status_out.lines().any(|l| l == dry_line),
        "dry-run porcelain line `{dry_line}` should match status --porcelain `{status_out}`"
    );
}

#[test]
fn dry_run_with_all_does_not_mutate_index() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write_and_add(&repo, "a.txt", "x\n");
    commit(&repo, "base");

    // Modify a tracked file WITHOUT staging it.
    fs::write(repo.join("a.txt"), "modified\n").unwrap();
    let index_path = repo.join(".libra").join("index");
    let before = fs::read(&index_path).unwrap();

    let out = run_libra(&["commit", "--dry-run", "-a", "-m", "preview"], &repo);
    assert_eq!(
        out.status.code(),
        Some(0),
        "dry-run -a with a tracked modification should exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = fs::read(&index_path).unwrap();
    assert_eq!(before, after, "dry-run -a must not persist the index");
}

#[test]
fn dry_run_all_excludes_untracked_from_would_commit() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write_and_add(&repo, "a.txt", "x\n");
    commit(&repo, "base");

    // An untracked file is advisory only; `-a` never stages it.
    fs::write(repo.join("untracked.txt"), "u\n").unwrap();
    let index_path = repo.join(".libra/index");
    let before_index = fs::read(&index_path).unwrap();
    let before_head = head_commit(&repo);
    let out = run_libra(&["commit", "--dry-run", "-a", "-m", "preview"], &repo);
    assert_eq!(
        out.status.code(),
        Some(128),
        "untracked-only with --dry-run -a should be nothing to commit (128): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The long status snapshot still reports advisory untracked files. The
    // porcelain preview distinguishes them from files auto-staged by `-a`.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Untracked files:"), "{stdout}");
    assert!(stdout.contains("untracked.txt"), "{stdout}");
    assert_eq!(fs::read(&index_path).unwrap(), before_index);
    assert_eq!(head_commit(&repo), before_head);

    fs::write(repo.join("a.txt"), "modified\n").unwrap();
    let preview = run_libra(&["commit", "--porcelain", "-a", "-m", "preview"], &repo);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let porcelain = String::from_utf8_lossy(&preview.stdout);
    assert!(
        porcelain.lines().any(|line| line == "M  a.txt"),
        "tracked modifications must be auto-staged in the preview: {porcelain}"
    );
    assert!(
        porcelain.lines().any(|line| line == "?? untracked.txt"),
        "untracked files must remain advisory in the preview: {porcelain}"
    );
    assert_eq!(fs::read(&index_path).unwrap(), before_index);
    assert_eq!(head_commit(&repo), before_head);
}

// ---------------------------------------------------------------------------
// Verbose
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn verbose_appends_diff_below_scissors_and_strips_it() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write_and_add(&repo, "a.txt", "first\n");
    commit(&repo, "base");

    write_and_add(&repo, "a.txt", "second\n");
    let capture = temp.path().join("captured.txt");
    // The editor records what it was shown, then writes a clean final message.
    let editor = write_editor_script(
        temp.path(),
        "ed.sh",
        &format!(
            "#!/bin/sh\ncp \"$1\" \"{}\"\nprintf 'verbose subject\\n' > \"$1\"\n",
            capture.display()
        ),
    );

    let out = run_libra_env(&["commit", "-v"], &repo, &[("EDITOR", &editor)]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "verbose commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let seen = fs::read_to_string(&capture).unwrap();
    assert!(
        seen.contains(">8"),
        "editor should see the scissors cut line, saw: {seen}"
    );
    assert!(
        seen.contains("second") || seen.contains("a.txt"),
        "editor should see the staged diff below the scissors line, saw: {seen}"
    );

    let committed = last_commit_message(&repo);
    assert!(committed.contains("verbose subject"));
    assert!(
        !committed.contains(">8"),
        "the scissors block must be stripped from the committed message: {committed}"
    );
}

#[cfg(unix)]
#[test]
fn commit_verbose_config_default_on() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    assert!(
        run_libra(&["config", "commit.verbose", "true"], &repo)
            .status
            .success()
    );
    write_and_add(&repo, "a.txt", "first\n");
    commit(&repo, "base");

    write_and_add(&repo, "a.txt", "second\n");
    let capture = temp.path().join("captured.txt");
    let editor = write_editor_script(
        temp.path(),
        "ed.sh",
        &format!(
            "#!/bin/sh\ncp \"$1\" \"{}\"\nprintf 'cfg verbose\\n' > \"$1\"\n",
            capture.display()
        ),
    );

    // No -v flag; commit.verbose=true should enable the diff block.
    let out = run_libra_env(&["commit"], &repo, &[("EDITOR", &editor)]);
    assert_eq!(out.status.code(), Some(0));
    let seen = fs::read_to_string(&capture).unwrap();
    assert!(
        seen.contains(">8"),
        "commit.verbose=true should append the scissors block, saw: {seen}"
    );
}

/// M-RI Q3 (HF-24): `commit --squash=<A>` then `rebase -i --autosquash`.
#[cfg(unix)]
#[test]
fn test_commit_squash_then_rebase_i_autosquash() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    write_and_add(&repo, "init.txt", "init\n");
    commit(&repo, "init");
    write_and_add(&repo, "a.txt", "A body\n");
    commit(&repo, "A");
    let a = head_commit(&repo);

    write_and_add(&repo, "a.txt", "A body\nsquash extra\n");
    let squash = run_libra(&["commit", "--squash", &a], &repo);
    assert_eq!(
        squash.status.code(),
        Some(0),
        "commit --squash: {}",
        String::from_utf8_lossy(&squash.stderr)
    );
    assert!(
        last_commit_message(&repo).contains("squash! A"),
        "expected squash! subject: {}",
        last_commit_message(&repo)
    );

    let captured = repo.join(".q3.todo");
    let editor = write_editor_script(
        &repo,
        "q3-capture.sh",
        &format!("#!/bin/sh\ncp \"$1\" '{}'\n", captured.display()),
    );
    let rebase = run_libra_env(
        &["rebase", "-i", "--autosquash", "HEAD~2"],
        &repo,
        &[("GIT_SEQUENCE_EDITOR", &editor)],
    );
    assert_eq!(
        rebase.status.code(),
        Some(0),
        "Q3 rebase -i --autosquash: {}",
        String::from_utf8_lossy(&rebase.stderr)
    );
    let todo = fs::read_to_string(&captured).expect("Q3 todo");
    assert!(
        todo.contains("squash ") && todo.contains(&a[..7.min(a.len())]),
        "Q3 todo must mark the squash commit: {todo}"
    );
    let message = last_commit_message(&repo);
    assert!(
        message.contains("A") && (message.contains("squash") || message.contains("A body")),
        "Q3 final message must keep A and the squash body: {message}"
    );
    let log_out = run_libra(&["log", "--oneline"], &repo);
    let log = String::from_utf8_lossy(&log_out.stdout);
    assert!(
        !log.contains("squash!"),
        "Q3 must fold the squash commit: {log}"
    );
}

#[test]
#[serial(cwd)]
fn commit_fixup_message_and_prefix_errors() {
    let repo = tempdir().unwrap();
    init_repo(repo.path());
    write_and_add(repo.path(), "a", "a\n");
    commit(repo.path(), "target subject");
    let target = head_commit(repo.path());
    write_and_add(repo.path(), "s", "s\n");

    for prefix in ["amend", "reword"] {
        let out = run_libra(
            &["commit", "-m", "x", &format!("--fixup={prefix}:{target}")],
            repo.path(),
        );
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!(
                "options '-m' and '--fixup:{prefix}' cannot be used together"
            )),
            "{stderr}"
        );
    }
    let unknown = run_libra(&["commit", &format!("--fixup=bogus:{target}")], repo.path());
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown option: --fixup=bogus:"));
}
