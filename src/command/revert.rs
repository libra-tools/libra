//! Implements the revert command by parsing targets, reversing commit changes into the index/worktree, and optionally creating a new commit.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::{
        index::{Index, IndexEntry},
        object::{
            ObjectTrait,
            blob::Blob,
            commit::Commit,
            tree::{Tree, TreeItemMode},
            types::ObjectType,
        },
    },
};
use serde::{Deserialize, Serialize};

use crate::{
    command::{commit::create_commit_signatures, editor, load_object, save_object},
    common_utils::{format_commit_msg, parse_commit_msg},
    internal::{branch::Branch, head::Head},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        object_ext::{BlobExt, TreeExt},
        output::{OutputConfig, emit_json_data},
        path,
        text::short_display_hash,
        util,
    },
};

const REVERT_EXAMPLES: &str = "\
EXAMPLES:
    libra revert HEAD                     Revert the most recent commit
    libra revert abc1234                  Revert a specific commit
    libra revert -n HEAD                  Revert without auto-committing
    libra revert -m 1 <merge>             Revert a merge commit relative to parent 1
    libra revert HEAD --edit              Edit the revert message in $EDITOR before committing
    libra revert HEAD --no-edit           Accept the default revert message (no editor)
    libra revert --json HEAD              Structured JSON output for agents";

// ── Typed error ──────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
enum RevertError {
    #[error("not a libra repository")]
    NotInRepo,

    #[error("you are in a 'detached HEAD' state; reverting is not allowed")]
    DetachedHead,

    #[error("failed to resolve commit reference '{0}'")]
    InvalidCommit(String),

    #[error("commit {0} is a merge but no -m option was given")]
    MainlineRequired(String),

    #[error("mainline was specified but commit {0} is not a merge")]
    MainlineForNonMerge(String),

    #[error("commit {commit} does not have a parent number {mainline} (it has {parents})")]
    InvalidMainline {
        commit: String,
        mainline: usize,
        parents: usize,
    },

    #[error("revert produced conflicts in: {paths}")]
    Conflicts { paths: String },

    #[error(
        "a revert is already in progress; finish it with 'libra revert --continue', skip the current commit with '--skip', or cancel with '--abort'"
    )]
    RevertInProgress,

    #[error("no revert in progress")]
    NoRevertInProgress,

    #[error("unresolved conflict markers remain in '{0}'")]
    UnresolvedConflicts(String),

    #[error("failed to access revert state: {0}")]
    StateIo(String),

    #[error("failed to load object: {0}")]
    LoadObject(String),

    #[error("failed to save object: {0}")]
    SaveObject(String),

    #[error("failed to write worktree: {0}")]
    WriteWorktree(String),

    #[error("failed to save index: {0}")]
    IndexSave(String),

    #[error("failed to update HEAD: {0}")]
    UpdateHead(String),

    #[error("failed to resolve committer identity for --signoff: {0}")]
    Signoff(String),

    #[error("failed to resolve commit identity: {0}")]
    Identity(String),

    #[error("{0}")]
    MultiCommitUnsupported(String),

    #[error("Aborting revert due to empty commit message")]
    EmptyMessage,

    #[error("{0}")]
    Editor(String),
}

/// `--edit`: open the configured editor on `initial`, returning the edited
/// message with `#` comment lines stripped and surrounding blank lines trimmed.
/// Errors if no editor is configured, the editor fails, or the result is empty.
async fn edit_revert_message(initial: &str) -> Result<String, RevertError> {
    let Some(editor_cmd) = editor::resolve_editor().await else {
        return Err(RevertError::Editor(
            "no editor configured for --edit; set $GIT_EDITOR, core.editor, $VISUAL, or $EDITOR"
                .to_string(),
        ));
    };
    let path = util::storage_path().join("REVERT_EDITMSG");
    let raw = editor::edit_message(&path, initial, &editor_cmd, true)
        .await
        .map_err(|e| RevertError::Editor(e.to_string()))?;
    let cleaned = raw
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return Err(RevertError::EmptyMessage);
    }
    Ok(cleaned.to_string())
}

/// Build the `Signed-off-by` trailer (prefixed with a blank line) for
/// `revert --signoff`, using the canonical committer identity. Returns an empty
/// string when `signoff` is false.
async fn signoff_trailer(signoff: bool) -> Result<String, RevertError> {
    if !signoff {
        return Ok(String::new());
    }
    let identity = crate::command::commit::resolve_committer_identity()
        .await
        .map_err(|e| RevertError::Signoff(e.to_string()))?;
    Ok(format!(
        "\n\nSigned-off-by: {} <{}>",
        identity.name, identity.email
    ))
}

impl RevertError {
    fn stable_code(&self) -> StableErrorCode {
        match self {
            Self::NotInRepo => StableErrorCode::RepoNotFound,
            Self::DetachedHead => StableErrorCode::RepoStateInvalid,
            Self::InvalidCommit(_) => StableErrorCode::CliInvalidTarget,
            Self::MainlineRequired(_)
            | Self::MainlineForNonMerge(_)
            | Self::InvalidMainline { .. } => StableErrorCode::CliInvalidArguments,
            Self::LoadObject(_) => StableErrorCode::IoReadFailed,
            Self::SaveObject(_) => StableErrorCode::IoWriteFailed,
            Self::WriteWorktree(_) => StableErrorCode::IoWriteFailed,
            Self::IndexSave(_) => StableErrorCode::IoWriteFailed,
            Self::UpdateHead(_) => StableErrorCode::IoWriteFailed,
            Self::Signoff(_) => StableErrorCode::CliInvalidArguments,
            Self::Identity(_) => StableErrorCode::AuthMissingCredentials,
            Self::MultiCommitUnsupported(_) => StableErrorCode::CliInvalidArguments,
            Self::Conflicts { .. } | Self::UnresolvedConflicts(_) => {
                StableErrorCode::ConflictUnresolved
            }
            Self::RevertInProgress | Self::NoRevertInProgress => StableErrorCode::RepoStateInvalid,
            Self::StateIo(_) => StableErrorCode::IoWriteFailed,
            Self::EmptyMessage | Self::Editor(_) => StableErrorCode::CliInvalidArguments,
        }
    }
}

impl From<RevertError> for CliError {
    fn from(error: RevertError) -> Self {
        let stable_code = error.stable_code();
        let message = error.to_string();
        match error {
            RevertError::NotInRepo => CliError::repo_not_found(),
            RevertError::DetachedHead => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("switch to a branch first with 'libra switch <branch>'"),
            RevertError::InvalidCommit(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("use 'libra log' to find valid commit references"),
            // Mainline usage errors mirror Git's exit 128 (the Cli category
            // would otherwise default to 129), so override explicitly.
            RevertError::MainlineRequired(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_exit_code(128)
                .with_hint("pass '-m <parent-number>' (e.g. '-m 1') to revert a merge commit"),
            RevertError::MainlineForNonMerge(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_exit_code(128)
                .with_hint("'-m' is only valid when reverting a merge commit"),
            RevertError::InvalidMainline { .. } => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_exit_code(128)
                .with_hint("choose a parent number within the merge commit's parent count"),
            RevertError::Conflicts { .. } => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint("resolve the conflicts, then run 'libra revert --continue'")
                .with_hint("or skip this commit with 'libra revert --skip'")
                .with_hint("or cancel with 'libra revert --abort'"),
            _ => CliError::fatal(message).with_stable_code(stable_code),
        }
    }
}

// ── Structured output ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct RevertOutput {
    pub reverted_commit: String,
    pub short_reverted: String,
    pub new_commit: Option<String>,
    pub short_new: Option<String>,
    pub no_commit: bool,
    pub files_changed: usize,
}

// ── Entry points ─────────────────────────────────────────────────────

/// Arguments for the revert command.
/// Reverts the specified commit by creating a new commit that undoes the changes.
#[derive(Parser, Debug)]
#[command(about = "Revert some existing commits")]
#[command(after_help = REVERT_EXAMPLES)]
pub struct RevertArgs {
    /// Commits to revert, in order (commit hash, branch name, or HEAD). Multiple
    /// commits are reverted sequentially, each as its own revert commit.
    #[clap(required_unless_present_any = ["continue_revert", "abort", "skip"], num_args = 0..)]
    pub commit: Vec<String>,

    /// Continue an in-progress revert after resolving conflicts.
    #[clap(long = "continue", conflicts_with_all = ["abort", "skip", "no_commit", "mainline"])]
    pub continue_revert: bool,

    /// Abort an in-progress revert and restore the pre-revert state.
    #[clap(long, conflicts_with_all = ["continue_revert", "skip", "no_commit", "mainline"])]
    pub abort: bool,

    /// Skip the current (conflicted) commit and continue the revert sequence
    /// with the remaining commits, discarding the current commit's changes.
    #[clap(long, conflicts_with_all = ["continue_revert", "abort", "no_commit", "mainline"])]
    pub skip: bool,

    /// Don't automatically commit the revert, just stage the changes
    #[clap(short = 'n', long)]
    pub no_commit: bool,

    /// Parent number (1-based) to treat as the mainline when reverting a merge
    /// commit. Required for merge commits; rejected for non-merge commits.
    #[clap(short = 'm', long, value_name = "parent-number")]
    pub mainline: Option<usize>,

    /// Add a `Signed-off-by` trailer (using the committer identity) to the
    /// revert commit message.
    #[clap(short = 's', long)]
    pub signoff: bool,

    /// Open the editor on the auto-generated revert message before committing
    /// (`$GIT_EDITOR` / `core.editor` / `$VISUAL` / `$EDITOR`), like
    /// `git revert --edit`. Unlike Git, Libra's revert does NOT open an editor
    /// by default; pass `--edit` to opt in. Mutually exclusive with `--no-edit`.
    #[clap(short = 'e', long, conflicts_with = "no_edit")]
    pub edit: bool,

    /// Accept the auto-generated revert message without launching an editor.
    /// This is Libra's default behavior, so the flag is a no-op accepted for Git
    /// parity; pass `-e`/`--edit` to open the editor instead.
    #[clap(long = "no-edit")]
    pub no_edit: bool,

    /// Do not update the rerere (reuse recorded resolution) index. Accepted for
    /// Git parity and is a no-op: `libra rerere` exists as a standalone command
    /// but is not yet auto-integrated into revert, so there is nothing to update
    /// here. (Git's `--rerere-autoupdate` is not exposed.)
    #[clap(long = "no-rerere-autoupdate")]
    pub no_rerere_autoupdate: bool,
}

pub async fn execute(args: RevertArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Reverses one or more commits by replaying their inverse
/// changes into the index/worktree and optionally creating new commits.
pub async fn execute_safe(args: RevertArgs, output: &OutputConfig) -> CliResult<()> {
    crate::command::ensure_main_worktree("revert")?;
    // Symmetric sequencer mutex (lore.md 2.6): refuse a NEW revert while any
    // OTHER sequence is unresolved. Control verbs are exempt; same-op falls
    // through to run_revert's own RevertInProgress check.
    if !(args.abort || args.continue_revert || args.skip) {
        crate::internal::sequencer::ensure_none_in_progress(
            crate::internal::sequencer::SequenceKind::Revert,
        )
        .await?;
    }
    let result = run_revert(args).await.map_err(CliError::from)?;
    render_revert_output(&result, output)
}

// ── Core execution ───────────────────────────────────────────────────

async fn run_revert(args: RevertArgs) -> Result<RevertOutput, RevertError> {
    util::require_repo().map_err(|_| RevertError::NotInRepo)?;

    // Sequencer control verbs short-circuit the normal revert path.
    if args.abort {
        return run_revert_abort().await;
    }
    if args.continue_revert {
        return run_revert_continue().await;
    }
    if args.skip {
        return run_revert_skip().await;
    }

    // A conflicted revert must be resolved (`--continue`) or unwound (`--abort`)
    // before a new revert can start.
    if RevertState::load_optional()?.is_some() {
        return Err(RevertError::RevertInProgress);
    }

    if let Head::Detached(_) = Head::current().await {
        return Err(RevertError::DetachedHead);
    }

    // `--no-commit` and `-m` operate on a single revert; combining them with
    // multiple commits would need a full multi-commit sequencer, so reject the
    // combination rather than silently misbehave.
    if args.commit.len() > 1 {
        if args.no_commit {
            return Err(RevertError::MultiCommitUnsupported(
                "--no-commit cannot be combined with multiple commits".to_string(),
            ));
        }
        if args.mainline.is_some() {
            return Err(RevertError::MultiCommitUnsupported(
                "-m/--mainline cannot be combined with multiple commits".to_string(),
            ));
        }
    }

    // Resolve every commit spec to a stable commit ID up front (failing fast on
    // a bad ref before any revert is applied). The pending queue persisted on a
    // conflict then holds commit IDs, not refs — so a branch/`HEAD` that moves
    // while the revert is paused cannot make `--continue`/`--skip` revert a
    // different commit than the one originally named (matching Git's sequencer).
    let mut resolved: Vec<ObjectHash> = Vec::with_capacity(args.commit.len());
    for spec in &args.commit {
        let id = resolve_commit(spec)
            .await
            .map_err(|_| RevertError::InvalidCommit(spec.clone()))?;
        resolved.push(id);
    }

    // Revert each commit in order; each revert is applied relative to (and
    // committed onto) the HEAD produced by the previous one. A conflict stops the
    // sequence and records state (with the still-pending commit IDs) so
    // `--continue`/`--skip` can finish the rest.
    let params = RevertParams {
        mainline: args.mainline,
        no_commit: args.no_commit,
        signoff: args.signoff,
        edit: args.edit,
    };
    let outcome = revert_sequence(&resolved, &params, None, 0).await?;
    // A clean run leaves no in-progress state to clear (cleanup is a no-op then).
    RevertState::cleanup()?;
    // INVARIANT: control reached here only past the abort/continue/skip
    // short-circuits, so clap's `required_unless_present_any` guarantees
    // `args.commit` (and thus `resolved`) is non-empty; `revert_sequence` over a
    // non-empty slice always yields `Some` (each iteration sets `last` or returns
    // early).
    let (commit_str, revert_commit_id, total_files_changed) =
        outcome.expect("non-empty commit list yields a sequence result");
    Ok(RevertOutput {
        reverted_commit: commit_str.clone(),
        short_reverted: short_display_hash(&commit_str).to_string(),
        new_commit: revert_commit_id.as_ref().map(|id| id.to_string()),
        short_new: revert_commit_id
            .as_ref()
            .map(|id| short_display_hash(&id.to_string()).to_string()),
        no_commit: args.no_commit,
        files_changed: total_files_changed,
    })
}

/// Revert the already-resolved commit `ids` in order against the current HEAD
/// using `params`. `seed` is the already-completed
/// `(reverted_commit, revert_commit_id)` from a resumed sequence (so an empty
/// `ids` after `--continue` still reports a result), and `seed_files_changed`
/// accumulates onto it.
///
/// The queue carries resolved [`ObjectHash`] values (never re-run through the ref
/// resolver) so a branch/tag that moves while the revert is paused cannot
/// redirect the operation. On the first conflict, persists [`RevertState`]
/// carrying the **still-pending** commit IDs (as hex strings) as `remaining` and
/// returns `Conflicts`. On full success returns
/// `Some((last_reverted, last_revert_commit, total_files_changed))`, or `None`
/// only when both `ids` and `seed` are empty.
/// Parse the persisted `remaining` queue (hex commit IDs) back into
/// [`ObjectHash`] values WITHOUT going through the ref resolver, so a branch/tag
/// created with the same name as a stored hash cannot redirect a resumed revert.
fn parse_remaining_ids(remaining: &[String]) -> Result<Vec<ObjectHash>, RevertError> {
    remaining
        .iter()
        .map(|s| {
            ObjectHash::from_str(s).map_err(|e| {
                RevertError::LoadObject(format!(
                    "invalid pending commit id '{s}' in revert state: {e}"
                ))
            })
        })
        .collect()
}

async fn revert_sequence(
    ids: &[ObjectHash],
    params: &RevertParams,
    seed: Option<(String, Option<ObjectHash>)>,
    seed_files_changed: usize,
) -> Result<Option<(String, Option<ObjectHash>, usize)>, RevertError> {
    let mut last = seed;
    let mut total_files_changed = seed_files_changed;
    for (i, commit_id) in ids.iter().enumerate() {
        let orig_head = Head::current_commit().await.map(|h| h.to_string());
        match revert_single_commit(commit_id, params).await? {
            SingleRevertOutcome::Committed {
                revert_commit_id,
                files_changed,
            } => {
                total_files_changed += files_changed;
                last = Some((commit_id.to_string(), revert_commit_id));
            }
            SingleRevertOutcome::Conflicted { conflicted_paths } => {
                if let Some(orig_head) = orig_head {
                    RevertState {
                        orig_head,
                        reverted_commit: commit_id.to_string(),
                        signoff: params.signoff,
                        edit: params.edit,
                        remaining: ids[(i + 1)..].iter().map(|h| h.to_string()).collect(),
                        conflicted_paths: conflicted_paths.clone(),
                    }
                    .save()?;
                }
                return Err(RevertError::Conflicts {
                    paths: conflicted_paths.join(", "),
                });
            }
        }
    }
    Ok(last.map(|(c, id)| (c, id, total_files_changed)))
}

/// `revert --continue`: require every conflict resolved (no markers staged),
/// then create the revert commit from the resolved index and clear the state.
async fn run_revert_continue() -> Result<RevertOutput, RevertError> {
    let state = RevertState::load_optional()?.ok_or(RevertError::NoRevertInProgress)?;

    // Refuse to finish while conflict markers remain in any *staged* file (the
    // index is what gets committed, so the user must resolve and re-`add`).
    let index = Index::load(path::index()).map_err(|e| RevertError::IndexSave(e.to_string()))?;
    for path in index.tracked_files() {
        let Some(key) = path.to_str() else { continue };
        if let Some(hash) = index.get_hash(key, 0)
            && let Ok(blob) = load_object::<Blob>(&hash)
            && String::from_utf8_lossy(&blob.data).contains(CONFLICT_MARKER)
        {
            return Err(RevertError::UnresolvedConflicts(path.display().to_string()));
        }
    }

    let orig_head = ObjectHash::from_str(&state.orig_head)
        .map_err(|e| RevertError::LoadObject(e.to_string()))?;
    let reverted_commit_id = ObjectHash::from_str(&state.reverted_commit)
        .map_err(|e| RevertError::LoadObject(e.to_string()))?;

    // Build the revert commit from the (resolved) index tree.
    let tree_items: std::collections::HashMap<PathBuf, ObjectHash> = index
        .tracked_files()
        .into_iter()
        .filter_map(|p| {
            let key = p.to_str()?;
            index.get_hash(key, 0).map(|h| (p.clone(), h))
        })
        .collect();
    let files_changed = tree_items.len();
    let tree_id = build_tree_from_map(tree_items).await?;
    let message = resolve_revert_message(&reverted_commit_id, state.signoff, state.edit).await?;
    let revert_commit_id = create_revert_commit(&orig_head, &tree_id, &message).await?;

    // The conflicted commit is now finished. Clear its state BEFORE draining the
    // rest of the sequence: if a remaining commit fails with a non-conflict error
    // (bad ref, merge without `-m`, editor failure), we must not leave stale
    // state pointing at the already-committed conflict (which a retry would
    // re-process). A remaining *conflict* re-saves fresh state inside
    // `revert_sequence`; a clean drain leaves no state behind.
    RevertState::cleanup()?;
    let remaining = parse_remaining_ids(&state.remaining)?;
    let params = RevertParams::for_sequence(state.signoff, state.edit);
    let seed = Some((state.reverted_commit.clone(), Some(revert_commit_id)));
    let outcome = revert_sequence(&remaining, &params, seed, files_changed).await?;

    // INVARIANT: `seed` is `Some` (the just-finished conflict commit), so
    // `revert_sequence` initialises `last` to `Some` and always returns `Some`.
    let (commit_str, last_revert_commit, total_files_changed) =
        outcome.expect("seeded sequence result is always populated");
    Ok(RevertOutput {
        reverted_commit: commit_str.clone(),
        short_reverted: short_display_hash(&commit_str).to_string(),
        new_commit: last_revert_commit.as_ref().map(|id| id.to_string()),
        short_new: last_revert_commit
            .as_ref()
            .map(|id| short_display_hash(&id.to_string()).to_string()),
        no_commit: false,
        files_changed: total_files_changed,
    })
}

/// `revert --skip`: discard the current (conflicted) commit's partial changes by
/// restoring the working tree/index to the commit's start point, then continue
/// the sequence with the remaining commits.
async fn run_revert_skip() -> Result<RevertOutput, RevertError> {
    let state = RevertState::load_optional()?.ok_or(RevertError::NoRevertInProgress)?;

    // HEAD is already at `orig_head` (the conflict stopped before committing), so
    // restoring the index/worktree to its tree drops the conflict markers.
    restore_to_orig_head(&state.orig_head).await?;

    // Clear the skipped commit's state before draining the rest, so a non-conflict
    // error among the remaining commits cannot leave stale state (see
    // `run_revert_continue`). A remaining conflict re-saves fresh state.
    RevertState::cleanup()?;

    if state.remaining.is_empty() {
        // Nothing left after the skipped commit: the sequence is complete.
        let commit_str = state.reverted_commit.clone();
        return Ok(RevertOutput {
            reverted_commit: commit_str.clone(),
            short_reverted: short_display_hash(&commit_str).to_string(),
            new_commit: None,
            short_new: None,
            no_commit: false,
            files_changed: 0,
        });
    }

    let remaining = parse_remaining_ids(&state.remaining)?;
    let params = RevertParams::for_sequence(state.signoff, state.edit);
    let outcome = revert_sequence(&remaining, &params, None, 0).await?;

    // INVARIANT: the empty-`remaining` case returned above, so `remaining`
    // is non-empty here and `revert_sequence` over a non-empty slice yields `Some`.
    let (commit_str, last_revert_commit, total_files_changed) =
        outcome.expect("non-empty remaining yields a sequence result");
    Ok(RevertOutput {
        reverted_commit: commit_str.clone(),
        short_reverted: short_display_hash(&commit_str).to_string(),
        new_commit: last_revert_commit.as_ref().map(|id| id.to_string()),
        short_new: last_revert_commit
            .as_ref()
            .map(|id| short_display_hash(&id.to_string()).to_string()),
        no_commit: false,
        files_changed: total_files_changed,
    })
}

/// Reset the index and working tree to `orig_head`'s tree and point HEAD there.
/// Shared by `--abort` (which then clears state) and `--skip` (which then
/// continues with the remaining commits). HEAD is already `orig_head` during a
/// conflict, so the `update_head` is a no-op for `--skip` and the reset target
/// for `--abort`.
async fn restore_to_orig_head(orig_head_str: &str) -> Result<(), RevertError> {
    let orig_head =
        ObjectHash::from_str(orig_head_str).map_err(|e| RevertError::LoadObject(e.to_string()))?;
    let commit: Commit =
        load_object(&orig_head).map_err(|e| RevertError::LoadObject(e.to_string()))?;
    let tree: Tree =
        load_object(&commit.tree_id).map_err(|e| RevertError::LoadObject(e.to_string()))?;
    let mut new_index = Index::new();
    rebuild_index_from_tree(&tree, &mut new_index, "")?;
    let current_index = Index::load(path::index()).unwrap_or_else(|_| Index::new());
    reset_workdir_safely(&current_index, &new_index)?;
    new_index
        .save(path::index())
        .map_err(|e| RevertError::IndexSave(e.to_string()))?;
    update_head(orig_head_str).await
}

/// `revert --abort`: reset HEAD/index/worktree to the pre-revert commit and clear
/// the state.
async fn run_revert_abort() -> Result<RevertOutput, RevertError> {
    let state = RevertState::load_optional()?.ok_or(RevertError::NoRevertInProgress)?;
    restore_to_orig_head(&state.orig_head).await?;
    RevertState::cleanup()?;

    let commit_str = state.reverted_commit.clone();
    Ok(RevertOutput {
        reverted_commit: commit_str.clone(),
        short_reverted: short_display_hash(&commit_str).to_string(),
        new_commit: None,
        short_new: None,
        no_commit: false,
        files_changed: 0,
    })
}

// ── Rendering ────────────────────────────────────────────────────────

fn render_revert_output(result: &RevertOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("revert", result, output);
    }

    if output.quiet {
        return Ok(());
    }

    if let Some(short_new) = &result.short_new {
        println!("[{}] Revert commit {}", short_new, result.short_reverted,);
    } else if result.no_commit {
        // `-n`/`--no-commit`: the inverse changes are staged for a manual commit.
        println!("Changes staged for revert. Use 'libra commit' to finalize.");
    } else {
        // A control verb (`--abort`, or `--skip` with nothing left in the
        // sequence) finished without creating a revert commit.
        println!("Revert finished; no revert commit created.");
    }
    Ok(())
}

// ── Conflict-sequencer state ─────────────────────────────────────────

/// Conflict marker that introduces the "ours" side of a 3-way conflict.
const CONFLICT_MARKER: &str = "<<<<<<<";

/// Persisted state for an in-progress conflicted revert (`.libra/revert-state.json`),
/// mirroring merge's file-based state so `revert --continue`/`--abort` can finish
/// or unwind the revert.
#[derive(Debug, Serialize, Deserialize)]
struct RevertState {
    /// HEAD at the time the revert started — the `--abort` reset target and the
    /// parent of the eventual revert commit.
    orig_head: String,
    /// The commit being reverted (for the `--continue` revert message).
    reverted_commit: String,
    /// Whether `--signoff` was requested, so `--continue` reproduces the trailer.
    signoff: bool,
    /// Whether `--edit` was requested, so `--continue` opens the editor too.
    #[serde(default)]
    edit: bool,
    /// Commit specs still to revert after the current (conflicted) one — drained
    /// by `--continue`/`--skip`. Empty when the conflict was the last in the
    /// sequence. `#[serde(default)]` keeps older state files loadable.
    #[serde(default)]
    remaining: Vec<String>,
    /// Paths left with conflict markers for the user to resolve.
    conflicted_paths: Vec<String>,
}

impl RevertState {
    fn path() -> PathBuf {
        util::storage_path().join("revert-state.json")
    }

    fn load_optional() -> Result<Option<Self>, RevertError> {
        let path = Self::path();
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path).map_err(|e| RevertError::StateIo(e.to_string()))?;
        serde_json::from_str(&data)
            .map(Some)
            .map_err(|e| RevertError::StateIo(e.to_string()))
    }

    fn save(&self) -> Result<(), RevertError> {
        let path = Self::path();
        let data =
            serde_json::to_vec_pretty(self).map_err(|e| RevertError::StateIo(e.to_string()))?;
        // Atomic + fsynced write (lore.md §7.7): recovery-critical sequencer
        // state must never be left truncated by a crash.
        crate::utils::atomic_write::write_atomic(&path, &data, true)
            .map_err(|e| RevertError::StateIo(e.to_string()))
    }

    fn cleanup() -> Result<(), RevertError> {
        let path = Self::path();
        if path.exists() {
            fs::remove_file(&path).map_err(|e| RevertError::StateIo(e.to_string()))?;
        }
        Ok(())
    }
}

/// Result of reverting one commit against the current worktree.
enum SingleRevertOutcome {
    Committed {
        revert_commit_id: Option<ObjectHash>,
        files_changed: usize,
    },
    Conflicted {
        conflicted_paths: Vec<String>,
    },
}

/// Content-level 3-way merge for a path that diverged since the reverted commit:
/// base = the reverted commit's blob, ours = the current blob, theirs = the
/// parent's blob (the revert target). Returns the resulting blob hash and whether
/// it carries conflict markers.
fn three_way_revert_blob(
    reverted_hash: ObjectHash,
    current_hash: ObjectHash,
    parent_hash: Option<ObjectHash>,
) -> Result<(ObjectHash, bool), RevertError> {
    let reverted: Blob =
        load_object(&reverted_hash).map_err(|e| RevertError::LoadObject(e.to_string()))?;
    let current: Blob =
        load_object(&current_hash).map_err(|e| RevertError::LoadObject(e.to_string()))?;
    let parent_data = match parent_hash {
        Some(h) => {
            load_object::<Blob>(&h)
                .map_err(|e| RevertError::LoadObject(e.to_string()))?
                .data
        }
        None => Vec::new(),
    };
    let (bytes, conflicted) = match diffy::merge_bytes(&reverted.data, &current.data, &parent_data)
    {
        Ok(merged) => (merged, false),
        Err(conflicted) => (conflicted, true),
    };
    let blob = Blob::from_content_bytes(bytes);
    save_object(&blob, &blob.id).map_err(|e| RevertError::SaveObject(e.to_string()))?;
    Ok((blob.id, conflicted))
}

// ── Internal logic ───────────────────────────────────────────────────

/// The per-commit knobs that drive a single revert, decoupled from `RevertArgs`
/// so the same logic serves the initial run and the `--continue`/`--skip`
/// sequence drain (where `mainline`/`no_commit` never apply and `signoff`/`edit`
/// come from the persisted [`RevertState`]).
#[derive(Clone, Copy)]
struct RevertParams {
    mainline: Option<usize>,
    no_commit: bool,
    signoff: bool,
    edit: bool,
}

impl RevertParams {
    /// Knobs for draining a `--continue`/`--skip` sequence: always commit, never
    /// a mainline, carrying the original `--signoff`/`--edit` choices.
    fn for_sequence(signoff: bool, edit: bool) -> Self {
        Self {
            mainline: None,
            no_commit: false,
            signoff,
            edit,
        }
    }
}

async fn revert_single_commit(
    commit_id: &ObjectHash,
    params: &RevertParams,
) -> Result<SingleRevertOutcome, RevertError> {
    let reverted_commit: Commit =
        load_object(commit_id).map_err(|e| RevertError::LoadObject(e.to_string()))?;

    // Select the baseline parent to diff against. A merge commit (>1 parent)
    // requires `-m <n>` to pick the mainline; a non-merge commit rejects `-m`.
    // The generated revert still records a single parent (the current HEAD).
    let parents = &reverted_commit.parent_commit_ids;
    let parent_commit_id = match (parents.len(), params.mainline) {
        (0, None) => return revert_root_commit(params).await,
        (0, Some(_)) | (1, Some(_)) => {
            return Err(RevertError::MainlineForNonMerge(commit_id.to_string()));
        }
        (1, None) => parents[0],
        (_, None) => return Err(RevertError::MainlineRequired(commit_id.to_string())),
        (count, Some(mainline)) => {
            if mainline < 1 || mainline > count {
                return Err(RevertError::InvalidMainline {
                    commit: commit_id.to_string(),
                    mainline,
                    parents: count,
                });
            }
            parents[mainline - 1]
        }
    };

    let parent_commit: Commit =
        load_object(&parent_commit_id).map_err(|e| RevertError::LoadObject(e.to_string()))?;

    let current_head_commit_id = Head::current_commit()
        .await
        .ok_or_else(|| RevertError::LoadObject("could not get current HEAD commit".into()))?;
    let current_commit: Commit =
        load_object(&current_head_commit_id).map_err(|e| RevertError::LoadObject(e.to_string()))?;

    let current_tree: Tree =
        load_object(&current_commit.tree_id).map_err(|e| RevertError::LoadObject(e.to_string()))?;
    let reverted_tree: Tree = load_object(&reverted_commit.tree_id)
        .map_err(|e| RevertError::LoadObject(e.to_string()))?;
    let parent_tree: Tree =
        load_object(&parent_commit.tree_id).map_err(|e| RevertError::LoadObject(e.to_string()))?;

    let mut current_files: std::collections::HashMap<_, _> =
        current_tree.get_plain_items().into_iter().collect();
    let reverted_files: std::collections::HashMap<_, _> =
        reverted_tree.get_plain_items().into_iter().collect();
    let parent_files: std::collections::HashMap<_, _> =
        parent_tree.get_plain_items().into_iter().collect();

    let mut files_changed: usize = 0;
    let mut conflicted_paths: Vec<String> = Vec::new();

    for (path, &reverted_hash) in &reverted_files {
        let parent_hash = parent_files.get(path);

        if Some(&reverted_hash) == parent_hash {
            continue;
        }

        // A path that no longer matches the reverted commit diverged since: do a
        // content 3-way merge (base = reverted, ours = current, theirs = parent)
        // and record a conflict (with markers in the worktree) when both sides
        // touched overlapping regions.
        if current_files.get(path) != Some(&reverted_hash) && current_files.contains_key(path) {
            let current_hash = current_files[path];
            let (merged_hash, conflicted) =
                three_way_revert_blob(reverted_hash, current_hash, parent_hash.copied())?;
            current_files.insert(path.clone(), merged_hash);
            files_changed += 1;
            if conflicted {
                conflicted_paths.push(path.display().to_string());
            }
            continue;
        }

        if let Some(parent_hash) = parent_hash {
            if current_files.insert(path.clone(), *parent_hash) != Some(*parent_hash) {
                files_changed += 1;
            }
        } else if current_files.remove(path).is_some() {
            files_changed += 1;
        }
    }

    for (path, &parent_hash) in &parent_files {
        if !reverted_files.contains_key(path)
            && current_files.insert(path.clone(), parent_hash) != Some(parent_hash)
        {
            files_changed += 1;
        }
    }

    let final_tree_id = build_tree_from_map(current_files).await?;
    let final_tree: Tree =
        load_object(&final_tree_id).map_err(|e| RevertError::LoadObject(e.to_string()))?;

    // Resolve the (possibly `--edit`-ed) commit message BEFORE mutating the
    // working tree, so an editor failure on a clean revert leaves nothing
    // applied. Conflicts defer the message to `--continue`; `--no-commit` needs
    // no message.
    let prepared_message = if conflicted_paths.is_empty() && !params.no_commit {
        Some(resolve_revert_message(commit_id, params.signoff, params.edit).await?)
    } else {
        None
    };

    let mut new_index = Index::new();
    rebuild_index_from_tree(&final_tree, &mut new_index, "")?;
    let current_index = Index::load(path::index()).unwrap_or_else(|_| Index::new());
    reset_workdir_safely(&current_index, &new_index)?;
    new_index
        .save(path::index())
        .map_err(|e| RevertError::IndexSave(e.to_string()))?;

    // Conflicts: the index/worktree now hold the partially-reverted tree with
    // markers; stop before committing so the user can resolve and `--continue`.
    if !conflicted_paths.is_empty() {
        return Ok(SingleRevertOutcome::Conflicted { conflicted_paths });
    }

    let revert_commit_id = match prepared_message {
        Some(message) => {
            Some(create_revert_commit(&current_head_commit_id, &final_tree_id, &message).await?)
        }
        None => None, // `--no-commit`
    };
    Ok(SingleRevertOutcome::Committed {
        revert_commit_id,
        files_changed,
    })
}

async fn build_tree_from_map(
    files: std::collections::HashMap<PathBuf, ObjectHash>,
) -> Result<ObjectHash, RevertError> {
    fn build_subtree(
        paths: &std::collections::HashMap<PathBuf, ObjectHash>,
        current_dir: &PathBuf,
    ) -> Result<Tree, RevertError> {
        let mut tree_items = Vec::new();
        let mut subdirs = std::collections::HashMap::new();
        for (path, hash) in paths {
            if let Ok(relative_path) = path.strip_prefix(current_dir) {
                if relative_path.components().count() == 1 {
                    tree_items.push(git_internal::internal::object::tree::TreeItem {
                        mode: git_internal::internal::object::tree::TreeItemMode::Blob,
                        name: path_to_utf8(relative_path)?.to_string(),
                        id: *hash,
                    });
                } else {
                    let subdir_component = relative_path.components().next().ok_or_else(|| {
                        RevertError::LoadObject(format!(
                            "missing path component for {}",
                            path.display()
                        ))
                    })?;
                    let subdir = current_dir.join(subdir_component);
                    subdirs
                        .entry(subdir)
                        .or_insert_with(Vec::new)
                        .push((path.clone(), *hash));
                }
            }
        }
        for (subdir, subdir_files) in subdirs {
            let subdir_tree = build_subtree(&subdir_files.into_iter().collect(), &subdir)?;
            tree_items.push(git_internal::internal::object::tree::TreeItem {
                mode: git_internal::internal::object::tree::TreeItemMode::Tree,
                name: file_name_to_utf8(&subdir)?,
                id: subdir_tree.id,
            });
        }
        crate::utils::tree::sort_tree_items_for_git(&mut tree_items);
        Tree::from_tree_items(tree_items).map_err(|e| RevertError::SaveObject(e.to_string()))
    }

    let root_dir = PathBuf::new();
    let root_tree = build_subtree(&files, &root_dir)?;
    save_object(&root_tree, &root_tree.id).map_err(|e| RevertError::SaveObject(e.to_string()))?;
    Ok(root_tree.id)
}

async fn revert_root_commit(params: &RevertParams) -> Result<SingleRevertOutcome, RevertError> {
    let new_index = Index::new();
    let current_index = Index::load(path::index()).unwrap_or_else(|_| Index::new());
    let files_changed = current_index.tracked_files().len();

    // Resolve the HEAD + (possibly `--edit`-ed) message BEFORE clearing the
    // working tree, so an editor failure leaves nothing applied.
    let prepared = if params.no_commit {
        None
    } else {
        let current_head = Head::current_commit()
            .await
            .ok_or_else(|| RevertError::LoadObject("failed to resolve current HEAD".into()))?;
        let message = resolve_root_revert_message(params.signoff, params.edit).await?;
        Some((current_head, message))
    };

    reset_workdir_safely(&current_index, &new_index)?;
    new_index
        .save(path::index())
        .map_err(|e| RevertError::IndexSave(e.to_string()))?;

    // Reverting the root commit clears the tree entirely; there is no parent to
    // conflict against, so it always completes cleanly.
    let revert_commit_id = match prepared {
        Some((current_head, message)) => {
            Some(create_empty_revert_commit(&current_head, &message).await?)
        }
        None => None,
    };
    Ok(SingleRevertOutcome::Committed {
        revert_commit_id,
        files_changed,
    })
}

fn rebuild_index_from_tree(
    tree: &Tree,
    index: &mut Index,
    prefix: &str,
) -> Result<(), RevertError> {
    for item in &tree.tree_items {
        let full_path = if prefix.is_empty() {
            PathBuf::from(&item.name)
        } else {
            PathBuf::from(prefix).join(&item.name)
        };

        if let TreeItemMode::Tree = item.mode {
            let subtree: Tree =
                load_object(&item.id).map_err(|e| RevertError::LoadObject(e.to_string()))?;
            let full_path_str = full_path.to_str().ok_or_else(|| {
                RevertError::LoadObject(format!("failed to convert path to UTF-8: {full_path:?}"))
            })?;
            rebuild_index_from_tree(&subtree, index, full_path_str)?;
        } else {
            let blob = git_internal::internal::object::blob::Blob::load(&item.id);
            let entry = IndexEntry::new_from_blob(
                full_path
                    .to_str()
                    .ok_or_else(|| {
                        RevertError::LoadObject(format!(
                            "failed to convert path to UTF-8: {full_path:?}"
                        ))
                    })?
                    .to_string(),
                item.id,
                blob.data.len() as u32,
            );
            index.add(entry);
        }
    }
    Ok(())
}

fn reset_workdir_safely(current_index: &Index, new_index: &Index) -> Result<(), RevertError> {
    let workdir = util::working_dir();
    let new_tracked_paths: HashSet<_> = new_index.tracked_files().into_iter().collect();

    for path_buf in current_index.tracked_files() {
        if !new_tracked_paths.contains(&path_buf) {
            let full_path = workdir.join(path_buf);
            if full_path.exists() {
                fs::remove_file(&full_path).map_err(|e| {
                    RevertError::WriteWorktree(format!(
                        "failed to remove '{}': {e}",
                        full_path.display()
                    ))
                })?;
            }
        }
    }

    for path_buf in new_index.tracked_files() {
        let path_str = path_to_utf8(&path_buf)?;
        if let Some(entry) = new_index.get(path_str, 0) {
            let blob = git_internal::internal::object::blob::Blob::load(&entry.hash);
            let target_path = workdir.join(path_str);
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    RevertError::WriteWorktree(format!(
                        "failed to create directory '{}': {e}",
                        parent.display()
                    ))
                })?;
            }
            fs::write(&target_path, &blob.data).map_err(|e| {
                RevertError::WriteWorktree(format!(
                    "failed to write '{}': {e}",
                    target_path.display()
                ))
            })?;
        }
    }

    Ok(())
}

fn path_to_utf8(path: &Path) -> Result<&str, RevertError> {
    path.to_str().ok_or_else(|| {
        RevertError::LoadObject(format!("invalid path encoding: {}", path.display()))
    })
}

fn file_name_to_utf8(path: &Path) -> Result<String, RevertError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            RevertError::LoadObject(format!("invalid file name encoding: {}", path.display()))
        })
}

/// Build the default `Revert "<subject>"` message for a commit, plus the
/// `--signoff` trailer.
async fn build_revert_message(
    reverted_commit_id: &ObjectHash,
    signoff: bool,
) -> Result<String, RevertError> {
    let reverted_commit: Commit =
        load_object(reverted_commit_id).map_err(|e| RevertError::LoadObject(e.to_string()))?;
    let (message, _) = parse_commit_msg(&reverted_commit.message);
    let subject = message.lines().next().unwrap_or("").trim();
    Ok(format!(
        "Revert \"{}\"\n\nThis reverts commit {}.{}",
        subject,
        reverted_commit_id,
        signoff_trailer(signoff).await?
    ))
}

/// Resolve the final revert message: the default message, with the editor
/// applied when `edit` is set. Callers run this BEFORE mutating the working
/// tree so an editor failure (no editor / abort / empty) cannot leave a
/// half-applied revert behind.
async fn resolve_revert_message(
    reverted_commit_id: &ObjectHash,
    signoff: bool,
    edit: bool,
) -> Result<String, RevertError> {
    let message = build_revert_message(reverted_commit_id, signoff).await?;
    if edit {
        edit_revert_message(&message).await
    } else {
        Ok(message)
    }
}

async fn create_revert_commit(
    parent_id: &ObjectHash,
    tree_id: &ObjectHash,
    message: &str,
) -> Result<ObjectHash, RevertError> {
    let (author, committer, _identity) = create_commit_signatures(None, None)
        .await
        .map_err(|e| RevertError::Identity(e.to_string()))?;
    let commit = Commit::new(
        author,
        committer,
        *tree_id,
        vec![*parent_id],
        &format_commit_msg(message, None),
    );

    save_object(&commit, &commit.id).map_err(|e| RevertError::SaveObject(e.to_string()))?;
    update_head(&commit.id.to_string()).await?;
    Ok(commit.id)
}

/// The default root-revert message (`Revert root commit`) plus the `--signoff`
/// trailer, with the editor applied when `edit` is set. Resolved before the
/// working tree is mutated.
async fn resolve_root_revert_message(signoff: bool, edit: bool) -> Result<String, RevertError> {
    let message = format!(
        "Revert root commit\n\nThis reverts the initial commit.{}",
        signoff_trailer(signoff).await?
    );
    if edit {
        edit_revert_message(&message).await
    } else {
        Ok(message)
    }
}

async fn create_empty_revert_commit(
    parent_id: &ObjectHash,
    message: &str,
) -> Result<ObjectHash, RevertError> {
    // `Tree::from_tree_items` rejects an empty item list, but reverting a root
    // commit legitimately yields the empty tree (Git's well-known
    // 4b825dc642cb6eb9a060e54bf8d69288fbee4904). Build it directly from empty
    // bytes — `to_data()` serialises a zero-item tree fine — so the object is
    // stored under the canonical empty-tree id rather than erroring out.
    let empty_tree = empty_tree().map_err(RevertError::SaveObject)?;
    save_object(&empty_tree, &empty_tree.id).map_err(|e| RevertError::SaveObject(e.to_string()))?;

    let (author, committer, _identity) = create_commit_signatures(None, None)
        .await
        .map_err(|e| RevertError::Identity(e.to_string()))?;
    let commit = Commit::new(
        author,
        committer,
        empty_tree.id,
        vec![*parent_id],
        &format_commit_msg(message, None),
    );

    save_object(&commit, &commit.id).map_err(|e| RevertError::SaveObject(e.to_string()))?;
    update_head(&commit.id.to_string()).await?;
    Ok(commit.id)
}

/// Build Git's canonical empty tree (id `4b825dc642cb6eb9a060e54bf8d69288fbee4904`
/// under SHA-1). `Tree::from_tree_items` refuses a zero-length item list, so we
/// parse it from empty bytes instead — the resulting hash is computed from the
/// empty serialisation, matching what Git uses for an empty directory tree.
fn empty_tree() -> Result<Tree, String> {
    let empty_id = ObjectHash::from_type_and_data(ObjectType::Tree, &[]);
    Tree::from_bytes(&[], empty_id).map_err(|e| e.to_string())
}

async fn resolve_commit(reference: &str) -> Result<ObjectHash, String> {
    util::get_commit_base(reference).await
}

async fn update_head(commit_id: &str) -> Result<(), RevertError> {
    if let Head::Branch(name) = Head::current().await {
        Branch::update_branch(&name, commit_id, None)
            .await
            .map_err(|e| RevertError::UpdateHead(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the `Display` format for every variant of [`RevertError`].
    /// The strings are surfaced as the `CliError` message via
    /// `From<RevertError> for CliError` and appear in both the human
    /// and `--json` envelopes for `libra revert`. Variants that wrap a
    /// `{0}` `String` use an "ignored" payload — every template ends
    /// with the bare interpolation, so the surface prefix is enough
    /// to lock the contract.
    #[test]
    fn revert_error_display_pins_each_variant() {
        assert_eq!(RevertError::NotInRepo.to_string(), "not a libra repository",);
        assert_eq!(
            RevertError::DetachedHead.to_string(),
            "you are in a 'detached HEAD' state; reverting is not allowed",
        );
        assert_eq!(
            RevertError::InvalidCommit("deadbeef".to_string()).to_string(),
            "failed to resolve commit reference 'deadbeef'",
        );
        assert_eq!(
            RevertError::MainlineRequired("deadbeef".to_string()).to_string(),
            "commit deadbeef is a merge but no -m option was given",
        );
        assert_eq!(
            RevertError::MainlineForNonMerge("deadbeef".to_string()).to_string(),
            "mainline was specified but commit deadbeef is not a merge",
        );
        assert_eq!(
            RevertError::InvalidMainline {
                commit: "deadbeef".to_string(),
                mainline: 3,
                parents: 2,
            }
            .to_string(),
            "commit deadbeef does not have a parent number 3 (it has 2)",
        );
        assert_eq!(
            RevertError::Conflicts {
                paths: "src/main.rs".to_string(),
            }
            .to_string(),
            "revert produced conflicts in: src/main.rs",
        );
        assert_eq!(
            RevertError::LoadObject("ignored".to_string()).to_string(),
            "failed to load object: ignored",
        );
        assert_eq!(
            RevertError::SaveObject("ignored".to_string()).to_string(),
            "failed to save object: ignored",
        );
        assert_eq!(
            RevertError::WriteWorktree("ignored".to_string()).to_string(),
            "failed to write worktree: ignored",
        );
        assert_eq!(
            RevertError::IndexSave("ignored".to_string()).to_string(),
            "failed to save index: ignored",
        );
        assert_eq!(
            RevertError::UpdateHead("ignored".to_string()).to_string(),
            "failed to update HEAD: ignored",
        );
    }

    /// Pin the `stable_code()` mapping for every variant of
    /// [`RevertError`]. The [`StableErrorCode`] is what `--json`
    /// consumers read from the error envelope and branch on
    /// (e.g. `IoWriteFailed` is the retry-on-disk-failure code).
    /// Enumerate every variant explicitly so a future refactor that
    /// reroutes any variant — for example flipping `IndexSave` from
    /// `IoWriteFailed` to `IoReadFailed` — trips this guard rather
    /// than silently changing the wire surface.
    #[test]
    fn revert_error_stable_code_pins_each_variant() {
        assert_eq!(
            RevertError::NotInRepo.stable_code(),
            StableErrorCode::RepoNotFound,
        );
        assert_eq!(
            RevertError::DetachedHead.stable_code(),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            RevertError::InvalidCommit("deadbeef".to_string()).stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            RevertError::MainlineRequired("x".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            RevertError::MainlineForNonMerge("x".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            RevertError::InvalidMainline {
                commit: "x".to_string(),
                mainline: 3,
                parents: 2,
            }
            .stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            RevertError::Conflicts {
                paths: "ignored".to_string(),
            }
            .stable_code(),
            StableErrorCode::ConflictUnresolved,
        );
        assert_eq!(
            RevertError::LoadObject("ignored".to_string()).stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            RevertError::SaveObject("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            RevertError::WriteWorktree("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            RevertError::IndexSave("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            RevertError::UpdateHead("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
        // `--edit` failure modes both surface as invalid-arguments.
        assert_eq!(
            RevertError::EmptyMessage.stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            RevertError::Editor("no editor".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            RevertError::EmptyMessage.to_string(),
            "Aborting revert due to empty commit message"
        );
    }
}
