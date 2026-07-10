//! Shallow clone/fetch integrity contracts for plan-20260708 P0-03.

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
}

impl CliFixture {
    fn new() -> Self {
        let temp = tempdir().expect("create tempdir");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        fs::create_dir_all(&home).expect("create isolated home");
        Self {
            _temp: temp,
            root,
            home,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
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

    fn create_source_repo(&self) -> PathBuf {
        let source = self.path("source");
        fs::create_dir_all(&source).expect("create source dir");
        self.success(&self.root, &["init", source.to_str().expect("utf8 source")]);
        self.success(&source, &["config", "set", "user.name", "Shallow Test"]);
        self.success(
            &source,
            &["config", "set", "user.email", "shallow@example.com"],
        );
        for (name, contents) in [("base", "one\n"), ("tip", "two\n")] {
            fs::write(source.join("file.txt"), contents).expect("write source file");
            self.success(&source, &["add", "file.txt"]);
            self.success(&source, &["commit", "-s", "-m", name]);
        }
        source
    }
}

fn assert_lbr_repo_002(output: &Output, context: &str) {
    assert_eq!(
        output.status.code(),
        Some(128),
        "{context} should fail closed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error-Code: LBR-REPO-002"),
        "{context} should be classified as repository integrity failure:\n{stderr}"
    );
    assert!(
        stderr.contains("local Libra remotes do not support --depth"),
        "{context} should explain the unsupported shallow boundary source:\n{stderr}"
    );
}

#[test]
fn clone_depth_from_local_libra_source_fails_without_broken_target() {
    let fixture = CliFixture::new();
    let source = fixture.create_source_repo();
    let dest = fixture.path("clone-depth");
    let remote = format!("file://{}", source.display());

    let output = fixture.run(
        &fixture.root,
        &[
            "clone",
            "--depth",
            "1",
            &remote,
            dest.to_str().expect("utf8 dest"),
        ],
    );

    assert_lbr_repo_002(&output, "clone --depth from local Libra source");
    assert!(
        !dest.join(".libra").exists(),
        "failed shallow clone must not leave an initialized target"
    );
}

#[test]
fn fetch_depth_from_local_libra_remote_fails_before_shallow_metadata() {
    let fixture = CliFixture::new();
    let source = fixture.create_source_repo();
    let repo = fixture.path("fetch-target");
    fs::create_dir_all(&repo).expect("create fetch target");
    fixture.success(&fixture.root, &["init", repo.to_str().expect("utf8 repo")]);
    fixture.success(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            source.to_str().expect("utf8 source"),
        ],
    );

    let output = fixture.run(&repo, &["fetch", "origin", "--depth", "1"]);

    assert_lbr_repo_002(&output, "fetch --depth from local Libra remote");
    assert!(
        !repo.join(".libra").join("shallow").exists(),
        "failed shallow fetch must not write shallow metadata"
    );
    let shallow = fixture.success(&repo, &["rev-parse", "--is-shallow-repository"]);
    assert_eq!(String::from_utf8_lossy(&shallow.stdout), "false\n");
}

#[test]
fn rev_parse_reports_shallow_repository_boolean() {
    let fixture = CliFixture::new();
    let source = fixture.create_source_repo();

    let before = fixture.success(&source, &["rev-parse", "--is-shallow-repository"]);
    assert_eq!(String::from_utf8_lossy(&before.stdout), "false\n");

    let head = fixture.success(&source, &["rev-parse", "HEAD"]);
    let head = String::from_utf8(head.stdout).expect("head is utf8");
    fs::write(source.join(".libra").join("shallow"), head).expect("write shallow metadata");

    let after = fixture.success(&source, &["rev-parse", "--is-shallow-repository"]);
    assert_eq!(String::from_utf8_lossy(&after.stdout), "true\n");
}
