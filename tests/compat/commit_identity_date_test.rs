//! Identity, date, and message-source contracts for plan-20260708 P0-08.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

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
        fs::create_dir_all(&self.repo).expect("create repo dir");
        self.success(
            &self.root,
            &[
                "init",
                "--vault",
                "false",
                self.repo.to_str().expect("utf8 repo"),
            ],
        );
        self.success(&self.repo, &["config", "set", "user.name", "Config User"]);
        self.success(
            &self.repo,
            &["config", "set", "user.email", "config@example.com"],
        );
    }

    fn rev_parse(&self, spec: &str) -> String {
        stdout_trim(&self.success(&self.repo, &["rev-parse", spec]))
    }

    fn cat_file(&self, spec: &str) -> String {
        String::from_utf8(self.success(&self.repo, &["cat-file", "-p", spec]).stdout)
            .expect("cat-file output is utf8")
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

fn stdout_trim(output: &Output) -> String {
    String::from_utf8(output.stdout.clone())
        .expect("stdout is utf8")
        .trim()
        .to_string()
}

fn first_line_with<'a>(text: &'a str, prefix: &str) -> &'a str {
    text.lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("missing {prefix:?} line in commit:\n{text}"))
}

fn message_body(raw_commit: &str) -> &str {
    raw_commit
        .split_once("\n\n")
        .map(|(_, body)| body)
        .unwrap_or("")
}

#[test]
fn commit_honors_git_identity_and_date_env_over_config() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    fs::write(fixture.repo.join("env.txt"), "env\n").expect("write file");
    fixture.success(&fixture.repo, &["add", "env.txt"]);
    fixture.success_env(
        &fixture.repo,
        &[
            "commit",
            "--no-gpg-sign",
            "--no-verify",
            "-m",
            "env identity",
        ],
        &[
            ("GIT_AUTHOR_NAME", "Env Author"),
            ("GIT_AUTHOR_EMAIL", "author@example.com"),
            ("GIT_AUTHOR_DATE", "1700000000 +0200"),
            ("GIT_COMMITTER_NAME", "Env Committer"),
            ("GIT_COMMITTER_EMAIL", "committer@example.com"),
            ("GIT_COMMITTER_DATE", "1700000500 -0330"),
            ("LIBRA_COMMITTER_NAME", "Ignored Libra"),
            ("LIBRA_COMMITTER_EMAIL", "ignored@example.com"),
        ],
    );

    let raw = fixture.cat_file("HEAD");
    assert_eq!(
        first_line_with(&raw, "author "),
        "author Env Author <author@example.com> 1700000000 +0200"
    );
    assert_eq!(
        first_line_with(&raw, "committer "),
        "committer Env Committer <committer@example.com> 1700000500 -0330"
    );
}

#[test]
fn commit_date_flag_and_reset_author_update_author_metadata() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    fs::write(fixture.repo.join("date.txt"), "date\n").expect("write file");
    fixture.success(&fixture.repo, &["add", "date.txt"]);
    fixture.success_env(
        &fixture.repo,
        &[
            "commit",
            "--no-gpg-sign",
            "--no-verify",
            "--date",
            "1700000700 -0100",
            "-m",
            "date flag",
        ],
        &[
            ("GIT_AUTHOR_NAME", "Flag Author"),
            ("GIT_AUTHOR_EMAIL", "flag@example.com"),
            ("GIT_AUTHOR_DATE", "1700000000 +0200"),
            ("GIT_COMMITTER_NAME", "Flag Committer"),
            ("GIT_COMMITTER_EMAIL", "flag-committer@example.com"),
            ("GIT_COMMITTER_DATE", "1700000800 +0000"),
        ],
    );
    let raw = fixture.cat_file("HEAD");
    assert_eq!(
        first_line_with(&raw, "author "),
        "author Flag Author <flag@example.com> 1700000700 -0100"
    );
    assert_eq!(
        first_line_with(&raw, "committer "),
        "committer Flag Committer <flag-committer@example.com> 1700000800 +0000"
    );

    fixture.success_env(
        &fixture.repo,
        &[
            "commit",
            "--amend",
            "--reset-author",
            "--no-edit",
            "--no-gpg-sign",
            "--no-verify",
        ],
        &[
            ("GIT_AUTHOR_NAME", "Reset Author"),
            ("GIT_AUTHOR_EMAIL", "reset@example.com"),
            ("GIT_AUTHOR_DATE", "1700000900 +0000"),
            ("GIT_COMMITTER_NAME", "Reset Committer"),
            ("GIT_COMMITTER_EMAIL", "reset-committer@example.com"),
            ("GIT_COMMITTER_DATE", "1700001000 +0000"),
        ],
    );
    let amended = fixture.cat_file("HEAD");
    assert_eq!(
        first_line_with(&amended, "author "),
        "author Reset Author <reset@example.com> 1700000900 +0000"
    );
    assert_eq!(
        first_line_with(&amended, "committer "),
        "committer Reset Committer <reset-committer@example.com> 1700001000 +0000"
    );
}

#[test]
fn reuse_and_reedit_message_reuse_source_author_metadata() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    fs::write(fixture.repo.join("source.txt"), "source\n").expect("write source");
    fixture.success(&fixture.repo, &["add", "source.txt"]);
    fixture.success(
        &fixture.repo,
        &[
            "commit",
            "--no-gpg-sign",
            "--no-verify",
            "--author",
            "Source Author <source@example.com>",
            "--date",
            "1700000100 +0200",
            "-m",
            "source subject\n\nsource body",
        ],
    );
    let source = fixture.rev_parse("HEAD");

    fs::write(fixture.repo.join("reuse.txt"), "reuse\n").expect("write reuse");
    fixture.success(&fixture.repo, &["add", "reuse.txt"]);
    fixture.success_env(
        &fixture.repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-C", &source],
        &[
            ("GIT_AUTHOR_NAME", "Current Author"),
            ("GIT_AUTHOR_EMAIL", "current@example.com"),
            ("GIT_AUTHOR_DATE", "1700000200 +0000"),
        ],
    );
    let reuse_raw = fixture.cat_file("HEAD");
    assert_eq!(
        first_line_with(&reuse_raw, "author "),
        "author Source Author <source@example.com> 1700000100 +0200"
    );
    assert_eq!(
        message_body(&reuse_raw).trim(),
        "source subject\n\nsource body"
    );

    fs::write(fixture.repo.join("reedit.txt"), "reedit\n").expect("write reedit");
    fixture.success(&fixture.repo, &["add", "reedit.txt"]);
    fixture.success_env(
        &fixture.repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-c", &source],
        &[
            ("GIT_EDITOR", "true"),
            ("GIT_AUTHOR_NAME", "Current Author"),
            ("GIT_AUTHOR_EMAIL", "current@example.com"),
            ("GIT_AUTHOR_DATE", "1700000300 +0000"),
        ],
    );
    let reedit_raw = fixture.cat_file("HEAD");
    assert_eq!(
        first_line_with(&reedit_raw, "author "),
        "author Source Author <source@example.com> 1700000100 +0200"
    );
    assert_eq!(
        message_body(&reedit_raw).trim(),
        "source subject\n\nsource body"
    );
}
