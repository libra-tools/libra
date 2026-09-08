//! Editor, cleanup-mode, and template behavior for `libra commit` (Batch 0).
//!
//! **Layer:** L1 — deterministic, no external dependencies.
//!
//! Editor launch is exercised with a tiny script editor (no TTY needed for an
//! *explicitly configured* editor); the `vi` fallback path is exercised by
//! clearing all editor env vars in a non-TTY subprocess.

use std::{fs, path::Path, process::Command};

use tempfile::tempdir;

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
        .env("LIBRA_CONFIG_GLOBAL_DB", home.join(".libra/config.db"))
        .env_remove("RUST_LOG")
        .env_remove("LIBRA_LOG")
        .env_remove("GIT_EDITOR")
        .env_remove("VISUAL")
        .env_remove("EDITOR");
    for (k, v) in env {
        command.env(k, v);
    }
    command.output().unwrap()
}

fn run_libra(args: &[&str], cwd: &Path) -> std::process::Output {
    run_libra_env(args, cwd, &[])
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

fn stage_file(repo: &Path, name: &str, content: &str) {
    fs::write(repo.join(name), content).unwrap();
    assert!(
        run_libra(&["add", name], repo).status.success(),
        "add failed"
    );
}

/// Write a `#!/bin/sh` editor script that writes `body` to its last argument
/// (the COMMIT_EDITMSG path), make it executable, and return its absolute path.
#[cfg(unix)]
fn write_editor_script(dir: &Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\nprintf '%s' '{body}' > \"$1\"\n")).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path.to_string_lossy().into_owned()
}

fn last_commit_message(repo: &Path) -> String {
    let out = run_libra(&["log", "-1"], repo);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[cfg(unix)]
#[test]
fn editor_launched_when_no_message_and_writes_message() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");
    let editor = write_editor_script(temp.path(), "ed.sh", "editor subject\\n");

    let out = run_libra_env(&["commit"], &repo, &[("EDITOR", &editor)]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "editor commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        last_commit_message(&repo).contains("editor subject"),
        "commit should use the editor-written message"
    );
}

#[cfg(unix)]
#[test]
fn editor_priority_visual_over_editor() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");
    let visual = write_editor_script(temp.path(), "visual.sh", "from-visual\\n");
    let editor = write_editor_script(temp.path(), "editor.sh", "from-editor\\n");

    let out = run_libra_env(
        &["commit"],
        &repo,
        &[("VISUAL", &visual), ("EDITOR", &editor)],
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(
        last_commit_message(&repo).contains("from-visual"),
        "VISUAL should take precedence over EDITOR"
    );
}

#[cfg(unix)]
#[test]
fn edit_flag_uses_message_as_initial_then_edits() {
    // --edit launches the editor even with -m; here the script overwrites it.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");
    let editor = write_editor_script(temp.path(), "ed.sh", "edited-final\\n");

    let out = run_libra_env(
        &["commit", "-e", "-m", "initial"],
        &repo,
        &[("EDITOR", &editor)],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "edit commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(last_commit_message(&repo).contains("edited-final"));
}

#[cfg(unix)]
#[test]
fn editor_nonzero_exit_aborts_with_128() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");

    // `false` exits non-zero without writing the file.
    let out = run_libra_env(&["commit"], &repo, &[("EDITOR", "false")]);
    assert_eq!(
        out.status.code(),
        Some(128),
        "editor failure must abort with 128: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn no_edit_coexists_with_message_flag() {
    // --no-edit is now allowed outside --amend and may carry -m.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");

    let out = run_libra(&["commit", "--no-edit", "-m", "msg via no-edit"], &repo);
    assert_eq!(
        out.status.code(),
        Some(0),
        "--no-edit with -m must commit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(last_commit_message(&repo).contains("msg via no-edit"));
}

#[test]
fn bare_no_edit_without_message_source_errors_128() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");

    let out = run_libra(&["commit", "--no-edit"], &repo);
    assert_eq!(
        out.status.code(),
        Some(128),
        "bare --no-edit with no message must error 128: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn non_tty_without_editor_errors_no_hang() {
    // No editor env (cleared) + non-TTY subprocess → must error, not hang.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");

    let out = run_libra(&["commit"], &repo);
    assert_ne!(
        out.status.code(),
        Some(0),
        "must not succeed without a message"
    );
}

#[test]
fn edit_conflicts_with_no_edit_exits_129() {
    // Libra maps clap parse errors to 129 (classify_parse_error), not clap's
    // native 2.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");

    let out = run_libra(&["commit", "-e", "--no-edit", "-m", "x"], &repo);
    assert_eq!(
        out.status.code(),
        Some(129),
        "--edit conflicts with --no-edit (clap parse error → 129 in Libra)"
    );
}

#[test]
fn invalid_cleanup_mode_exits_129() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");

    let out = run_libra(&["commit", "--cleanup=bogus", "-m", "x"], &repo);
    assert_eq!(
        out.status.code(),
        Some(129),
        "invalid --cleanup mode → exit 129 (Libra maps clap errors to 129)"
    );
}

#[test]
fn cleanup_verbatim_keeps_comment_lines() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");

    let out = run_libra(
        &["commit", "--cleanup=verbatim", "-m", "#issue-1\nkeep me"],
        &repo,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "verbatim commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        last_commit_message(&repo).contains("#issue-1"),
        "--cleanup=verbatim must keep the # line"
    );
}

#[test]
fn cleanup_strip_drops_comment_lines() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");

    let out = run_libra(
        &["commit", "--cleanup=strip", "-m", "subject\n# a comment"],
        &repo,
    );
    assert_eq!(out.status.code(), Some(0));
    let msg = last_commit_message(&repo);
    assert!(msg.contains("subject") && !msg.contains("# a comment"));
}

#[test]
fn cleanup_config_default_strips_comment_lines() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    assert!(
        run_libra(&["config", "commit.cleanup", "strip"], &repo)
            .status
            .success()
    );
    stage_file(&repo, "a.txt", "x\n");

    let out = run_libra(&["commit", "-m", "subject\n# configured comment"], &repo);
    assert_eq!(
        out.status.code(),
        Some(0),
        "configured cleanup commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let msg = last_commit_message(&repo);
    assert!(
        msg.contains("subject") && !msg.contains("# configured comment"),
        "commit.cleanup=strip should drop comments by default: {msg}"
    );
}

#[test]
fn cleanup_flag_overrides_config() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    assert!(
        run_libra(&["config", "commit.cleanup", "strip"], &repo)
            .status
            .success()
    );
    stage_file(&repo, "a.txt", "x\n");

    let out = run_libra(
        &[
            "commit",
            "--cleanup=verbatim",
            "-m",
            "subject\n# explicit comment",
        ],
        &repo,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "explicit cleanup commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        last_commit_message(&repo).contains("# explicit comment"),
        "--cleanup=verbatim should override commit.cleanup=strip"
    );
}

#[test]
fn template_t_flag_loads_initial_content() {
    // -t supplies the initial message; with --no-edit it is used directly.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");
    let tpl = temp.path().join("tpl.txt");
    fs::write(&tpl, "templated subject\n").unwrap();

    let out = run_libra(&["commit", "--no-edit", "-t", tpl.to_str().unwrap()], &repo);
    assert_eq!(
        out.status.code(),
        Some(0),
        "template commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(last_commit_message(&repo).contains("templated subject"));
}

#[cfg(unix)]
#[test]
fn template_seeds_editor_and_edited_message_is_committed() {
    // -t seeds the editor buffer; an editor that rewrites the message commits
    // the edited content (and does NOT trigger the unedited-template abort).
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");
    let tpl = temp.path().join("tpl.txt");
    fs::write(&tpl, "templated subject\n").unwrap();
    let editor = write_editor_script(temp.path(), "ed.sh", "brand new message\\n");

    let out = run_libra_env(
        &["commit", "-t", tpl.to_str().unwrap()],
        &repo,
        &[("EDITOR", &editor)],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "templated editor commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let message = last_commit_message(&repo);
    assert!(
        message.contains("brand new message"),
        "the edited message should be committed: {message}"
    );
    assert!(
        !message.contains("templated subject"),
        "the template was replaced by the edit: {message}"
    );
}

#[cfg(unix)]
#[test]
fn template_overrides_amend_parent_message_with_no_edit() {
    // `--amend --no-edit -t` uses the template directly, overriding the amend
    // parent message (matching Git's `-t` precedence over the reused message).
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");
    let first = run_libra(&["commit", "-m", "original parent", "--no-verify"], &repo);
    assert_eq!(first.status.code(), Some(0));
    let tpl = temp.path().join("tpl.txt");
    fs::write(&tpl, "templated amend message\n").unwrap();

    stage_file(&repo, "a.txt", "y\n");
    let out = run_libra(
        &[
            "commit",
            "--amend",
            "--no-edit",
            "-t",
            tpl.to_str().unwrap(),
            "--no-verify",
        ],
        &repo,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "amend with template failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let message = last_commit_message(&repo);
    assert!(
        message.contains("templated amend message"),
        "the template overrides the amend parent message: {message}"
    );
    assert!(
        !message.contains("original parent"),
        "the parent message was replaced by the template: {message}"
    );
}

#[test]
fn amend_no_edit_ignores_bad_commit_template_config() {
    // A bad `commit.template` config must NOT break a bare `--amend --no-edit`
    // (the config template seeds new commits, not the amend parent reuse), so
    // it is never read on this path.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    assert!(
        run_libra(
            &["config", "commit.template", "/nonexistent/template/path"],
            &repo,
        )
        .status
        .success()
    );
    stage_file(&repo, "a.txt", "x\n");
    assert_eq!(
        run_libra(&["commit", "-m", "amend parent", "--no-verify"], &repo)
            .status
            .code(),
        Some(0)
    );

    stage_file(&repo, "a.txt", "y\n");
    let out = run_libra(&["commit", "--amend", "--no-edit", "--no-verify"], &repo);
    assert_eq!(
        out.status.code(),
        Some(0),
        "bare --amend --no-edit must not read/fail on commit.template: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("commit template"),
        "the bad commit.template config must not be read on amend reuse: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        last_commit_message(&repo).contains("amend parent"),
        "the amend reuses the parent message: {}",
        last_commit_message(&repo)
    );
}

#[cfg(unix)]
#[test]
fn template_requires_editing_when_no_editor_available() {
    // `-t` (without --no-edit) needs the editor; with no editor configured and a
    // non-interactive stdin the template is never edited, so the commit aborts
    // rather than silently committing the unedited template.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");
    let tpl = temp.path().join("tpl.txt");
    fs::write(&tpl, "templated subject\n").unwrap();

    // run_libra strips GIT_EDITOR/EDITOR; with no editor env and a piped stdin,
    // no editor can open.
    let out = run_libra(
        &["commit", "-t", tpl.to_str().unwrap(), "--no-verify"],
        &repo,
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "an unedited template with no editor must abort"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("did not edit the message"),
        "abort names the unedited template: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
#[test]
fn template_left_unedited_aborts() {
    // -t seeds the editor; leaving the buffer unchanged (a no-op `true` editor)
    // aborts with Git's "you did not edit the message" message.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");
    let tpl = temp.path().join("tpl.txt");
    fs::write(&tpl, "templated subject\n").unwrap();

    let out = run_libra_env(
        &["commit", "-t", tpl.to_str().unwrap()],
        &repo,
        &[("EDITOR", "true")],
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "an unedited template must abort the commit"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("did not edit the message"),
        "abort names the unedited template: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `commit -v`: the editor template must contain the scissors marker and the
/// staged diff, but the committed message must contain neither.
#[cfg(unix)]
#[test]
fn verbose_template_includes_diff_but_message_excludes_it() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    // Establish HEAD so the staged diff has a base to compare against.
    stage_file(&repo, "base.txt", "base\n");
    assert!(
        run_libra(&["commit", "-m", "base"], &repo).status.success(),
        "base commit failed"
    );
    stage_file(&repo, "a.txt", "hello world\n");
    assert!(
        run_libra(&["config", "diff.srcPrefix", "COMMIT-OLD/"], &repo)
            .status
            .success(),
        "setting diff.srcPrefix failed"
    );
    assert!(
        run_libra(&["config", "diff.dstPrefix", "COMMIT-NEW/"], &repo)
            .status
            .success(),
        "setting diff.dstPrefix failed"
    );

    // Git's commit-verbose helper always uses the built-in staged diff. A
    // configured external driver must neither replace nor reformat the template.
    let external_diff = temp.path().join("external_diff.sh");
    fs::write(
        &external_diff,
        "#!/bin/sh\nprintf 'EXTDIFF-SENTINEL\\n\\n'\n",
    )
    .unwrap();
    let mut external_perms = fs::metadata(&external_diff).unwrap().permissions();
    external_perms.set_mode(0o755);
    fs::set_permissions(&external_diff, external_perms).unwrap();
    assert!(
        run_libra(
            &["config", "diff.external", external_diff.to_str().unwrap()],
            &repo,
        )
        .status
        .success(),
        "setting diff.external failed"
    );

    // Editor: capture the template it received, then write the final message.
    let capture = temp.path().join("captured.txt");
    let editor = temp.path().join("capture_ed.sh");
    fs::write(
        &editor,
        format!(
            "#!/bin/sh\ncp \"$1\" '{}'\nprintf 'verbose subject' > \"$1\"\n",
            capture.display()
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&editor).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&editor, perms).unwrap();

    let out = run_libra_env(
        &["commit", "-v"],
        &repo,
        &[("EDITOR", editor.to_str().unwrap())],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "commit -v failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let template = fs::read_to_string(&capture).unwrap();
    assert!(
        template.contains("------------------------ >8"),
        "template must contain the scissors marker: {template}"
    );
    assert!(
        template.contains("hello world"),
        "template must contain the staged diff: {template}"
    );
    assert!(
        template.contains("diff --git COMMIT-OLD/a.txt COMMIT-NEW/a.txt"),
        "verbose staged diff must honor configured prefixes: {template}"
    );
    assert!(
        !template.contains("EXTDIFF-SENTINEL"),
        "commit -v must ignore diff.external: {template}"
    );

    let msg = last_commit_message(&repo);
    assert!(
        msg.contains("verbose subject"),
        "message must be the edited subject: {msg}"
    );
    assert!(
        !msg.contains(">8"),
        "scissors marker must not enter the message: {msg}"
    );
    assert!(
        !msg.contains("hello world"),
        "staged diff must not enter the message: {msg}"
    );
}

#[test]
fn verbose_without_editor_propagates_invalid_diff_config_before_commit() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "base.txt", "base\n");
    assert!(run_libra(&["commit", "-m", "base"], &repo).status.success());
    stage_file(&repo, "next.txt", "next\n");
    assert!(
        run_libra(&["config", "diff.noPrefix", "sideways"], &repo)
            .status
            .success()
    );

    let out = run_libra(
        &[
            "commit",
            "-v",
            "--no-status",
            "-m",
            "must not commit",
            "--no-gpg-sign",
        ],
        &repo,
    );
    assert_eq!(
        out.status.code(),
        Some(129),
        "invalid diff config must abort no-editor commit -v: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    assert!(stderr.contains("diff.noPrefix"), "{stderr}");
    assert!(
        last_commit_message(&repo).contains("base"),
        "HEAD must remain on the base commit"
    );
}

#[test]
fn verbose_without_editor_preserves_diff_config_read_error_before_commit() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "base.txt", "base\n");
    assert!(run_libra(&["commit", "-m", "base"], &repo).status.success());
    stage_file(&repo, "next.txt", "next\n");
    for (key, value) in [
        ("diff.context", "3"),
        ("diff.renames", "true"),
        ("diff.noPrefix", "false"),
        ("diff.mnemonicPrefix", "false"),
    ] {
        assert!(run_libra(&["config", key, value], &repo).status.success());
    }
    fs::create_dir_all(
        repo.join(".libra-test-home")
            .join(".libra")
            .join("config.db"),
    )
    .unwrap();

    let out = run_libra(
        &[
            "commit",
            "-v",
            "--no-status",
            "-m",
            "must not commit",
            "--no-gpg-sign",
        ],
        &repo,
    );
    assert_eq!(out.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("diff.srcPrefix"), "{stderr}");
    assert!(!stderr.contains("LBR-REPO-002"), "{stderr}");
    fs::remove_dir(
        repo.join(".libra-test-home")
            .join(".libra")
            .join("config.db"),
    )
    .unwrap();
    assert!(
        last_commit_message(&repo).contains("base"),
        "HEAD must remain on the base commit"
    );
}

/// `-v` only truncates the appended diff; the selected `--cleanup` mode still
/// governs the message. With `--cleanup=verbatim -v`, a `#` comment line above the
/// scissors marker is preserved (verbatim does not strip comments), while the diff
/// below the marker is still dropped.
#[cfg(unix)]
#[test]
fn verbose_with_verbatim_cleanup_keeps_comment_lines() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "base.txt", "base\n");
    assert!(run_libra(&["commit", "-m", "base"], &repo).status.success());
    stage_file(&repo, "a.txt", "hello\n");

    // Editor writes a message with a `#` comment, then a scissors marker and a diff
    // below it (mimicking the `-v` template the user edited).
    let editor = temp.path().join("ed.sh");
    fs::write(
        &editor,
        "#!/bin/sh\nprintf 'subject line\\n# keep this comment\\n# ------------------------ >8 ------------------------\\nDIFFBODYLINE\\n' > \"$1\"\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&editor).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&editor, perms).unwrap();

    let out = run_libra_env(
        &["commit", "-v", "--cleanup=verbatim"],
        &repo,
        &[("EDITOR", editor.to_str().unwrap())],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "commit -v --cleanup=verbatim failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let msg = last_commit_message(&repo);
    assert!(
        msg.contains("# keep this comment"),
        "verbatim keeps `#` comment lines above the scissors even with -v: {msg}"
    );
    assert!(
        !msg.contains("DIFFBODYLINE"),
        "the diff below the scissors marker is still truncated: {msg}"
    );
}

/// Regression: `commit -v --cleanup=verbatim` must not commit Libra's own
/// verbose-template helper comments (`# Please enter ...`). The template omits
/// those `#` helper lines under a non-comment-stripping cleanup, so even an editor
/// that leaves the template intact (here it only prepends a subject) cannot leak
/// them into the message.
#[cfg(unix)]
#[test]
fn verbose_verbatim_does_not_commit_template_helper_comments() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "base.txt", "base\n");
    assert!(run_libra(&["commit", "-m", "base"], &repo).status.success());
    stage_file(&repo, "a.txt", "hello\n");

    // Editor prepends a subject and keeps the rest of the template verbatim.
    let editor = temp.path().join("ed.sh");
    fs::write(
        &editor,
        "#!/bin/sh\nprintf 'subject line\\n' > \"$1.new\"\ncat \"$1\" >> \"$1.new\"\nmv \"$1.new\" \"$1\"\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&editor).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&editor, perms).unwrap();

    let out = run_libra_env(
        &["commit", "-v", "--cleanup=verbatim"],
        &repo,
        &[("EDITOR", editor.to_str().unwrap())],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let msg = last_commit_message(&repo);
    assert!(
        msg.contains("subject line"),
        "the edited subject is committed: {msg}"
    );
    assert!(
        !msg.contains("Please enter") && !msg.contains(">8") && !msg.contains("hello"),
        "no template helper comments / scissors / diff leak into the message: {msg}"
    );
}

/// Write an editor script that first copies the template it is handed (the
/// COMMIT_EDITMSG path, `$1`) to `capture`, then overwrites it with `body` so
/// the commit succeeds. Lets a test inspect what was seeded into the template.
#[cfg(unix)]
fn write_capturing_editor_script(dir: &Path, name: &str, capture: &Path, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(
        &path,
        format!(
            "#!/bin/sh\ncp \"$1\" '{}'\nprintf '%s' '{body}' > \"$1\"\n",
            capture.display()
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path.to_string_lossy().into_owned()
}

#[cfg(unix)]
#[test]
fn status_flag_seeds_commented_status_into_template_and_strips_it() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "tracked.txt", "x\n");

    // `--status`: the editor template carries a commented status section.
    let capture = temp.path().join("with-status.txt");
    let editor = write_capturing_editor_script(temp.path(), "ed.sh", &capture, "status subject\\n");
    let out = run_libra_env(&["commit", "--status"], &repo, &[("EDITOR", &editor)]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "commit --status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let template = fs::read_to_string(&capture).unwrap();
    assert!(
        template
            .lines()
            .any(|l| l.starts_with('#') && l.contains("tracked.txt")),
        "template must include a commented status line naming the staged file:\n{template}"
    );
    // The commented status is stripped from the final commit message.
    let msg = last_commit_message(&repo);
    assert!(
        msg.contains("status subject"),
        "commit uses the editor message"
    );
    assert!(
        !msg.contains("Changes to be committed"),
        "status section must not leak into the final message:\n{msg}"
    );
}

#[cfg(unix)]
#[test]
fn default_includes_status_and_no_status_omits_it() {
    // Git and Libra default to including status; explicit `--no-status` omits it.
    for (flags, includes_status) in [
        (vec!["commit"], true),
        (vec!["commit", "--no-status"], false),
    ] {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_repo(&repo);
        stage_file(&repo, "tracked.txt", "x\n");
        let capture = temp.path().join("no-status.txt");
        let editor =
            write_capturing_editor_script(temp.path(), "ed.sh", &capture, "plain subject\\n");
        let out = run_libra_env(&flags, &repo, &[("EDITOR", &editor)]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{flags:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let template = fs::read_to_string(&capture).unwrap();
        assert_eq!(
            template.contains("tracked.txt"),
            includes_status,
            "{flags:?}: unexpected status-template behavior:\n{template}"
        );
    }
}

#[cfg(unix)]
#[test]
fn status_not_seeded_under_non_comment_stripping_cleanup() {
    // Under `--cleanup=verbatim`/`whitespace`/`scissors` the `#` comment lines
    // above the message are NOT stripped (explicit scissors is whitespace cleanup
    // plus truncation), so `--status` must NOT seed the status block (it would leak
    // into the message). The status section is omitted for those modes.
    for mode in ["verbatim", "whitespace", "scissors"] {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_repo(&repo);
        stage_file(&repo, "tracked.txt", "x\n");
        let capture = temp.path().join(format!("tpl-{mode}.txt"));
        let editor =
            write_capturing_editor_script(temp.path(), "ed.sh", &capture, "verbatim subject\\n");
        let cleanup = format!("--cleanup={mode}");
        let out = run_libra_env(
            &["commit", "--status", &cleanup],
            &repo,
            &[("EDITOR", &editor)],
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "commit --status --cleanup={mode} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let template = fs::read_to_string(&capture).unwrap();
        assert!(
            !template.contains("tracked.txt") && !template.contains("Changes to be committed"),
            "--cleanup={mode}: status must NOT be seeded (comments are not stripped):\n{template}"
        );
        // And nothing status-like leaked into the final message.
        assert!(
            !last_commit_message(&repo).contains("Changes to be committed"),
            "--cleanup={mode}: no status in the final message"
        );
    }
}

#[cfg(unix)]
#[test]
fn commit_template_status_section_stays_long_with_status_short_config() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    stage_file(&repo, "a.txt", "x\n");
    let config = run_libra(&["config", "status.short", "true"], &repo);
    assert!(config.status.success());

    // Editor script that captures the template before writing the message.
    let capture = temp.path().join("template.txt");
    let editor_path = temp.path().join("capture-ed.sh");
    fs::write(
        &editor_path,
        format!(
            "#!/bin/sh\ncat \"$1\" > '{}'\nprintf 'template msg\\n' > \"$1\"\n",
            capture.display()
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&editor_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&editor_path, perms).unwrap();
    let editor = editor_path.to_string_lossy().into_owned();

    let out = run_libra_env(&["commit", "--status"], &repo, &[("EDITOR", &editor)]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "commit --status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let template = fs::read_to_string(&capture).unwrap();
    assert!(
        template.contains("Changes to be committed"),
        "the template's status section must stay in the long format even with \
         status.short=true (Git behavior), got:\n{template}"
    );
}
