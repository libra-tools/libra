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

// ---- PD-05 (`--missing-ok`) extensions ----------------------------------

impl CliFixture {
    fn init_repo_with_format(&self, object_format: &str) {
        fs::create_dir_all(&self.repo).expect("create repo dir");
        self.success(
            &self.root,
            &[
                "init",
                "--vault",
                "false",
                "--object-format",
                object_format,
                self.repo.to_str().expect("utf8 repo"),
            ],
        );
        self.success(&self.repo, &["config", "set", "user.name", "Test User"]);
        self.success(
            &self.repo,
            &["config", "set", "user.email", "test@example.com"],
        );
    }
}

/// PD-05: `--missing-ok` tolerates an absent blob and writes the tree with
/// the recorded id, byte-identical to Git. The expected OID is pinned from
/// `git write-tree --missing-ok` (verified against git 2.53.0) for the entry
/// `100644 1111…11 missing.txt`. Without the flag the same index keeps
/// failing closed.
#[test]
fn write_tree_missing_ok_writes_git_parity_tree() {
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

    // No flag: unchanged fail-closed behavior.
    let strict = fixture.run(&fixture.repo, &["write-tree"]);
    assert_repo_corrupt(&["write-tree"], &strict);

    // With the valve: success, Git-identical tree id.
    let tree = stdout_trim(&fixture.success(&fixture.repo, &["write-tree", "--missing-ok"]));
    assert_eq!(
        tree, "b14457811d8419dffed3b9f33dbaf7dc88dfdf26",
        "--missing-ok tree id must match `git write-tree --missing-ok`"
    );
}

/// PD-05: the valve excuses ONLY absent blobs — an index entry whose object
/// exists with the wrong type still fails closed with `LBR-REPO-002`.
#[test]
fn write_tree_missing_ok_still_rejects_wrong_object_type() {
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
    let output = fixture.run(&fixture.repo, &["write-tree", "--missing-ok"]);
    assert_repo_corrupt(&["write-tree", "--missing-ok"], &output);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("expected blob object but found tree"),
        "wrong-type diagnostic must survive --missing-ok:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// PD-05: an unreadable (corrupt) loose object is NOT a missing object — the
/// valve never masks a damaged object store.
#[test]
fn write_tree_missing_ok_keeps_corrupt_object_fatal() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    // Plant garbage bytes at the loose-object path of the referenced blob so
    // the read fails with a corruption error rather than not-found.
    let objects = fixture.repo.join(".libra").join("objects");
    let dir = objects.join(&MISSING_BLOB[..2]);
    fs::create_dir_all(&dir).expect("create loose object shard");
    fs::write(dir.join(&MISSING_BLOB[2..]), b"not a zlib stream").expect("plant corrupt object");

    fixture.success(
        &fixture.repo,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{MISSING_BLOB},missing.txt"),
        ],
    );
    let output = fixture.run(&fixture.repo, &["write-tree", "--missing-ok"]);
    assert_repo_corrupt(&["write-tree", "--missing-ok"], &output);
}

/// PD-05 hash-kind neutrality: the valve works identically in a SHA-256
/// repository (expected id computed from the canonical tree encoding).
#[test]
fn write_tree_missing_ok_sha256_smoke() {
    const MISSING_BLOB_256: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";

    let fixture = CliFixture::new();
    fixture.init_repo_with_format("sha256");

    fixture.success(
        &fixture.repo,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{MISSING_BLOB_256},missing.txt"),
        ],
    );
    let strict = fixture.run(&fixture.repo, &["write-tree"]);
    assert_repo_corrupt(&["write-tree"], &strict);

    let tree = stdout_trim(&fixture.success(&fixture.repo, &["write-tree", "--missing-ok"]));
    assert_eq!(
        tree, "ba011e28b0d8de9276dad1641de08128347d6d876b4f4183c2a06fa4f3c360af",
        "--missing-ok tree id must be the canonical SHA-256 tree encoding"
    );
}
