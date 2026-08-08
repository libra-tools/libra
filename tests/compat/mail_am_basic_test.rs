//! Minimal mail-patch sequencer contracts for plan-20260708 P2-01.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use chrono::DateTime;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

struct CliFixture {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
}

impl CliFixture {
    fn new() -> Self {
        let temp = tempdir().expect("create tempdir");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        let repo = root.join("repo");
        fs::create_dir_all(&home).expect("create isolated home");
        Self {
            _temp: temp,
            root,
            home,
            repo,
        }
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let config_home = self.home.join(".config");
        let global_db = self.home.join(".libra").join("config.db");
        fs::create_dir_all(&config_home).expect("create isolated config dir");
        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("LIBRA_CONFIG_GLOBAL_DB", &global_db)
            .env("LIBRA_TEST", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C");
        if let Some(profile_file) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile_file);
        }
        command
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd, args).output().expect("spawn libra")
    }

    fn run_env(&self, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut command = self.command(cwd, args);
        for (key, value) in envs {
            command.env(key, value);
        }
        command.output().expect("spawn libra with env")
    }

    fn success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.run(cwd, args);
        assert_success(args, &output);
        output
    }

    fn success_env(&self, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let output = self.run_env(cwd, args, envs);
        assert_success(args, &output);
        output
    }

    fn init_repo(&self) {
        self.init_repo_with_format(None);
    }

    fn init_repo_with_format(&self, object_format: Option<&str>) {
        fs::create_dir_all(&self.repo).expect("create repo dir");
        let mut args = vec!["init", "--vault", "false"];
        if let Some(object_format) = object_format {
            args.extend(["--object-format", object_format]);
        }
        args.push(self.repo.to_str().expect("utf8 repo"));
        self.success(&self.root, &args);
        self.success(&self.repo, &["config", "set", "user.name", "Am Tester"]);
        self.success(
            &self.repo,
            &["config", "set", "user.email", "am-tester@example.com"],
        );
    }

    fn commit_file(&self, path: &str, contents: &str, message: &str) -> String {
        fs::write(self.repo.join(path), contents).expect("write fixture file");
        self.success(&self.repo, &["add", path]);
        self.success(&self.repo, &["commit", "-m", message]);
        self.rev_parse("HEAD")
    }

    fn rev_parse(&self, spec: &str) -> String {
        stdout_trim(&self.success(&self.repo, &["rev-parse", spec]))
    }

    fn format_series(&self, base: &str) -> Vec<PathBuf> {
        let out = self.root.join("patches");
        self.success(
            &self.repo,
            &[
                "format-patch",
                "-o",
                out.to_str().expect("utf8 output dir"),
                &format!("{base}..HEAD"),
            ],
        );
        let mut patches: Vec<PathBuf> = fs::read_dir(out)
            .expect("read patch dir")
            .map(|entry| entry.expect("patch entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "patch"))
            .collect();
        patches.sort();
        patches
    }

    fn am(&self, patches: &[PathBuf]) -> Output {
        let mut args = vec!["am"];
        let names: Vec<&str> = patches
            .iter()
            .map(|path| path.to_str().expect("utf8 patch path"))
            .collect();
        args.extend(names);
        self.run(&self.repo, &args)
    }
}

fn assert_success(args: &[&str], output: &Output) {
    assert!(
        output.status.success(),
        "{} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure(output: &Output, needle: &str) {
    assert!(!output.status.success(), "command unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(needle),
        "stderr must contain {needle:?}:\n{stderr}"
    );
}

fn stdout_trim(output: &Output) -> String {
    String::from_utf8(output.stdout.clone())
        .expect("stdout is utf8")
        .trim()
        .to_string()
}

fn setup_single_patch() -> (CliFixture, String, PathBuf) {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let base = fixture.commit_file("file.txt", "base\n", "base");
    fs::write(fixture.repo.join("file.txt"), "from mail\n").expect("write mail change");
    fixture.success(&fixture.repo, &["add", "file.txt"]);
    fixture.success_env(
        &fixture.repo,
        &["commit", "-m", "mail change\n\nMail body."],
        &[
            ("GIT_AUTHOR_NAME", "Mail Author"),
            ("GIT_AUTHOR_EMAIL", "mail-author@example.com"),
            ("GIT_AUTHOR_DATE", "1700000000 +0530"),
        ],
    );
    let patches = fixture.format_series(&base);
    assert_eq!(patches.len(), 1);
    (fixture, base, patches[0].clone())
}

fn expected_author_line(patch: &Path) -> String {
    let mail = fs::read_to_string(patch).expect("read patch mail");
    let from = mail
        .lines()
        .find_map(|line| line.strip_prefix("From: "))
        .expect("From header");
    let date = mail
        .lines()
        .find_map(|line| line.strip_prefix("Date: "))
        .expect("Date header");
    let parsed = DateTime::parse_from_rfc2822(date).expect("RFC 2822 Date header");
    let seconds = parsed.offset().local_minus_utc();
    let sign = if seconds < 0 { '-' } else { '+' };
    let absolute = seconds.unsigned_abs();
    format!(
        "author {from} {} {sign}{:02}{:02}",
        parsed.timestamp(),
        absolute / 3600,
        (absolute % 3600) / 60
    )
}

#[test]
fn applies_format_patch_and_preserves_message_author_and_date() {
    let (fixture, base, patch) = setup_single_patch();
    let expected_author = expected_author_line(&patch);
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);

    let output = fixture.am(&[patch]);
    assert_success(&["am"], &output);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("read applied file"),
        "from mail\n"
    );
    let raw = String::from_utf8(
        fixture
            .success(&fixture.repo, &["cat-file", "-p", "HEAD"])
            .stdout,
    )
    .expect("commit is utf8");
    assert!(
        raw.contains(&expected_author),
        "author metadata was not preserved:\nexpected: {expected_author}\nactual:\n{raw}"
    );
    assert!(raw.ends_with("\n\nmail change\n\nMail body.\n"), "{raw}");
    assert_failure(
        &fixture.run(&fixture.repo, &["am", "--abort"]),
        "no am operation",
    );
}

#[test]
fn conflict_then_continue_commits_only_staged_patch_paths() {
    let (fixture, base, patch) = setup_single_patch();
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);
    let local = fixture.commit_file("file.txt", "local\n", "local divergence");

    let conflict = fixture.am(&[patch]);
    assert_failure(&conflict, "patch failed");
    assert_eq!(fixture.rev_parse("HEAD"), local);
    let status = fixture.success(&fixture.repo, &["status"]);
    assert!(String::from_utf8_lossy(&status.stdout).contains("middle of an am operation"));

    fs::write(fixture.repo.join("file.txt"), "resolved\n").expect("write resolution");
    fixture.success(&fixture.repo, &["add", "file.txt"]);
    let continued = fixture.success(&fixture.repo, &["am", "--continue"]);
    assert!(String::from_utf8_lossy(&continued.stdout).contains("Applying: mail change"));
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("read resolution"),
        "resolved\n"
    );
    assert_eq!(fixture.rev_parse("HEAD^"), local);
}

#[test]
fn abort_restores_original_tip_index_and_worktree() {
    let (fixture, base, patch) = setup_single_patch();
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);
    let local = fixture.commit_file("file.txt", "local\n", "local divergence");
    let before = stdout_trim(&fixture.success(&fixture.repo, &["status", "--short"]));
    assert_failure(&fixture.am(&[patch]), "patch failed");

    fs::write(fixture.repo.join("file.txt"), "partial resolution\n")
        .expect("write partial resolution");
    fixture.success(&fixture.repo, &["add", "file.txt"]);
    fixture.success(&fixture.repo, &["am", "--abort"]);

    assert_eq!(fixture.rev_parse("HEAD"), local);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("read restored file"),
        "local\n"
    );
    let status = stdout_trim(&fixture.success(&fixture.repo, &["status", "--short"]));
    assert_eq!(status, before, "abort did not restore the pre-am status");
}

#[test]
fn skip_discards_failed_patch_and_applies_remaining_mail() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let base = fixture.commit_file("file.txt", "base\n", "base");
    fixture.commit_file("file.txt", "mail edit\n", "conflicting mail");
    fixture.commit_file("other.txt", "second\n", "independent mail");
    let patches = fixture.format_series(&base);
    assert_eq!(patches.len(), 2);

    fixture.success(&fixture.repo, &["reset", "--hard", &base]);
    let local = fixture.commit_file("file.txt", "local\n", "local divergence");
    assert_failure(&fixture.am(&patches), "patch failed");
    fixture.success(&fixture.repo, &["am", "--skip"]);

    assert_eq!(fixture.rev_parse("HEAD^"), local);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("read local file"),
        "local\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("other.txt")).expect("read second patch"),
        "second\n"
    );
}

#[test]
fn dirty_start_and_unexpected_staged_continue_fail_closed() {
    let (fixture, base, patch) = setup_single_patch();
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);
    fs::write(fixture.repo.join("file.txt"), "dirty\n").expect("write dirty file");
    assert_failure(&fixture.am(std::slice::from_ref(&patch)), "cannot start am");
    assert_failure(
        &fixture.run(&fixture.repo, &["am", "--abort"]),
        "no am operation",
    );

    fixture.success(&fixture.repo, &["reset", "--hard", &base]);
    fixture.commit_file("file.txt", "local\n", "local divergence");
    fixture.commit_file("tracked.txt", "tracked\n", "add tracked path");
    assert_failure(&fixture.am(&[patch]), "patch failed");
    fs::write(fixture.repo.join("file.txt"), "resolved\n").expect("write resolution");
    fs::write(
        fixture.repo.join("tracked.txt"),
        "unrelated tracked change\n",
    )
    .expect("write unrelated tracked change");
    fixture.success(&fixture.repo, &["add", "file.txt"]);
    assert_failure(
        &fixture.run(&fixture.repo, &["am", "--continue"]),
        "outside the current am patch has unstaged changes",
    );
    fixture.success(&fixture.repo, &["restore", "tracked.txt"]);
    fs::write(fixture.repo.join("unrelated.txt"), "do not commit\n").expect("write unrelated");
    fixture.success(&fixture.repo, &["add", "file.txt", "unrelated.txt"]);
    assert_failure(
        &fixture.run(&fixture.repo, &["am", "--continue"]),
        "outside the current am patch",
    );
    fixture.success(&fixture.repo, &["am", "--abort"]);
}

#[test]
fn same_branch_head_movement_blocks_resume_but_not_abort() {
    let (fixture, base, patch) = setup_single_patch();
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);
    let original = fixture.commit_file("file.txt", "local\n", "local divergence");
    assert_failure(&fixture.am(&[patch]), "patch failed");

    fixture.success(&fixture.repo, &["reset", "--hard", &base]);
    fs::write(fixture.repo.join("file.txt"), "resolved\n").expect("write resolution");
    fixture.success(&fixture.repo, &["add", "file.txt"]);
    assert_failure(
        &fixture.run(&fixture.repo, &["am", "--continue"]),
        "moved during am",
    );

    fixture.success(&fixture.repo, &["am", "--abort"]);
    assert_eq!(fixture.rev_parse("HEAD"), original);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("read restored file"),
        "local\n"
    );
}

#[test]
fn abort_cleans_new_file_left_by_interruption_before_staging() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let base = fixture.commit_file("base.txt", "base\n", "base");
    fs::create_dir_all(fixture.repo.join("nested")).expect("create nested dir");
    fixture.commit_file("nested/new.txt", "new\n", "add nested file");
    let patches = fixture.format_series(&base);
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);
    let before = stdout_trim(&fixture.success(&fixture.repo, &["status", "--short"]));

    let interrupted = fixture.run_env(
        &fixture.repo,
        &["am", patches[0].to_str().expect("utf8 patch")],
        &[("LIBRA_TEST_AM_FAIL_AFTER_WRITE", "1")],
    );
    assert_failure(&interrupted, "test-injected am interruption");
    assert!(fixture.repo.join("nested/new.txt").is_file());

    fixture.success(&fixture.repo, &["am", "--abort"]);
    assert!(!fixture.repo.join("nested/new.txt").exists());
    assert!(!fixture.repo.join("nested").exists());
    assert_eq!(
        stdout_trim(&fixture.success(&fixture.repo, &["status", "--short"])),
        before
    );
}

#[test]
fn continue_retries_mail_after_interruption_following_state_save() {
    let (fixture, base, patch) = setup_single_patch();
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);

    let interrupted = fixture.run_env(
        &fixture.repo,
        &["am", patch.to_str().expect("utf8 patch")],
        &[("LIBRA_TEST_AM_FAIL_AFTER_STATE", "1")],
    );
    assert_failure(&interrupted, "interruption after saving initial state");
    assert_eq!(fixture.rev_parse("HEAD"), base);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("read untouched file"),
        "base\n"
    );

    fixture.success(&fixture.repo, &["am", "--continue"]);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("read resumed file"),
        "from mail\n"
    );
    assert_eq!(fixture.rev_parse("HEAD^"), base);
}

#[test]
fn continue_applies_next_mail_after_interruption_between_commits() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let base = fixture.commit_file("first.txt", "base\n", "base");
    fixture.commit_file("first.txt", "first mail\n", "first mail");
    fixture.commit_file("second.txt", "second mail\n", "second mail");
    let patches = fixture.format_series(&base);
    assert_eq!(patches.len(), 2);
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);
    let patch_names: Vec<&str> = patches
        .iter()
        .map(|patch| patch.to_str().expect("utf8 patch"))
        .collect();
    let mut args = vec!["am"];
    args.extend(patch_names);

    let interrupted = fixture.run_env(
        &fixture.repo,
        &args,
        &[("LIBRA_TEST_AM_FAIL_AFTER_COMMIT", "1")],
    );
    assert_failure(&interrupted, "interruption between commits");
    assert_eq!(fixture.rev_parse("HEAD^"), base);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("first.txt")).expect("read first result"),
        "first mail\n"
    );
    assert!(!fixture.repo.join("second.txt").exists());

    fixture.success(&fixture.repo, &["am", "--continue"]);
    assert_eq!(fixture.rev_parse("HEAD^^"), base);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("second.txt")).expect("read second result"),
        "second mail\n"
    );
}

#[test]
fn untracked_patch_target_is_rejected_before_state_is_saved() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let base = fixture.commit_file("base.txt", "base\n", "base");
    fixture.commit_file("new.txt", "from mail\n", "add file");
    let patches = fixture.format_series(&base);
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);
    fs::write(fixture.repo.join("new.txt"), "untracked user data\n")
        .expect("write untracked collision");

    assert_failure(&fixture.am(&patches), "would be overwritten by am");
    assert_eq!(
        fs::read_to_string(fixture.repo.join("new.txt")).expect("read untracked data"),
        "untracked user data\n"
    );
    assert_failure(
        &fixture.run(&fixture.repo, &["am", "--abort"]),
        "no am operation",
    );
}

#[test]
fn ignored_untracked_patch_target_is_rejected_before_state_is_saved() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file(".libraignore", "ignored.txt\n", "ignore target");
    fs::write(fixture.repo.join("ignored.txt"), "ignored user data\n")
        .expect("write ignored user data");
    assert!(
        stdout_trim(&fixture.success(&fixture.repo, &["status", "--short"])).is_empty(),
        "fixture target must be hidden by ignore rules"
    );
    let patch = fixture.root.join("ignored.patch");
    fs::write(
        &patch,
        "From: Mail Author <mail-author@example.com>\n\
Date: Tue, 14 Jul 2026 10:00:00 +0800\n\
Subject: [PATCH] overwrite ignored target\n\
Content-Type: text/plain; charset=UTF-8\n\
\n\
---\n\
diff --git a/ignored.txt b/ignored.txt\n\
--- a/ignored.txt\n\
+++ b/ignored.txt\n\
@@ -1 +1 @@\n\
-ignored user data\n\
+mail data\n",
    )
    .expect("write ignored-target mail");

    assert_failure(&fixture.am(&[patch]), "would be overwritten by am");
    assert_eq!(
        fs::read_to_string(fixture.repo.join("ignored.txt")).expect("read ignored user data"),
        "ignored user data\n"
    );
    assert_failure(
        &fixture.run(&fixture.repo, &["am", "--abort"]),
        "no am operation",
    );
}

#[test]
fn noncanonical_patch_path_cannot_alias_untracked_user_data() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file("base.txt", "base\n", "base");
    fs::create_dir_all(fixture.repo.join("dir")).expect("create untracked dir");
    fs::write(fixture.repo.join("dir/file.txt"), "user data\n").expect("write untracked user data");
    let patch = fixture.repo.join("alias.patch");
    fs::write(
        &patch,
        "From: Mail Author <mail-author@example.com>\n\
Date: Tue, 14 Jul 2026 10:00:00 +0800\n\
Subject: [PATCH] overwrite alias\n\
Content-Type: text/plain; charset=UTF-8\n\
\n\
---\n\
diff --git a/dir//file.txt b/dir//file.txt\n\
--- a/dir//file.txt\n\
+++ b/dir//file.txt\n\
@@ -1 +1 @@\n\
-user data\n\
+mail data\n",
    )
    .expect("write alias mail");

    assert_failure(&fixture.am(&[patch]), "non-canonical");
    assert_eq!(
        fs::read_to_string(fixture.repo.join("dir/file.txt")).expect("read preserved data"),
        "user data\n"
    );
    assert_failure(
        &fixture.run(&fixture.repo, &["am", "--abort"]),
        "no am operation",
    );
}

#[cfg(unix)]
#[test]
fn content_patch_preserves_executable_permission() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = CliFixture::new();
    fixture.init_repo();
    fs::write(fixture.repo.join("script.sh"), "#!/bin/sh\necho base\n").expect("write script");
    fs::set_permissions(
        fixture.repo.join("script.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("make executable");
    fixture.success(&fixture.repo, &["add", "script.sh"]);
    fixture.success(&fixture.repo, &["commit", "-m", "base script"]);
    let base = fixture.rev_parse("HEAD");
    fixture.commit_file("script.sh", "#!/bin/sh\necho mail\n", "edit script");
    let patches = fixture.format_series(&base);
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);

    let applied = fixture.am(&patches);
    assert_success(&["am"], &applied);
    let mode = fs::metadata(fixture.repo.join("script.sh"))
        .expect("script metadata")
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0, "am cleared the executable bits");
}

#[test]
fn one_mail_can_add_and_delete_paths() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let base = fixture.commit_file("old file.txt", "remove me\n", "base");
    fs::remove_file(fixture.repo.join("old file.txt")).expect("remove tracked file");
    fs::write(fixture.repo.join("new file.txt"), "new file\n").expect("write new file");
    fixture.success(&fixture.repo, &["add", "--all"]);
    fixture.success(&fixture.repo, &["commit", "-m", "replace file"]);
    let patches = fixture.format_series(&base);
    assert_eq!(patches.len(), 1);
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);

    assert_success(&["am"], &fixture.am(&patches));
    assert!(!fixture.repo.join("old file.txt").exists());
    assert_eq!(
        fs::read_to_string(fixture.repo.join("new file.txt")).expect("read new path"),
        "new file\n"
    );
    assert!(
        stdout_trim(&fixture.success(&fixture.repo, &["status", "--short"])).is_empty(),
        "am left the index or worktree dirty"
    );
}

#[test]
fn mail_replay_is_hash_kind_neutral_in_sha256_repo() {
    let fixture = CliFixture::new();
    fixture.init_repo_with_format(Some("sha256"));
    let base = fixture.commit_file("wide.txt", "base\n", "base");
    fixture.commit_file("wide.txt", "from sha256 mail\n", "wide mail");
    let patches = fixture.format_series(&base);
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);

    assert_success(&["am"], &fixture.am(&patches));
    assert_eq!(fixture.rev_parse("HEAD").len(), 64);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("wide.txt")).expect("read sha256 result"),
        "from sha256 mail\n"
    );
}

#[test]
fn json_output_and_help_expose_the_minimal_surface() {
    let fixture = CliFixture::new();
    let help = fixture.success(&fixture.root, &["am", "--help"]);
    let help = String::from_utf8_lossy(&help.stdout);
    for expected in ["--continue", "--skip", "--abort", "EXAMPLES:"] {
        assert!(
            help.contains(expected),
            "missing {expected} in help:\n{help}"
        );
    }

    fixture.init_repo();
    let no_state = fixture.run(&fixture.repo, &["--json", "am", "--continue"]);
    assert!(!no_state.status.success());
    let error: Value = serde_json::from_slice(&no_state.stderr).expect("JSON error");
    assert_eq!(error["error_code"], "LBR-CONFLICT-002");

    let (applied_fixture, base, patch) = setup_single_patch();
    applied_fixture.success(&applied_fixture.repo, &["reset", "--hard", &base]);
    let applied = applied_fixture.success(
        &applied_fixture.repo,
        &["--json", "am", patch.to_str().expect("utf8 patch")],
    );
    let response: Value = serde_json::from_slice(&applied.stdout).expect("JSON success");
    assert_eq!(response["ok"], true);
    assert_eq!(response["command"], "am");
    assert_eq!(response["data"]["action"], "apply");
    assert_eq!(response["data"]["applied"][0]["subject"], "mail change");
    assert_eq!(
        response["data"]["applied"][0]["commit"],
        applied_fixture.rev_parse("HEAD")
    );
}

// ---------------------------------------------------------------------------
// PD-09 ①: stdin `-` and mbox splitting
// ---------------------------------------------------------------------------

impl CliFixture {
    /// Run `libra` with `input` piped to stdin.
    fn run_stdin(&self, cwd: &Path, args: &[&str], input: &[u8]) -> Output {
        use std::{io::Write, process::Stdio};

        let mut command = self.command(cwd, args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn libra with stdin");
        child
            .stdin
            .take()
            .expect("stdin piped")
            .write_all(input)
            .expect("write stdin payload");
        child.wait_with_output().expect("wait for libra")
    }

    /// A two-commit series exported as one mbox string (the `.patch`
    /// files each begin with an mbox `From ` envelope line, so
    /// concatenation IS the mbox).
    fn two_message_mbox(&self) -> (String, String) {
        let base = self.commit_file("file.txt", "base\n", "base");
        self.commit_file("file.txt", "base\nfirst\n", "mbox first change");
        self.commit_file("file.txt", "base\nfirst\nsecond\n", "mbox second change");
        let patches = self.format_series(&base);
        assert_eq!(patches.len(), 2, "two exported mails");
        let mbox: String = patches
            .iter()
            .map(|path| fs::read_to_string(path).expect("read exported mail"))
            .collect();
        assert!(
            mbox.starts_with("From "),
            "exported mails carry the mbox envelope: {mbox}"
        );
        // Rewind to base so the series can be re-applied.
        self.success(&self.repo, &["reset", "--hard", &base]);
        (mbox, base)
    }
}

/// A FILE whose first line is an mbox envelope is split into its
/// messages and applied in order — one commit per message.
#[test]
fn mbox_file_applies_every_message_in_order() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let (mbox, _base) = fixture.two_message_mbox();
    let mbox_path = fixture.root.join("series.mbox");
    fs::write(&mbox_path, &mbox).expect("write mbox");

    let out = fixture.run(
        &fixture.repo,
        &["--json", "am", mbox_path.to_str().expect("utf8 mbox path")],
    );
    assert_success(&["am", "series.mbox"], &out);
    let doc: Value = serde_json::from_str(stdout_trim(&out).as_str()).expect("am json");
    let applied = doc["data"]["applied"].as_array().expect("applied array");
    assert_eq!(applied.len(), 2, "{doc}");
    assert_eq!(applied[0]["subject"], "mbox first change", "{doc}");
    assert_eq!(applied[1]["subject"], "mbox second change", "{doc}");
    let source = applied[0]["source"].as_str().expect("source label");
    assert!(
        source.ends_with("series.mbox#1"),
        "multi-message sources are position-labelled: {source}"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("result"),
        "base\nfirst\nsecond\n"
    );
}

/// `libra am -` reads the same mbox from stdin.
#[test]
fn stdin_mbox_applies_every_message() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let (mbox, _base) = fixture.two_message_mbox();

    let out = fixture.run_stdin(&fixture.repo, &["am", "-"], mbox.as_bytes());
    assert_success(&["am", "-"], &out);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("result"),
        "base\nfirst\nsecond\n"
    );
    let log = fixture.success(&fixture.repo, &["log", "--oneline", "-2"]);
    let text = stdout_trim(&log);
    assert!(text.contains("mbox second change"), "{text}");
    assert!(text.contains("mbox first change"), "{text}");
}

/// A single non-mbox mail on stdin keeps the plain single-message path.
#[test]
fn stdin_single_mail_without_envelope_applies() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let (mbox, _base) = fixture.two_message_mbox();
    // Strip the envelope line and keep only the FIRST message: a plain
    // RFC-2822 mail without any mbox framing.
    let first = mbox
        .split_inclusive('\n')
        .take_while(|line| {
            !line.starts_with("From ") || line == &mbox.split_inclusive('\n').next().unwrap()
        })
        .collect::<String>();
    let single = first
        .split_once('\n')
        .map(|(_, rest)| rest.to_string())
        .expect("strip envelope");

    let out = fixture.run_stdin(&fixture.repo, &["am", "-"], single.as_bytes());
    assert_success(&["am", "-"], &out);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("result"),
        "base\nfirst\n"
    );
}

/// `-` may be given at most once.
#[test]
fn stdin_twice_is_rejected() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file("file.txt", "base\n", "base");
    let out = fixture.run_stdin(&fixture.repo, &["am", "-", "-"], b"");
    assert_failure(&out, "stdin ('-') can be given at most once");
}

/// mboxrd body quoting is undone: a commit-message line the writer
/// quoted as `>From ` stays byte-for-byte (git-default mboxo reading),
/// and a prose `From …` body line (no ctime-shaped date) never splits
/// the mbox.
#[test]
fn mbox_body_from_lines_are_preserved_and_never_split() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let base = fixture.commit_file("file.txt", "base\n", "base");
    fixture.commit_file("file.txt", "base\nquoted\n", "quoted change");
    let patches = fixture.format_series(&base);
    assert_eq!(patches.len(), 1);
    let mail = fs::read_to_string(&patches[0]).expect("read exported mail");
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);

    // Inject a quoted `>From ` line AND a prose `From …` line ending in
    // four digits (the shape the old loose heuristic false-split on).
    let injected = mail.replacen(
        "\n\n---\n",
        "\n\n>From here on, quoting matters.\nFrom my reading of RFC 9110\n---\n",
        1,
    );
    assert_ne!(injected, mail, "the fixture mail must carry a message slot");
    let out = fixture.run_stdin(&fixture.repo, &["am", "-"], injected.as_bytes());
    assert_success(&["am", "-"], &out);
    let log = fixture.success(&fixture.repo, &["log", "-1"]);
    let text = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(
        text.contains(">From here on, quoting matters."),
        "the quoted line survives byte-for-byte like git: {text}"
    );
    assert!(
        text.contains("From my reading of RFC 9110"),
        "a prose From line never splits the mail: {text}"
    );
}

/// Real-world MTA envelopes carry timezone suffixes after the year; the
/// splitter must not silently drop the messages that follow.
#[test]
fn mbox_envelope_with_timezone_suffix_still_splits() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let (mbox, _base) = fixture.two_message_mbox();
    // Rewrite every envelope line to the UUCP/MTA shape with a timezone.
    let rewritten: String = mbox
        .lines()
        .map(|line| {
            if line.starts_with("From ") && line.contains(':') {
                "From dev@example.com Wed Jun 30 21:49:08 1993 -0400".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let out = fixture.run_stdin(&fixture.repo, &["am", "-"], rewritten.as_bytes());
    assert_success(&["am", "-"], &out);
    let log = fixture.success(&fixture.repo, &["log", "--oneline", "-2"]);
    let text = stdout_trim(&log);
    assert!(text.contains("mbox first change"), "{text}");
    assert!(
        text.contains("mbox second change"),
        "the second message is not silently dropped: {text}"
    );
}

/// Sequencer state created from STDIN content persists: a conflicting
/// first message pauses the run even though `-` cannot be re-read (the
/// full mail bytes live in the saved state), and `--abort` restores the
/// pre-am tip.
#[test]
fn stdin_mbox_conflict_state_persists_and_aborts() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let (mbox, _base) = fixture.two_message_mbox();
    // Diverge so message 1's context ("base") no longer matches.
    let diverged = fixture.commit_file("file.txt", "diverged\n", "diverge");

    let out = fixture.run_stdin(&fixture.repo, &["am", "-"], mbox.as_bytes());
    assert!(!out.status.success(), "conflicting mbox pauses the run");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("am --abort") || stderr.contains("conflict"),
        "actionable conflict guidance: {stderr}"
    );

    let abort = fixture.success(&fixture.repo, &["am", "--abort"]);
    let _ = abort;
    assert_eq!(
        fixture.rev_parse("HEAD"),
        diverged,
        "abort restores the tip"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("worktree restored"),
        "diverged\n"
    );
}

// ---------------------------------------------------------------------------
// PD-09 ②: binary / rename / copy / mode-only patch sections
// ---------------------------------------------------------------------------

/// Compose a minimal format-patch-shaped mail around raw diff sections.
fn craft_mail(subject: &str, sections: &str) -> String {
    format!(
        "From 1111111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001\n\
         From: Crafted Author <crafted@example.com>\n\
         Date: Thu, 1 Jan 2026 00:00:00 +0000\n\
         Subject: [PATCH] {subject}\n\
         \n\
         ---\n\
         {sections}\
         -- \n\
         2.43.0\n"
    )
}

const TEST_B85: &[u8; 85] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";

/// Test-side encoder for a `GIT binary patch` forward hunk (zlib deflate
/// + git base85 with per-line length chars), mirroring git's writer.
fn binary_hunk(kind: &str, payload: &[u8]) -> String {
    use std::io::Write as _;

    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(payload).expect("deflate payload");
    let deflated = encoder.finish().expect("finish deflate");

    let mut out = format!("GIT binary patch\n{kind} {}\n", payload.len());
    for chunk in deflated.chunks(52) {
        let len_char = if chunk.len() <= 26 {
            (b'A' + chunk.len() as u8 - 1) as char
        } else {
            (b'a' + chunk.len() as u8 - 27) as char
        };
        out.push(len_char);
        for group in chunk.chunks(4) {
            let mut buf = [0u8; 4];
            buf[..group.len()].copy_from_slice(group);
            let mut acc = u32::from_be_bytes(buf);
            let mut chars = [0u8; 5];
            for slot in (0..5).rev() {
                chars[slot] = TEST_B85[(acc % 85) as usize];
                acc /= 85;
            }
            out.push_str(std::str::from_utf8(&chars).expect("ascii"));
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

fn delta_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// A git pack delta that copies `copy_len` bytes from the base start and
/// then inserts `tail`.
fn git_delta(base: &[u8], copy_len: usize, tail: &[u8]) -> Vec<u8> {
    let mut delta = Vec::new();
    delta_varint(base.len() as u64, &mut delta);
    delta_varint((copy_len + tail.len()) as u64, &mut delta);
    // Copy op: offset 0 (no offset bytes), size in one byte (bit 0x10).
    assert!(copy_len > 0 && copy_len < 256, "test copy fits one byte");
    delta.push(0x80 | 0x10);
    delta.push(copy_len as u8);
    // Insert ops in <=127-byte chunks.
    for chunk in tail.chunks(127) {
        delta.push(chunk.len() as u8);
        delta.extend_from_slice(chunk);
    }
    delta
}

/// Binary literal patch: a new file materializes byte-identical content
/// (NUL bytes included), then a delta patch rewrites its tail.
#[test]
fn binary_literal_and_delta_patches_apply() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file("file.txt", "base\n", "base");

    let payload: Vec<u8> = (0u8..=255).cycle().take(700).collect();
    let add = craft_mail(
        "add binary blob",
        &format!(
            "diff --git a/blob.bin b/blob.bin\n\
             new file mode 100644\n\
             index 0000000..1111111\n\
             {}",
            binary_hunk("literal", &payload)
        ),
    );
    let mail_path = fixture.root.join("binary-add.patch");
    fs::write(&mail_path, &add).expect("write binary mail");
    let out = fixture.run(
        &fixture.repo,
        &["am", mail_path.to_str().expect("utf8 path")],
    );
    assert_success(&["am", "binary-add"], &out);
    assert_eq!(
        fs::read(fixture.repo.join("blob.bin")).expect("blob written"),
        payload,
        "literal payload lands byte-identical"
    );

    // Delta: keep the first 128 bytes, replace the rest.
    let tail: Vec<u8> = b"DELTA-TAIL".iter().copied().cycle().take(90).collect();
    let delta = git_delta(&payload, 128, &tail);
    let mut expected = payload[..128].to_vec();
    expected.extend_from_slice(&tail);
    // The delta's preimage id must MATCH the current content (the apply
    // seam verifies it like git); an unrelated id must refuse.
    let payload_oid =
        git_internal::internal::object::blob::Blob::from_content_bytes(payload.clone())
            .id
            .to_string();
    let edit = craft_mail(
        "rewrite binary blob",
        &format!(
            "diff --git a/blob.bin b/blob.bin\n\
             index {}..2222222 100644\n\
             {}",
            &payload_oid[..7],
            binary_hunk("delta", &delta)
        ),
    );
    let mail_path = fixture.root.join("binary-delta.patch");
    fs::write(&mail_path, &edit).expect("write delta mail");
    let out = fixture.run(
        &fixture.repo,
        &["am", mail_path.to_str().expect("utf8 path")],
    );
    assert_success(&["am", "binary-delta"], &out);
    assert_eq!(
        fs::read(fixture.repo.join("blob.bin")).expect("blob rewritten"),
        expected,
        "delta result matches copy+insert reconstruction"
    );

    // A delta whose recorded preimage does not match the CURRENT content
    // must refuse instead of corrupting silently.
    let stale = craft_mail(
        "stale delta",
        &format!(
            "diff --git a/blob.bin b/blob.bin\n\
             index {}..3333333 100644\n\
             {}",
            &payload_oid[..7],
            binary_hunk("delta", &git_delta(&expected, 16, b"XX"))
        ),
    );
    let mail_path = fixture.root.join("binary-stale.patch");
    fs::write(&mail_path, &stale).expect("write stale mail");
    let out = fixture.run(
        &fixture.repo,
        &["am", mail_path.to_str().expect("utf8 path")],
    );
    assert!(!out.status.success(), "stale preimage must refuse");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("recorded preimage"),
        "actionable preimage refusal: {stderr}"
    );
    fixture.success(&fixture.repo, &["am", "--abort"]);
}

/// A pure rename section moves the file; a rename with hunks moves AND
/// edits; the rename source deletion is staged in the same commit.
#[test]
fn rename_sections_move_and_edit() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file("old-name.txt", "alpha\nbeta\n", "base");

    let pure = craft_mail(
        "pure rename",
        "diff --git a/old-name.txt b/new-name.txt\n\
         similarity index 100%\n\
         rename from old-name.txt\n\
         rename to new-name.txt\n",
    );
    let mail_path = fixture.root.join("rename-pure.patch");
    fs::write(&mail_path, &pure).expect("write rename mail");
    let out = fixture.run(
        &fixture.repo,
        &["am", mail_path.to_str().expect("utf8 path")],
    );
    assert_success(&["am", "rename-pure"], &out);
    assert!(
        !fixture.repo.join("old-name.txt").exists(),
        "source removed"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("new-name.txt")).expect("dest exists"),
        "alpha\nbeta\n"
    );
    // The commit records both sides (source deletion staged too).
    let show = fixture.success(&fixture.repo, &["show", "--name-status", "HEAD"]);
    let text = String::from_utf8_lossy(&show.stdout).into_owned();
    assert!(text.contains("old-name.txt"), "{text}");
    assert!(text.contains("new-name.txt"), "{text}");

    let edited = craft_mail(
        "rename with edit",
        "diff --git a/new-name.txt b/final-name.txt\n\
         similarity index 66%\n\
         rename from new-name.txt\n\
         rename to final-name.txt\n\
         --- a/new-name.txt\n\
         +++ b/final-name.txt\n\
         @@ -1,2 +1,2 @@\n \
         alpha\n\
         -beta\n\
         +gamma\n",
    );
    let mail_path = fixture.root.join("rename-edit.patch");
    fs::write(&mail_path, &edited).expect("write rename-edit mail");
    let out = fixture.run(
        &fixture.repo,
        &["am", mail_path.to_str().expect("utf8 path")],
    );
    assert_success(&["am", "rename-edit"], &out);
    assert!(!fixture.repo.join("new-name.txt").exists());
    assert_eq!(
        fs::read_to_string(fixture.repo.join("final-name.txt")).expect("dest exists"),
        "alpha\ngamma\n"
    );
}

/// A copy section duplicates the source (which stays) and a mode-only
/// section flips the executable bit without content changes.
#[test]
fn copy_and_mode_only_sections_apply() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file("tool.sh", "#!/bin/sh\necho ok\n", "base");

    let copy = craft_mail(
        "copy the tool",
        "diff --git a/tool.sh b/tool-copy.sh\n\
         similarity index 100%\n\
         copy from tool.sh\n\
         copy to tool-copy.sh\n",
    );
    let mail_path = fixture.root.join("copy.patch");
    fs::write(&mail_path, &copy).expect("write copy mail");
    let out = fixture.run(
        &fixture.repo,
        &["am", mail_path.to_str().expect("utf8 path")],
    );
    assert_success(&["am", "copy"], &out);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("tool.sh")).expect("source stays"),
        "#!/bin/sh\necho ok\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("tool-copy.sh")).expect("copy exists"),
        "#!/bin/sh\necho ok\n"
    );

    let chmod = craft_mail(
        "make the tool executable",
        "diff --git a/tool.sh b/tool.sh\n\
         old mode 100644\n\
         new mode 100755\n",
    );
    let mail_path = fixture.root.join("mode.patch");
    fs::write(&mail_path, &chmod).expect("write mode mail");
    let out = fixture.run(
        &fixture.repo,
        &["am", mail_path.to_str().expect("utf8 path")],
    );
    assert_success(&["am", "mode-only"], &out);
    let mode = fs::metadata(fixture.repo.join("tool.sh"))
        .expect("stat tool")
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "executable bit set: {mode:o}");
    assert_eq!(
        fs::read_to_string(fixture.repo.join("tool.sh")).expect("content unchanged"),
        "#!/bin/sh\necho ok\n"
    );
}

/// The path-safety refusal surface does not shrink for the new section
/// kinds: rename destinations still reject escapes and `.libra`.
#[test]
fn extended_sections_keep_path_safety() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file("safe.txt", "content\n", "base");

    for (label, to) in [("escape", "../escape.txt"), ("internal", ".libra/hook.sh")] {
        let mail = craft_mail(
            "hostile rename",
            &format!(
                "diff --git a/safe.txt b/{to}\n\
                 similarity index 100%\n\
                 rename from safe.txt\n\
                 rename to {to}\n"
            ),
        );
        let mail_path = fixture.root.join(format!("hostile-{label}.patch"));
        fs::write(&mail_path, &mail).expect("write hostile mail");
        let out = fixture.run(
            &fixture.repo,
            &["am", mail_path.to_str().expect("utf8 path")],
        );
        assert!(!out.status.success(), "{label}: hostile rename must fail");
        assert!(
            fixture.repo.join("safe.txt").exists(),
            "{label}: source untouched after refusal"
        );
    }
}

// ---------------------------------------------------------------------------
// PD-09 ③: `-3` three-way fallback
// ---------------------------------------------------------------------------

/// Build a repo where the exported patch no longer applies (context
/// drifted) and return the patch path. The patch edits line 3 of a
/// 5-line file; the target then rewrites line 1 (disjoint → clean
/// three-way) or line 3 (overlapping → conflict).
fn three_way_fixture(fixture: &CliFixture, target_edit: &str) -> PathBuf {
    let base = fixture.commit_file(
        "file.txt",
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n",
        "base",
    );
    fixture.commit_file(
        "file.txt",
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\nEIGHT\nnine\n",
        "patch three",
    );
    let patches = fixture.format_series(&base);
    assert_eq!(patches.len(), 1);
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);
    // Diverge the target so the exported hunk's context no longer
    // matches at line 3's neighborhood.
    fixture.commit_file("file.txt", target_edit, "diverge");
    patches[0].clone()
}

/// Disjoint drift: plain am fails, `-3` merges both edits cleanly.
#[test]
fn three_way_merges_disjoint_drift() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    // The drift sits INSIDE the exported hunk's context window (line 5,
    // three lines above the patched line 8) so the plain apply fails,
    // while two clean separating lines let the three-way merge succeed.
    let patch = three_way_fixture(
        &fixture,
        "one\ntwo\nthree\nfour\nFIVE\nsix\nseven\neight\nnine\n",
    );
    let path = patch.to_str().expect("utf8 patch");

    let plain = fixture.run(&fixture.repo, &["am", path]);
    assert!(!plain.status.success(), "plain am must conflict");
    fixture.success(&fixture.repo, &["am", "--abort"]);

    let merged = fixture.run(&fixture.repo, &["am", "-3", path]);
    assert_success(&["am", "-3"], &merged);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("merged"),
        "one\ntwo\nthree\nfour\nFIVE\nsix\nseven\nEIGHT\nnine\n",
        "both sides of the disjoint drift survive"
    );
}

/// Overlapping drift: `-3` writes conflict markers, pauses, and the
/// resolved series continues to a commit.
#[test]
fn three_way_conflict_pauses_with_markers_then_continues() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let patch = three_way_fixture(
        &fixture,
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\nCHANGED-EIGHT\nnine\n",
    );
    let path = patch.to_str().expect("utf8 patch");

    let out = fixture.run(&fixture.repo, &["am", "-3", path]);
    assert!(!out.status.success(), "overlapping drift conflicts");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("three-way merge left conflicts"),
        "{stderr}"
    );
    let content = fs::read_to_string(fixture.repo.join("file.txt")).expect("marked file");
    assert!(
        content.contains("<<<<<<<"),
        "conflict markers present: {content}"
    );

    // Resolve, stage only the patch's path, continue.
    fs::write(
        fixture.repo.join("file.txt"),
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\nRESOLVED\nnine\n",
    )
    .expect("resolve");
    fixture.success(&fixture.repo, &["add", "file.txt"]);
    let done = fixture.success(&fixture.repo, &["am", "--continue"]);
    let _ = done;
    let log = fixture.success(&fixture.repo, &["log", "-1"]);
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("patch three"),
        "resolved mail committed"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("final"),
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\nRESOLVED\nnine\n"
    );
}

/// An unresolvable base (bogus index ids) keeps the plain refusal — the
/// fallback never fabricates content.
#[test]
fn three_way_without_local_base_keeps_plain_conflict() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let patch = three_way_fixture(
        &fixture,
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\nCHANGED-EIGHT\nnine\n",
    );
    let text = fs::read_to_string(&patch).expect("read patch");
    // Corrupt the index header's old id so no local blob can match.
    let corrupted: String = text
        .lines()
        .map(|line| {
            if line.starts_with("index ") {
                "index ffffffffff..ffffffffff 100644".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let path = fixture.root.join("bogus-base.patch");
    fs::write(&path, corrupted).expect("write corrupted patch");

    let out = fixture.run(
        &fixture.repo,
        &["am", "-3", path.to_str().expect("utf8 path")],
    );
    assert!(!out.status.success(), "no base -> still a conflict");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does not apply"),
        "plain refusal (no fabricated merge): {stderr}"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("untouched"),
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\nCHANGED-EIGHT\nnine\n",
        "worktree untouched without a resolvable base"
    );
}

// ---------------------------------------------------------------------------
// PD-09 ④: MIME multipart / attachment mails
// ---------------------------------------------------------------------------

/// Libra's own `format-patch --attach` output (multipart/mixed with a
/// text/x-patch attachment) round-trips through `libra am`.
#[test]
fn multipart_attach_self_roundtrip_applies() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let base = fixture.commit_file("file.txt", "base\n", "base");
    fixture.commit_file("file.txt", "base\nattached\n", "attached change");
    let mail = fixture.success(
        &fixture.repo,
        &["format-patch", "-1", "--attach", "--stdout"],
    );
    let text = String::from_utf8_lossy(&mail.stdout).into_owned();
    assert!(text.contains("multipart/mixed"), "{text}");
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);

    let out = fixture.run_stdin(&fixture.repo, &["am", "-"], text.as_bytes());
    assert_success(&["am", "-"], &out);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("result"),
        "base\nattached\n"
    );
    let log = fixture.success(&fixture.repo, &["log", "-1"]);
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("attached change"),
        "attached mail committed"
    );
}

/// multipart/alternative: the HTML part is skipped, the text part (with
/// the patch) applies; a base64 text attachment decodes per-part.
#[test]
fn multipart_alternative_skips_html_and_decodes_base64_part() {
    use base64::Engine as _;

    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file("file.txt", "alpha\n", "base");

    let patch_part = "\
patch body message\n\
---\n\
 file.txt | 1 +\n\
 1 file changed, 1 insertion(+)\n\
\n\
diff --git a/file.txt b/file.txt\n\
index 0000000..1111111 100644\n\
--- a/file.txt\n\
+++ b/file.txt\n\
@@ -1 +1,2 @@\n \
alpha\n\
+beta\n";
    let encoded_patch = base64::engine::general_purpose::STANDARD.encode(patch_part);
    // Wrap base64 to 60-char lines like real mailers.
    let wrapped: String = encoded_patch
        .as_bytes()
        .chunks(60)
        .map(|chunk| format!("{}\n", std::str::from_utf8(chunk).unwrap()))
        .collect();
    let mail = format!(
        "From 1111111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001\n\
         From: Crafted Author <crafted@example.com>\n\
         Date: Thu, 1 Jan 2026 00:00:00 +0000\n\
         Subject: [PATCH] alternative parts\n\
         Content-Type: multipart/alternative; boundary=\"=-=alt boundary=-=\"\n\
         \n\
         preamble to be ignored\n\
         --=-=alt boundary=-=\n\
         Content-Type: text/html; charset=utf-8\n\
         \n\
         <p>HTML that must never be parsed as a patch</p>\n\
         --=-=alt boundary=-=\n\
         Content-Type: text/plain; charset=utf-8\n\
         Content-Transfer-Encoding: base64\n\
         \n\
         {wrapped}\
         --=-=alt boundary=-=--\n"
    );
    let out = fixture.run_stdin(&fixture.repo, &["am", "-"], mail.as_bytes());
    assert_success(&["am", "-"], &out);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("result"),
        "alpha\nbeta\n"
    );
    let log = fixture.success(&fixture.repo, &["log", "-1"]);
    let text = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(text.contains("patch body message"), "{text}");
    assert!(!text.contains("HTML"), "html part never leaks: {text}");
}

/// A multipart mail whose only parts are unsupported (binary/html)
/// fails closed with an actionable message.
#[test]
fn multipart_without_text_part_fails_closed() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file("file.txt", "alpha\n", "base");
    let mail = "\
From 1111111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001\n\
From: Crafted Author <crafted@example.com>\n\
Date: Thu, 1 Jan 2026 00:00:00 +0000\n\
Subject: [PATCH] no text part\n\
Content-Type: multipart/mixed; boundary=\"bb\"\n\
\n\
--bb\n\
Content-Type: application/octet-stream\n\
Content-Transfer-Encoding: base64\n\
\n\
AAAA\n\
--bb--\n";
    let out = fixture.run_stdin(&fixture.repo, &["am", "-"], mail.as_bytes());
    assert!(!out.status.success(), "no text part must fail closed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no supported text part"),
        "actionable refusal: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// PD-09 ⑤: applypatch-msg / pre-applypatch / post-applypatch hooks
// ---------------------------------------------------------------------------

fn install_hook(fixture: &CliFixture, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;

    let hooks = fixture.repo.join(".libra").join("hooks");
    fs::create_dir_all(&hooks).expect("create hooks dir");
    let path = hooks.join(name);
    fs::write(&path, script).expect("write hook");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod hook");
}

fn remove_hook(fixture: &CliFixture, name: &str) {
    let _ = fs::remove_file(fixture.repo.join(".libra").join("hooks").join(name));
}

fn one_patch_series(fixture: &CliFixture) -> PathBuf {
    let base = fixture.commit_file("file.txt", "base\n", "base");
    fixture.commit_file("file.txt", "base\nhooked\n", "hooked change");
    let patches = fixture.format_series(&base);
    assert_eq!(patches.len(), 1);
    fixture.success(&fixture.repo, &["reset", "--hard", &base]);
    patches[0].clone()
}

/// `applypatch-msg` may rewrite the proposed message; a non-zero exit
/// refuses the mail BEFORE any worktree write, leaving a resumable
/// series.
#[test]
fn applypatch_msg_hook_edits_message_and_gates() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let patch = one_patch_series(&fixture);
    let path = patch.to_str().expect("utf8 patch");

    install_hook(
        &fixture,
        "applypatch-msg",
        "#!/bin/sh\nprintf '\\nHook-Edited: yes\\n' >> \"$1\"\n",
    );
    let out = fixture.run(&fixture.repo, &["am", path]);
    assert_success(&["am", "edit-hook"], &out);
    let log = fixture.success(&fixture.repo, &["log", "-1"]);
    let text = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(text.contains("Hook-Edited: yes"), "edited message: {text}");

    // Refusal: state saved, worktree untouched, --abort restores.
    let tip = fixture.rev_parse("HEAD");
    fixture.success(&fixture.repo, &["reset", "--hard", "HEAD~1"]);
    install_hook(&fixture, "applypatch-msg", "#!/bin/sh\nexit 1\n");
    let refused = fixture.run(&fixture.repo, &["am", path]);
    assert!(!refused.status.success(), "hook refusal fails the mail");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("applypatch-msg hook refused"),
        "actionable refusal: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("untouched"),
        "base\n",
        "refusal happens before any worktree write"
    );
    fixture.success(&fixture.repo, &["am", "--abort"]);
    let _ = tip;
}

/// `pre-applypatch` gates the commit AFTER the worktree write; removing
/// the hook lets `--continue` finish the paused mail.
#[test]
fn pre_applypatch_gate_blocks_commit_then_continue_finishes() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let patch = one_patch_series(&fixture);
    let path = patch.to_str().expect("utf8 patch");
    let tip_before = fixture.rev_parse("HEAD");

    install_hook(&fixture, "pre-applypatch", "#!/bin/sh\nexit 1\n");
    let out = fixture.run(&fixture.repo, &["am", path]);
    assert!(!out.status.success(), "pre-applypatch refusal pauses");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("pre-applypatch hook refused"),
        "actionable refusal: {stderr}"
    );
    assert_eq!(
        fixture.rev_parse("HEAD"),
        tip_before,
        "no commit while the gate refuses"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("written"),
        "base\nhooked\n",
        "the worktree write already happened"
    );

    remove_hook(&fixture, "pre-applypatch");
    let done = fixture.success(&fixture.repo, &["am", "--continue"]);
    let _ = done;
    let log = fixture.success(&fixture.repo, &["log", "-1"]);
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("hooked change"),
        "resolved mail committed after the gate lifts"
    );
}

/// `post-applypatch` is advisory: a failing hook warns but never fails
/// the applied mail.
#[test]
fn post_applypatch_failure_is_advisory() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let patch = one_patch_series(&fixture);
    let path = patch.to_str().expect("utf8 patch");

    install_hook(&fixture, "post-applypatch", "#!/bin/sh\nexit 7\n");
    let out = fixture.run(&fixture.repo, &["am", path]);
    assert_success(&["am", "advisory-hook"], &out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("post-applypatch") && stderr.contains("exited with code 7"),
        "advisory warning surfaces: {stderr}"
    );
    let log = fixture.success(&fixture.repo, &["log", "-1"]);
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("hooked change"),
        "the mail still committed"
    );
}

// ---------------------------------------------------------------------------
// PD-09 adversarial-review regression pins
// ---------------------------------------------------------------------------

/// Review #4/#5/#6: empty-file creation applies (git emits no hunks),
/// new files get umask-derived permissions (not the temp file's 0600),
/// and `new file mode 120000` materializes a REAL symlink.
#[test]
fn empty_file_umask_and_symlink_sections_apply() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file("file.txt", "base\n", "base");

    let mail = craft_mail(
        "empty file, exec file and symlink",
        "diff --git a/keep/.gitkeep b/keep/.gitkeep\n\
         new file mode 100644\n\
         index 0000000..e69de29\n\
         diff --git a/run.sh b/run.sh\n\
         new file mode 100755\n\
         index 0000000..1111111\n\
         --- /dev/null\n\
         +++ b/run.sh\n\
         @@ -0,0 +1 @@\n\
         +#!/bin/sh\n\
         diff --git a/link.txt b/link.txt\n\
         new file mode 120000\n\
         index 0000000..2222222\n\
         --- /dev/null\n\
         +++ b/link.txt\n\
         @@ -0,0 +1 @@\n\
         +file.txt\n\
         \\ No newline at end of file\n",
    );
    let mail_path = fixture.root.join("mixed-new.patch");
    fs::write(&mail_path, &mail).expect("write mixed mail");
    let out = fixture.run(
        &fixture.repo,
        &["am", mail_path.to_str().expect("utf8 path")],
    );
    assert_success(&["am", "mixed-new"], &out);

    // Empty file exists and is empty.
    assert_eq!(
        fs::read(fixture.repo.join("keep/.gitkeep")).expect("empty file"),
        Vec::<u8>::new()
    );
    // New files carry umask-derived modes: group/other readable under
    // any sane umask (022/002), never the private 0600/0700.
    let keep_mode = fs::metadata(fixture.repo.join("keep/.gitkeep"))
        .expect("stat keep")
        .permissions()
        .mode();
    assert_ne!(keep_mode & 0o044, 0, "group/other readable: {keep_mode:o}");
    let run_mode = fs::metadata(fixture.repo.join("run.sh"))
        .expect("stat run.sh")
        .permissions()
        .mode();
    assert_ne!(run_mode & 0o111, 0, "exec bit: {run_mode:o}");
    assert_ne!(run_mode & 0o044, 0, "group/other readable: {run_mode:o}");
    // The symlink is a real symlink pointing at the patched target.
    let meta = fs::symlink_metadata(fixture.repo.join("link.txt")).expect("lstat link");
    assert!(meta.file_type().is_symlink(), "materialized as a symlink");
    assert_eq!(
        fs::read_link(fixture.repo.join("link.txt")).expect("read link"),
        std::path::PathBuf::from("file.txt")
    );
    // And it round-trips through the commit as mode 120000.
    let show = fixture.success(&fixture.repo, &["show", "--name-status", "HEAD"]);
    let text = String::from_utf8_lossy(&show.stdout).into_owned();
    assert!(text.contains("link.txt"), "{text}");
}

/// Review #10/#12: when a -3 mail pauses on conflict, the applypatch-msg
/// hook's edit and the mail's chmod section BOTH survive into the
/// `--continue` commit.
#[test]
fn conflict_continue_keeps_hook_edit_and_chmod() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file("tool.sh", "#!/bin/sh\n", "add tool");
    let patch = three_way_fixture(
        &fixture,
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\nCHANGED-EIGHT\nnine\n",
    );
    // Extend the conflicting mail with a chmod section for tool.sh —
    // INSIDE the patch body (before the `-- ` signature trailer, which
    // the mail parser strips).
    let raw = fs::read_to_string(&patch).expect("read mail");
    let mail = raw.replacen(
        "\n-- \n",
        "\ndiff --git a/tool.sh b/tool.sh\nold mode 100644\nnew mode 100755\n-- \n",
        1,
    );
    assert_ne!(
        mail, raw,
        "the exported mail must carry a signature trailer"
    );
    let mail_path = fixture.root.join("conflict-chmod.patch");
    fs::write(&mail_path, &mail).expect("write mail");
    install_hook(
        &fixture,
        "applypatch-msg",
        "#!/bin/sh\nprintf '\\nHook-Stamp: kept\\n' >> \"$1\"\n",
    );

    let out = fixture.run(
        &fixture.repo,
        &["am", "-3", mail_path.to_str().expect("utf8 path")],
    );
    assert!(!out.status.success(), "conflicting mail pauses");

    // Resolve the conflicted file, stage it, continue.
    fs::write(
        fixture.repo.join("file.txt"),
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\nRESOLVED\nnine\n",
    )
    .expect("resolve");
    fixture.success(&fixture.repo, &["add", "file.txt"]);
    fixture.success(&fixture.repo, &["am", "--continue"]);

    let log = fixture.success(&fixture.repo, &["log", "-1"]);
    let text = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(
        text.contains("Hook-Stamp: kept"),
        "hook edit survives the pause: {text}"
    );
    let mode = fs::metadata(fixture.repo.join("tool.sh"))
        .expect("stat tool")
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "chmod survives the pause: {mode:o}");
    // The chmod is IN the commit, not just the worktree.
    let show = fixture.success(&fixture.repo, &["show", "--raw", "HEAD"]);
    let raw = String::from_utf8_lossy(&show.stdout).into_owned();
    assert!(raw.contains("100755"), "committed tree mode: {raw}");
}

/// Review #13: resolving a -3 conflict by restoring HEAD content must
/// NOT silently re-apply the mail and re-clobber the file with markers.
#[test]
fn conflict_continue_with_nothing_staged_errors_instead_of_reclobbering() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let patch = three_way_fixture(
        &fixture,
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\nCHANGED-EIGHT\nnine\n",
    );
    let path = patch.to_str().expect("utf8 patch");
    let out = fixture.run(&fixture.repo, &["am", "-3", path]);
    assert!(!out.status.success(), "conflicting mail pauses");

    // "Resolve" by restoring the exact HEAD content (nothing to stage).
    fixture.success(&fixture.repo, &["restore", "file.txt"]);
    let retry = fixture.run(&fixture.repo, &["am", "--continue"]);
    assert!(!retry.status.success(), "nothing staged is an error");
    let stderr = String::from_utf8_lossy(&retry.stderr);
    assert!(
        stderr.contains("no staged resolution"),
        "actionable guidance instead of a silent re-clobber: {stderr}"
    );
    assert!(
        !fs::read_to_string(fixture.repo.join("file.txt"))
            .expect("file intact")
            .contains("<<<<<<<"),
        "the restored file is not re-clobbered with markers"
    );
    fixture.success(&fixture.repo, &["am", "--abort"]);
}

/// Review #11: `am --abort` refuses to hard-reset away commits made ON
/// TOP of the paused state, while the backward-rescue reset keeps the
/// historical restore behavior (pinned elsewhere).
#[test]
fn abort_refuses_to_discard_descendant_commits() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let patch = three_way_fixture(
        &fixture,
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\nCHANGED-EIGHT\nnine\n",
    );
    let path = patch.to_str().expect("utf8 patch");
    let out = fixture.run(&fixture.repo, &["am", "-3", path]);
    assert!(!out.status.success(), "conflicting mail pauses");

    // The user commits the resolution manually (forgetting --continue).
    fs::write(
        fixture.repo.join("file.txt"),
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\nRESOLVED\nnine\n",
    )
    .expect("resolve");
    fixture.success(&fixture.repo, &["add", "file.txt"]);
    fixture.success(&fixture.repo, &["commit", "-m", "manual resolution"]);
    let manual = fixture.rev_parse("HEAD");

    let abort = fixture.run(&fixture.repo, &["am", "--abort"]);
    assert!(!abort.status.success(), "descendant tip is protected");
    let stderr = String::from_utf8_lossy(&abort.stderr);
    assert!(
        stderr.contains("refusing to discard"),
        "actionable refusal: {stderr}"
    );
    assert_eq!(
        fixture.rev_parse("HEAD"),
        manual,
        "the manual resolution commit survives"
    );
    // Clean up per the hint: move back to the paused tip, then abort.
    fixture.success(&fixture.repo, &["reset", "--hard", "HEAD~1"]);
    fixture.success(&fixture.repo, &["am", "--abort"]);
}

/// Review #14/#15/#16: multipart edge shapes — boundary transport
/// padding, an empty-header part, and quoted `;` inside Content-Type
/// parameters — all parse like git.
#[test]
fn multipart_edge_shapes_parse() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file("file.txt", "alpha\n", "base");

    let mail = "From 1111111111111111111111111111111111111111 Mon Sep 17 00:00:00 2001\n\
From: Crafted Author <crafted@example.com>\n\
Date: Thu, 1 Jan 2026 00:00:00 +0000\n\
Subject: [PATCH] multipart edges\n\
Content-Type: multipart/mixed; name=\"x;boundary=zzz\"; boundary=\"real b\"\n\
\n\
preamble\n\
--real b \n\
\n\
edge shapes message\n\
---\n\
 file.txt | 1 +\n\
 1 file changed, 1 insertion(+)\n\
\n\
diff --git a/file.txt b/file.txt\n\
index 0000000..1111111 100644\n\
--- a/file.txt\n\
+++ b/file.txt\n\
@@ -1 +1,2 @@\n alpha\n\
+beta\n\
--real b--\t\n\
epilogue is ignored\n";
    let out = fixture.run_stdin(&fixture.repo, &["am", "-"], mail.as_bytes());
    assert_success(&["am", "multipart-edges"], &out);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("result"),
        "alpha\nbeta\n"
    );
    let log = fixture.success(&fixture.repo, &["log", "-1"]);
    let text = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(
        text.contains("edge shapes message"),
        "empty-header part body survives: {text}"
    );
}
