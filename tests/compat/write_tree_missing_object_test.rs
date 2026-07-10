//! Object-integrity guards for plan-20260708 P0-09.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::{TempDir, tempdir};

const MISSING_BLOB: &str = "1111111111111111111111111111111111111111";

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
        self.success(&self.repo, &["config", "set", "user.name", "Test User"]);
        self.success(
            &self.repo,
            &["config", "set", "user.email", "test@example.com"],
        );
    }

    fn rev_parse(&self, spec: &str) -> String {
        stdout_trim(&self.success(&self.repo, &["rev-parse", spec]))
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

fn assert_repo_corrupt(args: &[&str], output: &Output) {
    assert!(
        !output.status.success(),
        "{} unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LBR-REPO-002"),
        "expected LBR-REPO-002 for {}, got stderr:\n{}",
        args.join(" "),
        stderr
    );
}

fn stdout_trim(output: &Output) -> String {
    String::from_utf8(output.stdout.clone())
        .expect("stdout is utf8")
        .trim()
        .to_string()
}

#[test]
fn write_tree_rejects_missing_index_blob() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    fixture.success(
        &fixture.repo,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{MISSING_BLOB},missing.txt"),
        ],
    );
    let output = fixture.run(&fixture.repo, &["write-tree"]);
    assert_repo_corrupt(&["write-tree"], &output);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("missing or unreadable blob object"),
        "missing-object diagnostic should name the blob contract:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn write_tree_rejects_wrong_index_object_type() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    let tree_id = stdout_trim(&fixture.success(&fixture.repo, &["write-tree"]));
    fixture.success(
        &fixture.repo,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{tree_id},tree-as-blob.txt"),
        ],
    );
    let output = fixture.run(&fixture.repo, &["write-tree"]);
    assert_repo_corrupt(&["write-tree"], &output);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("expected blob object but found tree"),
        "wrong-type diagnostic should name expected and actual types:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn commit_rejects_missing_index_blob_and_leaves_head_unchanged() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    fs::write(fixture.repo.join("base.txt"), "base\n").expect("write base");
    fixture.success(&fixture.repo, &["add", "base.txt"]);
    fixture.success(
        &fixture.repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "base"],
    );
    let before = fixture.rev_parse("HEAD");

    fixture.success(
        &fixture.repo,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{MISSING_BLOB},missing.txt"),
        ],
    );
    let output = fixture.run(
        &fixture.repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "broken"],
    );
    assert_repo_corrupt(
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "broken"],
        &output,
    );
    assert_eq!(
        fixture.rev_parse("HEAD"),
        before,
        "failed integrity precheck must not move HEAD"
    );
}
