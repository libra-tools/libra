//! Shared test utilities and re-exports for the command integration test suite.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
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
        test::{self, ChangeDirGuard},
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

/// Run the Libra binary with an isolated HOME so host config never leaks into tests.
fn base_libra_command(args: &[&str], cwd: &Path) -> Command {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    let global_db = home.join(".libra").join("config.db");
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
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env(LIBRA_TEST_ENV, "1");
    if let Some(llvm_profile_file) = llvm_profile_file {
        // Preserve the llvm-cov profile target for child CLI processes so they
        // do not fall back to writing `default.profraw` inside the temp repo.
        command.env("LLVM_PROFILE_FILE", llvm_profile_file);
    }
    command
}

/// Run the Libra binary with an isolated HOME so host config never leaks into tests.
fn run_libra_command(args: &[&str], cwd: &Path) -> Output {
    base_libra_command(args, cwd)
        .output()
        .expect("failed to execute libra binary")
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

/// Assert that a CLI command succeeded and include stderr in the failure output.
fn assert_cli_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context}: {}",
        String::from_utf8_lossy(&output.stderr)
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

mod add_cli_test;
mod add_json_test;
mod add_test;
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
mod code_control_help_test;
mod code_test;
mod code_thread_id_test;
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
mod graph_test;
mod grep_test;
mod hash_object_test;
mod hooks_help_test;
mod hydrate_test;
mod index_pack_keep_test;
mod index_pack_progress_test;
mod index_pack_stdin_test;
mod index_pack_test;
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
mod metadata_test;
mod mv_test;
mod notes_test;
mod op_test;
mod open_test;
mod output_flags_test;
mod publish_test;
mod pull_json_test;
mod pull_test;
mod push_error_test;
mod push_json_test;
mod push_test;
mod read_tree_test;
mod rebase_test;
mod reflog_test;
mod remote_test;
mod remove_test;
mod repack_test;
mod replace_test;
mod rerere_test;
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
mod switch_error_test;
mod switch_json_test;
mod switch_test;
mod symbolic_ref_test;
mod tag_test;
mod update_index_test;
mod update_ref_test;
mod usage_help_test;
mod verify_pack_stat_test;
mod verify_pack_test;
#[cfg(all(unix, feature = "worktree-fuse"))]
mod worktree_fuse_test;
mod worktree_isolation_test;
mod worktree_test;
mod write_tree_test;
