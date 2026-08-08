//! Implements stash push/pop/show/drop/apply by saving worktree/index states as commits and restoring them on demand.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Component, Path, PathBuf},
    str::FromStr,
};

use git_internal::{
    errors::GitError,
    hash::ObjectHash,
    internal::{
        index::{Index, Time},
        object::{
            ObjectTrait,
            blob::Blob,
            commit::Commit,
            signature::Signature,
            tree::{Tree, TreeItem, TreeItemMode},
            types::ObjectType,
        },
    },
};
use serde::Serialize;

use crate::{
    cli::Stash,
    command::{
        load_object, log,
        merge::{MergeTreeEntry, create_tree_from_items_map},
        reset::{
            rebuild_index_from_tree, remove_empty_directories, reset_index_to_commit,
            restore_working_directory_from_tree,
        },
        status,
    },
    internal::{
        branch::{Branch as InternalBranch, BranchStoreError},
        head::Head,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        object,
        object_ext::TreeExt,
        output::{OutputConfig, emit_json_data},
        path, tree, util,
    },
};

/// GitHub Issues URL surfaced on `StashError::Other` so users can report
/// catch-all bucket failures that map to `InternalInvariant`. Mirrors
/// push.rs / tag.rs's hint pattern per Cross-Cutting G.
const ISSUE_URL: &str = "https://github.com/libra-tools/libra/issues";

// ── Typed error ──────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub(crate) enum StashError {
    #[error("not a libra repository")]
    NotInRepo,

    #[error("you do not have the initial commit yet")]
    NoInitialCommit,

    #[error("no stash found")]
    NoStashFound,

    #[error("'{0}' is not a valid stash reference")]
    InvalidStashRef(String),

    #[error("stash@{{{0}}}: stash does not exist")]
    StashNotExist(usize),

    #[error("merge conflict during stash apply:\n  {0}")]
    MergeConflict(String),

    #[error("a branch named '{0}' already exists")]
    BranchExists(String),

    #[error("failed to query branch '{branch}': {detail}")]
    BranchLookupFailed { branch: String, detail: String },

    #[error("clearing all stash entries requires --force in interactive mode")]
    ClearRequiresForce,

    #[error(
        "the stash stack changed concurrently while this command ran; nothing further was \
         modified — inspect `libra stash list` and re-run"
    )]
    StackChanged,

    #[error(
        "the stash was applied to this worktree, but the stash stack changed concurrently so \
         entry {stash_id} was KEPT (the successful apply is not rolled back) — inspect \
         `libra stash list` and `libra stash drop` it explicitly if desired"
    )]
    StackChangedAfterApply { stash_id: String },

    #[error("cannot lock the stash stack: {0}")]
    StackLock(String),

    #[error("failed to read object: {0}")]
    ReadObject(String),

    #[error("failed to write object: {0}")]
    WriteObject(String),

    #[error("failed to load index: {0}")]
    IndexLoad(String),

    #[error("failed to save index: {0}")]
    IndexSave(String),

    #[error("failed to reset working directory: {0}")]
    ResetFailed(String),

    #[error("pathspec '{0}' did not match any tracked files")]
    PathspecNoMatch(String),

    #[error("'{0}' cannot be combined with a pathspec")]
    PathspecWithOption(String),

    #[error("{0}")]
    Other(String),
}

impl StashError {
    fn stable_code(&self) -> StableErrorCode {
        match self {
            Self::NotInRepo => StableErrorCode::RepoNotFound,
            Self::NoInitialCommit => StableErrorCode::RepoStateInvalid,
            Self::NoStashFound => StableErrorCode::CliInvalidTarget,
            Self::InvalidStashRef(_) => StableErrorCode::CliInvalidArguments,
            Self::StashNotExist(_) => StableErrorCode::CliInvalidTarget,
            Self::MergeConflict(_) => StableErrorCode::ConflictUnresolved,
            Self::BranchExists(_) => StableErrorCode::ConflictOperationBlocked,
            Self::BranchLookupFailed { .. } => StableErrorCode::IoReadFailed,
            Self::ClearRequiresForce => StableErrorCode::CliInvalidArguments,
            Self::ReadObject(_) => StableErrorCode::IoReadFailed,
            Self::WriteObject(_) => StableErrorCode::IoWriteFailed,
            Self::IndexLoad(_) => StableErrorCode::IoReadFailed,
            Self::IndexSave(_) => StableErrorCode::IoWriteFailed,
            Self::ResetFailed(_) => StableErrorCode::IoWriteFailed,
            Self::PathspecNoMatch(_) => StableErrorCode::CliInvalidTarget,
            Self::PathspecWithOption(_) => StableErrorCode::CliInvalidArguments,
            Self::StackChanged | Self::StackChangedAfterApply { .. } => {
                StableErrorCode::ConflictOperationBlocked
            }
            Self::StackLock(_) => StableErrorCode::IoWriteFailed,
            Self::Other(_) => StableErrorCode::InternalInvariant,
        }
    }
}

impl From<StashError> for CliError {
    fn from(error: StashError) -> Self {
        let stable_code = error.stable_code();
        let message = error.to_string();
        match error {
            StashError::NotInRepo => CliError::repo_not_found(),
            StashError::NoInitialCommit => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("create an initial commit first"),
            StashError::NoStashFound => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("use 'libra stash push' to create a stash first"),
            StashError::InvalidStashRef(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("use stash@{N} syntax, e.g. stash@{0}"),
            StashError::StashNotExist(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("use 'libra stash list' to see available stashes"),
            StashError::MergeConflict(_) => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint("resolve conflicts manually, then use 'libra add'"),
            StashError::BranchExists(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("use a different branch name or delete the existing branch first"),
            StashError::BranchLookupFailed { .. } => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("repair branch storage, then retry 'libra stash branch'."),
            StashError::ClearRequiresForce => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("re-run with --force, or use --json / --machine for scripted use"),
            StashError::PathspecNoMatch(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("check the path exists and is tracked, or omit it to stash everything"),
            StashError::PathspecWithOption(_) => CliError::command_usage(message)
                .with_stable_code(stable_code)
                .with_hint("run the option without a pathspec, or the pathspec without the option"),
            StashError::IndexLoad(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("repair or refresh the index, then retry the stash operation."),
            StashError::Other(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint(format!("this is a bug; please report it at {ISSUE_URL}")),
            _ => CliError::fatal(message).with_stable_code(stable_code),
        }
    }
}

// ── Structured output ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "action")]
pub enum StashOutput {
    #[serde(rename = "noop")]
    Noop { message: String },
    #[serde(rename = "push")]
    Push {
        message: String,
        stash_id: String,
        #[serde(default, skip_serializing_if = "is_zero_usize")]
        included_untracked: usize,
        #[serde(default, skip_serializing_if = "is_false")]
        kept_index: bool,
        /// The exact reflog line this push appended — the entry's identity
        /// for a later raw-line CAS delete (autostash). Internal only, never
        /// serialized (the JSON envelope is a stable public surface).
        #[serde(skip)]
        raw_line: String,
    },
    #[serde(rename = "pop")]
    Pop {
        index: usize,
        stash_id: String,
        branch: String,
    },
    #[serde(rename = "apply")]
    Apply {
        index: usize,
        stash_id: String,
        branch: String,
    },
    #[serde(rename = "drop")]
    Drop { index: usize, stash_id: String },
    #[serde(rename = "list")]
    List { entries: Vec<StashListEntry> },
    #[serde(rename = "show")]
    Show {
        stash: String,
        stash_id: String,
        files: Vec<StashFileChange>,
        files_changed: StashFilesChangedStats,
        /// Unified diff of the stashed changes, present only with `-p`/`--patch`
        /// (additive; omitted from JSON otherwise so existing consumers are
        /// unaffected).
        #[serde(skip_serializing_if = "Option::is_none")]
        patch: Option<String>,
        // Human-render hints. Skipped in JSON because the structured output
        // always carries the full file list with status.
        #[serde(skip)]
        name_only: bool,
        #[serde(skip)]
        name_status: bool,
    },
    #[serde(rename = "branch")]
    Branch {
        branch: String,
        stash: String,
        stash_id: String,
        applied: bool,
        dropped: bool,
    },
    #[serde(rename = "clear")]
    Clear { cleared_count: usize },
}

#[derive(Debug, Clone, Serialize)]
pub struct StashListEntry {
    pub index: usize,
    pub message: String,
    pub stash_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StashFileChange {
    pub path: String,
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StashFilesChangedStats {
    pub total: usize,
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// `--help` examples shown in `libra stash --help` output.
pub const STASH_EXAMPLES: &str = "\
EXAMPLES:
    libra stash push -m 'WIP'         Save current changes
    libra stash push -u               Include untracked files
    libra stash push -a               Include untracked and ignored files
    libra stash push --keep-index     Keep staged changes in place
    libra stash list                  Show all stash entries
    libra stash show                  File-level summary of stash@{0}
    libra stash show -p               Show the stashed changes as a unified diff
    libra stash show stash@{1}        Inspect a specific stash entry
    libra stash branch hotfix         Branch off the latest stash and drop it
    libra stash apply                 Re-apply stash@{0} without dropping
    libra stash pop                   Apply stash@{0} and drop it
    libra stash clear --force         Remove every stash entry";

// ── Entry points ─────────────────────────────────────────────────────

pub async fn execute(stash_cmd: Stash) {
    if let Err(e) = execute_safe(stash_cmd, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Dispatches to stash sub-commands (push, pop, list,
/// apply, drop, show, branch, clear).
pub async fn execute_safe(stash_cmd: Stash, output: &OutputConfig) -> CliResult<()> {
    // §C.10: finish any rollback an interrupted `stash branch` recorded.
    recover_stash_branch_journal()
        .await
        .map_err(CliError::from)?;

    // W2 §C.4.3: the stash STACK (`refs/stash` + reflog) stays deliberately
    // repository-shared — a stash pushed in one worktree may be applied in
    // another — while push/apply/pop snapshot and mutate only the CURRENT
    // worktree's index/workdir (both are cwd-scoped since lore 2.1). Every
    // stack mutation serializes on the stack lock, and pop/branch delete
    // their applied entry via the by-id CAS `do_drop`, so linked worktrees
    // run every subcommand (the former W0 guard is lifted).
    let result = run_stash(stash_cmd, output).await.map_err(CliError::from)?;
    render_stash_output(&result, output)
}

// ── Core execution ───────────────────────────────────────────────────

async fn run_stash(stash_cmd: Stash, output: &OutputConfig) -> Result<StashOutput, StashError> {
    util::require_repo().map_err(|_| StashError::NotInRepo)?;

    match stash_cmd {
        Stash::Push {
            message,
            include_untracked,
            no_include_untracked: _,
            all,
            keep_index,
            pathspec,
        } => {
            run_push(StashPushOptions {
                message,
                include_untracked: include_untracked || all,
                include_ignored: all,
                keep_index,
                pathspec,
            })
            .await
        }
        Stash::Pop { stash } => run_pop(stash).await,
        Stash::List => run_list().await,
        Stash::Apply { stash } => run_apply(stash).await,
        Stash::Drop { stash } => run_drop(stash).await,
        Stash::Show {
            stash,
            name_only,
            name_status,
            patch,
        } => run_show(stash, name_only, name_status, patch).await,
        Stash::Branch { branch, stash } => run_branch(branch, stash).await,
        Stash::Clear { force } => run_clear(force, output).await,
    }
}

#[derive(Debug, Default)]
struct StashPushOptions {
    message: Option<String>,
    include_untracked: bool,
    include_ignored: bool,
    keep_index: bool,
    /// When non-empty, stash only the changes to these paths (Git's
    /// `stash push -- <pathspec>...`); the rest of the working tree is left
    /// untouched.
    pathspec: Vec<String>,
}

async fn run_push(options: StashPushOptions) -> Result<StashOutput, StashError> {
    // `stash push -- <pathspec>` stashes only the changes to the named paths and
    // leaves the rest of the working tree intact — a distinct, self-contained
    // flow so the common full-stash path stays unchanged.
    if !options.pathspec.is_empty() {
        return run_push_pathspec(options).await;
    }

    let git_dir = util::request_storage_path();
    let index_path = path::index();
    let index = Index::load(&index_path)
        .map_err(|error| StashError::IndexLoad(format!("{}: {error}", index_path.display())))?;
    let included_untracked_paths = collect_included_untracked_paths(&options)?;

    if !has_changes().await && included_untracked_paths.is_empty() {
        return Ok(StashOutput::Noop {
            message: "No local changes to save".to_string(),
        });
    }

    let head_commit_hash = Head::current_commit()
        .await
        .ok_or(StashError::NoInitialCommit)?;
    let head_commit_hash_str = head_commit_hash.to_string();

    // lore.md 2.4 / §C.11 W1: `stash push` turns the current index into a tree
    // and publishes it through `refs/stash` — reachable history, so the same
    // guard as `commit` and `write-tree`.
    crate::internal::layer::reject_layer_owned_entries(&index, "to stash")
        .await
        .map_err(StashError::WriteObject)?;
    let index_tree =
        tree::create_tree_from_index(&index).map_err(|e| StashError::WriteObject(e.to_string()))?;
    let index_tree_data = index_tree
        .to_data()
        .map_err(|error| StashError::WriteObject(error.to_string()))?;
    let index_tree_hash = object::write_git_object(&git_dir, "tree", &index_tree_data)
        .map_err(|error| StashError::WriteObject(error.to_string()))?;

    let (author, committer) = util::create_signatures().await;
    let (current_branch_name, head_commit_summary) = match Head::current().await {
        Head::Branch(name) => {
            let c: Commit = load_object(&head_commit_hash)
                .map_err(|e| StashError::ReadObject(e.to_string()))?;
            let summary = c.message.lines().next().unwrap_or("").to_string();
            (name, summary)
        }
        Head::Detached(_) => {
            let c: Commit = load_object(&head_commit_hash)
                .map_err(|e| StashError::ReadObject(e.to_string()))?;
            let summary = c.message.lines().next().unwrap_or("").to_string();
            ("(no branch)".to_string(), summary)
        }
    };

    let head_commit_short = head_commit_hash_str
        .get(..7)
        .unwrap_or(head_commit_hash_str.as_str());
    let wip_message = format!(
        "WIP on {}: {} {}",
        current_branch_name, head_commit_short, head_commit_summary
    );
    let final_message = options.message.unwrap_or(wip_message);

    let index_commit = Commit::new(
        author.clone(),
        committer.clone(),
        index_tree_hash,
        vec![head_commit_hash],
        &final_message,
    );
    let data = index_commit
        .to_data()
        .map_err(|e| StashError::WriteObject(e.to_string()))?;
    let index_commit_hash = object::write_git_object(&git_dir, "commit", &data)
        .map_err(|e| StashError::WriteObject(e.to_string()))?;

    let workdir = &util::request_working_dir();
    let worktree_tree =
        create_tree_from_workdir(workdir, &git_dir, &index).map_err(StashError::WriteObject)?;
    let worktree_tree_data = worktree_tree
        .to_data()
        .map_err(|e| StashError::WriteObject(e.to_string()))?;
    let worktree_tree_hash = object::write_git_object(&git_dir, "tree", &worktree_tree_data)
        .map_err(|e| StashError::WriteObject(e.to_string()))?;

    let untracked_parent = if included_untracked_paths.is_empty() {
        None
    } else {
        let short_head = head_commit_hash_str
            .get(..7)
            .unwrap_or(head_commit_hash_str.as_str());
        let untracked_message =
            format!("untracked files on {current_branch_name}: {short_head} {head_commit_summary}");
        Some(create_untracked_parent_commit(
            workdir,
            &git_dir,
            &included_untracked_paths,
            &author,
            &committer,
            &untracked_message,
        )?)
    };

    let mut parents = vec![head_commit_hash, index_commit_hash];
    if let Some(untracked_commit_hash) = untracked_parent {
        parents.push(untracked_commit_hash);
    }

    let stash_commit = Commit::new(
        author,
        committer.clone(),
        worktree_tree_hash,
        parents,
        &final_message,
    );
    let stash_commit_data = stash_commit
        .to_data()
        .map_err(|e| StashError::WriteObject(e.to_string()))?;
    let stash_commit_hash = object::write_git_object(&git_dir, "commit", &stash_commit_data)
        .map_err(|e| StashError::WriteObject(e.to_string()))?;

    let pushed_raw_line =
        update_stash_ref_locked(&git_dir, &stash_commit_hash, &committer, &final_message)?;

    perform_hard_reset(&head_commit_hash)
        .await
        .map_err(StashError::ResetFailed)?;
    if options.keep_index {
        restore_worktree_to_index(&index, &head_commit_hash, workdir, &git_dir)
            .map_err(StashError::ResetFailed)?;
        index
            .save(&index_path)
            .map_err(|e| StashError::IndexSave(e.to_string()))?;
    }
    remove_included_untracked_paths(workdir, &included_untracked_paths)
        .map_err(StashError::ResetFailed)?;

    Ok(StashOutput::Push {
        message: final_message,
        stash_id: stash_commit_hash.to_string(),
        included_untracked: included_untracked_paths.len(),
        kept_index: options.keep_index,
        raw_line: pushed_raw_line,
    })
}

/// Create a HELD stash COMMIT for merge autostash (lore.md §1.8):
/// tracked-only (index + worktree vs HEAD; untracked/ignored stay in place —
/// Git parity), message-tagged, written to the object store but deliberately
/// NOT entered into refs/stash (the MERGE_AUTOSTASH model). This does NOT
/// touch the worktree: the caller must persist a durable reference (the
/// sidecar) FIRST and only then call [`reset_to_head_for_held_stash`] — the
/// reverse order would open a crash window where the changes are gone from
/// the tree and the stash commit is referenced by nothing. Returns `None`
/// when the tree is clean (strict no-op).
pub(crate) async fn create_held_stash_commit(
    message: &str,
) -> Result<Option<ObjectHash>, StashError> {
    let git_dir = util::request_storage_path();
    let index_path = path::index();
    let index = Index::load(&index_path)
        .map_err(|error| StashError::IndexLoad(format!("{}: {error}", index_path.display())))?;

    if !has_changes().await {
        return Ok(None);
    }

    let head_commit_hash = Head::current_commit()
        .await
        .ok_or(StashError::NoInitialCommit)?;
    let index_tree =
        tree::create_tree_from_index(&index).map_err(|e| StashError::WriteObject(e.to_string()))?;
    let index_tree_data = index_tree
        .to_data()
        .map_err(|error| StashError::WriteObject(error.to_string()))?;
    let index_tree_hash = object::write_git_object(&git_dir, "tree", &index_tree_data)
        .map_err(|error| StashError::WriteObject(error.to_string()))?;
    let (author, committer) = util::create_signatures().await;

    let index_commit = Commit::new(
        author.clone(),
        committer.clone(),
        index_tree_hash,
        vec![head_commit_hash],
        message,
    );
    let data = index_commit
        .to_data()
        .map_err(|e| StashError::WriteObject(e.to_string()))?;
    let index_commit_hash = object::write_git_object(&git_dir, "commit", &data)
        .map_err(|e| StashError::WriteObject(e.to_string()))?;

    let workdir = &util::request_working_dir();
    let worktree_tree =
        create_tree_from_workdir(workdir, &git_dir, &index).map_err(StashError::WriteObject)?;
    let worktree_tree_data = worktree_tree
        .to_data()
        .map_err(|e| StashError::WriteObject(e.to_string()))?;
    let worktree_tree_hash = object::write_git_object(&git_dir, "tree", &worktree_tree_data)
        .map_err(|e| StashError::WriteObject(e.to_string()))?;

    let stash_commit = Commit::new(
        author,
        committer,
        worktree_tree_hash,
        vec![head_commit_hash, index_commit_hash],
        message,
    );
    let stash_commit_data = stash_commit
        .to_data()
        .map_err(|e| StashError::WriteObject(e.to_string()))?;
    let stash_commit_hash = object::write_git_object(&git_dir, "commit", &stash_commit_data)
        .map_err(|e| StashError::WriteObject(e.to_string()))?;

    Ok(Some(stash_commit_hash))
}

/// Second half of the held-stash push: hard-reset index + worktree to HEAD.
/// Call ONLY after the held stash commit is durably referenced (sidecar
/// written) — see [`create_held_stash_commit`].
pub(crate) async fn reset_to_head_for_held_stash() -> Result<(), StashError> {
    let head_commit_hash = Head::current_commit()
        .await
        .ok_or(StashError::NoInitialCommit)?;
    perform_hard_reset(&head_commit_hash)
        .await
        .map_err(StashError::ResetFailed)
}

/// Enter an existing stash COMMIT into refs/stash (promote a held autostash
/// into the visible stash list, e.g. after its re-apply conflicted).
pub(crate) async fn store_stash_commit(hash: &ObjectHash, message: &str) -> Result<(), StashError> {
    let git_dir = util::request_storage_path();
    let (_, committer) = util::create_signatures().await;
    update_stash_ref_locked(&git_dir, hash, &committer, message).map(|_| ())
}

/// Map an index entry's raw mode to the tree-item mode used for stash trees.
fn index_mode_to_tree_mode(mode: u32) -> TreeItemMode {
    match mode & 0o170000 {
        0o120000 => TreeItemMode::Link,
        0o160000 => TreeItemMode::Commit,
        _ if mode & 0o111 != 0 => TreeItemMode::BlobExecutable,
        _ => TreeItemMode::Blob,
    }
}

/// The Unix permission bits a restored worktree file should carry for a tree mode.
#[cfg(unix)]
fn tree_mode_to_unix_perm(mode: TreeItemMode) -> u32 {
    match mode {
        TreeItemMode::BlobExecutable => 0o755,
        _ => 0o644,
    }
}

/// Resolve user pathspecs to the set of candidate paths they select. A pathspec
/// matches a path when they are equal or the path lies under the pathspec
/// directory (`<spec>/...`); separators are normalised to `/`. An empty/`.`
/// pathspec (the repository root) matches every candidate. Returns a sorted,
/// de-duplicated list for deterministic processing.
fn paths_matching_pathspec(pathspec: &[String], candidates: &HashSet<String>) -> Vec<String> {
    let norm = |s: &str| {
        s.trim_start_matches("./")
            .trim_end_matches('/')
            .replace('\\', "/")
    };
    // The root pathspec — `.`, `./`, or the empty string after normalising a
    // worktree-relative path at the repo root — selects the whole tree.
    let match_all = pathspec.iter().any(|s| {
        let n = norm(s);
        n.is_empty() || n == "."
    });
    if match_all {
        let mut all: Vec<String> = candidates.iter().cloned().collect();
        all.sort();
        all.dedup();
        return all;
    }
    let specs: Vec<String> = pathspec
        .iter()
        .map(|s| norm(s))
        .filter(|s| !s.is_empty())
        .collect();
    let mut matched: Vec<String> = candidates
        .iter()
        .filter(|path| {
            let p = norm(path);
            specs
                .iter()
                .any(|spec| p == *spec || p.starts_with(&format!("{spec}/")))
        })
        .cloned()
        .collect();
    matched.sort();
    matched.dedup();
    matched
}

/// `stash push -- <pathspec>`: stash only the changes to the matched paths.
///
/// The stash trees are HEAD overlaid with the matched paths' index / working-tree
/// content, so unmatched paths read as unchanged from HEAD — a later edit to one
/// of them can never produce a spurious conflict on `stash pop` (which now merges
/// onto the live working tree). After recording the stash, ONLY the matched paths
/// are reset to HEAD; the rest of the working tree is left exactly as it was.
///
/// `-u`/`-a`/`-k` are not yet modelled together with a pathspec and are rejected
/// (LBR-CLI-002) rather than silently ignored.
async fn run_push_pathspec(options: StashPushOptions) -> Result<StashOutput, StashError> {
    // `-u`/`-a`/`-k` with a pathspec are not yet modelled; reject the combination
    // explicitly rather than silently ignoring the option.
    if options.include_untracked {
        return Err(StashError::PathspecWithOption("--include-untracked".into()));
    }
    if options.include_ignored {
        return Err(StashError::PathspecWithOption("--all".into()));
    }
    if options.keep_index {
        return Err(StashError::PathspecWithOption("--keep-index".into()));
    }

    let git_dir = util::request_storage_path();
    let index_path = path::index();
    let index = Index::load(&index_path).unwrap_or_else(|_| Index::new());
    let workdir = &util::request_working_dir();

    let head_commit_hash = Head::current_commit()
        .await
        .ok_or(StashError::NoInitialCommit)?;
    let head_commit: Commit =
        load_object(&head_commit_hash).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let head_tree: Tree =
        load_object(&head_commit.tree_id).map_err(|e| StashError::ReadObject(e.to_string()))?;

    // HEAD file map — the baseline every stash tree starts from.
    let head_map: HashMap<PathBuf, MergeTreeEntry> = head_tree
        .get_plain_items_with_mode()
        .into_iter()
        .map(|(path, hash, mode)| (path, MergeTreeEntry::new(hash, mode)))
        .collect();

    // Candidate paths the pathspec can match: HEAD ∪ index-tracked.
    let mut candidates: HashSet<String> = head_map
        .keys()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    for p in index.tracked_files() {
        candidates.insert(p.to_string_lossy().replace('\\', "/"));
    }
    // Normalise each pathspec to a worktree-relative path so a pathspec given
    // relative to the caller's current directory (a subdirectory of the repo)
    // matches the repo-root-relative candidates — like Git's other pathspec
    // commands. `to_workdir_path` resolves against the repo root.
    let normalised: Vec<String> = options
        .pathspec
        .iter()
        .map(|spec| {
            util::to_workdir_path(spec)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    let matched = paths_matching_pathspec(&normalised, &candidates);
    if matched.is_empty() {
        return Err(StashError::PathspecNoMatch(options.pathspec.join(" ")));
    }

    // Stash worktree tree = HEAD overlaid with each matched path's EFFECTIVE
    // change. An unstaged working-tree change wins; otherwise a staged-only
    // change is folded in (Libra has no `stash apply --index`, so the worktree
    // restore must carry staged selections too, else `pop` would silently drop
    // them); otherwise the path stays at HEAD. This is what `pop` replays.
    let mut worktree_map = head_map.clone();
    for path in &matched {
        let rel = PathBuf::from(path);
        let full = workdir.join(&rel);
        let head_entry = head_map.get(&rel).copied();

        let worktree_entry = if full.is_file() {
            let content = fs::read(&full).map_err(|e| StashError::ReadObject(e.to_string()))?;
            let blob_hash = object::write_git_object(&git_dir, "blob", &content)
                .map_err(|e| StashError::WriteObject(e.to_string()))?;
            let meta = fs::metadata(&full).map_err(|e| StashError::ReadObject(e.to_string()))?;
            Some(MergeTreeEntry::new(
                blob_hash,
                tree_item_mode_from_metadata(&meta),
            ))
        } else {
            None
        };
        let index_entry = index
            .get(path, 0)
            .map(|e| MergeTreeEntry::new(e.hash, index_mode_to_tree_mode(e.mode)));

        let effective = if worktree_entry != head_entry {
            worktree_entry
        } else if index_entry != head_entry {
            index_entry
        } else {
            head_entry
        };
        match effective {
            Some(entry) => {
                worktree_map.insert(rel, entry);
            }
            None => {
                worktree_map.remove(&rel);
            }
        }
    }
    // Stash index tree (parent 2) = HEAD overlaid with the matched paths' staged content.
    let mut index_map = head_map.clone();
    for path in &matched {
        let rel = PathBuf::from(path);
        if let Some(entry) = index.get(path, 0) {
            index_map.insert(
                rel,
                MergeTreeEntry::new(entry.hash, index_mode_to_tree_mode(entry.mode)),
            );
        } else {
            index_map.remove(&rel);
        }
    }

    // Nothing to stash only when BOTH the working-tree and index overlays leave
    // every matched path at HEAD — a staged-only change (e.g. a staged deletion)
    // must still be stashed even when the working tree matches HEAD.
    if worktree_map == head_map && index_map == head_map {
        return Ok(StashOutput::Noop {
            message: "No local changes to save".to_string(),
        });
    }
    let worktree_tree_hash =
        create_tree_from_items_map(&worktree_map).map_err(StashError::WriteObject)?;
    let index_tree_hash =
        create_tree_from_items_map(&index_map).map_err(StashError::WriteObject)?;

    // Stash metadata + commits.
    let (author, committer) = util::create_signatures().await;
    let head_commit_hash_str = head_commit_hash.to_string();
    let head_commit_short = head_commit_hash_str
        .get(..7)
        .unwrap_or(head_commit_hash_str.as_str());
    let head_summary = head_commit.message.lines().next().unwrap_or("").to_string();
    let branch_name = match Head::current().await {
        Head::Branch(name) => name,
        Head::Detached(_) => "(no branch)".to_string(),
    };
    let final_message = options
        .message
        .clone()
        .unwrap_or_else(|| format!("WIP on {branch_name}: {head_commit_short} {head_summary}"));

    let index_commit = Commit::new(
        author.clone(),
        committer.clone(),
        index_tree_hash,
        vec![head_commit_hash],
        &final_message,
    );
    let index_commit_data = index_commit
        .to_data()
        .map_err(|e| StashError::WriteObject(e.to_string()))?;
    let index_commit_hash = object::write_git_object(&git_dir, "commit", &index_commit_data)
        .map_err(|e| StashError::WriteObject(e.to_string()))?;

    let stash_commit = Commit::new(
        author,
        committer.clone(),
        worktree_tree_hash,
        vec![head_commit_hash, index_commit_hash],
        &final_message,
    );
    let stash_commit_data = stash_commit
        .to_data()
        .map_err(|e| StashError::WriteObject(e.to_string()))?;
    let stash_commit_hash = object::write_git_object(&git_dir, "commit", &stash_commit_data)
        .map_err(|e| StashError::WriteObject(e.to_string()))?;

    let pushed_raw_line =
        update_stash_ref_locked(&git_dir, &stash_commit_hash, &committer, &final_message)?;

    // Reset ONLY the matched paths to HEAD (worktree + index); leave the rest.
    reset_pathspec_to_head(&matched, &head_map, workdir, &index_path)?;

    Ok(StashOutput::Push {
        message: final_message,
        stash_id: stash_commit_hash.to_string(),
        included_untracked: 0,
        kept_index: false,
        raw_line: pushed_raw_line,
    })
}

/// Reset only the given paths to their HEAD state, in both the working tree and
/// the on-disk index, leaving every other path untouched. A path absent from
/// HEAD (a matched add) is removed from the working tree and the index.
fn reset_pathspec_to_head(
    matched: &[String],
    head_map: &HashMap<PathBuf, MergeTreeEntry>,
    workdir: &Path,
    index_path: &Path,
) -> Result<(), StashError> {
    let mut index = Index::load(index_path).unwrap_or_else(|_| Index::new());
    for path in matched {
        let rel = PathBuf::from(path);
        let full = workdir.join(&rel);
        index.remove(path, 0);
        match head_map.get(&rel) {
            Some(entry) => {
                let blob: Blob =
                    load_object(&entry.hash).map_err(|e| StashError::ReadObject(e.to_string()))?;
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| StashError::WriteObject(e.to_string()))?;
                }
                fs::write(&full, &blob.data).map_err(|e| StashError::WriteObject(e.to_string()))?;
                #[cfg(unix)]
                {
                    let perm = std::fs::Permissions::from_mode(tree_mode_to_unix_perm(entry.mode));
                    let _ = fs::set_permissions(&full, perm);
                }
                // Pass the repo-relative path: the entry name is recorded
                // verbatim (an absolute path would corrupt the index). The
                // entry is smudged unconditionally (`pre_read = None`): the
                // blob was JUST written from the object store, but a
                // concurrent edit between that write and the stat would
                // pair the old hash with a post-edit stat — zeroed stats
                // make the next status content-compare instead (2026-08-06
                // R0-8 review).
                let mut new_entry =
                    crate::command::verified_index_entry(&rel, entry.hash, workdir, None)
                        .map_err(|e| StashError::IndexSave(e.to_string()))?;
                // Preserve HEAD's recorded file mode (e.g. the executable bit),
                // which a plain `new_from_file` would re-derive from disk.
                new_entry.mode = match entry.mode {
                    TreeItemMode::BlobExecutable => 0o100755,
                    TreeItemMode::Link => 0o120000,
                    _ => 0o100644,
                };
                index.add(new_entry);
            }
            None => {
                if full.exists() {
                    fs::remove_file(&full).map_err(|e| StashError::WriteObject(e.to_string()))?;
                }
            }
        }
    }
    index
        .save(index_path)
        .map_err(|e| StashError::IndexSave(e.to_string()))?;
    Ok(())
}

async fn run_pop(stash: Option<String>) -> Result<StashOutput, StashError> {
    // Phase 1 (C.10): resolve ONCE — the entry's commit hash pins the apply
    // content, and its RAW REFLOG LINE is the unambiguous entry identity for
    // the later CAS delete (the same commit id can legitimately appear more
    // than once on the stack; reflog lines chain the previous tip, so a
    // line identifies exactly one entry).
    let (index, stash_id, raw_line) = resolve_stash_to_commit_hash(stash)?;
    let stash_commit_hash =
        ObjectHash::from_str(&stash_id).map_err(|e| StashError::ReadObject(e.to_string()))?;
    apply_stash_commit(&stash_commit_hash).await?;
    let branch = match Head::current().await {
        Head::Branch(name) => name,
        Head::Detached(_) => "(no branch)".to_string(),
    };

    // Phase 2 (C.10): drop ONLY the applied entry — located by its raw line
    // under the stack lock. A CAS miss keeps the entry and reports; the
    // successful local apply is never rolled back.
    match do_drop(None, Some(&raw_line)) {
        Ok(_) => {}
        Err(StashError::StackChanged) => {
            return Err(StashError::StackChangedAfterApply { stash_id });
        }
        Err(other) => return Err(other),
    }

    Ok(StashOutput::Pop {
        index,
        stash_id,
        branch,
    })
}

/// Stash the tracked working-tree changes for `pull --autostash`. Returns
/// the created entry's `(commit id, raw reflog line)`, or `None` when there
/// was nothing to stash (clean tree). Untracked/ignored files are left in
/// place, matching Git's autostash. The caller must pop via
/// [`autostash_pop_by_entry`] with BOTH values — the raw line captured at
/// push time names exactly this entry even if another worktree later pushes
/// the same commit id onto the shared stack.
pub(crate) async fn autostash_push() -> Result<Option<(String, String)>, String> {
    let options = StashPushOptions {
        message: Some("autostash before pull".to_string()),
        include_untracked: false,
        include_ignored: false,
        keep_index: false,
        pathspec: Vec::new(),
    };
    match run_push(options).await.map_err(|e| e.to_string())? {
        StashOutput::Noop { .. } => Ok(None),
        StashOutput::Push {
            stash_id, raw_line, ..
        } => Ok(Some((stash_id, raw_line))),
        other => Err(format!(
            "internal error: expected stash push output, got {other:?}"
        )),
    }
}

/// Re-apply and drop EXACTLY the autostash created by [`autostash_push`]
/// (W2 §C.4.3): the push-time RAW REFLOG LINE names the entry (immune to a
/// duplicate commit id pushed by another worktree), the apply is pinned BY
/// HASH, and the delete goes through the raw-line CAS `do_drop`.
pub(crate) async fn autostash_pop_by_entry(
    expected_id: &str,
    expected_line: &str,
) -> Result<(), String> {
    let entries = stack_entries().map_err(|e| e.to_string())?;
    if !entries.iter().any(|entry| entry.raw_line == expected_line) {
        return Err(format!(
            "the autostash entry {expected_id} is no longer on the stash stack (a concurrent \
             pop/drop consumed it) — your stashed changes may have been applied elsewhere; \
             inspect `libra stash list`"
        ));
    }
    let hash = ObjectHash::from_str(expected_id).map_err(|e| e.to_string())?;
    apply_stash_commit(&hash).await.map_err(|e| e.to_string())?;
    match do_drop(None, Some(expected_line)) {
        Ok(_) => Ok(()),
        Err(StashError::StackChanged) => Err(format!(
            "the autostash was re-applied, but the stash stack changed concurrently so entry \
             {expected_id} was kept — `libra stash drop` it explicitly if desired"
        )),
        Err(other) => Err(other.to_string()),
    }
}

/// One consistent read of the shared stack's entries (empty when absent).
fn stack_entries() -> Result<Vec<StashLogEntry>, StashError> {
    // §C.10: repair a tip left stale by a crash before reporting the stack, so
    // a reader and a later mutation cannot disagree about what the top entry
    // is. The repair takes the STACK LOCK — an unlocked repair could overwrite
    // a tip another process had just published.
    reconcile_stash_ref_locked(&util::request_storage_path())?;
    let git_dir = util::request_storage_path();
    let stash_log_path = git_dir.join("logs/refs/stash");
    if !stash_log_path.exists() {
        return Ok(Vec::new());
    }
    parse_stash_log_entries(read_stash_log_lines(&stash_log_path)?)
}

async fn run_list() -> Result<StashOutput, StashError> {
    // §C.10: repair a stale tip before reporting the stack (unlocked entry
    // point, so the locked form is safe).
    reconcile_stash_ref_locked(&util::request_storage_path())?;
    if !has_stash()? {
        return Ok(StashOutput::List {
            entries: Vec::new(),
        });
    }

    let git_dir = util::request_storage_path();
    let stash_log_path = git_dir.join("logs/refs/stash");
    if !stash_log_path.exists() {
        return Ok(StashOutput::List {
            entries: Vec::new(),
        });
    }
    let entries = parse_stash_log_entries(read_stash_log_lines(&stash_log_path)?)?
        .into_iter()
        .enumerate()
        .map(|(index, entry)| StashListEntry {
            index,
            message: entry.message,
            stash_id: entry.stash_id,
        })
        .collect();

    Ok(StashOutput::List { entries })
}

async fn run_apply(stash: Option<String>) -> Result<StashOutput, StashError> {
    do_apply(stash).await
}

async fn run_drop(stash: Option<String>) -> Result<StashOutput, StashError> {
    do_drop(stash, None)
}

async fn run_show(
    stash: Option<String>,
    name_only: bool,
    name_status: bool,
    patch: bool,
) -> Result<StashOutput, StashError> {
    let (index, stash_id_str, _raw_line) = resolve_stash_to_commit_hash(stash)?;
    let git_dir = util::request_storage_path();

    let stash_hash =
        ObjectHash::from_str(&stash_id_str).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_commit: Commit =
        load_object(&stash_hash).map_err(|e| StashError::ReadObject(e.to_string()))?;

    let base_hash = *stash_commit
        .parent_commit_ids
        .first()
        .ok_or_else(|| StashError::ReadObject("stash commit is malformed".into()))?;
    let base_commit: Commit =
        load_object(&base_hash).map_err(|e| StashError::ReadObject(e.to_string()))?;

    let base_tree: Tree =
        load_object(&base_commit.tree_id).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_tree: Tree =
        load_object(&stash_commit.tree_id).map_err(|e| StashError::ReadObject(e.to_string()))?;

    let base_files = tree::get_tree_files_recursive(&base_tree, &git_dir, &PathBuf::new())
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_files = tree::get_tree_files_recursive(&stash_tree, &git_dir, &PathBuf::new())
        .map_err(|e| StashError::ReadObject(e.to_string()))?;

    let mut files: Vec<StashFileChange> = Vec::new();
    let mut stats = StashFilesChangedStats::default();
    let mut seen = HashSet::new();

    for (path, stash_item) in stash_files.iter() {
        seen.insert(path.clone());
        match base_files.get(path) {
            Some(base_item) => {
                if base_item.id != stash_item.id {
                    files.push(StashFileChange {
                        path: path.clone(),
                        status: "modified".to_string(),
                    });
                    stats.modified += 1;
                }
            }
            None => {
                files.push(StashFileChange {
                    path: path.clone(),
                    status: "added".to_string(),
                });
                stats.added += 1;
            }
        }
    }
    for path in base_files.keys() {
        if !seen.contains(path) {
            files.push(StashFileChange {
                path: path.clone(),
                status: "deleted".to_string(),
            });
            stats.deleted += 1;
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    stats.total = files.len();

    // `-p`/`--patch`: the stashed changes as a unified diff. A stash commit's
    // first parent IS the base it was created against, so `log::generate_diff`
    // (which diffs a commit against its first parent — the same git-faithful
    // engine `log`/`show`/`format-patch` use) yields exactly the stash diff.
    let patch_text = if patch {
        Some(
            log::generate_diff(&stash_commit, Vec::new())
                .await
                .map_err(|e| {
                    StashError::ReadObject(format!("failed to generate stash diff: {e}"))
                })?,
        )
    } else {
        None
    };

    Ok(StashOutput::Show {
        stash: format!("stash@{{{index}}}"),
        stash_id: stash_id_str,
        files,
        files_changed: stats,
        patch: patch_text,
        name_only,
        name_status,
    })
}

/// Durable rollback journal for `stash branch` (W2 §C.10).
///
/// The command is create-branch → switch-HEAD → apply → drop. A failure after
/// the first two must undo them, and the UNDO itself can fail (or the process
/// can die mid-way) — so the intent to roll back is recorded durably BEFORE
/// HEAD moves, and [`recover_stash_branch_journal`] completes it on the next
/// stash invocation. The working tree is never touched by recovery: a
/// half-applied stash leaves dirty files, and deleting them would destroy the
/// only copy of the user's changes.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StashBranchJournal {
    branch: String,
    /// The branch's creation tip — deletion is tip-conditional, so a branch
    /// the user has since committed to is KEPT.
    base: String,
    /// The HEAD to restore: a branch name, or a detached commit id.
    prior_branch: Option<String>,
    prior_detached: Option<String>,
    /// How far the command provably got (W2 r6 #1, r7 #1):
    ///
    /// * `prepared`  — recorded BEFORE the exclusive create. Whether the
    ///   create COMMITTED is answered by the provenance row the create writes
    ///   in its own transaction — never inferred from this file, because a
    ///   crash can land on either side of the transaction.
    /// * `committed` — the command succeeded past its mutating phase but the
    ///   journal could not be cleared; recovery only clears it. Rolling back
    ///   here would delete a branch the user is standing on.
    #[serde(default = "StashBranchJournal::default_phase")]
    phase: String,
    /// The key under which the create recorded its provenance (the created
    /// reference's row id) — atomically with the branch row itself.
    #[serde(default)]
    nonce: String,
}

/// The SEMANTIC OID fields of this gitdir's `stash-branch-journal.json`, for
/// GC root collection (plan-20260714 §C.4.3 / §C.10): `base` always, and
/// `prior_detached` when the interrupted command left a detached HEAD to
/// restore. Branch names, the phase, and the 32-hex nonce are text and are
/// NOT returned.
pub(crate) fn stash_branch_journal_gc_oids(
    gitdir: &Path,
) -> Result<Option<Vec<(&'static str, String)>>, String> {
    let path = gitdir.join("stash-branch-journal.json");
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    let journal: StashBranchJournal = serde_json::from_str(&data)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    let mut oids = vec![("base", journal.base)];
    if let Some(prior) = journal.prior_detached {
        oids.push(("prior_detached", prior));
    }
    Ok(Some(oids))
}

impl StashBranchJournal {
    fn path() -> PathBuf {
        util::request_worktree_gitdir_strict().join("stash-branch-journal.json")
    }

    fn write(&self) -> Result<(), StashError> {
        let data = serde_json::to_vec_pretty(self)
            .map_err(|error| StashError::WriteObject(error.to_string()))?;
        crate::utils::atomic_write::write_atomic(&Self::path(), &data, true)
            .map_err(|error| StashError::WriteObject(error.to_string()))
    }

    fn clear() -> Result<(), StashError> {
        remove_durably(&Self::path())
    }

    /// No released binary ever wrote a phase-less journal; treating one as
    /// any known phase would be a guess, and recovery refuses guesses.
    fn default_phase() -> String {
        "unknown".to_string()
    }

    fn with_phase(&self, phase: &str) -> Self {
        Self {
            branch: self.branch.clone(),
            base: self.base.clone(),
            prior_branch: self.prior_branch.clone(),
            prior_detached: self.prior_detached.clone(),
            phase: phase.to_string(),
            nonce: self.nonce.clone(),
        }
    }

    fn prior_head(&self) -> Option<Head> {
        if let Some(branch) = &self.prior_branch {
            return Some(Head::Branch(branch.clone()));
        }
        self.prior_detached
            .as_deref()
            .and_then(|oid| ObjectHash::from_str(oid).ok())
            .map(Head::Detached)
    }
}

/// Complete a rollback an earlier `stash branch` recorded but could not
/// finish (§C.10 expected-state recovery). Runs at every stash entry point;
/// a missing journal is the fast path. Unreadable journals refuse the
/// command rather than guessing.
/// Conclude a journaled creation in-process (HEAD already restored by the
/// caller): the branch is removed by provenance, and on success the journal
/// is cleared. Any failure leaves the journal for the next invocation.
async fn conclude_journaled_branch_or_warn(journal: &StashBranchJournal) {
    match InternalBranch::conclude_journaled_branch(&journal.nonce, &journal.base).await {
        Ok(_) => {
            if let Err(journal_error) = StashBranchJournal::clear() {
                eprintln!(
                    "warning: the completed rollback's journal could not be removed \
                     ({journal_error}); the next stash command will re-verify it"
                );
            }
        }
        Err(error) => {
            eprintln!(
                "warning: could not conclude the created branch ({error}); the rollback \
                 is journaled and will complete on the next stash command"
            );
        }
    }
}

async fn recover_stash_branch_journal() -> Result<(), StashError> {
    let path = StashBranchJournal::path();
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(StashError::Other(format!(
                "cannot read the stash-branch rollback journal '{}': {error}",
                path.display()
            )));
        }
    };
    let journal: StashBranchJournal = serde_json::from_str(&data).map_err(|error| {
        StashError::Other(format!(
            "the stash-branch rollback journal '{}' is corrupt ({error}); inspect and \
             remove it manually",
            path.display()
        ))
    })?;

    // The PHASE bounds what recovery may touch; whether the create COMMITTED
    // is answered by the provenance row it wrote in its own transaction —
    // never inferred from this file, because a crash can land on either side
    // of that transaction.
    match journal.phase.as_str() {
        "prepared" => {}
        // The command succeeded; only the journal itself is stale.
        "committed" => {
            StashBranchJournal::clear()?;
            return Ok(());
        }
        other => {
            return Err(StashError::Other(format!(
                "the stash-branch rollback journal '{}' records unknown phase '{other}'; \
                 inspect and remove it manually",
                path.display()
            )));
        }
    }

    // PROVENANCE first, read-only (W2 r8 #1): no row means the create never
    // committed — and then NOTHING may be touched, HEAD included. A user may
    // have created and switched to a branch of the journaled name themselves
    // after the interrupted command; moving their HEAD on the strength of a
    // `prepared` journal alone would hijack it.
    let created_by_us = InternalBranch::journaled_provenance_exists(&journal.nonce)
        .await
        .map_err(|error| {
            StashError::Other(format!(
                "rollback cannot read the creation provenance ({error}); the journal is \
                 kept and the next stash command will retry"
            ))
        })?;
    if !created_by_us {
        StashBranchJournal::clear()?;
        return Ok(());
    }

    // HEAD next: if it still points at the journaled branch, restore the
    // prior head; if the user already moved on, leave HEAD alone.
    if let Head::Branch(current) = Head::current().await
        && current == journal.branch
        && let Some(prior) = journal.prior_head()
    {
        Head::update_result(prior, None)
            .await
            .map_err(|error| StashError::Other(format!("rollback cannot restore HEAD: {error}")))?;
    }
    // Branch second, BY PROVENANCE (W2 r7 #1/#2): the create recorded the
    // branch row's id atomically with the row itself, so recovery deletes
    // exactly the row that create made — never a same-name same-tip branch
    // the user recreated — and a missing provenance row proves the create
    // never committed. A transient store error keeps the journal so the next
    // invocation retries.
    let base = ObjectHash::from_str(&journal.base).map_err(|error| {
        StashError::Other(format!(
            "the rollback journal's base '{}' is not a valid object id ({error}); \
             inspect and remove '{}' manually",
            journal.base,
            path.display()
        ))
    })?;
    use crate::internal::branch::JournaledBranchFate;
    let branch_note =
        match InternalBranch::conclude_journaled_branch(&journal.nonce, &base.to_string()).await {
            // The create never committed, or the recorded row was deleted at
            // its base — nothing of the user's was touched.
            Ok(JournaledBranchFate::NeverCreated | JournaledBranchFate::Deleted) => None,
            // The recorded row is gone or its tip moved: the user's now, KEPT
            // — said out loud, because a silently retained branch from a
            // half-failed command looks like corruption when found later.
            Ok(JournaledBranchFate::KeptOrGone) => Some(format!(
                "branch '{}' was KEPT: it no longer matches the journaled creation (its \
                 tip moved, or it was recreated), so it now carries your work — delete it \
                 with `libra branch -d {}` if unwanted",
                journal.branch, journal.branch
            )),
            Err(error) => {
                return Err(StashError::Other(format!(
                    "rollback cannot conclude branch '{}' ({error}); the journal is kept \
                     and the next stash command will retry",
                    journal.branch
                )));
            }
        };
    StashBranchJournal::clear()?;
    crate::utils::error::emit_warning(format!(
        "completed the rollback of an interrupted `stash branch {}` (the working tree \
         was left untouched)",
        journal.branch
    ));
    if let Some(note) = branch_note {
        crate::utils::error::emit_warning(note);
    }
    Ok(())
}

async fn run_branch(branch_name: String, stash: Option<String>) -> Result<StashOutput, StashError> {
    // Resolve stash & metadata for the new branch base. The raw reflog line
    // is the unambiguous entry identity for the post-apply CAS delete.
    let (index, stash_id_str, raw_line) = resolve_stash_to_commit_hash(stash)?;
    let stash_hash =
        ObjectHash::from_str(&stash_id_str).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_commit: Commit =
        load_object(&stash_hash).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let base_hash = *stash_commit
        .parent_commit_ids
        .first()
        .ok_or_else(|| StashError::ReadObject("stash commit is malformed".into()))?;

    // Capture the restore point BEFORE any persistent mutation (W2 §C.4.3):
    // both the branch creation and the HEAD switch may need rolling back.
    let prior_head = Head::current().await;
    // §C.10: record the rollback intent DURABLY before ANY mutation — the
    // journal precedes even the branch create, so there is no window in which
    // a created branch exists without its recovery record. A journal whose
    // branch was never created recovers as a no-op (the tip-conditional
    // delete finds nothing, HEAD never moved).
    let journal = StashBranchJournal {
        branch: branch_name.clone(),
        base: base_hash.to_string(),
        prior_branch: match &prior_head {
            Head::Branch(name) => Some(name.clone()),
            Head::Detached(_) => None,
        },
        prior_detached: match &prior_head {
            Head::Detached(oid) => Some(oid.to_string()),
            Head::Branch(_) => None,
        },
        phase: "prepared".to_string(),
        nonce: uuid::Uuid::now_v7().simple().to_string(),
    };
    journal.write()?;

    // CREATE, never upsert (Codex W2 r1 #4): `update_branch` moves an existing
    // tip, and a name checked free a moment earlier can be taken by another
    // worktree before the write — `stash branch` would then silently move
    // THAT branch. The exclusive create does the check and the insert in one
    // write-locked transaction, so a collision is refused, not overwritten.
    if let Err(error) = InternalBranch::create_branch_exclusive(
        &branch_name,
        &base_hash.to_string(),
        None,
        // Provenance committed WITH the branch row (W2 r7 #1): recovery asks
        // this row — not the journal file — whether the create ever
        // committed, and rolls back by the recorded row id, never by
        // name+tip (r7 #2).
        Some(&journal.nonce),
    )
    .await
    {
        // Nothing was mutated; a journal that cannot be cleared is harmless
        // (recovery no-ops on it) but the user should know.
        if let Err(clear_error) = StashBranchJournal::clear() {
            eprintln!("warning: could not remove the rollback journal: {clear_error}");
        }
        return Err(match error {
            crate::internal::branch::BranchStoreError::AlreadyExists(name) => {
                StashError::BranchExists(name)
            }
            other => stash_branch_store_error(&branch_name, other),
        });
    }
    // Switch HEAD to the new branch so apply runs on the right tip — via the
    // RESULT-returning API (W2 §C.4.3 scoped HEAD guard): a swallowed HEAD
    // failure would apply the stash onto the wrong branch tip. On failure the
    // journaled creation is concluded by provenance; if that fails too, the
    // journal persists and the next stash invocation retries.
    if let Err(head_error) = Head::update_result(Head::Branch(branch_name.clone()), None).await {
        conclude_journaled_branch_or_warn(&journal).await;
        return Err(StashError::Other(format!(
            "failed to switch HEAD to new branch '{branch_name}': {head_error}"
        )));
    }

    // Apply BY HASH (pinned to the resolved entry's content).
    if let Err(apply_error) = apply_stash_commit(&stash_hash).await {
        // Roll back the half-created state (new branch + switched HEAD). If
        // any step fails, the JOURNAL persists and the next stash invocation
        // finishes the rollback — the user is never left with a silent
        // half-state; the ORIGINAL apply error surfaces either way.
        match Head::update_result(prior_head, None).await {
            Err(head_error) => {
                eprintln!(
                    "warning: could not switch HEAD back after the failed stash apply \
                     ({head_error}); the rollback is journaled and will complete on the \
                     next stash command"
                );
            }
            Ok(()) => {
                conclude_journaled_branch_or_warn(&journal).await;
            }
        }
        return Err(apply_error);
    }
    // The command succeeded past its mutating phase: the journaled rollback
    // must not fire later. The provenance row goes first — while it exists, a
    // stale `prepared` journal would roll back the branch the user now owns.
    if let Err(error) = InternalBranch::clear_journaled_branch_provenance(&journal.nonce).await {
        journal.with_phase("committed").write().map_err(|_| {
            StashError::Other(format!(
                "the command succeeded, but its creation provenance could not be cleared \
                 ({error}); remove '{}' manually before the next stash command",
                StashBranchJournal::path().display()
            ))
        })?;
    }
    // If the journal cannot be REMOVED, it is demoted to `committed` —
    // recovery only clears that phase — and only if even the demotion fails
    // does the command error, naming the file.
    if let Err(clear_error) = StashBranchJournal::clear() {
        journal.with_phase("committed").write().map_err(|_| {
            StashError::Other(format!(
                "the command succeeded, but its rollback journal could not be removed OR \
                 demoted ({clear_error}); remove '{}' manually before the next stash \
                 command, or it will roll back the branch you are on",
                StashBranchJournal::path().display()
            ))
        })?;
    }
    let applied = true;
    let dropped = {
        // Unified CAS deletion (same do_drop path as pop): locate the applied
        // entry by its raw line under the stack lock; a CAS miss keeps the
        // entry and reports without failing the branch creation or the apply.
        match do_drop(None, Some(&raw_line)) {
            Ok(_) => true,
            Err(StashError::StackChanged) => {
                eprintln!(
                    "warning: the stash stack changed concurrently — entry {stash_id_str} was \
                     kept; `libra stash drop` it explicitly if desired"
                );
                false
            }
            Err(other) => {
                // The branch exists, HEAD moved, the apply landed — returning
                // an error here would report failure for a command whose
                // user-visible work all succeeded, with no rollback that
                // could be non-destructive. The entry stays on the stack
                // (nothing was published), which is the same safe state as a
                // CAS miss; say so and succeed.
                eprintln!(
                    "warning: could not drop stash entry {stash_id_str} after the apply \
                     ({other}); it remains on the stack — `libra stash drop` it explicitly"
                );
                false
            }
        }
    };

    Ok(StashOutput::Branch {
        branch: branch_name,
        stash: format!("stash@{{{index}}}"),
        stash_id: stash_id_str,
        applied,
        dropped,
    })
}

fn stash_branch_store_error(branch: &str, error: BranchStoreError) -> StashError {
    StashError::BranchLookupFailed {
        branch: branch.to_string(),
        detail: error.to_string(),
    }
}

async fn run_clear(force: bool, output: &OutputConfig) -> Result<StashOutput, StashError> {
    if !force && !output.is_json() {
        return Err(StashError::ClearRequiresForce);
    }

    // The whole read→count→delete sequence runs under the stack lock (W2
    // §C.4.3) so a concurrent push/drop cannot interleave.
    let _stack_lock = acquire_stash_stack_lock()?;
    let git_dir = util::request_storage_path();
    // §C.10: repair under the lock this frame holds (bare form — flock is not
    // reentrant) before the emptiness gate reads the ref.
    reconcile_stash_ref(&git_dir)?;
    if !has_stash()? {
        return Ok(StashOutput::Clear { cleared_count: 0 });
    }

    let stash_log_path = git_dir.join("logs/refs/stash");

    let cleared = if stash_log_path.exists() {
        let entries = parse_stash_log_entries(read_stash_log_lines(&stash_log_path)?)?;
        entries.len()
    } else {
        0
    };

    // The SAME publication path as push and drop (§C.10): an empty stack is a
    // published state, not a pair of ad-hoc unlinks, so `clear` gets the same
    // order and the same durability.
    publish_stash_stack(&git_dir, &[])?;

    Ok(StashOutput::Clear {
        cleared_count: cleared,
    })
}

// ── Rendering ────────────────────────────────────────────────────────

fn render_stash_output(result: &StashOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("stash", result, output);
    }

    if output.quiet {
        return Ok(());
    }

    match result {
        StashOutput::Noop { message } => {
            println!("{message}");
        }
        StashOutput::Push { message, .. } => {
            println!("Saved working directory and index state {message}");
        }
        StashOutput::Pop {
            index,
            stash_id,
            branch,
        } => {
            println!("On branch {branch}");
            println!(
                "Dropped stash@{{{index}}} ({})",
                &stash_id[..stash_id.len().min(7)]
            );
        }
        StashOutput::Apply { index, branch, .. } => {
            println!("On branch {branch}");
            println!("Applied stash@{{{index}}}");
        }
        StashOutput::Drop { index, stash_id } => {
            println!(
                "Dropped stash@{{{index}}} ({})",
                &stash_id[..stash_id.len().min(7)]
            );
        }
        StashOutput::List { entries } => {
            for entry in entries {
                println!("stash@{{{}}}: {}", entry.index, entry.message);
            }
        }
        StashOutput::Show {
            stash,
            files,
            files_changed,
            patch,
            name_only,
            name_status,
            ..
        } => {
            if let Some(patch) = patch {
                // `-p`/`--patch`: emit the unified diff only (no file summary),
                // matching `git stash show -p`.
                print!("{patch}");
            } else if *name_only {
                for change in files {
                    println!("{}", change.path);
                }
            } else {
                println!("Files changed in {stash}:");
                let prefix_len = if *name_status { 0 } else { 9 };
                for change in files {
                    if *name_status {
                        println!("{}\t{}", change.status, change.path);
                    } else {
                        println!(
                            "  {:<prefix_len$}{}",
                            format!("{}:", change.status),
                            change.path
                        );
                    }
                }
                println!(
                    "{} files changed, {} insertions(+), {} deletions(-)",
                    files_changed.total, files_changed.added, files_changed.deleted
                );
            }
        }
        StashOutput::Branch {
            branch,
            stash,
            applied,
            dropped,
            ..
        } => {
            println!("Switched to a new branch '{branch}'");
            if *applied {
                println!("Applied {stash}");
            }
            if *dropped {
                println!("Dropped {stash}");
            }
        }
        StashOutput::Clear { cleared_count } => {
            if *cleared_count == 0 {
                println!("No stash entries to clear.");
            } else if *cleared_count == 1 {
                println!("Cleared 1 stash entry.");
            } else {
                println!("Cleared {cleared_count} stash entries.");
            }
        }
    }
    Ok(())
}

// ── Internal helpers ─────────────────────────────────────────────────

async fn do_apply(stash: Option<String>) -> Result<StashOutput, StashError> {
    let (index, hash_str, _raw_line) = resolve_stash_to_commit_hash(stash)?;
    let stash_commit_hash =
        ObjectHash::from_str(&hash_str).map_err(|e| StashError::ReadObject(e.to_string()))?;
    apply_stash_commit(&stash_commit_hash).await?;

    let branch = match Head::current().await {
        Head::Branch(name) => name,
        Head::Detached(_) => "(no branch)".to_string(),
    };

    Ok(StashOutput::Apply {
        index,
        stash_id: hash_str,
        branch,
    })
}

/// Apply a stash COMMIT by OID — the three-way apply shared by
/// `stash apply/pop` and the merge autostash finalizer (which holds a stash
/// commit reachable only from its sidecar, never from refs/stash). All-or-
/// nothing for the working tree: any conflict or collision fails BEFORE files
/// are rewritten, leaving the current state intact. The current index is
/// intentionally preserved by default.
pub(crate) async fn apply_stash_commit(hash: &ObjectHash) -> Result<(), StashError> {
    apply_stash_commit_inner(hash, false).await
}

/// Apply a held autostash and restore its staged index layer as well as its
/// working-tree layer. Unlike ordinary `stash apply`, autostash temporarily
/// resets both layers, so leaving the index untouched would lose staged-only
/// changes once the held object becomes unreachable.
pub(crate) async fn apply_held_stash_commit(hash: &ObjectHash) -> Result<(), StashError> {
    apply_stash_commit_inner(hash, true).await
}

async fn apply_stash_commit_inner(
    hash: &ObjectHash,
    restore_index: bool,
) -> Result<(), StashError> {
    let stash_commit_hash = *hash;
    let git_dir = util::request_storage_path();

    let stash_commit: Commit =
        load_object(&stash_commit_hash).map_err(|e| StashError::ReadObject(e.to_string()))?;

    let base_commit_hash = *stash_commit
        .parent_commit_ids
        .first()
        .ok_or_else(|| StashError::ReadObject("stash commit is malformed".into()))?;
    let base_commit: Commit =
        load_object(&base_commit_hash).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let base_tree: Tree =
        load_object(&base_commit.tree_id).map_err(|e| StashError::ReadObject(e.to_string()))?;

    let stash_tree: Tree =
        load_object(&stash_commit.tree_id).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let untracked_tree = load_untracked_parent_tree(&stash_commit)?;
    let stash_index_tree = if restore_index {
        Some(load_stash_index_parent_tree(&stash_commit)?)
    } else {
        None
    };

    let workdir = &util::request_working_dir();
    let index_path = path::index();

    // "ours" for the three-way apply is the CURRENT working tree, NOT HEAD. This
    // preserves uncommitted changes that are not part of the stash — the paths a
    // pathspec `stash push` deliberately left behind, or unrelated edits made
    // after stashing. (Applying against HEAD would silently overwrite them.)
    // base = the commit the stash was created on; theirs = the stashed tree.
    // `create_tree_from_workdir` writes every blob/subtree it visits, so the
    // resulting tree is fully materialised for `merge_trees`.
    let current_index = Index::load(&index_path)
        .map_err(|e| StashError::IndexLoad(format!("{}: {e}", index_path.display())))?;
    let worktree_tree = create_tree_from_workdir(workdir, &git_dir, &current_index)
        .map_err(StashError::ReadObject)?;

    let merged_tree = merge_trees(&base_tree, &worktree_tree, &stash_tree, &git_dir)
        .map_err(StashError::MergeConflict)?;
    let restored_index = if let Some(stash_index_tree) = stash_index_tree.as_ref() {
        let current_index_tree = tree::create_tree_from_index(&current_index)
            .map_err(|error| StashError::WriteObject(error.to_string()))?;
        let merged_index_tree =
            merge_trees(&base_tree, &current_index_tree, stash_index_tree, &git_dir)
                .map_err(StashError::MergeConflict)?;
        let mut restored = Index::new();
        rebuild_index_from_tree(&merged_index_tree, &mut restored, "")
            .map_err(StashError::IndexLoad)?;
        Some(restored)
    } else {
        None
    };

    let worktree_files = tree::get_tree_files_recursive(&worktree_tree, &git_dir, &PathBuf::new())
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let merged_files = tree::get_tree_files_recursive(&merged_tree, &git_dir, &PathBuf::new())
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    if let Some(untracked_tree) = untracked_tree.as_ref() {
        ensure_untracked_restore_paths_clear(untracked_tree, workdir, &git_dir)?;
    }

    // A pure ADDITION in the merge result (absent from the current worktree
    // tree) must not silently overwrite an untracked file the user created at
    // the same path with different content — fail all-or-nothing instead
    // (the caller keeps/promotes the stash; nothing is lost on either side).
    for (path, merged_item) in &merged_files {
        if worktree_files.contains_key(path) {
            continue;
        }
        let full_path = workdir.join(path);
        if full_path.exists() {
            let existing = crate::command::calc_file_blob_hash(&full_path)
                .map_err(|e| StashError::ReadObject(e.to_string()))?;
            if existing != merged_item.id {
                return Err(StashError::MergeConflict(format!(
                    "untracked file '{path}' would be overwritten by the stashed addition"
                )));
            }
        }
    }

    // Remove any currently-tracked file the merge result drops (e.g. a deletion
    // recorded in the stash), based on the actual working tree rather than HEAD.
    for path in worktree_files.keys() {
        if !merged_files.contains_key(path) {
            let full_path = workdir.join(path);
            if full_path.exists() {
                fs::remove_file(full_path).map_err(|e| StashError::WriteObject(e.to_string()))?;
            }
        }
    }

    restore_working_directory_from_tree(&merged_tree, workdir, "")
        .map_err(StashError::WriteObject)?;
    if let Some(untracked_tree) = untracked_tree.as_ref() {
        restore_working_directory_from_tree(untracked_tree, workdir, "")
            .map_err(StashError::WriteObject)?;
    }

    if let Some(restored_index) = restored_index {
        restored_index
            .save(&index_path)
            .map_err(|error| StashError::IndexSave(error.to_string()))?;
    }

    // Git's default `stash apply/pop` restores changes to the working tree only.
    // Keep the existing index intact unless the caller is restoring a held
    // autostash, whose reset removed the staged layer as well. A future public
    // `--index` mode can reuse that path explicitly.

    Ok(())
}

fn load_stash_index_parent_tree(stash_commit: &Commit) -> Result<Tree, StashError> {
    let index_commit_hash = stash_commit
        .parent_commit_ids
        .get(1)
        .ok_or_else(|| StashError::ReadObject("stash index parent is missing".into()))?;
    let index_commit: Commit =
        load_object(index_commit_hash).map_err(|e| StashError::ReadObject(e.to_string()))?;
    load_object(&index_commit.tree_id).map_err(|error| StashError::ReadObject(error.to_string()))
}

fn load_untracked_parent_tree(stash_commit: &Commit) -> Result<Option<Tree>, StashError> {
    let Some(untracked_commit_hash) = stash_commit.parent_commit_ids.get(2) else {
        return Ok(None);
    };

    let untracked_commit: Commit =
        load_object(untracked_commit_hash).map_err(|e| StashError::ReadObject(e.to_string()))?;
    load_object(&untracked_commit.tree_id)
        .map(Some)
        .map_err(|e| StashError::ReadObject(e.to_string()))
}

fn ensure_untracked_restore_paths_clear(
    untracked_tree: &Tree,
    workdir: &Path,
    git_dir: &Path,
) -> Result<(), StashError> {
    let files = tree::get_tree_files_recursive(untracked_tree, git_dir, &PathBuf::new())
        .map_err(StashError::ReadObject)?;
    let mut conflicts: Vec<String> = files
        .keys()
        .filter(|path| workdir.join(Path::new(path)).exists())
        .cloned()
        .collect();
    conflicts.sort();

    if conflicts.is_empty() {
        return Ok(());
    }

    Err(StashError::MergeConflict(format!(
        "untracked files would be overwritten by stash apply:\n  {}",
        conflicts.join("\n  ")
    )))
}

/// Publish a stash-stack state so a crash can never leave the tip and the log
/// describing different stacks (§C.10).
///
/// `refs/stash` and `logs/refs/stash` are two files, and the old code wrote
/// them as two independent operations: a crash, a full disk or a kill between
/// them left a tip naming an entry the log did not list, or a log whose first
/// entry the tip did not name. Neither is recoverable by inspection, because
/// nothing recorded which of the two was meant to win.
///
/// The fix is not a second journal but an ORDER plus an authority. The LOG is
/// the stack — every entry, with the chaining that makes each line a unique
/// identity — and `refs/stash` is a derived pointer to its first line. So:
///
/// 1. the log is written first, atomically (temp file + rename) and fsynced,
///    so it is never torn and never lost after this call returns;
/// 2. the ref is written second, from the log that was just committed.
///
/// A crash between them leaves a STALE REF over a correct log, and that state
/// is repairable by anyone who reads it — which is what
/// [`reconcile_stash_ref`] does, under the same lock, before every read and
/// every mutation. There is no window in which the log is wrong, so there is
/// no window in which recovery has to guess.
fn publish_stash_stack(storage: &Path, entries: &[StashLogEntry]) -> Result<(), StashError> {
    let log_path = storage.join("logs/refs/stash");
    let ref_path = storage.join("refs/stash");
    let write_error = |path: &Path, error: std::io::Error| {
        StashError::WriteObject(format!("{}: {error}", path.display()))
    };

    if entries.is_empty() {
        // Removing the LOG first keeps the same authority order: an empty
        // stack with a leftover ref is repairable; a ref-less log is a stack
        // that would silently reappear. Each unlink is made DURABLE by
        // fsyncing the parent directory — an unlink that has not reached the
        // disk is exactly as lossy as a write that has not, and a power loss
        // that restored the log while keeping the ref deletion would let
        // reconciliation resurrect a stash the user just cleared.
        remove_durably(&log_path)?;
        remove_durably(&ref_path)?;
        return Ok(());
    }

    // Backfill: any line without a generation (written by an older binary)
    // gets one now, so the whole stack is ABA-proof after its first mutation.
    let body = entries
        .iter()
        .map(|entry| with_generation(&entry.raw_line))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|error| write_error(&log_path, error))?;
    }
    // fsync unconditionally: this pair is recovery-critical state, like the
    // sequencer's, so it does not wait for `--sync-data`.
    crate::utils::atomic_write::write_atomic(&log_path, body.as_bytes(), true)
        .map_err(|error| write_error(&log_path, error))?;

    if let Some(parent) = ref_path.parent() {
        fs::create_dir_all(parent).map_err(|error| write_error(&ref_path, error))?;
    }
    let tip = format!("{}\n", entries[0].stash_id);
    crate::utils::atomic_write::write_atomic(&ref_path, tip.as_bytes(), true)
        .map_err(|error| write_error(&ref_path, error))
}

/// Block at the pre-drop rendezvous when the test harness asks.
///
/// `LIBRA_TEST=1` plus `LIBRA_TEST_STASH_DROP_BARRIER=<dir>` — debug builds
/// only, gated on the same `LIBRA_TEST` sentinel as every other failpoint, so
/// a release binary has no path to it. Each arriving process drops a unique
/// marker file into `<dir>` and waits until TWO markers exist (or ten seconds
/// pass, so a partner that failed early cannot hang the suite). A sleep is
/// not a rendezvous: it proves overlap only when the scheduler cooperates,
/// and a test that needs the scheduler's cooperation to fail is not a test.
#[cfg(debug_assertions)]
fn hold_for_drop_rendezvous() -> Result<(), StashError> {
    use std::time::{Duration, Instant};
    if std::env::var_os("LIBRA_TEST").is_none() {
        return Ok(());
    }
    let Some(dir) = std::env::var_os("LIBRA_TEST_STASH_DROP_BARRIER") else {
        return Ok(());
    };
    let dir = std::path::PathBuf::from(dir);
    let _ = fs::create_dir_all(&dir);
    let marker = dir.join(format!("arrived-{}", std::process::id()));
    let _ = fs::write(&marker, b"1");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let arrived = fs::read_dir(&dir).map(Iterator::count).unwrap_or(0);
        if arrived >= 2 {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // A partner that never arrived means the race the test wanted did not
    // happen. Proceeding would let a delayed partner resolve AFTER this drop
    // publishes — two serialized winners, a flaky pass. Failing here turns a
    // scheduler hiccup into an explicit error instead of a wrong verdict.
    Err(StashError::StackLock(
        "test rendezvous timed out waiting for the partner process".to_string(),
    ))
}

#[cfg(not(debug_assertions))]
fn hold_for_drop_rendezvous() -> Result<(), StashError> {
    Ok(())
}

/// Unlink a file so the removal SURVIVES a power loss: the parent directory
/// entry is fsynced, because an unlink that has not reached the disk leaves
/// the file behind exactly as a lost write would.
fn remove_durably(path: &Path) -> Result<(), StashError> {
    // Strict: a swallowed parent-fsync error would report a durable deletion
    // that is not — a power loss could then restore the log after the ref
    // deletion and resurrect a stash the user just cleared.
    crate::utils::atomic_write::remove_durably(path)
        .map_err(|error| StashError::WriteObject(format!("{}: {error}", path.display())))
}

/// Repair a tip left stale by a crash between the two writes of
/// [`publish_stash_stack`] (§C.10 expected-state recovery).
///
/// Call under the stack lock, before reading or mutating the stack. The log is
/// the authority: whatever it says the top entry is, the ref must name — and
/// when the log is gone, no ref may survive it. Returns whether it repaired
/// something, so a caller can report it.
///
/// A repository whose log cannot be read is NOT repaired: an unreadable log is
/// not evidence that the tip is wrong, and deleting a tip on that basis would
/// turn a transient read failure into lost stash entries.
fn reconcile_stash_ref(storage: &Path) -> Result<bool, StashError> {
    let log_path = storage.join("logs/refs/stash");
    let ref_path = storage.join("refs/stash");
    // No-follow guards on BOTH paths before any read or repair: this binary
    // only ever writes regular files here, so a symlink or directory is
    // corruption (or tampering) — following it would read through
    // uncontrolled indirection, and "repairing" it away would destroy the
    // evidence. Fail closed instead; repair is only for states a crash of
    // OUR writer can produce.
    let recorded_tip = match fs::symlink_metadata(&ref_path) {
        Ok(metadata) if metadata.is_file() => match fs::read_to_string(&ref_path) {
            Ok(contents) => Some(contents.trim().to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(StashError::WriteObject(format!(
                    "{}: {error}",
                    ref_path.display()
                )));
            }
        },
        Ok(_) => {
            return Err(StashError::ReadObject(format!(
                "stash ref '{}' is not a regular file",
                ref_path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(StashError::ReadObject(format!(
                "failed to inspect stash ref '{}': {error}",
                ref_path.display()
            )));
        }
    };

    let log_present = match fs::symlink_metadata(&log_path) {
        Ok(metadata) if metadata.is_file() => true,
        Ok(_) => {
            return Err(StashError::ReadObject(format!(
                "stash log '{}' is not a regular file",
                log_path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(StashError::ReadObject(format!(
                "failed to inspect stash log '{}': {error}",
                log_path.display()
            )));
        }
    };
    if !log_present {
        // No stack. A ref that outlived its log names an entry no `pop` can
        // ever find; `stash list` shows nothing while `refs/stash` claims a
        // tip, and a later push would chain onto a line that does not exist.
        if recorded_tip.is_some() {
            remove_durably(&ref_path)?;
            return Ok(true);
        }
        return Ok(false);
    }

    let entries = parse_stash_log_entries(read_stash_log_lines(&log_path)?)?;
    let Some(top) = entries.first() else {
        // An empty log file is an empty stack.
        if recorded_tip.is_some() {
            remove_durably(&ref_path)?;
            return Ok(true);
        }
        return Ok(false);
    };
    let expected = top.stash_id.clone();
    if recorded_tip.as_deref() == Some(expected.as_str()) {
        return Ok(false);
    }
    if let Some(parent) = ref_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| StashError::WriteObject(format!("{}: {error}", ref_path.display())))?;
    }
    crate::utils::atomic_write::write_atomic(&ref_path, format!("{expected}\n").as_bytes(), true)
        .map_err(|error| StashError::WriteObject(format!("{}: {error}", ref_path.display())))?;
    Ok(true)
}

/// [`reconcile_stash_ref`] for a caller that does NOT already hold the stack
/// lock (§C.10).
///
/// The repair is itself a write to `refs/stash`, so running it unlocked could
/// overwrite a tip a concurrent push had just published — the reader would
/// "repair" the stack back to the state it read a moment earlier. Callers
/// inside the lock use the bare form; everyone else comes through here.
fn reconcile_stash_ref_locked(storage: &Path) -> Result<bool, StashError> {
    let _lock = acquire_stash_stack_lock()?;
    reconcile_stash_ref(storage)
}

/// RAII guard over the shared stash-STACK mutation lock (W2 §C.4.3):
/// `refs/stash` + its reflog are repository-shared across worktrees, so every
/// stack mutation (push/store, drop, pop's drop phase, clear, branch's drop)
/// serializes on `stash-stack.lock` in the common storage. Blocking,
/// cross-platform (std `File::lock`), released on drop.
struct StashStackLockGuard {
    file: fs::File,
}

impl Drop for StashStackLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn acquire_stash_stack_lock() -> Result<StashStackLockGuard, StashError> {
    let git_dir =
        util::try_get_storage_path(None).map_err(|e| StashError::StackLock(e.to_string()))?;
    let lock_path = git_dir.join("stash-stack.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| StashError::StackLock(format!("{}: {e}", lock_path.display())))?;
    file.lock()
        .map_err(|e| StashError::StackLock(format!("{}: {e}", lock_path.display())))?;
    Ok(StashStackLockGuard { file })
}

/// The SINGLE shared-stack deletion path (W2 §C.4.3): every stash-entry
/// removal — `drop`, `pop`'s post-apply phase, and `stash branch`'s
/// post-apply phase — goes through here. The whole read→resolve→rewrite runs
/// UNDER the stack lock; when `expected_id` is given (the two post-apply
/// callers) the entry is located BY ID under the lock (indexes shift when a
/// concurrent push lands) and a missing id fails the CAS with
/// [`StashError::StackChanged`] — the caller keeps its successful apply and
/// reports, never rolls back, never deletes a different entry.
fn do_drop(stash: Option<String>, expected_line: Option<&str>) -> Result<StashOutput, StashError> {
    // Test rendezvous (§C.12): hold BEFORE the lock, after the caller has
    // resolved and applied its entry, so two processes provably arrive with
    // the SAME resolved raw line — then race the lock, and exactly one CAS
    // may win. A hold inside the lock serializes the second process's
    // resolve behind the first's publication, which tests nothing.
    hold_for_drop_rendezvous()?;
    let _stack_lock = acquire_stash_stack_lock()?;
    let git_dir = util::request_storage_path();
    // §C.10 recovery: a tip left stale by a crash is repaired FIRST, under the
    // lock this frame just took (the bare form — flock is not reentrant), so
    // both the `has_stash` gate and the CAS below read a consistent pair.
    reconcile_stash_ref(&git_dir)?;
    // A CAS caller APPLIED an entry that was on the stack moments ago — the
    // stack's wholesale disappearance IS a concurrent change, not a user
    // error about an empty stash.
    let missing = |cas: bool| {
        if cas {
            StashError::StackChanged
        } else {
            StashError::NoStashFound
        }
    };
    if !has_stash()? {
        return Err(missing(expected_line.is_some()));
    }

    let stash_log_path = git_dir.join("logs/refs/stash");
    if !stash_log_path.exists() {
        return Err(missing(expected_line.is_some()));
    }

    let mut entries = parse_stash_log_entries(read_stash_log_lines(&stash_log_path)?)?;
    if entries.is_empty() {
        return Err(missing(expected_line.is_some()));
    }

    let index_to_drop = if let Some(expected_line) = expected_line {
        // CAS: locate the applied entry by its RAW REFLOG LINE under the
        // lock. The raw line is the unambiguous per-entry identity — the
        // same commit id can appear more than once on the stack (store /
        // stale-autostash recovery), but each reflog line chains the
        // previous tip, so a line names exactly one entry. No match fails
        // the CAS and keeps the stack untouched.
        match entries
            .iter()
            .position(|entry| entry.raw_line == expected_line)
        {
            Some(index) => index,
            None => return Err(StashError::StackChanged),
        }
    } else {
        let index = match stash {
            None => 0,
            Some(s) => parse_stash_index(&s)?,
        };
        if index >= entries.len() {
            return Err(StashError::StashNotExist(index));
        }
        index
    };
    let removed_entry = entries.remove(index_to_drop);
    let stash_commit_hash = removed_entry.stash_id;

    // ONE crash-safe publication (§C.10): log first and fsynced, then the tip
    // derived from it. The old code wrote the log and then conditionally the
    // ref, so a crash in between left a tip naming a dropped entry.
    publish_stash_stack(&git_dir, &entries)?;

    Ok(StashOutput::Drop {
        index: index_to_drop,
        stash_id: stash_commit_hash,
    })
}

fn parse_stash_index(s: &str) -> Result<usize, StashError> {
    if s.starts_with("stash@{") && s.ends_with('}') {
        s[7..s.len() - 1]
            .parse::<usize>()
            .map_err(|_| StashError::InvalidStashRef(s.to_string()))
    } else {
        Err(StashError::InvalidStashRef(s.to_string()))
    }
}

// ── Unchanged helpers ────────────────────────────────────────────────

async fn has_changes() -> bool {
    let head_tree_hash = match Head::current_commit().await {
        Some(hash) => {
            // Storage-backed load (loose OR packed — a HEAD that arrived via
            // clone/pull lives in a pack). An unreadable commit reports
            // CHANGED (fail-safe): the old silent `false` made `stash push`
            // no-op as "No local changes to save" on a mere read failure.
            let Ok(commit) = load_object::<Commit>(&hash) else {
                return true;
            };
            commit.tree_id
        }
        None => ObjectHash::from_type_and_data(ObjectType::Tree, &[]),
    };

    let index_path = path::index();
    let Ok(index) = Index::load(&index_path) else {
        return false;
    };
    let Ok(index_tree) = tree::create_tree_from_index(&index) else {
        return false;
    };
    let index_tree_hash = index_tree.id;

    if head_tree_hash != index_tree_hash {
        return true;
    }

    let workdir = util::request_working_dir();
    for entry in index.tracked_entries(0) {
        let file_path = workdir.join(&entry.name);

        let Ok(metadata) = fs::metadata(&file_path) else {
            return true;
        };

        let mtime =
            Time::from_system_time(metadata.modified().unwrap_or(std::time::SystemTime::now()));
        if metadata.len() == entry.size as u64 && mtime == entry.mtime {
            continue;
        }

        if let Ok(content) = fs::read(&file_path) {
            let header = format!("blob {}\0", content.len());
            let mut full_content = header.into_bytes();
            full_content.extend_from_slice(&content);
            let current_hash = ObjectHash::new(&full_content);

            if current_hash != entry.hash {
                return true;
            }
        } else {
            return true;
        }
    }

    false
}

fn has_stash() -> Result<bool, StashError> {
    // §C.4.2: the repository this INVOCATION acts on, like every other stash
    // path. Deliberately BARE — no lock, no repair: this predicate runs both
    // under the stack lock (`do_drop`, `run_clear`) and outside it, and flock
    // is not reentrant, so taking the lock here deadlocks the locked callers.
    // Every ENTRY POINT reconciles first (locked outside the lock, bare
    // inside), so by the time this reads the ref it has been repaired.
    let storage = util::request_storage_path();
    let stash_ref = storage.join("refs/stash");
    match fs::symlink_metadata(&stash_ref) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(StashError::ReadObject(format!(
            "stash ref '{}' is not a regular file",
            stash_ref.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(StashError::ReadObject(format!(
            "failed to inspect stash ref '{}': {error}",
            stash_ref.display()
        ))),
    }
}

fn empty_tree() -> Result<Tree, String> {
    let empty_id = ObjectHash::from_type_and_data(ObjectType::Tree, &[]);
    Tree::from_bytes(&[], empty_id).map_err(|e| e.to_string())
}

fn read_stash_log_lines(stash_log_path: &Path) -> Result<Vec<String>, StashError> {
    let file = std::fs::File::open(stash_log_path).map_err(|e| {
        StashError::ReadObject(format!(
            "failed to open stash log '{}': {}",
            stash_log_path.display(),
            e
        ))
    })?;
    let reader = BufReader::new(file);
    reader.lines().collect::<Result<Vec<_>, _>>().map_err(|e| {
        StashError::ReadObject(format!(
            "failed to read stash log '{}': {}",
            stash_log_path.display(),
            e
        ))
    })
}

#[derive(Debug, Clone)]
struct StashLogEntry {
    raw_line: String,
    stash_id: String,
    message: String,
}

/// Whether a trailing tab-separated field is a generation column THIS writer
/// minted: `gen=` followed by exactly 32 lowercase hex digits (a simple
/// uuid7). Anything else — including a user message that legitimately
/// contains `\tgen=...` — is message content and must never be stripped.
fn is_generation_column(field: &str) -> bool {
    field.strip_prefix("gen=").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}

/// Mint a fresh generation column.
fn generation_column() -> String {
    format!("gen={}", uuid::Uuid::now_v7().simple())
}

/// A raw line WITH a generation: the line itself when it already carries one,
/// or the line with a fresh generation appended. Publication runs every line
/// through this, so a stack written by an older binary becomes ABA-proof on
/// its first mutation — and any raw-line handle a concurrent holder captured
/// BEFORE the upgrade then misses its CAS, which fails in the safe direction
/// (the entry is kept and the holder is told the stack changed).
fn with_generation(raw_line: &str) -> String {
    let has_generation = raw_line
        .rsplit_once('\t')
        .is_some_and(|(_, field)| is_generation_column(field));
    if has_generation {
        raw_line.to_string()
    } else {
        format!("{raw_line}\t{}", generation_column())
    }
}

fn parse_stash_log_entries(lines: Vec<String>) -> Result<Vec<StashLogEntry>, StashError> {
    let mut entries = Vec::new();

    for (line_index, line) in lines.into_iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let stash_id = line.split_whitespace().nth(1).ok_or_else(|| {
            StashError::ReadObject(format!(
                "corrupted stash log entry at line {}: missing stash commit hash",
                line_index + 1
            ))
        })?;
        let stash_id = ObjectHash::from_str(stash_id).map_err(|_| {
            StashError::ReadObject(format!(
                "corrupted stash log entry at line {}: invalid stash commit hash '{}'",
                line_index + 1,
                stash_id
            ))
        })?;
        let message = line
            .split_once('\t')
            .map(|(_, rest)| match rest.rsplit_once('\t') {
                // The trailing generation column is entry identity, not
                // message — but only the EXACT shape this writer mints
                // (`gen=` + 32 lowercase hex) is stripped. A legacy `-m`
                // message that happens to contain a tab and a `gen=` prefix
                // keeps every byte it always had.
                Some((message, generation)) if is_generation_column(generation) => {
                    message.to_string()
                }
                _ => rest.to_string(),
            })
            .unwrap_or_default();

        entries.push(StashLogEntry {
            raw_line: line,
            stash_id: stash_id.to_string(),
            message,
        });
    }

    Ok(entries)
}

/// Return every file-backed stash object that repository maintenance must
/// trace. Older stash entries live only in the stash reflog; tracing just the
/// refs/stash tip would let GC delete stash@{1} and later entries.
/// §C.4.2: takes the caller's BOUND storage root instead of resolving one —
/// the GC collection binds every file-backed source to a single pinned root,
/// and an ambient re-resolution here could hand it another repository's
/// stash stack after an in-process cwd move.
pub(crate) fn gc_roots(storage: &Path) -> Result<Vec<ObjectHash>, StashError> {
    let mut roots = HashSet::new();
    let stash_ref_path = storage.join("refs/stash");
    match fs::read_to_string(&stash_ref_path) {
        Ok(raw_oid) => {
            let oid = ObjectHash::from_str(raw_oid.trim()).map_err(|error| {
                StashError::ReadObject(format!(
                    "stash ref '{}' contains an invalid object id: {error}",
                    stash_ref_path.display()
                ))
            })?;
            roots.insert(oid);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(StashError::ReadObject(format!(
                "failed to read stash ref '{}': {error}",
                stash_ref_path.display()
            )));
        }
    }

    let stash_log_path = storage.join("logs/refs/stash");
    match fs::symlink_metadata(&stash_log_path) {
        Ok(metadata) if metadata.is_file() => {
            let entries = parse_stash_log_entries(read_stash_log_lines(&stash_log_path)?)?;
            for entry in entries {
                let oid = ObjectHash::from_str(&entry.stash_id).map_err(|error| {
                    StashError::ReadObject(format!(
                        "stash log '{}' contains an invalid object id '{}': {error}",
                        stash_log_path.display(),
                        entry.stash_id
                    ))
                })?;
                roots.insert(oid);
            }
        }
        Ok(_) => {
            return Err(StashError::ReadObject(format!(
                "stash log '{}' is not a regular file",
                stash_log_path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(StashError::ReadObject(format!(
                "failed to inspect stash log '{}': {error}",
                stash_log_path.display()
            )));
        }
    }

    let mut roots = roots.into_iter().collect::<Vec<_>>();
    roots.sort_by_key(ToString::to_string);
    Ok(roots)
}

fn resolve_stash_to_commit_hash(
    stash_ref: Option<String>,
) -> Result<(usize, String, String), StashError> {
    // §C.10: repair a stale tip before resolving against the stack. This
    // entry point never runs under the stack lock, so the locked form is
    // safe here.
    reconcile_stash_ref_locked(&util::request_storage_path())?;
    if !has_stash()? {
        return Err(StashError::NoStashFound);
    }

    let git_dir = util::request_storage_path();
    let stash_log_path = git_dir.join("logs/refs/stash");
    if !stash_log_path.exists() {
        return Err(StashError::NoStashFound);
    }

    let entries = parse_stash_log_entries(read_stash_log_lines(&stash_log_path)?)?;

    let index_to_resolve = match stash_ref {
        None => 0,
        Some(s) => parse_stash_index(&s)?,
    };

    if index_to_resolve >= entries.len() {
        return Err(StashError::StashNotExist(index_to_resolve));
    }

    Ok((
        index_to_resolve,
        entries[index_to_resolve].stash_id.clone(),
        entries[index_to_resolve].raw_line.clone(),
    ))
}

/// Locked wrapper for the push/store side: the read-old-tip → write-tip →
/// append-reflog sequence must not interleave with another worktree's stack
/// mutation (W2 §C.4.3).
fn update_stash_ref_locked(
    git_dir: &Path,
    stash_hash: &ObjectHash,
    committer: &Signature,
    message: &str,
) -> Result<String, StashError> {
    let _stack_lock = acquire_stash_stack_lock()?;
    // §C.10: repair a stale tip BEFORE reading it as this entry's parent. A
    // crash could have left the tip naming `B` while the log is headed by `A`;
    // chaining the new entry onto `B` would record a reflog line whose parent
    // no line in the log produces, and the CAS identity of every later entry
    // would be built on it.
    reconcile_stash_ref(git_dir)?;
    update_stash_ref(git_dir, stash_hash, committer, message)
        .map_err(|e| StashError::WriteObject(e.to_string()))
}

/// Returns the exact reflog line appended (the entry's unambiguous identity
/// for a later raw-line CAS delete — see `do_drop`).
fn update_stash_ref(
    git_dir: &Path,
    stash_hash: &ObjectHash,
    committer: &Signature,
    message: &str,
) -> Result<String, GitError> {
    let stash_ref_path = git_dir.join("refs/stash");
    let stash_log_path = git_dir.join("logs/refs/stash");

    let old_hash = if stash_ref_path.exists() {
        let content = fs::read_to_string(&stash_ref_path)?;
        ObjectHash::from_str(content.trim())
            .map_err(|_| GitError::InvalidHashValue(content.trim().to_string()))?
    } else {
        ObjectHash::default()
    };

    // A unique GENERATION as a third tab-separated column (§C.10). The raw
    // line is every CAS's entry identity, and without this it is reusable: a
    // drop-and-repush of the same commit onto the same parent within the same
    // second reproduces the line byte for byte, and a delayed CAS then
    // deletes the NEW entry (ABA). The generation makes every line minted
    // distinct; readers that split on the first tab still see the message,
    // and old lines without one keep working.
    let reflog_entry = format!(
        "{} {} {} <{}> {} {}\t{}\t{}",
        old_hash,
        stash_hash,
        committer.name,
        committer.email,
        committer.timestamp,
        committer.timezone,
        message,
        generation_column()
    );

    let mut lines = if stash_log_path.exists() {
        let content = fs::read_to_string(&stash_log_path)?;
        content.lines().map(String::from).collect()
    } else {
        Vec::new()
    };
    lines.insert(0, reflog_entry.clone());

    // ONE crash-safe publication (§C.10): the LOG is written first, atomically
    // and fsynced, and the tip is derived from it. Writing the ref first — as
    // this did — meant a crash before the log left `refs/stash` naming an
    // entry `stash list` could not see and `pop` could not find.
    let entries = parse_stash_log_entries(lines.clone())
        .map_err(|error| GitError::CustomError(error.to_string()))?;
    publish_stash_stack(git_dir, &entries)
        .map_err(|error| GitError::CustomError(error.to_string()))?;

    Ok(reflog_entry)
}

async fn perform_hard_reset(target_commit_id: &ObjectHash) -> Result<(), String> {
    let workdir = &util::request_working_dir();
    let index_path = path::index();

    let index_before_reset = Index::load(&index_path).unwrap_or_else(|_| Index::new());
    let all_tracked_paths: Vec<PathBuf> = index_before_reset
        .tracked_entries(0)
        .into_iter()
        .map(|e| PathBuf::from(&e.name))
        .collect();

    let target_commit: Commit =
        load_object(target_commit_id).map_err(|e| format!("failed to load target commit: {e}"))?;
    let target_tree: Tree = load_object(&target_commit.tree_id)
        .map_err(|e| format!("failed to load target tree: {e}"))?;
    let files_in_target_tree: HashSet<PathBuf> = target_tree
        .get_plain_items()
        .into_iter()
        .map(|(p, _)| p)
        .collect();

    reset_index_to_commit(target_commit_id)?;

    for path in &all_tracked_paths {
        if !files_in_target_tree.contains(path) {
            let full_path = workdir.join(path);
            if full_path.exists() {
                fs::remove_file(full_path).map_err(|e| format!("failed to remove file: {e}"))?;
            }
        }
    }

    restore_working_directory_from_tree(&target_tree, workdir, "")?;
    remove_empty_directories(workdir)?;

    Ok(())
}

fn create_tree_from_workdir(workdir: &Path, git_dir: &Path, index: &Index) -> Result<Tree, String> {
    fn build_tree_recursive(
        dir: &Path,
        git_dir: &Path,
        index: &Index,
        workdir: &Path,
    ) -> Result<Tree, String> {
        let mut items = Vec::new();
        let entries = fs::read_dir(dir).map_err(|e| e.to_string())?;

        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let file_name = path
                .file_name()
                .ok_or_else(|| format!("entry has no file name: {}", path.display()))?
                .to_str()
                .ok_or_else(|| format!("invalid path encoding: {}", path.display()))?
                .to_string();

            // Skip only Libra's metadata directory. User-managed dotfiles such
            // as `.gitignore`, `.env`, or `.config/*` must remain stashed.
            if path == git_dir {
                continue;
            }

            if path.is_dir() {
                let subtree = build_tree_recursive(&path, git_dir, index, workdir)?;
                // Skip empty subtrees to avoid Tree serialisation errors
                if subtree.tree_items.is_empty() {
                    continue;
                }
                let subtree_data = subtree.to_data().map_err(|e| e.to_string())?;
                let subtree_hash = object::write_git_object(git_dir, "tree", &subtree_data)
                    .map_err(|e| e.to_string())?;
                items.push(TreeItem::new(TreeItemMode::Tree, subtree_hash, file_name));
            } else if path.is_file() {
                let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
                let relative_path = path.strip_prefix(workdir).map_err(|e| {
                    format!(
                        "failed to strip workdir prefix from {}: {e}",
                        path.display()
                    )
                })?;
                let relative_path_str = relative_path
                    .to_str()
                    .ok_or_else(|| format!("invalid path encoding: {}", relative_path.display()))?;

                // Skip files that are not tracked in the index. Untracked files
                // are only captured when `-u`/`--include-untracked` requests it,
                // via the stash's dedicated third (untracked) parent commit.
                let Some(entry) = index.get(relative_path_str, 0) else {
                    continue;
                };

                let mtime = Time::from_system_time(
                    metadata.modified().unwrap_or(std::time::SystemTime::now()),
                );
                let size = metadata.len() as u32;

                if entry.mtime == mtime && entry.size == size {
                    let mode = tree_item_mode_from_metadata(&metadata);
                    items.push(TreeItem::new(mode, entry.hash, file_name));
                    continue;
                }

                let content = fs::read(&path).map_err(|e| e.to_string())?;
                let blob_hash = object::write_git_object(git_dir, "blob", &content)
                    .map_err(|e| e.to_string())?;
                let mode = tree_item_mode_from_metadata(&metadata);
                items.push(TreeItem::new(mode, blob_hash, file_name));
            }
        }

        items.sort_by(|a, b| a.name.cmp(&b.name));
        if items.is_empty() {
            empty_tree()
        } else {
            Tree::from_tree_items(items).map_err(|e| e.to_string())
        }
    }

    build_tree_recursive(workdir, git_dir, index, workdir)
}

fn build_tree_from_flat_items(
    files: &HashMap<String, TreeItem>,
    git_dir: &Path,
) -> Result<Tree, String> {
    #[derive(Default)]
    struct DirectoryEntries {
        files: Vec<TreeItem>,
        subdirs: HashSet<String>,
    }

    fn build_dir(
        current_dir: &Path,
        directories: &mut HashMap<PathBuf, DirectoryEntries>,
        git_dir: &Path,
    ) -> Result<Tree, String> {
        let mut directory = directories.remove(current_dir).unwrap_or_default();
        let mut subdirs: Vec<String> = directory.subdirs.into_iter().collect();
        subdirs.sort();

        for subdir_name in subdirs {
            let subdir_path = if current_dir.as_os_str().is_empty() {
                PathBuf::from(&subdir_name)
            } else {
                current_dir.join(&subdir_name)
            };
            let subtree = build_dir(&subdir_path, directories, git_dir)?;
            if subtree.tree_items.is_empty() {
                continue;
            }
            let subtree_data = subtree.to_data().map_err(|e| e.to_string())?;
            let subtree_hash = object::write_git_object(git_dir, "tree", &subtree_data)
                .map_err(|e| e.to_string())?;
            directory
                .files
                .push(TreeItem::new(TreeItemMode::Tree, subtree_hash, subdir_name));
        }

        directory.files.sort_by(|a, b| a.name.cmp(&b.name));
        if directory.files.is_empty() {
            empty_tree()
        } else {
            Tree::from_tree_items(directory.files).map_err(|e| e.to_string())
        }
    }

    let mut directories: HashMap<PathBuf, DirectoryEntries> = HashMap::new();
    directories.entry(PathBuf::new()).or_default();

    for (path_str, item) in files {
        let path_buf = PathBuf::from(path_str);
        let file_name = path_buf
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("invalid merged stash path: {}", path_buf.display()))?
            .to_string();
        let parent_dir = path_buf
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();

        let mut tree_item = item.clone();
        tree_item.name = file_name;
        directories
            .entry(parent_dir.clone())
            .or_default()
            .files
            .push(tree_item);

        let mut current_dir = PathBuf::new();
        for component in parent_dir.components() {
            let subdir_name = component
                .as_os_str()
                .to_str()
                .ok_or_else(|| format!("invalid merged stash path: {}", path_buf.display()))?
                .to_string();
            if subdir_name.is_empty() {
                continue;
            }
            directories
                .entry(current_dir.clone())
                .or_default()
                .subdirs
                .insert(subdir_name.clone());
            current_dir.push(&subdir_name);
            directories.entry(current_dir.clone()).or_default();
        }
    }

    build_dir(Path::new(""), &mut directories, git_dir)
}

/// Performs a three-way merge of tree objects.
/// This is a simplified implementation that prefers the stash version in case of conflicts.
fn merge_trees(base: &Tree, head: &Tree, stash: &Tree, git_dir: &Path) -> Result<Tree, String> {
    let base_items = tree::get_tree_files_recursive(base, git_dir, &PathBuf::new())?;
    let mut head_items = tree::get_tree_files_recursive(head, git_dir, &PathBuf::new())?;
    let stash_items = tree::get_tree_files_recursive(stash, git_dir, &PathBuf::new())?;
    let mut conflicts = Vec::new();

    // Two tree entries are equal only when BOTH content and mode match, so a
    // mode-only change (e.g. the executable bit) still counts as a real change.
    let same = |a: &TreeItem, b: &TreeItem| a.id == b.id && a.mode == b.mode;

    // Replay only paths changed by the stash snapshot. If the working tree
    // (`head`) diverged from the stash base in a different way, stop instead of
    // overwriting newer work.
    for (path, stash_item) in stash_items.iter() {
        let base_item = base_items.get(path);
        let head_item = head_items.get(path);

        match (base_item, head_item) {
            (Some(b), Some(h)) => {
                if !same(b, h) && !same(b, stash_item) && !same(h, stash_item) {
                    conflicts.push(path.clone());
                    continue;
                }
                // Stash version differs from base: apply the stash change.
                if !same(b, stash_item) {
                    head_items.insert(path.clone(), stash_item.clone());
                }
            }
            (Some(b), None) => {
                // The path was deleted in the working tree. Keep the deletion if
                // the stash left it unchanged; conflict if the stash changed it
                // (a delete/modify clash). Never resurrect an unrelated file.
                if !same(b, stash_item) {
                    conflicts.push(path.clone());
                }
            }
            (None, Some(h)) => {
                // Added relative to base on both sides: take the stash's version
                // when they agree, otherwise it is an add/add conflict.
                if !same(h, stash_item) {
                    conflicts.push(path.clone());
                }
            }
            (None, None) => {
                // A pure stash addition (absent from base and the working tree).
                head_items.insert(path.clone(), stash_item.clone());
            }
        }
    }

    for (path, base_item) in base_items.iter() {
        if !stash_items.contains_key(path) {
            if let Some(head_item) = head_items.get(path)
                && !same(head_item, base_item)
            {
                conflicts.push(path.clone());
                continue;
            }
            head_items.remove(path);
        }
    }

    if !conflicts.is_empty() {
        let error_message = format!(
            "Your local changes to the following files would be overwritten by merge:\n  {}\n\
             Please commit your changes or stash them before you merge.",
            conflicts.join("\n  ")
        );
        return Err(error_message);
    }

    build_tree_from_flat_items(&head_items, git_dir)
}

/// Get the number of stashes
pub(crate) fn get_stash_num() -> Result<usize, String> {
    // §C.10: unlocked entry point — repair before counting.
    reconcile_stash_ref_locked(&util::request_storage_path()).map_err(|error| error.to_string())?;
    if !has_stash().map_err(|error| error.to_string())? {
        return Ok(0);
    }

    let git_dir = util::try_get_storage_path(None).map_err(|e| e.to_string())?;
    let stash_log_path = git_dir.join("logs/refs/stash");
    if !stash_log_path.try_exists().map_err(|error| {
        format!(
            "failed to inspect stash log '{}': {error}",
            stash_log_path.display()
        )
    })? {
        return Ok(0);
    }
    let count =
        parse_stash_log_entries(read_stash_log_lines(&stash_log_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?
            .len();

    Ok(count)
}

// ── `stash push -u` / `--keep-index` helpers ──────────────────────────

/// Collects the worktree-relative paths of untracked files that should be
/// captured in the stash's third parent commit. Returns an empty vector when
/// `-u`/`--include-untracked` was not requested. `--all` additionally folds in
/// ignored files. Libra's own metadata directory is always excluded.
fn collect_included_untracked_paths(
    options: &StashPushOptions,
) -> Result<Vec<PathBuf>, StashError> {
    if !options.include_untracked {
        return Ok(Vec::new());
    }

    let (mut visible, ignored) = if options.include_ignored {
        status::changes_to_be_staged_split_force()
    } else {
        status::changes_to_be_staged_split_safe()
    }
    .map_err(|error| {
        StashError::ReadObject(format!(
            "failed to inspect working tree for untracked files: {error}"
        ))
    })?;

    if options.include_ignored {
        visible.new.extend(ignored.new);
    }
    visible.new.retain(|path| !is_internal_untracked_path(path));
    visible.new.sort();
    visible.new.dedup();
    Ok(visible.new)
}

fn is_internal_untracked_path(path: &Path) -> bool {
    let Some(Component::Normal(first)) = path.components().next() else {
        return false;
    };
    let Some(first) = first.to_str() else {
        return false;
    };

    first == util::ROOT_DIR || first == ".git" || first == ".libra-test-home"
}

/// Writes a parentless commit whose tree captures the included untracked files.
/// The resulting commit becomes the stash commit's third parent, mirroring
/// Git's `stash` layout for `-u`/`--include-untracked`.
fn create_untracked_parent_commit(
    workdir: &Path,
    git_dir: &Path,
    paths: &[PathBuf],
    author: &Signature,
    committer: &Signature,
    message: &str,
) -> Result<ObjectHash, StashError> {
    let untracked_tree =
        create_tree_from_paths(workdir, git_dir, paths).map_err(StashError::WriteObject)?;
    let untracked_tree_data = untracked_tree
        .to_data()
        .map_err(|error| StashError::WriteObject(error.to_string()))?;
    let untracked_tree_hash = object::write_git_object(git_dir, "tree", &untracked_tree_data)
        .map_err(|error| StashError::WriteObject(error.to_string()))?;
    let untracked_commit = Commit::new(
        author.clone(),
        committer.clone(),
        untracked_tree_hash,
        Vec::new(),
        message,
    );
    let untracked_commit_data = untracked_commit
        .to_data()
        .map_err(|error| StashError::WriteObject(error.to_string()))?;
    object::write_git_object(git_dir, "commit", &untracked_commit_data)
        .map_err(|error| StashError::WriteObject(error.to_string()))
}

fn create_tree_from_paths(
    workdir: &Path,
    git_dir: &Path,
    paths: &[PathBuf],
) -> Result<Tree, String> {
    let mut files = HashMap::new();
    for relative_path in paths {
        let full_path = workdir.join(relative_path);
        if !full_path.is_file() {
            return Err(format!(
                "included untracked path is not a file: {}",
                relative_path.display()
            ));
        }
        let path_str = worktree_relative_path_to_string(relative_path)?;
        let metadata = fs::metadata(&full_path).map_err(|error| error.to_string())?;
        let content = fs::read(&full_path).map_err(|error| error.to_string())?;
        let blob_hash = object::write_git_object(git_dir, "blob", &content)
            .map_err(|error| error.to_string())?;
        let mode = tree_item_mode_from_metadata(&metadata);
        files.insert(path_str.clone(), TreeItem::new(mode, blob_hash, path_str));
    }

    build_tree_from_flat_items(&files, git_dir)
}

fn worktree_relative_path_to_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(ToString::to_string)
        .ok_or_else(|| format!("invalid path encoding: {}", path.display()))
}

fn tree_item_mode_from_metadata(metadata: &fs::Metadata) -> TreeItemMode {
    #[cfg(unix)]
    {
        if metadata.permissions().mode() & 0o111 != 0 {
            TreeItemMode::BlobExecutable
        } else {
            TreeItemMode::Blob
        }
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        TreeItemMode::Blob
    }
}

/// Restores the working tree to the staged index state after a `--keep-index`
/// push. Files present at HEAD but absent from the index are removed, then the
/// index tree is materialised on disk so staged content survives the stash.
fn restore_worktree_to_index(
    index: &Index,
    head_commit_hash: &ObjectHash,
    workdir: &Path,
    git_dir: &Path,
) -> Result<(), String> {
    let target_commit: Commit = load_object(head_commit_hash)
        .map_err(|error| format!("failed to load target commit: {error}"))?;
    let target_tree: Tree = load_object(&target_commit.tree_id)
        .map_err(|error| format!("failed to load target tree: {error}"))?;
    let head_files = tree::get_tree_files_recursive(&target_tree, git_dir, &PathBuf::new())?;

    for path in head_files.keys() {
        if index.get(path, 0).is_none() {
            let full_path = workdir.join(path);
            if full_path.exists() {
                fs::remove_file(&full_path).map_err(|error| {
                    format!("failed to remove file {}: {error}", full_path.display())
                })?;
            }
        }
    }

    let index_tree = tree::create_tree_from_index(index).map_err(|error| error.to_string())?;
    restore_working_directory_from_tree(&index_tree, workdir, "")?;
    remove_empty_directories(workdir)?;
    Ok(())
}

/// Removes the untracked files that were captured into the stash so the working
/// tree is left clean, trimming any directories that become empty as a result.
fn remove_included_untracked_paths(workdir: &Path, paths: &[PathBuf]) -> Result<(), String> {
    let mut sorted_paths = paths.to_vec();
    sorted_paths.sort_by_key(|path| Reverse(path.components().count()));

    for relative_path in &sorted_paths {
        let full_path = workdir.join(relative_path);
        if full_path.is_dir() {
            fs::remove_dir_all(&full_path).map_err(|error| {
                format!(
                    "failed to remove directory {}: {error}",
                    full_path.display()
                )
            })?;
        } else if full_path.exists() {
            fs::remove_file(&full_path).map_err(|error| {
                format!("failed to remove file {}: {error}", full_path.display())
            })?;
        }
        remove_empty_parent_dirs(workdir, relative_path)?;
    }

    Ok(())
}

fn remove_empty_parent_dirs(workdir: &Path, relative_path: &Path) -> Result<(), String> {
    let Some(parent) = relative_path.parent() else {
        return Ok(());
    };
    let mut current = workdir.join(parent);
    while current != workdir && current.starts_with(workdir) {
        if current.file_name().and_then(|name| name.to_str()) == Some(util::ROOT_DIR) {
            break;
        }
        match fs::remove_dir(&current) {
            Ok(()) => {
                let Some(next) = current.parent() else {
                    break;
                };
                current = next.to_path_buf();
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(next) = current.parent() else {
                    break;
                };
                current = next.to_path_buf();
            }
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
            Err(error) => {
                return Err(format!(
                    "failed to remove empty directory {}: {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W2 §C.4.3: the raw-line CAS branch of the unified `do_drop` — a
    /// caller that applied an entry may only delete THAT entry (identified
    /// by its raw reflog line, which stays unambiguous even when the SAME
    /// commit id appears twice on the stack); a missing line fails the CAS
    /// with `StackChanged` and leaves the stack byte-for-byte untouched,
    /// and a stack that vanished entirely maps to `StackChanged` too.
    #[tokio::test]
    #[serial_test::serial]
    async fn do_drop_cas_misses_leave_the_stack_untouched() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = crate::utils::test::ChangeDirGuard::new(tmp.path());
        crate::utils::test::setup_with_new_libra_in(tmp.path()).await;
        let storage = util::storage_path();

        // Craft a three-entry stack whose two OLDER entries share ONE id
        // (duplicate OIDs are legitimate: store / stale-autostash recovery).
        let id_top = "1111111111111111111111111111111111111111";
        let id_dup = "2222222222222222222222222222222222222222";
        let zero = "0000000000000000000000000000000000000000";
        fs::create_dir_all(storage.join("refs")).unwrap();
        fs::create_dir_all(storage.join("logs/refs")).unwrap();
        fs::write(storage.join("refs/stash"), format!("{id_top}\n")).unwrap();
        let line_top = format!("{zero} {id_top} t <t@x> 3 +0000\tWIP on main: top");
        let line_dup_new = format!("{zero} {id_dup} t <t@x> 2 +0000\tWIP on main: dup-new");
        let line_dup_old = format!("{zero} {id_dup} t <t@x> 1 +0000\tWIP on main: dup-old");
        let log = format!("{line_top}\n{line_dup_new}\n{line_dup_old}\n");
        let log_path = storage.join("logs/refs/stash");
        fs::write(&log_path, &log).unwrap();
        let ref_bytes = fs::read(storage.join("refs/stash")).unwrap();
        let log_bytes = fs::read(&log_path).unwrap();

        // CAS miss: a line not on the stack → StackChanged, nothing touched.
        let missing_line = format!("{zero} {id_top} t <t@x> 9 +0000\tWIP on main: elsewhere");
        let err = do_drop(None, Some(&missing_line)).expect_err("missing line must fail the CAS");
        assert!(matches!(err, StashError::StackChanged), "{err:?}");
        assert_eq!(fs::read(storage.join("refs/stash")).unwrap(), ref_bytes);
        assert_eq!(fs::read(&log_path).unwrap(), log_bytes);

        // CAS hit on the OLDER duplicate: exactly that line leaves; the
        // NEWER duplicate with the same id and the tip both stay.
        let out = do_drop(None, Some(&line_dup_old)).expect("existing line drops");
        match out {
            StashOutput::Drop { stash_id, .. } => assert_eq!(stash_id, id_dup),
            other => panic!("unexpected output: {other:?}"),
        }
        assert_eq!(
            fs::read(storage.join("refs/stash")).unwrap(),
            ref_bytes,
            "dropping a non-top entry leaves the tip"
        );
        let remaining = fs::read_to_string(&log_path).unwrap();
        assert!(
            remaining.contains(&line_top)
                && remaining.contains(&line_dup_new)
                && !remaining.contains(&line_dup_old),
            "exactly the older duplicate left: {remaining}"
        );

        // Stack vanished entirely between apply and CAS → StackChanged (not
        // a misleading "no stash found").
        fs::remove_file(&log_path).unwrap();
        fs::remove_file(storage.join("refs/stash")).unwrap();
        let err = do_drop(None, Some(&line_top)).expect_err("vanished stack fails the CAS");
        assert!(matches!(err, StashError::StackChanged), "{err:?}");
        // ...while a plain drop still reports the ordinary empty-stash error.
        let err = do_drop(None, None).expect_err("plain drop on empty stack");
        assert!(matches!(err, StashError::NoStashFound), "{err:?}");
    }

    /// §C.10: only the EXACT generation shape is stripped from messages — a
    /// legacy `-m` message that happens to contain `\tgen=...` keeps every
    /// byte, while a real generation column never leaks into display.
    #[test]
    fn generation_stripping_never_eats_a_legacy_message() {
        let zero = "0000000000000000000000000000000000000000";
        let id = "1111111111111111111111111111111111111111";
        let legacy = format!("{zero} {id} t <t@x> 1 +0000\tnote\tgen=keep-this");
        let entries = parse_stash_log_entries(vec![legacy.clone()]).expect("parse legacy");
        assert_eq!(
            entries[0].message, "note\tgen=keep-this",
            "a message that merely LOOKS like a generation keeps every byte"
        );

        let minted = format!("{zero} {id} t <t@x> 1 +0000\tnote\t{}", generation_column());
        let entries = parse_stash_log_entries(vec![minted]).expect("parse minted");
        assert_eq!(
            entries[0].message, "note",
            "a real generation column never leaks into the message"
        );

        // The shape predicate itself: exactly gen= + 32 lowercase hex.
        assert!(is_generation_column(&generation_column()));
        for not_a_generation in [
            "gen=keep-this",
            "gen=ABCDEF00112233445566778899AABBCC",
            "gen=0123456789abcdef",
            "generation=0123456789abcdef0123456789abcdef",
        ] {
            assert!(
                !is_generation_column(not_a_generation),
                "{not_a_generation:?} must not be stripped"
            );
        }
    }

    /// §C.10: the first locked publication BACKFILLS generations onto lines
    /// an older binary wrote — after it, a raw-line handle captured before
    /// the upgrade misses its CAS (the safe direction), and every line on
    /// disk is ABA-proof.
    #[tokio::test]
    #[serial_test::serial]
    async fn publication_backfills_generations_onto_legacy_lines() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = crate::utils::test::ChangeDirGuard::new(tmp.path());
        crate::utils::test::setup_with_new_libra_in(tmp.path()).await;
        let storage = util::storage_path();

        // A two-entry stack exactly as an old binary would have written it.
        let zero = "0000000000000000000000000000000000000000";
        let id_top = "1111111111111111111111111111111111111111";
        let id_old = "2222222222222222222222222222222222222222";
        fs::create_dir_all(storage.join("refs")).unwrap();
        fs::create_dir_all(storage.join("logs/refs")).unwrap();
        fs::write(storage.join("refs/stash"), format!("{id_top}\n")).unwrap();
        let legacy_top = format!("{zero} {id_top} t <t@x> 2 +0000\tWIP top");
        let legacy_old = format!("{zero} {id_old} t <t@x> 1 +0000\tWIP old");
        fs::write(
            storage.join("logs/refs/stash"),
            format!("{legacy_top}\n{legacy_old}\n"),
        )
        .unwrap();

        // First locked mutation: drop the TOP entry via its legacy line.
        do_drop(None, Some(&legacy_top)).expect("the pre-upgrade line still CASes once");

        // The surviving line was rewritten WITH a generation…
        let entries = stack_entries().expect("stack entries");
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0]
                .raw_line
                .rsplit_once('\t')
                .is_some_and(|(_, field)| is_generation_column(field)),
            "the survivor was backfilled: {}",
            entries[0].raw_line
        );
        assert_eq!(entries[0].message, "WIP old");

        // …so the STALE legacy handle for it now misses, in the safe
        // direction.
        let err = do_drop(None, Some(&legacy_old)).expect_err("the stale handle misses");
        assert!(matches!(err, StashError::StackChanged), "{err:?}");
        assert_eq!(
            stack_entries().expect("entries").len(),
            1,
            "nothing deleted"
        );
    }

    /// §C.10 ABA: a drop-and-repush that reproduces every VISIBLE field of a
    /// reflog line must not satisfy a CAS taken against the original entry.
    ///
    /// The line's visible fields — parent OID, stash OID, identity,
    /// second-resolution timestamp, message — are all reusable within one
    /// second, which is exactly what "drop the held autostash, then
    /// re-promote the same commit onto the same parent" produces. The minted
    /// GENERATION column is what makes each line non-reusable: the delayed
    /// CAS misses the reincarnation and the new entry survives.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_reused_visible_line_does_not_satisfy_the_cas() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = crate::utils::test::ChangeDirGuard::new(tmp.path());
        crate::utils::test::setup_with_new_libra_in(tmp.path()).await;
        let storage = util::storage_path();
        let hash = ObjectHash::from_str("00000000000000000000000000000000000000cc").expect("hash");
        let committer = Signature::from_data(
            "committer T <t@x> 1700000000 +0000"
                .to_string()
                .into_bytes(),
        )
        .expect("signature");

        // The original entry, as a pop would resolve it.
        let original =
            update_stash_ref(&storage, &hash, &committer, "WIP").expect("push the original");

        // Another actor drops it and re-pushes the SAME commit with the SAME
        // identity, message and timestamp — every visible field reproduced.
        do_drop(None, Some(&original)).expect("the other actor drops it");
        let reincarnation =
            update_stash_ref(&storage, &hash, &committer, "WIP").expect("repush the same commit");
        assert_ne!(
            original, reincarnation,
            "the minted generation makes the reincarnated line distinct"
        );

        // The DELAYED pop's CAS, still holding the original line: it must
        // MISS — dropping the reincarnation would delete an entry its owner
        // intended to keep.
        let err = do_drop(None, Some(&original)).expect_err("the stale CAS misses");
        assert!(matches!(err, StashError::StackChanged), "{err:?}");
        let entries = stack_entries().expect("stack entries");
        assert_eq!(entries.len(), 1, "the reincarnated entry survives");
        assert_eq!(entries[0].raw_line, reincarnation);
        assert_eq!(
            entries[0].message, "WIP",
            "and the generation column never leaks into the message"
        );
    }

    /// W2 §C.4.3: the raw line `update_stash_ref` RETURNS is byte-identical
    /// to what a later stack read parses — the push-time capture really is
    /// the entry's identity (autostash carries it across the pull).
    #[tokio::test]
    #[serial_test::serial]
    async fn update_stash_ref_returns_the_parsed_raw_line() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = crate::utils::test::ChangeDirGuard::new(tmp.path());
        crate::utils::test::setup_with_new_libra_in(tmp.path()).await;
        let storage = util::storage_path();
        let hash = ObjectHash::from_str("00000000000000000000000000000000000000bb").expect("hash");
        let committer = Signature::from_data(
            "committer T <t@x> 1700000000 +0000"
                .to_string()
                .into_bytes(),
        )
        .expect("signature");

        let returned = update_stash_ref(&storage, &hash, &committer, "autostash before pull")
            .expect("update stash ref");
        let entries = stack_entries().expect("stack entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].raw_line, returned,
            "the returned line is exactly what a later read parses"
        );
        assert_eq!(entries[0].stash_id, hash.to_string());
    }

    /// W2 §C.4.3 fail-safe: an UNREADABLE HEAD commit must report "changed"
    /// (so `stash push` proceeds and surfaces real errors) instead of the
    /// old silent `false` that no-op'd as "No local changes to save".
    #[tokio::test]
    #[serial_test::serial]
    async fn has_changes_fails_safe_on_unreadable_head_commit() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = crate::utils::test::ChangeDirGuard::new(tmp.path());
        crate::utils::test::setup_with_new_libra_in(tmp.path()).await;
        // Point HEAD at a commit that does not exist in the object store.
        let bogus =
            ObjectHash::from_str("00000000000000000000000000000000000000aa").expect("bogus hash");
        Head::update_result(Head::Detached(bogus), None)
            .await
            .expect("detach onto bogus commit");
        assert!(
            has_changes().await,
            "an unreadable HEAD commit reports CHANGED (fail-safe)"
        );
    }

    /// Pin the `Display` format for the static-message and direct-message
    /// variants of [`StashError`]. These strings are used as the
    /// `CliError` message via the From<StashError> mapping and surface
    /// in both human and `--json` envelopes for `stash`.
    ///
    /// Source-chained variants whose body is solely a wrapped string
    /// (ReadObject, WriteObject, IndexLoad, IndexSave, ResetFailed, Other) are
    /// covered indirectly by pinning the inner `{0}` echo form here for
    /// representative cases (Other does that explicitly).
    #[test]
    fn stash_error_display_pins_each_variant() {
        assert_eq!(
            StashError::StackChanged.to_string(),
            "the stash stack changed concurrently while this command ran; nothing further was \
         modified — inspect `libra stash list` and re-run",
        );
        assert_eq!(
            StashError::StackChangedAfterApply {
                stash_id: "abc".to_string()
            }
            .to_string(),
            "the stash was applied to this worktree, but the stash stack changed concurrently \
         so entry abc was KEPT (the successful apply is not rolled back) — inspect \
         `libra stash list` and `libra stash drop` it explicitly if desired",
        );
        assert_eq!(
            StashError::StackLock("busy".to_string()).to_string(),
            "cannot lock the stash stack: busy",
        );
        assert_eq!(StashError::NotInRepo.to_string(), "not a libra repository");
        assert_eq!(
            StashError::NoInitialCommit.to_string(),
            "you do not have the initial commit yet",
        );
        assert_eq!(StashError::NoStashFound.to_string(), "no stash found");
        assert_eq!(
            StashError::InvalidStashRef("@bogus".to_string()).to_string(),
            "'@bogus' is not a valid stash reference",
        );
        assert_eq!(
            StashError::StashNotExist(3).to_string(),
            "stash@{3}: stash does not exist",
        );
        assert_eq!(
            StashError::MergeConflict("foo.txt".to_string()).to_string(),
            "merge conflict during stash apply:\n  foo.txt",
        );
        assert_eq!(
            StashError::BranchExists("feature".to_string()).to_string(),
            "a branch named 'feature' already exists",
        );
        assert_eq!(
            StashError::BranchLookupFailed {
                branch: "topic/x".to_string(),
                detail: "db locked".to_string(),
            }
            .to_string(),
            "failed to query branch 'topic/x': db locked",
        );
        assert_eq!(
            StashError::ClearRequiresForce.to_string(),
            "clearing all stash entries requires --force in interactive mode",
        );
        assert_eq!(
            StashError::ReadObject("permission denied".to_string()).to_string(),
            "failed to read object: permission denied",
        );
        assert_eq!(
            StashError::WriteObject("disk full".to_string()).to_string(),
            "failed to write object: disk full",
        );
        assert_eq!(
            StashError::IndexLoad("corrupt".to_string()).to_string(),
            "failed to load index: corrupt",
        );
        assert_eq!(
            StashError::IndexSave("io error".to_string()).to_string(),
            "failed to save index: io error",
        );
        assert_eq!(
            StashError::ResetFailed("could not restore".to_string()).to_string(),
            "failed to reset working directory: could not restore",
        );
        // Other(s) echoes the inner string verbatim.
        assert_eq!(
            StashError::Other("custom error".to_string()).to_string(),
            "custom error",
        );
    }

    /// Pin the `stable_code()` mapping for every variant of
    /// [`StashError`]. JSON consumers branch on the
    /// [`StableErrorCode`] in the error envelope; three variants
    /// share `IoWriteFailed` (WriteObject / IndexSave / ResetFailed)
    /// and three share `IoReadFailed` (BranchLookupFailed /
    /// ReadObject / IndexLoad), while two share `CliInvalidArguments` (InvalidStashRef /
    /// ClearRequiresForce). A future refactor that reroutes any
    /// variant — for example flipping `BranchExists` from
    /// `ConflictOperationBlocked` to `CliInvalidTarget` — silently
    /// changes the wire surface unless every variant has its own
    /// guard. The single-variant `stash_error_other_has_issue_url_hint`
    /// below stays focused on the Issues-URL hint surface; this test
    /// owns the stable_code surface contract exhaustively.
    #[test]
    fn stash_error_stable_code_pins_each_variant() {
        assert_eq!(
            StashError::StackChanged.stable_code(),
            StableErrorCode::ConflictOperationBlocked,
        );
        assert_eq!(
            StashError::StackChangedAfterApply {
                stash_id: "ignored".to_string()
            }
            .stable_code(),
            StableErrorCode::ConflictOperationBlocked,
        );
        assert_eq!(
            StashError::StackLock("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            StashError::NotInRepo.stable_code(),
            StableErrorCode::RepoNotFound,
        );
        assert_eq!(
            StashError::NoInitialCommit.stable_code(),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            StashError::NoStashFound.stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            StashError::InvalidStashRef("ignored".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            StashError::StashNotExist(0).stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            StashError::MergeConflict("ignored".to_string()).stable_code(),
            StableErrorCode::ConflictUnresolved,
        );
        assert_eq!(
            StashError::BranchExists("ignored".to_string()).stable_code(),
            StableErrorCode::ConflictOperationBlocked,
        );
        assert_eq!(
            StashError::BranchLookupFailed {
                branch: "ignored".to_string(),
                detail: "ignored".to_string(),
            }
            .stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            StashError::ClearRequiresForce.stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            StashError::ReadObject("ignored".to_string()).stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            StashError::WriteObject("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            StashError::IndexLoad("ignored".to_string()).stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            StashError::IndexSave("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            StashError::ResetFailed("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            StashError::Other("ignored".to_string()).stable_code(),
            StableErrorCode::InternalInvariant,
        );
    }

    /// Cross-Cutting G: `StashError::Other` is the catch-all bucket
    /// that maps to `InternalInvariant`. It must surface the GitHub
    /// Issues URL hint so users can report the bug.
    #[test]
    fn stash_error_other_has_issue_url_hint() {
        let err: CliError = StashError::Other("synthetic failure".to_string()).into();
        assert_eq!(err.stable_code(), StableErrorCode::InternalInvariant);
        assert!(
            err.hints().iter().any(|h| h.as_str().contains("issues")),
            "StashError::Other must include the GitHub Issues URL hint, got hints: {:?}",
            err.hints()
        );
    }
}
