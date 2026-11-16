//! Non-interactive history controls for plan-20260708 P1-07a/P1-07b/P1-07c.

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
    #[cfg(target_os = "linux")]
    sandbox_helper: PathBuf,
}

impl CliFixture {
    fn new() -> Self {
        let temp = tempdir().expect("create tempdir");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        fs::create_dir_all(&home).expect("create isolated home");
        #[cfg(target_os = "linux")]
        let sandbox_helper = {
            use std::os::unix::fs::PermissionsExt;
            let sandbox_helper = root.join("test-linux-sandbox");
            fs::write(
                &sandbox_helper,
                "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"--\" ]; then\n    shift\n    exec \"$@\"\n  fi\n  shift\ndone\nexit 125\n",
            )
            .expect("write test sandbox helper");
            let mut permissions = fs::metadata(&sandbox_helper)
                .expect("stat test sandbox helper")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&sandbox_helper, permissions)
                .expect("make test sandbox helper executable");
            sandbox_helper
        };
        Self {
            _temp: temp,
            root,
            home,
            #[cfg(target_os = "linux")]
            sandbox_helper,
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
        #[cfg(target_os = "linux")]
        command.env("LIBRA_LINUX_SANDBOX_EXE", &self.sandbox_helper);
        command
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd, args).output().expect("spawn libra")
    }

    #[cfg(target_os = "linux")]
    fn run_with_required_system_sandbox(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd, args)
            .env_remove("LIBRA_LINUX_SANDBOX_EXE")
            .output()
            .expect("spawn libra with system sandbox")
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

    fn init_repo(&self, name: &str) -> PathBuf {
        let repo = self.root.join(name);
        fs::create_dir_all(&repo).expect("create repository directory");
        self.success(&self.root, &["init", repo.to_str().expect("utf8 repo")]);
        self.success(&repo, &["config", "set", "user.name", "History Test"]);
        self.success(
            &repo,
            &["config", "set", "user.email", "history@example.com"],
        );
        repo
    }

    fn commit_file(&self, repo: &Path, path: &str, contents: &str, message: &str) -> String {
        fs::write(repo.join(path), contents).expect("write commit fixture");
        self.success(repo, &["add", path]);
        self.success(repo, &["commit", "-s", "-m", message]);
        self.oid(repo, "HEAD")
    }

    fn oid(&self, repo: &Path, revision: &str) -> String {
        let output = self.success(repo, &["rev-parse", revision]);
        String::from_utf8(output.stdout)
            .expect("oid output utf8")
            .trim()
            .to_string()
    }
}

fn divergent_feature(
    fixture: &CliFixture,
    name: &str,
    feature_commits: usize,
) -> (PathBuf, String) {
    let repo = fixture.init_repo(name);
    fixture.commit_file(&repo, "base.txt", "base\n", "base");
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["switch", "feature"]);
    for index in 1..=feature_commits {
        fixture.commit_file(
            &repo,
            &format!("feature-{index}.txt"),
            &format!("feature {index}\n"),
            &format!("feature-{index}"),
        );
    }
    fixture.success(&repo, &["switch", "main"]);
    let main_tip = fixture.commit_file(&repo, "main.txt", "main\n", "main-change");
    fixture.success(&repo, &["switch", "feature"]);
    (repo, main_tip)
}

fn force_moved_upstream(fixture: &CliFixture, name: &str) -> (PathBuf, String) {
    let repo = fixture.init_repo(name);
    let base = fixture.commit_file(&repo, "base.txt", "base\n", "base");
    fixture.success(&repo, &["branch", "upstream"]);
    fixture.success(&repo, &["switch", "upstream"]);
    fixture.commit_file(&repo, "old-upstream.txt", "old upstream\n", "old-upstream");
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["switch", "feature"]);
    fixture.commit_file(&repo, "feature.txt", "feature\n", "feature-change");
    fixture.success(&repo, &["switch", "upstream"]);
    fixture.success(&repo, &["reset", "--hard", &base]);
    let new_upstream =
        fixture.commit_file(&repo, "new-upstream.txt", "new upstream\n", "new-upstream");
    fixture.success(&repo, &["switch", "feature"]);
    (repo, new_upstream)
}

fn conflicting_merge_repo(fixture: &CliFixture, name: &str) -> (PathBuf, String, String) {
    let repo = fixture.init_repo(name);
    fixture.commit_file(
        &repo,
        "shared.txt",
        "top\nconflict\nmiddle\nbottom\n",
        "base",
    );
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["switch", "feature"]);
    let feature_tip = fixture.commit_file(
        &repo,
        "shared.txt",
        "top\nTHEIRS\nmiddle\ntheirs-clean\n",
        "feature-change",
    );
    fixture.success(&repo, &["switch", "main"]);
    let main_tip = fixture.commit_file(
        &repo,
        "shared.txt",
        "top\nOURS\nmiddle\nbottom\n",
        "main-change",
    );
    (repo, main_tip, feature_tip)
}

#[test]
fn rebase_autostash_restores_dirty_tracked_changes_after_history_rewrite() {
    let fixture = CliFixture::new();
    let (repo, main_tip) = divergent_feature(&fixture, "autostash", 1);
    fs::write(repo.join("feature-1.txt"), "feature 1\nlocal dirty\n")
        .expect("write dirty tracked file");

    fixture.success(&repo, &["rebase", "--autostash", "main"]);

    assert_eq!(
        fs::read_to_string(repo.join("feature-1.txt")).expect("read restored file"),
        "feature 1\nlocal dirty\n"
    );
    assert_eq!(fixture.oid(&repo, "HEAD^"), main_tip);
    let status = fixture.success(&repo, &["status", "--porcelain"]);
    assert!(String::from_utf8_lossy(&status.stdout).contains("feature-1.txt"));
    assert!(!repo.join(".libra/rebase-aux.json").exists());
}

#[test]
fn rebase_autostash_restores_staged_and_worktree_layers_without_data_loss() {
    let fixture = CliFixture::new();
    let (repo, main_tip) = divergent_feature(&fixture, "autostash-index", 1);
    fs::write(repo.join("feature-1.txt"), "staged only\n").expect("write staged version");
    fixture.success(&repo, &["add", "feature-1.txt"]);
    fs::write(repo.join("feature-1.txt"), "feature 1\n")
        .expect("restore worktree version after staging");
    let before = fixture.success(&repo, &["ls-files", "--stage", "feature-1.txt"]);
    let before = String::from_utf8(before.stdout).expect("pre-rebase stage row utf8");
    let before_oid = before
        .split_whitespace()
        .nth(1)
        .expect("pre-rebase stage row has object id");
    assert_eq!(
        fixture
            .success(&repo, &["cat-file", "-p", before_oid])
            .stdout,
        b"staged only\n"
    );

    let rebase = fixture.success(&repo, &["rebase", "--autostash", "main"]);

    assert_eq!(fixture.oid(&repo, "HEAD^"), main_tip);
    assert_eq!(
        fs::read_to_string(repo.join("feature-1.txt")).expect("read restored worktree layer"),
        "feature 1\n"
    );
    let staged = fixture.success(&repo, &["ls-files", "--stage", "feature-1.txt"]);
    let staged = String::from_utf8(staged.stdout).expect("stage row utf8");
    let (metadata, staged_path) = staged.trim().split_once('\t').expect("stage row has a tab");
    assert_eq!(staged_path, "feature-1.txt");
    let staged_oid = metadata
        .split_whitespace()
        .nth(1)
        .expect("stage row has an object id");
    let staged_blob = fixture.success(&repo, &["cat-file", "-p", staged_oid]);
    assert_eq!(
        staged_blob.stdout,
        b"staged only\n",
        "staged-only content was not restored; rebase stderr:\n{}",
        String::from_utf8_lossy(&rebase.stderr)
    );
    let unstaged = fixture.success(&repo, &["diff"]);
    let unstaged = String::from_utf8_lossy(&unstaged.stdout);
    assert!(
        unstaged.contains("-staged only") && unstaged.contains("+feature 1"),
        "worktree/index distinction was not restored:\n{unstaged}"
    );
    assert!(!repo.join(".libra/rebase-aux.json").exists());
}

#[test]
fn rebase_autostash_stays_held_through_conflict_and_abort() {
    let fixture = CliFixture::new();
    let repo = fixture.init_repo("autostash-abort");
    fs::write(repo.join("shared.txt"), "base\n").expect("write shared base");
    fs::write(repo.join("dirty.txt"), "clean\n").expect("write dirty base");
    fixture.success(&repo, &["add", "shared.txt", "dirty.txt"]);
    fixture.success(&repo, &["commit", "-s", "-m", "base"]);
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["switch", "feature"]);
    let original_feature =
        fixture.commit_file(&repo, "shared.txt", "feature\n", "feature-conflict");
    fixture.success(&repo, &["switch", "main"]);
    fixture.commit_file(&repo, "shared.txt", "main\n", "main-conflict");
    fixture.success(&repo, &["switch", "feature"]);
    fs::write(repo.join("dirty.txt"), "local dirty\n").expect("write dirty change");

    let conflict = fixture.run(&repo, &["rebase", "--autostash", "main"]);
    assert!(!conflict.status.success());
    assert_eq!(
        fs::read_to_string(repo.join("dirty.txt")).expect("read held worktree"),
        "clean\n",
        "autostash must remain held while the sequencer is stopped"
    );
    assert!(repo.join(".libra/rebase-aux.json").exists());

    fixture.success(&repo, &["maintenance", "run", "--task", "gc"]);

    fixture.success(&repo, &["rebase", "--abort"]);
    assert_eq!(fixture.oid(&repo, "HEAD"), original_feature);
    assert_eq!(
        fs::read_to_string(repo.join("dirty.txt")).expect("read restored dirty file"),
        "local dirty\n"
    );
    assert!(!repo.join(".libra/rebase-aux.json").exists());
}

#[test]
fn rebase_autostash_toggle_is_last_wins() {
    let fixture = CliFixture::new();
    let (repo, _) = divergent_feature(&fixture, "autostash-toggle", 1);
    let original_feature = fixture.oid(&repo, "HEAD");
    fs::write(repo.join("feature-1.txt"), "feature 1\nlocal dirty\n")
        .expect("write dirty tracked file");

    let disabled = fixture.run(&repo, &["rebase", "--autostash", "--no-autostash", "main"]);
    assert!(!disabled.status.success());
    assert_eq!(fixture.oid(&repo, "HEAD"), original_feature);
    assert!(!repo.join(".libra/rebase-aux.json").exists());

    fixture.success(&repo, &["rebase", "--no-autostash", "--autostash", "main"]);
    assert_eq!(
        fs::read_to_string(repo.join("feature-1.txt")).expect("read restored dirty file"),
        "feature 1\nlocal dirty\n"
    );
}

#[test]
fn rebase_exec_runs_after_each_replayed_commit_and_preserves_history() {
    let fixture = CliFixture::new();
    let (repo, main_tip) = divergent_feature(&fixture, "exec-success", 2);

    fixture.success(
        &repo,
        &["rebase", "--exec", "printf 'ran\\n' >> exec.log", "main"],
    );

    let lines = fs::read_to_string(repo.join("exec.log"))
        .expect("exec log")
        .lines()
        .count();
    assert_eq!(lines, 2, "--exec must run once per replayed commit");
    assert_eq!(fixture.oid(&repo, "HEAD~2"), main_tip);
}

#[test]
fn rebase_exec_failure_stops_and_continue_retries_the_command() {
    let fixture = CliFixture::new();
    let (repo, main_tip) = divergent_feature(&fixture, "exec-retry", 1);
    let original_feature = fixture.oid(&repo, "HEAD");
    fixture.success(&repo, &["branch", "exec-pointer", &original_feature]);

    let libra = env!("CARGO_BIN_EXE_libra");
    let create_exec_commit = format!(
        "printf 'exec-created\\n' > exec-created.txt && '{libra}' add exec-created.txt && '{libra}' commit --no-gpg-sign -s -m exec-created"
    );
    let failed = fixture.run(
        &repo,
        &[
            "rebase",
            "--exec",
            &create_exec_commit,
            "--exec",
            "test -f allow-exec || exit 23",
            "--update-refs",
            "main",
        ],
    );
    assert!(!failed.status.success());
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("exit 23"), "stderr was: {stderr}");
    assert!(stderr.contains("LBR-CONFLICT-002"), "stderr was: {stderr}");
    assert!(repo.join(".libra/rebase-aux.json").exists());

    fs::write(repo.join("allow-exec"), "allow\n").expect("create retry marker");
    fixture.success(&repo, &["rebase", "--continue"]);

    assert_eq!(fixture.oid(&repo, "HEAD^^"), main_tip);
    assert_eq!(
        fixture.oid(&repo, "exec-pointer"),
        fixture.oid(&repo, "HEAD")
    );
    assert!(repo.join("exec-created.txt").exists());
    assert!(!repo.join(".libra/rebase-aux.json").exists());
}

#[test]
#[cfg(target_os = "linux")]
fn rebase_exec_cannot_write_outside_the_repository_workspace() {
    let fixture = CliFixture::new();
    let (repo, _) = divergent_feature(&fixture, "exec-sandbox", 1);
    let escaped = fixture.root.join("sandbox-escape");

    let outcome = fixture.run_with_required_system_sandbox(
        &repo,
        &["rebase", "--exec", "touch ../sandbox-escape", "main"],
    );
    // The non-negotiable security property: the write must never reach the
    // host filesystem outside the writable workspace root, in every
    // environment. How the sandbox achieves that is environment-dependent:
    // when bubblewrap can run, the repository's parent directory exists only
    // as sandbox-private mount scaffolding, so the write is contained inside
    // the namespace and discarded (the exec command itself succeeds); when
    // the system sandbox cannot start, the required enforcement fails the
    // exec closed and the rebase stops.
    assert!(
        !escaped.exists(),
        "sandbox command escaped its writable root"
    );
    if outcome.status.success() {
        // Contained-and-discarded: the rebase completed, nothing leaked, and
        // no sequencer state is left behind.
        assert!(!repo.join(".libra/rebase-aux.json").exists());
    } else {
        // Denied or fail-closed: the exec failure must stop the rebase with
        // the stable conflict code and remain abortable.
        let stderr = String::from_utf8_lossy(&outcome.stderr);
        assert!(stderr.contains("LBR-CONFLICT-002"), "stderr was: {stderr}");
        fixture.success(&repo, &["rebase", "--abort"]);
    }
}

#[test]
fn rebase_update_refs_moves_rewritten_branches_but_excludes_checked_out_branches() {
    let fixture = CliFixture::new();
    let repo = fixture.init_repo("update-refs");
    fixture.commit_file(&repo, "base.txt", "base\n", "base");
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["switch", "feature"]);
    let first = fixture.commit_file(&repo, "first.txt", "first\n", "feature-first");
    fixture.success(&repo, &["branch", "movable", &first]);
    fixture.success(&repo, &["branch", "checked-out", &first]);
    fixture.commit_file(&repo, "second.txt", "second\n", "feature-second");
    fixture.success(&repo, &["switch", "main"]);
    let main_tip = fixture.commit_file(&repo, "main.txt", "main\n", "main-change");
    fixture.success(&repo, &["switch", "feature"]);

    let linked = fixture.root.join("update-refs-linked");
    fixture.success(
        &repo,
        &[
            "worktree",
            "add",
            "update-refs-linked",
            "--path",
            linked.to_str().expect("utf8 linked"),
        ],
    );
    fixture.success(&linked, &["switch", "checked-out"]);

    fixture.success(&repo, &["rebase", "--update-refs", "main"]);

    let moved = fixture.oid(&repo, "movable");
    assert_ne!(moved, first);
    assert_eq!(fixture.oid(&repo, "movable^"), main_tip);
    assert_eq!(fixture.oid(&repo, "checked-out"), first);
    assert_eq!(fixture.oid(&repo, "HEAD~2"), main_tip);
}

#[test]
fn rebase_update_refs_toggle_is_last_wins() {
    let fixture = CliFixture::new();

    let (disabled_repo, _) = divergent_feature(&fixture, "update-refs-disabled", 1);
    let disabled_original = fixture.oid(&disabled_repo, "HEAD");
    fixture.success(&disabled_repo, &["branch", "pointer", &disabled_original]);
    fixture.success(
        &disabled_repo,
        &["rebase", "--update-refs", "--no-update-refs", "main"],
    );
    assert_eq!(fixture.oid(&disabled_repo, "pointer"), disabled_original);

    let (enabled_repo, _) = divergent_feature(&fixture, "update-refs-enabled", 1);
    let enabled_original = fixture.oid(&enabled_repo, "HEAD");
    fixture.success(&enabled_repo, &["branch", "pointer", &enabled_original]);
    fixture.success(
        &enabled_repo,
        &["rebase", "--no-update-refs", "--update-refs", "main"],
    );
    assert_eq!(
        fixture.oid(&enabled_repo, "pointer"),
        fixture.oid(&enabled_repo, "HEAD")
    );
}

#[test]
fn rebase_update_refs_maps_a_skipped_conflicting_commit_to_the_new_base() {
    let fixture = CliFixture::new();
    let repo = fixture.init_repo("update-refs-skip");
    fixture.commit_file(&repo, "shared.txt", "base\n", "base");
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["switch", "feature"]);
    let conflicting = fixture.commit_file(&repo, "shared.txt", "feature\n", "feature-conflict");
    fixture.success(&repo, &["branch", "points-at-conflict", &conflicting]);
    fixture.success(&repo, &["switch", "main"]);
    let main_tip = fixture.commit_file(&repo, "shared.txt", "main\n", "main-conflict");
    fixture.success(&repo, &["switch", "feature"]);

    let conflict = fixture.run(&repo, &["rebase", "--update-refs", "main"]);
    assert!(!conflict.status.success());
    fixture.success(&repo, &["rebase", "--skip"]);

    assert_eq!(fixture.oid(&repo, "HEAD"), main_tip);
    assert_eq!(fixture.oid(&repo, "points-at-conflict"), main_tip);
}

#[test]
fn rebase_update_refs_maps_start_empty_commits_dropped_by_no_keep_empty() {
    let fixture = CliFixture::new();
    let repo = fixture.init_repo("update-refs-empty");
    fixture.commit_file(&repo, "base.txt", "base\n", "base");
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["switch", "feature"]);
    let retained = fixture.commit_file(&repo, "feature.txt", "feature\n", "feature-change");
    fixture.success(
        &repo,
        &["commit", "--allow-empty", "-s", "-m", "empty-change"],
    );
    let empty = fixture.oid(&repo, "HEAD");
    fixture.success(&repo, &["branch", "points-at-empty", &empty]);
    fixture.success(&repo, &["branch", "points-at-retained", &retained]);
    fixture.success(&repo, &["switch", "main"]);
    let main_tip = fixture.commit_file(&repo, "main.txt", "main\n", "main-change");
    fixture.success(&repo, &["switch", "feature"]);

    fixture.success(
        &repo,
        &["rebase", "--update-refs", "--no-keep-empty", "main"],
    );

    let rewritten_retained = fixture.oid(&repo, "points-at-retained");
    assert_ne!(rewritten_retained, retained);
    assert_eq!(fixture.oid(&repo, "points-at-empty"), rewritten_retained);
    assert_eq!(fixture.oid(&repo, "points-at-retained^"), main_tip);
}

#[test]
fn rebase_fork_point_uses_an_upstream_reflog_tip_instead_of_replaying_it() {
    let fixture = CliFixture::new();
    let (repo, new_upstream) = force_moved_upstream(&fixture, "fork-point");

    let output = fixture.success(
        &repo,
        &["rebase", "--fork-point", "--json=compact", "upstream"],
    );
    let payload: Value = serde_json::from_slice(&output.stdout).expect("parse rebase JSON");
    assert_eq!(payload["data"]["replay_count"], 1);
    assert_eq!(fixture.oid(&repo, "HEAD^"), new_upstream);
}

#[test]
fn rebase_fork_point_toggle_is_last_wins() {
    let fixture = CliFixture::new();

    let (ordinary_repo, _) = force_moved_upstream(&fixture, "fork-point-disabled");
    let ordinary = fixture.success(
        &ordinary_repo,
        &[
            "rebase",
            "--fork-point",
            "--no-fork-point",
            "--json=compact",
            "upstream",
        ],
    );
    let ordinary: Value = serde_json::from_slice(&ordinary.stdout).expect("ordinary JSON");
    assert_eq!(ordinary["data"]["replay_count"], 2);

    let (fork_repo, _) = force_moved_upstream(&fixture, "fork-point-enabled");
    let fork = fixture.success(
        &fork_repo,
        &[
            "rebase",
            "--no-fork-point",
            "--fork-point",
            "--json=compact",
            "upstream",
        ],
    );
    let fork: Value = serde_json::from_slice(&fork.stdout).expect("fork-point JSON");
    assert_eq!(fork["data"]["replay_count"], 1);
}

#[test]
fn merge_strategy_option_ours_keeps_clean_target_hunks() {
    let fixture = CliFixture::new();
    let (repo, main_tip, feature_tip) = conflicting_merge_repo(&fixture, "merge-x-ours");

    fixture.success(&repo, &["merge", "-X", "theirs", "-X", "ours", "feature"]);

    assert_eq!(
        fs::read_to_string(repo.join("shared.txt")).expect("read favored merge"),
        "top\nOURS\nmiddle\ntheirs-clean\n",
        "-X ours must choose ours only for the conflicting hunk"
    );
    assert_eq!(fixture.oid(&repo, "HEAD^"), main_tip);
    assert_eq!(fixture.oid(&repo, "HEAD^2"), feature_tip);
}

#[test]
fn merge_strategy_option_theirs_resolves_conflicting_hunks() {
    let fixture = CliFixture::new();
    let (repo, main_tip, feature_tip) = conflicting_merge_repo(&fixture, "merge-x-theirs");

    fixture.success(&repo, &["merge", "-Xours", "-Xtheirs", "feature"]);

    assert_eq!(
        fs::read_to_string(repo.join("shared.txt")).expect("read favored merge"),
        "top\nTHEIRS\nmiddle\ntheirs-clean\n"
    );
    assert_eq!(fixture.oid(&repo, "HEAD^"), main_tip);
    assert_eq!(fixture.oid(&repo, "HEAD^2"), feature_tip);
}

#[test]
fn merge_ours_strategy_records_parents_but_retains_current_tree() {
    let fixture = CliFixture::new();
    let (repo, main_tip) = divergent_feature(&fixture, "merge-strategy-ours", 2);
    let feature_tip = fixture.oid(&repo, "HEAD");
    fixture.success(&repo, &["switch", "main"]);

    let output = fixture.success(&repo, &["merge", "-s", "ours", "--json=compact", "feature"]);
    let payload: Value = serde_json::from_slice(&output.stdout).expect("parse merge JSON");
    assert_eq!(payload["data"]["strategy"], "ours");
    assert_eq!(payload["data"]["files_changed"], 0);
    assert_eq!(fixture.oid(&repo, "HEAD^"), main_tip);
    assert_eq!(fixture.oid(&repo, "HEAD^2"), feature_tip);
    assert!(repo.join("main.txt").exists());
    assert!(!repo.join("feature-1.txt").exists());
    assert!(!repo.join("feature-2.txt").exists());
}

#[test]
fn merge_ours_no_commit_continue_preserves_strategy_and_tree() {
    let fixture = CliFixture::new();
    let (repo, main_tip) = divergent_feature(&fixture, "merge-strategy-ours-continue", 1);
    let feature_tip = fixture.oid(&repo, "HEAD");
    fixture.success(&repo, &["switch", "main"]);

    fixture.success(&repo, &["merge", "-s", "ours", "--no-commit", "feature"]);
    assert_eq!(fixture.oid(&repo, "HEAD"), main_tip);
    assert!(repo.join(".libra/merge-state.json").exists());
    assert!(!repo.join("feature-1.txt").exists());

    let output = fixture.success(&repo, &["merge", "--continue", "--json=compact"]);
    let payload: Value = serde_json::from_slice(&output.stdout).expect("parse continue JSON");
    assert_eq!(payload["data"]["strategy"], "ours");
    assert_eq!(fixture.oid(&repo, "HEAD^"), main_tip);
    assert_eq!(fixture.oid(&repo, "HEAD^2"), feature_tip);
    assert!(repo.join("main.txt").exists());
    assert!(!repo.join("feature-1.txt").exists());
    assert!(!repo.join(".libra/merge-state.json").exists());
}

#[test]
fn merge_allow_unrelated_histories_combines_root_trees() {
    let fixture = CliFixture::new();
    let repo = fixture.init_repo("merge-unrelated-clean");
    let main_tip = fixture.commit_file(&repo, "main-root.txt", "main root\n", "main-root");
    fixture.success(&repo, &["switch", "--orphan", "unrelated"]);
    let unrelated_tip = fixture.commit_file(&repo, "other-root.txt", "other root\n", "other-root");
    fixture.success(&repo, &["switch", "main"]);

    let refused = fixture.run(&repo, &["merge", "unrelated"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("refusing to merge unrelated histories")
    );
    assert_eq!(fixture.oid(&repo, "HEAD"), main_tip);

    fixture.success(
        &repo,
        &["merge", "--allow-unrelated-histories", "unrelated"],
    );
    assert_eq!(fixture.oid(&repo, "HEAD^"), main_tip);
    assert_eq!(fixture.oid(&repo, "HEAD^2"), unrelated_tip);
    assert_eq!(
        fs::read_to_string(repo.join("main-root.txt")).expect("read main root"),
        "main root\n"
    );
    assert_eq!(
        fs::read_to_string(repo.join("other-root.txt")).expect("read other root"),
        "other root\n"
    );
}

#[test]
fn merge_unrelated_conflict_restart_and_continue_round_trip() {
    let fixture = CliFixture::new();
    let repo = fixture.init_repo("merge-unrelated-conflict");
    let main_tip = fixture.commit_file(&repo, "shared.txt", "main root\n", "main-root");
    fixture.success(&repo, &["switch", "--orphan", "unrelated"]);
    let unrelated_tip = fixture.commit_file(&repo, "shared.txt", "other root\n", "other-root");
    fixture.success(&repo, &["switch", "main"]);

    let conflict = fixture.run(
        &repo,
        &["merge", "--allow-unrelated-histories", "unrelated"],
    );
    assert!(!conflict.status.success());
    assert!(repo.join(".libra/merge-state.json").exists());

    let restarted = fixture.run(&repo, &["merge", "--restart"]);
    assert!(!restarted.status.success());
    let restart_stderr = String::from_utf8_lossy(&restarted.stderr);
    assert!(
        restart_stderr.contains("merge has conflicts"),
        "{restart_stderr}"
    );
    assert!(
        !restart_stderr.contains("unrelated histories"),
        "--restart must replay the unrelated-history permission: {restart_stderr}"
    );

    fs::write(repo.join("shared.txt"), "resolved roots\n").expect("resolve root conflict");
    fixture.success(&repo, &["add", "shared.txt"]);
    fixture.success(&repo, &["merge", "--continue"]);
    assert_eq!(fixture.oid(&repo, "HEAD^"), main_tip);
    assert_eq!(fixture.oid(&repo, "HEAD^2"), unrelated_tip);
    assert_eq!(
        fs::read_to_string(repo.join("shared.txt")).expect("read resolution"),
        "resolved roots\n"
    );
}

#[test]
fn merge_log_with_custom_message_survives_conflict_continue() {
    let fixture = CliFixture::new();
    let repo = fixture.init_repo("merge-log-continue");
    fixture.commit_file(&repo, "shared.txt", "base\n", "base");
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["switch", "feature"]);
    fixture.commit_file(&repo, "shared.txt", "feature\n", "feature-conflict");
    fixture.commit_file(&repo, "feature-note.txt", "note\n", "feature-note");
    fixture.success(&repo, &["switch", "main"]);
    fixture.commit_file(&repo, "shared.txt", "main\n", "main-conflict");

    let conflict = fixture.run(
        &repo,
        &["merge", "-m", "custom merge", "--log=1", "feature"],
    );
    assert!(!conflict.status.success());
    fs::write(repo.join("shared.txt"), "resolved\n").expect("resolve merge conflict");
    fixture.success(&repo, &["add", "shared.txt"]);
    fixture.success(&repo, &["merge", "--continue"]);

    let message = fixture.success(&repo, &["log", "-1", "--pretty=%B"]);
    let message = String::from_utf8_lossy(&message.stdout);
    assert!(message.starts_with("custom merge\n"), "{message}");
    assert!(message.contains("* feature:\n  feature-note"), "{message}");
    assert!(
        !message.contains("feature-conflict"),
        "--log=1 exceeded its limit: {message}"
    );
}

#[test]
fn merge_log_toggle_is_last_wins() {
    let fixture = CliFixture::new();

    let (disabled_repo, _) = divergent_feature(&fixture, "merge-log-disabled", 1);
    fixture.success(&disabled_repo, &["switch", "main"]);
    fixture.success(&disabled_repo, &["merge", "--log", "--no-log", "feature"]);
    let disabled = fixture.success(&disabled_repo, &["log", "-1", "--pretty=%B"]);
    assert!(!String::from_utf8_lossy(&disabled.stdout).contains("* feature:"));

    let (enabled_repo, _) = divergent_feature(&fixture, "merge-log-enabled", 1);
    fixture.success(&enabled_repo, &["switch", "main"]);
    fixture.success(&enabled_repo, &["merge", "--no-log", "--log=1", "feature"]);
    let enabled = fixture.success(&enabled_repo, &["log", "-1", "--pretty=%B"]);
    assert!(String::from_utf8_lossy(&enabled.stdout).contains("* feature:\n  feature-1"));
}

#[test]
fn cherry_pick_strategy_option_is_hunk_level_and_last_wins() {
    let fixture = CliFixture::new();
    let repo = fixture.init_repo("cherry-pick-x");
    fixture.commit_file(&repo, "shared.txt", "top\nbase\nmiddle\nbottom\n", "base");
    fixture.success(&repo, &["branch", "feature"]);
    fixture.success(&repo, &["switch", "feature"]);
    let picked = fixture.commit_file(
        &repo,
        "shared.txt",
        "top\nTHEIRS\nmiddle\ntheirs-clean\n",
        "feature-change",
    );
    fixture.success(&repo, &["switch", "main"]);
    let main_tip = fixture.commit_file(
        &repo,
        "shared.txt",
        "top\nOURS\nmiddle\nbottom\n",
        "main-change",
    );

    fixture.success(&repo, &["cherry-pick", "-Xtheirs", "-X", "ours", &picked]);

    assert_eq!(
        fs::read_to_string(repo.join("shared.txt")).expect("read favored pick"),
        "top\nOURS\nmiddle\ntheirs-clean\n",
        "-X ours must select only the conflicting hunk"
    );
    assert_eq!(fixture.oid(&repo, "HEAD^"), main_tip);
    assert!(!repo.join(".libra/cherry-pick-state.json").exists());
}

fn conflicting_revert_repo(fixture: &CliFixture, name: &str) -> (PathBuf, String, String) {
    let repo = fixture.init_repo(name);
    fixture.commit_file(&repo, "shared.txt", "top\nbase\nmiddle\nbottom\n", "base");
    let reverted = fixture.commit_file(
        &repo,
        "shared.txt",
        "top\nREVERTED\nmiddle\nreverted-clean\n",
        "change-to-revert",
    );
    let current = fixture.commit_file(
        &repo,
        "shared.txt",
        "top\nCURRENT\nmiddle\nreverted-clean\n",
        "later-conflict",
    );
    (repo, reverted, current)
}

#[test]
fn revert_strategy_option_is_hunk_level_and_last_wins() {
    let fixture = CliFixture::new();
    let (repo, reverted, current) = conflicting_revert_repo(&fixture, "revert-x");

    fixture.success(
        &repo,
        &["revert", "-Xtheirs", "--strategy-option=ours", &reverted],
    );

    assert_eq!(
        fs::read_to_string(repo.join("shared.txt")).expect("read favored revert"),
        "top\nCURRENT\nmiddle\nbottom\n",
        "-X ours must preserve the current conflicting hunk and apply the clean inverse hunk"
    );
    assert_eq!(fixture.oid(&repo, "HEAD^"), current);
    assert!(!repo.join(".libra/revert-state.json").exists());
}

#[test]
fn revert_cleanup_survives_conflict_continue() {
    let fixture = CliFixture::new();
    let (repo, reverted, current) = conflicting_revert_repo(&fixture, "revert-cleanup");

    let conflict = fixture.run(&repo, &["revert", "--cleanup=scissors", "-e", &reverted]);
    assert!(
        !conflict.status.success(),
        "revert should stop for a conflict"
    );
    assert!(repo.join(".libra/revert-state.json").exists());

    fs::write(repo.join("shared.txt"), "top\nRESOLVED\nmiddle\nbottom\n")
        .expect("resolve revert conflict");
    fixture.success(&repo, &["add", "shared.txt"]);
    let editor = repo.join("append-scissors.sh");
    fs::write(
        &editor,
        "printf '\\n# ------------------------ >8 ------------------------\\ndiscarded by cleanup\\n' >> \"$1\"\n",
    )
    .expect("write editor helper");
    let editor_command = format!("sh {}", editor.display());
    let continued = fixture
        .command(&repo, &["revert", "--continue"])
        .env("GIT_EDITOR", editor_command)
        .output()
        .expect("spawn revert continue");
    assert!(
        continued.status.success(),
        "continue failed: {}",
        String::from_utf8_lossy(&continued.stderr)
    );

    assert_eq!(fixture.oid(&repo, "HEAD^"), current);
    let message = fixture.success(&repo, &["log", "-1", "--pretty=%B"]);
    let message = String::from_utf8_lossy(&message.stdout);
    assert!(
        message.starts_with("Revert \"change-to-revert\""),
        "{message}"
    );
    assert!(!message.contains("discarded by cleanup"), "{message}");
    assert!(!repo.join(".libra/revert-state.json").exists());
}

#[test]
fn revert_abort_fails_closed_on_a_corrupt_index() {
    let fixture = CliFixture::new();
    let (repo, reverted, current) = conflicting_revert_repo(&fixture, "revert-corrupt-index");
    let conflict = fixture.run(&repo, &["revert", &reverted]);
    assert!(
        !conflict.status.success(),
        "fixture must enter revert state"
    );
    let state_path = repo.join(".libra/revert-state.json");
    assert!(state_path.exists());
    let conflicted_worktree =
        fs::read(repo.join("shared.txt")).expect("read conflicted worktree before abort");
    fs::write(repo.join(".libra/index"), b"not an index")
        .expect("corrupt index for fail-closed regression");

    let abort = fixture.run(&repo, &["revert", "--abort", "--json=compact"]);
    assert!(!abort.status.success(), "abort must reject a corrupt index");
    assert!(
        String::from_utf8_lossy(&abort.stderr).contains("failed to load index"),
        "{}",
        String::from_utf8_lossy(&abort.stderr)
    );
    assert_eq!(fixture.oid(&repo, "HEAD"), current);
    assert_eq!(
        fs::read(repo.join("shared.txt")).expect("read worktree after refused abort"),
        conflicted_worktree
    );
    assert!(state_path.exists(), "failed abort must remain recoverable");
}

fn reset_preservation_repo(fixture: &CliFixture, name: &str) -> (PathBuf, String, String) {
    let repo = fixture.init_repo(name);
    let base = fixture.commit_file(&repo, "changed.txt", "old\n", "base");
    fixture.commit_file(&repo, "local.txt", "local base\n", "local-base");
    let target = fixture.oid(&repo, "HEAD");
    fixture.commit_file(&repo, "changed.txt", "new\n", "changed-at-head");
    assert_ne!(base, target);
    let head = fixture.oid(&repo, "HEAD");
    (repo, target, head)
}

#[test]
fn reset_merge_preserves_unstaged_changes_and_moves_to_target() {
    let fixture = CliFixture::new();
    let (repo, target, head) = reset_preservation_repo(&fixture, "reset-merge");
    fs::write(repo.join("local.txt"), "local dirty\n").expect("write local change");

    let output = fixture.success(&repo, &["reset", "--merge", "--json=compact", &target]);
    let payload: Value = serde_json::from_slice(&output.stdout).expect("parse reset JSON");
    assert_eq!(payload["data"]["mode"], "merge");
    assert_eq!(payload["data"]["previous_commit"], head);
    assert_eq!(fixture.oid(&repo, "HEAD"), target);
    assert_eq!(
        fs::read_to_string(repo.join("changed.txt")).expect("read reset path"),
        "old\n"
    );
    assert_eq!(
        fs::read_to_string(repo.join("local.txt")).expect("read preserved path"),
        "local dirty\n"
    );
}

#[test]
fn reset_keep_preserves_unaffected_changes_and_moves_to_target() {
    let fixture = CliFixture::new();
    let (repo, target, head) = reset_preservation_repo(&fixture, "reset-keep");
    fs::write(repo.join("local.txt"), "local dirty\n").expect("write local change");

    let output = fixture.success(&repo, &["reset", "--keep", "--json=compact", &target]);
    let payload: Value = serde_json::from_slice(&output.stdout).expect("parse reset JSON");
    assert_eq!(payload["data"]["mode"], "keep");
    assert_eq!(payload["data"]["previous_commit"], head);
    assert_eq!(fixture.oid(&repo, "HEAD"), target);
    assert_eq!(
        fs::read_to_string(repo.join("changed.txt")).expect("read reset path"),
        "old\n"
    );
    assert_eq!(
        fs::read_to_string(repo.join("local.txt")).expect("read preserved path"),
        "local dirty\n"
    );
}

#[test]
fn reset_merge_and_keep_refuse_overwrite_atomically() {
    let fixture = CliFixture::new();
    for mode in ["--merge", "--keep"] {
        let (repo, target, head) =
            reset_preservation_repo(&fixture, &format!("reset-refuse-{}", &mode[2..]));
        fs::write(repo.join("changed.txt"), "local conflict\n")
            .expect("write conflicting local change");
        let before_index = fixture.success(&repo, &["ls-files", "--stage"]);

        let reset = fixture.run(&repo, &["reset", mode, &target]);
        assert!(!reset.status.success(), "{mode} must refuse the overwrite");
        assert!(
            String::from_utf8_lossy(&reset.stderr).contains("would be overwritten"),
            "{}",
            String::from_utf8_lossy(&reset.stderr)
        );
        assert_eq!(fixture.oid(&repo, "HEAD"), head);
        assert_eq!(
            fs::read_to_string(repo.join("changed.txt")).expect("read unchanged worktree"),
            "local conflict\n"
        );
        let after_index = fixture.success(&repo, &["ls-files", "--stage"]);
        assert_eq!(
            after_index.stdout, before_index.stdout,
            "{mode} changed index"
        );
    }
}

#[test]
fn reset_merge_discards_safe_staged_change_while_keep_refuses() {
    let fixture = CliFixture::new();

    let (merge_repo, merge_target, _) = reset_preservation_repo(&fixture, "reset-merge-staged");
    fs::write(merge_repo.join("changed.txt"), "staged local\n").expect("write staged merge change");
    fixture.success(&merge_repo, &["add", "changed.txt"]);
    fixture.success(&merge_repo, &["reset", "--merge", &merge_target]);
    assert_eq!(fixture.oid(&merge_repo, "HEAD"), merge_target);
    assert_eq!(
        fs::read_to_string(merge_repo.join("changed.txt")).expect("read merged staged path"),
        "old\n"
    );

    let (keep_repo, keep_target, keep_head) =
        reset_preservation_repo(&fixture, "reset-keep-staged");
    fs::write(keep_repo.join("changed.txt"), "staged local\n").expect("write staged keep change");
    fixture.success(&keep_repo, &["add", "changed.txt"]);
    let before_index = fixture.success(&keep_repo, &["ls-files", "--stage"]);
    let keep = fixture.run(&keep_repo, &["reset", "--keep", &keep_target]);
    assert!(
        !keep.status.success(),
        "--keep must refuse staged affected changes"
    );
    assert_eq!(fixture.oid(&keep_repo, "HEAD"), keep_head);
    assert_eq!(
        fs::read_to_string(keep_repo.join("changed.txt")).expect("read refused staged path"),
        "staged local\n"
    );
    let after_index = fixture.success(&keep_repo, &["ls-files", "--stage"]);
    assert_eq!(after_index.stdout, before_index.stdout);
}

#[test]
fn reset_merge_head_discards_safe_staged_change() {
    let fixture = CliFixture::new();
    let repo = fixture.init_repo("reset-merge-head-staged");
    fixture.commit_file(&repo, "changed.txt", "committed\n", "base");
    fs::write(repo.join("changed.txt"), "staged local\n").expect("write staged merge change");
    fixture.success(&repo, &["add", "changed.txt"]);

    fixture.success(&repo, &["reset", "--merge", "HEAD"]);

    assert_eq!(
        fs::read_to_string(repo.join("changed.txt")).expect("read reset worktree path"),
        "committed\n"
    );
    let diff = fixture.success(&repo, &["diff", "--exit-code"]);
    assert!(diff.stdout.is_empty());
    let cached = fixture.success(&repo, &["diff", "--cached", "--exit-code"]);
    assert!(cached.stdout.is_empty());
}

#[test]
fn reset_merge_and_keep_refuse_untracked_overwrite_atomically() {
    let fixture = CliFixture::new();
    for mode in ["--merge", "--keep"] {
        let repo = fixture.init_repo(&format!("reset-untracked-{}", &mode[2..]));
        let base = fixture.commit_file(&repo, "base.txt", "base\n", "base");
        let target = fixture.commit_file(&repo, "incoming.txt", "tracked\n", "incoming");
        fixture.success(&repo, &["reset", "--hard", &base]);
        fs::write(repo.join("incoming.txt"), "untracked local\n")
            .expect("write untracked collision");
        let before_index = fixture.success(&repo, &["ls-files", "--stage"]);

        let reset = fixture.run(&repo, &["reset", mode, &target]);
        assert!(
            !reset.status.success(),
            "{mode} must refuse untracked overwrite"
        );
        assert!(
            String::from_utf8_lossy(&reset.stderr).contains("would be overwritten"),
            "{}",
            String::from_utf8_lossy(&reset.stderr)
        );
        assert_eq!(fixture.oid(&repo, "HEAD"), base);
        assert_eq!(
            fs::read_to_string(repo.join("incoming.txt")).expect("read untracked collision"),
            "untracked local\n"
        );
        let after_index = fixture.success(&repo, &["ls-files", "--stage"]);
        assert_eq!(
            after_index.stdout, before_index.stdout,
            "{mode} changed index"
        );
    }
}

#[test]
fn reset_merge_and_keep_handle_file_directory_transitions() {
    let fixture = CliFixture::new();
    for mode in ["--merge", "--keep"] {
        let repo = fixture.init_repo(&format!("reset-df-{}", &mode[2..]));
        let file_tip = fixture.commit_file(&repo, "node", "file form\n", "file-form");
        fixture.success(&repo, &["rm", "node"]);
        fs::create_dir_all(repo.join("node")).expect("create directory-form parent");
        let directory_tip =
            fixture.commit_file(&repo, "node/child", "directory form\n", "directory-form");

        fixture.success(&repo, &["reset", mode, &file_tip]);
        assert_eq!(
            fs::read_to_string(repo.join("node")).expect("read file-form reset"),
            "file form\n"
        );

        fixture.success(&repo, &["reset", mode, &directory_tip]);
        assert!(repo.join("node").is_dir());
        assert_eq!(
            fs::read_to_string(repo.join("node/child")).expect("read directory-form reset"),
            "directory form\n"
        );
    }
}

#[cfg(unix)]
#[test]
fn reset_merge_and_keep_never_follow_an_ignored_symlink_ancestor() {
    use std::os::unix::fs::symlink;

    let fixture = CliFixture::new();
    for mode in ["--merge", "--keep"] {
        let repo = fixture.init_repo(&format!("reset-symlink-ancestor-{}", &mode[2..]));
        let base = fixture.commit_file(&repo, "base.txt", "base\n", "base");
        fs::create_dir_all(repo.join("incoming")).expect("create target parent");
        let target = fixture.commit_file(&repo, "incoming/file.txt", "tracked\n", "incoming");
        fixture.success(&repo, &["reset", "--hard", &base]);
        fs::write(repo.join(".libraignore"), "incoming\n").expect("ignore symlink ancestor");
        let outside = fixture.root.join(format!("outside-{}", &mode[2..]));
        fs::create_dir_all(&outside).expect("create outside directory");
        fs::write(outside.join("file.txt"), "outside sentinel\n").expect("write outside sentinel");
        symlink(&outside, repo.join("incoming")).expect("create ignored symlink ancestor");
        let before_index = fixture.success(&repo, &["ls-files", "--stage"]);

        let reset = fixture.run(&repo, &["reset", mode, &target]);
        assert!(
            !reset.status.success(),
            "{mode} must reject symlink traversal"
        );
        assert!(
            String::from_utf8_lossy(&reset.stderr).contains("would be overwritten"),
            "{}",
            String::from_utf8_lossy(&reset.stderr)
        );
        assert_eq!(fixture.oid(&repo, "HEAD"), base);
        assert_eq!(
            fs::read_to_string(outside.join("file.txt")).expect("read outside sentinel"),
            "outside sentinel\n"
        );
        assert!(repo.join("incoming").symlink_metadata().is_ok());
        let after_index = fixture.success(&repo, &["ls-files", "--stage"]);
        assert_eq!(
            after_index.stdout, before_index.stdout,
            "{mode} changed index"
        );
    }
}
