//! Orphan branch root-commit contracts for plan-20260708 P0-05.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

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

    fn success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.run(cwd, args);
        assert!(
            output.status.success(),
            "{} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn init_repo(&self) {
        fs::create_dir_all(&self.repo).expect("create repo dir");
        self.success(
            &self.root,
            &["init", self.repo.to_str().expect("utf8 repo")],
        );
        self.success(&self.repo, &["config", "set", "user.name", "Orphan Test"]);
        self.success(
            &self.repo,
            &["config", "set", "user.email", "orphan@example.com"],
        );
    }

    fn commit_file(&self, contents: &str, message: &str) -> String {
        fs::write(self.repo.join("file.txt"), contents).expect("write fixture file");
        self.success(&self.repo, &["add", "file.txt"]);
        self.success(&self.repo, &["commit", "-s", "-m", message]);
        self.rev_parse("HEAD")
    }

    fn rev_parse(&self, spec: &str) -> String {
        let output = self.success(&self.repo, &["rev-parse", spec]);
        stdout_trim(&output)
    }

    fn symbolic_head(&self) -> String {
        let output = self.success(&self.repo, &["rev-parse", "--symbolic-full-name", "HEAD"]);
        stdout_trim(&output)
    }

    fn assert_orphan_first_commit(&self, branch: &str, message: &str) {
        assert_eq!(
            self.symbolic_head(),
            format!("refs/heads/{branch}"),
            "HEAD should point at the unborn orphan branch"
        );
        let unborn_head = self.run(&self.repo, &["rev-parse", "HEAD"]);
        assert!(
            !unborn_head.status.success(),
            "orphan HEAD should not resolve before the first user commit\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&unborn_head.stdout),
            String::from_utf8_lossy(&unborn_head.stderr)
        );
        assert_eq!(
            fs::read_to_string(self.repo.join("file.txt")).expect("read retained worktree file"),
            "base\n",
            "orphan switch should preserve the working tree"
        );

        self.success(&self.repo, &["commit", "-s", "-m", message]);

        let count = self.success(&self.repo, &["rev-list", "--count", "HEAD"]);
        assert_eq!(stdout_trim(&count), "1");
        let parents = self.success(&self.repo, &["log", "--pretty=%P", "-1"]);
        assert_eq!(
            stdout_trim(&parents),
            "",
            "first commit on an orphan branch must have no parents"
        );
        let log = self.success(&self.repo, &["log", "--oneline"]);
        assert!(
            !String::from_utf8_lossy(&log.stdout).contains("orphan branch root commit"),
            "orphan flow must not create Libra's historical placeholder commit"
        );
    }
}

fn stdout_trim(output: &Output) -> String {
    String::from_utf8(output.stdout.clone())
        .expect("stdout is utf8")
        .trim()
        .to_string()
}

fn repo_with_base() -> CliFixture {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.commit_file("base\n", "base");
    fixture
}

#[test]
fn switch_orphan_keeps_unborn_head_until_first_root_commit() {
    let fixture = repo_with_base();

    fixture.success(&fixture.repo, &["switch", "--orphan", "fresh"]);

    fixture.assert_orphan_first_commit("fresh", "fresh root");
}

#[test]
fn checkout_orphan_uses_same_unborn_root_semantics() {
    let fixture = repo_with_base();

    fixture.success(&fixture.repo, &["checkout", "--orphan", "fresh"]);

    fixture.assert_orphan_first_commit("fresh", "checkout root");
}

#[test]
fn switch_orphan_existing_branch_fails_without_deleting_or_moving_head() {
    let fixture = repo_with_base();
    fixture.success(&fixture.repo, &["branch", "existing"]);
    let main_oid = fixture.rev_parse("HEAD");
    let existing_oid = fixture.rev_parse("existing");
    let original_head = fixture.symbolic_head();

    let output = fixture.run(&fixture.repo, &["switch", "--orphan", "existing"]);

    assert!(
        !output.status.success(),
        "orphan to existing branch must fail closed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.symbolic_head(), original_head);
    assert_eq!(fixture.rev_parse("HEAD"), main_oid);
    assert_eq!(fixture.rev_parse("existing"), existing_oid);
}

#[test]
fn switch_orphan_rejects_startpoint_without_moving_head() {
    let fixture = repo_with_base();
    let main_oid = fixture.rev_parse("HEAD");
    let original_head = fixture.symbolic_head();

    let output = fixture.run(&fixture.repo, &["switch", "--orphan", "fresh", "main"]);

    assert!(
        !output.status.success(),
        "switch --orphan start-point is unsupported and must fail closed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.symbolic_head(), original_head);
    assert_eq!(fixture.rev_parse("HEAD"), main_oid);
    let missing_branch = fixture.run(&fixture.repo, &["rev-parse", "fresh"]);
    assert!(
        !missing_branch.status.success(),
        "failed switch --orphan must not create the target branch"
    );
}

#[test]
fn checkout_orphan_rejects_startpoint_without_moving_head() {
    let fixture = repo_with_base();
    let main_oid = fixture.rev_parse("HEAD");
    let original_head = fixture.symbolic_head();

    let output = fixture.run(&fixture.repo, &["checkout", "--orphan", "fresh", "main"]);

    assert!(
        !output.status.success(),
        "checkout --orphan start-point is unsupported and must fail closed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.symbolic_head(), original_head);
    assert_eq!(fixture.rev_parse("HEAD"), main_oid);
}

#[test]
fn switch_orphan_json_reports_unborn_state() {
    let fixture = repo_with_base();

    let output = fixture.success(&fixture.repo, &["--json", "switch", "--orphan", "fresh"]);
    let json: Value = serde_json::from_slice(&output.stdout).expect("switch json output");
    let data = &json["data"];

    assert_eq!(json["command"], "switch");
    assert_eq!(data["branch"], "fresh");
    assert_eq!(data["created"], true);
    assert_eq!(data["detached"], false);
    assert_eq!(data["unborn"], true);
    let commit = data["commit"].as_str().expect("commit string");
    assert!(
        commit.chars().all(|ch| ch == '0'),
        "unborn switch should report an all-zero commit placeholder: {commit}"
    );
    assert!(
        !fixture
            .run(&fixture.repo, &["rev-parse", "HEAD"])
            .status
            .success(),
        "JSON orphan switch must leave HEAD unborn until first commit"
    );
}
