//! Symlink compatibility guards for plan-20260708 P0-11.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
#[cfg(not(unix))]
use std::{io::Write, process::Stdio};

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

    #[cfg(not(unix))]
    fn run_with_stdin(&self, cwd: &Path, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = self
            .command(cwd, args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn libra with stdin");
        child
            .stdin
            .as_mut()
            .expect("stdin is piped")
            .write_all(stdin)
            .expect("write stdin");
        child.wait_with_output().expect("wait for libra")
    }

    fn success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.run(cwd, args);
        assert_success(args, &output);
        output
    }

    fn stdout(&self, args: &[&str]) -> String {
        stdout_text(&self.success(&self.repo, args))
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
        self.success(&self.repo, &["config", "set", "user.name", "Symlink Test"]);
        self.success(
            &self.repo,
            &["config", "set", "user.email", "symlink@example.com"],
        );
    }

    #[cfg(unix)]
    fn write_link(&self, target: &str) {
        use std::os::unix::fs::symlink;

        let link = self.repo.join("link");
        if fs::symlink_metadata(&link).is_ok() {
            fs::remove_file(&link).expect("remove existing link");
        }
        symlink(target, &link).expect("create symlink");
    }

    #[cfg(unix)]
    fn add_link(&self, target: &str) {
        self.write_link(target);
        self.success(&self.repo, &["add", "link"]);
    }

    fn commit_all(&self, message: &str) {
        self.success(
            &self.repo,
            &["commit", "--no-gpg-sign", "--no-verify", "-m", message],
        );
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

fn stdout_text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is utf8")
}

#[cfg(not(unix))]
fn stdout_trim(output: &Output) -> String {
    stdout_text(output).trim().to_string()
}

fn staged_link_oid(fixture: &CliFixture) -> String {
    let output = fixture.stdout(&["ls-files", "-s", "link"]);
    let line = output
        .lines()
        .find(|line| line.ends_with("\tlink"))
        .unwrap_or_else(|| panic!("missing staged link row:\n{output}"));
    let (meta, path) = line.split_once('\t').expect("stage row has tab");
    assert_eq!(path, "link");
    let fields = meta.split_whitespace().collect::<Vec<_>>();
    assert_eq!(fields[0], "120000", "symlink mode must be stored");
    assert_eq!(fields[2], "0", "symlink row should use stage 0");
    fields[1].to_string()
}

#[cfg(unix)]
#[test]
fn add_symlink_stores_mode_and_target_blob() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    fixture.add_link("target.txt");

    let oid = staged_link_oid(&fixture);
    let blob = fixture.success(&fixture.repo, &["cat-file", "-p", &oid]);
    assert_eq!(blob.stdout, b"target.txt");
}

#[cfg(unix)]
#[test]
fn add_renormalize_dry_run_reports_symlink_without_staging() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.add_link("target.txt");
    fixture.commit_all("add symlink");
    fixture.write_link("other.txt");

    let preview = fixture.stdout(&["add", "--renormalize", "--dry-run", "link"]);
    assert!(
        preview.lines().any(|line| line == "add: link"),
        "dry-run should report the symlink that would be re-staged:\n{preview}"
    );
    assert!(
        preview.contains("(dry run, no files were staged)"),
        "dry-run footer should confirm no staging occurred:\n{preview}"
    );

    let oid = staged_link_oid(&fixture);
    let blob = fixture.success(&fixture.repo, &["cat-file", "-p", &oid]);
    assert_eq!(
        blob.stdout, b"target.txt",
        "dry-run must not rewrite the index"
    );
}

#[cfg(unix)]
#[test]
fn checkout_restores_tracked_symlink_as_symlink() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.add_link("target.txt");
    fixture.commit_all("add symlink");
    fs::remove_file(fixture.repo.join("link")).expect("remove worktree symlink");

    fixture.success(&fixture.repo, &["checkout", "HEAD", "--", "link"]);

    let metadata = fs::symlink_metadata(fixture.repo.join("link")).expect("stat restored link");
    assert!(
        metadata.file_type().is_symlink(),
        "checkout must restore a real symlink"
    );
    assert_eq!(
        fs::read_link(fixture.repo.join("link")).expect("read restored symlink"),
        PathBuf::from("target.txt")
    );
}

#[cfg(unix)]
#[test]
fn restore_ours_preserves_symlink_conflict_stage_mode() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.add_link("base.txt");
    fixture.commit_all("add base symlink");

    fixture.success(&fixture.repo, &["checkout", "-b", "ours"]);
    fixture.add_link("ours.txt");
    fixture.commit_all("ours symlink");

    fixture.success(&fixture.repo, &["checkout", "-b", "theirs", "HEAD~1"]);
    fixture.add_link("theirs.txt");
    fixture.commit_all("theirs symlink");

    fixture.success(&fixture.repo, &["checkout", "ours"]);
    let merge = fixture.run(&fixture.repo, &["merge", "theirs"]);
    assert!(
        !merge.status.success(),
        "merge should leave a symlink conflict\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&merge.stdout),
        String::from_utf8_lossy(&merge.stderr)
    );

    fixture.success(&fixture.repo, &["restore", "--ours", "link"]);

    let metadata = fs::symlink_metadata(fixture.repo.join("link")).expect("stat restored side");
    assert!(
        metadata.file_type().is_symlink(),
        "restore --ours must restore the symlink stage as a symlink"
    );
    assert_eq!(
        fs::read_link(fixture.repo.join("link")).expect("read restored side"),
        PathBuf::from("ours.txt")
    );
}

#[cfg(unix)]
#[test]
fn restore_merge_does_not_write_conflict_markers_through_symlink() {
    use std::os::unix::fs::symlink;

    let fixture = CliFixture::new();
    fixture.init_repo();
    fs::write(fixture.repo.join("file.txt"), "base\n").expect("write base");
    fixture.success(&fixture.repo, &["add", "file.txt"]);
    fixture.commit_all("add base file");

    fixture.success(&fixture.repo, &["checkout", "-b", "ours"]);
    fs::write(fixture.repo.join("file.txt"), "ours\n").expect("write ours");
    fixture.success(&fixture.repo, &["add", "file.txt"]);
    fixture.commit_all("ours file");

    fixture.success(&fixture.repo, &["checkout", "-b", "theirs", "HEAD~1"]);
    fs::write(fixture.repo.join("file.txt"), "theirs\n").expect("write theirs");
    fixture.success(&fixture.repo, &["add", "file.txt"]);
    fixture.commit_all("theirs file");

    fixture.success(&fixture.repo, &["checkout", "ours"]);
    let merge = fixture.run(&fixture.repo, &["merge", "theirs"]);
    assert!(
        !merge.status.success(),
        "merge should leave a text conflict\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&merge.stdout),
        String::from_utf8_lossy(&merge.stderr)
    );

    let outside = fixture.root.join("outside.txt");
    fs::write(&outside, "outside\n").expect("write outside sentinel");
    fs::remove_file(fixture.repo.join("file.txt")).expect("remove conflict file");
    symlink(&outside, fixture.repo.join("file.txt")).expect("replace conflict file with symlink");

    fixture.success(&fixture.repo, &["restore", "--merge", "file.txt"]);

    assert_eq!(
        fs::read_to_string(&outside).expect("read outside sentinel"),
        "outside\n",
        "restore --merge must not write through the worktree symlink"
    );
    let metadata =
        fs::symlink_metadata(fixture.repo.join("file.txt")).expect("stat restored conflict file");
    assert!(
        !metadata.file_type().is_symlink(),
        "restore --merge should replace a worktree symlink with a regular conflict file"
    );
    let content =
        fs::read_to_string(fixture.repo.join("file.txt")).expect("read restored conflict file");
    assert!(
        content.contains("<<<<<<< ours") && content.contains(">>>>>>> theirs"),
        "restore --merge should write conflict markers into the worktree file:\n{content}"
    );
}

#[cfg(unix)]
#[test]
fn status_and_diff_detect_symlink_target_changes() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.add_link("target.txt");
    fixture.commit_all("add symlink");

    fixture.write_link("other.txt");

    let status = fixture.stdout(&["status", "--porcelain"]);
    assert!(
        status.lines().any(|line| line == " M link"),
        "status must report a changed symlink target:\n{status}"
    );
    let deleted = fixture.stdout(&["ls-files", "--deleted", "link"]);
    assert!(
        deleted.trim().is_empty(),
        "dangling tracked symlink must not be listed as deleted:\n{deleted}"
    );
    let modified = fixture.stdout(&["ls-files", "--modified", "link"]);
    assert_eq!(
        modified.trim(),
        "link",
        "ls-files --modified should report changed symlink target"
    );
    let diff = fixture.stdout(&["diff", "--", "link"]);
    assert!(
        diff.contains("-target.txt"),
        "diff should show the old symlink target:\n{diff}"
    );
    assert!(
        diff.contains("+other.txt"),
        "diff should show the new symlink target:\n{diff}"
    );
}

#[cfg(unix)]
#[test]
fn reset_hard_restores_tracked_symlink_as_symlink() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.add_link("target.txt");
    fixture.commit_all("add symlink");
    fs::write(fixture.repo.join("link"), "target.txt").expect("replace symlink with regular file");

    fixture.success(&fixture.repo, &["reset", "--hard", "HEAD"]);

    let metadata = fs::symlink_metadata(fixture.repo.join("link")).expect("stat reset link");
    assert!(
        metadata.file_type().is_symlink(),
        "reset --hard must restore a real symlink"
    );
    assert_eq!(
        fs::read_link(fixture.repo.join("link")).expect("read reset symlink"),
        PathBuf::from("target.txt")
    );
}

#[cfg(unix)]
#[test]
fn reset_pathspec_restores_symlink_index_mode() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    fixture.add_link("target.txt");
    fixture.commit_all("add symlink");
    fixture.add_link("other.txt");

    fixture.success(&fixture.repo, &["reset", "HEAD", "--", "link"]);

    let oid = staged_link_oid(&fixture);
    let blob = fixture.success(&fixture.repo, &["cat-file", "-p", &oid]);
    assert_eq!(blob.stdout, b"target.txt");
}

#[cfg(not(unix))]
#[test]
fn symlink_checkout_reports_platform_diagnostic() {
    let fixture = CliFixture::new();
    fixture.init_repo();
    let hash = stdout_trim(&fixture.run_with_stdin(
        &fixture.repo,
        &["hash-object", "-w", "--stdin"],
        b"target.txt",
    ));
    fixture.success(
        &fixture.repo,
        &[
            "update-index",
            "--cacheinfo",
            &format!("120000,{hash},link"),
        ],
    );
    fixture.commit_all("add symlink");

    let output = fixture.run(
        &fixture.repo,
        &["restore", "--source", "HEAD", "--worktree", "link"],
    );

    assert!(
        !output.status.success(),
        "non-Unix symlink checkout must fail explicitly\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("symlink checkout is not supported"),
        "diagnostic should name the unsupported symlink checkout:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
