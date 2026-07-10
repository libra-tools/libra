//! Checkout/switch branch start-point contracts for plan-20260708 P0-04.

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
        self.success(&self.repo, &["config", "set", "user.name", "Checkout Test"]);
        self.success(
            &self.repo,
            &["config", "set", "user.email", "checkout@example.com"],
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
        String::from_utf8(output.stdout)
            .expect("rev-parse output is utf8")
            .trim()
            .to_string()
    }

    fn symbolic_head(&self) -> String {
        let output = self.success(&self.repo, &["rev-parse", "--symbolic-full-name", "HEAD"]);
        String::from_utf8(output.stdout)
            .expect("symbolic head output is utf8")
            .trim()
            .to_string()
    }

    fn assert_on_branch(&self, branch: &str, expected_oid: &str, expected_file: &str) {
        assert_eq!(
            self.symbolic_head(),
            format!("refs/heads/{branch}"),
            "HEAD should remain a symbolic ref after branch creation"
        );
        assert_eq!(self.rev_parse("HEAD"), expected_oid);
        assert_eq!(self.rev_parse(branch), expected_oid);
        let contents = fs::read_to_string(self.repo.join("file.txt")).expect("read fixture file");
        assert_eq!(contents, expected_file);
    }
}

fn repo_with_base_and_tip() -> (CliFixture, String, String) {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let base = fixture.commit_file("base\n", "base");
    let tip = fixture.commit_file("tip\n", "tip");
    (fixture, base, tip)
}

#[test]
fn checkout_create_branch_from_startpoint_switches_symbolic_head() {
    let (fixture, base, _) = repo_with_base_and_tip();

    fixture.success(&fixture.repo, &["checkout", "-b", "topic", &base]);

    fixture.assert_on_branch("topic", &base, "base\n");
}

#[test]
fn checkout_force_create_from_startpoint_resets_and_switches_symbolic_head() {
    let (fixture, base, tip) = repo_with_base_and_tip();
    fixture.success(&fixture.repo, &["checkout", "-b", "topic"]);
    fixture.assert_on_branch("topic", &tip, "tip\n");
    fixture.success(&fixture.repo, &["checkout", "main"]);

    fixture.success(&fixture.repo, &["checkout", "-B", "topic", &base]);

    fixture.assert_on_branch("topic", &base, "base\n");
}

#[test]
fn checkout_force_create_invalid_startpoint_does_not_move_head_or_branch() {
    let (fixture, _, tip) = repo_with_base_and_tip();
    fixture.success(&fixture.repo, &["branch", "topic"]);
    let original_topic = fixture.rev_parse("topic");
    let original_head = fixture.rev_parse("HEAD");
    let original_symbolic_head = fixture.symbolic_head();

    let output = fixture.run(
        &fixture.repo,
        &["checkout", "-B", "topic", "definitely-missing"],
    );

    assert!(
        !output.status.success(),
        "invalid start-point must fail closed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.symbolic_head(), original_symbolic_head);
    assert_eq!(fixture.rev_parse("HEAD"), original_head);
    assert_eq!(fixture.rev_parse("topic"), original_topic);
    assert_eq!(original_head, tip);
}

#[test]
fn checkout_create_invalid_startpoint_does_not_create_branch_or_move_head() {
    let (fixture, _, tip) = repo_with_base_and_tip();
    let original_symbolic_head = fixture.symbolic_head();

    let output = fixture.run(
        &fixture.repo,
        &["checkout", "-b", "new-topic", "definitely-missing"],
    );

    assert!(
        !output.status.success(),
        "invalid start-point must fail closed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.symbolic_head(), original_symbolic_head);
    assert_eq!(fixture.rev_parse("HEAD"), tip);
    let missing_branch = fixture.run(&fixture.repo, &["rev-parse", "new-topic"]);
    assert!(
        !missing_branch.status.success(),
        "failed checkout -b must not create the target branch"
    );
}

#[test]
fn switch_force_create_from_startpoint_stays_on_symbolic_branch() {
    let (fixture, base, _) = repo_with_base_and_tip();

    fixture.success(&fixture.repo, &["switch", "-C", "switch-topic", &base]);

    fixture.assert_on_branch("switch-topic", &base, "base\n");
}
