//! Non-interactive history controls for plan-20260708 P1-07a/P1-07b.

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
        "printf 'exec-created\\n' > exec-created.txt && '{libra}' add exec-created.txt && '{libra}' commit -s -m exec-created"
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

    let failed = fixture.run_with_required_system_sandbox(
        &repo,
        &["rebase", "--exec", "touch ../sandbox-escape", "main"],
    );
    assert!(!failed.status.success());
    assert!(
        !escaped.exists(),
        "sandbox command escaped its writable root"
    );
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("LBR-CONFLICT-002"), "stderr was: {stderr}");
    fixture.success(&repo, &["rebase", "--abort"]);
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
        &["worktree", "add", linked.to_str().expect("utf8 linked")],
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
