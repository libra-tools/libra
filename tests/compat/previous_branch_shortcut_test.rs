//! Previous checkout target contracts for plan-20260708 P1-12.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
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
        self.success(&self.repo, &["config", "set", "user.name", "Shortcut Test"]);
        self.success(
            &self.repo,
            &["config", "set", "user.email", "shortcut@example.com"],
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

    fn symbolic_head(&self) -> Option<String> {
        let output = self.run(&self.repo, &["rev-parse", "--symbolic-full-name", "HEAD"]);
        if !output.status.success() {
            return None;
        }
        let name = String::from_utf8(output.stdout)
            .expect("symbolic HEAD output is utf8")
            .trim()
            .to_string();
        (name != "HEAD").then_some(name)
    }

    fn assert_branch(&self, branch: &str, oid: &str, contents: &str) {
        assert_eq!(
            self.symbolic_head().as_deref(),
            Some(format!("refs/heads/{branch}").as_str())
        );
        assert_eq!(self.rev_parse("HEAD"), oid);
        assert_eq!(
            fs::read_to_string(self.repo.join("file.txt")).expect("read fixture file"),
            contents
        );
    }

    fn assert_detached(&self, oid: &str, contents: &str) {
        assert_eq!(self.symbolic_head(), None);
        assert_eq!(self.rev_parse("HEAD"), oid);
        assert_eq!(
            fs::read_to_string(self.repo.join("file.txt")).expect("read fixture file"),
            contents
        );
    }
}

fn repo_with_topic() -> (CliFixture, String, String) {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let main = fixture.commit_file("main\n", "main");
    fixture.success(&fixture.repo, &["switch", "-c", "topic"]);
    let topic = fixture.commit_file("topic\n", "topic");
    (fixture, main, topic)
}

fn parse_json_error(output: &Output) -> Value {
    assert!(!output.status.success(), "command should fail");
    assert!(output.stdout.is_empty(), "JSON errors keep stdout empty");
    serde_json::from_slice(&output.stderr).expect("JSON error on stderr")
}

#[test]
fn switch_dash_toggles_between_local_branches() {
    let (fixture, main, topic) = repo_with_topic();

    let first = fixture.success(&fixture.repo, &["--json", "switch", "-"]);
    let first: Value = serde_json::from_slice(&first.stdout).expect("switch JSON output");
    assert_eq!(first["data"]["branch"], "main");
    fixture.assert_branch("main", &main, "main\n");

    fixture.success(&fixture.repo, &["switch", "-"]);
    fixture.assert_branch("topic", &topic, "topic\n");
}

#[test]
fn previous_branch_uses_its_current_tip() {
    let (fixture, _, topic) = repo_with_topic();
    fixture.success(&fixture.repo, &["branch", "reset", "main", "HEAD"]);

    fixture.success(&fixture.repo, &["switch", "-"]);

    fixture.assert_branch("main", &topic, "topic\n");
}

#[test]
fn checkout_dash_toggles_and_records_checkout_navigation() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let main = fixture.commit_file("main\n", "main");
    fixture.success(&fixture.repo, &["checkout", "-b", "topic"]);
    let topic = fixture.commit_file("topic\n", "topic");

    let first = fixture.success(&fixture.repo, &["--json", "checkout", "-"]);
    let first: Value = serde_json::from_slice(&first.stdout).expect("checkout JSON output");
    assert_eq!(first["data"]["branch"], "main");
    fixture.assert_branch("main", &main, "main\n");

    fixture.success(&fixture.repo, &["checkout", "-"]);
    fixture.assert_branch("topic", &topic, "topic\n");

    let reflog = fixture.success(&fixture.repo, &["reflog", "show", "HEAD"]);
    assert!(
        String::from_utf8_lossy(&reflog.stdout).contains("checkout: moving from main to topic"),
        "checkout movements must be distinguishable in the HEAD reflog"
    );
}

#[test]
fn shortcut_history_is_shared_across_checkout_and_switch() {
    let (fixture, main, topic) = repo_with_topic();

    fixture.success(&fixture.repo, &["checkout", "-"]);
    fixture.assert_branch("main", &main, "main\n");

    fixture.success(&fixture.repo, &["switch", "-"]);
    fixture.assert_branch("topic", &topic, "topic\n");
}

#[test]
fn shortcut_toggles_between_branch_and_detached_head() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let base = fixture.commit_file("base\n", "base");
    let tip = fixture.commit_file("tip\n", "tip");

    fixture.success(&fixture.repo, &["switch", "--detach", &base]);
    fixture.assert_detached(&base, "base\n");

    fixture.success(&fixture.repo, &["checkout", "-"]);
    fixture.assert_branch("main", &tip, "tip\n");

    fixture.success(&fixture.repo, &["switch", "-"]);
    fixture.assert_detached(&base, "base\n");
}

#[test]
fn shortcut_without_navigation_history_fails_without_moving_head() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let head = fixture.commit_file("only\n", "only");
    let symbolic_head = fixture.symbolic_head();

    for command in ["switch", "checkout"] {
        let output = fixture.run(&fixture.repo, &["--json", command, "-"]);
        let error = parse_json_error(&output);
        assert_eq!(output.status.code(), Some(129));
        assert_eq!(error["error_code"], "LBR-CLI-003");
        assert_eq!(error["message"], "no previous checkout target is available");
        assert_eq!(fixture.symbolic_head(), symbolic_head);
        assert_eq!(fixture.rev_parse("HEAD"), head);
        assert_eq!(
            fs::read_to_string(fixture.repo.join("file.txt")).expect("read fixture file"),
            "only\n"
        );
    }
}

#[test]
fn deleted_latest_previous_branch_fails_closed_without_falling_back() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let head = fixture.commit_file("base\n", "base");
    fixture.success(&fixture.repo, &["switch", "-c", "old-topic"]);
    fixture.success(&fixture.repo, &["switch", "-c", "current-topic"]);
    fixture.success(&fixture.repo, &["branch", "-D", "old-topic"]);
    let symbolic_head = fixture.symbolic_head();

    let output = fixture.run(&fixture.repo, &["--json", "switch", "-"]);
    let error = parse_json_error(&output);
    assert_eq!(error["error_code"], "LBR-CLI-003");
    assert_eq!(error["message"], "no previous checkout target is available");
    assert_eq!(fixture.symbolic_head(), symbolic_head);
    assert_eq!(fixture.rev_parse("HEAD"), head);
}

#[tokio::test]
async fn malformed_latest_navigation_record_fails_closed() {
    let (fixture, _, topic) = repo_with_topic();
    let symbolic_head = fixture.symbolic_head();
    let db_url = format!(
        "sqlite://{}?mode=rwc",
        fixture.repo.join(".libra/libra.db").display()
    );
    let db = Database::connect(db_url).await.expect("open repository DB");
    db.execute_raw(Statement::from_string(
        DbBackend::Sqlite,
        "UPDATE reflog SET message = 'malformed movement' \
         WHERE id = (SELECT id FROM reflog WHERE ref_name = 'HEAD' \
         AND action IN ('switch', 'checkout') ORDER BY timestamp DESC, id DESC LIMIT 1)"
            .to_string(),
    ))
    .await
    .expect("corrupt latest navigation record");
    db.close().await.expect("close repository DB");

    let output = fixture.run(&fixture.repo, &["--json", "checkout", "-"]);
    let error = parse_json_error(&output);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(error["error_code"], "LBR-REPO-002");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("malformed movement message")
    );
    assert_eq!(fixture.symbolic_head(), symbolic_head);
    assert_eq!(fixture.rev_parse("HEAD"), topic);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt")).expect("read fixture file"),
        "topic\n"
    );
}

#[test]
fn linked_worktree_does_not_consume_main_worktree_navigation_history() {
    let (fixture, _, topic) = repo_with_topic();
    let linked = fixture.root.join("linked");
    fixture.success(
        &fixture.repo,
        &[
            "worktree",
            "add",
            linked.to_str().expect("utf8 linked path"),
        ],
    );

    let output = fixture.run(&linked, &["--json", "switch", "-"]);
    let error = parse_json_error(&output);
    assert_eq!(error["error_code"], "LBR-CLI-003");

    let head = fixture.success(&linked, &["rev-parse", "HEAD"]);
    assert_eq!(
        String::from_utf8(head.stdout)
            .expect("linked HEAD output is utf8")
            .trim(),
        topic
    );
    let symbolic = fixture.success(&linked, &["rev-parse", "--symbolic-full-name", "HEAD"]);
    assert_eq!(
        String::from_utf8(symbolic.stdout)
            .expect("linked symbolic HEAD output is utf8")
            .trim(),
        "HEAD",
        "new linked worktree should remain detached"
    );
}
