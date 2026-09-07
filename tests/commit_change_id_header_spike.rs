//! OL-00: compare Git commit-header and sidecar-only Change ID persistence.
//!
//! This spike intentionally exercises only real Git plumbing. It does not
//! choose a production serialization format or touch Libra's implementation.
//! The header path is tested through object creation, fsck, push, and clone;
//! the sidecar path is tested by keeping the Change ID outside the Git object
//! while proving that the referenced commit/tree/blob closure remains
//! enumerable through ordinary Git commands.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use tempfile::{TempDir, tempdir};

const GIT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const CHANGE_ID: &str = "0123456789abcdef0123456789abcdef";

struct GitFixture {
    _temp: TempDir,
    root: PathBuf,
}

impl GitFixture {
    fn new() -> Self {
        let temp = tempdir().expect("create spike tempdir");
        let root = temp.path().to_path_buf();
        Self { _temp: temp, root }
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", GIT_PATH)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .output()
            .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")))
    }

    fn git_with_stdin(&self, cwd: &Path, args: &[&str], input: &str) -> Output {
        let mut child = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", GIT_PATH)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
        child
            .stdin
            .take()
            .expect("git stdin")
            .write_all(input.as_bytes())
            .expect("write git stdin");
        child
            .wait_with_output()
            .unwrap_or_else(|error| panic!("wait for git {}: {error}", args.join(" ")))
    }

    fn git_success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.git(cwd, args);
        assert_success(args, &output);
        output
    }

    fn git_stdin_success(&self, cwd: &Path, args: &[&str], input: &str) -> Output {
        let output = self.git_with_stdin(cwd, args, input);
        assert_success(args, &output);
        output
    }

    fn init_repo(&self, name: &str) -> PathBuf {
        let repo = self.root.join(name);
        fs::create_dir_all(&repo).expect("create Git repository directory");
        self.git_success(&repo, &["init", "--quiet"]);
        self.git_success(&repo, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        self.git_success(&repo, &["config", "user.name", "Change ID Spike"]);
        self.git_success(&repo, &["config", "user.email", "spike@example.com"]);
        repo
    }
}

fn assert_success(args: &[&str], output: &Output) {
    assert!(
        output.status.success(),
        "git {} failed (status {})\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout_trim(output: Output) -> String {
    String::from_utf8(output.stdout)
        .expect("Git stdout is UTF-8")
        .trim()
        .to_owned()
}

fn require_git_available() {
    let available = Command::new("git")
        .arg("--version")
        .env_clear()
        .env("PATH", GIT_PATH)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available,
        "Git is required for the OL-00 compatibility spike (searched PATH={GIT_PATH})"
    );
}

/// A nonstandard `change-id` commit header is accepted by Git's object
/// parser, remains readable, and survives a local push/clone round trip.
#[test]
fn git_change_id_header_survives_fsck_push_and_clone() {
    require_git_available();

    let fixture = GitFixture::new();
    let repo = fixture.init_repo("header-source");
    fs::write(repo.join("tracked.txt"), "header spike\n").expect("write tracked file");
    fixture.git_success(&repo, &["add", "tracked.txt"]);
    let tree_oid = stdout_trim(fixture.git_success(&repo, &["write-tree"]));
    let commit_body = format!(
        "tree {tree_oid}\nauthor Change ID Spike <spike@example.com> 1700000000 +0000\ncommitter Change ID Spike <spike@example.com> 1700000000 +0000\nchange-id {CHANGE_ID}\n\nheader-bearing commit\n"
    );
    let commit_oid = stdout_trim(fixture.git_stdin_success(
        &repo,
        &["hash-object", "-w", "-t", "commit", "--stdin"],
        &commit_body,
    ));
    fixture.git_success(&repo, &["update-ref", "refs/heads/main", &commit_oid]);

    let raw_commit = String::from_utf8(
        fixture
            .git_success(&repo, &["cat-file", "-p", "HEAD"])
            .stdout,
    )
    .expect("raw Git commit is UTF-8");
    assert!(
        raw_commit.contains(&format!("change-id {CHANGE_ID}")),
        "Git must return the custom Change ID header through cat-file:\n{raw_commit}"
    );
    fixture.git_success(&repo, &["fsck", "--full", "--no-reflogs"]);

    let bare = fixture.root.join("header-remote.git");
    fixture.git_success(
        &fixture.root,
        &["init", "--bare", "--quiet", "header-remote.git"],
    );
    let bare_url = bare.to_str().expect("bare repository path is UTF-8");
    fixture.git_success(&repo, &["remote", "add", "origin", bare_url]);
    fixture.git_success(
        &repo,
        &[
            "push",
            "--quiet",
            "origin",
            "refs/heads/main:refs/heads/main",
        ],
    );

    let clone = fixture.root.join("header-clone");
    fixture.git_success(
        &fixture.root,
        &[
            "clone",
            "--quiet",
            "--branch",
            "main",
            bare_url,
            "header-clone",
        ],
    );
    let cloned_commit = String::from_utf8(
        fixture
            .git_success(&clone, &["cat-file", "-p", "HEAD"])
            .stdout,
    )
    .expect("cloned raw Git commit is UTF-8");
    assert!(
        cloned_commit.contains(&format!("change-id {CHANGE_ID}")),
        "push/clone must not corrupt the custom Change ID header:\n{cloned_commit}"
    );
    fixture.git_success(&clone, &["fsck", "--full", "--no-reflogs"]);
}

/// A sidecar can carry the logical identity without changing the Git commit
/// object, while the commit's ordinary Git object closure remains enumerable.
#[test]
fn sidecar_only_keeps_git_object_closure_enumerable() {
    require_git_available();

    let fixture = GitFixture::new();
    let repo = fixture.init_repo("sidecar-source");
    fs::write(repo.join("tracked.txt"), "sidecar spike\n").expect("write tracked file");
    fixture.git_success(&repo, &["add", "tracked.txt"]);
    fixture.git_success(
        &repo,
        &["commit", "--quiet", "--no-gpg-sign", "-m", "sidecar commit"],
    );

    let commit_oid = stdout_trim(fixture.git_success(&repo, &["rev-parse", "HEAD"]));
    let tree_oid = stdout_trim(fixture.git_success(&repo, &["rev-parse", "HEAD^{tree}"]));
    let blob_oid = stdout_trim(fixture.git_success(&repo, &["rev-parse", "HEAD:tracked.txt"]));
    let commit_body_before = String::from_utf8(
        fixture
            .git_success(&repo, &["cat-file", "-p", "HEAD"])
            .stdout,
    )
    .expect("original Git commit is UTF-8");
    let sidecar = repo.join(".libra-change-id");
    fs::write(
        &sidecar,
        format!("change_id={CHANGE_ID}\ncommit_oid={commit_oid}\n"),
    )
    .expect("write Change ID sidecar");

    let sidecar_body = fs::read_to_string(&sidecar).expect("read Change ID sidecar");
    assert!(sidecar_body.contains(&format!("change_id={CHANGE_ID}")));
    assert!(sidecar_body.contains(&format!("commit_oid={commit_oid}")));

    let commit_oid_after = stdout_trim(fixture.git_success(&repo, &["rev-parse", "HEAD"]));
    let tree_oid_after = stdout_trim(fixture.git_success(&repo, &["rev-parse", "HEAD^{tree}"]));
    let blob_oid_after =
        stdout_trim(fixture.git_success(&repo, &["rev-parse", "HEAD:tracked.txt"]));
    let commit_body_after = String::from_utf8(
        fixture
            .git_success(&repo, &["cat-file", "-p", "HEAD"])
            .stdout,
    )
    .expect("Git commit after sidecar write is UTF-8");
    assert_eq!(
        commit_oid_after, commit_oid,
        "sidecar must not change Commit OID"
    );
    assert_eq!(tree_oid_after, tree_oid, "sidecar must not change tree OID");
    assert_eq!(blob_oid_after, blob_oid, "sidecar must not change blob OID");
    assert_eq!(
        commit_body_after, commit_body_before,
        "sidecar must not change commit content"
    );

    for (kind, oid) in [
        ("commit", &commit_oid),
        ("tree", &tree_oid),
        ("blob", &blob_oid),
    ] {
        fixture.git_success(&repo, &["cat-file", "-e", &format!("{oid}^{{{kind}}}")]);
    }
    let reachable = String::from_utf8(
        fixture
            .git_success(&repo, &["rev-list", "--objects", "--all"])
            .stdout,
    )
    .expect("reachable Git object list is UTF-8");
    for oid in [&commit_oid, &tree_oid, &blob_oid] {
        assert!(
            reachable.lines().any(|line| line.starts_with(oid)),
            "sidecar-referenced object {oid} must remain in Git's enumerable closure:\n{reachable}"
        );
    }
    fixture.git_success(&repo, &["fsck", "--full", "--no-reflogs"]);
}
