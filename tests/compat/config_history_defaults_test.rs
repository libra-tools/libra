//! P1-05b end-to-end coverage for history-changing Git config defaults.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::{TempDir, tempdir};

const PATH_ENV: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Fixture {
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

    fn command(&self, repo: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(repo)
            .env_clear()
            .env("PATH", PATH_ENV)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("LIBRA_TEST", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C");
        command
    }

    fn run(&self, repo: &Path, args: &[&str]) -> Output {
        self.command(repo, args).output().expect("spawn libra")
    }

    fn success(&self, repo: &Path, args: &[&str]) -> Output {
        let output = self.run(repo, args);
        assert!(
            output.status.success(),
            "libra {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn repo(&self, name: &str) -> PathBuf {
        let repo = self.root.join(name);
        self.success(&self.root, &["init", "--vault", "true", path_str(&repo)]);
        self.success(&repo, &["config", "user.name", "Config History"]);
        self.success(&repo, &["config", "user.email", "history@example.com"]);
        self.success(&repo, &["config", "vault.signing", "false"]);
        write_and_commit(self, &repo, "base.txt", "base", "base commit");
        repo
    }

    fn feature_with_commits(&self, repo: &Path, subjects: &[&str]) {
        self.success(repo, &["branch", "feature"]);
        self.success(repo, &["checkout", "feature"]);
        for (index, subject) in subjects.iter().enumerate() {
            write_and_commit(
                self,
                repo,
                &format!("feature-{index}.txt"),
                subject,
                subject,
            );
        }
        self.success(repo, &["checkout", "main"]);
    }
}

#[test]
fn merge_ff_false_forces_merge_commit_when_fast_forward_is_possible() {
    let fixture = Fixture::new();
    let repo = fixture.repo("merge-ff");
    fixture.feature_with_commits(&repo, &["feature tip"]);

    fixture.success(&repo, &["config", "merge.ff", "false"]);
    fixture.success(&repo, &["merge", "feature"]);

    let parents = fixture.success(&repo, &["log", "-1", "--format=%P"]);
    assert_eq!(
        String::from_utf8_lossy(&parents.stdout)
            .split_whitespace()
            .count(),
        2
    );
}

#[test]
fn merge_ff_cli_flag_overrides_false_config() {
    let fixture = Fixture::new();
    let repo = fixture.repo("merge-ff-override");
    fixture.feature_with_commits(&repo, &["feature tip"]);

    fixture.success(&repo, &["config", "merge.ff", "false"]);
    fixture.success(&repo, &["merge", "--ff", "feature"]);

    let parents = fixture.success(&repo, &["log", "-1", "--format=%P"]);
    assert_eq!(
        String::from_utf8_lossy(&parents.stdout)
            .split_whitespace()
            .count(),
        1
    );
}

#[test]
fn merge_log_limit_adds_only_the_requested_number_of_subjects() {
    let fixture = Fixture::new();
    let repo = fixture.repo("merge-log");
    fixture.feature_with_commits(&repo, &["older feature", "newer feature"]);

    fixture.success(&repo, &["config", "merge.ff", "false"]);
    fixture.success(&repo, &["config", "merge.log", "1"]);
    fixture.success(&repo, &["merge", "feature"]);

    let parents = fixture.success(&repo, &["log", "-1", "--format=%P"]);
    assert_eq!(
        String::from_utf8_lossy(&parents.stdout)
            .split_whitespace()
            .count(),
        2,
        "merge.log must be tested on an actual merge commit"
    );
    let commit = fixture.success(&repo, &["cat-file", "-p", "HEAD"]);
    let body = String::from_utf8_lossy(&commit.stdout);
    assert!(body.contains("newer feature"), "commit: {body}");
    assert!(!body.contains("older feature"), "commit: {body}");
}

#[test]
fn merge_verify_signatures_config_fails_closed_and_cli_can_override() {
    let fixture = Fixture::new();
    let repo = fixture.repo("merge-verify");
    fixture.feature_with_commits(&repo, &["unsigned feature"]);
    fixture.success(&repo, &["config", "merge.verifySignatures", "true"]);

    let rejected = fixture.run(&repo, &["merge", "feature"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("does not have a GPG signature"));

    fixture.success(&repo, &["merge", "--no-verify-signatures", "feature"]);
}

#[test]
fn commit_gpgsign_controls_vault_signing_and_no_gpg_sign_wins() {
    let fixture = Fixture::new();
    let repo = fixture.repo("commit-signing");

    fixture.success(&repo, &["config", "vault.signing", "true"]);
    fixture.success(&repo, &["config", "commit.gpgSign", "false"]);
    write_and_commit(
        &fixture,
        &repo,
        "unsigned.txt",
        "unsigned",
        "unsigned config",
    );
    assert!(!head_has_gpg_signature(&fixture, &repo));

    fixture.success(&repo, &["config", "vault.signing", "false"]);
    fixture.success(&repo, &["config", "commit.gpgSign", "true"]);
    write_and_commit(&fixture, &repo, "signed.txt", "signed", "signed config");
    assert!(head_has_gpg_signature(&fixture, &repo));

    fs::write(repo.join("override.txt"), "override\n").expect("write override");
    fixture.success(&repo, &["add", "override.txt"]);
    fixture.success(
        &repo,
        &[
            "commit",
            "--no-gpg-sign",
            "-m",
            "unsigned override",
            "--no-verify",
        ],
    );
    assert!(!head_has_gpg_signature(&fixture, &repo));
}

#[test]
fn invalid_history_defaults_fail_before_changing_head() {
    let fixture = Fixture::new();
    let repo = fixture.repo("invalid-history-defaults");
    fixture.feature_with_commits(&repo, &["feature tip"]);
    let original = fixture.success(&repo, &["rev-parse", "HEAD"]);

    for key in ["merge.ff", "merge.log", "merge.verifySignatures"] {
        fixture.success(&repo, &["config", key, "invalid"]);
        let rejected = fixture.run(&repo, &["merge", "feature"]);
        assert_eq!(rejected.status.code(), Some(129), "key: {key}");
        fixture.success(&repo, &["config", "--unset", key]);
    }

    fs::write(repo.join("invalid-signing.txt"), "invalid\n").expect("write invalid signing file");
    fixture.success(&repo, &["add", "invalid-signing.txt"]);
    fixture.success(&repo, &["config", "commit.gpgSign", "invalid"]);
    let rejected = fixture.run(&repo, &["commit", "-m", "must fail", "--no-verify"]);
    assert_eq!(rejected.status.code(), Some(129));

    let current = fixture.success(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(current.stdout, original.stdout);
}

#[test]
fn merge_verify_signatures_rejection_precedes_autostash_and_merge_side_effects() {
    let fixture = Fixture::new();
    let repo = fixture.repo("verify-before-autostash");
    fixture.feature_with_commits(&repo, &["unsigned feature"]);
    fixture.success(&repo, &["config", "merge.verifySignatures", "true"]);
    fixture.success(&repo, &["config", "merge.autostash", "true"]);

    // Dirty the worktree so a (wrongly ordered) merge would create an
    // autostash — which writes commit/tree objects — before verification.
    fs::write(repo.join("base.txt"), "dirty local edit\n").expect("dirty worktree");
    let rejected = fixture.run(&repo, &["merge", "feature"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("does not have a GPG signature"));

    // A rejected mutation may still create the v2 failed-Operation audit
    // objects. The user-visible merge domain must remain untouched: no
    // autostash objects/sidecar or stash entry, and the local edit survives.
    let operations = fixture.success(&repo, &["--json", "op", "log", "-n", "1"]);
    let operations: Value = serde_json::from_slice(&operations.stdout).expect("op log JSON");
    assert_eq!(operations["data"]["operations"][0]["status"], "failed");
    let stash = fixture.success(&repo, &["stash", "list"]);
    assert!(String::from_utf8_lossy(&stash.stdout).trim().is_empty());
    assert_eq!(
        fs::read_to_string(repo.join("base.txt")).expect("read dirty file"),
        "dirty local edit\n"
    );
}

#[test]
fn merge_log_survives_conflicted_continue() {
    let fixture = Fixture::new();
    let repo = fixture.repo("merge-log-continue");
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["checkout", "feature"]);
    write_and_commit(
        &fixture,
        &repo,
        "base.txt",
        "feature side",
        "feature change",
    );
    fixture.success(&repo, &["checkout", "main"]);
    write_and_commit(&fixture, &repo, "base.txt", "main side", "main change");
    fixture.success(&repo, &["config", "merge.log", "true"]);

    let conflicted = fixture.run(&repo, &["merge", "feature"]);
    assert!(!conflicted.status.success());

    fs::write(repo.join("base.txt"), "resolved\n").expect("resolve conflict");
    fixture.success(&repo, &["add", "base.txt"]);
    fixture.success(&repo, &["merge", "--continue"]);

    let parents = fixture.success(&repo, &["log", "-1", "--format=%P"]);
    assert_eq!(
        String::from_utf8_lossy(&parents.stdout)
            .split_whitespace()
            .count(),
        2
    );
    let commit = fixture.success(&repo, &["cat-file", "-p", "HEAD"]);
    let body = String::from_utf8_lossy(&commit.stdout);
    assert!(
        body.contains("* feature:") && body.contains("feature change"),
        "merge --continue must replay the merge.log shortlog, got: {body}"
    );
}

#[test]
fn no_commit_continue_replays_resolved_message() {
    let fixture = Fixture::new();
    let repo = fixture.repo("no-commit-continue");
    fixture.feature_with_commits(&repo, &["feature tip"]);

    fixture.success(
        &repo,
        &[
            "merge",
            "--no-commit",
            "-m",
            "custom recorded subject",
            "feature",
        ],
    );
    fixture.success(&repo, &["merge", "--continue"]);

    let commit = fixture.success(&repo, &["cat-file", "-p", "HEAD"]);
    let body = String::from_utf8_lossy(&commit.stdout);
    assert!(
        body.contains("custom recorded subject"),
        "merge --continue must replay the -m message, got: {body}"
    );
}

#[test]
fn merge_ff_only_config_allows_fast_forwardable_squash() {
    let fixture = Fixture::new();
    let repo = fixture.repo("ff-only-squash");
    fixture.feature_with_commits(&repo, &["feature tip"]);
    fixture.success(&repo, &["config", "merge.ff", "only"]);

    let head_before = fixture.success(&repo, &["rev-parse", "HEAD"]);
    fixture.success(&repo, &["merge", "--squash", "feature"]);
    let head_after = fixture.success(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(
        head_before.stdout, head_after.stdout,
        "squash must not move HEAD"
    );
    let status = fixture.success(&repo, &["status", "--porcelain"]);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("feature-0.txt"),
        "squash must stage the fast-forwardable result"
    );

    // A genuinely diverged history keeps being refused under merge.ff=only.
    let diverged = fixture.repo("ff-only-squash-diverged");
    fixture.feature_with_commits(&diverged, &["feature tip"]);
    write_and_commit(&fixture, &diverged, "main-div.txt", "div", "diverge main");
    fixture.success(&diverged, &["config", "merge.ff", "only"]);
    let rejected = fixture.run(&diverged, &["merge", "--squash", "feature"]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("non-fast-forward"),
        "diverged squash must still be refused under merge.ff=only"
    );
}

#[test]
fn merge_cli_ff_only_combines_with_squash_and_no_commit() {
    let fixture = Fixture::new();

    // Explicit `--ff-only --squash` on a fast-forwardable target must pass
    // argument parsing AND succeed (HEAD untouched, result staged).
    let squash = fixture.repo("cli-ff-only-squash");
    fixture.feature_with_commits(&squash, &["feature tip"]);
    let head_before = fixture.success(&squash, &["rev-parse", "HEAD"]);
    fixture.success(&squash, &["merge", "--ff-only", "--squash", "feature"]);
    let head_after = fixture.success(&squash, &["rev-parse", "HEAD"]);
    assert_eq!(head_before.stdout, head_after.stdout);

    // Explicit `--ff-only --no-commit` likewise parses and completes through
    // `merge --continue` with a two-parent commit.
    let no_commit = fixture.repo("cli-ff-only-no-commit");
    fixture.feature_with_commits(&no_commit, &["feature tip"]);
    fixture.success(
        &no_commit,
        &["merge", "--ff-only", "--no-commit", "feature"],
    );
    fixture.success(&no_commit, &["merge", "--continue"]);
    let parents = fixture.success(&no_commit, &["log", "-1", "--format=%P"]);
    assert_eq!(
        String::from_utf8_lossy(&parents.stdout)
            .split_whitespace()
            .count(),
        2
    );

    // A diverged history is refused by the merge itself, not by clap.
    let diverged = fixture.repo("cli-ff-only-diverged");
    fixture.feature_with_commits(&diverged, &["feature tip"]);
    write_and_commit(&fixture, &diverged, "main-div.txt", "div", "diverge main");
    let rejected = fixture.run(&diverged, &["merge", "--ff-only", "--squash", "feature"]);
    assert!(!rejected.status.success());
    assert_ne!(
        rejected.status.code(),
        Some(129),
        "divergence must be a merge failure, not an argument-parsing conflict"
    );
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("non-fast-forward"));
}

#[test]
fn commit_help_documents_gpgsign_precedence() {
    let fixture = Fixture::new();
    let help = fixture.success(&fixture.root, &["commit", "--help"]);
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(
        text.contains("commit.gpgSign"),
        "commit --help must document the commit.gpgSign default"
    );
    assert!(
        text.contains("vault.signing"),
        "commit --help must keep documenting the vault.signing fallback"
    );
}

fn write_and_commit(fixture: &Fixture, repo: &Path, file: &str, body: &str, subject: &str) {
    fs::write(repo.join(file), format!("{body}\n")).expect("write fixture file");
    fixture.success(repo, &["add", file]);
    fixture.success(repo, &["commit", "-m", subject, "--no-verify"]);
}

fn head_has_gpg_signature(fixture: &Fixture, repo: &Path) -> bool {
    fixture
        .run(repo, &["merge", "--verify-signatures", "HEAD"])
        .status
        .success()
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("fixture path is utf8")
}

#[test]
fn history_defaults_accept_git_numeric_booleans_and_keep_only() {
    let fixture = Fixture::new();

    // merge.ff=2 is a Git-boolean true: fast-forward stays allowed.
    let repo = fixture.repo("numeric-ff-true");
    fixture.feature_with_commits(&repo, &["feature tip"]);
    fixture.success(&repo, &["config", "merge.ff", "2"]);
    fixture.success(&repo, &["merge", "feature"]);
    let parents = fixture.success(&repo, &["log", "-1", "--format=%P"]);
    assert_eq!(
        String::from_utf8_lossy(&parents.stdout)
            .split_whitespace()
            .count(),
        1,
        "merge.ff=2 must count as true (fast-forward allowed)"
    );

    // merge.ff=0k is a Git-boolean false: force a two-parent merge commit.
    let repo = fixture.repo("numeric-ff-false");
    fixture.feature_with_commits(&repo, &["feature tip"]);
    fixture.success(&repo, &["config", "merge.ff", "0k"]);
    fixture.success(&repo, &["merge", "feature"]);
    let parents = fixture.success(&repo, &["log", "-1", "--format=%P"]);
    assert_eq!(
        String::from_utf8_lossy(&parents.stdout)
            .split_whitespace()
            .count(),
        2,
        "merge.ff=0k must count as false (merge commit forced)"
    );

    // The non-boolean 'only' keyword must survive the widened parser.
    let repo = fixture.repo("numeric-ff-only");
    fixture.feature_with_commits(&repo, &["feature tip"]);
    write_and_commit(&fixture, &repo, "diverge.txt", "div", "diverge main");
    fixture.success(&repo, &["config", "merge.ff", "only"]);
    let rejected = fixture.run(&repo, &["merge", "feature"]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("non-fast-forward"),
        "merge.ff=only must still reject a diverged merge"
    );

    // merge.verifySignatures=1m is true: an unsigned tip is rejected.
    let repo = fixture.repo("numeric-verify");
    fixture.feature_with_commits(&repo, &["unsigned feature"]);
    fixture.success(&repo, &["config", "merge.verifySignatures", "1m"]);
    let rejected = fixture.run(&repo, &["merge", "feature"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("does not have a GPG signature"));

    // commit.gpgSign=2 is true: the commit is vault-signed even with
    // vault.signing=false.
    let repo = fixture.repo("numeric-gpgsign");
    fixture.success(&repo, &["config", "vault.signing", "false"]);
    fixture.success(&repo, &["config", "commit.gpgSign", "2"]);
    write_and_commit(&fixture, &repo, "signed.txt", "signed", "numeric signed");
    assert!(head_has_gpg_signature(&fixture, &repo));
}
