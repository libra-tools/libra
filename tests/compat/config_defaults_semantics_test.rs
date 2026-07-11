//! High-impact Git config default guards for plan-20260708 P1-05.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[allow(deprecated)]
use libra::internal::{config::Config, db::get_db_conn_instance_for_path};
use serde_json::Value;
use tempfile::{TempDir, tempdir};

#[path = "config_defaults_diff.rs"]
mod diff_defaults;

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

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn libra_command(&self, cwd: &Path, args: &[&str]) -> Command {
        let config_home = self.home.join(".config");
        let global_db = self.home.join(".libra").join("config.db");
        let system_db = self.home.join(".libra").join("system-config.db");
        fs::create_dir_all(&config_home).expect("create isolated config dir");

        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", PATH_ENV)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("LIBRA_CONFIG_GLOBAL_DB", &global_db)
            .env("LIBRA_CONFIG_SYSTEM_DB", &system_db)
            .env("LIBRA_TEST", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C");
        if let Some(profile_file) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile_file);
        }
        command
    }

    fn git_command(&self, cwd: &Path, args: &[&str]) -> Command {
        let git_home = self.home.join("git-home");
        fs::create_dir_all(&git_home).expect("create isolated git home");

        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", PATH_ENV)
            .env("HOME", &git_home)
            .env("USERPROFILE", &git_home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", "Remote User")
            .env("GIT_AUTHOR_EMAIL", "remote@example.com")
            .env("GIT_COMMITTER_NAME", "Remote User")
            .env("GIT_COMMITTER_EMAIL", "remote@example.com")
            .env("LANG", "C")
            .env("LC_ALL", "C");
        command
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.libra_command(cwd, args).output().expect("spawn libra")
    }

    fn success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.run(cwd, args);
        assert_success("libra", args, &output);
        output
    }

    fn git_success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.git_command(cwd, args).output().expect("spawn git");
        assert_success("git", args, &output);
        output
    }

    fn git_stdout(&self, cwd: &Path, args: &[&str]) -> String {
        stdout_trim(&self.git_success(cwd, args))
    }

    fn init_repo(&self, repo: &Path) {
        self.success(&self.root, &["init", "--vault", "false", path_str(repo)]);
        self.success(repo, &["config", "set", "user.name", "Config Test"]);
        self.success(repo, &["config", "set", "user.email", "config@example.com"]);
    }

    #[allow(deprecated)]
    fn legacy_config(
        &self,
        repo: &Path,
        section: &str,
        subsection: Option<&str>,
        variable: &str,
        value: &str,
    ) {
        let db_path = repo.join(".libra").join("libra.db");
        let runtime = tokio::runtime::Runtime::new().expect("create runtime");
        runtime.block_on(async {
            let conn = get_db_conn_instance_for_path(&db_path)
                .await
                .expect("open repo db");
            Config::insert_with_conn(&conn, section, subsection, variable, value).await;
        });
    }

    fn commit_file(&self, repo: &Path, file: &str, content: &str, message: &str) {
        fs::write(repo.join(file), content).expect("write file");
        self.success(repo, &["add", file]);
        self.success(
            repo,
            &["commit", "--no-gpg-sign", "--no-verify", "-m", message],
        );
    }

    fn remote_fixture(&self, name: &str) -> (PathBuf, PathBuf, String) {
        assert!(
            git_available(),
            "pull config compatibility tests require the git binary"
        );

        let remote_dir = self.path(&format!("{name}-remote.git"));
        let work_dir = self.path(&format!("{name}-work"));
        self.git_success(&self.root, &["init", "--bare", path_str(&remote_dir)]);
        self.git_success(&self.root, &["init", path_str(&work_dir)]);
        self.git_success(&work_dir, &["config", "user.name", "Remote User"]);
        self.git_success(&work_dir, &["config", "user.email", "remote@example.com"]);
        fs::write(work_dir.join("README.md"), "base\n").expect("write remote base");
        self.git_success(&work_dir, &["add", "README.md"]);
        self.git_success(&work_dir, &["commit", "-m", "base"]);
        let branch = self.git_stdout(&work_dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
        self.git_success(
            &work_dir,
            &["remote", "add", "origin", path_str(&remote_dir)],
        );
        self.git_success(
            &work_dir,
            &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        );
        (remote_dir, work_dir, branch)
    }

    fn push_remote_commit(&self, work_dir: &Path, branch: &str, file: &str, message: &str) {
        fs::write(work_dir.join(file), format!("{message}\n")).expect("write remote update");
        self.git_success(work_dir, &["add", file]);
        self.git_success(work_dir, &["commit", "-m", message]);
        self.git_success(
            work_dir,
            &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        );
    }

    fn configure_tracking(&self, repo: &Path, remote_dir: &Path, branch: &str) {
        self.success(repo, &["remote", "add", "origin", path_str(remote_dir)]);
        self.success(repo, &["config", "branch.main.remote", "origin"]);
        self.success(
            repo,
            &[
                "config",
                "branch.main.merge",
                &format!("refs/heads/{branch}"),
            ],
        );
    }
}

#[test]
fn init_default_branch_config_sets_initial_head_and_cli_flag_wins() {
    let fixture = Fixture::new();
    fixture.success(
        &fixture.root,
        &["config", "--global", "init.defaultBranch", "trunk"],
    );

    let configured = fixture.path("configured");
    fixture.success(
        &fixture.root,
        &["init", "--vault", "false", path_str(&configured)],
    );
    assert_eq!(
        stdout_trim(&fixture.success(&configured, &["symbolic-ref", "--short", "HEAD"])),
        "trunk"
    );

    let explicit = fixture.path("explicit");
    fixture.success(
        &fixture.root,
        &[
            "init",
            "--vault",
            "false",
            "--initial-branch",
            "topic",
            path_str(&explicit),
        ],
    );
    assert_eq!(
        stdout_trim(&fixture.success(&explicit, &["symbolic-ref", "--short", "HEAD"])),
        "topic"
    );
}

#[test]
fn init_default_branch_local_scope_overrides_global_scope() {
    let fixture = Fixture::new();
    fixture.init_repo(&fixture.root);
    fixture.success(
        &fixture.root,
        &["config", "--global", "init.defaultBranch", "global-trunk"],
    );
    fixture.success(
        &fixture.root,
        &["config", "set", "init.defaultBranch", "local-trunk"],
    );
    let child = fixture.path("local-configured");

    fixture.success(
        &fixture.root,
        &["init", "--vault", "false", path_str(&child)],
    );
    assert_eq!(
        stdout_trim(&fixture.success(&child, &["symbolic-ref", "--short", "HEAD"])),
        "local-trunk"
    );
}

#[test]
fn init_default_branch_legacy_config_row_is_honored() {
    let fixture = Fixture::new();
    fixture.init_repo(&fixture.root);
    fixture.legacy_config(&fixture.root, "INIT", None, "defaultBranch", "legacy-trunk");
    let child = fixture.path("legacy-configured");

    fixture.success(
        &fixture.root,
        &["init", "--vault", "false", path_str(&child)],
    );
    assert_eq!(
        stdout_trim(&fixture.success(&child, &["symbolic-ref", "--short", "HEAD"])),
        "legacy-trunk"
    );
}

#[test]
fn init_default_branch_invalid_config_fails_before_creating_repo() {
    let fixture = Fixture::new();
    fixture.success(
        &fixture.root,
        &["config", "--global", "init.defaultBranch", "bad name"],
    );
    let repo = fixture.path("bad");

    let output = fixture.run(
        &fixture.root,
        &["init", "--vault", "false", path_str(&repo)],
    );

    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    assert!(stderr.contains("bad name"), "{stderr}");
    assert!(!repo.join(".libra").exists());
}

#[test]
fn init_default_branch_uses_system_scope_and_case_insensitive_variable() {
    let fixture = Fixture::new();
    fixture.success(
        &fixture.root,
        &["config", "--system", "init.defaultbranch", "system-trunk"],
    );
    let repo = fixture.path("system-configured");

    fixture.success(
        &fixture.root,
        &["init", "--vault", "false", path_str(&repo)],
    );
    assert_eq!(
        stdout_trim(&fixture.success(&repo, &["symbolic-ref", "--short", "HEAD"])),
        "system-trunk"
    );
}

#[test]
fn init_default_branch_empty_config_fails_before_creating_repo() {
    let fixture = Fixture::new();
    fixture.success(
        &fixture.root,
        &["config", "--global", "init.defaultBranch", ""],
    );
    let repo = fixture.path("empty-default-branch");

    let output = fixture.run(
        &fixture.root,
        &["init", "--vault", "false", path_str(&repo)],
    );

    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    assert!(stderr.contains("init.defaultBranch"), "{stderr}");
    assert!(!repo.join(".libra").exists());
}

#[test]
fn init_default_branch_config_read_failure_is_io_error_before_creating_repo() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.home.join(".libra").join("config.db"))
        .expect("create unreadable config-db directory");
    let repo = fixture.path("config-read-failure");

    let output = fixture.run(
        &fixture.root,
        &["init", "--vault", "false", path_str(&repo)],
    );

    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("init.defaultBranch"), "{stderr}");
    assert!(!repo.join(".libra").exists());
}

#[cfg(unix)]
#[test]
fn init_default_branch_permission_failure_is_io_error_before_creating_repo() {
    let fixture = Fixture::new();
    let config_dir = fixture.home.join(".libra");
    fs::create_dir_all(&config_dir).expect("create config dir");
    fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o000))
        .expect("make config dir inaccessible");
    let repo = fixture.path("permission-failure");

    let output = fixture.run(
        &fixture.root,
        &["init", "--vault", "false", path_str(&repo)],
    );

    fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o700))
        .expect("restore config dir permissions");
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("init.defaultBranch"), "{stderr}");
    assert!(!repo.join(".libra").exists());
}

#[test]
fn pull_rebase_system_config_changes_advice_and_branch_override_wins() {
    let fixture = Fixture::new();
    let repo = fixture.path("pull-rebase-advice");
    fixture.init_repo(&repo);

    let default = fixture.run(&repo, &["pull"]);
    assert!(
        String::from_utf8_lossy(&default.stderr).contains("merge with"),
        "default pull advice should describe merge:\n{}",
        String::from_utf8_lossy(&default.stderr)
    );

    fixture.success(
        &fixture.root,
        &["config", "--system", "pull.Rebase", "true"],
    );
    let configured = fixture.run(&repo, &["pull"]);
    assert!(
        String::from_utf8_lossy(&configured.stderr).contains("rebase against"),
        "pull.rebase=true should describe rebase:\n{}",
        String::from_utf8_lossy(&configured.stderr)
    );

    fixture.success(&repo, &["config", "branch.main.rebase", "false"]);
    let branch_override = fixture.run(&repo, &["pull"]);
    assert!(
        String::from_utf8_lossy(&branch_override.stderr).contains("merge with"),
        "branch.main.rebase=false should override pull.rebase=true:\n{}",
        String::from_utf8_lossy(&branch_override.stderr)
    );
}

#[test]
fn pull_rebase_config_rebases_and_cli_no_rebase_overrides() {
    let fixture = Fixture::new();
    let (remote_dir, work_dir, branch) = fixture.remote_fixture("rebase-config");
    let repo = fixture.path("rebase-config-local");
    fixture.init_repo(&repo);
    fixture.configure_tracking(&repo, &remote_dir, &branch);
    fixture.success(&repo, &["pull"]);

    fixture.push_remote_commit(&work_dir, &branch, "remote.txt", "remote update");
    fixture.commit_file(&repo, "local.txt", "local change\n", "local update");
    fixture.legacy_config(&repo, "PULL", None, "Rebase", "true");
    fixture.success(&repo, &["pull"]);

    let parents = stdout_trim(&fixture.success(&repo, &["log", "-1", "--format=%P"]));
    assert_eq!(parents.split_whitespace().count(), 1);
    assert_eq!(
        stdout_trim(&fixture.success(&repo, &["log", "-1", "--format=%s"])),
        "local update"
    );

    let (remote_dir, work_dir, branch) = fixture.remote_fixture("rebase-cli");
    let repo = fixture.path("rebase-cli-local");
    fixture.init_repo(&repo);
    fixture.configure_tracking(&repo, &remote_dir, &branch);
    fixture.success(&repo, &["pull"]);
    fixture.push_remote_commit(&work_dir, &branch, "remote.txt", "remote update");
    fixture.commit_file(&repo, "local.txt", "local change\n", "local update");
    fixture.success(&repo, &["config", "pull.rebase", "true"]);
    fixture.success(&repo, &["pull", "--no-rebase"]);

    let parents = stdout_trim(&fixture.success(&repo, &["log", "-1", "--format=%P"]));
    assert_eq!(
        parents.split_whitespace().count(),
        2,
        "--no-rebase must override pull.rebase=true"
    );
}

#[test]
fn pull_rebase_invalid_config_is_usage_error() {
    let fixture = Fixture::new();
    let repo = fixture.path("pull-rebase-invalid");
    fixture.init_repo(&repo);
    fixture.success(&repo, &["config", "pull.rebase", "maybe"]);

    let output = fixture.run(&repo, &["pull"]);

    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    assert!(stderr.contains("pull.rebase"), "{stderr}");
    assert!(stderr.contains("maybe"), "{stderr}");
}

#[test]
fn pull_rebase_unsupported_modes_are_reported_explicitly() {
    for mode in ["merges", "interactive", "m", "i"] {
        let fixture = Fixture::new();
        let repo = fixture.path(&format!("pull-rebase-{mode}"));
        fixture.init_repo(&repo);
        fixture.success(&repo, &["config", "pull.rebase", mode]);

        let output = fixture.run(&repo, &["pull"]);

        assert_eq!(output.status.code(), Some(129), "mode={mode}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("LBR-CLI-002"), "mode={mode}: {stderr}");
        assert!(stderr.contains("unsupported"), "mode={mode}: {stderr}");
        assert!(stderr.contains(mode), "mode={mode}: {stderr}");
    }
}

#[test]
fn pull_rebase_empty_config_is_usage_error_before_fetch() {
    let fixture = Fixture::new();
    let repo = fixture.path("pull-rebase-empty");
    fixture.init_repo(&repo);
    fixture.success(&fixture.root, &["config", "--global", "pull.rebase", ""]);

    let output = fixture.run(&repo, &["pull"]);

    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    assert!(stderr.contains("pull.rebase"), "{stderr}");
    assert!(!repo.join(".libra").join("FETCH_HEAD").exists());
}

#[test]
fn pull_branch_rebase_invalid_config_is_usage_error() {
    let fixture = Fixture::new();
    let repo = fixture.path("branch-rebase-invalid");
    fixture.init_repo(&repo);
    fixture.success(&repo, &["config", "branch.main.Rebase", "maybe"]);

    let output = fixture.run(&repo, &["pull"]);

    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    assert!(stderr.contains("branch.main.rebase"), "{stderr}");
    assert!(stderr.contains("maybe"), "{stderr}");
}

#[test]
fn pull_ff_invalid_config_fails_before_fetch() {
    let fixture = Fixture::new();
    let (remote_dir, _work_dir, branch) = fixture.remote_fixture("ff-invalid");
    let repo = fixture.path("ff-invalid-local");
    fixture.init_repo(&repo);
    fixture.configure_tracking(&repo, &remote_dir, &branch);
    fixture.success(&repo, &["config", "pull.FF", "maybe"]);

    let output = fixture.run(&repo, &["pull"]);

    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    assert!(stderr.contains("pull.ff"), "{stderr}");
    assert!(stderr.contains("maybe"), "{stderr}");
    assert!(
        !repo.join(".libra").join("FETCH_HEAD").exists(),
        "invalid pull.ff must fail before fetch writes FETCH_HEAD"
    );
}

#[test]
fn pull_config_read_failure_is_io_error_before_fetch() {
    let fixture = Fixture::new();
    let repo = fixture.path("pull-config-read-failure");
    fixture.init_repo(&repo);
    fs::create_dir_all(fixture.home.join(".libra").join("config.db"))
        .expect("create unreadable config-db directory");

    let output = fixture.run(&repo, &["pull"]);

    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("branch.main.rebase"), "{stderr}");
    assert!(!repo.join(".libra").join("FETCH_HEAD").exists());
}

#[test]
fn pull_ff_only_config_rejects_diverged_history() {
    let fixture = Fixture::new();
    let (remote_dir, work_dir, branch) = fixture.remote_fixture("ff-only");
    let repo = fixture.path("ff-only-local");
    fixture.init_repo(&repo);
    fixture.configure_tracking(&repo, &remote_dir, &branch);
    fixture.success(&repo, &["pull"]);

    fixture.push_remote_commit(&work_dir, &branch, "remote.txt", "remote update");
    fixture.commit_file(&repo, "local.txt", "local change\n", "local update");
    fixture.success(&repo, &["config", "pull.ff", "only"]);

    let output = fixture.run(&repo, &["--json", "pull"]);

    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-CONFLICT-002"), "{stderr}");
    assert!(stderr.contains("non-fast-forward"), "{stderr}");
}

#[test]
fn pull_ff_false_config_forces_merge_commit_on_fast_forwardable_update() {
    let fixture = Fixture::new();
    let (remote_dir, work_dir, branch) = fixture.remote_fixture("no-ff");
    let repo = fixture.path("no-ff-local");
    fixture.init_repo(&repo);
    fixture.configure_tracking(&repo, &remote_dir, &branch);
    fixture.success(&repo, &["pull"]);

    fixture.push_remote_commit(&work_dir, &branch, "remote.txt", "remote update");
    fixture.success(&repo, &["config", "pull.ff", "false"]);
    fixture.success(&repo, &["pull"]);

    let parents = stdout_trim(&fixture.success(&repo, &["log", "-1", "--format=%P"]));
    assert_eq!(
        parents.split_whitespace().count(),
        2,
        "pull.ff=false should force a two-parent merge commit, got parents: {parents}"
    );
}

#[test]
fn pull_ff_true_config_allows_fast_forward() {
    let fixture = Fixture::new();
    let (remote_dir, work_dir, branch) = fixture.remote_fixture("ff-true");
    let repo = fixture.path("ff-true-local");
    fixture.init_repo(&repo);
    fixture.configure_tracking(&repo, &remote_dir, &branch);
    fixture.success(&repo, &["pull"]);

    fixture.push_remote_commit(&work_dir, &branch, "remote.txt", "remote update");
    fixture.success(&repo, &["config", "pull.FF", "true"]);
    fixture.success(&repo, &["pull"]);

    let parents = stdout_trim(&fixture.success(&repo, &["log", "-1", "--format=%P"]));
    assert_eq!(
        parents.split_whitespace().count(),
        1,
        "pull.ff=true should retain fast-forward behavior"
    );
}

#[test]
fn pull_commit_flag_selects_merge_without_overriding_pull_ff() {
    let fixture = Fixture::new();
    let (remote_dir, work_dir, branch) = fixture.remote_fixture("commit-override");
    let repo = fixture.path("commit-override-local");
    fixture.init_repo(&repo);
    fixture.configure_tracking(&repo, &remote_dir, &branch);
    fixture.success(&repo, &["pull"]);

    fixture.push_remote_commit(&work_dir, &branch, "remote.txt", "remote update");
    fixture.success(&repo, &["config", "pull.ff", "only"]);
    fixture.success(&repo, &["config", "pull.rebase", "true"]);
    fixture.success(&repo, &["pull", "--commit"]);

    let parents = stdout_trim(&fixture.success(&repo, &["log", "-1", "--format=%P"]));
    assert_eq!(
        parents.split_whitespace().count(),
        1,
        "--commit must override configured rebase without overriding pull.ff=only"
    );
}

#[test]
fn pull_cli_ff_flags_override_configured_ff_modes() {
    let fixture = Fixture::new();
    let (remote_dir, work_dir, branch) = fixture.remote_fixture("ff-cli");
    let repo = fixture.path("ff-cli-local");
    fixture.init_repo(&repo);
    fixture.configure_tracking(&repo, &remote_dir, &branch);
    fixture.success(&repo, &["pull"]);

    fixture.push_remote_commit(&work_dir, &branch, "allow-ff.txt", "allow fast-forward");
    fixture.success(&repo, &["config", "pull.ff", "false"]);
    fixture.success(&repo, &["pull", "--ff"]);
    let parents = stdout_trim(&fixture.success(&repo, &["log", "-1", "--format=%P"]));
    assert_eq!(
        parents.split_whitespace().count(),
        1,
        "--ff must override pull.ff=false"
    );

    fixture.push_remote_commit(&work_dir, &branch, "force-merge.txt", "force merge");
    fixture.success(&repo, &["config", "pull.ff", "true"]);
    fixture.success(&repo, &["pull", "--no-ff"]);
    let parents = stdout_trim(&fixture.success(&repo, &["log", "-1", "--format=%P"]));
    assert_eq!(
        parents.split_whitespace().count(),
        2,
        "--no-ff must override pull.ff=true"
    );

    fixture.push_remote_commit(&work_dir, &branch, "ff-only.txt", "reject non-fast-forward");
    fixture.success(&repo, &["config", "pull.ff", "false"]);
    let output = fixture.run(&repo, &["pull", "--ff-only"]);
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-CONFLICT-002"), "stderr: {stderr}");
    assert!(stderr.contains("non-fast-forward"), "stderr: {stderr}");
}

#[test]
fn pull_config_selected_rebase_is_present_in_json_output() {
    let fixture = Fixture::new();
    let (remote_dir, work_dir, branch) = fixture.remote_fixture("rebase-json");
    let repo = fixture.path("rebase-json-local");
    fixture.init_repo(&repo);
    fixture.configure_tracking(&repo, &remote_dir, &branch);
    fixture.success(&repo, &["pull"]);

    fixture.push_remote_commit(&work_dir, &branch, "remote.txt", "remote update");
    fixture.commit_file(&repo, "local.txt", "local change\n", "local update");
    fixture.success(&repo, &["config", "pull.rebase", "true"]);
    fixture.success(&repo, &["config", "pull.ff", "not-a-merge-policy"]);

    let output = fixture.success(&repo, &["--json", "pull"]);
    let report: Value = serde_json::from_slice(&output.stdout).expect("valid pull JSON");
    assert!(report["data"]["rebase"].is_object(), "report: {report}");
    assert!(report["data"]["merge"].is_null(), "report: {report}");
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .env_clear()
        .env("PATH", PATH_ENV)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("fixture path is utf8")
}

fn stdout_trim(output: &Output) -> String {
    String::from_utf8(output.stdout.clone())
        .expect("stdout should be utf8")
        .trim()
        .to_string()
}

fn assert_success(program: &str, args: &[&str], output: &Output) {
    assert!(
        output.status.success(),
        "{} {} failed\nstdout:\n{}\nstderr:\n{}",
        program,
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── P1-05c: fetch.prune / remote.<name>.prune defaults ──────────────────────

fn fetch_prune_local(fixture: &Fixture, name: &str) -> (PathBuf, PathBuf, PathBuf, String) {
    let (remote_dir, work_dir, branch) = fixture.remote_fixture(name);
    // A second remote branch that will later go stale.
    fixture.git_success(&work_dir, &["push", "origin", "HEAD:refs/heads/side"]);
    let repo = fixture.path(&format!("{name}-local"));
    fixture.init_repo(&repo);
    fixture.success(&repo, &["remote", "add", "origin", path_str(&remote_dir)]);
    fixture.success(&repo, &["fetch", "origin"]);
    assert!(
        has_tracking_ref(fixture, &repo, "side"),
        "fixture must start with a live origin/side"
    );
    // Delete the branch on the remote so origin/side becomes stale.
    fixture.git_success(&remote_dir, &["update-ref", "-d", "refs/heads/side"]);
    (remote_dir, work_dir, repo, branch)
}

fn has_tracking_ref(fixture: &Fixture, repo: &Path, name: &str) -> bool {
    fixture
        .run(repo, &["rev-parse", &format!("refs/remotes/origin/{name}")])
        .status
        .success()
}

#[test]
fn fetch_prune_config_removes_stale_tracking_ref_without_cli_flag() {
    let fixture = Fixture::new();

    // Local scope.
    let (_remote, _work, repo, branch) = fetch_prune_local(&fixture, "prune-local");
    fixture.success(&repo, &["config", "fetch.prune", "true"]);
    fixture.success(&repo, &["fetch", "origin"]);
    assert!(
        !has_tracking_ref(&fixture, &repo, "side"),
        "fetch.prune=true must prune the stale ref without --prune"
    );
    assert!(
        has_tracking_ref(&fixture, &repo, &branch),
        "the live branch must survive pruning"
    );

    // Global scope resolves through the same cascade.
    let (_remote, _work, repo, _branch) = fetch_prune_local(&fixture, "prune-global");
    fixture.success(&repo, &["config", "--global", "fetch.prune", "true"]);
    fixture.success(&repo, &["fetch", "origin"]);
    assert!(
        !has_tracking_ref(&fixture, &repo, "side"),
        "a global fetch.prune=true must be honored"
    );
    fixture.success(&repo, &["config", "--global", "--unset", "fetch.prune"]);
}

#[test]
fn remote_scoped_prune_config_overrides_fetch_prune() {
    let fixture = Fixture::new();
    let (_remote, _work, repo, _branch) = fetch_prune_local(&fixture, "prune-remote-scope");

    fixture.success(&repo, &["config", "fetch.prune", "true"]);
    fixture.success(&repo, &["config", "remote.origin.prune", "false"]);
    fixture.success(&repo, &["fetch", "origin"]);
    assert!(
        has_tracking_ref(&fixture, &repo, "side"),
        "remote.origin.prune=false must override fetch.prune=true"
    );

    fixture.success(&repo, &["config", "--unset", "fetch.prune"]);
    fixture.success(&repo, &["config", "remote.origin.prune", "true"]);
    fixture.success(&repo, &["fetch", "origin"]);
    assert!(
        !has_tracking_ref(&fixture, &repo, "side"),
        "remote.origin.prune=true alone must enable pruning"
    );
}

#[test]
fn fetch_cli_prune_flags_override_configured_prune() {
    let fixture = Fixture::new();
    let (_remote, _work, repo, _branch) = fetch_prune_local(&fixture, "prune-cli");

    fixture.success(&repo, &["config", "fetch.prune", "true"]);
    fixture.success(&repo, &["fetch", "--no-prune", "origin"]);
    assert!(
        has_tracking_ref(&fixture, &repo, "side"),
        "--no-prune must override fetch.prune=true"
    );

    fixture.success(&repo, &["config", "fetch.prune", "false"]);
    fixture.success(&repo, &["config", "remote.origin.prune", "false"]);
    fixture.success(&repo, &["fetch", "--prune", "origin"]);
    assert!(
        !has_tracking_ref(&fixture, &repo, "side"),
        "--prune must override configured false values"
    );
}

#[test]
fn fetch_prune_invalid_config_fails_before_fetch() {
    let fixture = Fixture::new();
    let (remote_dir, work_dir, branch) = fixture.remote_fixture("prune-invalid");
    let repo = fixture.path("prune-invalid-local");
    fixture.init_repo(&repo);
    fixture.success(&repo, &["remote", "add", "origin", path_str(&remote_dir)]);
    fixture.success(&repo, &["fetch", "origin"]);
    let before = stdout_trim(&fixture.success(
        &repo,
        &["rev-parse", &format!("refs/remotes/origin/{branch}")],
    ));

    // Advance the remote so a fetch WOULD move the tracking ref.
    fixture.push_remote_commit(&work_dir, &branch, "advance.txt", "remote advance");

    for key in ["fetch.prune", "remote.origin.prune"] {
        fixture.success(&repo, &["config", key, "sometimes"]);
        let rejected = fixture.run(&repo, &["fetch", "origin"]);
        assert_eq!(
            rejected.status.code(),
            Some(129),
            "invalid {key} must be a usage error"
        );
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains(key),
            "the error must name the offending key {key}"
        );
        fixture.success(&repo, &["config", "--unset", key]);
    }

    let after = stdout_trim(&fixture.success(
        &repo,
        &["rev-parse", &format!("refs/remotes/origin/{branch}")],
    ));
    assert_eq!(
        before, after,
        "an invalid prune config must fail before the fetch touches the network"
    );
}

#[test]
fn fetch_prune_accepts_git_numeric_booleans() {
    let fixture = Fixture::new();
    let (_remote, _work, repo, _branch) = fetch_prune_local(&fixture, "prune-numeric");

    // Git treats any non-zero integer (with optional k/m/g suffix) as true …
    fixture.success(&repo, &["config", "fetch.prune", "2"]);
    // … and the remote-scoped key still wins, here as a zero-with-suffix false.
    fixture.success(&repo, &["config", "remote.origin.prune", "0k"]);
    fixture.success(&repo, &["fetch", "origin"]);
    assert!(
        has_tracking_ref(&fixture, &repo, "side"),
        "remote.origin.prune=0k (Git false) must override fetch.prune=2"
    );

    fixture.success(&repo, &["config", "--unset", "remote.origin.prune"]);
    fixture.success(&repo, &["fetch", "origin"]);
    assert!(
        !has_tracking_ref(&fixture, &repo, "side"),
        "fetch.prune=2 must count as a Git-boolean true"
    );
}

#[test]
fn remote_scoped_prune_wins_across_scopes() {
    let fixture = Fixture::new();

    // A GLOBAL remote-scoped key beats a LOCAL fetch.prune: precedence is
    // per key first (remote.<name>.prune > fetch.prune), scope second.
    let (_remote, _work, repo, _branch) = fetch_prune_local(&fixture, "prune-xscope-keep");
    fixture.success(&repo, &["config", "fetch.prune", "true"]);
    fixture.success(
        &repo,
        &["config", "--global", "remote.origin.prune", "false"],
    );
    fixture.success(&repo, &["fetch", "origin"]);
    assert!(
        has_tracking_ref(&fixture, &repo, "side"),
        "global remote.origin.prune=false must override local fetch.prune=true"
    );
    fixture.success(
        &repo,
        &["config", "--global", "--unset", "remote.origin.prune"],
    );

    let (_remote, _work, repo, _branch) = fetch_prune_local(&fixture, "prune-xscope-drop");
    fixture.success(&repo, &["config", "fetch.prune", "false"]);
    fixture.success(
        &repo,
        &["config", "--global", "remote.origin.prune", "true"],
    );
    fixture.success(&repo, &["fetch", "origin"]);
    assert!(
        !has_tracking_ref(&fixture, &repo, "side"),
        "global remote.origin.prune=true must override local fetch.prune=false"
    );
    fixture.success(
        &repo,
        &["config", "--global", "--unset", "remote.origin.prune"],
    );
}

#[test]
fn fetch_all_invalid_prune_config_fails_before_any_fetch() {
    let fixture = Fixture::new();
    let (remote_a, work_a, branch_a) = fixture.remote_fixture("prune-all-a");
    let (remote_b, _work_b, _branch_b) = fixture.remote_fixture("prune-all-b");
    let repo = fixture.path("prune-all-local");
    fixture.init_repo(&repo);
    fixture.success(&repo, &["remote", "add", "origin", path_str(&remote_a)]);
    fixture.success(&repo, &["remote", "add", "second", path_str(&remote_b)]);
    fixture.success(&repo, &["fetch", "--all"]);
    let before = stdout_trim(&fixture.success(
        &repo,
        &["rev-parse", &format!("refs/remotes/origin/{branch_a}")],
    ));

    // Advance the FIRST remote, break the SECOND remote's prune config: the
    // pre-validation pass must reject the run before any remote is fetched.
    fixture.push_remote_commit(&work_a, &branch_a, "advance-a.txt", "remote a advance");
    fixture.success(&repo, &["config", "remote.second.prune", "sometimes"]);

    let rejected = fixture.run(&repo, &["fetch", "--all"]);
    assert_eq!(rejected.status.code(), Some(129));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("remote.second.prune"));

    let after = stdout_trim(&fixture.success(
        &repo,
        &["rev-parse", &format!("refs/remotes/origin/{branch_a}")],
    ));
    assert_eq!(
        before, after,
        "with --all, an invalid prune config on ANY remote must fail before the FIRST fetch"
    );
}

#[test]
fn pull_rebase_accepts_git_numeric_boolean() {
    let fixture = Fixture::new();
    let (remote_dir, work_dir, branch) = fixture.remote_fixture("rebase-numeric");
    let repo = fixture.path("rebase-numeric-local");
    fixture.init_repo(&repo);
    fixture.configure_tracking(&repo, &remote_dir, &branch);
    fixture.success(&repo, &["pull"]);

    fixture.push_remote_commit(&work_dir, &branch, "remote.txt", "remote update");
    fixture.commit_file(&repo, "local.txt", "local change\n", "local update");
    fixture.success(&repo, &["config", "pull.rebase", "2"]);
    fixture.success(&repo, &["pull"]);

    let parents = stdout_trim(&fixture.success(&repo, &["log", "-1", "--format=%P"]));
    assert_eq!(
        parents.split_whitespace().count(),
        1,
        "pull.rebase=2 must count as a Git-boolean true and rebase"
    );
}

// ── P1-05d: status.* display defaults ────────────────────────────────────────

#[test]
fn status_show_untracked_files_config_controls_all_formats_and_cli_wins() {
    let fixture = Fixture::new();
    let repo = fixture.path("status-untracked");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "base.txt", "base\n", "base");
    fs::create_dir_all(repo.join("dir")).expect("mkdir");
    fs::write(repo.join("dir/nested.txt"), "n\n").expect("write untracked");

    // Default (unset): normal mode collapses the directory.
    let normal = fixture.success(&repo, &["status", "--porcelain"]);
    assert!(stdout_trim(&normal).contains("?? dir/"));
    assert!(!stdout_trim(&normal).contains("dir/nested.txt"));

    // no: untracked entries disappear (porcelain honors the config, like Git).
    fixture.success(&repo, &["config", "status.showUntrackedFiles", "no"]);
    let none = fixture.success(&repo, &["status", "--porcelain"]);
    assert!(!stdout_trim(&none).contains("??"));

    // all: nested paths are listed individually.
    fixture.success(&repo, &["config", "status.showUntrackedFiles", "all"]);
    let all = fixture.success(&repo, &["status", "--porcelain"]);
    assert!(stdout_trim(&all).contains("?? dir/nested.txt"));

    // The CLI flag overrides the configured mode.
    let cli = fixture.success(&repo, &["status", "--porcelain", "-unormal"]);
    assert!(stdout_trim(&cli).contains("?? dir/"));
    assert!(!stdout_trim(&cli).contains("dir/nested.txt"));
}

#[test]
fn status_short_and_branch_configs_shape_only_the_human_short_format() {
    let fixture = Fixture::new();
    let repo = fixture.path("status-short-branch");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "base.txt", "base\n", "base");
    fs::write(repo.join("base.txt"), "changed\n").expect("modify");

    // status.short=true selects the short format without any flag …
    fixture.success(&repo, &["config", "status.short", "true"]);
    let short = fixture.success(&repo, &["status"]);
    let short_out = stdout_trim(&short);
    assert!(short_out.contains(" M base.txt"), "short: {short_out}");
    assert!(!short_out.contains("On branch"));

    // … an explicit --long still wins over the config …
    let long = fixture.success(&repo, &["status", "--long"]);
    assert!(stdout_trim(&long).contains("On branch"));

    // … and status.branch=true adds the ## header to the short format only.
    fixture.success(&repo, &["config", "status.branch", "true"]);
    let with_branch = fixture.success(&repo, &["status"]);
    assert!(stdout_trim(&with_branch).starts_with("## "));
    let no_branch = fixture.success(&repo, &["status", "--no-branch"]);
    assert!(!stdout_trim(&no_branch).starts_with("## "));

    // Porcelain stays config-immune: no branch header without an explicit -b.
    let porcelain = fixture.success(&repo, &["status", "--porcelain"]);
    assert!(!stdout_trim(&porcelain).contains("## "));
    assert!(stdout_trim(&porcelain).contains(" M base.txt"));
}

#[test]
fn status_show_stash_config_adds_hint_and_cli_negation_wins() {
    let fixture = Fixture::new();
    let repo = fixture.path("status-stash");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "base.txt", "base\n", "base");
    fs::write(repo.join("base.txt"), "stash me\n").expect("modify");
    fixture.success(&repo, &["stash", "push"]);
    fs::write(repo.join("base.txt"), "changed again\n").expect("modify");

    let plain = fixture.success(&repo, &["status"]);
    assert!(!stdout_trim(&plain).contains("stash"));

    fixture.success(&repo, &["config", "status.showStash", "true"]);
    let hinted = fixture.success(&repo, &["status"]);
    assert!(
        stdout_trim(&hinted).contains("stash currently has 1"),
        "config must enable the stash hint: {}",
        stdout_trim(&hinted)
    );

    let suppressed = fixture.success(&repo, &["status", "--no-show-stash"]);
    assert!(!stdout_trim(&suppressed).contains("stash currently has"));
}

#[test]
fn status_relative_paths_false_keeps_repo_root_paths() {
    let fixture = Fixture::new();
    let repo = fixture.path("status-relpaths");
    fixture.init_repo(&repo);
    fs::create_dir_all(repo.join("sub")).expect("mkdir");
    fixture.commit_file(&repo, "sub/inner.txt", "base\n", "base");
    fs::write(repo.join("sub/inner.txt"), "changed\n").expect("modify");
    let subdir = repo.join("sub");

    // Default (true): paths render relative to the current directory.
    let relative = fixture.success(&subdir, &["status", "--short"]);
    let relative_out = stdout_trim(&relative);
    assert!(
        relative_out.contains(" M inner.txt"),
        "cwd-relative: {relative_out}"
    );

    // false: repository-root-relative paths, even from a subdirectory.
    fixture.success(&repo, &["config", "status.relativePaths", "false"]);
    let rooted = fixture.success(&subdir, &["status", "--short"]);
    let rooted_out = stdout_trim(&rooted);
    assert!(
        rooted_out.contains(" M sub/inner.txt"),
        "repo-root: {rooted_out}"
    );

    // Filtering and metadata pipelines must keep working: a cwd-relative
    // pathspec still matches …
    let by_pathspec = fixture.success(&subdir, &["status", "--short", "inner.txt"]);
    assert!(
        stdout_trim(&by_pathspec).contains("M sub/inner.txt"),
        "pathspec filtering must survive relativePaths=false: {}",
        stdout_trim(&by_pathspec)
    );
    let by_top = fixture.success(&subdir, &["status", "--short", ":(top)sub/inner.txt"]);
    assert!(
        stdout_trim(&by_top).contains("M sub/inner.txt"),
        ":(top) pathspec must survive relativePaths=false: {}",
        stdout_trim(&by_top)
    );
    // … and porcelain v2 keeps real modes/object ids (no zeroed metadata).
    let v2 = fixture.success(&subdir, &["status", "--porcelain=v2"]);
    let v2_out = stdout_trim(&v2);
    assert!(
        v2_out.contains("100644") && !v2_out.contains("000000 000000 000000"),
        "porcelain v2 metadata must survive relativePaths=false: {v2_out}"
    );
    // `--exit-code` still sees the dirty file.
    let dirty = fixture.run(&subdir, &["status", "--exit-code", "--quiet"]);
    assert_eq!(dirty.status.code(), Some(1), "--exit-code must stay dirty");

    // Collapsed untracked directories keep their trailing `/` marker after
    // the repo-root conversion.
    fs::create_dir_all(repo.join("newdir")).expect("mkdir");
    fs::write(
        repo.join("newdir/inside.txt"),
        "n
",
    )
    .expect("write untracked");
    let with_dir = fixture.success(&subdir, &["status", "--short"]);
    assert!(
        stdout_trim(&with_dir).contains("?? newdir/"),
        "collapsed directory must keep its trailing slash: {}",
        stdout_trim(&with_dir)
    );
}

#[test]
fn status_config_defaults_apply_to_fresh_dirty_cache() {
    let fixture = Fixture::new();
    let repo = fixture.path("status-cache-config");
    fixture.init_repo(&repo);
    fs::create_dir_all(repo.join("sub")).expect("mkdir");
    fixture.commit_file(
        &repo,
        "sub/inner.txt",
        "base
",
        "base",
    );
    fs::write(
        repo.join("sub/inner.txt"),
        "changed
",
    )
    .expect("modify");
    fs::write(
        repo.join("stray.txt"),
        "untracked
",
    )
    .expect("untracked");

    // Build a fresh dirty-set cache, then flip the display configs: the
    // fresh `--cached` view must honor them exactly like the full status.
    fixture.success(&repo, &["status", "--scan"]);

    fixture.success(&repo, &["config", "status.showUntrackedFiles", "no"]);
    let cached = fixture.success(&repo, &["status", "--cached"]);
    assert!(
        !stdout_trim(&cached).contains("stray.txt"),
        "fresh --cached must honor status.showUntrackedFiles=no: {}",
        stdout_trim(&cached)
    );

    fixture.success(&repo, &["config", "status.relativePaths", "false"]);
    let rooted = fixture.success(&repo.join("sub"), &["status", "--cached"]);
    let rooted_out = stdout_trim(&rooted);
    assert!(
        rooted_out.contains("modified: sub/inner.txt"),
        "fresh --cached must honor relativePaths=false: {rooted_out}"
    );

    fs::write(
        repo.join("sub/inner.txt"),
        "stash me
",
    )
    .expect("modify");
    fixture.success(&repo, &["stash", "push"]);
    fs::write(
        repo.join("sub/inner.txt"),
        "changed
",
    )
    .expect("modify");
    fixture.success(&repo, &["status", "--scan"]);
    fixture.success(&repo, &["config", "status.showStash", "true"]);
    let hinted = fixture.success(&repo, &["status", "--cached"]);
    assert!(
        stdout_trim(&hinted).contains("stash currently has 1"),
        "fresh --cached must honor status.showStash=true: {}",
        stdout_trim(&hinted)
    );
}

#[test]
fn invalid_status_config_fails_before_output() {
    let fixture = Fixture::new();
    let repo = fixture.path("status-invalid");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "base.txt", "base\n", "base");

    for key in [
        "status.showUntrackedFiles",
        "status.short",
        "status.branch",
        "status.showStash",
        "status.relativePaths",
    ] {
        fixture.success(&repo, &["config", key, "sometimes"]);
        let rejected = fixture.run(&repo, &["status"]);
        assert_eq!(rejected.status.code(), Some(129), "key: {key}");
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains(key),
            "error must name {key}"
        );
        assert!(
            rejected.stdout.is_empty(),
            "no status output may precede the failure for {key}"
        );
        fixture.success(&repo, &["config", "--unset", key]);
    }
}

// ── P1-05d: branch.sort / tag.sort defaults ──────────────────────────────────

#[test]
fn branch_sort_config_orders_list_and_cli_wins() {
    let fixture = Fixture::new();
    let repo = fixture.path("branch-sort");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "base.txt", "base\n", "base");
    fixture.success(&repo, &["branch", "v1.10"]);
    fixture.success(&repo, &["branch", "v1.9"]);

    // version:refname from config orders v1.9 before v1.10.
    fixture.success(&repo, &["config", "branch.sort", "version:refname"]);
    let listed = fixture.success(&repo, &["branch"]);
    let out = stdout_trim(&listed);
    let pos = |needle: &str| out.find(needle).unwrap_or(usize::MAX);
    assert!(
        pos("v1.9") < pos("v1.10"),
        "branch.sort=version:refname must order versions: {out}"
    );

    // The CLI flag overrides the config.
    let cli = fixture.success(&repo, &["branch", "--sort=-refname"]);
    let cli_out = stdout_trim(&cli);
    let cli_pos = |needle: &str| cli_out.find(needle).unwrap_or(usize::MAX);
    assert!(
        cli_pos("v1.9") < cli_pos("v1.10"),
        "--sort=-refname must override the config (v1.9 > v1.10 lexically): {cli_out}"
    );

    // Branch creation still works with a configured sort.
    fixture.success(&repo, &["branch", "created-with-config"]);
    assert!(stdout_trim(&fixture.success(&repo, &["branch"])).contains("created-with-config"));

    // Invalid config fails closed before any listing output.
    fixture.success(&repo, &["config", "branch.sort", "sideways"]);
    let rejected = fixture.run(&repo, &["branch"]);
    assert_eq!(rejected.status.code(), Some(129));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("branch.sort"));
    assert!(rejected.stdout.is_empty());

    // Repeated config values collapse to the LAST one (documented narrowing:
    // Git stacks repeated sort keys into a multi-key sort; Libra's cascade is
    // single-value).
    fixture.success(&repo, &["config", "--", "branch.sort", "-refname"]);
    fixture.success(&repo, &["config", "--add", "branch.sort", "refname"]);
    let stacked = stdout_trim(&fixture.success(&repo, &["branch"]));
    let stacked_pos = |needle: &str| stacked.find(needle).unwrap_or(usize::MAX);
    assert!(
        stacked_pos("v1.10") < stacked_pos("v1.9"),
        "the last repeated branch.sort value (refname asc) must win: {stacked}"
    );
}

#[test]
fn branch_sort_config_keeps_unborn_head_line() {
    let fixture = Fixture::new();
    let repo = fixture.path("branch-sort-unborn");
    fixture.init_repo(&repo);
    fixture.success(&repo, &["config", "branch.sort", "refname"]);

    // An unborn repository still shows the current-branch line: the config
    // default, unlike the --sort flag, must not suppress it.
    let listed = fixture.success(&repo, &["branch"]);
    assert!(
        stdout_trim(&listed).starts_with("* "),
        "unborn HEAD line expected with branch.sort set: {}",
        stdout_trim(&listed)
    );
}

#[test]
fn sort_config_read_failure_is_io_error_before_listing() {
    let fixture = Fixture::new();
    let repo = fixture.path("sort-config-read-failure");
    fixture.init_repo(&repo);
    fixture.commit_file(
        &repo, "base.txt", "base
", "base",
    );
    fixture.success(&repo, &["tag", "v1"]);
    // An unreadable global config DB (a directory in its place) must fail
    // both listings closed with LBR-IO-001 naming the offending key.
    fs::create_dir_all(fixture.home.join(".libra").join("config.db"))
        .expect("create unreadable config-db directory");

    let branch = fixture.run(&repo, &["branch"]);
    assert_eq!(branch.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&branch.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("branch.sort"), "{stderr}");
    assert!(branch.stdout.is_empty());

    let tag = fixture.run(&repo, &["tag"]);
    assert_eq!(tag.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&tag.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("tag.sort"), "{stderr}");
    assert!(tag.stdout.is_empty());
}

#[test]
fn tag_sort_config_orders_list_without_forcing_list_mode() {
    let fixture = Fixture::new();
    let repo = fixture.path("tag-sort");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "base.txt", "base\n", "base");
    fixture.success(&repo, &["tag", "zulu"]);
    fixture.success(&repo, &["tag", "alpha"]);

    // Unset: Git's default is refname-ascending (not insertion order).
    let default_order = stdout_trim(&fixture.success(&repo, &["tag"]));
    assert_eq!(
        default_order.lines().collect::<Vec<_>>(),
        vec!["alpha", "zulu"],
        "default tag order must be refname-ascending"
    );

    // Config reverses it; the CLI flag wins over the config.
    fixture.success(&repo, &["config", "--", "tag.sort", "-refname"]);
    let reversed = stdout_trim(&fixture.success(&repo, &["tag"]));
    assert_eq!(reversed.lines().collect::<Vec<_>>(), vec!["zulu", "alpha"]);
    let cli = stdout_trim(&fixture.success(&repo, &["tag", "--sort=refname", "-l"]));
    assert_eq!(cli.lines().collect::<Vec<_>>(), vec!["alpha", "zulu"]);

    // A configured sort must NOT flip tag creation into list mode.
    fixture.success(&repo, &["tag", "mid"]);
    let created = stdout_trim(&fixture.success(&repo, &["tag"]));
    assert!(
        created.lines().any(|l| l == "mid"),
        "tag creation must still work with tag.sort set: {created}"
    );

    // Invalid config fails closed before any listing output.
    fixture.success(&repo, &["config", "tag.sort", "sideways"]);
    let rejected = fixture.run(&repo, &["tag"]);
    assert_eq!(rejected.status.code(), Some(129));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("tag.sort"));
    assert!(rejected.stdout.is_empty());

    // Repeated config values collapse to the LAST one (documented narrowing).
    fixture.success(&repo, &["config", "--", "tag.sort", "-refname"]);
    fixture.success(&repo, &["config", "--add", "tag.sort", "refname"]);
    let stacked = stdout_trim(&fixture.success(&repo, &["tag", "-l"]));
    assert_eq!(
        stacked.lines().next(),
        Some("alpha"),
        "the last repeated tag.sort value must win: {stacked}"
    );
}
