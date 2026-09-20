//! Shared test utilities and re-exports for the command integration test suite.

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    io::{self, Write},
    ops::{Deref, DerefMut},
    path::Path,
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{Condvar, LazyLock, Mutex},
};

use git_internal::{
    hash::{HashKind, ObjectHash, set_hash_kind_for_test},
    internal::object::{
        commit::Commit,
        signature::{Signature, SignatureType},
        tag::Tag as GitTag,
        tree::Tree,
        types::ObjectType,
    },
};
use libra::{
    command::{
        add::{self, AddArgs},
        branch::{BranchArgs, execute, filter_branches},
        calc_file_blob_hash,
        clean::{self, CleanArgs},
        commit::{self, CommitArgs, execute_safe},
        get_target_commit,
        init::{InitArgs, init},
        load_object,
        log::{LogArgs, get_reachable_commits},
        mv::{self, MvArgs},
        remove::{self, RemoveArgs},
        save_object,
        shortlog::{self, ShortlogArgs},
        status::{changes_to_be_committed, changes_to_be_staged},
        switch::{self, SwitchArgs},
    },
    common_utils::format_commit_msg,
    internal::{branch::Branch, head::Head},
    utils::{
        pager::LIBRA_TEST_ENV,
        test::{self, ChangeDirGuard, ConfigDbFixture},
    },
};
use serde::Deserialize;
use serde_json::Value;
use serial_test::serial;
use tempfile::tempdir;

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct CliErrorReport {
    pub(crate) error_code: String,
    pub(crate) category: String,
    pub(crate) exit_code: i32,
    pub(crate) severity: String,
    pub(crate) message: String,
    pub(crate) usage: Option<String>,
    #[serde(default)]
    pub(crate) hints: Vec<String>,
    #[serde(default)]
    pub(crate) details: BTreeMap<String, Value>,
}

/// Default process-local cap on live CLI children (plan-20260917 SP-00/SP-01).
/// Must stay ≥ 3 so `registry_mutators_serialize_on_worktrees_lock` cannot
/// deadlock under nextest (one process, three concurrent `worktree add`s).
/// 8 still SIGKILL'd `create_committed_repo_via_cli` once in two
/// `--test-threads=32` runs (plan-20260917 SP-01); 4 stays under the
/// SP-00 crush band while leaving the three-add barrier runnable.
const DEFAULT_CLI_SPAWN_LIMIT: usize = 4;

fn parse_cli_spawn_limit(raw: Option<&str>) -> usize {
    raw.and_then(|value| value.parse::<usize>().ok())
        .filter(|&limit| limit >= 1)
        .unwrap_or(DEFAULT_CLI_SPAWN_LIMIT)
}

struct CliSpawnLimiter {
    max: usize,
    live: Mutex<usize>,
    cv: Condvar,
}

impl CliSpawnLimiter {
    fn new(max: usize) -> Self {
        Self {
            max,
            live: Mutex::new(0),
            cv: Condvar::new(),
        }
    }

    fn acquire(&self) -> CliSpawnPermit<'_> {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *live >= self.max {
            live = self
                .cv
                .wait(live)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *live += 1;
        CliSpawnPermit { limiter: self }
    }

    fn release(&self) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *live = live.saturating_sub(1);
        self.cv.notify_one();
    }
}

struct CliSpawnPermit<'a> {
    limiter: &'a CliSpawnLimiter,
}

impl Drop for CliSpawnPermit<'_> {
    fn drop(&mut self) {
        self.limiter.release();
    }
}

static CLI_SPAWN_LIMITER: LazyLock<CliSpawnLimiter> = LazyLock::new(|| {
    CliSpawnLimiter::new(parse_cli_spawn_limit(
        std::env::var("LIBRA_TEST_CLI_SPAWN_LIMIT").ok().as_deref(),
    ))
});

/// `Command` wrapper that holds a limiter permit for the life of the child.
pub(crate) struct LimitedCommand {
    inner: Command,
}

impl LimitedCommand {
    fn env<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.inner.env(key, value);
        self
    }

    fn env_remove<K: AsRef<OsStr>>(&mut self, key: K) -> &mut Self {
        self.inner.env_remove(key);
        self
    }

    fn stdin<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stdin(cfg);
        self
    }

    fn stdout<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stdout(cfg);
        self
    }

    fn stderr<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stderr(cfg);
        self
    }

    fn output(&mut self) -> io::Result<Output> {
        // One SIGKILL retry: even at DEFAULT_CLI_SPAWN_LIMIT, cargo-test
        // --test-threads=32 still occasionally reaps a debug `libra` child
        // with signal 9 (SP-01 cap 8 and cap 4 each lost one fixture add/commit).
        {
            let _permit = CLI_SPAWN_LIMITER.acquire();
            let output = self.inner.output()?;
            if unix_exit_signal(output.status) != Some(9) {
                return Ok(output);
            }
        }
        let _permit = CLI_SPAWN_LIMITER.acquire();
        self.inner.output()
    }

    fn spawn(&mut self) -> io::Result<LimitedChild> {
        let permit = CLI_SPAWN_LIMITER.acquire();
        Ok(LimitedChild {
            child: self.inner.spawn()?,
            permit: Some(permit),
        })
    }
}

pub(crate) struct LimitedChild {
    child: Child,
    permit: Option<CliSpawnPermit<'static>>,
}

impl LimitedChild {
    fn wait_with_output(mut self) -> io::Result<Output> {
        let permit = self.permit.take();
        let output = self.child.wait_with_output();
        drop(permit);
        output
    }
}

impl Deref for LimitedChild {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.child
    }
}

impl DerefMut for LimitedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

/// Run the Libra binary with an isolated HOME so host config never leaks into tests.
fn base_libra_command(args: &[&str], cwd: &Path) -> LimitedCommand {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    let global_db = home.join(".libra").join("config.db");
    let system_db = home.join(".libra").join("system-config.db");
    let llvm_profile_file = std::env::var_os("LLVM_PROFILE_FILE");
    fs::create_dir_all(&config_home).expect("failed to create isolated config directory");

    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("LIBRA_CONFIG_GLOBAL_DB", &global_db)
        .env("LIBRA_CONFIG_SYSTEM_DB", &system_db)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env(LIBRA_TEST_ENV, "1");
    if let Some(llvm_profile_file) = llvm_profile_file {
        // Preserve the llvm-cov profile target for child CLI processes so they
        // do not fall back to writing `default.profraw` inside the temp repo.
        command.env("LLVM_PROFILE_FILE", llvm_profile_file);
    }
    LimitedCommand { inner: command }
}

/// Run the Libra binary with an isolated HOME so host config never leaks into tests.
fn run_libra_command(args: &[&str], cwd: &Path) -> Output {
    base_libra_command(args, cwd)
        .output()
        .expect("failed to execute libra binary")
}

#[allow(dead_code)]
fn run_libra_command_with_env(args: &[&str], cwd: &Path, extra_env: &[(&str, &str)]) -> Output {
    spawn_libra_command_with_env(args, cwd, extra_env)
        .wait_with_output()
        .expect("failed to execute libra binary")
}

#[allow(dead_code)]
fn spawn_libra_command_with_env(
    args: &[&str],
    cwd: &Path,
    extra_env: &[(&str, &str)],
) -> LimitedChild {
    let mut command = base_libra_command(args, cwd);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn libra binary")
}

#[allow(dead_code)]
fn run_libra_command_with_stdin(args: &[&str], cwd: &Path, stdin_body: &str) -> Output {
    let mut child = base_libra_command(args, cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to execute libra binary");

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_body.as_bytes())
            .expect("failed to write stdin to libra process");
        // Explicit close so NDJSON clients see EOF and exit the read loop.
        drop(stdin);
    }

    child
        .wait_with_output()
        .expect("failed to collect libra command output")
}

#[allow(dead_code)]
fn run_libra_command_with_stdin_and_env(
    args: &[&str],
    cwd: &Path,
    stdin_body: &str,
    extra_env: &[(&str, &str)],
) -> Output {
    let mut command = base_libra_command(args, cwd);
    for (key, value) in extra_env {
        command.env(key, value);
    }

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to execute libra binary");

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_body.as_bytes())
            .expect("failed to write stdin to libra process");
    }

    child
        .wait_with_output()
        .expect("failed to collect libra command output")
}

fn cli_output_prefix(bytes: &[u8]) -> String {
    const PREFIX: usize = 200;
    String::from_utf8_lossy(&bytes[..bytes.len().min(PREFIX)]).into_owned()
}

fn unix_exit_signal(status: ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

fn format_cli_failure(output: &Output, context: &str) -> String {
    format!(
        "{context}: success={} code={:?} signal={:?} stderr={:?} stdout={:?}",
        output.status.success(),
        output.status.code(),
        unix_exit_signal(output.status),
        cli_output_prefix(&output.stderr),
        cli_output_prefix(&output.stdout)
    )
}

/// Assert that a CLI command succeeded and include status / stdout / stderr.
fn assert_cli_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{}",
        format_cli_failure(output, context)
    );
}

/// Split a structured CLI error into the human-readable block and the JSON report.
fn parse_cli_error_stderr(stderr: &[u8]) -> (String, CliErrorReport) {
    let stderr = String::from_utf8_lossy(stderr).to_string();
    let trimmed = stderr.trim_end();
    if let Ok(report) = serde_json::from_str::<CliErrorReport>(trimmed) {
        return (String::new(), report);
    }

    let json_start = trimmed
        .rfind("\n{")
        .map(|index| index + 1)
        .or_else(|| trimmed.find('{'))
        .expect("expected structured CLI stderr to contain a JSON report");
    let (human, json) = trimmed.split_at(json_start);
    let report: CliErrorReport =
        serde_json::from_str(json.trim()).expect("expected stderr JSON report to be valid JSON");
    (human.trim_end().to_string(), report)
}

fn parse_json_stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("expected stdout to be valid JSON")
}

fn create_non_commit_tag_object(repo: &Path) -> String {
    let _hash_guard = set_hash_kind_for_test(HashKind::Sha1);
    let _guard = ChangeDirGuard::new(repo);
    let runtime = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    let head = runtime
        .block_on(Head::current_commit())
        .expect("expected HEAD commit");
    let commit: Commit = load_object(&head).expect("failed to load HEAD commit");
    let tag = GitTag::new(
        commit.tree_id,
        ObjectType::Tree,
        "tree-tag".to_string(),
        Signature {
            signature_type: SignatureType::Tagger,
            name: "tester".to_string(),
            email: "tester@example.com".to_string(),
            timestamp: 1,
            timezone: "+0000".to_string(),
        },
        "tag points to a tree".to_string(),
    );
    save_object(&tag, &tag.id).expect("failed to save tree tag object");
    tag.id.to_string()
}

/// Build the on-disk path to a loose object given the repository root and full
/// hex hash. Used by tests that need to corrupt or delete individual objects.
fn loose_object_path(repo: &Path, hash: &str) -> std::path::PathBuf {
    repo.join(libra::utils::util::ROOT_DIR)
        .join("objects")
        .join(&hash[..2])
        .join(&hash[2..])
}

/// Initialize a repository through the CLI to exercise the real process entrypoint.
/// Set `skip_worktree` on a tracked path through the git-internal index API
/// (the CLI entry point arrives with plan issues/490 SW-07).
#[allow(dead_code)]
pub(crate) fn mark_skip_worktree(repo: &Path, path: &str) {
    use git_internal::{
        hash::HashKind,
        internal::index::{Index, IndexEntry},
    };
    let index_path = repo.join(".libra/index");
    let mut index =
        Index::load_with_hash_kind(HashKind::Sha1, &index_path).expect("load index for marking");
    let (hash, mode, size) = {
        let entry = index.get(path, 0).expect("tracked path");
        (entry.hash, entry.mode, entry.size)
    };
    let mut entry = IndexEntry::new_from_blob(path.to_string(), hash, size);
    entry.mode = mode;
    entry.flags.skip_worktree = true;
    index.update(entry);
    index
        .save_with_hash_kind(HashKind::Sha1, &index_path)
        .expect("save index");
}

/// Whether a tracked path currently carries the `skip_worktree` bit.
#[allow(dead_code)]
pub(crate) fn skip_worktree_set(repo: &Path, path: &str) -> bool {
    use git_internal::{hash::HashKind, internal::index::Index};
    Index::load_with_hash_kind(HashKind::Sha1, repo.join(".libra/index"))
        .expect("load index")
        .get(path, 0)
        .is_some_and(|entry| entry.flags.skip_worktree)
}

/// The stage-0 index entry for `path` as `(mode, hash hex, size,
/// intent_to_add, skip_worktree)`, or `None` when the path has no stage 0.
#[allow(dead_code)]
pub(crate) fn index_entry_snapshot(
    repo: &Path,
    path: &str,
) -> Option<(u32, String, u32, bool, bool)> {
    use git_internal::{hash::HashKind, internal::index::Index};
    Index::load_with_hash_kind(HashKind::Sha1, repo.join(".libra/index"))
        .ok()?
        .get(path, 0)
        .map(|entry| {
            (
                entry.mode,
                entry.hash.to_string(),
                entry.size,
                entry.flags.intent_to_add,
                entry.flags.skip_worktree,
            )
        })
}

/// The on-disk index header version (2 or 3).
#[allow(dead_code)]
pub(crate) fn index_version(repo: &Path) -> u32 {
    let bytes = std::fs::read(repo.join(".libra/index")).expect("read index");
    u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])
}

fn init_repo_via_cli(repo: &Path) {
    fs::create_dir_all(repo).expect("failed to create repository directory");
    let output = run_libra_command(&["init"], repo);
    assert_cli_success(&output, "failed to initialize repository");
}

/// Configure a stable local identity for commands that require commits.
fn configure_identity_via_cli(repo: &Path) {
    let output = run_libra_command(&["config", "user.name", "Test User"], repo);
    assert_cli_success(&output, "failed to configure user.name");

    let output = run_libra_command(&["config", "user.email", "test@example.com"], repo);
    assert_cli_success(&output, "failed to configure user.email");
}

/// Create a committed repository that is ready for branch, tag, and remote tests.
fn create_committed_repo_via_cli() -> tempfile::TempDir {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("tracked.txt"), "tracked\n").expect("failed to create tracked file");

    let output = run_libra_command(&["add", ".libraignore", "tracked.txt"], repo.path());
    assert_cli_success(&output, "failed to add tracked file");

    let output = run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path());
    assert_cli_success(&output, "failed to create initial commit");

    repo
}

#[cfg(unix)]
fn skip_permission_denied_test_if_root(test_name: &str) -> bool {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }

    // SAFETY: On Unix targets libc exposes `geteuid()` with no arguments and a
    // numeric return type compatible with `u32` on the platforms this suite runs on.
    let is_root = unsafe { geteuid() == 0 };
    if is_root {
        eprintln!(
            "skipping {test_name}: permission-based write failure injection is unreliable as root"
        );
    }

    is_root
}

#[test]
fn assert_cli_success_reports_signal() {
    let repo = tempdir().expect("temp repo");
    let failed = run_libra_command(&["definitely-not-a-libra-command"], repo.path());
    assert!(!failed.status.success(), "garbage argv must fail");
    let message = format_cli_failure(&failed, "probe");
    assert!(
        message.contains("success=false"),
        "status flag missing: {message}"
    );
    assert!(message.contains("code="), "exit code missing: {message}");
    assert!(
        message.contains("signal="),
        "signal field missing: {message}"
    );
    assert!(
        message.contains("stderr="),
        "stderr prefix missing: {message}"
    );
    assert!(
        message.contains("stdout="),
        "stdout prefix missing: {message}"
    );

    #[cfg(unix)]
    {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sleep");
        child.kill().expect("kill sleep");
        let killed = child.wait_with_output().expect("reap sleep");
        let signaled = format_cli_failure(&killed, "killed");
        assert!(
            signaled.contains("signal=Some("),
            "unix signal must be Some: {signaled}"
        );
    }
}

#[test]
fn cli_spawn_limit_rejects_zero_and_garbage() {
    assert_eq!(parse_cli_spawn_limit(None), DEFAULT_CLI_SPAWN_LIMIT);
    assert_eq!(parse_cli_spawn_limit(Some("0")), DEFAULT_CLI_SPAWN_LIMIT);
    assert_eq!(parse_cli_spawn_limit(Some("nope")), DEFAULT_CLI_SPAWN_LIMIT);
    assert_eq!(parse_cli_spawn_limit(Some("8")), 8);
    assert_eq!(parse_cli_spawn_limit(Some("3")), 3);
}

mod add_cli_test;
mod add_json_test;
mod add_patch_test;
mod add_test;
mod agent_bridge_test;
mod agent_checkpoint_export_test;
mod agent_checkpoint_test;
mod agent_clean_test;
mod agent_erasure_test;
mod agent_fix_bridge_test;
mod agent_help_test;
mod agent_push_test;
mod agent_roster_test;
mod agent_rpc_trust_test;
mod agent_run_admission_test;
mod agent_skill_search_test;
mod agent_workspace_test;
mod alternates_test;
mod apply_test;
mod archive_test;
mod auth_test;
mod automation_help_test;
mod bisect_test;
mod blame_test;
mod branch_diff_test;
mod branch_reset_test;
mod branch_test;
mod bundle_test;
mod cache_test;
mod case_handling_test;
mod cat_file_test;
mod change_revision_provenance_test;
mod check_attr_test;
mod check_ignore_test;
mod check_mailmap_test;
mod checkout_test;
mod cherry_pick_test;
mod clean_test;
mod cli_error_test;
mod clone_cli_test;
mod clone_test;
mod cloud_test;
mod commit_autosquash_test;
mod commit_editor_test;
mod commit_error_test;
mod commit_json_test;
mod commit_test;
mod commit_tree_test;
mod completions_test;
mod config_test;
mod credential_test;
mod deps_test;
mod deps_travel_test;
mod describe_long_test;
mod describe_test;
mod diff_plumbing_test;
mod diff_rename_limit_test;
mod diff_test;
mod dirty_test;
mod fast_export_test;
mod fast_import_test;
mod fetch_test;
mod file_obliterate_test;
mod for_each_ref_test;
mod format_patch_test;
mod fsck_test;
mod grep_test;
mod hash_object_test;
#[path = "../helpers/historical_schema.rs"]
mod historical_schema;
mod hooks_help_test;
mod hydrate_test;
mod index_flag_preservation_test;
mod index_format_test;
mod index_pack_keep_test;
mod index_pack_progress_test;
mod index_pack_stdin_test;
mod index_pack_test;
mod info_exclude_scope_test;
mod init_from_git_test;
mod init_json_test;
mod init_separate_libra_dir_test;
mod init_test;
mod layer_test;
mod lfs_test;
mod log_test;
mod logfile_test;
mod ls_files_test;
mod ls_remote_options_test;
mod ls_remote_test;
mod ls_tree_test;
mod maintenance_test;
mod merge_base_test;
mod merge_file_test;
mod merge_test;
mod mergetool_test;
mod metadata_test;
mod mv_test;
mod notes_test;
mod op_test;
mod open_test;
mod output_flags_test;
mod pull_json_test;
mod pull_test;
mod push_error_test;
mod push_json_test;
mod push_test;
mod read_tree_test;
mod rebase_interactive_test;
mod rebase_test;
mod reflog_test;
mod remote_test;
mod remove_test;
mod repack_test;
mod replace_test;
mod rerere_test;
mod reset_patch_test;
mod reset_test;
mod restore_test;
mod rev_list_test;
mod rev_parse_test;
mod revert_test;
mod revision_test;
mod sandbox_status_test;
mod schema_upgrade_test;
mod service_test;
mod shortlog_test;
mod show_ref_abbrev_test;
mod show_ref_alias_test;
mod show_ref_deref_pattern_test;
mod show_ref_exclude_existing_test;
mod show_ref_test;
mod show_test;
mod sparse_view_test;
mod stash_test;
mod status_error_test;
mod status_json_test;
mod status_test;
#[path = "status_wave0_test.rs"]
mod status_wave0;
mod switch_error_test;
mod switch_json_test;
mod switch_test;
mod symbolic_ref_test;
mod t4_port_test;
mod tag_test;
mod update_index_test;
mod update_ref_test;
mod upgrade_cmd_test;
mod verify_pack_stat_test;
mod verify_pack_test;
mod worktree_doctor_test;
#[cfg(all(unix, feature = "worktree-fuse"))]
mod worktree_fuse_test;
mod worktree_isolation_test;
mod worktree_test;
mod write_tree_test;
