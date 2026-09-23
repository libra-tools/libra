//! Branch management subcommand (`libra branch`).
//!
//! Implements creation, deletion, listing, renaming, upstream tracking, and
//! current-branch reporting. The single [`run_branch`] entry inspects the
//! parsed [`BranchArgs`] and delegates to one of the `*_impl` helpers.
//!
//! Non-obvious responsibilities:
//! - Maps [`branch::BranchStoreError`] to the local [`BranchError`] domain so
//!   the CLI surface is decoupled from the storage layer; see
//!   `map_branch_store_error`.
//! - For deletes, walks reachable commits from HEAD via
//!   [`get_reachable_commits`] to detect "not fully merged" branches before
//!   permitting deletion (skipped under `-D`).
//! - Suggests near-matches via Levenshtein distance when the user names a
//!   missing branch.
//! - For listing, supports `--contains` / `--no-contains` commit filters
//!   that BFS-walk the commit graph from each branch tip.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::IsTerminal,
};

use clap::{ArgGroup, Parser};
use colored::Colorize;
use git_internal::{hash::ObjectHash, internal::object::commit::Commit};
use sea_orm::{ConnectionTrait, DbErr};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::{
    command::{get_target_commit, load_object, log::get_reachable_commits},
    info_println,
    internal::{
        ai::automation::{VCS_EVENT_POST_BRANCH, dispatch_current_repo_vcs_event_to_history},
        branch::{self, Branch},
        config::ConfigKv,
        db::get_db_conn_instance,
        head::Head,
        operation::{current_operation_id, store},
        workspace::RepoIdentity,
        worktree_scope::WorktreeScope,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode, emit_warning},
        output::{OutputConfig, emit_json_data},
        text::{levenshtein, short_display_hash},
        util::require_repo,
    },
};

/// Which branch namespace to enumerate during `libra branch -l`.
#[derive(Clone, Copy)]
pub enum BranchListMode {
    /// Only branches stored under `refs/heads/`.
    Local,
    /// Only branches stored under `refs/remotes/<remote>/`.
    Remote,
    /// Combined local + remote listing (`-a`).
    All,
}

const BRANCH_AFTER_HELP: &str = "\
NOTES:
    `libra branch diff [<base>] [<branch>]` is a Libra verb — tip-to-tip diff
    (see `libra branch diff --help`); `diff` is reserved as a branch name here.

    Libra's global --quiet suppresses the branch listing itself.
    This differs from `git branch --quiet`, which still prints the primary list.

EXAMPLES:
    libra branch feature-x                Create a branch from HEAD
    libra branch feature-x main           Create a branch from another branch
    libra branch --track t1 main          Create t1 tracking local main
    libra branch --track=inherit t3 t1    Copy t1's upstream onto t3
    libra branch --no-track t4 main       Create without tracking
    libra branch -d topic                 Delete a fully merged branch
    libra branch -D topic                 Force-delete a branch
    libra branch -c topic topic-backup    Copy a branch, keeping the original
    libra branch -u origin/main           Set upstream for the current branch
    libra branch -u main alpha            Point alpha at the local branch main
    libra branch --track t1 main          Create t1 tracking local branch main
    libra branch --track=inherit t3 t1    Create t3 copying t1's upstream
    libra branch --no-track t4 main       Create t4 without tracking
    libra branch --edit-description       Edit the current branch's description in an editor
    libra branch --merged main            List branches already merged into main
    libra branch --sort version:refname   List branches sorted by version-aware name
    libra branch --sort=-committerdate    List branches by tip commit date (newest first)
    libra branch --format='%(refname:short) %(objectname:short)'  Render branches with a for-each-ref format
    libra branch --column                 List branches laid out in columns
    libra branch -v                       List branches with tip sha and subject
    libra branch -vv                      Also show upstream tracking [ahead/behind]
    libra branch --json --show-current    Structured JSON output for agents";

/// Tagged-union output type for `libra branch`.
///
/// Each variant corresponds to one of the action paths in [`run_branch`].
/// JSON serialisation is driven by `#[serde(tag = "action")]` so each
/// variant produces an object with a distinct `"action"` field.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "action")]
pub enum BranchOutput {
    /// Result of a list operation. The `head_name`, `detached_head`, and
    /// `show_unborn_head` fields are skipped from JSON; they only exist to
    /// drive the human renderer's "*"-prefixed current-branch line and
    /// detached/unborn HEAD banners.
    #[serde(rename = "list")]
    List {
        branches: Vec<BranchListEntry>,
        #[serde(skip_serializing)]
        head_name: Option<String>,
        #[serde(skip_serializing)]
        detached_head: Option<String>,
        #[serde(skip_serializing)]
        show_unborn_head: bool,
        #[serde(skip_serializing)]
        ignore_case: bool,
        /// When set, `branches` is already ordered (`--sort`, `branch.sort`, or
        /// the default refname order). The renderer must not re-promote the
        /// current branch to the top.
        #[serde(skip_serializing)]
        sorted: bool,
    },
    /// `branch reset <name> <target>` succeeded (lore.md 1.13).
    #[serde(rename = "reset")]
    Reset {
        name: String,
        old_commit: String,
        new_commit: String,
        target: String,
    },
    /// `branch <name> [base]` succeeded.
    #[serde(rename = "create")]
    Create { name: String, commit: String },
    /// `-d` / `-D` succeeded. `force = true` corresponds to `-D` (the merged
    /// check was bypassed).
    #[serde(rename = "delete")]
    Delete {
        name: String,
        commit: String,
        force: bool,
    },
    /// `-m` succeeded. Both names are recorded so callers can update local
    /// state references.
    #[serde(rename = "rename")]
    Rename { old_name: String, new_name: String },
    /// `-c`/`-C` succeeded. The source (`old_name`) is preserved; `new_name` is
    /// the created copy.
    #[serde(rename = "copy")]
    Copy { old_name: String, new_name: String },
    /// `--set-upstream-to` succeeded. `upstream` is the user-supplied spec.
    /// `local` is skipped from JSON so the envelope stays `{branch,upstream}`.
    #[serde(rename = "set-upstream")]
    SetUpstream {
        branch: String,
        upstream: String,
        #[serde(skip_serializing)]
        local: bool,
    },
    /// `-u <branch> <branch>` (self-upstream): warning only, no config write.
    #[serde(rename = "set-upstream-unchanged")]
    SetUpstreamUnchanged { branch: String },
    /// `--unset-upstream` succeeded.
    #[serde(rename = "unset-upstream")]
    UnsetUpstream { branch: String },
    /// `--edit-description` succeeded. `set` is true when a non-empty
    /// description was stored, false when the (empty) buffer unset it.
    #[serde(rename = "edit-description")]
    EditDescription { branch: String, set: bool },
    /// `--show-current` result. `detached` is true when HEAD is detached;
    /// `name` is `None` in that case.
    #[serde(rename = "show-current")]
    ShowCurrent {
        name: Option<String>,
        detached: bool,
        commit: Option<String>,
    },
}

impl BranchOutput {
    fn mutated_repo_state(&self) -> bool {
        matches!(
            self,
            Self::Create { .. }
                | Self::Reset { .. }
                | Self::Delete { .. }
                | Self::Rename { .. }
                | Self::Copy { .. }
                | Self::SetUpstream { .. }
                | Self::UnsetUpstream { .. }
                | Self::EditDescription { .. }
        )
    }
}

/// One row in [`BranchOutput::List`]. `display_name` carries the colorised
/// label for the human renderer and is omitted from JSON.
#[derive(Debug, Clone, Serialize)]
pub struct BranchListEntry {
    pub name: String,
    pub current: bool,
    pub commit: String,
    #[serde(skip_serializing)]
    pub display_name: String,
    /// Uncolored human label (used by the `--column` layout). Omitted from JSON.
    #[serde(skip_serializing)]
    pub plain_name: String,
}

// action options are mutually exclusive with query options
// query options can be combined
pub const BRANCH_DIFF_EXAMPLES: &str = "\
EXAMPLES:
    libra branch diff                     Current branch vs its upstream
    libra branch diff main                What the current branch changes vs main
    libra branch diff main feature       What feature changes vs main (diff main..feature)
    libra branch diff --merge-base main feature   Three-dot: merge-base(main,feature)..feature
    libra branch diff main -- src/       Limit to a path
    libra branch diff main --stat        Diffstat only

NOTES:
    Tip-to-tip only — the working tree is never involved. Full diff options
    live on `libra diff <base>..<branch>`. Exit 0 even with differences
    (use --exit-code for 1-on-diff).";

/// Branch verbs (Libra extensions to the flat Git-style flag surface).
#[derive(clap::Subcommand, Debug)]
pub enum BranchSubcommand {
    /// Show what a branch changes relative to a base (tip-to-tip diff;
    /// reuses the diff engine — Lore's `branch diff`).
    #[command(after_help = BRANCH_DIFF_EXAMPLES)]
    Diff(BranchDiffArgs),
    /// Move a branch tip to a target commit without touching the worktree
    /// (Lore's `branch reset`; enforces protect/archive metadata).
    #[command(after_help = BRANCH_RESET_EXAMPLES)]
    Reset(BranchResetArgs),
}

pub const BRANCH_RESET_EXAMPLES: &str = "\
EXAMPLES:
    libra branch reset feature main~2     Move feature's tip to main~2
    libra branch reset hotfix abc123      Move hotfix to a commit by OID prefix
    libra metadata set --branch rel protect true    Protect rel from resets
    libra metadata unset --branch rel protect       Lift the protection

NOTES:
    The index and working tree are NEVER touched — resetting the CURRENTLY
    checked-out branch is refused (use `libra reset` for that). Protected or
    archived branches (metadata) refuse with LBR-POLICY-001; there is no
    --force — lift the flag explicitly, reset, then re-protect (auditable).
    A reflog entry is written for the moved branch.";

#[derive(Parser, Debug)]
pub struct BranchResetArgs {
    /// The LOCAL branch whose tip to move.
    #[clap(value_name = "BRANCH")]
    pub branch: String,

    /// The commit-ish to move it to (branch, tag, OID prefix, HEAD~n, …).
    #[clap(value_name = "TARGET")]
    pub target: String,
}

#[derive(Parser, Debug)]
pub struct BranchDiffArgs {
    /// The base (old side). Defaults to the current branch's upstream.
    #[clap(value_name = "BASE")]
    pub base: Option<String>,

    /// The branch (new side). Defaults to the current branch.
    #[clap(value_name = "BRANCH")]
    pub branch: Option<String>,

    /// Three-dot semantics: diff from merge-base(BASE, BRANCH) to BRANCH
    /// (mirrors `git diff --merge-base`).
    #[clap(long = "merge-base")]
    pub merge_base: bool,

    /// Exit 1 when differences exist (0 when clean), like `diff --exit-code`.
    #[clap(long = "exit-code")]
    pub exit_code: bool,

    /// Show a diffstat instead of the patch.
    #[clap(long)]
    pub stat: bool,

    /// Show only names of changed files.
    #[clap(long = "name-only", conflicts_with = "name_status")]
    pub name_only: bool,

    /// Show names and status letters of changed files.
    #[clap(long = "name-status")]
    pub name_status: bool,

    /// Limit the diff to the given paths (always after `--`).
    #[clap(last = true, value_name = "PATH")]
    pub paths: Vec<String>,
}

/// `--track[=direct|inherit]` mode for branch creation (ADR-HF-08).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchTrackMode {
    Direct,
    Inherit,
}

fn parse_track_mode(raw: &str) -> Result<BranchTrackMode, String> {
    match raw {
        "direct" => Ok(BranchTrackMode::Direct),
        "inherit" => Ok(BranchTrackMode::Inherit),
        other => Err(format!(
            "invalid tracking mode '{other}'; expected 'direct' or 'inherit'"
        )),
    }
}

#[derive(Parser, Debug)]
#[command(after_help = BRANCH_AFTER_HELP)]
#[command(args_conflicts_with_subcommands = true)]
#[command(group(
    ArgGroup::new("action")
        .multiple(false)
        .conflicts_with("query")
))]
#[command(group(
    ArgGroup::new("query")
        .required(false)
        .multiple(true)
        .conflicts_with("action")
))]
pub struct BranchArgs {
    /// Branch verbs (Libra extensions). NOTE: when flags make clap fall back
    /// to the positional parse, a literal `diff` in `new_branch` is refused
    /// at execute time (never silently creates a branch named `diff`).
    #[command(subcommand)]
    pub subcommand: Option<BranchSubcommand>,

    /// new branch name
    #[clap(group = "action")]
    pub new_branch: Option<String>,

    /// base branch name or commit hash
    #[clap(requires = "new_branch")]
    pub commit_hash: Option<String>,

    /// list all branches, don't include remote branches
    #[clap(short, long, group = "query")]
    pub list: bool,

    /// force delete branch
    #[clap(short = 'D', long = "delete-force", group = "action")]
    pub delete: Option<String>,

    /// safe delete branch (checks if merged before deletion)
    #[clap(short = 'd', long = "delete", group = "action")]
    pub delete_safe: Option<String>,

    /// Set the branch's tracking information so UPSTREAM is considered its
    /// upstream. An optional positional branch name selects which local branch
    /// to configure (defaults to the current branch).
    #[clap(
        short = 'u',
        long,
        value_name = "UPSTREAM",
        conflicts_with_all = [
            "delete",
            "delete_safe",
            "unset_upstream",
            "edit_description",
            "show_current",
            "rename",
            "copy",
            "copy_force"
        ]
    )]
    pub set_upstream_to: Option<String>,

    /// When creating a branch, set up tracking (`direct`, default) or copy the
    /// start-point's upstream (`inherit`). Ignored with delete/rename/list.
    /// Mode is accepted only as `--track=direct|inherit`; `-t` / `--track`
    /// alone means `direct`.
    #[clap(
        short = 't',
        long = "track",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "direct",
        value_name = "MODE",
        value_parser = parse_track_mode,
        overrides_with = "no_track"
    )]
    pub track: Option<BranchTrackMode>,

    /// When creating a branch, do not write tracking configuration.
    #[clap(long = "no-track", overrides_with = "track")]
    pub no_track: bool,

    /// Remove the branch's upstream configuration. Defaults to current branch.
    #[clap(long = "unset-upstream", group = "action", value_name = "BRANCH", num_args = 0..=1, default_missing_value = "")]
    pub unset_upstream: Option<String>,

    /// Edit the description of a branch (defaults to the current branch) in an
    /// editor, storing it as `branch.<name>.description`. Saving an empty
    /// (comment-only) buffer unsets the description.
    #[clap(long = "edit-description", group = "action", value_name = "BRANCH", num_args = 0..=1, default_missing_value = "")]
    pub edit_description: Option<String>,

    /// show current branch
    #[clap(long, group = "action")]
    pub show_current: bool,

    /// Rename a branch. With one argument, renames the current branch. With two arguments, renames OLD_BRANCH to NEW_BRANCH.
    #[clap(short = 'm', long = "move", group = "action", value_names = ["OLD_BRANCH", "NEW_BRANCH"], num_args = 1..=2)]
    pub rename: Vec<String>,

    /// Copy a branch (and its upstream config) to a new name, keeping the
    /// original. With one argument, copies the current branch. Fails if the
    /// destination already exists (use -C to overwrite).
    #[clap(short = 'c', long = "copy", group = "action", value_names = ["OLD_BRANCH", "NEW_BRANCH"], num_args = 1..=2)]
    pub copy: Vec<String>,

    /// Like -c, but overwrite the destination branch if it already exists.
    #[clap(short = 'C', long = "copy-force", group = "action", value_names = ["OLD_BRANCH", "NEW_BRANCH"], num_args = 1..=2)]
    pub copy_force: Vec<String>,

    /// show remote branches
    #[clap(short, long, group = "query")]
    // TODO limit to required `list` option, even in default
    pub remotes: bool,

    /// show all branches (includes local and remote)
    #[clap(short, long, group = "query")]
    pub all: bool,

    /// Only list branches which contain the specified commit (HEAD if not specified). Implies --list.
    #[clap(long, group = "query", alias = "with", value_name = "commit", num_args = 0..=1, default_missing_value = "HEAD", action = clap::ArgAction::Append)]
    pub contains: Vec<String>,

    /// Only list branches which don’t contain the specified commit (HEAD if not specified). Implies --list.
    #[clap(long, group = "query", alias = "without", value_name = "commit", num_args = 0..=1, default_missing_value = "HEAD", action = clap::ArgAction::Append)]
    pub no_contains: Vec<String>,

    /// Only list branches pointing at the given object. Implies --list.
    #[clap(long = "points-at", group = "query", value_name = "object")]
    pub points_at: Option<String>,

    /// Only list branches already merged into the specified commit (HEAD if not specified). Implies --list.
    #[clap(long, group = "query", value_name = "commit", num_args = 0..=1, default_missing_value = "HEAD")]
    pub merged: Option<String>,

    /// Only list branches not yet merged into the specified commit (HEAD if not specified). Implies --list.
    #[clap(long = "no-merged", group = "query", value_name = "commit", num_args = 0..=1, default_missing_value = "HEAD")]
    pub no_merged: Option<String>,

    /// Sort the listed branches by key: `refname` (default), `version:refname` /
    /// `v:refname` (numeric-aware), `committerdate` / `creatordate` / `authordate`
    /// (the tip commit's committer / author date), `objectsize` (the tip
    /// object's byte size), or `objectname` (the tip commit's object id). A
    /// leading `-` reverses (e.g. `-committerdate` lists newest first). Implies
    /// --list.
    #[clap(long, group = "query", value_name = "key")]
    pub sort: Option<String>,

    /// Sorting and name comparisons ignore case where applicable.
    #[clap(long = "ignore-case", group = "query")]
    pub ignore_case: bool,

    /// Render each branch with a for-each-ref format string (e.g.
    /// `%(refname:short)` / `%(objectname:short)` / `%(HEAD)` / `%(upstream)`).
    /// Replaces the default `* name` listing (and `-v`/`--column`). Implies
    /// --list.
    #[clap(long, group = "query", value_name = "format")]
    pub format: Option<String>,

    /// Display the branch list in columns. Modes: `always`, `auto` (only when
    /// stdout is a terminal), `never`. Bare `--column` means `always`.
    #[clap(long, num_args = 0..=1, default_missing_value = "always", value_name = "MODE", overrides_with = "no_column")]
    pub column: Option<String>,

    /// Do not display the branch list in columns (equivalent to
    /// `--column=never`), countermanding an earlier `--column` (last one on the
    /// command line wins), matching `git branch --no-column`. Branches list one
    /// per line by default, so on its own this is a no-op.
    #[clap(long = "no-column", overrides_with = "column")]
    pub no_column: bool,

    /// Show the sha1 and commit subject line for each branch (`-v`). Repeat
    /// (`-vv`) to also show the upstream-tracking segment
    /// `[<upstream>: ahead N, behind M]` for branches with a configured upstream.
    #[clap(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    pub verbose: u8,
}
/// Fire-and-forget entry: prints the rendered error to stderr but does not
/// signal exit code.
/// `branch reset` (lore.md §1.13): move a LOCAL branch tip to a target
/// commit through the authoritative SQLite transaction — reference update +
/// branch reflog entry — WITHOUT touching the index or worktree. The first
/// enforcement consumer of the 1.5 protect/archive metadata: both flags are
/// re-checked fail-closed INSIDE the transaction (a metadata read error or a
/// concurrently-set flag rolls back before any write), and the checked-out
/// branch is re-checked in-txn too (a concurrent `switch` cannot slip a
/// phantom-staged-diff state through). No `--force`: lift the flag
/// explicitly via `metadata unset`, reset, re-protect — auditable.
async fn execute_reset_safe(args: BranchResetArgs, output: &OutputConfig) -> CliResult<()> {
    crate::utils::util::require_repo().map_err(|_| CliError::repo_not_found())?;
    let branch = args.branch.clone();

    if crate::internal::branch::is_ai_managed_branch(&branch) {
        return Err(CliError::failure(format!(
            "branch '{branch}' is AI-managed; refusing to reset it"
        ))
        .with_stable_code(StableErrorCode::ConflictOperationBlocked));
    }
    // Existence (with suggestions) + current-branch preflights (UX; both are
    // re-verified authoritatively inside the txn).
    let existing = Branch::find_branch_result(&branch, None)
        .await
        .map_err(map_branch_store_error)
        .map_err(CliError::from)?;
    let Some(_) = existing else {
        return Err(CliError::from(branch_not_found_error(&branch).await));
    };
    if let Head::Branch(current) = Head::current().await
        && current == branch
    {
        return Err(CliError::from(BranchError::ResetCurrentBranch(branch)));
    }
    // Part C W0 (§C.11): refuse to reset a branch checked out in ANOTHER
    // worktree — moving its tip would silently diverge that worktree's working
    // tree from its branch. The current worktree's branch is caught above.
    if let Some(other) = checked_out_elsewhere_or_refuse(&branch).await? {
        return Err(CliError::from(BranchError::CheckedOutElsewhere(
            "reset", branch, other,
        )));
    }
    // Target must resolve AND load as a commit — a ref is never pointed at a
    // missing or non-commit object.
    let target_commit = get_target_commit(&args.target).await.map_err(|_| {
        CliError::fatal(format!("cannot resolve target '{}'", args.target))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;
    let _: Commit = load_object(&target_commit).map_err(|error| {
        CliError::fatal(format!(
            "target '{}' does not point at a loadable commit: {error}",
            args.target
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;
    // Preflight policy checks (friendly errors; authoritative re-check in-txn).
    {
        let db = crate::internal::db::get_db_conn_instance().await;
        if crate::internal::metadata::MetadataKv::is_protected_with_conn(&db, &branch)
            .await
            .map_err(|e| {
                CliError::fatal(format!("failed to read branch policy metadata: {e}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?
        {
            return Err(CliError::from(BranchError::Protected(branch)));
        }
        if crate::internal::metadata::MetadataKv::is_archived_with_conn(&db, &branch)
            .await
            .map_err(|e| {
                CliError::fatal(format!("failed to read branch policy metadata: {e}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?
        {
            return Err(CliError::from(BranchError::Archived(branch)));
        }
    }

    let new_commit = target_commit.to_string();
    record_branch_reset_operation_digest(&branch, &new_commit).await?;
    // Sentinel prefixes preserve the TYPED refusal through DbErr::Custom so a
    // race-window refusal still surfaces as LBR-POLICY-001 / the current-
    // branch message rather than a generic storage error.
    const SENTINEL_PROTECTED: &str = "LIBRA_POLICY_PROTECTED:";
    const SENTINEL_ARCHIVED: &str = "LIBRA_POLICY_ARCHIVED:";
    const SENTINEL_CURRENT: &str = "LIBRA_RESET_CURRENT:";
    let branch_for_txn = branch.clone();
    let target_for_txn = args.target.clone();
    let new_for_txn = target_commit;
    let database = crate::internal::db::get_db_conn_instance().await;
    let transaction = crate::internal::db::begin_write_transaction(&database)
        .await
        .map_err(|error| CliError::fatal(format!("branch reset transaction failed: {error}")))?;
    let result = async {
        // Authoritative, fail-closed policy gate (the 1.5 contract).
        let protected = crate::internal::metadata::MetadataKv::is_protected_with_conn(
            &transaction,
            &branch_for_txn,
        )
        .await
        .map_err(|e| DbErr::Custom(format!("policy metadata read failed: {e}")))?;
        if protected {
            return Err(DbErr::Custom(format!(
                "{SENTINEL_PROTECTED}{branch_for_txn}"
            )));
        }
        let archived = crate::internal::metadata::MetadataKv::is_archived_with_conn(
            &transaction,
            &branch_for_txn,
        )
        .await
        .map_err(|e| DbErr::Custom(format!("policy metadata read failed: {e}")))?;
        if archived {
            return Err(DbErr::Custom(format!(
                "{SENTINEL_ARCHIVED}{branch_for_txn}"
            )));
        }
        if let Head::Branch(current) = Head::current_with_conn(&transaction).await
            && current == branch_for_txn
        {
            return Err(DbErr::Custom(format!("{SENTINEL_CURRENT}{branch_for_txn}")));
        }
        let live = Branch::find_branch_result_with_conn(&transaction, &branch_for_txn, None)
            .await
            .map_err(|e| DbErr::Custom(e.to_string()))?
            .ok_or_else(|| {
                DbErr::Custom(format!("branch '{branch_for_txn}' vanished mid-reset"))
            })?;
        Branch::update_branch_with_conn(
            &transaction,
            &branch_for_txn,
            &new_for_txn.to_string(),
            None,
        )
        .await?;
        let context = crate::internal::reflog::ReflogContext {
            old_oid: live.commit.to_string(),
            new_oid: new_for_txn.to_string(),
            action: crate::internal::reflog::ReflogAction::Reset {
                target: target_for_txn,
            },
        };
        crate::internal::reflog::Reflog::insert_single_entry(
            &transaction,
            &context,
            &format!("refs/heads/{branch_for_txn}"),
        )
        .await
        .map_err(|e| DbErr::Custom(format!("reflog write failed: {e}")))?;
        Ok::<String, DbErr>(live.commit.to_string())
    }
    .await;
    let old_commit = match result {
        Ok(old_commit) => {
            transaction
                .commit()
                .await
                .map_err(|error| CliError::fatal(format!("branch reset commit failed: {error}")))?;
            old_commit
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            let text = error.to_string();
            if let Some(name) = text.split(SENTINEL_PROTECTED).nth(1) {
                return Err(CliError::from(BranchError::Protected(name.to_string())));
            }
            if let Some(name) = text.split(SENTINEL_ARCHIVED).nth(1) {
                return Err(CliError::from(BranchError::Archived(name.to_string())));
            }
            if let Some(name) = text.split(SENTINEL_CURRENT).nth(1) {
                return Err(CliError::from(BranchError::ResetCurrentBranch(
                    name.to_string(),
                )));
            }
            return Err(CliError::fatal(format!("branch reset failed: {text}"))
                .with_stable_code(StableErrorCode::IoWriteFailed));
        }
    };

    let reset_output = BranchOutput::Reset {
        name: branch,
        old_commit,
        new_commit,
        target: args.target,
    };
    render_branch_output(&reset_output, output, None, 0, None).await?;
    if reset_output.mutated_repo_state() {
        dispatch_current_repo_vcs_event_to_history(VCS_EVENT_POST_BRANCH).await;
    }
    Ok(())
}

async fn record_branch_reset_operation_digest(branch: &str, new_commit: &str) -> CliResult<()> {
    let Some(operation_id) = current_operation_id() else {
        return Ok(());
    };

    let payload = format!("reset\0{branch}\0{new_commit}");
    let args_digest = format!("sha256:{}", hex::encode(Sha256::digest(payload.as_bytes())));
    let scope = WorktreeScope::for_request();
    let worktree_id = scope.storage_key();
    let database = get_db_conn_instance().await;
    let repo_id = RepoIdentity::resolve(&database)
        .await
        .map_err(|error| CliError::fatal(format!("failed to read repository identity: {error}")))?;
    if let Some(previous_operation) = store::find_recent_success_by_args_digest(
        &database,
        repo_id.as_str(),
        worktree_id,
        "branch",
        &args_digest,
    )
    .await
    .map_err(|error| CliError::fatal(format!("failed to read operation log: {error}")))?
    {
        return Err(CliError::fatal(format!(
            "duplicate operation: branch reset matches recent operation {previous_operation}"
        )));
    }
    store::update_operation_args_digest(&database, repo_id.as_str(), &operation_id, &args_digest)
        .await
        .map_err(|error| CliError::fatal(format!("failed to update operation log: {error}")))?;
    Ok(())
}

/// `branch diff` (lore.md §1.12): thin sugar over the diff engine — resolve
/// branch-flavored defaults, then delegate. Tip-to-tip only (the working
/// tree is never involved), byte-identical output to `libra diff BASE..BRANCH`.
async fn execute_diff_safe(args: BranchDiffArgs, output: &OutputConfig) -> CliResult<()> {
    crate::utils::util::require_repo().map_err(|_| CliError::repo_not_found())?;

    // Subject (new side): explicit or the current branch.
    let subject = match &args.branch {
        Some(explicit) => explicit.clone(),
        None => match Head::current().await {
            Head::Branch(name) => name,
            Head::Detached(_) => {
                if args.base.is_some() && args.branch.is_some() {
                    unreachable!("both sides explicit");
                }
                return Err(CliError::from(BranchError::DetachedHead));
            }
        },
    };
    // Base (old side): explicit or the current branch's upstream.
    let base = match &args.base {
        Some(explicit) => explicit.clone(),
        None => {
            let current = match Head::current().await {
                Head::Branch(name) => name,
                Head::Detached(_) => return Err(CliError::from(BranchError::DetachedHead)),
            };
            // Propagate real config-store failures — only a genuine ABSENCE
            // of tracking config gets the setup hint.
            let remote = ConfigKv::get(&format!("branch.{current}.remote"))
                .await
                .map_err(|error| {
                    CliError::fatal(format!("failed to read branch.{current}.remote: {error}"))
                        .with_stable_code(StableErrorCode::IoReadFailed)
                })?
                .map(|entry| entry.value);
            let merge = ConfigKv::get(&format!("branch.{current}.merge"))
                .await
                .map_err(|error| {
                    CliError::fatal(format!("failed to read branch.{current}.merge: {error}"))
                        .with_stable_code(StableErrorCode::IoReadFailed)
                })?
                .map(|entry| entry.value);
            match (remote, merge) {
                (Some(remote), Some(merge)) => {
                    let merge_short = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
                    format_upstream_ref(&remote, merge_short)
                }
                _ => {
                    return Err(CliError::failure(format!(
                        "there is no tracking information for branch '{current}'"
                    ))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("set one with 'libra branch --set-upstream-to=<remote>/<branch>'")
                    .with_hint(
                        "or name the base explicitly: 'libra branch diff <base> [<branch>]'",
                    ));
                }
            }
        }
    };
    // Branch-flavored preflight: convert unknown sides into the branch UX
    // (with the levenshtein suggestion) BEFORE delegating. On success the
    // ORIGINAL strings are forwarded (the engine re-resolves identically).
    for side in [&base, &subject] {
        if get_target_commit(side).await.is_err() {
            return Err(CliError::from(branch_not_found_error(side).await));
        }
    }

    let mut argv: Vec<String> = vec!["diff".to_string()];
    if args.merge_base {
        // Three-dot: reuse the engine's own merge-base computation and
        // NoMergeBase error. Refnames cannot contain '..', so the glued
        // form cannot mis-split.
        argv.push(format!("{base}...{subject}"));
    } else {
        // --old/--new route: skips the revision/path ambiguity walk entirely
        // (a file named like the branch cannot shadow it).
        argv.push("--old".to_string());
        argv.push(base);
        argv.push("--new".to_string());
        argv.push(subject);
    }
    if args.exit_code {
        argv.push("--exit-code".to_string());
    }
    if args.stat {
        argv.push("--stat".to_string());
    }
    if args.name_only {
        argv.push("--name-only".to_string());
    }
    if args.name_status {
        argv.push("--name-status".to_string());
    }
    crate::command::diff_plumbing::push_paths(&mut argv, &args.paths);
    crate::command::diff_plumbing::delegate_to_diff(argv, output, None).await
}

pub async fn execute(args: BranchArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Structured entry: returns [`CliResult`] for the dispatcher.
///
/// Functional scope:
/// - Runs [`run_branch`] then forwards to [`render_branch_output`].
///
/// Boundary conditions:
/// - All [`BranchError`] variants are mapped to [`CliError`] via the
///   `From` impl which sets stable codes and hints.
pub async fn execute_safe(args: BranchArgs, output: &OutputConfig) -> CliResult<()> {
    match args.subcommand {
        Some(BranchSubcommand::Diff(diff_args)) => {
            return execute_diff_safe(diff_args, output).await;
        }
        Some(BranchSubcommand::Reset(reset_args)) => {
            return execute_reset_safe(reset_args, output).await;
        }
        None => {}
    }
    // Reserved verb guard: when flags force clap into the positional parse
    // (`branch -v diff`, `branch --no-column diff main`, …), the `diff` token
    // lands in `new_branch` — refuse rather than silently create a branch
    // literally named `diff` (documented; escape hatch: `libra switch -c diff`).
    if let Some(verb @ ("diff" | "reset")) = args.new_branch.as_deref() {
        return Err(CliError::command_usage(format!(
            "`{verb}` is a reserved branch verb here (`libra branch {verb} …`) and branch \
             flags cannot be combined with it"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint(format!(
            "to create a branch literally named `{verb}`: `libra switch -c {verb}`"
        )));
    }
    // Validate the `--column` mode up front so an invalid mode is rejected
    // regardless of output path; the enable decision is recomputed at render.
    if let Some(mode) = args.column.as_deref() {
        super::tag::resolve_column_enabled(mode)?;
    }
    let result = run_branch(&args).await.map_err(CliError::from)?;
    render_branch_output(
        &result,
        output,
        args.column.as_deref(),
        args.verbose,
        args.format.as_deref(),
    )
    .await?;
    if result.mutated_repo_state() {
        dispatch_current_repo_vcs_event_to_history(VCS_EVENT_POST_BRANCH).await;
    }
    Ok(())
}

/// Domain error for `libra branch`.
///
/// `DelegatedCli` exists to forward already-built [`CliError`]s (typically
/// from upstream helpers like [`get_reachable_commits`]) without
/// double-wrapping their stable codes.
#[derive(Debug, thiserror::Error)]
enum BranchError {
    #[error("not a libra repository")]
    NotInRepo,

    #[error("'{0}' is not a valid branch name")]
    InvalidName(String),

    #[error("a branch named '{0}' already exists")]
    AlreadyExists(String),

    #[error("branch '{name}' not found")]
    NotFound { name: String, similar: Vec<String> },

    /// `-d` / `--delete` missing-branch refusal. Same wording and stable
    /// code as [`Self::NotFound`], but Git exits 1 here.
    #[error("branch '{name}' not found")]
    DeleteNotFound { name: String, similar: Vec<String> },

    #[error("branch '{0}' is protected; refusing to reset it")]
    Protected(String),

    #[error("branch '{0}' is archived; refusing to reset it")]
    Archived(String),

    #[error("cannot reset branch '{0}': it is currently checked out")]
    ResetCurrentBranch(String),

    #[error("Cannot delete the branch '{0}' which you are currently on")]
    DeleteCurrent(String),

    /// The branch is checked out in ANOTHER worktree (Part C W0 §C.11): a
    /// destructive change would leave that worktree's HEAD dangling. `.0` is the
    /// operation verb, `.1` the branch, `.2` the other worktree's id/path.
    #[error("Cannot {0} branch '{1}': it is checked out at worktree '{2}'")]
    CheckedOutElsewhere(&'static str, String, String),

    #[error("the branch '{0}' is not fully merged")]
    NotFullyMerged(String),

    #[error("the '{0}' branch is locked and cannot be modified")]
    Locked(String),

    #[error("HEAD is detached")]
    DetachedHead,

    #[error("cannot force-copy onto the currently checked-out branch '{0}'")]
    CopyOntoCurrentBranch(String),

    #[error("not a valid object name: '{0}'")]
    InvalidCommit(String),

    #[error("unsupported sort key '{0}'")]
    InvalidSortKey(String),

    #[error("bad config value '{value}' for '{key}' (expected a for-each-ref sort key)")]
    InvalidSortConfig { key: &'static str, value: String },

    #[error("failed to read config '{key}': {detail}")]
    SortConfigRead { key: &'static str, detail: String },

    #[error("the requested upstream branch '{0}' does not exist")]
    UpstreamMissing(String),

    #[error("branch '{0}' does not exist")]
    UpstreamTargetMissing(String),

    #[error("too many arguments to set new upstream")]
    TooManyUpstreamArgs,

    #[error("cannot set up tracking information; starting point '{0}' is not a branch")]
    TrackStartNotBranch(String),

    #[error("{0}")]
    ConfigReadFailed(String),

    #[error("failed to persist branch config '{key}': {detail}")]
    ConfigWriteFailed { key: String, detail: String },

    #[error("failed to query branch storage: {0}")]
    StorageQueryFailed(String),

    #[error("{0}")]
    StoredReferenceCorrupt(String),

    #[error("failed to create branch '{branch}': {detail}")]
    CreateFailed { branch: String, detail: String },

    #[error("failed to delete branch '{branch}': {detail}")]
    DeleteFailed { branch: String, detail: String },

    #[error("failed to load commit {commit}: {detail}")]
    CommitLoadFailed { commit: String, detail: String },

    #[error("no editor configured for --edit-description")]
    NoEditor,

    #[error("failed to edit branch description: {0}")]
    EditorFailed(String),

    #[error("too many arguments")]
    RenameTooManyArgs,

    #[error(transparent)]
    DelegatedCli(#[from] CliError),
}

impl From<BranchError> for CliError {
    fn from(error: BranchError) -> Self {
        match error {
            BranchError::NotInRepo => CliError::repo_not_found(),
            BranchError::Protected(name) => {
                CliError::fatal(format!("branch '{name}' is protected; refusing to reset it"))
                    .with_stable_code(StableErrorCode::PolicyRefUpdateBlocked)
                    .with_hint(format!(
                        "lift it explicitly: 'libra metadata unset --branch {name} protect', \
                         reset, then re-protect (auditable; there is no --force)"
                    ))
            }
            BranchError::Archived(name) => {
                CliError::fatal(format!("branch '{name}' is archived; refusing to reset it"))
                    .with_stable_code(StableErrorCode::PolicyRefUpdateBlocked)
                    .with_hint(format!(
                        "unarchive first: 'libra metadata unset --branch {name} archive'"
                    ))
            }
            BranchError::ResetCurrentBranch(name) => {
                CliError::fatal(format!(
                    "cannot reset branch '{name}': it is currently checked out"
                ))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint(
                    "use 'libra reset [--soft|--mixed|--hard] <target>' to move the checked-out \
                     branch (it updates HEAD, index and worktree consistently)",
                )
            }
            BranchError::InvalidName(name) => {
                CliError::fatal(format!("'{name}' is not a valid branch name"))
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                    .with_hint(
                        "branch names cannot contain spaces, '..', '@{', or control characters.",
                    )
            }
            BranchError::AlreadyExists(name) => {
                CliError::fatal(format!("a branch named '{name}' already exists"))
                    .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                    .with_hint("delete it first or choose a different name.")
            }
            BranchError::NotFound { name, similar } => {
                let mut err = CliError::fatal(format!("branch '{name}' not found"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("use 'libra branch -l' to list branches");
                for suggestion in similar {
                    err = err.with_hint(format!("did you mean '{suggestion}'?"));
                }
                err
            }
            BranchError::DeleteNotFound { name, similar } => {
                let mut err = CliError::failure(format!("branch '{name}' not found"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_exit_code(1)
                    .with_hint("use 'libra branch -l' to list branches");
                for suggestion in similar {
                    err = err.with_hint(format!("did you mean '{suggestion}'?"));
                }
                err
            }
            BranchError::DeleteCurrent(name) => CliError::failure(format!(
                "Cannot delete the branch '{name}' which you are currently on"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_exit_code(1)
            .with_hint("switch to another branch first."),
            // §C.13: "branch checked out in another worktree" is a
            // CONFLICT, carrying the occupying worktree's id — not a
            // generic "unsupported", which tells the user nothing about
            // what to do next.
            BranchError::CheckedOutElsewhere(verb, name, other) => CliError::fatal(format!(
                "Cannot {verb} branch '{name}': it is checked out at worktree '{other}'"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked)
            .with_hint(
                "switch that worktree to another branch first, or run the command there",
            ),
            BranchError::NotFullyMerged(name) => {
                CliError::failure(format!("the branch '{name}' is not fully merged"))
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
                    .with_exit_code(1)
                    .with_hint(format!(
                        "If you are sure you want to delete it, run 'libra branch -D {name}'."
                    ))
            }
            BranchError::Locked(name) => CliError::fatal(format!(
                "the '{name}' branch is locked and cannot be modified"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked),
            BranchError::DetachedHead => CliError::fatal("HEAD is detached")
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("checkout a branch first"),
            BranchError::CopyOntoCurrentBranch(name) => CliError::fatal(format!(
                "cannot force-copy onto the currently checked-out branch '{name}'"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("switch to a different branch first, or copy to a new name"),
            BranchError::InvalidCommit(target) => {
                CliError::fatal(format!("not a valid object name: '{target}'"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("use 'libra log --oneline' to see available commits.")
            }
            BranchError::InvalidSortKey(key) => CliError::command_usage(format!(
                "unsupported sort key '{key}'"
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint(
                "supported keys: refname, version:refname (v:refname), committerdate, creatordate, authordate, objectsize, objectname, each reversible with a leading '-'",
            ),
            BranchError::InvalidSortConfig { key, value } => {
                CliError::command_usage(format!(
                    "bad config value '{value}' for '{key}' (expected a for-each-ref sort key)"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint(format!(
                    "fix the offending value with 'libra config {key} <key>' (e.g. refname, -committerdate, version:refname)"
                ))
            }
            BranchError::SortConfigRead { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::IoReadFailed),
            BranchError::UpstreamMissing(upstream) => CliError::command_usage(format!(
                "the requested upstream branch '{upstream}' does not exist"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint(
                "If you meant to set the upstream to a local branch, use -u <local-branch> \
                 or --set-upstream-to=<local-branch>. If the upstream is on a remote, \
                 run 'libra fetch' first.",
            ),
            BranchError::UpstreamTargetMissing(name) => {
                CliError::command_usage(format!("branch '{name}' does not exist"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("use 'libra branch -l' to list branches")
            }
            BranchError::TooManyUpstreamArgs => {
                CliError::command_usage("too many arguments to set new upstream")
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                    .with_hint("usage: libra branch -u <upstream> [<branch>]")
            }
            BranchError::TrackStartNotBranch(start) => CliError::command_usage(format!(
                "cannot set up tracking information; starting point '{start}' is not a branch"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("use a local branch or a remote-tracking name such as origin/main"),
            BranchError::ConfigReadFailed(detail) => CliError::fatal(detail)
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("check whether the repository database is readable."),
            BranchError::ConfigWriteFailed { key, detail } => {
                CliError::fatal(format!("failed to persist branch config '{key}': {detail}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
                    .with_hint("check whether the repository database is writable.")
            }
            BranchError::NoEditor => CliError::fatal(
                "no editor configured; set core.editor, $GIT_EDITOR, $VISUAL, or $EDITOR",
            )
            .with_stable_code(StableErrorCode::IoReadFailed)
            .with_hint("configure an editor, e.g. `libra config core.editor vim`"),
            BranchError::EditorFailed(detail) => CliError::fatal(detail)
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("set core.editor/$GIT_EDITOR to a working editor command"),
            BranchError::StorageQueryFailed(detail) => {
                CliError::fatal(format!("failed to query branch storage: {detail}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            }
            BranchError::StoredReferenceCorrupt(detail) => {
                CliError::fatal(detail).with_stable_code(StableErrorCode::RepoCorrupt)
            }
            BranchError::CreateFailed { branch, detail } => {
                CliError::fatal(format!("failed to create branch '{branch}': {detail}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            }
            BranchError::DeleteFailed { branch, detail } => {
                CliError::fatal(format!("failed to delete branch '{branch}': {detail}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            }
            BranchError::CommitLoadFailed { commit, detail } => {
                CliError::fatal(format!("failed to load commit {commit}: {detail}"))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
            }
            BranchError::RenameTooManyArgs => CliError::command_usage("too many arguments")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("usage: libra branch -m [old-name] new-name"),
            BranchError::DelegatedCli(cli_error) => cli_error,
        }
    }
}

/// Sentinel constructor — keeps the call site readable when building errors
/// at multiple branches that all need the same `DetachedHead` message.
fn detached_head_branch_error() -> BranchError {
    BranchError::DetachedHead
}

/// plan-20260714 §C.4.4: the ONE cross-worktree checkout probe every branch
/// writer in this module uses.
///
/// The infallible `Head::branch_checked_out_elsewhere` folded a database error
/// into `None` — indistinguishable from "no other worktree has it". Every
/// destructive caller then read a transient query failure as permission to
/// move or delete a branch another worktree was sitting on. This variant makes
/// the failure a refusal.
async fn checked_out_elsewhere_or_refuse(branch: &str) -> Result<Option<String>, BranchError> {
    Head::branch_checked_out_elsewhere_result(branch)
        .await
        .map_err(|error| {
            BranchError::StorageQueryFailed(format!(
                "cannot determine whether branch '{branch}' is checked out in another worktree: \
                 {error}"
            ))
        })
}

/// Translate an internal storage error into the user-facing [`BranchError`].
///
/// Boundary conditions:
/// - `NotFound` is mapped without similarity suggestions; callers that want
///   "did you mean…" hints must use [`branch_not_found_error`] instead.
fn map_branch_store_error(error: branch::BranchStoreError) -> BranchError {
    match error {
        branch::BranchStoreError::Query(detail) => BranchError::StorageQueryFailed(detail),
        branch::BranchStoreError::AlreadyExists(name) => BranchError::AlreadyExists(name),
        branch::BranchStoreError::Corrupt { name, detail } => BranchError::StoredReferenceCorrupt(
            format!("stored branch reference '{name}' is corrupt: {detail}"),
        ),
        branch::BranchStoreError::NotFound(name) => BranchError::NotFound {
            name,
            similar: Vec::new(),
        },
        branch::BranchStoreError::Delete { name, detail } => BranchError::DeleteFailed {
            branch: name,
            detail,
        },
        // §C.13 LBR-CONFLICT-002 — the SAME variant the preflight raises, so
        // a collision detected at the storage seam (after the preflight, in
        // the window this card closed) is reported identically to one caught
        // before it. Routing it through `DeleteFailed` instead would report a
        // storage failure for a branch that is perfectly healthy and in use.
        branch::BranchStoreError::CheckedOutElsewhere { name, worktree } => {
            BranchError::CheckedOutElsewhere("modify", name, worktree)
        }
    }
}

/// Translate a storage error encountered while resolving HEAD's commit.
///
/// Functional scope:
/// - Query failures map to `IoReadFailed`; everything else is treated as
///   structural corruption (`RepoCorrupt`).
///
/// See: tests::test_head_commit_query_error_maps_to_io_read_failed in
/// src/command/branch.rs:1104.
fn map_head_commit_store_error(error: branch::BranchStoreError) -> BranchError {
    let cli_error = match error {
        branch::BranchStoreError::Query(detail) => {
            CliError::fatal(format!("failed to resolve HEAD commit: {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        other => CliError::fatal(format!("failed to resolve HEAD commit: {other}"))
            .with_stable_code(StableErrorCode::RepoCorrupt),
    };
    BranchError::DelegatedCli(cli_error)
}

/// Suggest a "did you mean…" alternative for `branch_name` based on
/// Levenshtein distance.
///
/// Functional scope:
/// - Skips candidates whose name length differs by more than 2 chars.
/// - Returns the single best (lowest distance, lexicographically smallest)
///   match within distance 2; returns an empty vector if no candidate
///   qualifies.
fn find_similar_branch_names(branch_name: &str, branches: &[Branch]) -> Vec<String> {
    let target_len = branch_name.chars().count();
    let mut best: Option<(usize, String)> = None;

    for branch in branches {
        if branch.name.chars().count().abs_diff(target_len) > 2 {
            continue;
        }

        let distance = levenshtein(&branch.name, branch_name);
        if distance > 2 {
            continue;
        }

        match &mut best {
            Some((best_distance, best_name))
                if distance < *best_distance
                    || (distance == *best_distance && branch.name < *best_name) =>
            {
                *best_distance = distance;
                *best_name = branch.name.clone();
            }
            None => best = Some((distance, branch.name.clone())),
            _ => {}
        }
    }

    best.into_iter().map(|(_, name)| name).collect()
}

/// Build a `NotFound` error with similarity suggestions; falls back to a
/// store error if the branch listing itself fails.
async fn branch_not_found_error(branch_name: &str) -> BranchError {
    match Branch::list_branches_result(None).await {
        Ok(branches) => BranchError::NotFound {
            name: branch_name.to_string(),
            similar: find_similar_branch_names(branch_name, &branches),
        },
        Err(error) => map_branch_store_error(error),
    }
}

/// Resolve `branch_name` to a [`Branch`], returning a friendly NotFound
/// error (with suggestions) when missing.
async fn require_existing_local_branch(branch_name: &str) -> Result<Branch, BranchError> {
    match Branch::find_branch_result(branch_name, None)
        .await
        .map_err(map_branch_store_error)?
    {
        Some(branch) => Ok(branch),
        None => Err(branch_not_found_error(branch_name).await),
    }
}

/// Build a config-read error, prefixing with a human-readable `scope`
/// (e.g. "remote configuration").
fn branch_config_read_error(scope: impl Into<String>, error: impl ToString) -> BranchError {
    let scope = scope.into();
    BranchError::ConfigReadFailed(format!("failed to read {scope}: {}", error.to_string()))
}

/// Build a config-write error tagged with the offending key.
fn branch_config_write_error(key: &str, error: impl ToString) -> BranchError {
    BranchError::ConfigWriteFailed {
        key: key.to_string(),
        detail: error.to_string(),
    }
}

/// Resolved form of an `-u/--set-upstream-to` argument (ADR-HF-08).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedUpstream {
    Remote { remote: String, merge: String },
    Local { name: String },
}

enum SetUpstreamOutcome {
    Written { local: bool },
    Unchanged,
}

fn split_remote_tracking_name(upstream: &str) -> Option<(&str, &str)> {
    let (remote, branch) = upstream.split_once('/')?;
    if remote.is_empty() || branch.is_empty() {
        None
    } else {
        Some((remote, branch))
    }
}

async fn local_branch_exists(name: &str) -> Result<bool, BranchError> {
    Ok(Branch::find_branch_result(name, None)
        .await
        .map_err(map_branch_store_error)?
        .is_some())
}

async fn remote_tracking_exists(remote: &str, branch: &str) -> Result<bool, BranchError> {
    let full = format!("refs/remotes/{remote}/{branch}");
    if Branch::find_branch_result(&full, Some(remote))
        .await
        .map_err(map_branch_store_error)?
        .is_some()
    {
        return Ok(true);
    }
    Ok(Branch::find_branch_result(branch, Some(remote))
        .await
        .map_err(map_branch_store_error)?
        .is_some())
}

/// ADR-HF-08 parse order: configured `remote/branch` (preferring an existing
/// tracking ref), then a local `refs/heads/<upstream>`, otherwise missing.
async fn resolve_upstream_spec(upstream: &str) -> Result<ResolvedUpstream, BranchError> {
    if let Some((remote, branch)) = split_remote_tracking_name(upstream) {
        let configured = ConfigKv::remote_config(remote)
            .await
            .map_err(|e| branch_config_read_error(format!("remote '{remote}' configuration"), e))?
            .is_some();
        if configured {
            let tracking = remote_tracking_exists(remote, branch).await?;
            let local_same_name = local_branch_exists(upstream).await?;
            if tracking || !local_same_name {
                return Ok(ResolvedUpstream::Remote {
                    remote: remote.to_string(),
                    merge: branch.to_string(),
                });
            }
        }
    }
    if local_branch_exists(upstream).await? {
        return Ok(ResolvedUpstream::Local {
            name: upstream.to_string(),
        });
    }
    Err(BranchError::UpstreamMissing(upstream.to_string()))
}

fn format_upstream_ref(remote: &str, merge_branch: &str) -> String {
    if remote == "." {
        merge_branch.to_string()
    } else {
        format!("{remote}/{merge_branch}")
    }
}

async fn write_upstream_config(
    branch: &str,
    remote: &str,
    merge_short: &str,
) -> Result<(), BranchError> {
    let db = get_db_conn_instance().await;
    let branch_config = ConfigKv::branch_config_with_conn(&db, branch)
        .await
        .map_err(|e| {
            branch_config_read_error(format!("upstream config for branch '{branch}'"), e)
        })?;
    let merge_ref = format!("refs/heads/{merge_short}");
    // `branch_config_with_conn()` normalizes `refs/heads/<name>` to `<name>`,
    // so the idempotency check must compare against the short branch name.
    let should_write = branch_config
        .as_ref()
        .map(|config| config.remote != remote || config.merge != merge_short)
        .unwrap_or(true);

    if should_write {
        let remote_key = format!("branch.{branch}.remote");
        ConfigKv::set_with_conn(&db, &remote_key, remote, false)
            .await
            .map_err(|e| branch_config_write_error(&remote_key, e))?;
        let merge_key = format!("branch.{branch}.merge");
        ConfigKv::set_with_conn(&db, &merge_key, &merge_ref, false)
            .await
            .map_err(|e| branch_config_write_error(&merge_key, e))?;
    }

    Ok(())
}

async fn set_upstream_impl(
    branch: &str,
    upstream: &str,
) -> Result<SetUpstreamOutcome, BranchError> {
    if !local_branch_exists(branch).await? {
        return Err(BranchError::UpstreamTargetMissing(branch.to_string()));
    }
    let resolved = resolve_upstream_spec(upstream).await?;
    let (remote, merge_short, local) = match &resolved {
        ResolvedUpstream::Remote { remote, merge } => (remote.as_str(), merge.as_str(), false),
        ResolvedUpstream::Local { name } => (".", name.as_str(), true),
    };
    if local && merge_short == branch {
        emit_warning(format!("not setting branch '{branch}' as its own upstream"));
        return Ok(SetUpstreamOutcome::Unchanged);
    }
    write_upstream_config(branch, remote, merge_short).await?;
    Ok(SetUpstreamOutcome::Written { local })
}

/// An already configured upstream is a read-only branch invocation.  Keeping
/// this small preflight outside the v2 mutation boundary preserves the
/// idempotent path even when the repository database is read-only.
pub(crate) async fn set_upstream_is_idempotent(args: &BranchArgs) -> bool {
    let Some(upstream) = args.set_upstream_to.as_deref() else {
        return false;
    };
    let Some((remote, remote_branch)) = upstream.split_once('/') else {
        return false;
    };
    if remote.is_empty() || remote_branch.is_empty() {
        return false;
    }
    let branch = match Head::current().await {
        Head::Branch(name) => name,
        Head::Detached(_) => return false,
    };
    let database = get_db_conn_instance().await;
    let config = match ConfigKv::branch_config_with_conn(&database, &branch).await {
        Ok(config) => config,
        Err(_) => return false,
    };
    config.is_some_and(|config| config.remote == remote && config.merge == remote_branch)
}

async fn unset_upstream_impl(branch: &str) -> Result<(), BranchError> {
    require_existing_local_branch(branch).await?;
    let db = get_db_conn_instance().await;
    for key in [
        format!("branch.{branch}.remote"),
        format!("branch.{branch}.merge"),
    ] {
        ConfigKv::unset_all_with_conn(&db, &key)
            .await
            .map_err(|e| branch_config_write_error(&key, e))?;
    }
    Ok(())
}

/// `--edit-description`: open the configured editor seeded with the branch's
/// current `branch.<name>.description`, then store the cleaned result — or unset
/// the key when the saved buffer is empty (comment-only). Returns `true` when a
/// non-empty description was stored, `false` when it was unset.
async fn edit_description_impl(branch: &str) -> Result<bool, BranchError> {
    require_existing_local_branch(branch).await?;

    let key = format!("branch.{branch}.description");
    let current = crate::internal::config::ConfigKv::get(&key)
        .await
        .map_err(|e| branch_config_read_error(format!("config '{key}'"), e))?
        .map(|entry| entry.value)
        .unwrap_or_default();

    // Seed the editor with the existing description followed by a comment block;
    // lines starting with '#' are stripped on save (Git convention).
    let template = format!(
        "{current}\n\
         # Please edit the description for the branch:\n\
         #   {branch}\n\
         # Lines starting with '#' will be stripped.\n"
    );

    // An explicitly configured editor runs even without a TTY (so scripted
    // editors work in tests/automation); `vi` is only assumed on a terminal.
    let editor_cmd = match crate::command::editor::resolve_editor().await {
        Some(cmd) => cmd,
        None if std::io::stdin().is_terminal() => "vi".to_string(),
        None => return Err(BranchError::NoEditor),
    };

    // Part C §C.4.3: transient per-worktree editor scratch — on shared storage
    // two worktrees composing a message concurrently would truncate each other's
    // buffer. Identical path for the main worktree (local == common storage).
    let path =
        crate::utils::util::request_worktree_gitdir_strict().join("BRANCH_DESCRIPTION_EDITMSG");
    let raw = crate::command::editor::edit_message(&path, &template, &editor_cmd, true)
        .await
        .map_err(|e| BranchError::EditorFailed(e.to_string()))?;

    let description = clean_branch_description(&raw);
    if description.is_empty() {
        crate::internal::config::ConfigKv::unset_all(&key)
            .await
            .map_err(|e| branch_config_write_error(&key, e))?;
        Ok(false)
    } else {
        crate::internal::config::ConfigKv::set(&key, &description, false)
            .await
            .map_err(|e| branch_config_write_error(&key, e))?;
        Ok(true)
    }
}

/// Clean an edited branch-description buffer with `git stripspace` semantics
/// (as `git branch --edit-description` does):
/// - drop lines whose **first character** is the comment char `#` (an indented
///   `  # heading` is content and is kept);
/// - trim trailing whitespace from each line but preserve leading indentation;
/// - collapse runs of blank lines into a single blank and drop leading/trailing
///   blank lines.
fn clean_branch_description(raw: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut pending_blank = false;
    for line in raw.lines() {
        if line.starts_with('#') {
            continue;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            // Remember a blank only once we have content, so leading blanks are
            // dropped and interior runs collapse to a single blank line.
            pending_blank = !out.is_empty();
            continue;
        }
        if pending_blank {
            out.push(String::new());
            pending_blank = false;
        }
        out.push(trimmed.to_string());
    }
    out.join("\n")
}

/// Enumerate every branch stored under each known remote.
///
/// Functional scope:
/// - Reads all `[remote "..."]` sections, then asks the branch store for
///   branches scoped to each remote, concatenating results.
///
/// Boundary conditions:
/// - Config read failures raise [`BranchError::ConfigReadFailed`].
/// - Per-remote enumeration failures bubble up as
///   [`BranchError::StorageQueryFailed`] via [`map_branch_store_error`].
///
/// See: tests::test_load_remote_branches_with_conn_surfaces_config_read_failure
/// in src/command/branch.rs:1090.
async fn load_remote_branches_with_conn<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<Branch>, BranchError> {
    let remote_configs = ConfigKv::all_remote_configs_with_conn(db)
        .await
        .map_err(|e| branch_config_read_error("remote configuration", e))?;
    let mut remote_branches = Vec::new();
    for remote in remote_configs {
        remote_branches.extend(
            Branch::list_branches_result_with_conn(db, Some(&remote.name))
                .await
                .map_err(map_branch_store_error)?,
        );
    }
    Ok(remote_branches)
}

/// Convenience wrapper around [`load_remote_branches_with_conn`] that uses
/// the process-wide SQLite handle.
async fn load_remote_branches() -> Result<Vec<Branch>, BranchError> {
    let db = get_db_conn_instance().await;
    load_remote_branches_with_conn(&db).await
}

/// Body of `libra branch <new> [base]`.
///
/// Functional scope:
/// - Validates the new name, refuses locked names and pre-existing
///   branches, then resolves either an explicit base ref or HEAD.
/// - Loads the resolved commit object to confirm it actually exists in the
///   object store before writing the branch row.
///
/// Boundary conditions:
/// - HEAD with no commit (unborn branch) and no explicit base produces
///   [`BranchError::InvalidCommit`] tagged with the current HEAD label so
///   the user sees something actionable.
/// - Branch-store write failures map to [`BranchError::CreateFailed`].
async fn create_branch_impl(
    new_branch: String,
    branch_or_commit: Option<String>,
    record_operation: bool,
) -> Result<BranchOutput, BranchError> {
    tracing::debug!("create branch: {} from {:?}", new_branch, branch_or_commit);

    if !is_valid_git_branch_name(&new_branch) {
        return Err(BranchError::InvalidName(new_branch));
    }
    if branch::is_locked_branch(&new_branch) {
        return Err(BranchError::Locked(new_branch));
    }

    if Branch::find_branch_result(&new_branch, None)
        .await
        .map_err(map_branch_store_error)?
        .is_some()
    {
        return Err(BranchError::AlreadyExists(new_branch));
    }

    let base_name = branch_or_commit.clone();
    let commit_id = match branch_or_commit {
        Some(branch_or_commit) => get_target_commit(&branch_or_commit)
            .await
            .map_err(|_| BranchError::InvalidCommit(branch_or_commit))?,
        None => {
            if let Some(commit_id) = Head::current_commit_result()
                .await
                .map_err(map_head_commit_store_error)?
            {
                commit_id
            } else {
                let current = match Head::current().await {
                    Head::Branch(name) => name,
                    Head::Detached(commit_hash) => commit_hash.to_string(),
                };
                return Err(BranchError::InvalidCommit(current));
            }
        }
    };

    let commit_id_display = commit_id.to_string();
    load_object::<Commit>(&commit_id).map_err(|_| {
        BranchError::InvalidCommit(
            base_name
                .as_deref()
                .unwrap_or(commit_id_display.as_str())
                .to_string(),
        )
    })?;

    if record_operation {
        let database = crate::internal::db::get_db_conn_instance().await;
        let transaction = crate::internal::db::begin_write_transaction(&database)
            .await
            .map_err(|error| BranchError::CreateFailed {
                branch: new_branch.clone(),
                detail: error.to_string(),
            })?;
        let branch_for_operation = new_branch.clone();
        let commit_for_operation = commit_id_display.clone();
        let result = async {
            let exists = Branch::exists_result_with_conn(&transaction, &branch_for_operation, None)
                .await
                .map_err(|error| DbErr::Custom(error.to_string()))?;
            if exists {
                return Err(DbErr::Custom(format!(
                    "a branch named '{}' already exists",
                    branch_for_operation
                )));
            }
            Branch::update_branch_with_conn(
                &transaction,
                &branch_for_operation,
                &commit_for_operation,
                None,
            )
            .await?;
            Ok::<(), DbErr>(())
        }
        .await;
        match result {
            Ok(()) => transaction
                .commit()
                .await
                .map_err(|error| BranchError::CreateFailed {
                    branch: new_branch.clone(),
                    detail: error.to_string(),
                })?,
            Err(error) => {
                let _ = transaction.rollback().await;
                return Err(BranchError::CreateFailed {
                    branch: new_branch.clone(),
                    detail: error.to_string(),
                });
            }
        }
    } else {
        Branch::update_branch(&new_branch, &commit_id_display, None)
            .await
            .map_err(|error| BranchError::CreateFailed {
                branch: new_branch.clone(),
                detail: error.to_string(),
            })?;
    }

    Ok(BranchOutput::Create {
        name: new_branch,
        commit: commit_id_display,
    })
}

enum PlannedCreateTracking {
    Write {
        remote: String,
        merge: String,
        local: bool,
        display: String,
    },
    None,
}

async fn start_point_label(explicit: Option<&str>) -> String {
    match explicit {
        Some(start) => start.to_string(),
        None => match Head::current().await {
            Head::Branch(name) => name,
            Head::Detached(hash) => hash.to_string(),
        },
    }
}

async fn plan_create_tracking(
    mode: BranchTrackMode,
    start: &str,
) -> Result<PlannedCreateTracking, BranchError> {
    match mode {
        BranchTrackMode::Direct => match resolve_upstream_spec(start).await {
            Ok(ResolvedUpstream::Remote { remote, merge }) => Ok(PlannedCreateTracking::Write {
                display: format_upstream_ref(&remote, &merge),
                remote,
                merge,
                local: false,
            }),
            Ok(ResolvedUpstream::Local { name }) => Ok(PlannedCreateTracking::Write {
                display: name.clone(),
                remote: ".".to_string(),
                merge: name,
                local: true,
            }),
            Err(_) => Err(BranchError::TrackStartNotBranch(start.to_string())),
        },
        BranchTrackMode::Inherit => {
            if local_branch_exists(start).await? {
                match ConfigKv::branch_config(start).await.ok().flatten() {
                    Some(config) => {
                        let local = config.remote == ".";
                        Ok(PlannedCreateTracking::Write {
                            display: format_upstream_ref(&config.remote, &config.merge),
                            remote: config.remote,
                            merge: config.merge,
                            local,
                        })
                    }
                    None => Ok(PlannedCreateTracking::None),
                }
            } else {
                match resolve_upstream_spec(start).await {
                    Ok(ResolvedUpstream::Remote { remote, merge }) => {
                        Ok(PlannedCreateTracking::Write {
                            display: format_upstream_ref(&remote, &merge),
                            remote,
                            merge,
                            local: false,
                        })
                    }
                    Ok(ResolvedUpstream::Local { name }) => Ok(PlannedCreateTracking::Write {
                        display: name.clone(),
                        remote: ".".to_string(),
                        merge: name,
                        local: true,
                    }),
                    Err(_) => Err(BranchError::TrackStartNotBranch(start.to_string())),
                }
            }
        }
    }
}

async fn create_branch_with_optional_track(
    new_branch: String,
    args: &BranchArgs,
) -> Result<BranchOutput, BranchError> {
    let track_mode = if args.no_track { None } else { args.track };
    let start_label = start_point_label(args.commit_hash.as_deref()).await;
    let planned = match track_mode {
        Some(mode) => Some(plan_create_tracking(mode, &start_label).await?),
        None => None,
    };

    let created = create_branch_impl(new_branch.clone(), args.commit_hash.clone(), true).await?;
    match planned {
        Some(PlannedCreateTracking::Write {
            remote,
            merge,
            local,
            display,
        }) => {
            if local && merge == new_branch {
                emit_warning(format!(
                    "not setting branch '{new_branch}' as its own upstream"
                ));
                Ok(created)
            } else {
                write_upstream_config(&new_branch, &remote, &merge).await?;
                Ok(BranchOutput::SetUpstream {
                    branch: new_branch,
                    upstream: display,
                    local,
                })
            }
        }
        Some(PlannedCreateTracking::None) | None => Ok(created),
    }
}

/// Body of `libra branch -d <name>` / `-D <name>`.
///
/// Functional scope:
/// - Refuses to delete a locked branch or the currently checked-out branch.
/// - When `force == false`, walks `get_reachable_commits` from HEAD and
///   ensures the branch tip is reachable; otherwise reports
///   [`BranchError::NotFullyMerged`] (recoverable failure, exit code stays
///   non-fatal).
///
/// Boundary conditions:
/// - In detached HEAD mode the merged-check uses the detached commit hash.
async fn delete_branch_impl(branch_name: String, force: bool) -> Result<BranchOutput, BranchError> {
    if branch::is_locked_branch(&branch_name) {
        return Err(BranchError::Locked(branch_name));
    }

    let branch = match require_existing_local_branch(&branch_name).await {
        Ok(branch) => branch,
        Err(BranchError::NotFound { name, similar }) => {
            return Err(BranchError::DeleteNotFound { name, similar });
        }
        Err(error) => return Err(error),
    };
    let head = Head::current().await;
    if let Head::Branch(name) = &head
        && name == &branch_name
    {
        return Err(BranchError::DeleteCurrent(branch_name));
    }
    // Part C W0 (§C.11): refuse to delete a branch checked out in ANOTHER
    // worktree — that worktree's HEAD would be left dangling (Git parity). The
    // current worktree's own branch is caught by DeleteCurrent above.
    if let Some(other) = checked_out_elsewhere_or_refuse(&branch_name).await? {
        return Err(BranchError::CheckedOutElsewhere(
            "delete",
            branch_name,
            other,
        ));
    }

    if !force {
        let head_commit = match head {
            Head::Branch(_) => Head::current_commit_result()
                .await
                .map_err(map_head_commit_store_error)?
                .ok_or_else(|| {
                    BranchError::DelegatedCli(
                        CliError::fatal("cannot get HEAD commit")
                            .with_stable_code(StableErrorCode::RepoStateInvalid),
                    )
                })?,
            Head::Detached(commit_hash) => commit_hash,
        };

        let head_reachable = get_reachable_commits(head_commit.to_string(), None)
            .await
            .map_err(BranchError::DelegatedCli)?;
        let head_commit_ids: std::collections::HashSet<_> =
            head_reachable.iter().map(|c| c.id).collect();
        if !head_commit_ids.contains(&branch.commit) {
            return Err(BranchError::NotFullyMerged(branch_name));
        }
    }

    Branch::delete_branch_result(&branch_name, None)
        .await
        .map_err(map_branch_store_error)?;

    Ok(BranchOutput::Delete {
        name: branch_name,
        commit: branch.commit.to_string(),
        force,
    })
}

/// Body of `libra branch -m [old] new`.
///
/// Functional scope:
/// - One argument: rename the current branch (errors on detached HEAD).
/// - Two arguments: rename the named source branch.
/// - When the rename touches the checked-out branch, HEAD is updated to
///   point at the new name before deleting the old row.
///
/// Boundary conditions:
/// - Returns [`BranchError::RenameTooManyArgs`] for argv with >2 names.
/// - Returns [`BranchError::AlreadyExists`] if the destination already
///   exists; the rename is non-destructive.
async fn rename_branch_impl(args: &[String]) -> Result<BranchOutput, BranchError> {
    let (old_name, new_name) = match args.len() {
        1 => match Head::current().await {
            Head::Branch(name) => (name, args[0].clone()),
            Head::Detached(_) => return Err(detached_head_branch_error()),
        },
        2 => (args[0].clone(), args[1].clone()),
        _ => return Err(BranchError::RenameTooManyArgs),
    };

    if !is_valid_git_branch_name(&new_name) {
        return Err(BranchError::InvalidName(new_name));
    }
    if branch::is_locked_branch(&new_name) {
        return Err(BranchError::Locked(new_name));
    }
    if branch::is_locked_branch(&old_name) {
        return Err(BranchError::Locked(old_name));
    }

    let old_branch = require_existing_local_branch(&old_name).await?;
    // Part C W0 (§C.11): refuse to rename a branch checked out in ANOTHER
    // worktree — that worktree's HEAD would be left pointing at the deleted old
    // name. The current worktree's own HEAD is re-pointed to the new name below.
    if let Some(other) = checked_out_elsewhere_or_refuse(&old_name).await? {
        return Err(BranchError::CheckedOutElsewhere("rename", old_name, other));
    }
    if Branch::find_branch_result(&new_name, None)
        .await
        .map_err(map_branch_store_error)?
        .is_some()
    {
        return Err(BranchError::AlreadyExists(new_name));
    }

    let commit_hash = old_branch.commit.to_string();
    Branch::update_branch(&new_name, &commit_hash, None)
        .await
        .map_err(|e| BranchError::CreateFailed {
            branch: new_name.clone(),
            detail: e.to_string(),
        })?;

    if let Head::Branch(name) = Head::current().await
        && name == old_name
    {
        Head::update(Head::Branch(new_name.clone()), None).await;
    }

    // Move branch metadata (lore.md §1.5) BEFORE deleting the old ref — the
    // delete's metadata cascade would otherwise wipe the rows being moved.
    // The destination has no rows (rename onto an existing branch is refused
    // above), so the defensive dest-clear inside rename_target is a no-op.
    {
        let db = get_db_conn_instance().await;
        crate::internal::metadata::MetadataKv::rename_target_with_conn(
            &db,
            crate::internal::metadata::MetadataScope::Branch,
            &old_name,
            &new_name,
        )
        .await
        .map_err(|e| branch_config_write_error("branch metadata", e))?;
    }

    Branch::delete_branch_result(&old_name, None)
        .await
        .map_err(map_branch_store_error)?;

    Ok(BranchOutput::Rename { old_name, new_name })
}

/// Copy a branch to a new name, keeping the original (`git branch -c`/`-C`).
/// With one argument the current branch is the source. The new branch is
/// created at the source's commit and the source's upstream config
/// (`branch.<old>.remote`/`.merge`) is copied. `force` (`-C`) overwrites an
/// existing destination; otherwise a clashing destination is an error. HEAD is
/// never moved (the source remains intact).
async fn copy_branch_impl(args: &[String], force: bool) -> Result<BranchOutput, BranchError> {
    let (old_name, new_name) = match args.len() {
        1 => match Head::current().await {
            Head::Branch(name) => (name, args[0].clone()),
            Head::Detached(_) => return Err(detached_head_branch_error()),
        },
        2 => (args[0].clone(), args[1].clone()),
        _ => return Err(BranchError::RenameTooManyArgs),
    };

    if !is_valid_git_branch_name(&new_name) {
        return Err(BranchError::InvalidName(new_name));
    }
    if branch::is_locked_branch(&new_name) {
        return Err(BranchError::Locked(new_name));
    }

    let old_branch = require_existing_local_branch(&old_name).await?;

    let destination_exists = Branch::find_branch_result(&new_name, None)
        .await
        .map_err(map_branch_store_error)?
        .is_some();
    if !force && destination_exists {
        return Err(BranchError::AlreadyExists(new_name));
    }
    // Even with -C, refuse to overwrite the checked-out branch: its ref would
    // move but HEAD / the working tree would not, leaving an inconsistent state
    // (Git likewise refuses to force-update the current branch).
    if force
        && let Head::Branch(current) = Head::current().await
        && current == new_name
    {
        return Err(BranchError::CopyOntoCurrentBranch(new_name));
    }

    let commit_hash = old_branch.commit.to_string();
    Branch::update_branch(&new_name, &commit_hash, None)
        .await
        .map_err(|e| BranchError::CreateFailed {
            branch: new_name.clone(),
            detail: e.to_string(),
        })?;

    // Copy the source branch's upstream configuration, if any, to the new
    // branch (mirroring `git branch -c`). The raw stored values are copied
    // verbatim so the `refs/heads/` prefix on `branch.<old>.merge` is preserved.
    let db = get_db_conn_instance().await;
    for suffix in ["remote", "merge"] {
        let src_key = format!("branch.{old_name}.{suffix}");
        if let Some(entry) = ConfigKv::get_with_conn(&db, &src_key)
            .await
            .map_err(|e| branch_config_read_error(format!("config '{src_key}'"), e))?
        {
            let dst_key = format!("branch.{new_name}.{suffix}");
            ConfigKv::set_with_conn(&db, &dst_key, &entry.value, false)
                .await
                .map_err(|e| branch_config_write_error(&dst_key, e))?;
        }
    }

    // Copy branch metadata (lore.md §1.5). A forced copy (-C) replaces any
    // metadata the overwritten destination carried — destructive by design,
    // matching the ref overwrite itself.
    crate::internal::metadata::MetadataKv::copy_target_with_conn(
        &db,
        crate::internal::metadata::MetadataScope::Branch,
        &old_name,
        &new_name,
    )
    .await
    .map_err(|e| branch_config_write_error("branch metadata", e))?;

    Ok(BranchOutput::Copy { old_name, new_name })
}

/// Body of `libra branch -l` / `-r` / `-a` (with optional commit filters).
///
/// Functional scope:
/// - Picks a [`BranchListMode`] from `args.all`/`args.remotes`, fetches
///   the matching branches, and runs [`filter_branches`] for any
///   `--contains`/`--no-contains` arguments.
/// - Records HEAD (branch name vs detached commit) so the human renderer
///   can mark the current branch and emit "HEAD detached at" / "(unborn)"
///   banners.
async fn collect_branch_output(args: &BranchArgs) -> Result<BranchOutput, BranchError> {
    let list_mode = if args.all {
        BranchListMode::All
    } else if args.remotes {
        BranchListMode::Remote
    } else {
        BranchListMode::Local
    };
    let has_commit_filters = !args.contains.is_empty()
        || !args.no_contains.is_empty()
        || args.points_at.is_some()
        || args.merged.is_some()
        || args.no_merged.is_some()
        || args.sort.is_some();
    let (head_name, detached_head) = match Head::current().await {
        Head::Branch(name) => (Some(name), None),
        Head::Detached(commit_hash) => (None, Some(commit_hash.to_string())),
    };

    let mut local_branches = match list_mode {
        BranchListMode::Local | BranchListMode::All => Branch::list_branches_result(None)
            .await
            .map_err(map_branch_store_error)?,
        BranchListMode::Remote => vec![],
    };
    let mut remote_branches = if matches!(list_mode, BranchListMode::Remote | BranchListMode::All) {
        load_remote_branches().await?
    } else {
        vec![]
    };

    let points_at = match args.points_at.as_deref() {
        Some(points_at) => Some(
            get_target_commit(points_at)
                .await
                .map_err(|_| BranchError::InvalidCommit(points_at.to_string()))?,
        ),
        None => None,
    };
    if let Some(points_at) = points_at {
        local_branches.retain(|branch| branch.commit == points_at);
        remote_branches.retain(|branch| branch.commit == points_at);
    }

    let contains_set = resolve_commits(&args.contains).await?;
    let no_contains_set = resolve_commits(&args.no_contains).await?;
    // `--merged`/`--no-merged` keep (or drop) branches whose tip is reachable
    // from the target commit (i.e. already merged into it) — the inverse of
    // `--contains`. The reachable set is computed once per target.
    let merged_set = resolve_reachable_for_merge(args.merged.as_deref()).await?;
    let no_merged_set = resolve_reachable_for_merge(args.no_merged.as_deref()).await?;
    for branches in [&mut local_branches, &mut remote_branches] {
        filter_branches_result(branches, &contains_set, &no_contains_set)?;
        if let Some(set) = &merged_set {
            branches.retain(|branch| set.contains(&branch.commit));
        }
        if let Some(set) = &no_merged_set {
            branches.retain(|branch| !set.contains(&branch.commit));
        }
    }
    let local_branches_empty = local_branches.is_empty();

    let current_name = head_name.as_deref();
    let mut entries = Vec::new();
    for branch in local_branches {
        entries.push(BranchListEntry {
            current: current_name == Some(branch.name.as_str()),
            commit: branch.commit.to_string(),
            display_name: branch.name.clone(),
            plain_name: branch.name.clone(),
            name: branch.name,
        });
    }
    for branch in &remote_branches {
        let plain_name = match list_mode {
            BranchListMode::All => format!("remotes/{}", plain_branch_display_name(branch)),
            _ => plain_branch_display_name(branch),
        };
        entries.push(BranchListEntry {
            current: false,
            commit: branch.commit.to_string(),
            display_name: match list_mode {
                BranchListMode::All => plain_name.clone().red().to_string(),
                _ => format_branch_name(branch),
            },
            plain_name,
            name: branch.name.clone(),
        });
    }

    // Git `branch -a`/`-r` insert `[<remotes>/]<remote>/HEAD -> <remote>/<branch>`
    // for each cached remote HEAD symref (issues/474 CL-15 / M-BRA D1–D2).
    if matches!(list_mode, BranchListMode::Remote | BranchListMode::All) {
        let db = get_db_conn_instance().await;
        let remote_configs = ConfigKv::all_remote_configs_with_conn(&db)
            .await
            .map_err(|e| branch_config_read_error("remote configuration", e))?;
        for remote in remote_configs {
            match Head::remote_current_result_with_conn(&db, &remote.name).await {
                Ok(Some(Head::Branch(target))) => {
                    let target_ref = remote_tracking_refname(&remote.name, &target);
                    let Some(target_branch) = remote_branches.iter().find(|branch| {
                        remote_tracking_refname(&remote.name, &branch.name) == target_ref
                    }) else {
                        // Target was filtered out (--contains/--points-at/…) or is missing.
                        continue;
                    };
                    let short_target = remote_tracking_short_name(&target);
                    let plain_name =
                        remote_head_symlink_plain_name(list_mode, &remote.name, &short_target);
                    entries.push(BranchListEntry {
                        current: false,
                        commit: target_branch.commit.to_string(),
                        display_name: plain_name.clone().red().to_string(),
                        plain_name,
                        name: format!("refs/remotes/{}/HEAD", remote.name),
                    });
                }
                Ok(Some(Head::Detached(_)) | None) => {}
                Err(error) => return Err(map_branch_store_error(error)),
            }
        }
    }

    let show_unborn_head = local_branches_empty
        && detached_head.is_none()
        && !has_commit_filters
        && matches!(list_mode, BranchListMode::Local | BranchListMode::All)
        && head_name.is_some();

    // `--sort` / `branch.sort` / the default refname order are applied here
    // (human and JSON share the same sequence). The renderer must not
    // re-promote the current branch. Without `--sort`, the Git-compatible
    // `branch.sort` config default applies (strict local→global→system
    // cascade); when that is also unset, locals then remotes are ordered by
    // `plain_name`. The config is resolved here — after `has_commit_filters`
    // and `show_unborn_head` — so a configured sort, unlike the flag, neither
    // implies `--list` nor suppresses the unborn-HEAD line (Git behavior).
    let config_sort = if args.sort.is_none() {
        configured_branch_sort().await?
    } else {
        None
    };
    let sorted = match args.sort.as_deref() {
        Some(key) => {
            sort_branch_entries(&mut entries, key, args.ignore_case)?;
            true
        }
        None => match config_sort.as_deref() {
            Some(key) => {
                sort_branch_entries(&mut entries, key, args.ignore_case).map_err(|_| {
                    BranchError::InvalidSortConfig {
                        key: "branch.sort",
                        value: key.to_string(),
                    }
                })?;
                true
            }
            None => {
                sort_entries_default_refname(&mut entries, args.ignore_case);
                true
            }
        },
    };

    Ok(BranchOutput::List {
        branches: entries,
        head_name,
        detached_head,
        show_unborn_head,
        ignore_case: args.ignore_case,
        sorted,
    })
}

/// Top-level dispatcher: pick the right `*_impl` for the parsed args.
///
/// Functional scope:
/// - Honours the clap `action` group: at most one of create/delete/rename/
///   set-upstream/show-current is taken; the default falls through to
///   listing.
///
/// Boundary conditions:
/// - Returns [`BranchError::NotInRepo`] if the CWD is outside a `.libra`
///   repository.
async fn run_branch(args: &BranchArgs) -> Result<BranchOutput, BranchError> {
    require_repo().map_err(|_| BranchError::NotInRepo)?;

    if let Some(upstream) = args.set_upstream_to.as_deref() {
        if args.commit_hash.is_some() {
            return Err(BranchError::TooManyUpstreamArgs);
        }
        let branch = match args.new_branch.as_deref() {
            Some(name) => name.to_string(),
            None => match Head::current().await {
                Head::Branch(name) => name,
                Head::Detached(_) => return Err(detached_head_branch_error()),
            },
        };
        match set_upstream_impl(&branch, upstream).await? {
            SetUpstreamOutcome::Written { local } => Ok(BranchOutput::SetUpstream {
                branch,
                upstream: upstream.to_string(),
                local,
            }),
            SetUpstreamOutcome::Unchanged => Ok(BranchOutput::SetUpstreamUnchanged { branch }),
        }
    } else if let Some(new_branch) = args.new_branch.clone() {
        create_branch_with_optional_track(new_branch, args).await
    } else if let Some(branch_to_delete) = args.delete.clone() {
        delete_branch_impl(branch_to_delete, true).await
    } else if let Some(branch_to_delete) = args.delete_safe.clone() {
        delete_branch_impl(branch_to_delete, false).await
    } else if args.show_current {
        let head = Head::current().await;
        let output = match head {
            Head::Branch(name) => BranchOutput::ShowCurrent {
                name: Some(name),
                detached: false,
                commit: Head::current_commit_result()
                    .await
                    .map_err(map_head_commit_store_error)?
                    .map(|hash| hash.to_string()),
            },
            Head::Detached(hash) => BranchOutput::ShowCurrent {
                name: None,
                detached: true,
                commit: Some(hash.to_string()),
            },
        };
        Ok(output)
    } else if let Some(branch) = args.unset_upstream.as_deref() {
        let branch = if branch.is_empty() {
            match Head::current().await {
                Head::Branch(name) => name,
                Head::Detached(_) => return Err(detached_head_branch_error()),
            }
        } else {
            branch.to_string()
        };
        unset_upstream_impl(&branch).await?;
        Ok(BranchOutput::UnsetUpstream { branch })
    } else if let Some(branch) = args.edit_description.as_deref() {
        let branch = if branch.is_empty() {
            match Head::current().await {
                Head::Branch(name) => name,
                Head::Detached(_) => return Err(detached_head_branch_error()),
            }
        } else {
            branch.to_string()
        };
        let set = edit_description_impl(&branch).await?;
        Ok(BranchOutput::EditDescription { branch, set })
    } else if !args.rename.is_empty() {
        rename_branch_impl(&args.rename).await
    } else if !args.copy.is_empty() {
        copy_branch_impl(&args.copy, false).await
    } else if !args.copy_force.is_empty() {
        copy_branch_impl(&args.copy_force, true).await
    } else {
        collect_branch_output(args).await
    }
}

/// Lay out branch list entries (each already prefixed with the current-branch
/// marker) in dense column-major order to fit `width`, matching
/// `git branch --column`. Column mode uses plain (uncolored) names so the width
/// calculation is accurate.
fn format_branch_columns(entries: &[String], width: usize) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let max_len = entries.iter().map(|e| e.chars().count()).max().unwrap_or(0);
    let col_width = max_len + 2;
    let cols = std::cmp::max(1, width / col_width);
    let rows = entries.len().div_ceil(cols);
    let mut out = String::new();
    for r in 0..rows {
        let mut line = String::new();
        for c in 0..cols {
            let idx = c * rows + r;
            if idx < entries.len() {
                line.push_str(&format!("{:<col_width$}", entries[idx]));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Build the `-v`/`-vv` suffix for a branch's tip commit. The short sha is
/// always shown (Git lists it regardless); the subject is best-effort and
/// omitted if the commit object cannot be loaded. At verbosity >= 2 (`-vv`) the
/// upstream-tracking segment (`[<upstream>: ahead N, behind M]`) is inserted
/// between the sha and the subject for branches with a configured upstream.
async fn branch_verbose_suffix(branch_name: &str, commit_hash: &str, verbose: u8) -> String {
    let short = short_display_hash(commit_hash);
    let subject = commit_hash
        .parse::<ObjectHash>()
        .ok()
        .and_then(|hash| load_object::<Commit>(&hash).ok())
        .map(|commit| {
            crate::common_utils::parse_commit_msg(&commit.message)
                .0
                .lines()
                .next()
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_default();
    let upstream = if verbose >= 2 {
        branch_upstream_segment(branch_name, commit_hash).await
    } else {
        None
    };
    // Assemble `<sha> [<upstream>] <subject>`, omitting empty parts and keeping
    // single-space separation.
    let mut parts = vec![short.to_string()];
    if let Some(segment) = upstream {
        parts.push(segment);
    }
    if !subject.is_empty() {
        parts.push(subject);
    }
    format!(" {}", parts.join(" "))
}

/// Resolve the `[<upstream>: ahead N, behind M]` tracking segment for a local
/// branch (`-vv`). Returns `None` when the branch has no configured upstream.
/// When the remote-tracking ref cannot be resolved (e.g. never fetched), or the
/// counts cannot be computed (a warning says why), the ahead/behind counts are
/// omitted and only `[<upstream>]` is shown.
async fn branch_upstream_segment(branch_name: &str, branch_commit: &str) -> Option<String> {
    let remote = ConfigKv::get(&format!("branch.{branch_name}.remote"))
        .await
        .ok()
        .flatten()
        .map(|e| e.value)?;
    let merge = ConfigKv::get(&format!("branch.{branch_name}.merge"))
        .await
        .ok()
        .flatten()
        .map(|e| e.value)?;
    let merge_short = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
    let upstream_display = format_upstream_ref(&remote, merge_short);
    let remote_ref = if remote == "." {
        format!("refs/heads/{merge_short}")
    } else {
        format!("refs/remotes/{remote}/{merge_short}")
    };

    let counts = match (
        get_target_commit(&remote_ref).await,
        branch_commit.parse::<ObjectHash>(),
    ) {
        (Ok(upstream_commit), Ok(local)) => {
            match super::status::upstream_ahead_behind(&local, &upstream_commit) {
                Ok(counts) => Some(counts),
                Err(reason) => {
                    crate::utils::error::emit_warning(format!(
                        "cannot count commits ahead/behind '{upstream_display}': {reason}"
                    ));
                    None
                }
            }
        }
        _ => None,
    };
    let segment = match counts {
        Some((ahead, behind)) if ahead > 0 && behind > 0 => {
            format!("[{upstream_display}: ahead {ahead}, behind {behind}]")
        }
        Some((ahead, _)) if ahead > 0 => format!("[{upstream_display}: ahead {ahead}]"),
        Some((_, behind)) if behind > 0 => format!("[{upstream_display}: behind {behind}]"),
        _ => format!("[{upstream_display}]"),
    };
    Some(segment)
}

/// Render [`BranchOutput`] for the chosen output mode.
///
/// Functional scope:
/// - JSON mode emits via `emit_json_data`; quiet mode prints nothing.
/// - Human mode formats the list with a `*` prefix on the current branch,
///   keeps default refname order (current branch is not promoted), prints a
///   Git-style detached HEAD row when relevant, and shows an unborn HEAD
///   label as appropriate. `-v`/`-vv` pad the name column to the longest
///   displayed name. `--column` lays the list out in columns instead of one
///   branch per line.
async fn render_branch_output(
    result: &BranchOutput,
    output: &OutputConfig,
    column: Option<&str>,
    verbose: u8,
    format: Option<&str>,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("branch", result, output);
    }
    if output.quiet {
        return Ok(());
    }

    match result {
        BranchOutput::Reset {
            name,
            old_commit,
            new_commit,
            ..
        } => {
            println!(
                "Reset branch '{name}' (was {}, now {})",
                crate::utils::text::short_display_hash(old_commit),
                crate::utils::text::short_display_hash(new_commit)
            );
        }
        BranchOutput::List {
            branches,
            head_name,
            detached_head,
            show_unborn_head,
            ignore_case,
            sorted: presorted,
        } => {
            // Order the entries (shared by `--format` and the default listing).
            // `--sort` / `branch.sort` / the default refname pass already ordered
            // them; a leftover unsorted payload still uses refname, not
            // current-first.
            let mut sorted = branches.clone();
            if !*presorted {
                sort_entries_default_refname(&mut sorted, *ignore_case);
            }

            // `--format`: render each branch via the for-each-ref atom engine,
            // replacing the default `* name` listing, `-v`, and `--column`. A
            // remote entry's `plain_name` carries the `<remote>/<branch>` form
            // (so `plain_name != name`); a local entry's matches its `name`.
            if let Some(fmt) = format {
                use std::io::IsTerminal;
                let color_enabled = match output.color {
                    crate::utils::output::ColorChoice::Always => true,
                    crate::utils::output::ColorChoice::Never => false,
                    crate::utils::output::ColorChoice::Auto => std::io::stdout().is_terminal(),
                };
                let refs: Vec<(String, String)> = sorted
                    .iter()
                    .map(|b| {
                        let refname = if b.name.starts_with("refs/") {
                            b.name.clone()
                        } else {
                            format!("refs/heads/{}", b.name)
                        };
                        (refname, b.commit.clone())
                    })
                    .collect();
                let lines =
                    super::for_each_ref::render_ref_format_lines(&refs, fmt, color_enabled).await?;
                for line in lines {
                    println!("{line}");
                }
                return Ok(());
            }

            let detached_label = detached_head
                .as_ref()
                .map(|oid| format!("(HEAD detached at {})", short_display_hash(oid)));
            if *show_unborn_head && let Some(head_name) = head_name {
                println!("* {}", head_name.green());
            }
            if branches.is_empty() && detached_label.is_none() {
                return Ok(());
            }

            // `-v` (per-branch sha + subject) takes precedence over `--column`
            // (which is a names-only dense layout).
            let column_enabled = match column {
                Some(mode) => super::tag::resolve_column_enabled(mode)? && verbose == 0,
                None => false,
            };
            if column_enabled {
                // Plain names (current branch marked `* `) laid out in columns.
                let mut entries: Vec<String> = Vec::new();
                if let Some(label) = detached_label.as_ref() {
                    entries.push(format!("* {label}"));
                }
                entries.extend(sorted.iter().map(|branch| {
                    if branch.current {
                        format!("* {}", branch.plain_name)
                    } else {
                        format!("  {}", branch.plain_name)
                    }
                }));
                let width = super::tag::column_layout_width();
                print!("{}", format_branch_columns(&entries, width));
            } else {
                let name_width = if verbose >= 1 {
                    list_name_column_width(detached_label.as_deref(), &sorted)
                } else {
                    0
                };
                if let Some(label) = detached_label.as_ref() {
                    let name = format_list_name_column(label, name_width, verbose >= 1);
                    let suffix = if verbose >= 1 {
                        if let Some(oid) = detached_head.as_ref() {
                            branch_verbose_suffix("", oid, verbose).await
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    };
                    println!("* {}{suffix}", name.green());
                }
                for branch in sorted {
                    // Remote HEAD symlinks never carry `-v`/`-vv` tip metadata
                    // (matches `git branch -a -v`: `remotes/origin/HEAD -> …`).
                    let suffix = if verbose >= 1 && !is_remote_head_symlink(&branch) {
                        branch_verbose_suffix(&branch.name, &branch.commit, verbose).await
                    } else {
                        String::new()
                    };
                    let name =
                        format_list_name_column(&branch.plain_name, name_width, verbose >= 1);
                    if branch.current {
                        println!("* {}{suffix}", name.green());
                    } else if is_remote_list_entry(&branch) {
                        println!("  {}{suffix}", name.red());
                    } else {
                        println!("  {}{suffix}", name);
                    }
                }
            }
        }
        BranchOutput::Create { name, commit } => {
            println!("Created branch '{name}' at {}", short_display_hash(commit));
        }
        BranchOutput::Delete {
            name,
            commit,
            force: _,
        } => {
            println!(
                "Deleted branch {name} (was {}).",
                short_display_hash(commit)
            );
        }
        BranchOutput::Rename { old_name, new_name } => {
            println!("Renamed branch '{old_name}' to '{new_name}'");
        }
        BranchOutput::Copy { old_name, new_name } => {
            println!("Copied branch '{old_name}' to '{new_name}'");
        }
        BranchOutput::SetUpstream {
            branch,
            upstream,
            local,
        } => {
            if *local {
                println!("branch '{branch}' set up to track '{upstream}'.");
            } else {
                println!("Branch '{branch}' set up to track remote branch '{upstream}'");
            }
        }
        BranchOutput::SetUpstreamUnchanged { .. } => {}
        BranchOutput::UnsetUpstream { branch } => {
            println!("Branch '{branch}' no longer tracks an upstream branch");
        }
        BranchOutput::EditDescription { branch, set } => {
            if *set {
                println!("Updated description for branch '{branch}'");
            } else {
                println!("Removed description for branch '{branch}'");
            }
        }
        BranchOutput::ShowCurrent {
            name,
            detached,
            commit,
        } => {
            if *detached {
                if let Some(commit) = commit {
                    println!("HEAD detached at {}", short_display_hash(commit));
                }
            } else if let Some(name) = name {
                println!("{name}");
            }
        }
    }

    Ok(())
}

/// Public helper for callers (clone, fetch) that need to wire up an upstream.
/// Prints any error to stderr but does not propagate the failure.
pub async fn set_upstream(branch: &str, upstream: &str) {
    if let Err(err) = set_upstream_safe(branch, upstream).await {
        err.print_stderr();
    }
}

/// Structured variant of [`set_upstream`] using the default output config.
pub async fn set_upstream_safe(branch: &str, upstream: &str) -> CliResult<()> {
    set_upstream_safe_with_output(branch, upstream, &OutputConfig::default()).await
}

/// Structured variant that respects the provided [`OutputConfig`]
/// (used by `clone`/`fetch` so quiet mode is honoured).
pub async fn set_upstream_safe_with_output(
    branch: &str,
    upstream: &str,
    output: &OutputConfig,
) -> CliResult<()> {
    match set_upstream_impl(branch, upstream)
        .await
        .map_err(CliError::from)?
    {
        SetUpstreamOutcome::Written { local } => {
            if local {
                info_println!(output, "branch '{branch}' set up to track '{upstream}'.");
            } else {
                info_println!(
                    output,
                    "Branch '{branch}' set up to track remote branch '{upstream}'"
                );
            }
        }
        SetUpstreamOutcome::Unchanged => {}
    }
    Ok(())
}

/// Public helper for callers that need to create a branch programmatically
/// (clone, etc.). Suppresses errors to stderr.
pub async fn create_branch(new_branch: String, branch_or_commit: Option<String>) {
    if let Err(err) = create_branch_safe(new_branch, branch_or_commit).await {
        err.print_stderr();
    }
}

/// Structured variant of [`create_branch`].
///
/// Functional scope:
/// - Calls [`create_branch_impl`] and discards the [`BranchOutput`]; just
///   returns success/failure.
pub async fn create_branch_safe(
    new_branch: String,
    branch_or_commit: Option<String>,
) -> CliResult<()> {
    create_branch_impl(new_branch, branch_or_commit, false)
        .await
        .map(|_| ())
        .map_err(CliError::from)?;
    Ok(())
}

/// Render a branch's display label for the human-mode listing.
///
/// Functional scope:
/// - Strips the `refs/remotes/` prefix when present.
/// - Falls back to `<remote>/<short>` when `branch.remote` is set, else the
///   bare name.
/// - Colors the result red to distinguish remote branches in the list.
///
/// See: tests::test_format_branch_name_with_full_remote_ref in
/// src/command/branch.rs:1062;
/// tests::test_format_branch_name_with_short_remote_ref in
/// src/command/branch.rs:1076.
fn format_branch_name(branch: &Branch) -> String {
    plain_branch_display_name(branch).red().to_string()
}

/// The human-readable branch label without color (remote refs have their
/// `refs/remotes/` prefix stripped, or are shown as `<remote>/<name>`). Used for
/// the colorless `--column` layout so width calculation is accurate.
fn plain_branch_display_name(branch: &Branch) -> String {
    if let Some(stripped) = branch.name.strip_prefix("refs/remotes/") {
        stripped.to_string()
    } else {
        branch
            .remote
            .as_ref()
            .map(|remote| format!("{remote}/{}", branch.name))
            .unwrap_or_else(|| branch.name.clone())
    }
}

/// List branches with the given mode and commit filters, rendering directly to stdout.
///
/// This is a convenience wrapper around the structured `run_branch` path,
/// kept for backward compatibility with callers that need a simple
/// "print branches" operation.
pub async fn list_branches(
    list_mode: BranchListMode,
    commits_contains: &[String],
    commits_no_contains: &[String],
) -> CliResult<()> {
    let args = BranchArgs {
        subcommand: None,
        no_column: false,
        new_branch: None,
        commit_hash: None,
        list: true,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
        track: None,
        no_track: false,
        unset_upstream: None,
        edit_description: None,
        show_current: false,
        rename: vec![],
        copy: vec![],
        copy_force: vec![],
        remotes: matches!(list_mode, BranchListMode::Remote),
        all: matches!(list_mode, BranchListMode::All),
        contains: commits_contains.to_vec(),
        no_contains: commits_no_contains.to_vec(),
        points_at: None,
        merged: None,
        no_merged: None,
        sort: None,
        ignore_case: false,
        format: None,
        column: None,
        verbose: 0,
    };
    let result = collect_branch_output(&args).await.map_err(CliError::from)?;
    render_branch_output(
        &result,
        &OutputConfig::default(),
        args.column.as_deref(),
        args.verbose,
        args.format.as_deref(),
    )
    .await
}

/// Filter given branches by whether they contain or don't contain certain commits.
///
/// Internal test helper — not part of the stable public API.
#[doc(hidden)]
pub fn filter_branches(
    branches: &mut Vec<Branch>,
    contains_set: &HashSet<ObjectHash>,
    no_contains_set: &HashSet<ObjectHash>,
) -> CliResult<()> {
    filter_branches_result(branches, contains_set, no_contains_set).map_err(CliError::from)
}

fn filter_branches_result(
    branches: &mut Vec<Branch>,
    contains_set: &HashSet<ObjectHash>,
    no_contains_set: &HashSet<ObjectHash>,
) -> Result<(), BranchError> {
    // Filter branches, propagating errors.
    // `retain` doesn't support fallible predicates, so we capture the first
    // error and short-circuit the remaining iterations.
    let mut error: Option<BranchError> = None;
    branches.retain(|branch| {
        if error.is_some() {
            return false;
        }
        let contains_ok = contains_set.is_empty()
            || match commit_contains(branch, contains_set) {
                Ok(v) => v,
                Err(e) => {
                    error = Some(e);
                    return false;
                }
            };
        let no_contains_ok = no_contains_set.is_empty()
            || match commit_contains(branch, no_contains_set) {
                Ok(v) => !v,
                Err(e) => {
                    error = Some(e);
                    return false;
                }
            };
        contains_ok && no_contains_ok
    });
    if let Some(e) = error {
        return Err(e);
    }
    Ok(())
}

/// Resolve commit references to ObjectHash set.
async fn resolve_commits(commits: &[String]) -> Result<HashSet<ObjectHash>, BranchError> {
    let mut set = HashSet::new();
    for commit in commits {
        let target_commit = get_target_commit(commit)
            .await
            .map_err(|_| BranchError::InvalidCommit(commit.clone()))?;
        set.insert(target_commit);
    }
    Ok(set)
}

/// Sort branch list entries in place by `--sort` key. Supports `refname`,
/// `version:refname` / `v:refname` (numeric-aware), `committerdate` /
/// `creatordate` / `authordate` (the tip commit's date), `objectsize` (the tip
/// object's byte size), and `objectname` (the tip commit's object id), with a
/// leading `-` reversing. Unknown keys are a usage error.
/// Read the Git-compatible `branch.sort` default through the strict
/// local → global → system cascade (P1-05d). Empty values are rejected by the
/// caller via [`sort_branch_entries`]'s key validation.
async fn configured_branch_sort() -> Result<Option<String>, BranchError> {
    crate::internal::config::read_cascaded_config_value_strict(
        crate::internal::config::LocalIdentityTarget::CurrentRepo,
        "branch.sort",
    )
    .await
    .map(|value| value.map(|v| v.trim().to_string()))
    .map_err(|error| BranchError::SortConfigRead {
        key: "branch.sort",
        detail: format!("{error:#}"),
    })
}

fn is_remote_list_entry(entry: &BranchListEntry) -> bool {
    entry.name.starts_with("refs/remotes/")
}

/// `refs/remotes/<remote>/HEAD` list rows render as `<…>/HEAD -> <remote>/<branch>`.
fn is_remote_head_symlink(entry: &BranchListEntry) -> bool {
    entry.name.ends_with("/HEAD") && entry.plain_name.contains(" -> ")
}

/// Full remote-tracking ref for a short or already-qualified branch name.
fn remote_tracking_refname(remote: &str, branch_name: &str) -> String {
    if branch_name.starts_with("refs/remotes/") {
        return branch_name.to_string();
    }
    let short = branch_name
        .strip_prefix("refs/heads/")
        .unwrap_or(branch_name);
    format!("refs/remotes/{remote}/{short}")
}

fn remote_tracking_short_name(branch_name: &str) -> String {
    branch_name
        .strip_prefix("refs/heads/")
        .or_else(|| {
            branch_name
                .strip_prefix("refs/remotes/")
                .and_then(|rest| rest.split_once('/').map(|(_, name)| name))
        })
        .unwrap_or(branch_name)
        .to_string()
}

/// Human label for a remote HEAD symlink (`-a` keeps the `remotes/` prefix).
fn remote_head_symlink_plain_name(
    list_mode: BranchListMode,
    remote: &str,
    short_target: &str,
) -> String {
    let arrow = format!("{remote}/HEAD -> {remote}/{short_target}");
    match list_mode {
        BranchListMode::All => format!("remotes/{arrow}"),
        BranchListMode::Remote => arrow,
        BranchListMode::Local => arrow,
    }
}

fn sort_entries_default_refname(entries: &mut [BranchListEntry], ignore_case: bool) {
    entries.sort_by(
        |a, b| match (is_remote_list_entry(a), is_remote_list_entry(b)) {
            (false, true) => std::cmp::Ordering::Less,
            (true, false) => std::cmp::Ordering::Greater,
            _ => {
                if ignore_case {
                    a.plain_name
                        .to_lowercase()
                        .cmp(&b.plain_name.to_lowercase())
                } else {
                    a.plain_name.cmp(&b.plain_name)
                }
            }
        },
    );
}

fn list_name_column_width(detached_label: Option<&str>, branches: &[BranchListEntry]) -> usize {
    branches
        .iter()
        .map(|branch| branch.plain_name.chars().count())
        .chain(detached_label.map(|label| label.chars().count()))
        .max()
        .unwrap_or(0)
}

fn format_list_name_column(name: &str, width: usize, pad: bool) -> String {
    if pad && width > name.chars().count() {
        format!("{name:<width$}")
    } else {
        name.to_string()
    }
}

fn sort_branch_entries(
    entries: &mut [BranchListEntry],
    key: &str,
    ignore_case: bool,
) -> Result<(), BranchError> {
    use std::str::FromStr;

    let (base, reverse) = match key.strip_prefix('-') {
        Some(rest) => (rest, true),
        None => (key, false),
    };

    // Date keys sort by the tip commit's date: `committerdate`/`creatordate` use
    // its committer date (for a branch ref to a commit, Git's creatordate is the
    // committer date), `authordate` its author date. Pre-load the timestamps once
    // (keyed by tip-commit hash) so the comparator is a cheap lookup; a commit
    // that fails to load contributes timestamp 0 (sorts oldest) rather than
    // aborting the listing.
    let is_date_key = matches!(base, "committerdate" | "creatordate" | "authordate");
    let use_author_date = base == "authordate";
    let timestamps: HashMap<String, i64> = if is_date_key {
        let mut map = HashMap::new();
        for entry in entries.iter() {
            if map.contains_key(&entry.commit) {
                continue;
            }
            let ts = ObjectHash::from_str(&entry.commit)
                .ok()
                .and_then(|hash| load_object::<Commit>(&hash).ok())
                .map(|commit| {
                    if use_author_date {
                        commit.author.timestamp as i64
                    } else {
                        commit.committer.timestamp as i64
                    }
                })
                .unwrap_or(0);
            map.insert(entry.commit.clone(), ts);
        }
        map
    } else {
        HashMap::new()
    };

    // `objectsize` sorts by the tip object's decompressed byte size (Git's
    // for-each-ref `objectsize`). Pre-loaded like the date keys; an unreadable
    // object contributes size 0.
    let sizes: HashMap<String, i64> = if base == "objectsize" {
        let mut map = HashMap::new();
        for entry in entries.iter() {
            if map.contains_key(&entry.commit) {
                continue;
            }
            let size = ObjectHash::from_str(&entry.commit)
                .ok()
                .and_then(|hash| crate::utils::util::objects_storage().get(&hash).ok())
                .map(|data| data.len() as i64)
                .unwrap_or(0);
            map.insert(entry.commit.clone(), size);
        }
        map
    } else {
        HashMap::new()
    };

    if !matches!(
        base,
        "refname"
            | "version:refname"
            | "v:refname"
            | "committerdate"
            | "creatordate"
            | "authordate"
            | "objectsize"
            | "objectname"
    ) {
        return Err(BranchError::InvalidSortKey(base.to_string()));
    }

    // The PRIMARY key comparison; the refname tie-break is appended afterward so
    // that `-` reverses only the primary key (Git keeps the refname tie-break
    // ascending under a reversed sort).
    let primary = |a: &BranchListEntry, b: &BranchListEntry| -> std::cmp::Ordering {
        match base {
            "refname" => {
                if ignore_case {
                    a.name.to_lowercase().cmp(&b.name.to_lowercase())
                } else {
                    a.name.cmp(&b.name)
                }
            }
            "version:refname" | "v:refname" => {
                crate::utils::util::version_refname_cmp(&a.name, &b.name)
            }
            // Date keys compare the tip commit's committer/author timestamp.
            "committerdate" | "creatordate" | "authordate" => {
                let ta = timestamps.get(&a.commit).copied().unwrap_or(0);
                let tb = timestamps.get(&b.commit).copied().unwrap_or(0);
                ta.cmp(&tb)
            }
            // Object size compares the tip object's byte size.
            "objectsize" => {
                let sa = sizes.get(&a.commit).copied().unwrap_or(0);
                let sb = sizes.get(&b.commit).copied().unwrap_or(0);
                sa.cmp(&sb)
            }
            // `objectname` compares the tip commit's object id. Branch hashes are
            // equal-length hex, so a lexicographic string compare matches Git's
            // binary-oid ordering; equal ids fall through to the refname tie-break.
            "objectname" => a.commit.cmp(&b.commit),
            _ => std::cmp::Ordering::Equal,
        }
    };
    // `refname`/`version:refname` ARE the name, so reversing them flips the whole
    // order; the date/size keys reverse only the primary and keep the refname
    // tie-break ascending (matching Git's for-each-ref).
    let name_is_primary = matches!(base, "refname" | "version:refname" | "v:refname");
    entries.sort_by(|a, b| {
        let mut ord = primary(a, b);
        if reverse {
            ord = ord.reverse();
        }
        if name_is_primary {
            ord
        } else {
            ord.then_with(|| a.name.cmp(&b.name))
        }
    });
    Ok(())
}

/// For `--merged`/`--no-merged`: resolve the target spec and return the set of
/// commits reachable from it (its history). A branch is "merged into" the
/// target iff its tip is in this set. Returns `None` when the flag is absent.
async fn resolve_reachable_for_merge(
    spec: Option<&str>,
) -> Result<Option<HashSet<ObjectHash>>, BranchError> {
    let Some(spec) = spec else {
        return Ok(None);
    };
    let target = get_target_commit(spec)
        .await
        .map_err(|_| BranchError::InvalidCommit(spec.to_string()))?;
    let reachable = get_reachable_commits(target.to_string(), None)
        .await
        .map_err(BranchError::DelegatedCli)?;
    Ok(Some(reachable.iter().map(|c| c.id).collect()))
}

/// check if a branch contains at least one of the commits
///
/// NOTE: returns `false` if `commits` is empty
fn commit_contains(
    branch: &Branch,
    target_commits: &HashSet<ObjectHash>,
) -> Result<bool, BranchError> {
    // do BFS to find out whether `branch` contains `target_commit` or not
    let mut q = VecDeque::new();
    let mut visited = HashSet::new();

    q.push_back(branch.commit);
    visited.insert(branch.commit);

    while let Some(current_commit) = q.pop_front() {
        // found target commit
        if target_commits.contains(&current_commit) {
            return Ok(true);
        }

        // enqueue all parent commits of `current_commit`
        let current_commit_object: Commit =
            load_object(&current_commit).map_err(|error| BranchError::CommitLoadFailed {
                commit: current_commit.to_string(),
                detail: error.to_string(),
            })?;
        for parent_commit in current_commit_object.parent_commit_ids {
            if !visited.contains(&parent_commit) {
                visited.insert(parent_commit);
                q.push_back(parent_commit);
            }
        }
    }

    // contains no commits
    Ok(false)
}

pub fn is_valid_git_branch_name(name: &str) -> bool {
    // Validate branch name
    // Not contain spaces, control characters or special characters
    if name.contains(&[' ', '\t', '\\', ':', '"', '?', '*', '['][..])
        || name.chars().any(|c| c.is_ascii_control())
    {
        return false;
    }

    // Not start or end with a slash ('/'), or end with a dot ('.')
    // Not contain consecutive slashes ('//') or dots ('..')
    if name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.contains("//")
        || name.contains("..")
    {
        return false;
    }

    // Not be reserved names like 'HEAD' or contain '@{'
    if name == "HEAD" || name.contains("@{") {
        return false;
    }

    // Not be empty or just a dot ('.')
    if name.trim().is_empty() || name.trim() == "." {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, str::FromStr};

    use clap::Parser;
    use git_internal::hash::{ObjectHash, get_hash_kind};
    use sea_orm::Database;
    use serial_test::serial;

    use super::{
        Branch, BranchArgs, BranchError, BranchListEntry, BranchListMode, ResolvedUpstream,
        clean_branch_description, commit_contains, format_branch_name, format_list_name_column,
        format_upstream_ref, is_remote_head_symlink, list_name_column_width,
        load_remote_branches_with_conn, map_head_commit_store_error,
        remote_head_symlink_plain_name, resolve_upstream_spec, set_upstream_impl,
        sort_entries_default_refname,
    };
    use crate::utils::{
        error::{CliError, StableErrorCode},
        test,
    };

    #[test]
    fn clean_branch_description_uses_stripspace_semantics() {
        // Lines whose FIRST char is `#` are dropped; surrounding blanks removed.
        assert_eq!(
            clean_branch_description("my feature\n# a comment\n"),
            "my feature"
        );
        // A comment-only / blank buffer collapses to empty (which unsets the key).
        assert_eq!(
            clean_branch_description("# Please edit\n#   branch\n\n  \n"),
            ""
        );
        // An INDENTED `#` line is content (only a first-char `#` is a comment)
        // and its leading indentation is preserved; trailing whitespace per line
        // is trimmed; runs of blank lines collapse to one; leading/trailing
        // blanks are dropped.
        assert_eq!(
            clean_branch_description(
                "\n\n  keep me   \n  # indented heading\n\n\nlast\t\n# real comment\n\n"
            ),
            "  keep me\n  # indented heading\n\nlast"
        );
    }

    #[test]
    #[serial_test::serial(cwd)]
    fn edit_description_flag_parses_optional_branch() {
        // Bare flag defaults to "" (the current branch).
        let args = BranchArgs::try_parse_from(["branch", "--edit-description"]).unwrap();
        assert_eq!(args.edit_description.as_deref(), Some(""));
        // An explicit branch name is captured.
        let args = BranchArgs::try_parse_from(["branch", "--edit-description", "feature"]).unwrap();
        assert_eq!(args.edit_description.as_deref(), Some("feature"));
        // Absent when not requested.
        let args = BranchArgs::try_parse_from(["branch"]).unwrap();
        assert_eq!(args.edit_description, None);
    }

    #[test]
    fn set_upstream_flag_parses_optional_target_branch() {
        let args = BranchArgs::try_parse_from(["branch", "-u", "main", "alpha"]).unwrap();
        assert_eq!(args.set_upstream_to.as_deref(), Some("main"));
        assert_eq!(args.new_branch.as_deref(), Some("alpha"));
        assert_eq!(args.commit_hash, None);

        let args = BranchArgs::try_parse_from(["branch", "--set-upstream-to=main"]).unwrap();
        assert_eq!(args.set_upstream_to.as_deref(), Some("main"));
        assert_eq!(args.new_branch, None);

        let args = BranchArgs::try_parse_from(["branch", "-u", "main", "alpha", "extra"]).unwrap();
        assert_eq!(args.set_upstream_to.as_deref(), Some("main"));
        assert_eq!(args.new_branch.as_deref(), Some("alpha"));
        assert_eq!(args.commit_hash.as_deref(), Some("extra"));
    }

    #[test]
    fn track_flag_parses_optional_equals_mode() {
        let args = BranchArgs::try_parse_from(["branch", "--track", "t1", "main"]).unwrap();
        assert_eq!(args.track, Some(super::BranchTrackMode::Direct));
        assert_eq!(args.new_branch.as_deref(), Some("t1"));
        assert_eq!(args.commit_hash.as_deref(), Some("main"));

        let args = BranchArgs::try_parse_from(["branch", "-t", "t2", "main"]).unwrap();
        assert_eq!(args.track, Some(super::BranchTrackMode::Direct));
        assert_eq!(args.new_branch.as_deref(), Some("t2"));

        let args = BranchArgs::try_parse_from(["branch", "--track=inherit", "t3", "t1"]).unwrap();
        assert_eq!(args.track, Some(super::BranchTrackMode::Inherit));
        assert_eq!(args.new_branch.as_deref(), Some("t3"));
        assert_eq!(args.commit_hash.as_deref(), Some("t1"));

        let args = BranchArgs::try_parse_from(["branch", "--no-track", "t4", "main"]).unwrap();
        assert!(args.no_track);
        assert_eq!(args.track, None);
        assert_eq!(args.new_branch.as_deref(), Some("t4"));
    }

    #[test]
    fn format_upstream_ref_omits_dot_remote() {
        assert_eq!(format_upstream_ref(".", "main"), "main");
        assert_eq!(format_upstream_ref("origin", "main"), "origin/main");
    }

    #[tokio::test]
    #[serial(cwd)]
    async fn resolve_upstream_spec_prefers_remote_then_local() {
        let repo = tempfile::tempdir().expect("temp repo");
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        crate::command::commit::execute(crate::command::commit::CommitArgs {
            message: Some("base".to_string()),
            allow_empty: true,
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;

        let head = crate::internal::head::Head::current_commit()
            .await
            .expect("HEAD commit");
        crate::internal::branch::Branch::update_branch("alpha", &head.to_string(), None)
            .await
            .expect("create alpha");
        crate::internal::config::ConfigKv::set(
            "remote.origin.url",
            "https://example.invalid/repo.git",
            false,
        )
        .await
        .expect("configure origin");
        crate::internal::branch::Branch::update_branch(
            "refs/remotes/origin/main",
            &head.to_string(),
            Some("origin"),
        )
        .await
        .expect("create origin/main");

        assert_eq!(
            resolve_upstream_spec("origin/main").await.expect("remote"),
            ResolvedUpstream::Remote {
                remote: "origin".to_string(),
                merge: "main".to_string(),
            }
        );
        assert_eq!(
            resolve_upstream_spec("alpha").await.expect("local"),
            ResolvedUpstream::Local {
                name: "alpha".to_string(),
            }
        );
        let missing = resolve_upstream_spec("nosuch").await.expect_err("missing");
        assert!(matches!(missing, BranchError::UpstreamMissing(_)));
    }

    #[tokio::test]
    #[serial(cwd)]
    async fn set_upstream_impl_warns_on_self_upstream() {
        let repo = tempfile::tempdir().expect("temp repo");
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        crate::command::commit::execute(crate::command::commit::CommitArgs {
            message: Some("base".to_string()),
            allow_empty: true,
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;

        let current = match crate::internal::head::Head::current().await {
            crate::internal::head::Head::Branch(name) => name,
            crate::internal::head::Head::Detached(_) => panic!("expected a named branch"),
        };
        let outcome = set_upstream_impl(&current, &current)
            .await
            .expect("self-upstream is a warning, not an error");
        assert!(matches!(outcome, super::SetUpstreamOutcome::Unchanged));
        assert!(
            crate::internal::config::ConfigKv::get(&format!("branch.{current}.remote"))
                .await
                .expect("read")
                .is_none(),
            "self-upstream must not write branch.*.remote"
        );
    }

    struct ColorOverrideReset;

    impl Drop for ColorOverrideReset {
        fn drop(&mut self) {
            colored::control::unset_override();
        }
    }

    fn any_hash() -> ObjectHash {
        ObjectHash::from_str(&ObjectHash::zero_str(get_hash_kind())).unwrap()
    }

    /// Pin the `Display` format for the static-message and direct-message
    /// variants of [`BranchError`]. These strings are used directly as
    /// the `CliError` message via `From<BranchError> for CliError` and
    /// surface in both human and `--json` envelopes.
    ///
    /// Source-chained / wrapper variants (ConfigReadFailed,
    /// ConfigWriteFailed, StorageQueryFailed, StoredReferenceCorrupt,
    /// CreateFailed, DeleteFailed, CommitLoadFailed, DelegatedCli) wrap upstream error
    /// messages and are intentionally skipped — their content is owned
    /// by the wrapped type.
    #[test]
    fn branch_error_display_pins_static_message_variants() {
        assert_eq!(BranchError::NotInRepo.to_string(), "not a libra repository");
        assert_eq!(
            BranchError::InvalidName("@bad name".to_string()).to_string(),
            "'@bad name' is not a valid branch name",
        );
        assert_eq!(
            BranchError::AlreadyExists("feature".to_string()).to_string(),
            "a branch named 'feature' already exists",
        );
        assert_eq!(
            BranchError::NotFound {
                name: "topic/x".to_string(),
                similar: vec![],
            }
            .to_string(),
            "branch 'topic/x' not found",
        );
        assert_eq!(
            BranchError::DeleteNotFound {
                name: "topic/x".to_string(),
                similar: vec![],
            }
            .to_string(),
            "branch 'topic/x' not found",
        );
        assert_eq!(
            BranchError::DeleteCurrent("main".to_string()).to_string(),
            "Cannot delete the branch 'main' which you are currently on",
        );
        assert_eq!(
            BranchError::NotFullyMerged("feature".to_string()).to_string(),
            "the branch 'feature' is not fully merged",
        );
        assert_eq!(
            BranchError::Locked("intent".to_string()).to_string(),
            "the 'intent' branch is locked and cannot be modified",
        );
        assert_eq!(BranchError::DetachedHead.to_string(), "HEAD is detached");
        assert_eq!(
            BranchError::InvalidCommit("deadbeef".to_string()).to_string(),
            "not a valid object name: 'deadbeef'",
        );
        assert_eq!(
            BranchError::UpstreamMissing("nosuch".to_string()).to_string(),
            "the requested upstream branch 'nosuch' does not exist",
        );
        assert_eq!(
            BranchError::UpstreamTargetMissing("nosuchbranch".to_string()).to_string(),
            "branch 'nosuchbranch' does not exist",
        );
        assert_eq!(
            BranchError::TooManyUpstreamArgs.to_string(),
            "too many arguments to set new upstream",
        );
        assert_eq!(
            BranchError::TrackStartNotBranch("abc1234".to_string()).to_string(),
            "cannot set up tracking information; starting point 'abc1234' is not a branch",
        );
        assert_eq!(
            BranchError::RenameTooManyArgs.to_string(),
            "too many arguments",
        );
    }

    #[test]
    fn delete_refusal_errors_override_exit_to_one() {
        let not_merged = CliError::from(BranchError::NotFullyMerged("feat".to_string()));
        assert_eq!(not_merged.exit_code(), 1);
        assert_eq!(not_merged.stable_code(), StableErrorCode::RepoStateInvalid);

        let current = CliError::from(BranchError::DeleteCurrent("main".to_string()));
        assert_eq!(current.exit_code(), 1);
        assert_eq!(current.stable_code(), StableErrorCode::RepoStateInvalid);

        let missing = CliError::from(BranchError::DeleteNotFound {
            name: "ghost".to_string(),
            similar: vec![],
        });
        assert_eq!(missing.exit_code(), 1);
        assert_eq!(missing.stable_code(), StableErrorCode::CliInvalidTarget);

        let other_missing = CliError::from(BranchError::NotFound {
            name: "ghost".to_string(),
            similar: vec![],
        });
        assert_eq!(other_missing.exit_code(), 129);
    }

    #[test]
    #[serial(cwd, env)]
    fn commit_contains_surfaces_typed_commit_load_failure() {
        let repo = tempfile::tempdir().expect("temp repo");
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(test::setup_with_new_libra_in(repo.path()));
        let _guard = test::ChangeDirGuard::new(repo.path());

        let corrupt_commit = any_hash();
        let branch = Branch {
            name: "corrupt".to_string(),
            commit: corrupt_commit,
            remote: None,
        };
        let mut targets = HashSet::new();
        targets.insert(
            ObjectHash::from_str(
                "1111111111111111111111111111111111111111111111111111111111111111",
            )
            .unwrap(),
        );

        let error = commit_contains(&branch, &targets)
            .expect_err("corrupt branch commit should fail traversal");
        let BranchError::CommitLoadFailed { commit, .. } = &error else {
            panic!("expected CommitLoadFailed, got: {error:?}");
        };
        assert_eq!(commit, &corrupt_commit.to_string());
        assert_eq!(
            CliError::from(error).stable_code(),
            StableErrorCode::RepoCorrupt
        );
    }

    #[test]
    #[serial]
    fn test_format_branch_name_with_full_remote_ref() {
        let _guard = ColorOverrideReset;
        colored::control::set_override(false);
        let branch = Branch {
            name: "refs/remotes/origin/main".to_string(),
            commit: any_hash(),
            remote: Some("origin".to_string()),
        };

        assert_eq!(format_branch_name(&branch), "origin/main");
    }

    #[test]
    #[serial]
    fn test_format_branch_name_with_short_remote_ref() {
        let _guard = ColorOverrideReset;
        colored::control::set_override(false);
        let branch = Branch {
            name: "main".to_string(),
            commit: any_hash(),
            remote: Some("origin".to_string()),
        };

        assert_eq!(format_branch_name(&branch), "origin/main");
    }

    #[tokio::test]
    async fn test_load_remote_branches_with_conn_surfaces_config_read_failure() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.clone().close().await.unwrap();

        let error = load_remote_branches_with_conn(&db).await.unwrap_err();
        match error {
            BranchError::ConfigReadFailed(detail) => {
                assert!(detail.contains("failed to read remote configuration"));
            }
            other => panic!("expected config read failure, got {other:?}"),
        }
    }

    #[test]
    fn test_head_commit_query_error_maps_to_io_read_failed() {
        let cli_error = CliError::from(map_head_commit_store_error(
            crate::internal::branch::BranchStoreError::Query("database is locked".into()),
        ));
        assert_eq!(cli_error.stable_code(), StableErrorCode::IoReadFailed);
    }

    fn list_entry(name: &str, plain: &str) -> BranchListEntry {
        BranchListEntry {
            name: name.to_string(),
            current: false,
            commit: any_hash().to_string(),
            display_name: plain.to_string(),
            plain_name: plain.to_string(),
        }
    }

    #[test]
    fn default_list_order_locals_then_remotes_by_plain_name() {
        let mut entries = vec![
            list_entry("zeta", "zeta"),
            list_entry("refs/remotes/origin/main", "remotes/origin/main"),
            list_entry("main", "main"),
            list_entry("refs/remotes/origin/dev", "remotes/origin/dev"),
            list_entry(
                "refs/remotes/origin/HEAD",
                "remotes/origin/HEAD -> origin/main",
            ),
            list_entry("alpha", "alpha"),
        ];
        sort_entries_default_refname(&mut entries, false);
        let names: Vec<&str> = entries
            .iter()
            .map(|entry| entry.plain_name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "alpha",
                "main",
                "zeta",
                "remotes/origin/HEAD -> origin/main",
                "remotes/origin/dev",
                "remotes/origin/main"
            ]
        );
    }

    #[test]
    fn remote_head_symlink_plain_name_matches_git() {
        assert_eq!(
            remote_head_symlink_plain_name(BranchListMode::All, "origin", "main"),
            "remotes/origin/HEAD -> origin/main"
        );
        assert_eq!(
            remote_head_symlink_plain_name(BranchListMode::Remote, "origin", "main"),
            "origin/HEAD -> origin/main"
        );
    }

    #[test]
    fn is_remote_head_symlink_detects_arrow_rows() {
        let head = list_entry(
            "refs/remotes/origin/HEAD",
            "remotes/origin/HEAD -> origin/main",
        );
        let ordinary = list_entry("refs/remotes/origin/main", "remotes/origin/main");
        assert!(is_remote_head_symlink(&head));
        assert!(!is_remote_head_symlink(&ordinary));
    }

    #[test]
    fn list_name_column_width_includes_detached_and_remote_labels() {
        let entries = vec![
            list_entry("main", "main"),
            list_entry("refs/remotes/origin/main", "remotes/origin/main"),
        ];
        let detached = "(HEAD detached at abcdef0)";
        assert_eq!(
            list_name_column_width(Some(detached), &entries),
            detached.chars().count()
        );
        assert_eq!(
            list_name_column_width(Some("x"), &entries),
            "remotes/origin/main".chars().count()
        );
        assert_eq!(
            list_name_column_width(Some(detached), &[]),
            detached.chars().count()
        );
        assert_eq!(
            format_list_name_column("main", 20, true),
            format!("{:<20}", "main")
        );
        assert_eq!(format_list_name_column("main", 20, false), "main");
    }
}
