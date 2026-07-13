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
        index::{Index, IndexEntry, Time},
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
        tree, util,
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

    let git_dir =
        util::try_get_storage_path(None).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let index_path = git_dir.join("index");
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
            let data = object::read_git_object(&git_dir, &head_commit_hash)
                .map_err(|e| StashError::ReadObject(e.to_string()))?;
            let c = Commit::from_bytes(&data, head_commit_hash)
                .map_err(|e| StashError::ReadObject(e.to_string()))?;
            let summary = c.message.lines().next().unwrap_or("").to_string();
            (name, summary)
        }
        Head::Detached(_) => {
            let data = object::read_git_object(&git_dir, &head_commit_hash)
                .map_err(|e| StashError::ReadObject(e.to_string()))?;
            let c = Commit::from_bytes(&data, head_commit_hash)
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

    let workdir = git_dir
        .parent()
        .ok_or_else(|| StashError::Other("cannot find workdir".into()))?;
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

    update_stash_ref(&git_dir, &stash_commit_hash, &committer, &final_message)
        .map_err(|e| StashError::WriteObject(e.to_string()))?;

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
    let git_dir =
        util::try_get_storage_path(None).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let index_path = git_dir.join("index");
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

    let workdir = git_dir
        .parent()
        .ok_or_else(|| StashError::Other("cannot find workdir".into()))?;
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
    let git_dir =
        util::try_get_storage_path(None).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let (_, committer) = util::create_signatures().await;
    update_stash_ref(&git_dir, hash, &committer, message)
        .map_err(|e| StashError::WriteObject(e.to_string()))
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

    let git_dir =
        util::try_get_storage_path(None).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let index_path = git_dir.join("index");
    let index = Index::load(&index_path).unwrap_or_else(|_| Index::new());
    let workdir = git_dir
        .parent()
        .ok_or_else(|| StashError::Other("cannot find workdir".into()))?;

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

    update_stash_ref(&git_dir, &stash_commit_hash, &committer, &final_message)
        .map_err(|e| StashError::WriteObject(e.to_string()))?;

    // Reset ONLY the matched paths to HEAD (worktree + index); leave the rest.
    reset_pathspec_to_head(&matched, &head_map, workdir, &index_path)?;

    Ok(StashOutput::Push {
        message: final_message,
        stash_id: stash_commit_hash.to_string(),
        included_untracked: 0,
        kept_index: false,
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
                // Pass the repo-relative path: `new_from_file` records its first
                // argument verbatim as the index entry name (and joins it onto
                // `workdir` for the stat), so an absolute path would corrupt the
                // index. It reads metadata from `workdir.join(rel)`.
                let mut new_entry = IndexEntry::new_from_file(&rel, entry.hash, workdir)
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
    let apply_result = do_apply(stash.clone()).await?;
    let (index, stash_id, branch) = match apply_result {
        StashOutput::Apply {
            index,
            stash_id,
            branch,
        } => (index, stash_id, branch),
        other => {
            return Err(StashError::Other(format!(
                "internal error: expected stash apply to return StashOutput::Apply, got {other:?}",
            )));
        }
    };

    // Drop after successful apply
    do_drop(stash)?;

    Ok(StashOutput::Pop {
        index,
        stash_id,
        branch,
    })
}

/// Stash the tracked working-tree changes for `pull --autostash`. Returns `true`
/// when a stash was created, `false` when there was nothing to stash (clean
/// tree). Untracked/ignored files are left in place, matching Git's autostash.
pub(crate) async fn autostash_push() -> Result<bool, String> {
    let options = StashPushOptions {
        message: Some("autostash before pull".to_string()),
        include_untracked: false,
        include_ignored: false,
        keep_index: false,
        pathspec: Vec::new(),
    };
    match run_push(options).await.map_err(|e| e.to_string())? {
        StashOutput::Noop { .. } => Ok(false),
        _ => Ok(true),
    }
}

/// Re-apply and drop the autostash created by [`autostash_push`].
pub(crate) async fn autostash_pop() -> Result<(), String> {
    run_pop(None).await.map_err(|e| e.to_string())?;
    Ok(())
}

async fn run_list() -> Result<StashOutput, StashError> {
    if !has_stash()? {
        return Ok(StashOutput::List {
            entries: Vec::new(),
        });
    }

    let git_dir =
        util::try_get_storage_path(None).map_err(|e| StashError::ReadObject(e.to_string()))?;
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
    do_drop(stash)
}

async fn run_show(
    stash: Option<String>,
    name_only: bool,
    name_status: bool,
    patch: bool,
) -> Result<StashOutput, StashError> {
    let (index, stash_id_str) = resolve_stash_to_commit_hash(stash)?;
    let git_dir =
        util::try_get_storage_path(None).map_err(|e| StashError::ReadObject(e.to_string()))?;

    let stash_hash =
        ObjectHash::from_str(&stash_id_str).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_commit_data = object::read_git_object(&git_dir, &stash_hash)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_commit = Commit::from_bytes(&stash_commit_data, stash_hash)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;

    let base_hash = *stash_commit
        .parent_commit_ids
        .first()
        .ok_or_else(|| StashError::ReadObject("stash commit is malformed".into()))?;
    let base_commit_data = object::read_git_object(&git_dir, &base_hash)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let base_commit = Commit::from_bytes(&base_commit_data, base_hash)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;

    let base_tree_data = object::read_git_object(&git_dir, &base_commit.tree_id)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let base_tree = Tree::from_bytes(&base_tree_data, base_commit.tree_id)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_tree_data = object::read_git_object(&git_dir, &stash_commit.tree_id)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_tree = Tree::from_bytes(&stash_tree_data, stash_commit.tree_id)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;

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

async fn run_branch(branch_name: String, stash: Option<String>) -> Result<StashOutput, StashError> {
    if InternalBranch::exists_result(&branch_name, None)
        .await
        .map_err(|error| stash_branch_store_error(&branch_name, error))?
    {
        return Err(StashError::BranchExists(branch_name));
    }

    // Resolve stash & metadata for the new branch base.
    let (index, stash_id_str) = resolve_stash_to_commit_hash(stash.clone())?;
    let stash_hash =
        ObjectHash::from_str(&stash_id_str).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let git_dir =
        util::try_get_storage_path(None).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_commit_data = object::read_git_object(&git_dir, &stash_hash)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_commit = Commit::from_bytes(&stash_commit_data, stash_hash)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let base_hash = *stash_commit
        .parent_commit_ids
        .first()
        .ok_or_else(|| StashError::ReadObject("stash commit is malformed".into()))?;

    InternalBranch::update_branch(&branch_name, &base_hash.to_string(), None)
        .await
        .map_err(|e| StashError::Other(format!("failed to create branch '{branch_name}': {e}")))?;

    // Switch HEAD to the new branch so apply runs on the right tip.
    Head::update(Head::Branch(branch_name.clone()), None).await;

    let apply_result = do_apply(stash.clone()).await?;
    let applied = matches!(apply_result, StashOutput::Apply { .. });
    let dropped = if applied {
        do_drop(stash).is_ok()
    } else {
        false
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

    if !has_stash()? {
        return Ok(StashOutput::Clear { cleared_count: 0 });
    }

    let git_dir =
        util::try_get_storage_path(None).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_ref_path = git_dir.join("refs/stash");
    let stash_log_path = git_dir.join("logs/refs/stash");

    let cleared = if stash_log_path.exists() {
        let entries = parse_stash_log_entries(read_stash_log_lines(&stash_log_path)?)?;
        entries.len()
    } else {
        0
    };

    if stash_log_path.exists() {
        std::fs::remove_file(&stash_log_path)
            .map_err(|e| StashError::WriteObject(e.to_string()))?;
    }
    if stash_ref_path.exists() {
        std::fs::remove_file(&stash_ref_path)
            .map_err(|e| StashError::WriteObject(e.to_string()))?;
    }

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
    let (index, hash_str) = resolve_stash_to_commit_hash(stash)?;
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
    let git_dir =
        util::try_get_storage_path(None).map_err(|e| StashError::ReadObject(e.to_string()))?;

    let stash_commit_data = object::read_git_object(&git_dir, &stash_commit_hash)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_commit = Commit::from_bytes(&stash_commit_data, stash_commit_hash)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;

    let base_commit_hash = *stash_commit
        .parent_commit_ids
        .first()
        .ok_or_else(|| StashError::ReadObject("stash commit is malformed".into()))?;
    let base_commit_data = object::read_git_object(&git_dir, &base_commit_hash)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let base_commit = Commit::from_bytes(&base_commit_data, base_commit_hash)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let base_tree_data = object::read_git_object(&git_dir, &base_commit.tree_id)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let base_tree = Tree::from_bytes(&base_tree_data, base_commit.tree_id)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;

    let stash_tree_data = object::read_git_object(&git_dir, &stash_commit.tree_id)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_tree = Tree::from_bytes(&stash_tree_data, stash_commit.tree_id)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let untracked_tree = load_untracked_parent_tree(&stash_commit, &git_dir)?;
    let stash_index_tree = if restore_index {
        Some(load_stash_index_parent_tree(&stash_commit, &git_dir)?)
    } else {
        None
    };

    let workdir = git_dir.parent().ok_or_else(|| {
        StashError::Other(format!(
            "internal error: storage path '{}' has no parent",
            git_dir.display()
        ))
    })?;
    let index_path = git_dir.join("index");

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

fn load_stash_index_parent_tree(stash_commit: &Commit, git_dir: &Path) -> Result<Tree, StashError> {
    let index_commit_hash = stash_commit
        .parent_commit_ids
        .get(1)
        .ok_or_else(|| StashError::ReadObject("stash index parent is missing".into()))?;
    let index_commit_data = object::read_git_object(git_dir, index_commit_hash)
        .map_err(|error| StashError::ReadObject(error.to_string()))?;
    let index_commit = Commit::from_bytes(&index_commit_data, *index_commit_hash)
        .map_err(|error| StashError::ReadObject(error.to_string()))?;
    let index_tree_data = object::read_git_object(git_dir, &index_commit.tree_id)
        .map_err(|error| StashError::ReadObject(error.to_string()))?;
    Tree::from_bytes(&index_tree_data, index_commit.tree_id)
        .map_err(|error| StashError::ReadObject(error.to_string()))
}

fn load_untracked_parent_tree(
    stash_commit: &Commit,
    git_dir: &Path,
) -> Result<Option<Tree>, StashError> {
    let Some(untracked_commit_hash) = stash_commit.parent_commit_ids.get(2) else {
        return Ok(None);
    };

    let untracked_commit_data = object::read_git_object(git_dir, untracked_commit_hash)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let untracked_commit = Commit::from_bytes(&untracked_commit_data, *untracked_commit_hash)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    let untracked_tree_data = object::read_git_object(git_dir, &untracked_commit.tree_id)
        .map_err(|e| StashError::ReadObject(e.to_string()))?;
    Tree::from_bytes(&untracked_tree_data, untracked_commit.tree_id)
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

fn do_drop(stash: Option<String>) -> Result<StashOutput, StashError> {
    if !has_stash()? {
        return Err(StashError::NoStashFound);
    }

    let git_dir =
        util::try_get_storage_path(None).map_err(|e| StashError::ReadObject(e.to_string()))?;
    let stash_ref_path = git_dir.join("refs/stash");
    let stash_log_path = git_dir.join("logs/refs/stash");
    if !stash_log_path.exists() {
        return Err(StashError::NoStashFound);
    }

    let mut entries = parse_stash_log_entries(read_stash_log_lines(&stash_log_path)?)?;
    if entries.is_empty() {
        return Err(StashError::NoStashFound);
    }

    let index_to_drop = match stash {
        None => 0,
        Some(s) => parse_stash_index(&s)?,
    };

    if index_to_drop >= entries.len() {
        return Err(StashError::StashNotExist(index_to_drop));
    }
    let removed_entry = entries.remove(index_to_drop);
    let stash_commit_hash = removed_entry.stash_id;

    if entries.is_empty() {
        std::fs::remove_file(&stash_log_path)
            .map_err(|e| StashError::WriteObject(e.to_string()))?;
        if stash_ref_path.exists() {
            std::fs::remove_file(&stash_ref_path)
                .map_err(|e| StashError::WriteObject(e.to_string()))?;
        }
    } else {
        let new_content = entries
            .iter()
            .map(|entry| entry.raw_line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&stash_log_path, new_content)
            .map_err(|e| StashError::WriteObject(e.to_string()))?;

        if index_to_drop == 0
            && let Some(new_top_entry) = entries.first()
        {
            std::fs::write(&stash_ref_path, format!("{}\n", new_top_entry.stash_id))
                .map_err(|e| StashError::WriteObject(e.to_string()))?;
        }
    }

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
    let Some(git_dir) = util::try_get_storage_path(None).ok() else {
        return false;
    };

    let head_tree_hash = match Head::current_commit().await {
        Some(hash) => {
            let Ok(commit_data) = object::read_git_object(&git_dir, &hash) else {
                return false;
            };
            let Ok(commit) = Commit::from_bytes(&commit_data, hash) else {
                return false;
            };
            commit.tree_id
        }
        None => ObjectHash::from_type_and_data(ObjectType::Tree, &[]),
    };

    let index_path = git_dir.join("index");
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

    let Some(workdir) = git_dir.parent() else {
        return false;
    };
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
    let storage = util::try_get_storage_path(None).map_err(|error| {
        StashError::ReadObject(format!(
            "failed to resolve storage while inspecting the stash ref: {error}"
        ))
    })?;
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
            .map(|(_, message)| message.to_string())
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
pub(crate) fn gc_roots() -> Result<Vec<ObjectHash>, StashError> {
    let storage = util::try_get_storage_path(None).map_err(|error| {
        StashError::ReadObject(format!("failed to resolve stash GC roots: {error}"))
    })?;
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

fn resolve_stash_to_commit_hash(stash_ref: Option<String>) -> Result<(usize, String), StashError> {
    if !has_stash()? {
        return Err(StashError::NoStashFound);
    }

    let git_dir =
        util::try_get_storage_path(None).map_err(|e| StashError::ReadObject(e.to_string()))?;
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

    Ok((index_to_resolve, entries[index_to_resolve].stash_id.clone()))
}

fn update_stash_ref(
    git_dir: &Path,
    stash_hash: &ObjectHash,
    committer: &Signature,
    message: &str,
) -> Result<(), GitError> {
    let stash_ref_path = git_dir.join("refs/stash");
    let stash_log_path = git_dir.join("logs/refs/stash");

    let old_hash = if stash_ref_path.exists() {
        let content = fs::read_to_string(&stash_ref_path)?;
        ObjectHash::from_str(content.trim())
            .map_err(|_| GitError::InvalidHashValue(content.trim().to_string()))?
    } else {
        ObjectHash::default()
    };

    if let Some(parent) = stash_ref_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&stash_ref_path, format!("{stash_hash}\n"))?;

    if let Some(parent) = stash_log_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let reflog_entry = format!(
        "{} {} {} <{}> {} {}\t{}",
        old_hash,
        stash_hash,
        committer.name,
        committer.email,
        committer.timestamp,
        committer.timezone,
        message
    );

    let mut lines = if stash_log_path.exists() {
        let content = fs::read_to_string(&stash_log_path)?;
        content.lines().map(String::from).collect()
    } else {
        Vec::new()
    };

    lines.insert(0, reflog_entry);
    let new_content = lines.join("\n") + "\n";
    fs::write(stash_log_path, new_content)?;

    Ok(())
}

async fn perform_hard_reset(target_commit_id: &ObjectHash) -> Result<(), String> {
    let git_dir = util::try_get_storage_path(None).map_err(|e| e.to_string())?;
    let workdir = git_dir
        .parent()
        .ok_or_else(|| "cannot find workdir".to_string())?;
    let index_path = git_dir.join("index");

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
