//! Git-compat regression for issue #497: deleting the LAST tracked file must
//! produce a deletion commit, not "nothing to commit".
//!
//! The Git upstream scenario is `git rm <last-file> && git commit`, which
//! succeeds even though the worktree becomes empty. Libra must behave the same
//! for both `commit -a` and `add -A && commit`.

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

fn assert_failure(args: &[&str], output: &Output) {
    assert!(
        !output.status.success(),
        "{} unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Mirror the Git upstream scenario: deleting the LAST tracked file via
/// `rm` (staged) then `commit -a` must produce a deletion commit.
#[test]
fn git_deletes_last_tracked_file_via_git_rm_and_commit() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path();
    let git_repo = root.join("git_repo");
    fs::create_dir_all(&git_repo).expect("create git repo");

    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&git_repo)
            .output()
            .expect("spawn git")
    };

    assert_success(&["git init"], &git(&["init", "-q"]));
    assert_success(&["git config"], &git(&["config", "user.email", "t@t.com"]));
    assert_success(&["git config"], &git(&["config", "user.name", "t"]));

    fs::write(git_repo.join("only.txt"), "only\n").expect("write only.txt");
    assert_success(&["git add"], &git(&["add", "only.txt"]));
    assert_success(&["git commit"], &git(&["commit", "-q", "-m", "baseline"]));

    assert_success(&["git rm"], &git(&["rm", "-q", "only.txt"]));
    // Deleting the last tracked file must still create a deletion commit.
    assert_success(
        &["git commit"],
        &git(&["commit", "-q", "-m", "delete last"]),
    );

    let ls_tree = git(&["ls-tree", "-r", "--name-only", "HEAD"]);
    assert_success(&["git ls-tree"], &ls_tree);
    let names = String::from_utf8_lossy(&ls_tree.stdout);
    assert!(
        !names.contains("only.txt"),
        "git HEAD tree should no longer contain only.txt: {names}"
    );
}

/// Libra must match the above Git behavior for `libra rm` + `commit -a`.
#[test]
fn libra_deletes_last_tracked_file_via_rm_and_commit_all() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    // Track exactly one file so removing it empties the index (issue #497).
    fs::write(fixture.repo.join("only.txt"), "only\n").expect("write only.txt");
    fixture.success(&fixture.repo, &["add", "only.txt"]);
    fixture.success(&fixture.repo, &["commit", "--no-verify", "-m", "baseline"]);

    fixture.success(&fixture.repo, &["rm", "only.txt"]);
    // Must succeed — not "nothing to commit".
    fixture.success(
        &fixture.repo,
        &["commit", "-a", "--no-verify", "-m", "delete last"],
    );

    let ls_tree = fixture.success(&fixture.repo, &["ls-tree", "-r", "--name-only", "HEAD"]);
    let names = String::from_utf8_lossy(&ls_tree.stdout);
    assert!(
        !names.contains("only.txt"),
        "libra HEAD tree should no longer contain only.txt: {names}"
    );
}

/// Libra must match Git for the `add -A && commit` path.
#[test]
fn libra_deletes_last_tracked_file_via_add_all_and_commit() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    fs::write(fixture.repo.join("only.txt"), "only\n").expect("write only.txt");
    fixture.success(&fixture.repo, &["add", "only.txt"]);
    fixture.success(&fixture.repo, &["commit", "--no-verify", "-m", "baseline"]);

    fs::remove_file(fixture.repo.join("only.txt")).expect("remove only.txt");
    fixture.success(&fixture.repo, &["add", "-A"]);
    // `add -A` may also pick up untracked files; the deletion must still land.
    fixture.success(
        &fixture.repo,
        &["commit", "--no-verify", "-m", "delete last"],
    );

    let ls_tree = fixture.success(&fixture.repo, &["ls-tree", "-r", "--name-only", "HEAD"]);
    let names = String::from_utf8_lossy(&ls_tree.stdout);
    assert!(
        !names.contains("only.txt"),
        "libra HEAD tree should no longer contain only.txt: {names}"
    );
}

/// A genuinely clean repository (with HEAD content and no staged changes) must
/// still refuse to commit, matching Git's "nothing to commit".
#[test]
fn libra_clean_repo_still_reports_nothing_to_commit() {
    let fixture = CliFixture::new();
    fixture.init_repo();

    fs::write(fixture.repo.join("kept.txt"), "kept\n").expect("write kept.txt");
    // Track `.libraignore` too so the repo is genuinely clean (no untracked files).
    fixture.success(&fixture.repo, &["add", "kept.txt", ".libraignore"]);
    fixture.success(&fixture.repo, &["commit", "--no-verify", "-m", "baseline"]);

    // No changes staged and no untracked leftovers: must be refused.
    let output = fixture.run(&fixture.repo, &["commit", "--no-verify", "-m", "nothing"]);
    assert_failure(&["commit (clean)"], &output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nothing to commit"),
        "clean repo should report nothing to commit: {stderr}"
    );
}
