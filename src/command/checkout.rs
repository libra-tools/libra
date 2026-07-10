//! Handles checkout-style flows to show the current branch, switch to existing branches, or create and switch to a new one using restore utilities.

use std::str::FromStr;

use clap::Parser;
use git_internal::{hash::ObjectHash, internal::object::commit::Commit};
use serde::Serialize;

use crate::{
    command::{
        branch, load_object, pull,
        restore::{self, RestoreArgs},
        switch,
    },
    info_println,
    internal::{
        branch::{Branch, BranchStoreError, is_ai_managed_branch},
        head::Head,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
        util::get_commit_base,
    },
};

const CHECKOUT_EXAMPLES: &str = "\
NOTE:
    libra checkout is a branch compatibility surface. New code paths
    should prefer:
      - `libra switch <branch>` / `libra switch -c <branch>` for branch
        navigation and creation
      - `libra restore <path>` to restore files from the index or HEAD

EXAMPLES:
    libra checkout                         Show the current branch
    libra checkout main                    Switch to a branch (prefer: libra switch main)
    libra checkout feature-x               Switch to another branch (prefer: libra switch feature-x)
    libra checkout -b feature-x            Create + switch to a new branch (prefer: libra switch -c feature-x)
    libra checkout --orphan fresh-start    Create an unborn orphan branch (prefer: libra switch --orphan fresh-start)
    libra checkout --detach main           Detach HEAD at a branch's commit instead of switching
    libra checkout -t origin/main          --track accepted; remote checkout tracks via DWIM
    libra checkout -- file.txt             Restore a path from the index (prefer: libra restore file.txt)
    libra checkout HEAD -- file.txt        Restore a path from HEAD into index + worktree
    libra --json checkout main             Structured compatibility output
    libra checkout --quiet main            Switch without informational stdout";

#[derive(Parser, Debug)]
#[command(after_help = CHECKOUT_EXAMPLES)]
pub struct CheckoutArgs {
    /// Target branch, commit, or tag to check out (prefer `libra switch` for branches)
    branch: Option<String>,

    /// Create and switch to a new branch with the same content as the current branch
    #[clap(short = 'b', group = "sub")]
    new_branch: Option<String>,

    /// Create or reset a branch and switch to it
    #[clap(short = 'B', group = "sub")]
    force_new_branch: Option<String>,

    /// Create an unborn orphan branch and switch to it, preserving the index/worktree
    #[clap(long = "orphan", group = "sub")]
    orphan_branch: Option<String>,

    /// Proceed even when the working tree/index differs from HEAD, discarding
    /// local modifications to tracked files. Untracked files are still preserved.
    #[clap(short = 'f', long = "force")]
    force: bool,

    /// Detach HEAD at the named commit even when it is a branch (rather than
    /// switching to the branch).
    #[clap(short = 'd', long = "detach")]
    detach: bool,

    /// Set up upstream tracking when checking out a remote-tracking branch.
    /// Libra always configures tracking for a remote-tracking checkout (DWIM),
    /// so `--track` is accepted for Git parity and requests behavior Libra
    /// already performs; it has no effect for a non-remote target. For
    /// explicit, standalone tracking setup use `libra switch --track`.
    #[clap(short = 't', long = "track")]
    track: bool,

    /// Check out a branch even if it is already checked out in another worktree.
    /// Accepted for Git parity and is a no-op: Libra worktrees share a single
    /// `HEAD`/refs store, so a branch is never locked to one worktree and there
    /// is no other-worktree restriction to override.
    #[clap(long = "ignore-other-worktrees")]
    ignore_other_worktrees: bool,

    /// Do not show a progress meter. Accepted for Git parity and is a no-op:
    /// Libra's checkout never renders a progress meter, so there is nothing to
    /// suppress.
    #[clap(long = "no-progress")]
    no_progress: bool,

    /// Do not check out paths in overlay mode (the default): paths missing from
    /// the source are still removed. Accepted for Git parity and is a no-op:
    /// Libra's checkout is never in overlay mode, so this already matches the
    /// default. (Git's opposite `--overlay` is not implemented.)
    #[clap(long = "no-overlay")]
    no_overlay: bool,

    /// Paths to restore after an explicit `--` separator
    #[clap(last = true, value_name = "pathspec")]
    pathspec: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CheckoutOutput {
    action: String,
    previous_branch: Option<String>,
    previous_commit: Option<String>,
    branch: Option<String>,
    commit: Option<String>,
    short_commit: Option<String>,
    switched: bool,
    created: bool,
    pulled: bool,
    already_on: bool,
    detached: bool,
    tracking: Option<CheckoutTrackingOutput>,
    restore: Option<restore::RestoreOutput>,
}

#[derive(Debug, Clone, Serialize)]
struct CheckoutTrackingOutput {
    remote: String,
    remote_branch: String,
}

#[derive(Debug, thiserror::Error)]
enum CheckoutError {
    #[error("checking out '{0}' branch is not allowed")]
    CheckingOutBranchBlocked(String),

    #[error("creating/switching to '{0}' branch is not allowed")]
    CreatingBranchBlocked(String),

    #[error("switching to '{0}' branch is not allowed")]
    SwitchingToBranchBlocked(String),

    #[error("branch '{0}' not found")]
    BranchNotFound(String),

    #[error("path specification '{0}' did not match any files known to libra")]
    PathSpecNotMatched(String),

    #[error("unstaged changes, can't switch branch")]
    DirtyUnstaged,

    #[error("uncommitted changes, can't switch branch")]
    DirtyUncommitted,

    #[error("untracked working tree file would be overwritten by checkout: {0}")]
    UntrackedOverwrite(String),

    #[error("checkout path mode cannot be combined with {0}")]
    InvalidPathMode(String),

    #[error("failed to {context}: {detail}")]
    BranchStoreRead { context: String, detail: String },

    #[error("failed to {context}: {detail}")]
    BranchStoreCorrupt { context: String, detail: String },

    #[error("'{0}' is not a valid object name for checkout")]
    InvalidObjectName(String),

    #[error("failed to create branch '{branch}': {detail}")]
    BranchCreate { branch: String, detail: String },

    #[error("checkout remote branch left HEAD without a commit")]
    RemoteHeadMissing,

    #[error("failed to {stage} during remote branch checkout: {}", source.message())]
    RemoteSyncFailed {
        stage: &'static str,
        #[source]
        source: Box<CliError>,
    },

    #[error(transparent)]
    DelegatedCli(#[from] CliError),
}

impl From<CheckoutError> for CliError {
    fn from(error: CheckoutError) -> Self {
        match error {
            CheckoutError::CheckingOutBranchBlocked(branch) => {
                CliError::fatal(format!("checking out '{}' branch is not allowed", branch))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
            }

            CheckoutError::CreatingBranchBlocked(branch) => CliError::fatal(format!(
                "creating/switching to '{}' branch is not allowed",
                branch
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget),

            CheckoutError::SwitchingToBranchBlocked(branch) => {
                CliError::fatal(format!("switching to '{}' branch is not allowed", branch))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
            }

            CheckoutError::BranchNotFound(branch) => {
                CliError::fatal(format!("branch '{}' not found", branch))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
            }

            CheckoutError::PathSpecNotMatched(spec) => CliError::fatal(format!(
                "path specification '{}' did not match any files known to libra",
                spec
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget),

            CheckoutError::DirtyUnstaged | CheckoutError::DirtyUncommitted => {
                CliError::failure("local changes would be overwritten by checkout")
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
            }

            CheckoutError::UntrackedOverwrite(path) => CliError::failure(format!(
                "local changes would be overwritten by checkout: {path}"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked),

            CheckoutError::InvalidPathMode(flag) => CliError::fatal(format!(
                "checkout path mode cannot be combined with {flag}"
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint(
                "use 'libra restore' for file restoration, or omit '--' for branch checkout",
            ),

            CheckoutError::BranchStoreRead { context, detail } => {
                CliError::fatal(format!("failed to {context}: {detail}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            }
            CheckoutError::BranchStoreCorrupt { context, detail } => {
                CliError::fatal(format!("failed to {context}: {detail}"))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
            }
            CheckoutError::InvalidObjectName(name) => CliError::fatal(format!(
                "pathspec '{name}' did not match any files known to libra"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("check the branch name, commit hash, or tag and try again."),
            CheckoutError::BranchCreate { branch, detail } => {
                CliError::fatal(format!("failed to create branch '{branch}': {detail}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            }
            CheckoutError::RemoteHeadMissing => {
                CliError::fatal("checkout remote branch left HEAD without a commit")
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
            }
            CheckoutError::RemoteSyncFailed { stage, source } => {
                let inner = *source;
                let stable_code = inner.stable_code();
                let message = format!(
                    "failed to {stage} during remote branch checkout: {}",
                    inner.message()
                );
                let wrapped = match inner.kind() {
                    crate::utils::error::CliErrorKind::Fatal => CliError::fatal(message),
                    _ => CliError::failure(message),
                };
                wrapped.with_stable_code(stable_code)
            }
            CheckoutError::DelegatedCli(err) => err,
        }
    }
}

pub async fn execute(args: CheckoutArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting.
///
/// # Side Effects
/// - Validates target branch names and blocks the internal `intent` branch.
/// - May create a branch when `-b` is supplied.
/// - Switches HEAD/current branch and restores the working tree to the target.
/// - Emits status messages through [`OutputConfig`].
///
/// # Errors
/// Returns [`CliError`] when the target branch is invalid or missing, local
/// changes would be overwritten, branch creation fails, or checkout/restore
/// writes fail.
pub async fn execute_safe(args: CheckoutArgs, output: &OutputConfig) -> CliResult<()> {
    let result = run_checkout(args, output).await.map_err(CliError::from)?;
    render_checkout_output(&result, output)
}

async fn run_checkout(
    args: CheckoutArgs,
    output: &OutputConfig,
) -> Result<CheckoutOutput, CheckoutError> {
    if !args.pathspec.is_empty() {
        return restore_checkout_paths(args).await;
    }

    if let Some(ref branch_name) = args.branch
        && is_ai_managed_branch(branch_name)
    {
        return Err(CheckoutError::CheckingOutBranchBlocked(branch_name.clone()));
    }
    if let Some(ref new_branch_name) = args.new_branch
        && is_ai_managed_branch(new_branch_name)
    {
        return Err(CheckoutError::CreatingBranchBlocked(
            new_branch_name.clone(),
        ));
    }
    if let Some(ref force_new_branch_name) = args.force_new_branch
        && is_ai_managed_branch(force_new_branch_name)
    {
        return Err(CheckoutError::CreatingBranchBlocked(
            force_new_branch_name.clone(),
        ));
    }
    if let Some(ref orphan_branch_name) = args.orphan_branch
        && is_ai_managed_branch(orphan_branch_name)
    {
        return Err(CheckoutError::CreatingBranchBlocked(
            orphan_branch_name.clone(),
        ));
    }

    let previous_branch = get_current_branch().await;
    let previous_commit = current_commit_string().await?;

    // Match Git behavior: checking out the current branch is a no-op and should
    // not be blocked by unrelated local changes. `--detach` is the exception:
    // `checkout --detach <current-branch>` still detaches HEAD at its commit.
    if let Some(ref target_branch) = args.branch
        && previous_branch.as_ref() == Some(target_branch)
        && !args.detach
        && args.new_branch.is_none()
        && args.force_new_branch.is_none()
        && args.orphan_branch.is_none()
    {
        return Ok(CheckoutOutput {
            action: "already-on".to_string(),
            previous_branch,
            previous_commit: previous_commit.clone(),
            branch: Some(target_branch.clone()),
            commit: previous_commit.clone(),
            short_commit: previous_commit.as_deref().map(short_oid),
            switched: false,
            created: false,
            pulled: false,
            already_on: true,
            detached: false,
            tracking: None,
            restore: None,
        });
    }

    if let Some(orphan_branch) = args.orphan_branch {
        if let Some(start_point) = args.branch {
            return Err(CheckoutError::DelegatedCli(
                CliError::command_usage("checkout --orphan does not accept a start-point")
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                    .with_hint(format!(
                        "run 'libra checkout --orphan {orphan_branch}' without '{start_point}'."
                    )),
            ));
        }
        let switch_output = switch::switch_to_orphan_branch(
            orphan_branch.clone(),
            previous_branch,
            previous_commit,
            output,
        )
        .await
        .map_err(map_switch_error)?;
        return Ok(CheckoutOutput {
            action: "create".to_string(),
            previous_branch: switch_output.previous_branch,
            previous_commit: switch_output.previous_commit,
            branch: Some(orphan_branch),
            commit: None,
            short_commit: None,
            switched: true,
            created: true,
            pulled: false,
            already_on: false,
            detached: false,
            tracking: None,
            restore: None,
        });
    }

    if let Some(new_branch) = args.new_branch {
        let start_point = args.branch;
        let target_commit = resolve_checkout_create_startpoint(start_point.as_deref()).await?;
        let clean_status = if args.force {
            switch::ensure_no_untracked_overwrite(target_commit)
        } else {
            switch::ensure_clean_status_for_commit(target_commit, output).await
        };
        map_switch_preflight(clean_status)?;

        let child_output = silent_child_output(output);
        let commit = create_and_switch_new_branch(&new_branch, start_point, &child_output).await?;
        let commit = commit.to_string();
        return Ok(CheckoutOutput {
            action: "create".to_string(),
            previous_branch,
            previous_commit,
            branch: Some(new_branch),
            short_commit: Some(short_oid(&commit)),
            commit: Some(commit),
            switched: true,
            created: true,
            pulled: false,
            already_on: false,
            detached: false,
            tracking: None,
            restore: None,
        });
    }

    if let Some(new_branch) = args.force_new_branch {
        let start_point = args.branch;
        let target_commit = resolve_checkout_create_startpoint(start_point.as_deref()).await?;
        let clean_status = if args.force {
            switch::ensure_no_untracked_overwrite(target_commit)
        } else {
            switch::ensure_clean_status_for_commit(target_commit, output).await
        };
        map_switch_preflight(clean_status)?;

        let previous = get_current_branch().await;
        if let Some(prev) = previous.as_deref()
            && prev == new_branch
        {
            return Err(CheckoutError::CreatingBranchBlocked(new_branch));
        }
        reset_and_switch_branch(&new_branch, target_commit, &silent_child_output(output)).await?;
        let commit = target_commit.to_string();
        return Ok(CheckoutOutput {
            action: "create".to_string(),
            previous_branch,
            previous_commit,
            branch: Some(new_branch),
            short_commit: Some(short_oid(&commit)),
            commit: Some(commit),
            switched: true,
            created: true,
            pulled: false,
            already_on: false,
            detached: false,
            tracking: None,
            restore: None,
        });
    }

    let target_commit = if let Some(ref branch_name) = args.branch {
        if let Some(branch) = Branch::find_branch_result(branch_name, None)
            .await
            .map_err(|error| map_checkout_branch_store_error("resolve checkout target", error))?
        {
            Some(branch.commit)
        } else {
            get_commit_base(branch_name)
                .await
                .ok()
                .or_else(|| ObjectHash::from_str(branch_name).ok())
        }
    } else {
        None
    };

    let clean_status = if args.force {
        // `-f` discards local modifications to tracked files — `restore_to_commit`
        // overwrites them below. We deliberately do NOT skip the whole gate:
        // `ensure_clean_status*` returns only the first problem, so we still run
        // the untracked-overwrite check independently to avoid silently clobbering
        // an untracked file the target would write over.
        match target_commit {
            Some(target_commit) => switch::ensure_no_untracked_overwrite(target_commit),
            None => Ok(()),
        }
    } else {
        match target_commit {
            Some(target_commit) => {
                switch::ensure_clean_status_for_commit(target_commit, output).await
            }
            None => switch::ensure_clean_status(output).await,
        }
    };

    match clean_status {
        Ok(()) => {}
        Err(switch::SwitchError::DirtyUnstaged) => {
            return Err(CheckoutError::DirtyUnstaged);
        }
        Err(switch::SwitchError::DirtyUncommitted) => {
            return Err(CheckoutError::DirtyUncommitted);
        }
        Err(switch::SwitchError::UntrackedOverwrite(path)) => {
            return Err(CheckoutError::UntrackedOverwrite(path));
        }
        Err(err) => return Err(CheckoutError::DelegatedCli(CliError::from(err))),
    }

    match args.branch {
        Some(target_branch) => {
            let is_branch = Branch::find_branch_result(&target_branch, None)
                .await
                .map_err(|error| map_checkout_branch_store_error("resolve checkout target", error))?
                .is_some();
            match target_commit {
                // `--detach` forces the detach path even for a branch name.
                Some(commit_id) if !is_branch || args.detach => {
                    checkout_detached(
                        target_branch,
                        commit_id,
                        previous_branch,
                        previous_commit,
                        output,
                    )
                    .await
                }
                None => Err(CheckoutError::InvalidObjectName(target_branch)),
                _ => {
                    check_and_switch_branch(
                        &target_branch,
                        previous_branch,
                        previous_commit,
                        output,
                        args.ignore_other_worktrees,
                    )
                    .await
                }
            }
        }
        None => show_current_branch(previous_branch, previous_commit).await,
    }
}

async fn restore_checkout_paths(args: CheckoutArgs) -> Result<CheckoutOutput, CheckoutError> {
    if args.new_branch.is_some() {
        return Err(CheckoutError::InvalidPathMode("-b".to_string()));
    }
    if args.force_new_branch.is_some() {
        return Err(CheckoutError::InvalidPathMode("-B".to_string()));
    }
    if args.orphan_branch.is_some() {
        return Err(CheckoutError::InvalidPathMode("--orphan".to_string()));
    }

    let previous_branch = get_current_branch().await;
    let previous_commit = current_commit_string().await?;
    let source = args.branch;
    let restore_args = RestoreArgs {
        overlay: false,
        no_overlay: false,
        ours: false,
        theirs: false,
        ignore_unmerged: false,
        merge: false,
        conflict: None,
        worktree: true,
        staged: source.is_some(),
        source,
        pathspec: args.pathspec,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_progress: false,
    };
    let restore = restore::execute_to_output(restore_args)
        .await
        .map_err(CheckoutError::DelegatedCli)?;
    let was_detached = previous_branch.is_none();

    Ok(CheckoutOutput {
        action: "restore-paths".to_string(),
        previous_branch: previous_branch.clone(),
        previous_commit: previous_commit.clone(),
        branch: previous_branch,
        commit: previous_commit.clone(),
        short_commit: previous_commit.as_deref().map(short_oid),
        switched: false,
        created: false,
        pulled: false,
        already_on: false,
        detached: was_detached,
        tracking: None,
        restore: Some(restore),
    })
}

fn map_checkout_branch_store_error(context: &str, error: BranchStoreError) -> CheckoutError {
    match error {
        BranchStoreError::Query(detail) => CheckoutError::BranchStoreRead {
            context: context.to_string(),
            detail,
        },
        other => CheckoutError::BranchStoreCorrupt {
            context: context.to_string(),
            detail: other.to_string(),
        },
    }
}

fn map_switch_preflight(result: Result<(), switch::SwitchError>) -> Result<(), CheckoutError> {
    result.map_err(map_switch_error)
}

fn map_switch_error(err: switch::SwitchError) -> CheckoutError {
    match err {
        switch::SwitchError::DirtyUnstaged => CheckoutError::DirtyUnstaged,
        switch::SwitchError::DirtyUncommitted => CheckoutError::DirtyUncommitted,
        switch::SwitchError::UntrackedOverwrite(path) => CheckoutError::UntrackedOverwrite(path),
        err => CheckoutError::DelegatedCli(CliError::from(err)),
    }
}

pub async fn get_current_branch() -> Option<String> {
    match Head::current().await {
        Head::Detached(_) => None,
        Head::Branch(name) => Some(name),
    }
}

async fn current_commit_string() -> Result<Option<String>, CheckoutError> {
    Head::current_commit_result()
        .await
        .map(|commit| commit.map(|hash| hash.to_string()))
        .map_err(|error| CheckoutError::BranchStoreCorrupt {
            context: "resolve HEAD commit".to_string(),
            detail: error.to_string(),
        })
}

pub async fn switch_branch(branch_name: &str) -> CliResult<()> {
    switch_branch_with_output(branch_name, &OutputConfig::default(), false)
        .await
        .map(|_| ())
        .map_err(CliError::from)
}

async fn switch_branch_with_output(
    branch_name: &str,
    output: &OutputConfig,
    ignore_other_worktrees: bool,
) -> Result<ObjectHash, CheckoutError> {
    if is_ai_managed_branch(branch_name) {
        return Err(CheckoutError::SwitchingToBranchBlocked(
            branch_name.to_string(),
        ));
    }
    // lore.md 2.1: refuse a branch already checked out in another worktree
    // (branches are shared) unless --ignore-other-worktrees. git parity.
    if !ignore_other_worktrees
        && let Some(other) = Head::branch_checked_out_elsewhere(branch_name).await
    {
        return Err(CheckoutError::DelegatedCli(
            crate::utils::error::CliError::fatal(format!(
                "branch '{branch_name}' is already checked out at worktree '{other}'"
            ))
            .with_stable_code(crate::utils::error::StableErrorCode::ConflictOperationBlocked)
            .with_hint("check out a different branch, use --detach, or --ignore-other-worktrees"),
        ));
    }
    let target_branch = Branch::find_branch_result(branch_name, None)
        .await
        .map_err(|error| map_checkout_branch_store_error("resolve branch", error))?
        .ok_or_else(|| CheckoutError::BranchNotFound(branch_name.to_string()))?;
    let target_commit = target_branch.commit;
    restore_to_commit(target_branch.commit, output)
        .await
        .map_err(CheckoutError::DelegatedCli)?;
    let head = Head::Branch(branch_name.to_string());
    Head::update(head, None).await;
    Ok(target_commit)
}

async fn create_and_switch_new_branch(
    new_branch: &str,
    start_point: Option<String>,
    output: &OutputConfig,
) -> Result<ObjectHash, CheckoutError> {
    branch::create_branch_safe(new_branch.to_string(), start_point)
        .await
        .map_err(CheckoutError::DelegatedCli)?;
    // A freshly created branch cannot be checked out elsewhere.
    switch_branch_with_output(new_branch, output, true).await
}

async fn resolve_checkout_create_startpoint(
    start_point: Option<&str>,
) -> Result<ObjectHash, CheckoutError> {
    let commit_id = match start_point {
        Some(spec) => get_commit_base(spec)
            .await
            .ok()
            .or_else(|| ObjectHash::from_str(spec).ok())
            .ok_or_else(|| CheckoutError::InvalidObjectName(spec.to_string()))?,
        None => match Head::current_commit_result().await.map_err(|error| {
            CheckoutError::BranchStoreCorrupt {
                context: "resolve HEAD commit".to_string(),
                detail: error.to_string(),
            }
        })? {
            Some(commit_id) => commit_id,
            None => {
                let current = match Head::current().await {
                    Head::Branch(name) => name,
                    Head::Detached(commit_hash) => commit_hash.to_string(),
                };
                return Err(CheckoutError::InvalidObjectName(current));
            }
        },
    };
    let display = start_point
        .map(str::to_string)
        .unwrap_or_else(|| commit_id.to_string());
    load_object::<Commit>(&commit_id).map_err(|_| CheckoutError::InvalidObjectName(display))?;
    Ok(commit_id)
}

async fn reset_and_switch_branch(
    branch_name: &str,
    target_commit: ObjectHash,
    output: &OutputConfig,
) -> Result<ObjectHash, CheckoutError> {
    if !branch::is_valid_git_branch_name(branch_name) {
        return Err(CheckoutError::DelegatedCli(
            CliError::fatal(format!("'{branch_name}' is not a valid branch name"))
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        ));
    }
    if is_ai_managed_branch(branch_name) {
        return Err(CheckoutError::CreatingBranchBlocked(
            branch_name.to_string(),
        ));
    }
    Branch::update_branch(branch_name, &target_commit.to_string(), None)
        .await
        .map_err(|error| CheckoutError::BranchCreate {
            branch: branch_name.to_string(),
            detail: error.to_string(),
        })?;
    switch_branch_with_output(branch_name, output, true).await
}

async fn get_remote(branch_name: &str, output: &OutputConfig) -> Result<ObjectHash, CheckoutError> {
    let remote_branch_name: String = format!("origin/{branch_name}");
    let child_output = silent_child_output(output);

    create_and_switch_new_branch(branch_name, None, &child_output)
        .await
        .map_err(|err| wrap_remote_proxy_error("create local tracking branch", err))?;
    // Set branch upstream
    branch::set_upstream_safe_with_output(branch_name, &remote_branch_name, &child_output)
        .await
        .map_err(|err| CheckoutError::RemoteSyncFailed {
            stage: "set upstream",
            source: Box::new(err),
        })?;
    // Synchronous branches
    // Use the pull command to update the local branch with the latest changes from the remote branch
    pull::execute_safe(pull::PullArgs::make(None, None), &child_output)
        .await
        .map_err(|err| CheckoutError::RemoteSyncFailed {
            stage: "pull from remote",
            source: Box::new(err),
        })?;
    Head::current_commit_result()
        .await
        .map_err(|error| map_checkout_branch_store_error("resolve checkout result", error))?
        .ok_or(CheckoutError::RemoteHeadMissing)
}

/// Converts a [`CheckoutError`] surfaced by the local-creation step of remote tracking
/// into a [`CheckoutError::RemoteSyncFailed`] envelope so downstream callers see a single
/// proxy-error variant regardless of which sub-step failed.
fn wrap_remote_proxy_error(stage: &'static str, err: CheckoutError) -> CheckoutError {
    match err {
        already @ CheckoutError::RemoteSyncFailed { .. } => already,
        other => CheckoutError::RemoteSyncFailed {
            stage,
            source: Box::new(CliError::from(other)),
        },
    }
}

/// Returns `Ok(Some(true))` if remote branch found, `Ok(Some(false))` if local branch found,
/// `Ok(None)` if already on the branch.
pub async fn check_branch(branch_name: &str) -> CliResult<Option<bool>> {
    check_branch_with_output(branch_name, &OutputConfig::default())
        .await
        .map_err(CliError::from)
}

async fn check_branch_with_output(
    branch_name: &str,
    output: &OutputConfig,
) -> Result<Option<bool>, CheckoutError> {
    if get_current_branch().await == Some(branch_name.to_string()) {
        info_println!(output, "Already on {branch_name}");
        return Ok(None);
    }

    let target_branch: Option<Branch> = Branch::find_branch_result(branch_name, None)
        .await
        .map_err(|error| map_checkout_branch_store_error("resolve branch", error))?;
    if target_branch.is_none() {
        let remote_branch_name: String = format!("origin/{branch_name}");
        if !Branch::search_branch_result(&remote_branch_name)
            .await
            .map_err(|error| {
                map_checkout_branch_store_error("search remote tracking branches", error)
            })?
            .is_empty()
        {
            info_println!(
                output,
                "branch '{branch_name}' set up to track '{remote_branch_name}'."
            );
            Ok(Some(true))
        } else {
            Err(CheckoutError::PathSpecNotMatched(branch_name.to_string()))
        }
    } else {
        info_println!(output, "Switched to branch '{branch_name}'");
        Ok(Some(false))
    }
}

async fn check_and_switch_branch(
    branch_name: &str,
    previous_branch: Option<String>,
    previous_commit: Option<String>,
    output: &OutputConfig,
    ignore_other: bool,
) -> Result<CheckoutOutput, CheckoutError> {
    let child_output = silent_child_output(output);
    match check_branch_with_output(branch_name, &child_output).await? {
        Some(true) => {
            let commit = get_remote(branch_name, output).await?.to_string();
            Ok(CheckoutOutput {
                action: "track".to_string(),
                previous_branch,
                previous_commit,
                branch: Some(branch_name.to_string()),
                commit: Some(commit.clone()),
                short_commit: Some(short_oid(&commit)),
                switched: true,
                created: true,
                pulled: true,
                already_on: false,
                detached: false,
                tracking: Some(CheckoutTrackingOutput {
                    remote: "origin".to_string(),
                    remote_branch: format!("origin/{branch_name}"),
                }),
                restore: None,
            })
        }
        Some(false) => {
            let commit = switch_branch_with_output(branch_name, &child_output, ignore_other)
                .await?
                .to_string();
            Ok(CheckoutOutput {
                action: "switch".to_string(),
                previous_branch,
                previous_commit,
                branch: Some(branch_name.to_string()),
                commit: Some(commit.clone()),
                short_commit: Some(short_oid(&commit)),
                switched: true,
                created: false,
                pulled: false,
                already_on: false,
                detached: false,
                tracking: None,
                restore: None,
            })
        }
        None => Ok(CheckoutOutput {
            action: "already-on".to_string(),
            previous_branch: previous_branch.clone(),
            previous_commit: previous_commit.clone(),
            branch: Some(branch_name.to_string()),
            commit: previous_commit.clone(),
            short_commit: previous_commit.as_deref().map(short_oid),
            switched: false,
            created: false,
            pulled: false,
            already_on: true,
            detached: false,
            tracking: None,
            restore: None,
        }),
    }
}

async fn restore_to_commit(commit_id: ObjectHash, output: &OutputConfig) -> CliResult<()> {
    // Case-collision preflight (lore.md 1.14) — checkout has its own copy of
    // this restore path, so it gets its own guard (the review's must-fix:
    // guarding only switch would leave `libra checkout <branch>` unprotected).
    crate::command::switch::guard_target_tree_case(&commit_id).await?;
    let restore_args = RestoreArgs {
        overlay: false,
        no_overlay: false,
        ours: false,
        theirs: false,
        ignore_unmerged: false,
        merge: false,
        conflict: None,
        worktree: true,
        staged: true,
        source: Some(commit_id.to_string()),
        pathspec: vec![util::working_dir_string()],
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_progress: false,
    };
    restore::execute_safe(restore_args, &output.child_output_config()).await
}

async fn checkout_detached(
    _target: String,
    commit_id: ObjectHash,
    previous_branch: Option<String>,
    previous_commit: Option<String>,
    output: &OutputConfig,
) -> Result<CheckoutOutput, CheckoutError> {
    switch::ensure_clean_status_for_commit(commit_id, output)
        .await
        .map_err(|err| match err {
            switch::SwitchError::DirtyUnstaged => CheckoutError::DirtyUnstaged,
            switch::SwitchError::DirtyUncommitted => CheckoutError::DirtyUncommitted,
            switch::SwitchError::UntrackedOverwrite(path) => {
                CheckoutError::UntrackedOverwrite(path)
            }
            other => CheckoutError::DelegatedCli(CliError::from(other)),
        })?;

    // Case-collision preflight BEFORE the HEAD update — refusing after it
    // would strand a detached HEAD with an unrestored tree.
    crate::command::switch::guard_target_tree_case(&commit_id)
        .await
        .map_err(CheckoutError::DelegatedCli)?;
    let head = Head::Detached(commit_id);
    Head::update(head, None).await;
    restore_to_commit(commit_id, output)
        .await
        .map_err(CheckoutError::DelegatedCli)?;

    Ok(CheckoutOutput {
        action: "detach".to_string(),
        previous_branch,
        previous_commit,
        branch: None,
        commit: Some(commit_id.to_string()),
        short_commit: Some(short_oid(&commit_id.to_string())),
        switched: true,
        created: false,
        pulled: false,
        already_on: false,
        detached: true,
        tracking: None,
        restore: None,
    })
}

async fn show_current_branch(
    current_branch: Option<String>,
    current_commit: Option<String>,
) -> Result<CheckoutOutput, CheckoutError> {
    match Head::current().await {
        Head::Detached(commit_hash) => {
            let commit = commit_hash.to_string();
            Ok(CheckoutOutput {
                action: "show-current".to_string(),
                previous_branch: current_branch,
                previous_commit: current_commit,
                branch: None,
                commit: Some(commit.clone()),
                short_commit: Some(short_oid(&commit)),
                switched: false,
                created: false,
                pulled: false,
                already_on: false,
                detached: true,
                tracking: None,
                restore: None,
            })
        }
        Head::Branch(current_branch) => Ok(CheckoutOutput {
            action: "show-current".to_string(),
            previous_branch: Some(current_branch.clone()),
            previous_commit: current_commit.clone(),
            branch: Some(current_branch),
            commit: current_commit.clone(),
            short_commit: current_commit.as_deref().map(short_oid),
            switched: false,
            created: false,
            pulled: false,
            already_on: false,
            detached: false,
            tracking: None,
            restore: None,
        }),
    }
}

fn render_checkout_output(result: &CheckoutOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("checkout", result, output);
    }
    if output.quiet {
        return Ok(());
    }

    match result.action.as_str() {
        "show-current" if result.detached => {
            if let Some(short_commit) = &result.short_commit {
                println!("HEAD detached at {short_commit}");
            }
        }
        "show-current" => {
            if let Some(branch) = &result.branch {
                println!("Current branch is {branch}.");
            }
        }
        "already-on" => {
            if let Some(branch) = &result.branch {
                println!("Already on {branch}");
            }
        }
        "create" => {
            if let Some(branch) = &result.branch {
                println!("Switched to a new branch '{branch}'");
            }
        }
        "switch" => {
            if let Some(branch) = &result.branch {
                println!("Switched to branch '{branch}'");
            }
        }
        "track" => {
            if let (Some(branch), Some(tracking)) = (&result.branch, &result.tracking) {
                println!(
                    "branch '{branch}' set up to track '{}'.",
                    tracking.remote_branch
                );
                println!("Switched to a new branch '{branch}'");
            }
        }
        "restore-paths" => {
            if let Some(restore) = &result.restore {
                let total = restore.restored_files.len() + restore.deleted_files.len();
                if total > 0 {
                    let source_desc = restore.source.as_deref().unwrap_or("the index");
                    println!("Updated {total} path(s) from {source_desc}");
                }
            }
        }
        _ => {}
    }

    Ok(())
}

fn short_oid(oid: &str) -> String {
    oid.chars().take(8).collect()
}

fn silent_child_output(output: &OutputConfig) -> OutputConfig {
    let mut child = output.child_output_config();
    child.quiet = true;
    child
}

/// Unit tests for the checkout module
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_error_display_pins_owned_variants() {
        assert_eq!(
            CheckoutError::CheckingOutBranchBlocked("HEAD".to_string()).to_string(),
            "checking out 'HEAD' branch is not allowed",
        );
        assert_eq!(
            CheckoutError::CreatingBranchBlocked("HEAD".to_string()).to_string(),
            "creating/switching to 'HEAD' branch is not allowed",
        );
        assert_eq!(
            CheckoutError::SwitchingToBranchBlocked("intent".to_string()).to_string(),
            "switching to 'intent' branch is not allowed",
        );
        assert_eq!(
            CheckoutError::BranchNotFound("feature".to_string()).to_string(),
            "branch 'feature' not found",
        );
        assert_eq!(
            CheckoutError::PathSpecNotMatched("nonexistent".to_string()).to_string(),
            "path specification 'nonexistent' did not match any files known to libra",
        );
        assert_eq!(
            CheckoutError::DirtyUnstaged.to_string(),
            "unstaged changes, can't switch branch",
        );
        assert_eq!(
            CheckoutError::DirtyUncommitted.to_string(),
            "uncommitted changes, can't switch branch",
        );
        assert_eq!(
            CheckoutError::UntrackedOverwrite("src/new.rs".to_string()).to_string(),
            "untracked working tree file would be overwritten by checkout: src/new.rs",
        );
        assert_eq!(
            CheckoutError::BranchStoreRead {
                context: "load branch 'main'".to_string(),
                detail: "database is locked".to_string(),
            }
            .to_string(),
            "failed to load branch 'main': database is locked",
        );
        assert_eq!(
            CheckoutError::BranchStoreCorrupt {
                context: "resolve branch 'feature'".to_string(),
                detail: "ref points to non-commit object".to_string(),
            }
            .to_string(),
            "failed to resolve branch 'feature': ref points to non-commit object",
        );
        assert_eq!(
            CheckoutError::RemoteHeadMissing.to_string(),
            "checkout remote branch left HEAD without a commit",
        );
        let proxy_err = CliError::failure("remote not configured")
            .with_stable_code(StableErrorCode::NetworkUnavailable);
        assert_eq!(
            CheckoutError::RemoteSyncFailed {
                stage: "pull from remote",
                source: Box::new(proxy_err),
            }
            .to_string(),
            "failed to pull from remote during remote branch checkout: remote not configured",
        );
    }

    #[test]
    fn checkout_error_maps_owned_variants_to_stable_codes() {
        let cases: Vec<(CheckoutError, StableErrorCode)> = vec![
            (
                CheckoutError::CheckingOutBranchBlocked("intent".to_string()),
                StableErrorCode::CliInvalidTarget,
            ),
            (
                CheckoutError::CreatingBranchBlocked("intent".to_string()),
                StableErrorCode::CliInvalidTarget,
            ),
            (
                CheckoutError::SwitchingToBranchBlocked("intent".to_string()),
                StableErrorCode::CliInvalidTarget,
            ),
            (
                CheckoutError::BranchNotFound("feature".to_string()),
                StableErrorCode::CliInvalidTarget,
            ),
            (
                CheckoutError::PathSpecNotMatched("nope".to_string()),
                StableErrorCode::CliInvalidTarget,
            ),
            (
                CheckoutError::DirtyUnstaged,
                StableErrorCode::RepoStateInvalid,
            ),
            (
                CheckoutError::DirtyUncommitted,
                StableErrorCode::RepoStateInvalid,
            ),
            (
                CheckoutError::UntrackedOverwrite("a.txt".to_string()),
                StableErrorCode::ConflictOperationBlocked,
            ),
            (
                CheckoutError::BranchStoreRead {
                    context: "resolve branch".to_string(),
                    detail: "database is locked".to_string(),
                },
                StableErrorCode::IoReadFailed,
            ),
            (
                CheckoutError::BranchStoreCorrupt {
                    context: "resolve branch".to_string(),
                    detail: "ref points to non-commit object".to_string(),
                },
                StableErrorCode::RepoCorrupt,
            ),
            (
                CheckoutError::RemoteHeadMissing,
                StableErrorCode::RepoStateInvalid,
            ),
        ];

        for (err, expected) in cases {
            let cli: CliError = err.into();
            assert_eq!(cli.stable_code(), expected);
        }
    }

    #[test]
    fn checkout_remote_sync_failed_preserves_inner_stable_code() {
        let inner = CliError::fatal("upstream missing")
            .with_stable_code(StableErrorCode::NetworkUnavailable);
        let wrapped = CheckoutError::RemoteSyncFailed {
            stage: "set upstream",
            source: Box::new(inner),
        };
        let cli: CliError = wrapped.into();
        assert_eq!(cli.stable_code(), StableErrorCode::NetworkUnavailable);
        assert!(
            cli.message()
                .contains("failed to set upstream during remote branch checkout"),
            "got: {}",
            cli.message()
        );
        assert!(
            cli.message().contains("upstream missing"),
            "got: {}",
            cli.message()
        );
    }
}
