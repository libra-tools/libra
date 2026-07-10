//! Implements restore flows to reset files or entire trees from commits or the index, respecting pathspecs and staged vs worktree targets.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    env, fs, io,
    path::{Path, PathBuf},
};

use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::{
        index::{Index, IndexEntry},
        object::{
            blob::Blob,
            commit::Commit,
            tree::{Tree, TreeItemMode},
            types::ObjectType,
        },
    },
};
use serde::Serialize;

use crate::{
    command::{calc_file_blob_hash, load_object},
    internal::{
        branch::{self, Branch, BranchStoreError},
        head::Head,
        protocol::lfs_client::LFSClient,
    },
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        lfs,
        object_ext::{BlobExt, CommitExt, TreeExt},
        output::{OutputConfig, emit_json_data},
        path,
        pathspec::{PathspecError, PathspecSet},
        util,
    },
};

const RESTORE_EXAMPLES: &str = "\
EXAMPLES:
    libra restore file.txt                Restore file from index to worktree
    libra restore --staged file.txt       Unstage a file (restore index from HEAD)
    libra restore --source HEAD~1 .       Restore all files from a previous commit
    libra restore -S -W file.txt          Restore both worktree and index
    libra restore --ours file.txt         Take the 'our' side of a merge conflict
    libra restore --theirs file.txt       Take the 'their' side of a merge conflict
    libra restore --merge file.txt        Recreate the conflict markers from the index stages
    libra restore --json --source HEAD .  Structured JSON output for agents";

// ── Typed error ──────────────────────────────────────────────────────

/// Typed error for checkout / restore operations, providing enough detail for
/// callers (e.g. `clone`) to map each failure into a stable error code without
/// resorting to string matching on `io::Error` messages.
#[derive(thiserror::Error, Debug)]
pub enum RestoreError {
    #[error("failed to resolve checkout source")]
    ResolveSource,
    #[error("reference is not a commit")]
    ReferenceNotCommit,
    #[error("pathspec '{0}' did not match any files")]
    PathspecNotMatched(String),
    #[error("failed to read index")]
    ReadIndex,
    #[error("failed to read object")]
    ReadObject,
    #[error("failed to read worktree")]
    ReadWorktree,
    #[error("invalid path encoding")]
    InvalidPathEncoding,
    #[error("failed to write worktree file")]
    WriteWorktree,
    #[error("failed to download LFS content")]
    LfsDownload,
    /// Refused to restore from a Libra-managed locked branch (`intent`,
    /// `traces`, …). These refs hold AI-agent state that the user
    /// should not be able to overwrite with `restore --source`.
    #[error("refusing to restore from locked branch '{0}'")]
    LockedSource(String),
    /// Refused to mutate the worktree while HEAD is attached to a
    /// Libra-managed AI branch.
    #[error("refusing to restore worktree while on locked branch '{0}'")]
    LockedCurrentBranch(String),
    #[error("failed to read pathspec file: {0}")]
    PathspecFileRead(String),
    /// A matched path is unmerged (conflict stages 1/2/3 present) and no
    /// conflict-resolution flag was given. Mirrors Git's
    /// `path '<file>' is unmerged`.
    #[error("path '{0}' is unmerged")]
    PathUnmerged(String),
    /// `--overlay --ours`/`--theirs` asked for a conflict stage that the path
    /// does not have (a modify/delete conflict). This is the OVERLAY-mode error:
    /// it mirrors Git's `path '<file>' does not have our/their version`, which
    /// Git also only emits under `--overlay`. In the default (no-overlay) mode
    /// `restore` instead removes the worktree file (the deleting side wins), so
    /// this variant is not reached there.
    #[error("path '{path}' does not have {} version", stage_side(*stage))]
    MissingStageVersion { path: String, stage: u8 },
    /// `--conflict=<style>` was given an unsupported value (only `merge` and
    /// `diff3` are accepted; `zdiff3` is not implemented).
    #[error("unsupported conflict style '{0}' (expected 'merge' or 'diff3')")]
    UnsupportedConflictStyle(String),
    #[error("symlink checkout is not supported on this platform: {0}")]
    SymlinkUnsupported(String),
    #[error("{0}")]
    InvalidPathspec(String),
}

/// Human label for a conflict stage used by [`RestoreError::MissingStageVersion`].
const fn stage_side(stage: u8) -> &'static str {
    match stage {
        2 => "our",
        3 => "their",
        _ => "a",
    }
}

impl RestoreError {
    fn stable_code(&self) -> StableErrorCode {
        match self {
            Self::ResolveSource => StableErrorCode::CliInvalidTarget,
            Self::ReferenceNotCommit => StableErrorCode::CliInvalidTarget,
            Self::PathspecNotMatched(_) => StableErrorCode::CliInvalidTarget,
            Self::ReadIndex => StableErrorCode::IoReadFailed,
            Self::ReadObject => StableErrorCode::IoReadFailed,
            Self::ReadWorktree => StableErrorCode::IoReadFailed,
            Self::InvalidPathEncoding => StableErrorCode::CliInvalidArguments,
            Self::WriteWorktree => StableErrorCode::IoWriteFailed,
            Self::LfsDownload => StableErrorCode::NetworkUnavailable,
            Self::LockedSource(_) => StableErrorCode::CliInvalidTarget,
            Self::LockedCurrentBranch(_) => StableErrorCode::ConflictOperationBlocked,
            Self::PathspecFileRead(_) => StableErrorCode::IoReadFailed,
            Self::PathUnmerged(_) => StableErrorCode::ConflictUnresolved,
            Self::MissingStageVersion { .. } => StableErrorCode::ConflictUnresolved,
            Self::UnsupportedConflictStyle(_) => StableErrorCode::CliInvalidArguments,
            Self::SymlinkUnsupported(_) => StableErrorCode::Unsupported,
            Self::InvalidPathspec(_) => StableErrorCode::CliInvalidTarget,
        }
    }
}

impl From<RestoreError> for CliError {
    fn from(error: RestoreError) -> Self {
        let stable_code = error.stable_code();
        let message = error.to_string();
        match error {
            // Ref resolution keeps Git-compatible exit 128 semantics even though
            // the stable code stays target-oriented for machine classification.
            RestoreError::ResolveSource => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_exit_code(128)
                .with_hint("check that the source ref exists with 'libra log'"),
            RestoreError::ReferenceNotCommit => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_exit_code(128)
                .with_hint("only commit references can be used as restore source"),
            RestoreError::PathspecNotMatched(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("check the path and try again"),
            RestoreError::LfsDownload => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("check LFS server availability"),
            RestoreError::LockedSource(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_exit_code(128)
                .with_hint(
                    "Libra-managed branches like 'intent' and 'traces' cannot be used as restore sources",
                ),
            RestoreError::LockedCurrentBranch(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("switch to a user branch before modifying the worktree"),
            // Unresolved-conflict failures keep Git's exit 128 while exposing the
            // ConflictUnresolved stable code for machine classification.
            RestoreError::PathUnmerged(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_exit_code(128)
                .with_hint("resolve the conflict, or pass --ours/--theirs/--merge/--ignore-unmerged"),
            RestoreError::MissingStageVersion { .. } => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_exit_code(128)
                .with_hint("the path has no version at that conflict stage"),
            RestoreError::UnsupportedConflictStyle(_) => CliError::command_usage(message)
                .with_stable_code(stable_code)
                .with_hint("use --conflict=merge (default) or --conflict=diff3"),
            RestoreError::InvalidPathspec(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("use supported pathspec magic: top, exclude, icase, literal, glob"),
            _ => CliError::fatal(message).with_stable_code(stable_code),
        }
    }
}

// ── Structured output ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct RestoreOutput {
    pub source: Option<String>,
    pub worktree: bool,
    pub staged: bool,
    pub restored_files: Vec<String>,
    pub deleted_files: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct RestoreTarget {
    hash: ObjectHash,
    mode: Option<TreeItemMode>,
}

impl RestoreTarget {
    const fn new(hash: ObjectHash, mode: Option<TreeItemMode>) -> Self {
        Self { hash, mode }
    }

    fn index_mode(self) -> u32 {
        self.mode
            .and_then(tree_item_mode_to_index_mode)
            .unwrap_or(0o100644)
    }
}

// ── Entry points ─────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(about = "Restore working tree files")]
#[command(after_help = RESTORE_EXAMPLES)]
pub struct RestoreArgs {
    /// files or dir to restore
    #[clap(required_unless_present = "pathspec_from_file")]
    pub pathspec: Vec<String>,
    /// source
    #[clap(long, short)]
    pub source: Option<String>,
    /// worktree
    #[clap(long, short = 'W')]
    pub worktree: bool,
    /// staged
    #[clap(long, short = 'S')]
    pub staged: bool,
    /// Restore the "our" side (conflict stage 2) of unmerged paths to the
    /// working tree. Reads the conflict index stages and writes the worktree
    /// only; the index is left unmerged. Mutually exclusive with `--theirs`,
    /// `--source`, `--staged`, and `--ignore-unmerged`.
    #[clap(
        long,
        short = '2',
        conflicts_with_all = ["theirs", "source", "staged", "ignore_unmerged"],
    )]
    pub ours: bool,
    /// Restore the "their" side (conflict stage 3) of unmerged paths to the
    /// working tree. Mutually exclusive with `--ours`, `--source`, `--staged`,
    /// and `--ignore-unmerged`.
    #[clap(
        long,
        short = '3',
        conflicts_with_all = ["source", "staged", "ignore_unmerged"],
    )]
    pub theirs: bool,
    /// Skip unmerged paths instead of erroring. Without a conflict-resolution
    /// flag, `restore` refuses to touch unmerged paths; `--ignore-unmerged`
    /// silently skips them and restores the rest.
    #[clap(long)]
    pub ignore_unmerged: bool,
    /// Recreate the conflict in the working tree for unmerged paths: write the
    /// conflict markers from the index stages (ours from stage 2, theirs from
    /// stage 3), leaving the index unmerged. Mutually exclusive with
    /// `--ours`/`--theirs`/`--source`/`--staged`/`--ignore-unmerged`. (Implied by
    /// `--conflict`.) Restore independently rebuilds whole-file `ours`/`theirs`
    /// markers from the index stages (one `ours` block / one `theirs` block), with
    /// generic `ours`/`theirs` labels since the stages carry no commit names — not
    /// Git's line-level 3-way merge, and unlike `libra merge`/`cherry-pick`, which
    /// now emit line-level hunks for both-modified text conflicts.
    #[clap(
        long,
        conflicts_with_all = ["ours", "theirs", "source", "staged", "ignore_unmerged"],
    )]
    pub merge: bool,
    /// Conflict-marker style for `--merge` (implies `--merge`): `merge` (default —
    /// `ours`/`theirs` blocks) or `diff3` (also include the `base` from stage 1).
    /// `zdiff3` is not supported.
    #[clap(
        long,
        value_name = "STYLE",
        conflicts_with_all = ["ours", "theirs", "source", "staged", "ignore_unmerged"],
    )]
    pub conflict: Option<String>,
    /// Read pathspecs from the given file, one per line (`-` reads stdin).
    #[clap(long = "pathspec-from-file", value_name = "FILE")]
    pub pathspec_from_file: Option<String>,
    /// Pathspecs read via --pathspec-from-file are separated by NUL, not newlines.
    #[clap(long = "pathspec-file-nul", requires = "pathspec_from_file")]
    pub pathspec_file_nul: bool,
    /// Do not show a progress meter. Accepted for Git parity and is a no-op:
    /// Libra's restore never renders a progress meter, so there is nothing to
    /// suppress.
    #[clap(long = "no-progress")]
    pub no_progress: bool,
    /// Restore in overlay mode: only create or update the paths present in the
    /// source; tracked paths that are absent from the source are left alone
    /// rather than removed. Toggle pair with `--no-overlay`; the last one wins.
    #[clap(long = "overlay", overrides_with = "no_overlay")]
    pub overlay: bool,
    /// Do not restore in overlay mode (the default): paths missing from the
    /// source are removed from the target so it matches the source exactly.
    /// Toggle pair with `--overlay`; the last one wins.
    #[clap(long = "no-overlay", overrides_with = "overlay")]
    pub no_overlay: bool,
}

pub async fn execute(args: RestoreArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting.
///
/// # Side Effects
/// - Restores selected paths from the index or a commit tree.
/// - May rewrite index entries when `--staged` is set.
/// - May overwrite working-tree files when the worktree target is active.
/// - Renders human or JSON output for restored paths.
///
/// # Errors
/// Returns [`CliError`] when the repository is missing, the source revision or
/// pathspecs cannot be resolved, object reads fail, or index/worktree writes
/// fail.
pub async fn execute_safe(args: RestoreArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    let result = run_restore(args).await.map_err(CliError::from)?;
    render_restore_output(&result, output)
}

pub(crate) async fn execute_to_output(args: RestoreArgs) -> CliResult<RestoreOutput> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    run_restore(args).await.map_err(CliError::from)
}

// ── Core execution ───────────────────────────────────────────────────

/// Read pathspecs from a file for `--pathspec-from-file`. Entries are separated
/// by newlines, or by NUL when `--pathspec-file-nul` is set; `-` reads stdin.
/// Empty entries are dropped (and a trailing `\r` is stripped in newline mode).
fn read_restore_pathspec_file(path: &str, nul: bool) -> Result<Vec<String>, RestoreError> {
    let raw = if path == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|error| RestoreError::PathspecFileRead(error.to_string()))?;
        buf
    } else {
        std::fs::read_to_string(path)
            .map_err(|error| RestoreError::PathspecFileRead(format!("{path}: {error}")))?
    };

    let separator = if nul { '\0' } else { '\n' };
    Ok(raw
        .split(separator)
        .map(|entry| {
            if nul {
                entry
            } else {
                entry.strip_suffix('\r').unwrap_or(entry)
            }
        })
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect())
}

async fn compile_restore_pathspecs(raw: &[String]) -> Result<PathspecSet, RestoreError> {
    if raw.is_empty() {
        return Err(RestoreError::InvalidPathspec(
            "no pathspec was given".to_string(),
        ));
    }
    let current_dir = env::current_dir().map_err(|_| RestoreError::ReadWorktree)?;
    let workdir = util::try_working_dir().map_err(|_| RestoreError::ReadWorktree)?;
    let ignore_case = crate::utils::path_case::effective_ignore_case()
        .await
        .map_err(|_| RestoreError::ReadWorktree)?;
    PathspecSet::from_workdir_with_default_icase(raw, &current_dir, &workdir, ignore_case)
        .map_err(invalid_pathspec)
}

fn invalid_pathspec(error: PathspecError) -> RestoreError {
    RestoreError::InvalidPathspec(error.to_string())
}

async fn run_restore(mut args: RestoreArgs) -> Result<RestoreOutput, RestoreError> {
    // `--pathspec-from-file` populates the pathspec list from a file (or stdin
    // for `-`) before the normal restore logic runs.
    if let Some(file) = args.pathspec_from_file.take() {
        args.pathspec = read_restore_pathspec_file(&file, args.pathspec_file_nul)?;
    }
    let pathspecs = compile_restore_pathspecs(&args.pathspec).await?;
    let staged = args.staged;
    let mut worktree = args.worktree;
    if !staged {
        worktree = true;
    }

    const HEAD: &str = "HEAD";
    let mut source = args.source;
    if source.is_none() && staged {
        source = Some(HEAD.to_string());
    }

    // Refuse to use Libra-managed locked branches (`intent`, `traces`)
    // as a restore source. `is_locked_revision` also strips revision suffixes
    // (`~1`, `^`, `@{0}`) so users can't end-run the guard with `traces~1`.
    if let Some(src) = source.as_deref()
        && branch::is_locked_revision(src)
    {
        return Err(RestoreError::LockedSource(src.to_string()));
    }
    if worktree {
        reject_restore_on_ai_managed_current_branch().await?;
    }

    // Conflict-stage restore (`--ours`/`--theirs`): read the unmerged index
    // stages and write the working tree only, leaving the index unmerged. clap
    // guarantees `--source`/`--staged` are absent here, so this is purely a
    // worktree operation.
    if args.ours || args.theirs {
        let stage = if args.ours { 2 } else { 3 };
        let (restored, deleted) = restore_conflict_stage(&pathspecs, stage, args.overlay).await?;
        return Ok(RestoreOutput {
            source: None,
            worktree: true,
            staged: false,
            restored_files: restored,
            deleted_files: deleted,
        });
    }

    // Conflict re-materialization (`--merge` / `--conflict=<style>`): rewrite the
    // working tree for unmerged paths with the conflict markers rebuilt from the
    // index stages, leaving the index unmerged. clap guarantees
    // `--source`/`--staged` are absent here.
    if args.merge || args.conflict.is_some() {
        let diff3 = match args.conflict.as_deref() {
            None | Some("merge") => false,
            Some("diff3") => true,
            Some(other) => return Err(RestoreError::UnsupportedConflictStyle(other.to_string())),
        };
        let restored = restore_conflict_merge(&pathspecs, diff3).await?;
        return Ok(RestoreOutput {
            source: None,
            worktree: true,
            staged: false,
            restored_files: restored,
            deleted_files: Vec::new(),
        });
    }

    let storage = util::objects_storage();
    let mut target_blobs = resolve_target_blobs(source.as_deref(), staged, &storage).await?;

    // Unmerged guard: a plain restore must not silently act on a matched
    // unmerged path. Without an exemption this is a fatal error (Git's
    // `path '<file>' is unmerged`, exit 128); `--ignore-unmerged` instead drops
    // the unmerged paths from the restore set so the rest still restore.
    let mut skipped_unmerged_paths = Vec::new();
    let unmerged_matches = collect_matched_unmerged_paths(&pathspecs)?;
    if !unmerged_matches.is_empty() {
        if args.ignore_unmerged {
            // `skip` holds the index-relative unmerged paths matched by the
            // pathspecs. Drop them from the restore inputs so they are never
            // rewritten...
            skipped_unmerged_paths = unmerged_matches;
            target_blobs.retain(|(p, _)| !skipped_unmerged_paths.contains(p));
            // Paths matched only by skipped unmerged entries are treated as clean
            // no-ops below by including `skip` in the no-match allowance.
        } else {
            let first = path_to_utf8_typed(&unmerged_matches[0])?;
            return Err(RestoreError::PathUnmerged(first.to_string()));
        }
    }

    let mut restored_files = Vec::new();
    let mut deleted_files = Vec::new();

    // Overlay mode (`--overlay`) only creates/updates paths from the source and
    // never removes tracked paths absent from it; the default (`--no-overlay`)
    // removes them so the target matches the source exactly.
    let overlay = args.overlay;

    if worktree {
        let (restored, deleted) =
            restore_worktree_tracked(&pathspecs, &target_blobs, overlay, &skipped_unmerged_paths)
                .await?;
        restored_files.extend(restored);
        deleted_files.extend(deleted);
    }
    if staged {
        let (restored, deleted) =
            restore_index_tracked(&pathspecs, &target_blobs, overlay, &skipped_unmerged_paths)?;
        let mut restored_seen: HashSet<String> = restored_files.iter().cloned().collect();
        let mut deleted_seen: HashSet<String> = deleted_files.iter().cloned().collect();

        for f in restored {
            if restored_seen.insert(f.clone()) {
                restored_files.push(f);
            }
        }
        for f in deleted {
            if deleted_seen.insert(f.clone()) {
                deleted_files.push(f);
            }
        }
    }

    Ok(RestoreOutput {
        source: source.clone(),
        worktree,
        staged,
        restored_files,
        deleted_files,
    })
}

// ── Rendering ────────────────────────────────────────────────────────

fn render_restore_output(result: &RestoreOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("restore", result, output);
    }

    if output.quiet {
        return Ok(());
    }

    let total = result.restored_files.len() + result.deleted_files.len();
    if total > 0 {
        let source_desc = result.source.as_deref().unwrap_or("the index");
        println!("Updated {total} path(s) from {source_desc}");
    }
    Ok(())
}

// ── Resolve target blobs ─────────────────────────────────────────────

async fn resolve_target_blobs(
    source: Option<&str>,
    staged: bool,
    storage: &ClientStorage,
) -> Result<Vec<(PathBuf, RestoreTarget)>, RestoreError> {
    const HEAD: &str = "HEAD";

    match source {
        None => {
            if staged {
                return Err(RestoreError::ResolveSource);
            }
            let index = Index::load(path::index()).map_err(|_| RestoreError::ReadIndex)?;
            Ok(index
                .tracked_entries(0)
                .into_iter()
                .map(|entry| {
                    (
                        PathBuf::from(&entry.name),
                        RestoreTarget::new(entry.hash, index_mode_to_tree_item_mode(entry.mode)),
                    )
                })
                .collect())
        }
        Some(src) => {
            let commit = if src == HEAD {
                Head::current_commit_result()
                    .await
                    .map_err(map_restore_branch_store_error)?
                    .ok_or(RestoreError::ResolveSource)?
            } else if src.contains('~') || src.contains('^') {
                util::get_commit_base_typed(src)
                    .await
                    .map_err(|_| RestoreError::ResolveSource)?
            } else {
                resolve_source_commit(src, storage).await?
            };

            let tree_id = load_object::<Commit>(&commit)
                .map_err(|_| RestoreError::ReadObject)?
                .tree_id;
            Ok(load_object::<Tree>(&tree_id)
                .map_err(|_| RestoreError::ReadObject)?
                .get_plain_items_with_mode()
                .into_iter()
                .map(|(path, hash, mode)| (path, RestoreTarget::new(hash, Some(mode))))
                .collect())
        }
    }
}

// ── Worktree restore (unified typed path) ────────────────────────────

async fn restore_worktree_tracked(
    pathspecs: &PathspecSet,
    target_blobs: &[(PathBuf, RestoreTarget)],
    overlay: bool,
    allowed_unmatched: &[PathBuf],
) -> Result<(Vec<String>, Vec<String>), RestoreError> {
    let target_map = preprocess_blobs(target_blobs);
    let index = Index::load(path::index()).map_err(|_| RestoreError::ReadIndex)?;
    let file_paths =
        collect_restore_worktree_paths(pathspecs, &target_map, &index, allowed_unmatched)?;
    let mut restored = Vec::new();
    let mut deleted = Vec::new();

    for path_wd in &file_paths {
        let path_abs = util::workdir_to_absolute(path_wd);
        let path_wd_str = path_to_utf8_typed(path_wd)?;
        let tracked = index.tracked(path_wd_str, 0);
        if !worktree_path_exists(&path_abs) {
            if let Some(target) = target_map.get(path_wd) {
                restore_target_to_file_typed(*target, path_wd).await?;
                restored.push(path_wd.display().to_string());
            } else if !tracked {
                return Err(pathspec_not_matched(path_wd));
            }
        } else if let Some(target) = target_map.get(path_wd) {
            let hash = calc_file_blob_hash(&path_abs).map_err(|_| RestoreError::ReadObject)?;
            if hash != target.hash || !worktree_mode_matches(&path_abs, target.mode)? {
                restore_target_to_file_typed(*target, path_wd).await?;
                restored.push(path_wd.display().to_string());
            } else {
                apply_worktree_target_mode(&path_abs, target.mode)?;
            }
        } else if !overlay && tracked {
            fs::remove_file(&path_abs).map_err(|_| RestoreError::WriteWorktree)?;
            util::clear_empty_dir(&path_abs);
            deleted.push(path_wd.display().to_string());
        }
    }

    Ok((restored, deleted))
}

// ── Index restore (unified typed path) ───────────────────────────────

fn restore_index_tracked(
    pathspecs: &PathspecSet,
    target_blobs: &[(PathBuf, RestoreTarget)],
    overlay: bool,
    allowed_unmatched: &[PathBuf],
) -> Result<(Vec<String>, Vec<String>), RestoreError> {
    let target_map = preprocess_blobs(target_blobs);

    let idx_file = path::index();
    let mut index = Index::load(&idx_file).map_err(|_| RestoreError::ReadIndex)?;
    // Source paths missing from the index — added in BOTH modes. Overlay only
    // suppresses *removal* of index entries absent from the source (gated below).
    let deleted_files_index =
        get_index_deleted_files_in_filters_typed(&index, pathspecs, &target_map)?;

    ensure_positive_pathspecs_match(
        pathspecs,
        restore_match_candidates(&target_map, &index, allowed_unmatched),
    )?;
    let mut file_paths = filter_paths(&index.tracked_files(), pathspecs);
    file_paths.extend(deleted_files_index);

    let mut restored = Vec::new();
    let mut deleted = Vec::new();

    for path in &file_paths {
        let path_str = path_to_utf8_typed(path)?;
        if !index.tracked(path_str, 0) {
            if let Some(target) = target_map.get(path) {
                let blob =
                    load_object::<Blob>(&target.hash).map_err(|_| RestoreError::ReadObject)?;
                index.add(index_entry_from_target(
                    path_str.to_string(),
                    *target,
                    blob.data.len() as u32,
                ));
                restored.push(path.display().to_string());
            } else {
                return Err(pathspec_not_matched(path));
            }
        } else if let Some(target) = target_map.get(path) {
            let mode_matches = index
                .get(path_str, 0)
                .map(|entry| entry.mode == target.index_mode())
                .unwrap_or(false);
            if !index.verify_hash(path_str, 0, &target.hash) || !mode_matches {
                let blob =
                    load_object::<Blob>(&target.hash).map_err(|_| RestoreError::ReadObject)?;
                index.update(index_entry_from_target(
                    path_str.to_string(),
                    *target,
                    blob.data.len() as u32,
                ));
                restored.push(path.display().to_string());
            }
        } else if !overlay {
            index.remove(path_str, 0);
            deleted.push(path.display().to_string());
        }
    }

    index
        .save(&idx_file)
        .map_err(|_| RestoreError::WriteWorktree)?;

    Ok((restored, deleted))
}

// ── Legacy public API (used by worktree.rs and checkout) ─────────────

/// Low-level restore that skips the repository-existence check.
///
/// # Preconditions
///
/// The caller **must** ensure a valid libra repository is reachable from the
/// current working directory (e.g. by calling `util::require_repo()` or
/// `execute_safe()` first).  This function is `pub` because it is used by
/// `worktree.rs`, which performs its own repository validation.
pub async fn execute_checked(args: RestoreArgs) -> io::Result<()> {
    let staged = args.staged;
    let mut worktree = args.worktree;
    if !staged {
        worktree = true;
    }

    const HEAD: &str = "HEAD";
    let mut source = args.source;
    if source.is_none() && staged {
        source = Some(HEAD.to_string());
    }

    let storage = util::objects_storage();
    let target_commit: Option<ObjectHash> = match source {
        None => {
            assert!(!staged);
            None
        }
        Some(ref src) => {
            if src == HEAD {
                Some(
                    Head::current_commit_result()
                        .await
                        .map_err(|error| io::Error::other(error.to_string()))?
                        .ok_or_else(|| io::Error::other("could not resolve HEAD"))?,
                )
            } else if src.contains('~') || src.contains('^') {
                Some(
                    util::get_commit_base_typed(src)
                        .await
                        .map_err(|error| io::Error::other(error.to_string()))?,
                )
            } else {
                resolve_source_commit_io(src, &storage)
                    .await
                    .map(Some)
                    .map_err(|error| io::Error::other(error.to_string()))?
            }
        }
    };

    let target_blobs: Vec<(PathBuf, RestoreTarget)> = {
        match (source.as_ref(), target_commit) {
            (None, _) => {
                assert!(!staged);
                let index =
                    Index::load(path::index()).map_err(|e| io::Error::other(e.to_string()))?;
                index
                    .tracked_entries(0)
                    .into_iter()
                    .map(|entry| {
                        (
                            PathBuf::from(&entry.name),
                            RestoreTarget::new(
                                entry.hash,
                                index_mode_to_tree_item_mode(entry.mode),
                            ),
                        )
                    })
                    .collect()
            }
            (Some(_), Some(commit)) => {
                let tree_id = Commit::load(&commit).tree_id;
                let tree = Tree::load(&tree_id);
                tree.get_plain_items_with_mode()
                    .into_iter()
                    .map(|(path, hash, mode)| (path, RestoreTarget::new(hash, Some(mode))))
                    .collect()
            }
            (Some(src), None) => {
                if storage
                    .search_result(src)
                    .await
                    .map_err(|error| io::Error::other(error.to_string()))?
                    .len()
                    != 1
                {
                    return Err(io::Error::other(format!("could not resolve {src}")));
                } else {
                    return Err(io::Error::other(format!(
                        "reference is not a commit: {src}"
                    )));
                }
            }
        }
    };

    let pathspecs = compile_restore_pathspecs(&args.pathspec)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;

    if worktree {
        restore_worktree_tracked(&pathspecs, &target_blobs, false, &[])
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
    }
    if staged {
        restore_index_tracked(&pathspecs, &target_blobs, false, &[])
            .map_err(|error| io::Error::other(error.to_string()))?;
    }
    Ok(())
}

/// Typed checkout entry point that returns [`RestoreError`] instead of
/// `io::Error`, allowing callers like `clone` to map each failure category
/// into a distinct stable error code.
pub async fn execute_checked_typed(args: RestoreArgs) -> Result<(), RestoreError> {
    let staged = args.staged;
    let mut worktree = args.worktree;
    if !staged {
        worktree = true;
    }

    const HEAD: &str = "HEAD";
    let mut source = args.source;
    if source.is_none() && staged {
        source = Some(HEAD.to_string());
    }

    let storage = util::objects_storage();
    let target_blobs: Vec<(PathBuf, RestoreTarget)> = match source.as_ref() {
        None => {
            if staged {
                return Err(RestoreError::ResolveSource);
            }
            let index = Index::load(path::index()).map_err(|_| RestoreError::ReadIndex)?;
            index
                .tracked_entries(0)
                .into_iter()
                .map(|entry| {
                    (
                        PathBuf::from(&entry.name),
                        RestoreTarget::new(entry.hash, index_mode_to_tree_item_mode(entry.mode)),
                    )
                })
                .collect()
        }
        Some(src) => {
            let commit = if src == HEAD {
                Head::current_commit_result()
                    .await
                    .map_err(map_restore_branch_store_error)?
                    .ok_or(RestoreError::ResolveSource)?
            } else {
                resolve_source_commit(src, &storage).await?
            };

            let tree_id = load_object::<Commit>(&commit)
                .map_err(|_| RestoreError::ReadObject)?
                .tree_id;
            load_object::<Tree>(&tree_id)
                .map_err(|_| RestoreError::ReadObject)?
                .get_plain_items_with_mode()
                .into_iter()
                .map(|(path, hash, mode)| (path, RestoreTarget::new(hash, Some(mode))))
                .collect()
        }
    };

    let pathspecs = compile_restore_pathspecs(&args.pathspec).await?;
    if worktree {
        restore_worktree_tracked(&pathspecs, &target_blobs, false, &[]).await?;
    }
    if staged {
        restore_index_tracked(&pathspecs, &target_blobs, false, &[])?;
    }
    Ok(())
}

// ── Shared helpers ───────────────────────────────────────────────────

async fn resolve_source_commit(
    src: &str,
    storage: &ClientStorage,
) -> Result<ObjectHash, RestoreError> {
    if let Some(branch) = Branch::find_branch_result(src, None)
        .await
        .map_err(map_restore_branch_store_error)?
    {
        return Ok(branch.commit);
    }

    if Branch::exists_result(src, None)
        .await
        .map_err(map_restore_branch_store_error)?
    {
        return Err(RestoreError::ResolveSource);
    }

    let objs = storage
        .search_result(src)
        .await
        .map_err(|_| RestoreError::ReadObject)?;
    if objs.len() != 1 {
        return Err(RestoreError::ResolveSource);
    }
    if !storage.is_object_type(&objs[0], ObjectType::Commit) {
        return Err(RestoreError::ReferenceNotCommit);
    }
    Ok(objs[0])
}

async fn resolve_source_commit_io(
    src: &str,
    storage: &ClientStorage,
) -> Result<ObjectHash, String> {
    if let Some(branch) = Branch::find_branch_result(src, None)
        .await
        .map_err(|e| e.to_string())?
    {
        return Ok(branch.commit);
    }

    if Branch::exists_result(src, None)
        .await
        .map_err(|e| e.to_string())?
    {
        return Err(format!("could not resolve {src}"));
    }

    let objs = storage
        .search_result(src)
        .await
        .map_err(|e| e.to_string())?;
    if objs.len() != 1 {
        return Err(format!("could not resolve {src}"));
    }
    if !storage.is_object_type(&objs[0], ObjectType::Commit) {
        return Err(format!("reference is not a commit: {src}"));
    }
    Ok(objs[0])
}

fn map_restore_branch_store_error(error: BranchStoreError) -> RestoreError {
    match error {
        BranchStoreError::Query(_) => RestoreError::ReadObject,
        BranchStoreError::Corrupt { .. } => RestoreError::ReadObject,
        BranchStoreError::NotFound(_) => RestoreError::ResolveSource,
        BranchStoreError::Delete { .. } => RestoreError::ReadObject,
    }
}

async fn reject_restore_on_ai_managed_current_branch() -> Result<(), RestoreError> {
    match Head::current_result()
        .await
        .map_err(map_restore_branch_store_error)?
    {
        Head::Branch(name) if branch::is_ai_managed_branch(&name) => {
            Err(RestoreError::LockedCurrentBranch(name))
        }
        _ => Ok(()),
    }
}

fn preprocess_blobs(blobs: &[(PathBuf, RestoreTarget)]) -> HashMap<PathBuf, RestoreTarget> {
    blobs
        .iter()
        .map(|(path, target)| (path.clone(), *target))
        .collect()
}

fn legacy_targets(blobs: &[(PathBuf, ObjectHash)]) -> Vec<(PathBuf, RestoreTarget)> {
    blobs
        .iter()
        .map(|(path, hash)| (path.clone(), RestoreTarget::new(*hash, None)))
        .collect()
}

fn collect_restore_worktree_paths(
    pathspecs: &PathspecSet,
    target_map: &HashMap<PathBuf, RestoreTarget>,
    index: &Index,
    allowed_unmatched: &[PathBuf],
) -> Result<Vec<PathBuf>, RestoreError> {
    let tracked_paths = index.tracked_files();
    let target_paths = target_map.keys().cloned().collect::<Vec<_>>();
    let candidates = pathspec_candidates(&target_paths, &tracked_paths, allowed_unmatched);
    ensure_positive_pathspecs_match(pathspecs, candidates)?;
    let mut paths = BTreeSet::new();
    paths.extend(filter_paths(&target_paths, pathspecs));
    paths.extend(filter_paths(&tracked_paths, pathspecs));
    Ok(paths.into_iter().collect())
}

fn filter_paths(paths: &[PathBuf], pathspecs: &PathspecSet) -> Vec<PathBuf> {
    paths
        .iter()
        .filter(|path| pathspecs.matches_path(path))
        .cloned()
        .collect()
}

fn pathspec_candidates(
    target_paths: &[PathBuf],
    tracked_paths: &[PathBuf],
    allowed_unmatched: &[PathBuf],
) -> Vec<PathBuf> {
    let mut candidates =
        Vec::with_capacity(target_paths.len() + tracked_paths.len() + allowed_unmatched.len());
    candidates.extend(target_paths.iter().cloned());
    candidates.extend(tracked_paths.iter().cloned());
    candidates.extend(allowed_unmatched.iter().cloned());
    candidates
}

fn restore_match_candidates(
    target_map: &HashMap<PathBuf, RestoreTarget>,
    index: &Index,
    allowed_unmatched: &[PathBuf],
) -> Vec<PathBuf> {
    let target_paths = target_map.keys().cloned().collect::<Vec<_>>();
    let tracked_paths = index.tracked_files();
    pathspec_candidates(&target_paths, &tracked_paths, allowed_unmatched)
}

fn ensure_positive_pathspecs_match<I, P>(
    pathspecs: &PathspecSet,
    candidates: I,
) -> Result<(), RestoreError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    if let Some(unmatched) = pathspecs.unmatched_positive(candidates) {
        return Err(RestoreError::PathspecNotMatched(unmatched.to_string()));
    }
    Ok(())
}

fn tree_item_mode_to_index_mode(mode: TreeItemMode) -> Option<u32> {
    match mode {
        TreeItemMode::Blob => Some(0o100644),
        TreeItemMode::BlobExecutable => Some(0o100755),
        TreeItemMode::Link => Some(0o120000),
        TreeItemMode::Commit => Some(0o160000),
        TreeItemMode::Tree => None,
    }
}

fn index_mode_to_tree_item_mode(mode: u32) -> Option<TreeItemMode> {
    match mode {
        0o100644 => Some(TreeItemMode::Blob),
        0o100755 => Some(TreeItemMode::BlobExecutable),
        0o120000 => Some(TreeItemMode::Link),
        0o160000 => Some(TreeItemMode::Commit),
        _ => None,
    }
}

fn index_entry_from_target(path: String, target: RestoreTarget, size: u32) -> IndexEntry {
    let mut entry = IndexEntry::new_from_blob(path, target.hash, size);
    entry.mode = target.index_mode();
    entry
}

fn worktree_path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn worktree_mode_matches(path: &Path, mode: Option<TreeItemMode>) -> Result<bool, RestoreError> {
    let Some(mode) = mode else {
        return Ok(true);
    };
    let metadata = fs::symlink_metadata(path).map_err(|_| RestoreError::ReadWorktree)?;
    let file_type = metadata.file_type();
    Ok(match mode {
        TreeItemMode::Link => file_type.is_symlink(),
        TreeItemMode::Blob | TreeItemMode::BlobExecutable => !file_type.is_symlink(),
        TreeItemMode::Commit | TreeItemMode::Tree => true,
    })
}

#[cfg(unix)]
fn apply_worktree_target_mode(path: &Path, mode: Option<TreeItemMode>) -> Result<(), RestoreError> {
    use std::os::unix::fs::PermissionsExt;

    let Some(mode) = mode else {
        return Ok(());
    };
    let Some(mode) = (match mode {
        TreeItemMode::Blob => Some(0o644),
        TreeItemMode::BlobExecutable => Some(0o755),
        _ => None,
    }) else {
        return Ok(());
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|_| RestoreError::WriteWorktree)
}

#[cfg(not(unix))]
fn apply_worktree_target_mode(
    _path: &Path,
    _mode: Option<TreeItemMode>,
) -> Result<(), RestoreError> {
    Ok(())
}

fn path_to_utf8(path: &Path) -> io::Result<&str> {
    path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("non-UTF8 path: {}", path.display()),
        )
    })
}

fn path_to_utf8_typed(path: &Path) -> Result<&str, RestoreError> {
    path.to_str().ok_or(RestoreError::InvalidPathEncoding)
}

fn pathspec_not_matched(path: &Path) -> RestoreError {
    RestoreError::PathspecNotMatched(path.display().to_string())
}

/// Collect every path that is unmerged in `index` (i.e. has an entry at conflict
/// stage 1, 2, or 3), de-duplicated and in first-seen order. Read-only.
fn collect_unmerged_paths(index: &Index) -> Vec<PathBuf> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for stage in 1u8..=3 {
        for entry in index.tracked_entries(stage) {
            if seen.insert(entry.name.clone()) {
                out.push(PathBuf::from(&entry.name));
            }
        }
    }
    out
}

/// Unmerged paths (stages 1/2/3) that match the requested `filter` pathspecs.
/// Empty when nothing is unmerged, so the common conflict-free path stays cheap.
fn collect_matched_unmerged_paths(pathspecs: &PathspecSet) -> Result<Vec<PathBuf>, RestoreError> {
    let index = Index::load(path::index()).map_err(|_| RestoreError::ReadIndex)?;
    let unmerged = collect_unmerged_paths(&index);
    if unmerged.is_empty() {
        return Ok(Vec::new());
    }
    Ok(filter_paths(&unmerged, pathspecs))
}

/// Restore target for `path` at conflict `stage` (2 = ours, 3 = theirs).
/// Returns `None` when the path has no such stage.
fn stage_target(index: &Index, path: &str, stage: u8) -> Option<RestoreTarget> {
    index
        .tracked_entries(stage)
        .into_iter()
        .find(|entry| entry.name == path)
        .map(|entry| RestoreTarget::new(entry.hash, index_mode_to_tree_item_mode(entry.mode)))
}

/// Blob OID for `path` at conflict `stage` (2 = ours, 3 = theirs).
fn stage_blob(index: &Index, path: &str, stage: u8) -> Option<ObjectHash> {
    stage_target(index, path, stage).map(|target| target.hash)
}

/// Restore the `stage` side (2 = ours, 3 = theirs) of each matched unmerged path
/// to the working tree. Reads conflict stages only and writes the worktree; the
/// index is intentionally left unmerged so `libra status` still shows the
/// conflict until the user stages a resolution.
///
/// When the requested stage is ABSENT for a matched path (a modify/delete
/// conflict — that side deleted the file), the behavior follows Git's overlay
/// mode, exactly like the rest of `restore`:
/// - default (no-overlay): the deleting side "wins", so the worktree file is
///   removed (idempotently) — restoring a deletion means deleting. Exit 0.
/// - `--overlay`: overlay never removes paths, so this is an error
///   ([`RestoreError::MissingStageVersion`], matching Git's overlay message).
///
/// Returns `(restored, deleted)` working-tree paths.
async fn restore_conflict_stage(
    pathspecs: &PathspecSet,
    stage: u8,
    overlay: bool,
) -> Result<(Vec<String>, Vec<String>), RestoreError> {
    let index = Index::load(path::index()).map_err(|_| RestoreError::ReadIndex)?;
    let unmerged = collect_unmerged_paths(&index);
    let matched = filter_paths(&unmerged, pathspecs);
    if matched.is_empty() {
        if let Some(unmatched) = pathspecs.unmatched_positive(&unmerged) {
            return Err(RestoreError::PathspecNotMatched(unmatched.to_string()));
        }
        return Err(RestoreError::PathspecNotMatched(String::new()));
    }

    let mut restored = Vec::new();
    let mut deleted = Vec::new();
    for path in &matched {
        let path_str = path_to_utf8_typed(path)?;
        match stage_target(&index, path_str, stage) {
            Some(target) => {
                restore_target_to_file_typed(target, path).await?;
                restored.push(path.display().to_string());
            }
            None if overlay => {
                // Overlay mode never removes paths — Git errors here.
                return Err(RestoreError::MissingStageVersion {
                    path: path_str.to_string(),
                    stage,
                });
            }
            None => {
                // Default (no-overlay): the requested side deleted this file, so
                // restore it by removing it from the worktree. Idempotent — an
                // already-absent file is fine (Git exits 0 either way).
                let path_abs = util::workdir_to_absolute(path);
                if worktree_path_exists(&path_abs) {
                    fs::remove_file(&path_abs).map_err(|_| RestoreError::WriteWorktree)?;
                    util::clear_empty_dir(&path_abs);
                }
                deleted.push(path.display().to_string());
            }
        }
    }
    Ok((restored, deleted))
}

/// `restore --merge` / `--conflict=<style>`: for each matched unmerged path,
/// rebuild the conflict markers from the index stages (ours = stage 2, theirs =
/// stage 3, base = stage 1) and write them to the working tree, leaving the index
/// unmerged. `diff3` additionally emits the base block. Restore rebuilds the
/// markers independently from the index stages as a single whole-file `ours`
/// block / `theirs` block, with generic `ours`/`theirs` labels (the index stages
/// carry
/// no commit names) — not Git's line-level 3-way merge.
async fn restore_conflict_merge(
    pathspecs: &PathspecSet,
    diff3: bool,
) -> Result<Vec<String>, RestoreError> {
    let index = Index::load(path::index()).map_err(|_| RestoreError::ReadIndex)?;
    let unmerged = collect_unmerged_paths(&index);
    let matched = filter_paths(&unmerged, pathspecs);
    if matched.is_empty() {
        if let Some(unmatched) = pathspecs.unmatched_positive(&unmerged) {
            return Err(RestoreError::PathspecNotMatched(unmatched.to_string()));
        }
        return Err(RestoreError::PathspecNotMatched(String::new()));
    }

    let eol = if cfg!(windows) { "\r\n" } else { "\n" };
    let mut restored = Vec::new();
    for path in &matched {
        let path_str = path_to_utf8_typed(path)?;
        let ours = stage_payload(&index, path_str, 2)?;
        let theirs = stage_payload(&index, path_str, 3)?;
        let base = if diff3 {
            stage_payload(&index, path_str, 1)?
        } else {
            None
        };
        let content =
            build_conflict_markers(ours.as_deref(), theirs.as_deref(), base.as_deref(), eol);
        let path_abs = util::workdir_to_absolute(path);
        if let Some(parent) = path_abs.parent() {
            fs::create_dir_all(parent).map_err(|_| RestoreError::WriteWorktree)?;
        }
        remove_existing_symlink(&path_abs)?;
        util::write_file(content.as_bytes(), &path_abs).map_err(|_| RestoreError::WriteWorktree)?;
        restored.push(path.display().to_string());
    }
    Ok(restored)
}

/// Load the blob content for a conflict stage, rendered for inclusion in a
/// conflict marker (UTF-8 text, or a `[binary content, N bytes]` placeholder —
/// matching `libra merge`'s `conflict_payload`). `None` when the stage is absent
/// (e.g. an add/add conflict has no base; a modify/delete conflict lacks a side).
fn stage_payload(index: &Index, path: &str, stage: u8) -> Result<Option<String>, RestoreError> {
    let Some(hash) = stage_blob(index, path, stage) else {
        return Ok(None);
    };
    let blob = load_object::<Blob>(&hash).map_err(|_| RestoreError::ReadObject)?;
    let rendered = match std::str::from_utf8(&blob.data) {
        Ok(text) => text.to_string(),
        Err(_) => format!("[binary content, {} bytes]", blob.data.len()),
    };
    Ok(Some(rendered))
}

/// Build conflict-marker content from the present stages: a single `<<<<<<<
/// ours` block, `=======`, a single `>>>>>>> theirs` block, with an optional
/// `||||||| base` block for `diff3`. A missing ours/theirs side is rendered as
/// `(deleted)`. This is an independent whole-file rebuild from the index stages
/// (one `ours` block / one `theirs` block, not Git's line-level 3-way hunks —
/// and unlike `libra merge`/`cherry-pick`, which now emit line-level hunks for
/// both-modified text conflicts), with the generic `ours`/`theirs`
/// rather than merge's `HEAD` / commit-abbrev — the index stages do not carry the
/// original commit names. (If the labels are ever unified, update both sites.)
fn build_conflict_markers(
    ours: Option<&str>,
    theirs: Option<&str>,
    base: Option<&str>,
    eol: &str,
) -> String {
    let mut out = String::new();
    match ours {
        Some(text) => {
            out.push_str(&format!("<<<<<<< ours{eol}{text}{eol}"));
        }
        None => out.push_str(&format!("<<<<<<< ours (deleted){eol}")),
    }
    if let Some(base_text) = base {
        out.push_str(&format!("||||||| base{eol}{base_text}{eol}"));
    }
    out.push_str(&format!("======={eol}"));
    match theirs {
        Some(text) => {
            out.push_str(&format!("{text}{eol}>>>>>>> theirs{eol}"));
        }
        None => out.push_str(&format!(">>>>>>> theirs (deleted){eol}")),
    }
    out
}

async fn restore_target_to_file_typed(
    target: RestoreTarget,
    path: &PathBuf,
) -> Result<(), RestoreError> {
    let blob = load_object::<Blob>(&target.hash).map_err(|_| RestoreError::ReadObject)?;
    let path_abs = util::workdir_to_absolute(path);
    if let Some(parent) = path_abs.parent() {
        fs::create_dir_all(parent).map_err(|_| RestoreError::WriteWorktree)?;
    }

    if matches!(target.mode, Some(TreeItemMode::Link)) {
        return write_worktree_symlink(&path_abs, &blob.data);
    }

    remove_existing_symlink(&path_abs)?;

    match lfs::parse_pointer_data(&blob.data) {
        Some((oid, size)) => {
            let lfs_obj_path = lfs::lfs_object_path(&oid);
            if lfs_obj_path.exists() {
                fs::copy(&lfs_obj_path, &path_abs).map_err(|_| RestoreError::WriteWorktree)?;
            } else {
                LFSClient::get()
                    .await
                    .map_err(|_| RestoreError::LfsDownload)?
                    .download_object(&oid, size, &path_abs, None)
                    .await
                    .map_err(|_| RestoreError::LfsDownload)?;
            }
        }
        None => {
            util::write_file(&blob.data, &path_abs).map_err(|_| RestoreError::WriteWorktree)?;
        }
    }

    Ok(())
}

fn remove_existing_symlink(path: &Path) -> Result<(), RestoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(path).map_err(|_| RestoreError::WriteWorktree)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(RestoreError::ReadWorktree),
    }
}

#[cfg(unix)]
fn write_worktree_symlink(path: &Path, target: &[u8]) -> Result<(), RestoreError> {
    use std::{
        ffi::OsStr,
        os::unix::{ffi::OsStrExt, fs::symlink},
    };

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            return Err(RestoreError::WriteWorktree);
        }
        Ok(_) => fs::remove_file(path).map_err(|_| RestoreError::WriteWorktree)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(RestoreError::WriteWorktree),
    }

    let target = Path::new(OsStr::from_bytes(target));
    symlink(target, path).map_err(|_| RestoreError::WriteWorktree)
}

#[cfg(not(unix))]
fn write_worktree_symlink(path: &Path, _target: &[u8]) -> Result<(), RestoreError> {
    Err(RestoreError::SymlinkUnsupported(path.display().to_string()))
}

/// Restore a blob to file.
/// If blob is an LFS pointer, download the actual file from LFS server.
/// - `path` : to workdir
pub async fn restore_to_file(hash: &ObjectHash, path: &PathBuf) -> io::Result<()> {
    let blob = Blob::load(hash);
    let path_abs = util::workdir_to_absolute(path);
    if let Some(parent) = path_abs.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::symlink_metadata(&path_abs)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        fs::remove_file(&path_abs)?;
    }
    match lfs::parse_pointer_data(&blob.data) {
        Some((oid, size)) => {
            let lfs_obj_path = lfs::lfs_object_path(&oid);
            if lfs_obj_path.exists() {
                fs::copy(&lfs_obj_path, &path_abs)?;
            } else {
                let client = LFSClient::get()
                    .await
                    .map_err(|e| io::Error::other(e.to_string()))?;
                if let Err(e) = client.download_object(&oid, size, &path_abs, None).await {
                    return Err(io::Error::other(e.to_string()));
                }
            }
        }
        None => {
            util::write_file(&blob.data, &path_abs)?;
        }
    }
    Ok(())
}

// ── Legacy worktree/index restore (kept for execute_checked) ─────────

pub async fn restore_worktree(
    filter: &[PathBuf],
    target_blobs: &[(PathBuf, ObjectHash)],
) -> io::Result<()> {
    let target_blobs = legacy_targets(target_blobs);
    let target_blobs = preprocess_blobs(&target_blobs);
    let index = Index::load(path::index()).map_err(|e| io::Error::other(e.to_string()))?;
    let raw_pathspecs = filter
        .iter()
        .map(|path| util::path_to_string(path))
        .collect::<Vec<_>>();
    let pathspecs = compile_restore_pathspecs(&raw_pathspecs)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    let file_paths = collect_restore_worktree_paths(&pathspecs, &target_blobs, &index, &[])
        .map_err(|error| io::Error::other(error.to_string()))?;
    for path_wd in &file_paths {
        let path_abs = util::workdir_to_absolute(path_wd);
        let path_wd_str = path_to_utf8(path_wd)?;
        let tracked = index.tracked(path_wd_str, 0);
        if !worktree_path_exists(&path_abs) {
            if let Some(target) = target_blobs.get(path_wd) {
                restore_target_to_file_typed(*target, path_wd)
                    .await
                    .map_err(|error| io::Error::other(error.to_string()))?;
            } else if !tracked {
                return Err(io::Error::other(format!(
                    "pathspec '{}' did not match any files",
                    path_wd.display()
                )));
            }
        } else if let Some(target) = target_blobs.get(path_wd) {
            let hash =
                calc_file_blob_hash(&path_abs).map_err(|e| io::Error::other(e.to_string()))?;
            let mode_matches = worktree_mode_matches(&path_abs, target.mode)
                .map_err(|error| io::Error::other(error.to_string()))?;
            if hash != target.hash || !mode_matches {
                restore_target_to_file_typed(*target, path_wd)
                    .await
                    .map_err(|error| io::Error::other(error.to_string()))?;
            } else {
                apply_worktree_target_mode(&path_abs, target.mode)
                    .map_err(|error| io::Error::other(error.to_string()))?;
            }
        } else if tracked {
            fs::remove_file(&path_abs)?;
            util::clear_empty_dir(&path_abs);
        }
    }
    Ok(())
}

pub fn restore_index(filter: &[PathBuf], target_blobs: &[(PathBuf, ObjectHash)]) -> io::Result<()> {
    let target_blobs = legacy_targets(target_blobs);
    let target_blobs = preprocess_blobs(&target_blobs);

    let idx_file = path::index();
    let mut index = Index::load(&idx_file).map_err(|e| io::Error::other(e.to_string()))?;
    let deleted_files_index = get_index_deleted_files_in_filters(&index, filter, &target_blobs)?;

    let filter_vec = filter.to_vec();
    let mut file_paths = util::filter_to_fit_paths(&index.tracked_files(), &filter_vec);
    file_paths.extend(deleted_files_index);

    for path in &file_paths {
        let path_str = path_to_utf8(path)?;
        if !index.tracked(path_str, 0) {
            if let Some(target) = target_blobs.get(path) {
                let blob = Blob::load(&target.hash);
                index.add(index_entry_from_target(
                    path_str.to_string(),
                    *target,
                    blob.data.len() as u32,
                ));
            } else {
                return Err(io::Error::other(format!(
                    "pathspec '{}' did not match any files",
                    path.display()
                )));
            }
        } else if let Some(target) = target_blobs.get(path) {
            if !index.verify_hash(path_str, 0, &target.hash) {
                let blob = Blob::load(&target.hash);
                index.update(index_entry_from_target(
                    path_str.to_string(),
                    *target,
                    blob.data.len() as u32,
                ));
            }
        } else {
            index.remove(path_str, 0);
        }
    }
    index
        .save(&idx_file)
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(())
}

fn get_index_deleted_files_in_filters(
    index: &Index,
    filters: &[PathBuf],
    target_blobs: &HashMap<PathBuf, RestoreTarget>,
) -> io::Result<HashSet<PathBuf>> {
    let mut deleted = HashSet::new();
    for path_wd in target_blobs.keys() {
        let path_wd_str = path_to_utf8(path_wd)?;
        let path_abs = util::workdir_to_absolute(path_wd);
        if !index.tracked(path_wd_str, 0) && util::is_sub_of_paths(path_abs, filters) {
            deleted.insert(path_wd.clone());
        }
    }
    Ok(deleted)
}

fn get_index_deleted_files_in_filters_typed(
    index: &Index,
    pathspecs: &PathspecSet,
    target_blobs: &HashMap<PathBuf, RestoreTarget>,
) -> Result<HashSet<PathBuf>, RestoreError> {
    let mut deleted = HashSet::new();
    for path_wd in target_blobs.keys() {
        let path_wd_str = path_to_utf8_typed(path_wd)?;
        if !index.tracked(path_wd_str, 0) && pathspecs.matches_path(path_wd) {
            deleted.insert(path_wd.clone());
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the `Display` format for every variant of [`RestoreError`].
    /// These strings are used as the `CliError` message via the
    /// `From<RestoreError> for CliError` mapping and surface in both
    /// human and `--json` envelopes for `restore` and the
    /// checkout-from-commit phase of `clone` / `switch`.
    ///
    /// Every variant carries either a static message or an explicit
    /// `{0}` field interpolation; none wrap an upstream source error,
    /// so all variants are pinned.
    #[test]
    fn restore_error_display_pins_each_variant() {
        assert_eq!(
            RestoreError::ResolveSource.to_string(),
            "failed to resolve checkout source",
        );
        assert_eq!(
            RestoreError::ReferenceNotCommit.to_string(),
            "reference is not a commit",
        );
        assert_eq!(
            RestoreError::PathspecNotMatched("src/missing.rs".to_string()).to_string(),
            "pathspec 'src/missing.rs' did not match any files",
        );
        assert_eq!(RestoreError::ReadIndex.to_string(), "failed to read index");
        assert_eq!(
            RestoreError::ReadObject.to_string(),
            "failed to read object",
        );
        assert_eq!(
            RestoreError::ReadWorktree.to_string(),
            "failed to read worktree",
        );
        assert_eq!(
            RestoreError::InvalidPathEncoding.to_string(),
            "invalid path encoding",
        );
        assert_eq!(
            RestoreError::WriteWorktree.to_string(),
            "failed to write worktree file",
        );
        assert_eq!(
            RestoreError::LfsDownload.to_string(),
            "failed to download LFS content",
        );
        assert_eq!(
            RestoreError::LockedSource("intent".to_string()).to_string(),
            "refusing to restore from locked branch 'intent'",
        );
        assert_eq!(
            RestoreError::LockedCurrentBranch("traces".to_string()).to_string(),
            "refusing to restore worktree while on locked branch 'traces'",
        );
        assert_eq!(
            RestoreError::PathUnmerged("src/conflict.rs".to_string()).to_string(),
            "path 'src/conflict.rs' is unmerged",
        );
        assert_eq!(
            RestoreError::MissingStageVersion {
                path: "src/conflict.rs".to_string(),
                stage: 2,
            }
            .to_string(),
            "path 'src/conflict.rs' does not have our version",
        );
        assert_eq!(
            RestoreError::MissingStageVersion {
                path: "src/conflict.rs".to_string(),
                stage: 3,
            }
            .to_string(),
            "path 'src/conflict.rs' does not have their version",
        );
        assert_eq!(
            RestoreError::UnsupportedConflictStyle("zdiff3".to_string()).to_string(),
            "unsupported conflict style 'zdiff3' (expected 'merge' or 'diff3')",
        );
        assert_eq!(
            RestoreError::SymlinkUnsupported("src/link".to_string()).to_string(),
            "symlink checkout is not supported on this platform: src/link",
        );
        assert_eq!(
            RestoreError::InvalidPathspec("unsupported pathspec magic".to_string()).to_string(),
            "unsupported pathspec magic",
        );
    }

    /// Pin the `stable_code()` mapping for every variant of
    /// [`RestoreError`]. JSON consumers branch on the
    /// [`StableErrorCode`] in the error envelope — three of the read
    /// variants share `IoReadFailed`, three of the target variants
    /// share `CliInvalidTarget`, and `LfsDownload` is the only
    /// network-coded variant in the family. A future refactor that
    /// reroutes any of them (for example flipping `LfsDownload` from
    /// `NetworkUnavailable` to `IoReadFailed`) silently changes
    /// client retry classification unless every variant has its own
    /// guard. Enumerate every variant so a new variant trips both
    /// this exhaustive list and the `stable_code()` impl's match.
    #[test]
    fn restore_error_stable_code_pins_each_variant() {
        assert_eq!(
            RestoreError::ResolveSource.stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            RestoreError::ReferenceNotCommit.stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            RestoreError::PathspecNotMatched("ignored".to_string()).stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            RestoreError::ReadIndex.stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            RestoreError::ReadObject.stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            RestoreError::ReadWorktree.stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            RestoreError::InvalidPathEncoding.stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            RestoreError::WriteWorktree.stable_code(),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            RestoreError::LfsDownload.stable_code(),
            StableErrorCode::NetworkUnavailable,
        );
        assert_eq!(
            RestoreError::LockedSource("ignored".to_string()).stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            RestoreError::LockedCurrentBranch("ignored".to_string()).stable_code(),
            StableErrorCode::ConflictOperationBlocked,
        );
        assert_eq!(
            RestoreError::PathUnmerged("ignored".to_string()).stable_code(),
            StableErrorCode::ConflictUnresolved,
        );
        assert_eq!(
            RestoreError::MissingStageVersion {
                path: "ignored".to_string(),
                stage: 2,
            }
            .stable_code(),
            StableErrorCode::ConflictUnresolved,
        );
        assert_eq!(
            RestoreError::UnsupportedConflictStyle("zdiff3".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            RestoreError::SymlinkUnsupported("ignored".to_string()).stable_code(),
            StableErrorCode::Unsupported,
        );
        assert_eq!(
            RestoreError::InvalidPathspec("ignored".to_string()).stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
    }

    /// Pin the externally-visible `--merge` / `--conflict` marker contract,
    /// including the deleted-side (modify/delete) and `diff3` base-block cases,
    /// independently of how a particular merge stages a conflict.
    #[test]
    fn build_conflict_markers_covers_present_deleted_and_diff3() {
        assert_eq!(
            build_conflict_markers(Some("A"), Some("B"), None, "\n"),
            "<<<<<<< ours\nA\n=======\nB\n>>>>>>> theirs\n",
        );
        assert_eq!(
            build_conflict_markers(Some("A"), Some("B"), Some("BASE"), "\n"),
            "<<<<<<< ours\nA\n||||||| base\nBASE\n=======\nB\n>>>>>>> theirs\n",
        );
        // Stage 2 (ours) missing — theirs modified, ours deleted.
        assert_eq!(
            build_conflict_markers(None, Some("B"), None, "\n"),
            "<<<<<<< ours (deleted)\n=======\nB\n>>>>>>> theirs\n",
        );
        // Stage 3 (theirs) missing — ours modified, theirs deleted.
        assert_eq!(
            build_conflict_markers(Some("A"), None, None, "\n"),
            "<<<<<<< ours\nA\n=======\n>>>>>>> theirs (deleted)\n",
        );
    }
}
