//! `libra worktree` command implementation.
//!
//! Boundary: manages linked worktree metadata and filesystem layout while preserving
//! main-worktree safety invariants. Command tests cover add/list/remove, duplicate
//! paths, and main-worktree protection.

use std::{
    env, io,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::{
    internal::{sequencer::WorktreeControl, workspace::RepoIdentity},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

#[path = "worktree/doctor.rs"]
mod doctor;
#[path = "worktree/lock.rs"]
mod lock;
#[path = "worktree/operations.rs"]
mod operations;
#[path = "worktree/registry.rs"]
mod registry;

pub(crate) use doctor::*;
pub(crate) use lock::acquire_registry_lock_async;
pub(crate) use operations::*;
pub(crate) use registry::{
    DETACHED_MARKER, WorktreeEntry, WorktreeEntryState, WorktreeState, local_gitdir_for_scope,
    registry_knows_linked_worktree, registry_knows_linked_worktree_in_storage,
};
#[cfg(all(test, unix))]
use registry::{REGISTRY_SCHEMA_VERSION, WorktreeStateV1};
use registry::{
    RegistryShape, canonicalize, ensure_main_entry, find_entry, load_state, load_state_for_repair,
    load_state_readonly, normalize_v2_ids, save_state, state_path, write_state,
};

/// `--help` examples shown in `libra worktree --help` output.
pub const WORKTREE_EXAMPLES: &str = "\
 EXAMPLES:
     libra worktree add ../feature-x                Create a linked worktree (detached at
                                                    the source commit)
     libra worktree add ../fix-1 hotfix             Check the existing branch `hotfix` out
     libra worktree add --detach ../probe v1.2.0    Detached worktree at a commit-ish
     libra worktree add -b topic ../topic main      Create branch `topic` from `main` and
                                                    check it out
     libra worktree list                            List every registered worktree
     libra worktree list --porcelain                Machine-readable worktree list
     libra worktree lock ../feature-x --reason wip  Lock a worktree to prevent prune/remove
     libra worktree unlock ../feature-x             Release the lock
     libra worktree move ../old ../new              Rename a worktree
     libra worktree prune                           Drop entries whose paths vanished
     libra worktree remove ../feature-x             Unregister, keep the directory on disk
     libra worktree remove ../feature-x --delete-dir
                                                    Unregister and delete the directory
                                                    (refused on a dirty worktree)
     libra worktree repair --confirm                Fix stale or duplicate registry rows
     libra worktree repair --confirm ../feature-x   Restore that worktree's gitdir identity
                                                    from the registry (registry v2)
     libra worktree repair --migrate-layout --confirm
                                                    Migrate every legacy shared-.libra
                                                    symlink worktree to the isolated layout
     libra worktree repair --migrate-layout --dry-run
                                                    Report what would be migrated (read-only)
     libra worktree doctor                          Read-only diagnostics of per-worktree
                                                    scopes and Agent workspaces (paginated)
     libra --json worktree doctor --limit 20        One machine-readable page
     libra worktree doctor ws-3f0c                  Diagnose a single workspace scope";

/// Manage multiple working trees attached to this repository.
//
// Note: the user-facing summary for `libra worktree --help` is set via
// `#[command(about = "...", long_about = ...)]` on the Cli enum binding
// in src/cli.rs. We use `long_about` here so clap renders the same one-
// liner in both the top-level command list and `worktree --help`'s
// header, instead of leaking the previous "CLI arguments for the
// `worktree` subcommand. This type is wired into..." rustdoc body.
#[derive(Parser, Debug)]
#[command(long_about = "Manage multiple working trees attached to this repository.")]
pub struct WorktreeArgs {
    #[command(subcommand)]
    pub command: WorktreeSubcommand,
}
/// All supported `worktree` subcommands.
///
/// These roughly mirror `git worktree` operations while keeping Libra-specific
/// semantics (for example, `remove` does not delete directories on disk).
#[derive(Debug, Clone, Subcommand)]
pub enum WorktreeSubcommand {
    /// Create a new linked worktree at the given path.
    Add {
        /// Filesystem path at which to create the new worktree.
        path: String,
        /// Existing branch to check out in the new worktree, or a
        /// commit-ish for a detached HEAD. Omitted: detached at the source
        /// worktree's current commit (intentionally different from Git's
        /// basename-branch default). A nonexistent branch fails closed —
        /// Git's remote-branch DWIM is deferred.
        target: Option<String>,
        /// Detach HEAD in the new worktree even when <BRANCH-OR-COMMIT>
        /// names a branch.
        #[arg(short, long)]
        detach: bool,
        /// Create NEW_BRANCH (from <BRANCH-OR-COMMIT> or the source HEAD)
        /// and check it out in the new worktree. Refused if the branch
        /// already exists (no -B/--force).
        #[arg(short = 'b', long)]
        new_branch: Option<String>,
    },
    /// List all known worktrees and their state.
    List {
        /// Emit a stable, machine-readable porcelain format (one attribute per
        /// line, blank line between worktrees).
        #[arg(long)]
        porcelain: bool,
        /// JSON data schema selector (§C.8). `2` — the shipped shape, with
        /// `worktree_id`/`layout`/`epoch` — is the default and currently the
        /// only version: the pre-worktree-identity v1 shape gained those
        /// fields IN PLACE across the W1/W3 releases (each with its compat
        /// fixtures updated), so there is no frozen v1 left to serve and
        /// requesting it is refused rather than answered with a lie.
        #[arg(long, default_value_t = 2)]
        schema_version: u32,
    },
    /// Mark a worktree as locked to prevent it from being pruned or removed.
    Lock {
        /// Filesystem path of the worktree to lock.
        path: String,
        /// Optional free-form explanation for why this worktree is locked (shown in `worktree list`)
        #[arg(long)]
        reason: Option<String>,
    },
    /// Remove the lock from a previously locked worktree.
    Unlock {
        /// Filesystem path of the worktree to unlock.
        path: String,
    },
    /// Move or rename an existing worktree.
    Move {
        /// Current filesystem path of the worktree.
        src: String,
        /// New filesystem path for the worktree.
        dest: String,
    },
    /// Prune worktrees that are no longer valid or reachable.
    Prune,
    /// Unregister a worktree. By default the directory on disk is preserved;
    /// pass `--delete-dir` for Git-style behavior that also removes the
    /// directory after a dirty-state check.
    Remove {
        /// Filesystem path of the worktree to unregister.
        path: String,
        /// Also delete the worktree directory on disk after unregistering it.
        /// Refuses on a dirty worktree (uncommitted changes).
        #[arg(long)]
        delete_dir: bool,
    },
    /// Unmount a FUSE task worktree mountpoint.
    #[cfg(unix)]
    #[clap(alias = "unmount", about = "Unmount a FUSE worktree mountpoint")]
    Umount {
        /// Filesystem path of the FUSE mountpoint or its task worktree root.
        path: String,
        /// Remove the Libra task worktree root after unmounting its workspace mountpoint.
        #[arg(long)]
        cleanup: bool,
    },
    /// Diagnose Agent workspace scopes.
    ///
    /// Without an id this is a keyset-paginated view over every workspace
    /// record that still has something to say — including records left behind
    /// by a previous repository identity, which the identity-scoped listings
    /// hide. With an id it is the single-scope view of that one workspace.
    /// The default invocation is strictly read-only; the only repair action
    /// currently available is explicit legacy capture adoption, which also
    /// requires `--confirm` and writes an audit event.
    Doctor {
        /// Diagnose exactly one workspace (ids come from `worktree doctor`
        /// or `libra agent workspace list`). Cannot be combined with
        /// `--limit`/`--cursor`: a single scope is not a page.
        workspace_id: Option<String>,
        /// Maximum diagnostics per page (default 50, capped at 500).
        #[arg(long)]
        limit: Option<u64>,
        /// Keyset cursor: the `next_cursor` of the previous page, verbatim.
        #[arg(long)]
        cursor: Option<String>,
        /// Explicitly attribute one legacy unscoped capture session to this
        /// workspace. Requires a workspace id and --confirm; the default
        /// doctor command remains strictly read-only.
        #[arg(
             long,
             value_name = "SESSION_ID",
             requires = "workspace_id",
             conflicts_with_all = ["limit", "cursor"]
         )]
        adopt_capture_session: Option<String>,
        /// Copy the repository's common `info/exclude`/`info/attributes`
        /// (main's `.libra/info/*`) into ONE linked worktree's local gitdir
        /// (W0 §C.4.1.1: since info files became worktree-local they apply
        /// only to main; adoption is explicit and per-worktree, never
        /// automatic). Requires --confirm.
        #[arg(
             long,
             value_name = "WORKTREE_PATH",
             conflicts_with_all = [
                 "workspace_id", "limit", "cursor", "adopt_capture_session",
                 "adopt_approved_project", "clear_approved_project"
             ]
         )]
        adopt_info_to: Option<String>,
        /// Delete the repository's common `.libra/info/exclude` and
        /// `info/attributes` (explicit clear for rules that should no longer
        /// apply anywhere). Requires --confirm.
        #[arg(
             long,
             conflicts_with_all = [
                 "workspace_id", "limit", "cursor", "adopt_capture_session", "adopt_info_to",
                 "adopt_approved_project", "clear_approved_project"
             ]
         )]
        clear_common_info: bool,
        /// Re-home Always approvals whose opaque `project_id` is not the
        /// current `libra.repoid` onto the canonical repository identity
        /// (plan-20260715 W4-07). Migrations never do this; requires
        /// --confirm.
        #[arg(
             long,
             value_name = "LEGACY_PROJECT_ID",
             conflicts_with_all = [
                 "workspace_id", "limit", "cursor", "adopt_capture_session", "adopt_info_to",
                 "clear_common_info", "clear_approved_project"
             ]
         )]
        adopt_approved_project: Option<String>,
        /// Delete Always approvals under a legacy (non-canonical) `project_id`
        /// without adopting them. Requires --confirm.
        #[arg(
             long,
             value_name = "LEGACY_PROJECT_ID",
             conflicts_with_all = [
                 "workspace_id", "limit", "cursor", "adopt_capture_session", "adopt_info_to",
                 "clear_common_info", "adopt_approved_project"
             ]
         )]
        clear_approved_project: Option<String>,
        /// Confirm a mutating doctor action (capture-scope adoption,
        /// info-file adoption, common-info clearing, or approved_permission
        /// adopt/clear).
        #[arg(long)]
        confirm: bool,
    },
    /// Repair worktree metadata, attempting to recover from inconsistencies.
    /// With a path, restores that linked worktree's gitdir identity
    /// (`.libra/worktree_id` + `commondir`) from the registry's persisted
    /// stable id (registry v2, W3 §C.7).
    Repair {
        /// Linked worktree whose gitdir identity should be restored (or,
        /// with --migrate-layout, the single legacy worktree to migrate).
        path: Option<String>,
        /// Migrate legacy shared-`.libra` symlink worktrees to the isolated
        /// layout (W3-s3 §C.6.2). Runs from the MAIN worktree; without a
        /// path every legacy-symlink entry is migrated.
        #[arg(long)]
        migrate_layout: bool,
        /// With --migrate-layout: report what would be migrated, write
        /// nothing.
        #[arg(long)]
        dry_run: bool,
        /// Confirm a MUTATING repair action (W0 §C.11, Codex R16/R17).
        /// Required for the no-arg registry repair, `repair <path>`, and a
        /// non-dry-run `--migrate-layout`: without it the command refuses
        /// before any registry lock, database write, or filesystem touch
        /// (zero side effects), and with it the action records exactly one
        /// operation-log audit event (see `libra op log`). The read-only
        /// `--migrate-layout --dry-run` never needs it.
        #[arg(long)]
        confirm: bool,
        /// Resolve an AMBIGUOUS registry by unregistering the entry at
        /// `<path>` (W1 §C.7).
        ///
        /// `add A` → `move A B` → `add A` under an older binary leaves two
        /// entries claiming one path-derived identity. Every mutation is
        /// refused while that holds — including the remove that would fix it —
        /// so this is the one action that runs against the ambiguous registry.
        /// It DETACHES the named entry: the directory and its scoped rows are
        /// kept, and every command inside that directory fails closed until
        /// you re-add it or finish with `remove --delete-dir`. The surviving
        /// claimant owns the identity again. Requires --yes, because choosing
        /// which entry to detach is a judgement only the user can make.
        #[arg(long)]
        resolve_identity: bool,
        /// Confirm a --resolve-identity unregistration.
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeListOutput {
    pub(crate) worktrees: Vec<WorktreeListEntry>,
}

/// plan-20260714 W0 (§C.11): the read-only `worktree doctor` report.
///
/// `next_cursor` is present and always `null` here. W4 turns this into a
/// paginated machine interface with a frozen envelope; carrying the field
/// from the start means that wave ADDS pagination rather than reshaping a
/// document consumers already parse.
#[derive(Debug, Serialize)]
pub(crate) struct WorktreeListEntry {
    pub(crate) kind: &'static str,
    pub(crate) path: String,
    pub(crate) is_main: bool,
    pub(crate) locked: bool,
    pub(crate) lock_reason: Option<String>,
    pub(crate) exists: bool,
    /// Stable worktree identity (Part C §C.3.3): `None` = the main worktree
    /// (`worktree_id IS NULL`), `Some(id)` = a linked worktree. Consumers must
    /// use this as the primary key, never the path.
    pub(crate) worktree_id: Option<String>,
    /// Lifecycle state (W3-s1b): `active`, `detached_from_registry`, or
    /// `tombstone`.
    pub(crate) state: &'static str,
    /// Registration generation (W1 §C.4.1.1 service fence). A client that
    /// caches a worktree's identity must carry this too: ids and paths are
    /// both reused when a worktree is re-added in place, and the epoch is what
    /// tells the two registrations apart. `0` for main and for entries written
    /// before the field existed.
    pub(crate) epoch: u64,
    /// On-disk layout (W3-s3 §C.6.1): `main`, `linked-v2`, `legacy-symlink`
    /// (a pre-isolation `.libra` symlink sharing main's HEAD/index),
    /// `missing`, `corrupt`, or `task-fuse` for FUSE task worktrees.
    pub(crate) layout: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeAddOutput {
    path: String,
    already_exists: bool,
    /// The path was a DETACHED worktree and this add re-attached it (its
    /// scoped state and identity resume unchanged).
    reattached: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeLockOutput {
    path: String,
    locked: bool,
    lock_reason: Option<String>,
    changed: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeUnlockOutput {
    path: String,
    locked: bool,
    changed: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeMoveOutput {
    source: String,
    destination: String,
    registry_updated: bool,
    disk_directory_moved: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreePruneOutput {
    pruned: Vec<String>,
    pruned_count: usize,
    /// Entries whose directory is gone but whose scoped cleanup failed —
    /// kept as tombstones for `worktree repair` to retry.
    tombstoned: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeRemoveOutput {
    path: String,
    registry_removed: bool,
    disk_directory_deleted: bool,
    /// Keep-dir remove (W3-s1b): the entry moved to `detached_from_registry`
    /// — scoped DB rows are preserved and the directory is frozen until
    /// re-add or `--delete-dir`.
    detached: bool,
    /// `--delete-dir` deleted the directory but the scoped-row cleanup
    /// failed; a tombstone entry remains for `worktree repair` to retry.
    tombstone: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorktreeRepairOutput {
    changed: bool,
    /// Stale intent-journal rows rolled forward/back and resolved.
    journal_recovered: usize,
    /// Tombstone entries whose scoped cleanup finally succeeded.
    tombstones_cleaned: usize,
    /// Tombstone entries still pending (cleanup failed again).
    tombstones_pending: usize,
    /// Human-readable recovery notes (also printed).
    notes: Vec<String>,
}

#[cfg(unix)]
#[derive(Debug, Serialize)]
pub(crate) struct WorktreeUmountOutput {
    mountpoint: String,
    unmounted: bool,
    cleanup_requested: bool,
    cleanup_root: Option<String>,
    cleanup_root_removed: bool,
}

pub(crate) type WorktreeResult<T> = Result<T, WorktreeError>;

#[derive(Debug)]
pub(crate) enum WorktreeError {
    InvalidTarget(String),
    OperationBlocked(String),
    NoSuchWorktree { path: String },
    MainWorktree { action: &'static str, path: String },
    LockedWorktree { action: &'static str, path: String },
    DirtyWorktree { path: String },
    StateRead { path: PathBuf, source: io::Error },
    StateWrite { path: PathBuf, source: io::Error },
    StateCorrupt { path: PathBuf, source: String },
    StateRepair { source: io::Error },
    IoRead(String),
    IoWrite(String),
}

impl WorktreeError {
    fn stable_code(&self) -> StableErrorCode {
        match self {
            Self::InvalidTarget(_)
            | Self::NoSuchWorktree { .. }
            | Self::MainWorktree { .. }
            | Self::LockedWorktree { .. } => StableErrorCode::CliInvalidTarget,
            Self::OperationBlocked(_) | Self::DirtyWorktree { .. } => {
                StableErrorCode::ConflictOperationBlocked
            }
            Self::StateCorrupt { .. } | Self::StateRepair { .. } => StableErrorCode::RepoCorrupt,
            Self::StateRead { .. } | Self::IoRead(_) => StableErrorCode::IoReadFailed,
            Self::StateWrite { .. } | Self::IoWrite(_) => StableErrorCode::IoWriteFailed,
        }
    }

    pub(crate) fn into_cli_error(self) -> CliError {
        let code = self.stable_code();
        let mut error = CliError::fatal(self.to_string()).with_stable_code(code);
        if matches!(self, Self::DirtyWorktree { .. }) {
            error = error.with_hint(
                "commit or stash changes, or remove without --delete-dir to keep the directory",
            );
        }
        error
    }
}

impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTarget(message)
            | Self::OperationBlocked(message)
            | Self::IoRead(message)
            | Self::IoWrite(message) => f.write_str(message),
            Self::NoSuchWorktree { path } => write!(f, "no such worktree: {path}"),
            Self::MainWorktree { action, path } => {
                write!(f, "cannot {action} main worktree: {path}")
            }
            Self::LockedWorktree { action, path } => {
                write!(f, "cannot {action} locked worktree: {path}")
            }
            Self::DirtyWorktree { path } => {
                write!(
                    f,
                    "cannot delete dirty worktree '{path}' (uncommitted changes)"
                )
            }
            Self::StateRead { path, source } => {
                write!(
                    f,
                    "failed to read worktree state '{}': {source}",
                    path.display()
                )
            }
            Self::StateWrite { path, source } => {
                write!(
                    f,
                    "failed to write worktree state '{}': {source}",
                    path.display()
                )
            }
            Self::StateCorrupt { path, source } => {
                write!(
                    f,
                    "worktree state '{}' is corrupt: {source}",
                    path.display()
                )
            }
            Self::StateRepair { source } => {
                write!(f, "failed to repair worktree state invariant: {source}")
            }
        }
    }
}

impl std::error::Error for WorktreeError {}

/// RAII guard that temporarily changes the process current directory.
///
/// When created with `change_to`, it switches the current directory to the
/// provided path and remembers the previous one. When dropped, it restores
/// the original directory, even if the inner operation panics or early-returns.
struct DirGuard {
    old_dir: PathBuf,
    _cwd_lock: crate::utils::test::CwdLockGuard,
}

impl DirGuard {
    fn change_to(new_dir: &Path) -> io::Result<Self> {
        let cwd_lock = crate::utils::test::cwd_lock_guard();
        let old_dir = env::current_dir()?;
        env::set_current_dir(new_dir)?;
        Ok(Self {
            old_dir,
            _cwd_lock: cwd_lock,
        })
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = env::set_current_dir(&self.old_dir);
    }
}

/// Entry point for the `worktree` subcommand.
///
/// This function verifies that a Libra repository exists and then dispatches
/// to the concrete handler for the requested worktree operation. Any `io::Error`
/// returned from handlers is formatted as a `fatal:` message on stderr.
#[cfg_attr(all(unix, feature = "worktree-fuse"), allow(dead_code))]
pub async fn execute(args: WorktreeArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Dispatches to the appropriate worktree sub-command
/// (add, list, lock, unlock, move, prune, remove, repair, and Unix umount).
/// Part C bare boundary (plan-20260714 §C.4.1): a bare repository has no
/// working trees at all — `WorktreeScope::Main` presumes a main working
/// tree, and the registry's authoritative-root election presumes storage
/// lives at `<root>/.libra`. The whole worktree family is refused with a
/// stable error BEFORE any registry IO; bare worktree semantics are
/// deferred by design (intentionally-different, see COMPATIBILITY.md).
///
/// Classification is CONFIG-FIRST: `init` records `core.bare` in the
/// repository config, which survives any directory name (`init --bare
/// .libra` creates bare storage literally named `.libra`, defeating a
/// basename probe). The storage-basename heuristic remains only as a
/// fallback for repositories predating the config key.
pub(crate) async fn reject_bare_repository() -> CliResult<()> {
    let storage = util::storage_path();
    reject_bare_repository_impl(
        &storage,
        crate::internal::config::ConfigKv::get("core.bare").await,
    )
}

/// No-migration counterpart of [`reject_bare_repository`] for the refused
/// repair path. An unconfirmed repair is documented as zero-side-effect, so
/// it cannot take the migration-applying open above — yet the bare boundary
/// must still precede the confirmation refusal (a bare repository has no
/// working trees at all, the more fundamental error). Classify through a
/// connection that does not apply pending migrations.
pub(crate) async fn reject_bare_repository_without_migrations() -> CliResult<()> {
    let storage = util::storage_path();
    let db_path = crate::utils::path::database();
    let conn = crate::internal::db::open_database_without_migrations(&db_path)
        .await
        .map_err(|source| {
            CliError::fatal(format!(
                "cannot open the repository database without applying migrations to classify \
                  this repository: {source}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    reject_bare_repository_impl(
        &storage,
        crate::internal::config::ConfigKv::get_with_conn(&conn, "core.bare").await,
    )
}

fn reject_bare_repository_impl(
    storage: &std::path::Path,
    entry: anyhow::Result<Option<crate::internal::config::ConfigKvEntry>>,
) -> CliResult<()> {
    use crate::internal::config::parse_git_bool;
    // FAIL CLOSED on read failures and unparseable values — a bare boundary
    // that cannot be determined must refuse, not fall through to the
    // basename heuristic (which a `.libra`-named bare directory defeats).
    let is_bare = match entry {
        Ok(Some(entry)) => parse_git_bool(&entry.value).ok_or_else(|| {
            CliError::fatal(format!(
                "invalid core.bare value '{}': expected true/false/yes/no/on/off/1/0",
                entry.value
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?,
        // Key absent: repositories predating the recorded flag — fall back
        // to the standard-layout heuristic.
        Ok(None) => storage.file_name() != Some(std::ffi::OsStr::new(util::ROOT_DIR)),
        Err(error) => {
            return Err(CliError::fatal(format!(
                "cannot read core.bare to classify this repository: {error}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed));
        }
    };
    if is_bare {
        return Err(CliError::fatal(format!(
            "this is a bare repository ('{}'): it has no working trees, so the \
              `worktree` command family is unavailable here",
            storage.display()
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid));
    }
    Ok(())
}

pub async fn execute_safe(args: WorktreeArgs, output: &OutputConfig) -> CliResult<()> {
    let command = args.command;
    #[cfg(unix)]
    let needs_repo = !matches!(&command, WorktreeSubcommand::Umount { .. });
    #[cfg(not(unix))]
    let needs_repo = true;
    // W0 §C.11: `doctor` skips the migration-applying open below. It is a
    // read-only diagnostic, and applying migrations is a write — the one
    // command you want available on a repository you have not yet decided to
    // upgrade must not upgrade it as a side effect. An UNCONFIRMED mutating
    // repair skips it too (Codex R19 follow-up): the refusal is documented
    // as zero-side-effect, so it must reach `require_repair_confirmation`
    // without any migration-applying open on the way. The `--migrate-layout
    // --dry-run` preview skips it as well (Codex R20 follow-up): it is
    // documented as read-only end to end, so it enumerates layouts without
    // ever resolving a database connection.
    let readonly_layout_preview = repair_readonly_layout_preview(&command);
    let applies_migrations = !matches!(&command, WorktreeSubcommand::Doctor { .. })
        && !repair_invocation_refused_without_confirmation(&command)
        && !readonly_layout_preview;

    if needs_repo {
        util::require_repo().map_err(|_| CliError::repo_not_found())?;
    }
    if needs_repo && applies_migrations {
        // §C.7 ordering: apply pending repository migrations — including the
        // registry-v2 capability marker (2026072401) — BEFORE any
        // worktrees.json read or rewrite, so a pre-v2 binary is refused at
        // connect time no matter which worktree command first touches the v2
        // file. This also refuses a future-schema database gracefully
        // instead of parsing a registry this binary does not understand.
        // (Migrations may already have applied at the top-level CLI
        // preflight — the contract shared by repository commands using the
        // standard schema preflight, not a worktree-family side effect.)
        crate::internal::db::get_db_conn_instance_for_path(&crate::utils::path::database())
            .await
            .map_err(|source| {
                CliError::fatal(format!(
                    "cannot open the repository database before touching the worktree \
                      registry: {source}"
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
        // Bare boundary: refused before ANY registry IO (the config read
        // needs the database opened just above).
        reject_bare_repository().await?;
    } else if needs_repo
        && (repair_invocation_refused_without_confirmation(&command) || readonly_layout_preview)
    {
        // The bare boundary precedes even the confirmation refusal — a bare
        // repository has no working trees at all, which is the more
        // fundamental error — but the refused repair and the read-only
        // layout preview must stay zero-side-effect, so classify through the
        // no-migration open.
        reject_bare_repository_without_migrations().await?;
    }

    match command {
        WorktreeSubcommand::Add {
            path,
            target,
            detach,
            new_branch,
        } => {
            let result = add_worktree(path, target, detach, new_branch)
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_add_worktree(&result, output)
        }
        WorktreeSubcommand::List {
            porcelain,
            schema_version,
        } => list_worktrees(output, porcelain, schema_version).await,
        WorktreeSubcommand::Lock { path, reason } => {
            let result = lock_worktree(path, reason)
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_lock_worktree(&result, output)
        }
        WorktreeSubcommand::Unlock { path } => {
            let result = unlock_worktree(path)
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_unlock_worktree(&result, output)
        }
        WorktreeSubcommand::Move { src, dest } => {
            let result = move_worktree(src, dest)
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_move_worktree(&result, output)
        }
        WorktreeSubcommand::Prune => {
            let result = prune_worktrees()
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_prune_worktrees(&result, output)
        }
        WorktreeSubcommand::Remove { path, delete_dir } => {
            let result = remove_worktree(path, delete_dir)
                .await
                .map_err(WorktreeError::into_cli_error)?;
            render_remove_worktree(&result, output)
        }
        #[cfg(unix)]
        WorktreeSubcommand::Umount { path, cleanup } => {
            let result = umount_fuse_path(path, cleanup).map_err(WorktreeError::into_cli_error)?;
            render_umount_fuse_path(&result, output)
        }
        WorktreeSubcommand::Doctor {
            workspace_id,
            limit,
            cursor,
            adopt_capture_session,
            adopt_info_to,
            clear_common_info,
            adopt_approved_project,
            clear_approved_project,
            confirm,
        } => {
            if let Some(session_id) = adopt_capture_session {
                let workspace_id = workspace_id.ok_or_else(|| {
                    CliError::command_usage(
                        "--adopt-capture-session requires the target WORKSPACE_ID",
                    )
                })?;
                adopt_legacy_capture_scope(&workspace_id, &session_id, confirm, output).await
            } else if let Some(legacy_project_id) = adopt_approved_project {
                adopt_or_clear_legacy_approved_project(
                    &legacy_project_id,
                    confirm,
                    /* clear */ false,
                    output,
                )
                .await
            } else if let Some(legacy_project_id) = clear_approved_project {
                adopt_or_clear_legacy_approved_project(
                    &legacy_project_id,
                    confirm,
                    /* clear */ true,
                    output,
                )
                .await
            } else if let Some(target) = adopt_info_to {
                // W0 §C.4.1.1: explicit, confirmed, audited — like every
                // other mutating doctor/repair action.
                require_repair_confirmation(confirm, "worktree doctor --adopt-info-to")?;
                let boundary =
                    begin_repair_operation("worktree doctor --adopt-info-to", Some(&target))
                        .await?;
                let result = adopt_common_info_files(&target);
                let result = finish_repair_operation(boundary, result).await?;
                if output.is_json() {
                    // Distinct envelope, like `worktree.doctor.adopt_capture`
                    // — the read-only `worktree.doctor` page schema stays
                    // untouched.
                    return emit_json_data(
                        "worktree.doctor.adopt_info",
                        &serde_json::json!({ "target": target, "report": result }),
                        output,
                    );
                }
                println!("{result}");
                Ok(())
            } else if clear_common_info {
                require_repair_confirmation(confirm, "worktree doctor --clear-common-info")?;
                let boundary =
                    begin_repair_operation("worktree doctor --clear-common-info", None).await?;
                let result = clear_common_info_files();
                let result = finish_repair_operation(boundary, result).await?;
                if output.is_json() {
                    return emit_json_data(
                        "worktree.doctor.clear_common_info",
                        &serde_json::json!({ "report": result }),
                        output,
                    );
                }
                println!("{result}");
                Ok(())
            } else {
                run_worktree_doctor(workspace_id, limit, cursor, output).await
            }
        }
        WorktreeSubcommand::Repair {
            path,
            migrate_layout,
            dry_run,
            confirm,
            resolve_identity,
            yes,
        } => {
            if resolve_identity {
                let Some(path) = path else {
                    return Err(WorktreeError::OperationBlocked(
                        "--resolve-identity needs the path of the entry to unregister".to_string(),
                    )
                    .into_cli_error());
                };
                if !yes {
                    return Err(WorktreeError::OperationBlocked(format!(
                        "--resolve-identity detaches '{path}' from the registry — its files and \
                          scoped state are kept, and every command inside it will fail closed \
                          until you re-add it or run `remove --delete-dir`; re-run with --yes to \
                          confirm"
                    ))
                    .into_cli_error());
                }
                // W0 (§C.11, Codex R18): `--yes` is this action's dedicated
                // confirmation; once given, the detach runs inside the same
                // one-row operation-log audit boundary as every other
                // mutating repair, closed with the action's outcome.
                let boundary = begin_repair_operation(
                    &format!("worktree repair {path} --resolve-identity"),
                    Some(&path),
                )
                .await?;
                let result = resolve_identity_collision(&path)
                    .await
                    .map_err(WorktreeError::into_cli_error);
                let result = finish_repair_operation(boundary, result).await?;
                if !output.is_json() {
                    println!("{result}");
                }
                return Ok(());
            }
            // W0 (§C.11, Codex R16/R17): each mutating repair action is gated
            // on `--confirm` (the refusal precedes EVERY side effect) and,
            // once confirmed, runs inside one operation-log audit boundary —
            // exactly one row per executed action, closed with the action's
            // outcome. `--dry-run` writes nothing and stays confirmation-free.
            if migrate_layout {
                if dry_run {
                    let result = migrate_layout_run(path, dry_run)
                        .await
                        .map_err(WorktreeError::into_cli_error)?;
                    return render_migrate_layout(&result, output);
                }
                require_repair_confirmation(confirm, "worktree repair --migrate-layout")?;
                let boundary =
                    begin_repair_operation("worktree repair --migrate-layout", path.as_deref())
                        .await?;
                let result = migrate_layout_run(path, dry_run)
                    .await
                    .map_err(WorktreeError::into_cli_error);
                let result = finish_repair_operation(boundary, result).await?;
                return render_migrate_layout(&result, output);
            }
            if let Some(path) = path {
                require_repair_confirmation(confirm, &format!("worktree repair {path}"))?;
                let boundary =
                    begin_repair_operation("worktree repair <path>", Some(&path)).await?;
                let result = repair_worktree_identity(path)
                    .await
                    .map_err(WorktreeError::into_cli_error);
                let result = finish_repair_operation(boundary, result).await?;
                return render_repair_identity(&result, output);
            }
            require_repair_confirmation(confirm, "worktree repair")?;
            let boundary = begin_repair_operation("worktree repair", None).await?;
            let result = repair_worktrees()
                .await
                .map_err(WorktreeError::into_cli_error);
            let result = finish_repair_operation(boundary, result).await?;
            render_repair_worktrees(&result, output)
        }
    }
}

/// W0 (§C.11, Codex R19 follow-up): an UNCONFIRMED mutating repair is
/// documented as byte-for-byte side-effect free — and applying pending
/// schema migrations is a WRITE. An invocation this predicate marks must
/// therefore skip every migration-applying database open (the CLI preflight
/// AND the dispatch-time open in this file and in `worktree-fuse.rs`) so it
/// reaches only `require_repair_confirmation`'s pure refusal. Confirmed
/// actions keep the standard schema preflight: their audit boundary writes.
/// W0 (§C.11, Codex R20 follow-up): the `--migrate-layout --dry-run` preview
/// is documented as read-only end to end — and applying pending schema
/// migrations is a WRITE. An invocation this predicate marks must therefore
/// skip every migration-applying database open (the CLI preflight AND the
/// dispatch-time opens in this file and in `worktree-fuse.rs`) and never
/// resolve the global connection: the preview needs only the lockless
/// registry read and on-disk layout detection. The preview stays
/// confirmation-free by design, so it is NOT part of
/// [`repair_invocation_refused_without_confirmation`].
/// W0 (§C.11, Codex R16/R17): a MUTATING repair action runs only behind an
/// explicit `--confirm`. The refusal precedes every registry lock, database
/// write and filesystem touch, so an unconfirmed invocation is byte-for-byte
/// side-effect free — the property the table-driven regression
/// `worktree_doctor_mutations_require_confirmation_and_emit_audit` asserts.
/// The CLI's v2 operation boundary owns the audit row for confirmed repair
/// actions. Keep this small seam for callers that already structure the repair
/// flow as begin/finish, without reopening the retired v1 operation wrapper.
/// What main-scope layer/sparse state exists, for the doctor finding above.
///
/// `Ok(None)` means "nothing, and that is known". Every read failure — a
/// missing table included — is an `Err`, and the caller reports it: §C.13
/// requires doctor to be fail-closed, and a diagnostic that silently reports
/// "nothing to see" because it could not look is worse than one that says so.
///
/// The connection comes from the caller — under `worktree doctor` that is the
/// no-migration open (§C.11 W0): a diagnostic must never upgrade the database
/// it is observing, so this helper must NOT resolve `get_db_conn_instance()`
/// (which applies pending migrations) on its own.
async fn adopted_scope_settings_present(
    conn: &sea_orm::DatabaseConnection,
) -> Result<Option<String>, String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let count = async |sql: &str| -> Result<i64, String> {
        conn.query_one_raw(Statement::from_string(DbBackend::Sqlite, sql.to_string()))
            .await
            .map_err(|error| format!("{error}"))?
            .ok_or_else(|| "a COUNT query returned no row".to_string())?
            .try_get_by_index::<i64>(0)
            .map_err(|error| format!("{error}"))
    };

    let mut parts = Vec::new();
    let layers = count("SELECT COUNT(*) FROM `layer` WHERE `worktree_id` = ''").await?;
    if layers > 0 {
        parts.push(format!("{layers} layer registration(s)"));
    }
    // The ownership rows matter more than the registrations: they are what keeps
    // a retained overlay file unstageable.
    let owned = count("SELECT COUNT(*) FROM `layer_path` WHERE `worktree_id` = ''").await?;
    if owned > 0 {
        parts.push(format!("{owned} materialized overlay path(s)"));
    }
    let patterns = count("SELECT COUNT(*) FROM `sparse_view` WHERE `worktree_id` = ''").await?;
    if patterns > 0 {
        parts.push(format!("{patterns} sparse pattern(s)"));
    }
    // A legacy `sparse.enabled = true` with NO patterns migrates into
    // `sparse_view_meta`, so counting patterns alone would miss it.
    let sparse_enabled = count(
        "SELECT COUNT(*) FROM `sparse_view_meta` \
          WHERE `worktree_id` = '' AND `enabled` <> 0",
    )
    .await?;
    if sparse_enabled > 0 {
        parts.push("an enabled sparse view".to_string());
    }
    if parts.is_empty() {
        return Ok(None);
    }
    Ok(Some(parts.join(", ")))
}

/// Unregister one side of an identity collision so the registry becomes
/// unambiguous again (W1 §C.7).
///
/// Runs against the AMBIGUOUS registry on purpose: `load_state` refuses it,
/// which is what makes every other repair impossible, so this uses the repair
/// loader and does its own narrow validation. The directory on disk is never
/// touched — only the registry entry and that scope's rows go, so the user can
/// re-add it afterwards and get a fresh identity and generation.
async fn resolve_identity_collision(path: &str) -> WorktreeResult<String> {
    let _lock = acquire_registry_lock_async().await?;
    let mut state = load_state_for_repair()?;
    let target = resolve_path(path, "worktree path")?;
    let target_key = target.to_string_lossy().to_string();

    let Some(index) = state
        .entries
        .iter()
        .position(|entry| !entry.is_main && entry.path == target_key)
    else {
        return Err(WorktreeError::NoSuchWorktree {
            path: path.to_string(),
        });
    };
    let Some(identity) = state.entries[index].worktree_id.clone() else {
        return Err(WorktreeError::OperationBlocked(format!(
            "the entry at '{target_key}' carries no identity, so it is not part of a collision"
        )));
    };
    let claimants = state
        .entries
        .iter()
        .filter(|entry| {
            !entry.is_main
                && entry.state.is_active()
                && entry.worktree_id.as_deref() == Some(identity.as_str())
        })
        .count();
    if claimants < 2 {
        return Err(WorktreeError::OperationBlocked(format!(
            "'{target_key}' is the only live entry claiming identity '{identity}' — there is \
              no collision to resolve here"
        )));
    }

    // DETACH, do not delete. The directory on disk is a real worktree with the
    // user's files in it: dropping the entry outright would leave a gitdir
    // that still resolves the duplicated identity and could mutate the
    // survivor's scope, and sweeping the identity-keyed rows would take the
    // SURVIVOR's HEAD and reflog with them. Detaching is the lifecycle state
    // that already means "unregistered, rows kept, every command in that
    // directory fails closed until re-add or --delete-dir" — and a detached
    // entry no longer claims the identity, so the collision is resolved.
    //
    // JOURNALLED first, exactly like every other lifecycle action: the marker
    // and the registry are two writes that cannot be joined into one
    // transaction, and a crash between them leaves a frozen directory that the
    // registry still calls active. Reconciliation would then read the marker
    // as stale and lift it, silently undoing the repair. The intent row is
    // what makes `worktree repair` finish it instead.
    let db = crate::internal::db::get_db_conn_instance().await;
    let payload = serde_json::json!({
        "path": target.to_string_lossy(),
        "delete_dir": false,
        "reason": "resolve_identity",
    });
    let journal_id = journal_append(
        &db,
        WorktreeControl::Remove.declare(),
        Some(&identity),
        &payload,
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;

    write_detached_marker(&target, &identity)?;
    // The SQL lifecycle mirror BEFORE the registry write, like every other
    // detach path: the 2026072402 down-migration guard reads only the
    // mirror/journal/sequencer tables, so a detach recorded solely in the
    // registry file would let the rollback proceed while a detached
    // directory still exists on disk.
    lifecycle_upsert(
        &db,
        &identity,
        WorktreeEntryState::DetachedFromRegistry.as_str(),
        &target.to_string_lossy(),
    )
    .await
    .map_err(WorktreeError::OperationBlocked)?;
    state.entries[index].state = WorktreeEntryState::DetachedFromRegistry;
    save_state(&state).map_err(|error| {
        WorktreeError::IoWrite(format!("failed to write the worktree registry: {error}"))
    })?;
    if let Err(error) = journal_resolve(&db, journal_id).await {
        tracing::warn!(error, "detach journal row not resolved; repair reconciles");
    }
    Ok(format!(
        "Detached '{target_key}' from the registry (identity '{identity}'). Its files and \
          scoped state are kept and every command inside it now fails closed; the remaining \
          claimant owns the identity again. Finish with `libra worktree remove --delete-dir \
          {target_key}`, or `libra worktree add {target_key}` to re-attach it."
    ))
}

/// Returns the path to the on-disk worktree state file.
fn resolve_path(path: impl AsRef<Path>, role: &'static str) -> WorktreeResult<PathBuf> {
    let path = path.as_ref();
    canonicalize(path).map_err(|source| {
        WorktreeError::IoRead(format!(
            "failed to resolve {role} '{}': {source}",
            path.display()
        ))
    })
}

/// Implements `worktree add <path>`.
///
/// This command:
/// - validates the requested path is outside `.libra` storage,
/// - creates the target directory if it does not exist,
/// - rejects paths that canonicalize inside `.libra` (with cleanup),
/// - ensures the worktree is not already registered,
/// - creates a real per-worktree `.libra` gitdir (its own local HEAD, index,
///   and HEAD reflog) that records a `commondir` pointer to the shared object
///   store and a stable `worktree_id` — it is NOT a symlink to shared storage,
/// - when `HEAD` exists, populates the new worktree from committed `HEAD`
///   content (not staged-only index changes).
///
/// The checkout the new worktree is seeded with (W3-s2 §C.7), resolved
/// FAIL-CLOSED before any side effect.
enum AddCheckout {
    /// No target: detached at the source worktree's current commit
    /// (intentionally different from Git's basename-branch default).
    DetachedAtSource,
    /// Explicit commit-ish (or a branch under `--detach`).
    Detached(git_internal::hash::ObjectHash),
    /// Check out an existing branch (refused when any scope has it out).
    AttachBranch { name: String },
    /// `-b`: create the branch at `start`, then check it out. Fully rolled
    /// back on any later failure (no branch-only residue).
    CreateBranch {
        name: String,
        start: git_internal::hash::ObjectHash,
    },
}

/// Fence worktree lifecycle actions that publish or remove shared HEAD/branch
/// rows. The registry lock is acquired by each lifecycle operation before this
/// lease so concurrent adds queue on the registry before any other work starts.
async fn acquire_worktree_ref_lease()
-> WorktreeResult<Option<crate::internal::operation::middleware::ScopeLease>> {
    // The v2 CLI boundary already holds the repository lease for a central
    // `worktree` mutation. Re-acquiring the same flock from this process would
    // self-deadlock before the command body starts; standalone callers still
    // acquire the lease here.
    if crate::internal::operation::middleware::repository_ref_lease_is_held() {
        return Ok(None);
    }
    let scope = crate::internal::worktree_scope::WorktreeScope::request_scope()
        .or_else(|| crate::internal::worktree_scope::RequestScope::resolve(util::cur_dir()))
        .ok_or_else(|| {
            WorktreeError::OperationBlocked(
                "cannot acquire repository ref lease outside a pinned worktree".to_string(),
            )
        })?;
    let db_path = crate::utils::path::database();
    let db = crate::internal::db::get_db_conn_instance_for_path(&db_path)
        .await
        .map_err(|error| {
            WorktreeError::OperationBlocked(format!(
                "cannot open repository database before changing worktree refs: {error}"
            ))
        })?;
    let repo_id = RepoIdentity::resolve(&db)
        .await
        .map_err(|error| {
            WorktreeError::OperationBlocked(format!(
                "cannot resolve repository identity before changing worktree refs: {error}"
            ))
        })?
        .as_str()
        .to_string();
    let shared_repository =
        crate::internal::config::ConfigKv::get_with_conn(&db, "core.sharedRepository")
            .await
            .map_err(|error| {
                WorktreeError::OperationBlocked(format!(
                    "cannot read core.sharedRepository before changing worktree refs: {error}"
                ))
            })?;
    if shared_repository
        .as_ref()
        .is_some_and(|entry| entry.encrypted)
    {
        return Err(WorktreeError::OperationBlocked(
            "core.sharedRepository must be plaintext before changing worktree refs".to_string(),
        ));
    }
    crate::internal::operation::middleware::ScopeLease::acquire_repository_wait(
        &scope,
        &repo_id,
        shared_repository.as_ref().map(|entry| entry.value.as_str()),
    )
    .await
    .map(Some)
    .map_err(|error| {
        WorktreeError::OperationBlocked(format!(
            "cannot acquire repository ref lease before changing worktree refs: {error}"
        ))
    })
}

/// The `worktree add` target-filesystem probe (W0 §C.4.1.1): compare the
/// repository's persisted `core.ignorecase` with what the target volume
/// actually does, and warn on a mismatch. Best-effort — a probe or config
/// read failure must never fail the add.
async fn warn_on_case_probe_mismatch(target: &std::path::Path) {
    let Ok(Some(entry)) =
        crate::internal::config::ConfigKv::get_var_case_insensitive("core.", "ignorecase").await
    else {
        // No persisted value: materialization guards probe per use, which
        // already answers from this worktree's own filesystem.
        return;
    };
    let persisted = matches!(
        entry.value.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "on" | "1"
    );
    let probed = crate::utils::path_case::probe_dir_ignore_case(target);
    if probed != persisted {
        eprintln!(
            "warning: this worktree's filesystem is case-{} but the repository's persisted \
              core.ignorecase is {persisted} (probed from the main worktree at init); \
              case-collision guards here may misjudge until the per-worktree config overlay \
              lands (plan-20260714 W4). Set core.ignorecase explicitly if this repository \
              spans differing filesystems.",
            if probed { "insensitive" } else { "sensitive" }
        );
    }
}

/// Re-attach a detached worktree (W3-s1b, §C.7): verify the directory still
/// carries the SAME identity the registry persisted, then lift the
/// fail-closed marker and reactivate the entry. An identity mismatch (the
/// directory was recreated or swapped) refuses — never silently adopt.
/// GC a removed worktree's PRIVATE HEAD + HEAD-reflog rows (lore.md 2.1) — and
/// its worktree-scoped sequencer/bisect session rows (Part C W1) — so a reused
/// instance id never inherits stale state. Instance ids are DETERMINISTIC
/// (FNV of the canonical path), so a worktree re-added at the same path gets
/// the same id: a surviving `bisect_state`/`sequence_state` row would make the
/// fresh worktree silently resume a dead session (a resumed bisect step even
/// repaints candidate trees — data loss). Best-effort: a failure is logged,
/// not fatal (the registry drop is the source of truth).
/// Scoped-row sweep (W3-s1b: every caller is STRICT — a failed cleanup
/// becomes a tombstone or fails the operation, never a silent orphan): the
/// first failed DELETE
/// aborts and surfaces the error. `worktree add` uses this as its pre-seed
/// sweep — inheriting another (removed) worktree's rows must fail the add,
/// not proceed with a polluted scope.
/// Upsert this worktree's row in the SQL `worktree_lifecycle` mirror
/// (§C.7 W3-s1b) — the down-migration guard and doctor read lifecycle state
/// from SQL, so every registry state change writes the mirror too.
/// Delete this worktree's lifecycle mirror row (entry back to active, or
/// fully cleaned up).
/// Every row of the SQL lifecycle mirror, for the repair sweep that keeps
/// it convergent with the registry (stale rows would block the down
/// migration forever; missing rows would let it proceed wrongly).
/// Tri-state path probe for recovery decisions: only a NotFound stat
/// PROVES absence; any other error is AMBIGUOUS (permissions, an unmounted
/// volume) and must keep the intent journal pending rather than letting a
/// recovery branch guess.
pub(crate) enum PathPresence {
    Present,
    Missing,
    Unknown(String),
}

/// A pending row in the durable intent journal.
#[derive(Debug, Clone)]
pub(crate) struct PendingIntent {
    id: i64,
    op: String,
    worktree_id: Option<String>,
    payload: serde_json::Value,
}

/// Record a registry-mutation intent BEFORE any filesystem/registry write
/// (§C.7). SQLite cannot join a filesystem rename into one transaction, so
/// this is the recovery anchor: a crash leaves the row behind and
/// `worktree repair` rolls the operation forward or back deterministically.
/// A journal write failure ABORTS the mutation (fail-closed).
/// Advance a 'migrate' intent's durable stage (§C.6.2 state machine).
/// Resolve (delete) a journal row after the mutation is fully published.
/// Enumerate stale pending intents for `worktree repair` recovery.
/// True when this scope has ACTIVE sequencer/rebase/bisect state — remove
/// and prune refuse to detach/GC such a worktree (§C.7: "active
/// sequencer/bisect … 默认拒绝"). Fails CLOSED (treated active) on a read
/// error: an unreadable state must not be destroyed.
/// Create a linked worktree's REAL `.libra` gitdir (lore.md 2.1) instead of a
/// symlink: it holds `commondir` (pointing at the shared `.libra` for
/// db/objects/hooks) and `worktree_id` (its private HEAD/index scope). The
/// per-worktree `index` is created later when the worktree is populated.
/// Implements `worktree list`.
///
/// Each registered worktree is printed on its own line as either
/// `main <path>` or `worktree <path>`, with optional `[locked: <reason>]`
/// suffix when the entry is locked.
/// [`run_list_worktrees`] against an EXPLICIT registry path (§C.4.2) — see
/// `load_state_readonly_at`.
/// Classify a registered worktree's ON-DISK layout (W3-s3 §C.6.1).
/// Read-only: `legacy-symlink` (a pre-isolation `.libra` symlink pointing at
/// the common storage — shares main's HEAD/index) is recognized here so
/// `repair --migrate-layout` can target it and mutation commands can refuse
/// it; anything unexplainable is `corrupt`, never guessed.
/// Resolve a registered worktree's stable id from its path (Part C §C.3.3).
/// The main worktree's HEAD row is keyed `worktree_id IS NULL`, so it returns
/// `None`. A linked worktree returns its stored `.libra/worktree_id`, falling
/// back to the canonical-path derivation used at creation (so a recovered or
/// moved worktree still maps to its own scoped rows rather than aliasing main).
/// Render the worktree list as Git-style `--porcelain` output: one attribute
/// per line, with a blank line between worktrees. In the isolated layout each
/// worktree owns its own HEAD (index and HEAD reflog too), so each entry
/// reports ITS OWN `HEAD <sha>` and either `branch <ref>` or `detached`
/// (Git semantics, Part C §C.3.3) — never the running command's HEAD stamped
/// onto every entry. A worktree with no resolvable HEAD row (a legacy-symlink
/// layout, or a missing/corrupt scope) emits neither line rather than
/// mislabeling it with another worktree's commit.
/// plan-20260714 W0 (§C.11): `worktree doctor` — read-only scope diagnostics.
///
/// Every value here comes from a READ: the registry (already loaded
/// read-only), each entry's on-disk layout, and whether its identity is one
/// the registry knows. Nothing is written, adopted, reclaimed or repaired,
/// because a diagnostic that mutates cannot be run safely on a repository you
/// do not yet understand — which is exactly when you reach for it.
/// The WORKTREE-scope half of `worktree doctor` (§C.11 W0): per-worktree
/// layout, lifecycle and identity findings.
///
/// The workspace-scope half lives in [`run_worktree_doctor`]. Both are
/// reported by one bare invocation, in one envelope, because the W0 document
/// promised that W4 would ADD to this report rather than reshape it.
/// The two W0 §C.4.1.1 info-file names the doctor actions operate on.
const WORKTREE_INFO_FILE_NAMES: [&str; 2] = ["exclude", "attributes"];

// ---------------------------------------------------------------------------
// `worktree doctor` — read-only scope diagnosis (plan-20260714 §C.8/C.13 W4)
// ---------------------------------------------------------------------------
//
// Grammar is FROZEN (§C.8, Codex R18/R19):
//
//     libra worktree doctor [<workspace-id>] [--limit N] [--cursor C]
//     libra worktree doctor <workspace-id> --adopt-capture-session <session-id> --confirm
//
// * no id  -> paginated view: `data.diagnostics[]` + `data.next_cursor`
// * an id  -> single-scope view: `data.diagnostic` (singular, NO pagination)
// * id together with `--limit`/`--cursor` -> usage error (`LBR-CLI-002`)
//
// The default invocation is STRICTLY READ-ONLY: it opens no write path, takes
// no lease, never adopts a legacy row, and never rewrites the registry (it uses
// the lockless `load_state_readonly` reader). Legacy capture adoption is a
// separate, explicitly-confirmed grammar; reclaim/clear/repair are not part
// of this slice, so no hint may promise those actions.

/// `data.schema_version` of both doctor payloads. Additive evolution only.
const DOCTOR_SCHEMA_VERSION: u32 = 1;

/// Version tag inside the opaque cursor. A cursor from a different listing (or
/// a hand-written `workspace_id`) therefore fails to decode instead of silently
/// paginating from an unrelated position.
const DOCTOR_CURSOR_TAG: &str = "wtdoctor1:";

/// Decode an opaque cursor back into the `workspace_id` to resume after.
///
/// Fails closed (`LBR-WORKTREE-001`) rather than restarting at page one: a
/// caller walking the registry page by page would otherwise re-read rows it
/// already processed and believe it had seen everything.
/// A scope is corrupt or unreadable, so the report would be incomplete.
/// The doctor never degrades to a partial diagnosis (§C.13): an operator
/// acting on a silently-truncated report is worse off than one told the scope
/// cannot be read.
/// One finding about a workspace scope. `code` is the stable machine key;
/// `severity` is `warning` (needs attention) or `error` (blocks normal use).
impl ScopeDiagnostic {
    fn warning(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            severity: "warning",
            detail: detail.into(),
        }
    }

    fn error(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            severity: "error",
            detail: detail.into(),
        }
    }
}

/// The frozen per-workspace diagnostic record (§C.8 W4). `workspace_id`,
/// `repo_id`, `lease_state` and `scope_diagnostics` are the required fields;
/// the rest is additive context.
/// The registry entry that owns this record's scope, matched by stable id
/// first and canonical path second (a legacy v1 entry may still lack an id).
/// Compare two stored absolute paths, tolerating symlinked prefixes
/// (`/tmp` vs `/private/tmp`) that only differ once resolved.
/// Build one workspace's diagnosis from state that is already in hand — pure
/// over the record, the registry snapshot and the clock, plus read-only
/// filesystem probes.
/// §C.4.1.1 diagnosability: SCOPED capture rows whose recorded fence no
/// longer matches this workspace's live fence. Every capture/import/export
/// write for them fails closed (the owner claim is immutable by trigger),
/// so without this finding a reclaimed workspace's history dead-ends with a
/// refusal pointing at doctor — and doctor said nothing.
/// plan-20260715 W4-07: re-home or clear Always approvals whose opaque
/// `project_id` is not the current `libra.repoid`. Migrations never rewrite
/// those rows; this confirmed doctor action is the only supported path.
/// Explicit mutation for the W4 legacy boundary. Migration 2026080401 marks
/// historical capture rows `legacy_unknown` because their original scope was
/// not recorded. This command is the only supported way to assign one: it
/// requires a live workspace lease/fence, confirmation, and an audit row.
/// Probe, rather than count, legacy capture rows so a diagnostic page stays
/// bounded even in repositories with a long capture history. Before the W4
/// migration these columns do not exist; doctor must remain usable on that
/// older schema and simply has no legacy-scope classification to report.
/// Read-only discovery of opaque Always-approval buckets. Missing table or
/// columns (pre-migration / no-migration doctor open) reports empty.
/// `libra worktree doctor [<workspace-id>] [--limit N] [--cursor C]`.
/// Whether a workspace read failed only because the table does not exist yet.
/// This repository's canonical identity, needed to tell a record of THIS
/// repository from one left behind by a previous identity.
///
/// Deliberately the strict resolver: minting an identity would be a write, and
/// the doctor's default invocation is read-only.
/// Implements `worktree lock <path> [--reason <msg>]`.
///
/// Marks the specified worktree entry as locked and persists an optional
/// human-readable reason. Locking is a state-only operation and does not
/// alter directories on disk.
/// Implements `worktree unlock <path>`.
///
/// Clears the lock flag and reason for the specified worktree entry if it is
/// currently locked. Unlocking is idempotent and leaves the filesystem untouched.
/// Implements `worktree move <src> <dest>`.
///
/// This command:
/// - resolves both source and destination paths,
/// - rejects moves of the main or a locked worktree,
#[cfg(unix)]
#[cfg(all(test, unix))]
mod tests {
    use std::fs;

    use clap::Parser;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn unmount_alias_parses_as_umount() {
        let args = WorktreeArgs::try_parse_from(["worktree", "unmount", "/tmp/mount", "--cleanup"])
            .expect("documented alias `unmount` must parse");
        match args.command {
            WorktreeSubcommand::Umount { path, cleanup } => {
                assert_eq!(path, "/tmp/mount");
                assert!(cleanup);
            }
            other => panic!("unmount alias parsed as {other:?}"),
        }
    }

    #[test]
    fn registry_parse_accepts_v2_shape() {
        let data = br#"{
             "schema_version": 2,
             "entries": [
                 {"path": "/m", "is_main": true, "locked": false, "lock_reason": null},
                 {"path": "/w", "is_main": false, "locked": false, "lock_reason": null,
                  "worktree_id": "abc123"}
             ]
         }"#;
        let state = WorktreeState::parse(data).expect("v2 parses");
        assert_eq!(state.schema_version, REGISTRY_SCHEMA_VERSION);
        assert_eq!(state.entries.len(), 2);
        assert_eq!(state.entries[0].worktree_id, None);
        assert_eq!(state.entries[1].worktree_id.as_deref(), Some("abc123"));
    }

    #[test]
    fn registry_parse_upgrades_v1_shape_in_memory() {
        let data = br#"{
             "worktrees": [
                 {"path": "/m", "is_main": true, "locked": false, "lock_reason": null},
                 {"path": "/w", "is_main": false, "locked": true, "lock_reason": "keep"}
             ]
         }"#;
        let state = WorktreeState::parse(data).expect("v1 upgrades in memory");
        assert_eq!(state.schema_version, REGISTRY_SCHEMA_VERSION);
        assert_eq!(state.entries.len(), 2);
        // Ids stay unfilled on the read-only path; the durable upgrade
        // (load_state) backfills them.
        assert_eq!(state.entries[1].worktree_id, None);
        assert!(state.entries[1].locked);
        assert_eq!(state.entries[1].lock_reason.as_deref(), Some("keep"));
    }

    /// Fail-closed discrimination: a document carrying any v2 key must be a
    /// fully valid v2 registry — it never falls through to the lenient v1
    /// reader, so a malformed/hybrid file cannot be misread as (and later
    /// rewritten from) a stale embedded `worktrees` array.
    #[test]
    fn registry_parse_refuses_hybrid_and_malformed_v2_shapes() {
        // v2 marker + malformed entries + a plausible legacy array: must NOT
        // fall back to reading the stale v1 array.
        let hybrid = br#"{
             "schema_version": 2,
             "entries": "corrupt",
             "worktrees": [
                 {"path": "/m", "is_main": true, "locked": false, "lock_reason": null}
             ]
         }"#;
        assert!(WorktreeState::parse(hybrid).is_err());

        // Valid v2 alongside a stray legacy array is ambiguous — refused,
        // never silently ignored.
        let ambiguous = br#"{"schema_version": 2, "entries": [], "worktrees": []}"#;
        assert!(WorktreeState::parse(ambiguous).is_err());

        // A v2-marked document with malformed entries and NO legacy array is
        // corrupt, not empty.
        let malformed = br#"{"schema_version": 2, "entries": 7}"#;
        assert!(WorktreeState::parse(malformed).is_err());

        // Neither shape at all.
        assert!(WorktreeState::parse(br#"{}"#).is_err());
        assert!(WorktreeState::parse(br#"[]"#).is_err());
    }

    /// The main-entry invariant applies to v1 documents too: read-side
    /// consumers (service gate, rerere probe, cleanup snapshot) must not
    /// consume a mainless legacy registry as an empty root set.
    #[test]
    fn registry_parse_refuses_mainless_v1_shapes() {
        let empty = br#"{"worktrees": []}"#;
        let err = WorktreeState::parse(empty).expect_err("empty v1 fails closed");
        assert!(err.contains("exactly one main"), "{err}");

        let sole_linked = br#"{
             "worktrees": [
                 {"path": "/w", "is_main": false, "locked": false, "lock_reason": null}
             ]
         }"#;
        assert!(WorktreeState::parse(sole_linked).is_err());
    }

    /// v2 must contain exactly one main entry — zero (or several) mains is
    /// corruption a lockless reader must refuse, not silently re-elect.
    #[test]
    fn registry_parse_requires_exactly_one_main() {
        let zero_main = br#"{
             "schema_version": 2,
             "entries": [
                 {"path": "/w", "is_main": false, "locked": false, "lock_reason": null,
                  "worktree_id": "abc123"}
             ]
         }"#;
        let err = WorktreeState::parse(zero_main).expect_err("zero mains fail closed");
        assert!(err.contains("exactly one main"), "{err}");

        let two_mains = br#"{
             "schema_version": 2,
             "entries": [
                 {"path": "/m", "is_main": true, "locked": false, "lock_reason": null},
                 {"path": "/n", "is_main": true, "locked": false, "lock_reason": null}
             ]
         }"#;
        let err = WorktreeState::parse(two_mains).expect_err("two mains fail closed");
        assert!(err.contains("exactly one main"), "{err}");
    }

    /// v2 identity invariants: the registry is the persisted identity
    /// authority — a linked entry with no id (or a main entry with one) is
    /// corruption and must be refused, never patched from the gitdir.
    #[test]
    fn registry_parse_enforces_v2_identity_invariants() {
        let linked_without_id = br#"{
             "schema_version": 2,
             "entries": [
                 {"path": "/m", "is_main": true, "locked": false, "lock_reason": null},
                 {"path": "/w", "is_main": false, "locked": false, "lock_reason": null}
             ]
         }"#;
        let err = WorktreeState::parse(linked_without_id).expect_err("missing id fails closed");
        assert!(err.contains("missing its persisted worktree_id"), "{err}");

        let main_with_id = br#"{
             "schema_version": 2,
             "entries": [
                 {"path": "/m", "is_main": true, "locked": false, "lock_reason": null,
                  "worktree_id": "oops"}
             ]
         }"#;
        let err = WorktreeState::parse(main_with_id).expect_err("main id fails closed");
        assert!(err.contains("must not carry a worktree_id"), "{err}");
    }

    #[test]
    fn registry_parse_refuses_future_schema_version() {
        // 3 is CURRENT since the service-fence generations (migration
        // 2026073005); 4 is the future one. A version this binary does not know
        // is refused rather than reinterpreted — the whole point of the field.
        let data = br#"{"schema_version": 4, "entries": []}"#;
        let err = WorktreeState::parse(data).expect_err("future version fails closed");
        assert!(err.contains("schema_version 4"), "{err}");

        // The refusal is about the VERSION, not the shape: the same document
        // at the current version gets past it and fails on the main-entry
        // invariant instead.
        let current = br#"{"schema_version": 3, "entries": []}"#;
        let err = WorktreeState::parse(current).expect_err("no main entry");
        assert!(
            !err.contains("schema_version"),
            "the current version is not refused for its version: {err}"
        );

        // v2 is still READ and promoted in memory — its entries simply carry
        // no service-fence generations yet.
        let v2 = br#"{"schema_version": 2, "entries":
             [{"path": "/w", "is_main": true, "locked": false}]}"#;
        let parsed = WorktreeState::parse(v2).expect("a v2 registry is read");
        assert_eq!(
            parsed.schema_version, REGISTRY_SCHEMA_VERSION,
            "and is promoted to the current version in memory"
        );
        assert_eq!(
            parsed.linked_history,
            LinkedHistory::Unknown,
            "a pre-v3 registry cannot say its history was `never` (§C.4.3)"
        );
    }

    /// The second belt (§C.7): a v1 binary's `{ worktrees: [...] }` parser
    /// must FAIL on v2 bytes (renamed top-level key) instead of silently
    /// reading an empty registry and rewriting the file.
    #[test]
    fn v1_parser_fails_on_v2_bytes() {
        let v2 = serde_json::to_vec(&WorktreeState {
            epoch_counter: 0,
            linked_history: LinkedHistory::Never,
            schema_version: REGISTRY_SCHEMA_VERSION,
            entries: vec![WorktreeEntry {
                epoch: 0,
                path: "/w".to_string(),
                is_main: false,
                locked: false,
                lock_reason: None,
                worktree_id: Some("abc123".to_string()),
                state: WorktreeEntryState::Active,
            }],
        })
        .expect("serialize v2");
        assert!(serde_json::from_slice::<WorktreeStateV1>(&v2).is_err());
    }

    #[test]
    fn umount_fuse_path_cleans_task_worktree_root_without_repo() {
        let temp = tempdir().expect("create temp dir");
        let cleanup_root = temp
            .path()
            .join("libra-task-worktree-fuse-29353-019ddec6-de60-7383");
        let workspace = cleanup_root.join("workspace");
        fs::create_dir_all(&workspace).expect("create task workspace");
        let canonical_cleanup_root = cleanup_root.canonicalize().expect("canonical cleanup root");
        let canonical_workspace = workspace.canonicalize().expect("canonical workspace");

        let output = umount_fuse_path(cleanup_root.to_string_lossy().to_string(), true)
            .expect("umount cleanup should succeed for inactive task workspace");

        assert_eq!(
            output.mountpoint,
            canonical_workspace.to_string_lossy().as_ref()
        );
        assert!(output.unmounted);
        assert!(output.cleanup_requested);
        assert_eq!(
            output.cleanup_root.as_deref(),
            Some(canonical_cleanup_root.to_string_lossy().as_ref())
        );
        assert!(output.cleanup_root_removed);
        assert!(!cleanup_root.exists());
    }

    /// plan-20260714 W1: the BLOCKING registry acquisition keeps exactly one
    /// caller — the `spawn_blocking` inside `acquire_registry_lock_async`.
    ///
    /// Privacy stops other modules; this stops THIS module from quietly
    /// growing a second inline caller. Taking that `flock` on a runtime
    /// worker strands sqlx's spawned connection-return in the worker's
    /// non-stealable LIFO slot, and with sea-orm's 1-connection SQLite pool
    /// every database user in the process then waits out the acquire
    /// timeout. That deadlock shipped once; it must not ship twice.
    #[test]
    fn blocking_registry_lock_has_a_single_caller() {
        fn collect_sources(dir: &Path, paths: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("worktree modules must be readable") {
                let path = entry
                    .expect("worktree source entry must be readable")
                    .path();
                if path.is_dir() {
                    collect_sources(&path, paths);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    paths.push(path);
                }
            }
        }

        let command_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/command");
        let mut sources = vec![command_dir.join("worktree.rs")];
        collect_sources(&command_dir.join("worktree"), &mut sources);
        assert!(sources.len() > 1, "worktree child modules must be scanned");
        sources.sort();
        let mut callers = Vec::new();
        for path in sources {
            let source = std::fs::read_to_string(&path).expect("worktree source must be readable");
            // The root's inline test below contains the forbidden literal.
            let production = if path == command_dir.join("worktree.rs") {
                source
                    .split("#[cfg(all(test, unix))]")
                    .next()
                    .expect("the facade has a production half")
            } else {
                &source
            };
            callers.extend(
                production
                    .lines()
                    .map(str::trim)
                    .filter(|line| {
                        !line.starts_with("//")
                            && line.contains("acquire_registry_lock")
                            && !line.contains("acquire_registry_lock_async")
                            && !line.starts_with("fn acquire_registry_lock")
                    })
                    .map(|line| (path.clone(), line.to_owned())),
            );
        }
        assert_eq!(
            callers.len(),
            1,
            "the blocking registry acquisition grew a caller outside \
              `acquire_registry_lock_async`: take it on the blocking pool \
              instead (plan-20260714 W1)"
        );
        assert!(callers[0].0.ends_with("worktree/lock.rs"));
        assert_eq!(
            callers[0].1,
            "match tokio::task::spawn_blocking(acquire_registry_lock).await {"
        );
    }
}
