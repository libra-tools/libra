//! Implementation of `maintenance` command for periodic repository maintenance tasks.
//!
//! This command provides Git-compatible `maintenance` functionality for Libra
//! repositories, including running scheduled maintenance tasks, registering
//! repositories for automatic maintenance, and inspecting maintenance state.
//!
//! # Supported Tasks
//! - `gc`: Remove unreachable loose objects and optimize repository storage.
//! - `loose-objects`: Pack old loose objects into a new pack file to reduce
//!   filesystem overhead.
//! - `pack-refs`: Collapse individual ref files into a single `packed-refs` file.
//! - `incremental-repack`: Repack existing pack files to improve access locality.
//! - `commit-graph`: Update the commit-graph file to accelerate history walks.
//! - `prefetch`: Fetch refs from remotes without updating local branches.
//!
//! # Design Notes
//! Task implementations are intentionally conservative: they only mutate the
//! repository when explicitly requested, and `dry-run` mode reports what would
//! be changed without performing any writes. This mirrors Git's maintenance
//! philosophy while remaining safe for production repositories.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use clap::{Parser, Subcommand, ValueEnum};
use git_internal::{
    hash::{HashKind, ObjectHash, get_hash_kind},
    internal::object::{commit::Commit, tag::Tag as GitTag, tree::Tree, types::ObjectType},
};
use sea_orm::EntityTrait;
use serde::Serialize;
use sha1::Digest;
// Brought into scope (anonymously) so `sha2::Sha256::digest` resolves; sha1 and
// sha2 use different `digest` trait versions here, so both must be in scope.
use sha2::Digest as _;

use crate::{
    command::{fetch::fetch_repository_safe, load_object_raw, log::get_reachable_commits},
    internal::{
        branch::Branch,
        config::ConfigKv,
        db,
        model::{reference, reflog},
        pack_writer,
    },
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        path,
        util::try_get_storage_path,
    },
};

const MAINTENANCE_ENABLED_KEY: &str = "maintenance.enabled";
const MAINTENANCE_SCHEDULE_KEY: &str = "maintenance.schedule";
const MAINTENANCE_LAST_RUN_KEY: &str = "maintenance.last-run";
const DEFAULT_LOOSE_OBJECT_THRESHOLD: usize = 100;
const DEFAULT_PACK_COUNT_THRESHOLD: usize = 5;
const LOOSE_OBJECT_AGE_SECONDS: u64 = 14 * 24 * 60 * 60; // 2 weeks

/// `--help` examples shown in `libra maintenance --help` output.
pub const MAINTENANCE_EXAMPLES: &str = "\
 EXAMPLES:
     libra maintenance run                         Run all maintenance tasks
     libra maintenance run --task gc               Run only the garbage-collection task
     libra maintenance run --task loose-objects    Pack old loose objects
     libra maintenance run --dry-run               Show what would be done, without changes
     libra maintenance register                    Register this repo for periodic maintenance
     libra maintenance unregister                  Unregister this repo
     libra maintenance status                      Show maintenance registration state";

/// Maintenance subcommands matching Git's `git maintenance` interface.
#[derive(Subcommand, Debug)]
pub enum MaintenanceSubcommand {
    /// Run one or more maintenance tasks.
    Run {
        /// Task to run (may be given multiple times). Defaults to all tasks.
        #[arg(long, value_enum)]
        task: Vec<MaintenanceTask>,
        /// Report what would be done without making any changes.
        #[arg(long)]
        dry_run: bool,
        /// Suppress progress output.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Register the current repository for periodic maintenance.
    Register {
        /// Cron-like schedule expression (stored for external scheduler use).
        #[arg(long, default_value = "hourly")]
        schedule: String,
    },
    /// Unregister the current repository from periodic maintenance.
    Unregister,
    /// Show whether this repository is registered for maintenance.
    Status,
    /// Register the repository AND install an OS scheduler entry (launchd agent
    /// on macOS, a cron fragment elsewhere) that runs `libra maintenance run`.
    Start {
        /// Schedule frequency: `hourly`, `daily`, or `weekly`.
        #[arg(long, default_value = "hourly")]
        schedule: String,
    },
    /// Unregister and remove the installed OS scheduler entry.
    Stop,
}

/// Top-level arguments for `libra maintenance`.
#[derive(Parser, Debug)]
#[command(after_help = MAINTENANCE_EXAMPLES)]
pub struct MaintenanceArgs {
    #[command(subcommand)]
    pub command: MaintenanceSubcommand,
}

/// Individual maintenance tasks that can be executed.
#[derive(Clone, Debug, PartialEq, Eq, ValueEnum, Serialize)]
pub enum MaintenanceTask {
    /// Garbage-collect unreachable loose objects.
    Gc,
    /// Pack old loose objects into a new pack file.
    LooseObjects,
    /// Collapse loose refs into packed-refs.
    PackRefs,
    /// Repack existing pack files incrementally.
    IncrementalRepack,
    /// Update commit-graph file for faster history walks.
    CommitGraph,
    /// Prefetch remote refs without updating local branches.
    Prefetch,
    /// Evict verified-durable large objects from the local cache (lore.md
    /// 2.9). NOT in the default task set — select it explicitly (or schedule
    /// it) so `maintenance run` never surprise-deletes cache entries.
    CacheEvict,
}

impl std::fmt::Display for MaintenanceTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MaintenanceTask::Gc => write!(f, "gc"),
            MaintenanceTask::LooseObjects => write!(f, "loose-objects"),
            MaintenanceTask::PackRefs => write!(f, "pack-refs"),
            MaintenanceTask::IncrementalRepack => write!(f, "incremental-repack"),
            MaintenanceTask::CommitGraph => write!(f, "commit-graph"),
            MaintenanceTask::Prefetch => write!(f, "prefetch"),
            MaintenanceTask::CacheEvict => write!(f, "cache-evict"),
        }
    }
}

/// Result of running a single maintenance task.
#[derive(Debug, Serialize)]
pub struct TaskResult {
    pub task: String,
    pub success: bool,
    pub objects_removed: usize,
    pub objects_packed: usize,
    pub refs_packed: usize,
    pub packs_repacked: usize,
    /// PD-04: `object_index` catalogue rows dropped (or, under `--dry-run`,
    /// that would be dropped) alongside pruned unreachable loose objects, so
    /// cloud sync never re-advertises a deleted blob.
    #[serde(default)]
    pub object_index_rows_removed: u64,
    pub message: String,
}

/// Overall result of a `maintenance run` invocation.
#[derive(Debug, Serialize)]
pub struct MaintenanceRunOutput {
    pub dry_run: bool,
    pub tasks: Vec<TaskResult>,
    pub overall_success: bool,
}

/// JSON output for `maintenance status`.
#[derive(Debug, Serialize)]
pub struct MaintenanceStatusOutput {
    pub registered: bool,
    pub schedule: Option<String>,
    pub last_run: Option<String>,
}

/// Safely execute a maintenance subcommand, returning structured errors.
pub async fn execute_safe(args: MaintenanceArgs, output: &OutputConfig) -> CliResult<()> {
    match args.command {
        MaintenanceSubcommand::Run {
            task,
            dry_run,
            quiet,
        } => run_tasks(&task, dry_run, quiet, output).await,
        MaintenanceSubcommand::Register { schedule } => register(&schedule, output).await,
        MaintenanceSubcommand::Unregister => unregister(output).await,
        MaintenanceSubcommand::Status => status(output).await,
        MaintenanceSubcommand::Start { schedule } => start(&schedule, output).await,
        MaintenanceSubcommand::Stop => stop(output).await,
    }
}

// ---------------------------------------------------------------------------
// Run tasks
// ---------------------------------------------------------------------------

async fn run_tasks(
    tasks: &[MaintenanceTask],
    dry_run: bool,
    quiet: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    let repo_path = try_get_storage_path(None)
        .map_err(|e| CliError::repo_not_found().with_hint(e.to_string()))?;

    let selected = if tasks.is_empty() {
        vec![
            MaintenanceTask::Gc,
            MaintenanceTask::LooseObjects,
            MaintenanceTask::PackRefs,
            MaintenanceTask::IncrementalRepack,
            MaintenanceTask::CommitGraph,
            MaintenanceTask::Prefetch,
        ]
    } else {
        tasks.to_vec()
    };

    let mut results = Vec::with_capacity(selected.len());
    let mut overall_success = true;
    let mut first_task_error = None;

    for task in selected {
        if !quiet {
            info_println(output, &format!("Running maintenance task: {task}"));
        }
        // §C.4.3 writer-vs-deleter: the tasks that PUBLISH object references
        // hold the maintenance lock shared for their duration, exactly like
        // any other publishing command. `maintenance` itself is carved out of
        // the command-level shared hold (`cli::command_holds_shared_maintenance_lock`)
        // precisely so that this can be decided per task: the deleting tasks
        // take the lock EXCLUSIVELY inside themselves, and a shared hold
        // cannot be upgraded. The two sets are disjoint by construction, and
        // this match is exhaustive so a new task must classify itself.
        let publish_lock = match task {
             // `prefetch` runs the ordinary fetch writer in-process: it writes
             // objects AND publishes remote-tracking refs.
             MaintenanceTask::Prefetch
             // `pack-refs` rewrites the ref store. It deletes loose REF
             // files, never object payloads, so a shared hold is the right
             // mode for it.
             | MaintenanceTask::PackRefs => {
                 Some(crate::internal::maintenance_lock::MaintenanceLock::shared(&repo_path)?)
             }
             // These take the lock themselves, in the mode each PHASE needs.
             // `loose-objects` and `incremental-repack` both publish a pack
             // and then UNLINK — shared for the write, exclusive for the
             // deletion; `gc` and `cache-evict` are deletion phases outright.
             // `commit-graph` derives a file from objects it neither
             // publishes nor deletes.
             MaintenanceTask::LooseObjects
             | MaintenanceTask::IncrementalRepack
             | MaintenanceTask::Gc
             | MaintenanceTask::CacheEvict
             | MaintenanceTask::CommitGraph => None,
         };
        let result = match task {
            MaintenanceTask::Gc => run_gc(&repo_path, dry_run, quiet, output).await,
            MaintenanceTask::LooseObjects => {
                run_loose_objects(&repo_path, dry_run, quiet, output).await
            }
            MaintenanceTask::PackRefs => run_pack_refs(&repo_path, dry_run, quiet, output).await,
            MaintenanceTask::IncrementalRepack => {
                run_incremental_repack(&repo_path, dry_run, quiet, output).await
            }
            MaintenanceTask::CommitGraph => {
                run_commit_graph(&repo_path, dry_run, quiet, output).await
            }
            MaintenanceTask::Prefetch => run_prefetch(&repo_path, dry_run, quiet, output).await,
            MaintenanceTask::CacheEvict => run_cache_evict(dry_run).await,
        };
        drop(publish_lock);
        match result {
            Ok(r) => {
                if !r.success {
                    overall_success = false;
                }
                results.push(r);
            }
            Err(e) => {
                overall_success = false;
                if first_task_error.is_none() {
                    first_task_error = Some(e.clone());
                }
                results.push(TaskResult {
                    task: task.to_string(),
                    success: false,
                    objects_removed: 0,
                    objects_packed: 0,
                    refs_packed: 0,
                    packs_repacked: 0,
                    object_index_rows_removed: 0,
                    message: e.to_string(),
                });
            }
        }
    }

    // Record last-run timestamp on success
    if !dry_run && overall_success {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default();
        let _ = ConfigKv::set(MAINTENANCE_LAST_RUN_KEY, &now, false).await;
    }

    if output.is_json() {
        let data = MaintenanceRunOutput {
            dry_run,
            tasks: results,
            overall_success,
        };
        return emit_json_data("maintenance.run", &data, output);
    }

    for r in &results {
        let status = if r.success { "ok" } else { "failed" };
        if !quiet {
            info_println(
                output,
                &format!("  {task}: {status} - {msg}", task = r.task, msg = r.message),
            );
        }
    }

    if !overall_success {
        if let Some(error) = first_task_error {
            return Err(error);
        }
        return Err(CliError::failure("one or more maintenance tasks failed").with_exit_code(1));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// GC task
// ---------------------------------------------------------------------------

/// `cache-evict` task (lore.md 2.9): delegate to the same engine as
/// `libra cache evict`, with the resolved budget and the default age floor.
async fn run_cache_evict(dry_run: bool) -> CliResult<TaskResult> {
    use crate::utils::storage::EvictRequest;
    // The SAME preflight `libra cache evict` runs. Reaching the engine
    // directly from here used to bypass the alternates-borrower gate and the
    // offline read-policy gate outright — scheduled maintenance could delete
    // objects a borrowing repository still needed.
    // Held for the whole eviction: the guard IS the exclusion.
    let _deletion_lock = crate::command::cache::evict_preflight(dry_run)?;
    let budget = crate::utils::client_storage::resolve_cache_config()
        .map_err(|error| CliError::fatal(format!("cannot resolve the cache budget: {error}")))?
        .cache_size_bytes as u64;
    let storage = crate::utils::client_storage::ClientStorage::init(crate::utils::path::objects());
    let report = storage
        .evict_local(EvictRequest {
            budget_bytes: budget,
            min_age_secs: 600,
            dry_run,
        })
        .await
        .map_err(|error| CliError::fatal(format!("cache eviction failed: {error}")))?;
    let (removed, message) = match report {
        None => (
            0,
            "no durable tier configured — nothing evictable".to_string(),
        ),
        Some(report) => (
            report.evicted,
            format!(
                "evicted {} object(s), {} bytes (skipped: {} absent, {} probe errors, {} recent)",
                report.evicted,
                report.reclaimed_bytes,
                report.skipped_absent,
                report.skipped_probe_error,
                report.skipped_recent
            ),
        ),
    };
    Ok(TaskResult {
        task: "cache-evict".to_string(),
        success: true,
        objects_removed: removed,
        objects_packed: 0,
        refs_packed: 0,
        packs_repacked: 0,
        object_index_rows_removed: 0,
        message,
    })
}

/// The GC prune-candidate ledger: `oid -> unix seconds when it was FIRST
/// observed unreachable`.
///
/// Derivable state (§C.4.3 `IndexOnly`-adjacent): an unreadable or corrupt
/// ledger is treated as empty, which costs one delayed prune cycle and can
/// never cost an object. It is deliberately NOT in SQLite — it must survive
/// being lost, and it holds no authority over what exists.
fn read_prune_candidate_ledger(
    path: &std::path::Path,
) -> CliResult<std::collections::HashMap<String, u64>> {
    // Bounded: an unbounded read of a file that grows with the repository's
    // dead objects is a memory hazard on exactly the repositories that need
    // pruning most. Over the cap is a hard refusal rather than a silent
    // reset, because "start over" would restart the two-scan clock and could
    // delay pruning forever.
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_PRUNE_LEDGER_BYTES => {
            return Err(CliError::fatal(format!(
                "the GC prune-candidate ledger '{}' is {} bytes, past the \
                  {MAX_PRUNE_LEDGER_BYTES}-byte cap",
                path.display(),
                meta.len()
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint(
                "delete it to restart the quarantine clock (this delays pruning by one grace \
                  window; it never deletes an object)",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(std::collections::HashMap::new());
        }
        Err(error) => {
            return Err(CliError::fatal(format!(
                "failed to stat the GC prune-candidate ledger '{}': {error}",
                path.display()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed));
        }
    }
    match std::fs::read_to_string(path) {
        // A corrupt ledger is treated as empty: it holds no authority over
        // what exists, so the worst case is one delayed prune cycle.
        Ok(text) => Ok(serde_json::from_str(&text).unwrap_or_default()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(std::collections::HashMap::new())
        }
        Err(error) => Err(CliError::fatal(format!(
            "failed to read the GC prune-candidate ledger '{}': {error}",
            path.display()
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)),
    }
}

/// 4 MiB holds ~90k entries — far past any healthy repository, and small
/// enough that reading it can never be the thing that fails a prune.
const MAX_PRUNE_LEDGER_BYTES: u64 = 4 * 1024 * 1024;

/// Serializes the ledger's read-modify-write against a concurrent GC.
///
/// Two overlapping runs would otherwise last-writer-win, and the loser's
/// `first_seen` timestamps would be replaced by the winner's fresher ones —
/// restarting the two-scan clock on every overlap, so a busy repository
/// could never finish a quarantine cycle. The lock is taken BEFORE any
/// SQLite work in `run_gc`, so it can never be waited on by a process
/// holding a database transaction.
fn acquire_prune_ledger_lock(path: &std::path::Path) -> CliResult<std::fs::File> {
    use std::fs::File;
    let lock_path = path.with_extension("lock");
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|error| {
            CliError::fatal(format!(
                "failed to open the GC prune-candidate ledger lock '{}': {error}",
                lock_path.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    file.lock().map_err(|error| {
        CliError::fatal(format!(
            "failed to lock the GC prune-candidate ledger '{}': {error}",
            lock_path.display()
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    Ok(file)
}

fn write_prune_candidate_ledger(
    path: &std::path::Path,
    ledger: &std::collections::HashMap<String, u64>,
) -> CliResult<()> {
    let text = serde_json::to_string(ledger).map_err(|error| {
        CliError::fatal(format!(
            "failed to serialize the GC prune-candidate ledger: {error}"
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    // The cap is enforced on the way OUT as well as the way in. Checking it
    // only on read lets a ledger that was legal when loaded grow past the cap
    // in THIS run and be written anyway — after which every later run refuses
    // to read it, and the quarantine clock stops for a file this code
    // created. Refusing here leaves the previous, still-readable ledger in
    // place (one delayed prune cycle, never an object).
    if text.len() as u64 > MAX_PRUNE_LEDGER_BYTES {
        return Err(CliError::fatal(format!(
            "the GC prune-candidate ledger would grow to {} bytes, past the \
              {MAX_PRUNE_LEDGER_BYTES}-byte cap; '{}' was left unchanged",
            text.len(),
            path.display()
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint(
            "this repository has more quarantined objects than the ledger can track: run \
              `libra gc` again after the current grace window expires so the backlog drains, \
              or delete the ledger to restart the quarantine clock",
        ));
    }
    crate::utils::atomic_write::write_atomic(path, text.as_bytes(), false).map_err(|error| {
        CliError::fatal(format!(
            "failed to write the GC prune-candidate ledger '{}': {error}",
            path.display()
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })
}

/// plan-20260714 Part C W0 (§C.11 release gate): does this repository have any
/// LINKED worktree besides the main one?
///
/// Object deletion is only safe when every worktree's reachability roots are
/// collected. Until the typed `GcObjectSource` inventory covers linked private
/// indexes / held sidecars / operation-view pointers, deletion paths fail
/// closed whenever this returns true. A registry read failure is treated as
/// "yes" (fail closed) rather than silently enabling a prune. Also consulted
/// by `rebase`'s legacy-sidecar adoption gate (§C.4.2), which must fail
/// closed on ambiguous ownership.
pub(crate) fn repository_has_linked_worktrees() -> bool {
    match crate::command::worktree::run_list_worktrees() {
        Ok(list) => list.worktrees.iter().any(|entry| !entry.is_main),
        Err(_) => true,
    }
}

/// Whether this repository has EVER registered a linked worktree (§C.4.3
/// linked-history rule).
///
/// Currently-registered entries are not enough for the ambiguous-sidecar
/// decision: a linked worktree that has been fully removed leaves no entry, so
/// a common-storage sidecar whose owner might have been that worktree would
/// look unambiguously main's. The registry records the history durably
/// (`linked_history`), and a promoted pre-v3 registry records it as UNKNOWN
/// rather than "never" — because a pre-v3 removal left nothing to read. The
/// generation counter is a second witness.
///
/// This is the SINGLE helper behind every legacy read/adopt/delete decision
/// (rebase state, rerere `MERGE_RR`), so the answer cannot differ between the
/// path that adopts and the path that deletes.
///
/// Fail-closed: an unreadable registry answers "yes".
pub(crate) fn repository_had_linked_worktrees() -> bool {
    if repository_has_linked_worktrees() {
        return true;
    }
    let registry = crate::utils::util::storage_path().join("worktrees.json");
    let raw = match std::fs::read_to_string(&registry) {
        // No registry: a single-worktree repository that never had one.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Ok(raw) => raw,
        // Exists but unreadable — an unknown answer, which is evidence.
        Err(_) => return true,
    };
    // Through the VALIDATED parser, not ad-hoc JSON: it is the one place that
    // knows a pre-v3 registry's history is `Unknown` rather than `Never`, and
    // that a v1 shape predates every generation.
    match crate::command::worktree::WorktreeState::parse(raw.as_bytes()) {
        Ok(state) => state.ever_had_linked_worktree(),
        // Unparseable: evidence.
        Err(_) => true,
    }
}

async fn run_gc(
    repo_path: &Path,
    dry_run: bool,
    quiet: bool,
    output: &OutputConfig,
) -> CliResult<TaskResult> {
    let storage = ClientStorage::init(path::objects());
    // lore.md 2.3 deletion safety: if another repo borrows FROM this store, a
    // prune could delete an object it still needs (this store's reachability
    // does not include the borrower's refs). Refuse to prune loose objects
    // while any live borrower exists — the borrower must `alternates remove`
    // (or dissociate) first. This makes the base's gc "never delete a
    // borrowed object" AIRTIGHT.
    // The predicate comes from the ONE deletion gate (§C.11); scheduled
    // maintenance reports it as a skipped task instead of failing the whole
    // run, but it can never disagree with the interactive commands about
    // WHEN deletion is unsafe.
    if !dry_run
        && let Err(refusal) = crate::internal::alternates::ensure_no_live_borrowers(
            "prune loose objects",
            StableErrorCode::ConflictOperationBlocked,
        )
    {
        // A live borrower is a known state this task may report as skipped.
        // An UNREADABLE registration is a fault, and folding it into a
        // successful run would hide the one condition that makes deletion
        // unsafe for reasons nobody can see.
        if refusal.stable_code() != StableErrorCode::ConflictOperationBlocked {
            return Err(refusal);
        }
        return Ok(TaskResult {
            task: "gc".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: 0,
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!("skipped loose-object prune: {refusal}"),
        });
    }
    // W2 §C.4.3: the typed `GcObjectSource` inventory is complete — the
    // reachability walk enumerates EVERY worktree's private index (all
    // stages), every scope's sequencer/rebase/bisect rows, every gitdir's
    // held-autostash + merge/revert/rebase-aux sidecars, the
    // shared refs/reflogs/stash reflog, note blobs, undo view snapshots, and
    // AI capture checkpoints. The former W0 multi-worktree prune skip is
    // therefore LIFTED; any unreadable root still fails the walk closed.
    // W2 §C.4.3 traces-inflight contract: a LIVE ordinary marker means an
    // agent writer may hold objects it has NOT cataloged (or even listed)
    // yet — no enumerable root exists for them, so destructive pruning is
    // DEFERRED until the marker completes or its (clamped) TTL expires. A
    // `cleanup_pending` marker does NOT defer: it fully lists its owned
    // OIDs, which the roots walk keeps alive; doctor retires the row.
    if !dry_run {
        let db_conn = db::get_db_conn_instance().await;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let live =
            crate::internal::ai::history::list_live_traces_inflight_markers(&db_conn, now_ms)
                .await
                .map_err(|err| {
                    CliError::fatal(format!(
                        "traces-inflight markers cannot be trusted before pruning: {err:#}"
                    ))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
                })?;
        // Only LIVE ordinary markers defer: their write window may hold
        // objects not yet listed anywhere (window A), and their TTL bounds
        // the deferral — an abandoned writer's marker expires and prune
        // resumes on the next run. A `cleanup_pending` marker fully LISTS
        // its owned OIDs (`created_oids`) — those are already rooted by the
        // reachability walk, so it must NOT park prune forever (it never
        // expires by design; `libra agent doctor` retires it).
        // Liveness is centrally BOUNDED in `TracesInflightMarker::is_live`
        // (clamped start + 24h TTL cap), which the listing above already
        // applied — only the cleanup_pending exclusion is local here (those
        // rows list their owned OIDs exhaustively; rooting suffices).
        let live_ordinary = live.iter().filter(|m| !m.cleanup_pending).count();
        if live_ordinary > 0 {
            return Ok(TaskResult {
                task: "gc".to_string(),
                success: true,
                objects_removed: 0,
                objects_packed: 0,
                refs_packed: 0,
                packs_repacked: 0,
                object_index_rows_removed: 0,
                message: format!(
                    "deferred loose-object prune: {live_ordinary} live traces-inflight \
                      marker(s) — an agent write is in flight and may hold uncataloged \
                      objects; the marker TTL bounds this, re-run after it completes or \
                      expires"
                ),
            });
        }
    }

    // Race hardening (W2 §C.4.3): capture the LOOSE CANDIDATE LIST BEFORE
    // computing roots — an object a concurrent writer creates during the
    // walk is then not a deletion candidate at all — and additionally skip
    // any candidate younger than the grace window below (belt for an object
    // written just before the listing whose index/ref record lands moments
    // later).
    let all_loose = list_loose_objects(repo_path)
        .map_err(|e| CliError::fatal(format!("failed to list loose objects: {e}")))?;
    let reachable = collect_reachable_objects(&storage).await?;
    // One hour dwarfs any write→record gap while keeping prune useful.
    const PRUNE_GRACE_SECS: u64 = 3600;
    let now = std::time::SystemTime::now();

    // Select the candidates FIRST, then invalidate the catalogue, then
    // unlink (§C.4.3 item 12). The order matters: `object_index` is what
    // cloud sync and repair consult to decide an object is available here.
    //
    // Unlinking first — as this did — means a SQLite failure or lock in
    // between leaves rows advertising bytes that no longer exist, and
    // nothing later reconciles that: a peer asks for the object and gets a
    // hard miss. Dropping the rows first can only leave the opposite
    // asymmetry, an object present but uncatalogued, which under-advertises
    // (never a broken promise) and which `agent doctor` rebuilds from the
    // manifests.
    // Candidates for THIS pass: unreachable now, and old enough that they
    // cannot be an object written moments ago whose ref record has not landed
    // yet. A stat error keeps the object — never delete what cannot be aged.
    let mut unreachable_now: Vec<(&String, &std::path::PathBuf)> = Vec::new();
    for (hash_str, obj_path) in &all_loose {
        if let Some(hash) = parse_object_hash(hash_str)
            && !reachable.contains(&hash)
        {
            let age_ok = std::fs::metadata(obj_path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|mtime| now.duration_since(mtime).ok())
                .is_some_and(|age| age.as_secs() >= PRUNE_GRACE_SECS);
            if age_ok {
                unreachable_now.push((hash_str, obj_path));
            }
        }
    }

    // §C.4.3 writer-vs-deleter defence — the QUARANTINE half.
    //
    // An mtime grace alone does not protect an OLD orphan that becomes
    // reachable while this run is between its root scan and its unlink: a
    // concurrent `update-ref`, `reset`, `stash apply` or `op restore` can
    // republish a long-dead object, and the scan that decided it was
    // unreachable already happened.
    //
    // So nothing is deleted the first time it is seen unreachable. A
    // candidate is recorded, and only deleted by a LATER run that still finds
    // it unreachable after the grace window has passed — meaning it survived
    // two independent root scans, separated in time, with no reference
    // appearing in between. Anything that became reachable is dropped from
    // the ledger and never deleted.
    //
    // The ledger is derivable state: losing it costs one delayed prune cycle,
    // never an object.
    let ledger_path = crate::utils::util::storage_path().join("gc-prune-candidates.json");
    // Held across the read-modify-write below so two concurrent GCs cannot
    // last-writer-win and reset each other's quarantine clock. A dry run
    // only reads, so it takes no lock.
    let _ledger_lock = if dry_run {
        None
    } else {
        Some(acquire_prune_ledger_lock(&ledger_path)?)
    };
    let mut ledger = read_prune_candidate_ledger(&ledger_path)?;
    let now_secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let unreachable_keys: std::collections::HashSet<&str> = unreachable_now
        .iter()
        .map(|(hash_str, _)| hash_str.as_str())
        .collect();
    // Anything no longer unreachable (or no longer present) leaves the ledger.
    ledger.retain(|oid, _| unreachable_keys.contains(oid.as_str()));

    #[allow(unused_assignments)]
    let mut pruned_oids: Vec<String> = Vec::new();
    let mut prune_targets: Vec<(&String, &std::path::PathBuf)> = Vec::new();
    let mut newly_quarantined = 0usize;
    for (hash_str, obj_path) in unreachable_now {
        match ledger.get(hash_str.as_str()) {
            Some(first_seen) if now_secs.saturating_sub(*first_seen) >= PRUNE_GRACE_SECS => {
                if dry_run && !quiet {
                    info_println(
                        output,
                        &format!("  would remove unreachable object {hash_str}"),
                    );
                }
                pruned_oids.push(hash_str.clone());
                prune_targets.push((hash_str, obj_path));
            }
            Some(_) => {}
            None => {
                if !dry_run {
                    ledger.insert(hash_str.clone(), now_secs);
                }
                newly_quarantined += 1;
            }
        }
    }
    if !dry_run {
        // The candidates about to be deleted STAY in the ledger until they
        // actually are. Dropping them here was safe only while deletion
        // always followed; now that the deletion phase can be deferred (a
        // publisher holds the maintenance lock) or refuse individual
        // candidates, dropping them first would reset each deferred
        // candidate's `first_seen` to the next run's clock — and a
        // repository with a long-running session would restart its quarantine
        // on every attempt and never prune anything.
        write_prune_candidate_ledger(&ledger_path, &ledger)?;
    }
    if newly_quarantined > 0 && !quiet {
        info_println(
            output,
            &format!(
                "  {newly_quarantined} newly unreachable object(s) recorded; a later run \
                  deletes them if they are still unreachable"
            ),
        );
    }

    // PD-04: reclaim the `object_index` catalogue rows of the pruned blobs
    // in the SAME pass (idempotent — OIDs without a row delete nothing), so
    // cloud sync stops advertising deleted objects. Under `--dry-run` the
    // matching rows are only counted. Content-addressed agent findings
    // blobs reach this point exclusively through the reachability walk: a
    // blob anchored by any ref, index, sidecar, or live agent-run manifest
    // never becomes a prune candidate, so shared bytes stay alive.
    let mut removed = 0;
    let object_index_rows_removed = if pruned_oids.is_empty() {
        0
    } else if dry_run {
        let db_conn = db::get_db_conn_instance().await;
        crate::utils::client_storage::count_object_index_rows_with_conn(&db_conn, &pruned_oids)
            .await
            .map_err(|e| {
                CliError::fatal(format!(
                    "failed to count object_index rows for pruned objects: {e}"
                ))
            })?
    } else {
        // §C.4.3 writer-vs-deleter: the deletion phase runs under the
        // repository maintenance lock, held EXCLUSIVELY.
        //
        // Two historical scans prove an object was unreachable twice; they do
        // not prove nothing referenced it in between — and a database
        // transaction cannot prove it either, because the publications that
        // matter most here are FILES: a worktree's private index, a merge or
        // rebase sidecar, an agent-run manifest. Staging content that happens
        // to hash to a quarantined object commits without touching SQLite at
        // all, so a transaction's SHARED read lock excludes exactly the
        // writers that were never the problem.
        //
        // Every publisher instead holds this lock shared for its whole run
        // (`cli::command_holds_shared_maintenance_lock`). Holding it
        // exclusively across "final scan → catalogue invalidation → unlink"
        // is therefore the interval-emptiness proof the ledger cannot give,
        // and it covers filesystem and database publishers alike.
        let Some(_deletion_lock) =
            crate::internal::maintenance_lock::MaintenanceLock::try_exclusive(
                &crate::utils::util::storage_path(),
                crate::internal::maintenance_lock::DELETION_LOCK_WAIT,
            )?
        else {
            // Deferral, not failure: the objects stay, and the next run takes
            // them. Deleting them without the exclusion is the one option
            // that could cost data.
            return Ok(TaskResult {
                task: "gc".to_string(),
                success: true,
                objects_removed: 0,
                objects_packed: 0,
                refs_packed: 0,
                packs_repacked: 0,
                object_index_rows_removed: 0,
                message: format!(
                    "deferred the deletion of {} unreachable loose object(s): another command \
                      is still publishing objects in this repository (a long-running `libra \
                      code` session counts). Re-run when it finishes.",
                    prune_targets.len()
                ),
            });
        };

        if let Err(refusal) = crate::internal::alternates::ensure_no_live_borrowers(
            "prune loose objects",
            StableErrorCode::ConflictOperationBlocked,
        ) {
            // A live borrower is a known state this task may report as
            // skipped. An UNREADABLE registration is a fault, and folding
            // it into a successful run would hide the one condition that
            // makes deletion unsafe for reasons nobody can see.
            if refusal.stable_code() != StableErrorCode::ConflictOperationBlocked {
                return Err(refusal);
            }
            // Re-checked UNDER the exclusive hold: the gate at the top of
            // this function ran before the lock existed, and registering a
            // borrower is itself a publication (it takes the base's shared
            // hold now), so one that appeared in between is caught here
            // rather than losing objects it already depends on (§C.4.3).
            return Ok(TaskResult {
                task: "gc".to_string(),
                success: true,
                objects_removed: 0,
                objects_packed: 0,
                refs_packed: 0,
                packs_repacked: 0,
                object_index_rows_removed: 0,
                message: format!("skipped loose-object prune: {refusal}"),
            });
        }

        let db_conn = db::get_db_conn_instance().await;
        // The last word on reachability. No publisher can be running, so this
        // needs no transaction of its own — and must not have one: the walk
        // loads every commit and tree in the repository, and holding a
        // database read lock across it would block ref writers for the whole
        // traversal (§C.10 keeps database transactions short).
        let still_reachable = collect_reachable_objects_with_conn(&storage, &db_conn).await?;
        let mut final_targets: Vec<(&String, &std::path::PathBuf)> = Vec::new();
        let mut final_oids: Vec<String> = Vec::new();
        let mut resurrected_oids: Vec<String> = Vec::new();
        for (hash_str, obj_path) in &prune_targets {
            match parse_object_hash(hash_str) {
                Some(hash) if !still_reachable.contains(&hash) => {
                    final_targets.push((hash_str, obj_path));
                    final_oids.push((*hash_str).clone());
                }
                // Referenced since the earlier scan: drop it from this run,
                // and from the LEDGER — keeping its old `first_seen` would
                // mean that if the new reference goes away again, the very
                // next run deletes it on the strength of a quarantine
                // interval that a reference appeared inside. The window has
                // to be continuous to prove anything, so it starts over.
                Some(_) => resurrected_oids.push((*hash_str).clone()),
                None => {
                    return Err(CliError::fatal(format!(
                        "prune candidate '{hash_str}' is not a valid object id"
                    ))
                    .with_stable_code(StableErrorCode::RepoCorrupt));
                }
            }
        }
        if !resurrected_oids.is_empty() && !quiet {
            info_println(
                output,
                &format!(
                    "  {} candidate(s) became reachable since the earlier scan and were kept",
                    resurrected_oids.len()
                ),
            );
        }
        for oid in &resurrected_oids {
            ledger.remove(oid.as_str());
        }

        // Catalogue FIRST, and committed before the first unlink — the two
        // steps are ordered, not wrapped. A catalogue that has already
        // dropped rows for objects still on disk merely UNDER-advertises,
        // which `agent doctor` rebuilds; the inverse — unlinking inside a
        // transaction that then rolls back — would restore rows advertising
        // bytes that are gone, and nothing repairs that from the inside.
        let rows = if final_oids.is_empty() {
            0
        } else {
            crate::utils::client_storage::remove_object_index_rows_with_conn(&db_conn, &final_oids)
                .await
                .map_err(|e| {
                    CliError::fatal(format!(
                        "failed to remove object_index rows for pruned objects: {e}"
                    ))
                })?
        };

        // The catalogue no longer claims these objects, so unlinking them can
        // only make the store MORE honest, never less.
        for (hash_str, obj_path) in &final_targets {
            if let Err(e) = fs::remove_file(obj_path) {
                // A concurrent cache eviction may have removed it first —
                // the goal state (file gone) is reached either way.
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(CliError::fatal(format!(
                        "failed to remove unreachable object {hash_str}: {e}"
                    )));
                }
            }
            removed += 1;
        }
        let _ = cleanup_empty_dirs(&path::objects());
        // Now — and only now — the deleted candidates leave the ledger. The
        // ones this run kept (resurrected, or deferred by an earlier return)
        // keep their original `first_seen`, so the quarantine clock they
        // already served is not thrown away.
        for oid in &final_oids {
            ledger.remove(oid.as_str());
        }
        write_prune_candidate_ledger(&ledger_path, &ledger)?;
        pruned_oids = final_oids;
        rows
    };

    let message = if dry_run {
        format!(
            "would remove {} unreachable loose objects and {} object-index rows",
            pruned_oids.len(),
            object_index_rows_removed
        )
    } else {
        format!(
            "removed {removed} unreachable loose objects and {object_index_rows_removed} object-index rows"
        )
    };

    Ok(TaskResult {
        task: "gc".to_string(),
        success: true,
        objects_removed: removed,
        objects_packed: 0,
        refs_packed: 0,
        packs_repacked: 0,
        object_index_rows_removed,
        message,
    })
}

// ---------------------------------------------------------------------------
// Loose-objects task
// ---------------------------------------------------------------------------

async fn run_loose_objects(
    repo_path: &Path,
    dry_run: bool,
    quiet: bool,
    output: &OutputConfig,
) -> CliResult<TaskResult> {
    // §C.4.3: this task PUBLISHES (a pack that becomes the objects' home)
    // and then DELETES (the loose copies), so it holds the shared lock for
    // the first half and swaps to the exclusive one for the second — the
    // same shape as `repack -d`.
    let publish_lock = crate::internal::maintenance_lock::MaintenanceLock::shared(
        &crate::utils::util::storage_path(),
    )?;
    let loose = list_loose_objects(repo_path)
        .map_err(|e| CliError::fatal(format!("failed to list loose objects: {e}")))?;

    if loose.len() < DEFAULT_LOOSE_OBJECT_THRESHOLD {
        return Ok(TaskResult {
            task: "loose-objects".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: 0,
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!(
                "only {} loose objects (threshold: {}), skipping",
                loose.len(),
                DEFAULT_LOOSE_OBJECT_THRESHOLD
            ),
        });
    }

    // Under a configured durable tier, large (>= threshold) loose objects are
    // CACHE residents managed by the 2.9 evictor — packing them would move
    // them into local packs where the evictor never reaches, permanently
    // defeating the cache budget. Exclude them from packing.
    let cache_config = crate::utils::client_storage::resolve_cache_config().ok();
    let large_cache_floor = cache_config
        .as_ref()
        .filter(|config| config.tiered)
        .map(|config| config.threshold_bytes as u64);
    let old_loose: Vec<_> = loose
        .into_iter()
        .filter(|(_, p)| {
            fs::metadata(p)
                .and_then(|m| m.modified())
                .map(|t| {
                    SystemTime::now()
                        .duration_since(t)
                        .map(|d| d.as_secs() > LOOSE_OBJECT_AGE_SECONDS)
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        })
        .filter(|(_, p)| match large_cache_floor {
            // Classify by UNCOMPRESSED size (partial header decode) — the
            // same signal the evictor and the LRU use; compressed on-disk
            // size would let highly-compressible large residents slip into
            // packs (Codex improvement note).
            Some(floor) => crate::utils::storage::local::LocalStorage::peek_uncompressed_size(p)
                .map(|size| size < floor)
                .unwrap_or(true),
            None => true,
        })
        .collect();

    if old_loose.is_empty() {
        return Ok(TaskResult {
            task: "loose-objects".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: 0,
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: "no old loose objects to pack".to_string(),
        });
    }

    if dry_run {
        return Ok(TaskResult {
            task: "loose-objects".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: old_loose.len(),
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!("would pack {} old loose objects", old_loose.len()),
        });
    }

    // Encode the old loose objects into one valid pack via the shared writer.
    let pack_dir = repo_path.join("objects").join("pack");
    let storage = ClientStorage::init(path::objects());
    let hashes: Vec<ObjectHash> = old_loose
        .iter()
        .filter_map(|(hash_str, _)| parse_object_hash(hash_str))
        .collect();

    let publication =
        match pack_writer::write_pack_with_index(&storage, &hashes, &pack_dir, get_hash_kind())
            .await
        {
            Ok(Some(publication)) => publication,
            Ok(None) => {
                return Ok(TaskResult {
                    task: "loose-objects".to_string(),
                    success: true,
                    objects_removed: 0,
                    objects_packed: 0,
                    refs_packed: 0,
                    packs_repacked: 0,
                    object_index_rows_removed: 0,
                    message: "no old loose objects to pack".to_string(),
                });
            }
            Err(e) => {
                return Err(CliError::fatal(format!("failed to create pack file: {e}")));
            }
        };

    // §C.4.3 writer-vs-deleter: the pack is published, so the shared hold
    // ends and the UNLINKS take the exclusive one. A shared hold cannot be
    // upgraded in place (another process may hold it too), so this is a
    // release-then-acquire; the objects being removed live in the pack this
    // function just wrote, so nothing is lost if the deletion is deferred.
    // The pack is about to become these objects' only home, so its NAME must
    // be durable, not just its bytes: a crash that loses the directory entry
    // after the loose copies are gone loses the objects. The pack itself is
    // kept — it is valid, and the next run will delete the loose copies once
    // durability can be proven.
    if !publication.durable {
        return Ok(TaskResult {
            task: "loose-objects".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: old_loose.len(),
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!(
                "packed {} loose object(s); kept the loose copies because the new pack's \
                  directory entry could not be made durable",
                old_loose.len()
            ),
        });
    }
    drop(publish_lock);
    let Some(_deletion_lock) = crate::internal::maintenance_lock::MaintenanceLock::try_exclusive(
        &crate::utils::util::storage_path(),
        crate::internal::maintenance_lock::DELETION_LOCK_WAIT,
    )?
    else {
        return Ok(TaskResult {
            task: "loose-objects".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: old_loose.len(),
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!(
                "packed {} loose object(s), then deferred removing the loose copies: another \
                  command is still publishing objects in this repository. The objects are safe \
                  in the new pack — re-run when it finishes.",
                old_loose.len()
            ),
        });
    };

    // lore.md 2.3 / W0 deletion hard gate, evaluated UNDER the exclusive
    // hold: this is a DELETION entry point, so it passes the same borrower
    // gate as `gc` and `repack -d`. Packing the objects first does not make
    // the unlink safe for a BORROWER — a borrowing repository resolves loose
    // objects through the alternates path, and its reachability is not part
    // of this store's walk.
    if let Err(refusal) = crate::internal::alternates::ensure_no_live_borrowers(
        "remove loose objects after packing them",
        StableErrorCode::ConflictOperationBlocked,
    ) {
        // A live borrower is a known state this task may report as
        // skipped. An UNREADABLE registration is a fault, and folding
        // it into a successful run would hide the one condition that
        // makes deletion unsafe for reasons nobody can see.
        if refusal.stable_code() != StableErrorCode::ConflictOperationBlocked {
            return Err(refusal);
        }
        return Ok(TaskResult {
            task: "loose-objects".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: old_loose.len(),
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!(
                "packed {} loose object(s); skipped removing the loose copies: {refusal}",
                old_loose.len()
            ),
        });
    }

    // Remove the loose objects now that they live in the pack.
    for (hash_str, obj_path) in &old_loose {
        if let Err(e) = fs::remove_file(obj_path) {
            // Tolerate a concurrent eviction (the object is already packed).
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(CliError::fatal(format!(
                    "failed to remove packed loose object {}: {e}",
                    hash_str
                )));
            }
        }
    }
    let _ = cleanup_empty_dirs(&path::objects());
    let packed = hashes.len();
    let pack_name = publication
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "the new pack".to_string());

    if !quiet {
        info_println(
            output,
            &format!("  created pack file with {packed} objects"),
        );
    }

    Ok(TaskResult {
        task: "loose-objects".to_string(),
        success: true,
        objects_removed: 0,
        objects_packed: packed,
        refs_packed: 0,
        packs_repacked: 0,
        object_index_rows_removed: 0,
        message: format!("packed {packed} old loose objects into {pack_name}"),
    })
}

// ---------------------------------------------------------------------------
// Pack-refs task
// ---------------------------------------------------------------------------

async fn run_pack_refs(
    repo_path: &Path,
    dry_run: bool,
    _quiet: bool,
    _output: &OutputConfig,
) -> CliResult<TaskResult> {
    let refs_dir = repo_path.join("refs").join("heads");
    if !refs_dir.exists() {
        return Ok(TaskResult {
            task: "pack-refs".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: 0,
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: "no refs/heads directory".to_string(),
        });
    }

    let mut refs: HashMap<String, String> = HashMap::new();
    collect_refs(&refs_dir, &refs_dir, &mut refs)
        .map_err(|e| CliError::fatal(format!("failed to collect refs: {e}")))?;

    if refs.is_empty() {
        return Ok(TaskResult {
            task: "pack-refs".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: 0,
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: "no loose refs to pack".to_string(),
        });
    }

    let packed_refs_path = repo_path.join("packed-refs");

    if dry_run {
        return Ok(TaskResult {
            task: "pack-refs".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: 0,
            refs_packed: refs.len(),
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!("would pack {} refs into packed-refs", refs.len()),
        });
    }

    // Append to existing packed-refs if present
    let mut existing: HashMap<String, String> = HashMap::new();
    if packed_refs_path.exists() {
        let content = fs::read_to_string(&packed_refs_path)
            .map_err(|e| CliError::fatal(format!("failed to read packed-refs: {e}")))?;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((hash, name)) = line.split_once(' ') {
                existing.insert(name.to_string(), hash.to_string());
            }
        }
    }

    // Merge new refs, overwriting existing ones
    for (name, hash) in refs {
        existing.insert(name, hash);
    }

    // Write packed-refs
    let mut file = fs::File::create(&packed_refs_path)
        .map_err(|e| CliError::fatal(format!("failed to create packed-refs: {e}")))?;
    if let Err(e) = writeln!(file, "# packed-refs with peeled tags") {
        return Err(CliError::fatal(format!("failed to write packed-refs: {e}")));
    }
    for (name, hash) in &existing {
        if let Err(e) = writeln!(file, "{hash} {name}") {
            return Err(CliError::fatal(format!("failed to write packed-refs: {e}")));
        }
    }

    // Remove packed loose ref files
    let mut removed_count = 0;
    remove_packed_refs(&refs_dir, &refs_dir, &mut removed_count)
        .map_err(|e| CliError::fatal(format!("failed to remove packed refs: {e}")))?;

    Ok(TaskResult {
        task: "pack-refs".to_string(),
        success: true,
        objects_removed: 0,
        objects_packed: 0,
        refs_packed: removed_count,
        packs_repacked: 0,
        object_index_rows_removed: 0,
        message: format!("packed {removed_count} refs"),
    })
}

// ---------------------------------------------------------------------------
// Incremental-repack task
// ---------------------------------------------------------------------------

async fn run_incremental_repack(
    repo_path: &Path,
    dry_run: bool,
    quiet: bool,
    output: &OutputConfig,
) -> CliResult<TaskResult> {
    // Alternates safety (mirrors the gc prune guard): a borrower may depend
    // on objects that live only in this store's OLD packs yet are not in
    // THIS repository's root set — consolidating and deleting those packs
    // would corrupt the borrower. Refuse while live borrowers exist.
    if !dry_run
        && let Err(refusal) = crate::internal::alternates::ensure_no_live_borrowers(
            "consolidate and delete old packs",
            StableErrorCode::ConflictOperationBlocked,
        )
    {
        // A live borrower is a known state this task may report as skipped.
        // An UNREADABLE registration is a fault, and folding it into a
        // successful run would hide the one condition that makes deletion
        // unsafe for reasons nobody can see.
        if refusal.stable_code() != StableErrorCode::ConflictOperationBlocked {
            return Err(refusal);
        }
        return Ok(TaskResult {
            task: "incremental-repack".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: 0,
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!("skipped repack: {refusal}"),
        });
    }
    let pack_dir = repo_path.join("objects").join("pack");
    if !pack_dir.exists() {
        return Ok(TaskResult {
            task: "incremental-repack".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: 0,
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: "no pack directory".to_string(),
        });
    }
    // W2 §C.4.3: the typed `GcObjectSource` inventory made every worktree's
    // private roots part of `collect_reachable_objects` (see the gc prune
    // note), so the former W0 multi-worktree repack skip is LIFTED.

    let packs: Vec<_> = match fs::read_dir(&pack_dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "pack"))
            .map(|e| e.path())
            .collect(),
        Err(e) => {
            return Err(CliError::fatal(format!(
                "failed to read pack directory: {e}"
            )));
        }
    };

    if packs.len() < DEFAULT_PACK_COUNT_THRESHOLD {
        return Ok(TaskResult {
            task: "incremental-repack".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: 0,
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!(
                "only {} pack files (threshold: {}), skipping",
                packs.len(),
                DEFAULT_PACK_COUNT_THRESHOLD
            ),
        });
    }

    if dry_run {
        return Ok(TaskResult {
            task: "incremental-repack".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: 0,
            refs_packed: 0,
            packs_repacked: packs.len(),
            object_index_rows_removed: 0,
            message: format!("would repack {} pack files", packs.len()),
        });
    }

    // Consolidate into a single new pack. The set MUST include objects that
    // currently live only inside the existing packs — `list_all_objects_in_storage`
    // scans only loose shards, so packing that alone and then deleting the old
    // packs would drop every packed-only object. `collect_reachable_objects`
    // walks refs/reflogs/index through storage (which reads the packs too), so
    // the new pack contains all reachable objects before the old packs go.
    // §C.4.3: writing the consolidated pack PUBLISHES — the pack becomes the
    // only home of objects it holds — so this half runs under the shared
    // hold, like any other publisher. It is released before the deletion
    // phase takes the exclusive one (a shared hold cannot be upgraded).
    let publish_lock = crate::internal::maintenance_lock::MaintenanceLock::shared(
        &crate::utils::util::storage_path(),
    )?;
    let storage = ClientStorage::init(path::objects());
    let all_hashes: Vec<ObjectHash> = collect_reachable_objects(&storage)
        .await?
        .into_iter()
        .collect();

    let new_publication =
        match pack_writer::write_pack_with_index(&storage, &all_hashes, &pack_dir, get_hash_kind())
            .await
        {
            Ok(Some(publication)) => publication,
            Ok(None) => {
                return Ok(TaskResult {
                    task: "incremental-repack".to_string(),
                    success: true,
                    objects_removed: 0,
                    objects_packed: 0,
                    refs_packed: 0,
                    packs_repacked: 0,
                    object_index_rows_removed: 0,
                    message: "no objects to repack".to_string(),
                });
            }
            Err(e) => {
                return Err(CliError::fatal(format!(
                    "failed to create consolidated pack: {e}"
                )));
            }
        };

    // Pre-delete RE-VERIFICATION (W2 §C.4.3 race hardening): the pack list
    // was captured BEFORE the root walk (a pack arriving later is never
    // deleted), but a ref/row created DURING the walk could root an object
    // that lives only in an old pack. Re-collect the roots now — every
    // second-pass root must already be inside the consolidated set, else a
    // concurrent writer moved underneath us and deletion aborts (the new
    // pack is redundant-but-harmless; re-run when quiescent).
    //
    // §C.4.3 writer-vs-deleter: this second pass and the pack deletion below
    // run under the repository maintenance lock, held EXCLUSIVELY. Verifying
    // and then deleting without it leaves a window in which a writer can
    // repoint a ref — or stage an index entry, which never reaches SQLite at
    // all — at an object that lives only in an aged old pack: absent from
    // the consolidated pack, and gone once that pack is unlinked. Every
    // publisher holds the same lock shared for its whole run, so within this
    // section no publication can be in flight.
    // The consolidated pack is about to become the only home of everything
    // the old packs held, so its NAME must be durable before they are
    // unlinked. Keeping them costs disk, not data.
    if !new_publication.durable {
        return Ok(TaskResult {
            task: "incremental-repack".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: all_hashes.len(),
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!(
                "consolidated {} object(s) into a new pack, then kept the {} old pack(s): the \
                  new pack's directory entry could not be made durable",
                all_hashes.len(),
                packs.len()
            ),
        });
    }
    drop(publish_lock);
    let Some(_repack_deletion_lock) =
        crate::internal::maintenance_lock::MaintenanceLock::try_exclusive(
            &crate::utils::util::storage_path(),
            crate::internal::maintenance_lock::DELETION_LOCK_WAIT,
        )?
    else {
        // The pack WAS written, so the counts must say so: reporting
        // `objects_packed: 0` here would make the structured output false
        // about disk state. Only the old-pack deletion is deferred.
        return Ok(TaskResult {
            task: "incremental-repack".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: all_hashes.len(),
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!(
                "consolidated {} object(s) into a new pack, then deferred deleting the {} old \
                  pack(s): another command is still publishing objects in this repository. The \
                  consolidated pack was kept (harmless duplicate data) — re-run when it \
                  finishes.",
                all_hashes.len(),
                packs.len()
            ),
        });
    };
    if let Err(refusal) = crate::internal::alternates::ensure_no_live_borrowers(
        "delete old pack files",
        StableErrorCode::ConflictOperationBlocked,
    ) {
        // A live borrower is a known state this task may report as
        // skipped. An UNREADABLE registration is a fault, and folding
        // it into a successful run would hide the one condition that
        // makes deletion unsafe for reasons nobody can see.
        if refusal.stable_code() != StableErrorCode::ConflictOperationBlocked {
            return Err(refusal);
        }
        // Re-checked under the exclusive hold, for the same reason `gc` does.
        return Ok(TaskResult {
            task: "incremental-repack".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: all_hashes.len(),
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!(
                "consolidated {} object(s) into a new pack; skipped deleting the old packs: \
                  {refusal}",
                all_hashes.len()
            ),
        });
    }
    let repack_db = db::get_db_conn_instance().await;
    let first_pass: HashSet<ObjectHash> = all_hashes.iter().copied().collect();
    let second_pass = collect_reachable_objects_with_conn(&storage, &repack_db).await?;
    if !second_pass.is_subset(&first_pass) {
        // Same honesty rule as the contention path above: the consolidated
        // pack exists on disk, so the counts must say so. Only the old-pack
        // deletion was abandoned.
        return Ok(TaskResult {
            task: "incremental-repack".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: all_hashes.len(),
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: format!(
                "consolidated {} object(s) into a new pack, then aborted deleting the {} old \
                  pack(s): concurrent repository activity created new reachability roots during \
                  the repack. The consolidated pack was kept (harmless duplicate data) — re-run \
                  when the repository is quiescent.",
                all_hashes.len(),
                packs.len()
            ),
        });
    }

    // Remove the old packs (their objects now live in the consolidated pack).
    // `packs` was captured before the new pack was written, so it never names
    // it. Final belt for the second-pass→delete window: a pack YOUNGER than
    // the grace hour is retained this run (a fetch writes its pack before
    // its ref update — a just-fetched pack whose ref lands in that window
    // must survive; it consolidates on the next quiescent run). Stat errors
    // retain too (never delete what cannot be aged).
    const REPACK_GRACE_SECS: u64 = 3600;
    let now = std::time::SystemTime::now();
    let mut kept_pinned = 0usize;
    for old_pack in &packs {
        // A `.keep` sentinel (written by fetch before its pack lands,
        // removed after its refs update) pins the pack unconditionally —
        // this closes the arbitrarily-long pack-written→refs-updated
        // window that no time-based grace can. A crash-orphaned sentinel
        // fails SAFE (pack retained and reported until removed).
        if old_pack.with_extension("keep").exists() {
            kept_pinned += 1;
            continue;
        }
        let aged_out = fs::metadata(old_pack)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| now.duration_since(mtime).ok())
            .is_some_and(|age| age.as_secs() >= REPACK_GRACE_SECS);
        if !aged_out {
            continue;
        }
        let _ = fs::remove_file(old_pack);
        let idx_path = old_pack.with_extension("idx");
        let _ = fs::remove_file(idx_path);
    }
    // `_repack_deletion_lock` closes the exclusion window when this function
    // returns — after the last unlink above, and on the early-return and
    // unwind paths too.
    let repacked = all_hashes.len();
    let new_pack_name = new_publication
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "the consolidated pack".to_string());

    if !quiet {
        info_println(
            output,
            &format!("  consolidated into {new_pack_name} with {repacked} objects"),
        );
    }

    let mut message = format!(
        "repacked {} packs into {} with {repacked} objects",
        packs.len(),
        new_pack_name
    );
    if kept_pinned > 0 {
        message.push_str(&format!(
            "; retained {kept_pinned} pack(s) pinned by a .keep sentinel (an in-flight or \
              crashed fetch) — remove the stale sentinel to let a later repack reclaim them"
        ));
    }
    Ok(TaskResult {
        task: "incremental-repack".to_string(),
        success: true,
        objects_removed: 0,
        objects_packed: repacked,
        refs_packed: 0,
        packs_repacked: packs.len(),
        object_index_rows_removed: 0,
        message,
    })
}

// ---------------------------------------------------------------------------
// Commit-graph task
// ---------------------------------------------------------------------------

async fn run_commit_graph(
    _repo_path: &Path,
    dry_run: bool,
    _quiet: bool,
    _output: &OutputConfig,
) -> CliResult<TaskResult> {
    let skip = |msg: &str| TaskResult {
        task: "commit-graph".to_string(),
        success: true,
        objects_removed: 0,
        objects_packed: 0,
        refs_packed: 0,
        packs_repacked: 0,
        object_index_rows_removed: 0,
        message: msg.to_string(),
    };

    // Collect every commit reachable from a local branch tip.
    let branches = Branch::list_branches_result(None)
        .await
        .map_err(|e| CliError::fatal(format!("failed to list branches: {e}")))?;
    let mut commits: HashMap<ObjectHash, Commit> = HashMap::new();
    for branch in &branches {
        for commit in get_reachable_commits(branch.commit.to_string(), None).await? {
            commits.entry(commit.id).or_insert(commit);
        }
    }

    if commits.is_empty() {
        return Ok(skip("no commits to index; skipped"));
    }
    // Octopus merges (>2 parents) are written via the EDGE chunk and SHA-256
    // repositories via the wider OIDs + a SHA-256 header version/trailer, both
    // handled by `build_commit_graph`.

    let count = commits.len();
    if dry_run {
        return Ok(TaskResult {
            objects_packed: count,
            object_index_rows_removed: 0,
            message: format!("would write commit-graph for {count} commits"),
            ..skip("")
        });
    }

    let bytes = build_commit_graph(&commits)
        .ok_or_else(|| CliError::fatal("failed to build commit-graph".to_string()))?;
    let info_dir = path::objects().join("info");
    fs::create_dir_all(&info_dir)
        .map_err(|e| CliError::fatal(format!("failed to create objects/info: {e}")))?;
    fs::write(info_dir.join("commit-graph"), &bytes)
        .map_err(|e| CliError::fatal(format!("failed to write commit-graph: {e}")))?;

    Ok(TaskResult {
        objects_packed: count,
        object_index_rows_removed: 0,
        message: format!("wrote commit-graph for {count} commits"),
        ..skip("")
    })
}

/// Topological generation numbers: `gen(c) = 1 + max(gen(parents))`, roots = 1.
/// Iterates to a fixpoint (converges in O(history depth) passes).
fn compute_generations(commits: &HashMap<ObjectHash, Commit>) -> HashMap<ObjectHash, u32> {
    let mut generations: HashMap<ObjectHash, u32> = commits.keys().map(|k| (*k, 1u32)).collect();
    loop {
        let mut changed = false;
        for (oid, commit) in commits {
            let parent_max = commit
                .parent_commit_ids
                .iter()
                .filter_map(|p| generations.get(p))
                .copied()
                .max()
                .unwrap_or(0);
            if parent_max + 1 > generations[oid] {
                generations.insert(*oid, parent_max + 1);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    generations
}

/// Encode a v1 commit-graph file with the OIDF, OIDL, and CDAT chunks — plus an
/// EDGE chunk when any commit has more than two parents (octopus merges) — and a
/// trailing checksum, matching Git's format. The OID width, header hash version,
/// and trailer digest follow the repository's hash kind (SHA-1 or SHA-256).
fn build_commit_graph(commits: &HashMap<ObjectHash, Commit>) -> Option<Vec<u8>> {
    /// Sentinel parent slot meaning "no parent" (GRAPH_PARENT_NONE).
    const GRAPH_PARENT_NONE: u32 = 0x7000_0000;
    /// In a CDAT second-parent slot, this high bit means "more than two parents:
    /// the low 31 bits are an index into the EDGE chunk" (GRAPH_EXTRA_EDGES_NEEDED).
    const GRAPH_EXTRA_EDGES_NEEDED: u32 = 0x8000_0000;
    /// In the EDGE chunk, this high bit marks a commit's final extra parent.
    const GRAPH_LAST_EDGE: u32 = 0x8000_0000;
    if commits.is_empty() {
        return None;
    }

    let mut oids: Vec<ObjectHash> = commits.keys().copied().collect();
    oids.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));
    let pos: HashMap<ObjectHash, u32> = oids
        .iter()
        .enumerate()
        .map(|(i, o)| (*o, i as u32))
        .collect();
    let hash_len = oids[0].size();
    let n = oids.len();
    let generations = compute_generations(commits);

    // Pre-compute each commit's two CDAT parent slots and, for octopus merges
    // (>2 parents), the EDGE chunk holding parents 2..N. A commit with >2
    // parents stores `GRAPH_EXTRA_EDGES_NEEDED | <edge index>` in its second
    // slot; the EDGE chunk then lists those parents' positions, the last one
    // OR-ed with `GRAPH_LAST_EDGE`.
    let mut parent_slots: Vec<(u32, u32)> = Vec::with_capacity(n);
    let mut edge_data: Vec<u32> = Vec::new();
    for o in &oids {
        let parents = &commits[o].parent_commit_ids;
        let p1 = parents
            .first()
            .and_then(|p| pos.get(p))
            .copied()
            .unwrap_or(GRAPH_PARENT_NONE);
        let p2 = if parents.len() <= 2 {
            parents
                .get(1)
                .and_then(|p| pos.get(p))
                .copied()
                .unwrap_or(GRAPH_PARENT_NONE)
        } else {
            let edge_index = edge_data.len() as u32;
            let extra = &parents[1..];
            for (i, par) in extra.iter().enumerate() {
                let mut slot = pos.get(par).copied().unwrap_or(GRAPH_PARENT_NONE);
                if i + 1 == extra.len() {
                    slot |= GRAPH_LAST_EDGE;
                }
                edge_data.push(slot);
            }
            GRAPH_EXTRA_EDGES_NEEDED | edge_index
        };
        parent_slots.push((p1, p2));
    }
    let has_edges = !edge_data.is_empty();

    // Cumulative OID fanout over the first OID byte.
    let mut fanout = [0u32; 256];
    for o in &oids {
        fanout[o.as_ref()[0] as usize] += 1;
    }
    let mut acc = 0u32;
    for slot in fanout.iter_mut() {
        acc += *slot;
        *slot = acc;
    }

    // The EDGE chunk (when present) follows CDAT; the chunk count and offsets
    // grow accordingly.
    let num_chunks: u8 = if has_edges { 4 } else { 3 };
    let toc_len = (num_chunks as u64 + 1) * 12; // chunks + terminator entry
    let oidf_off = 8 + toc_len;
    let oidl_off = oidf_off + 1024;
    let cdat_off = oidl_off + (n as u64) * (hash_len as u64);
    let edge_off = cdat_off + (n as u64) * (hash_len as u64 + 16);
    let edge_bytes = edge_data.len() as u64 * 4;
    let trailer_off = if has_edges {
        edge_off + edge_bytes
    } else {
        cdat_off + (n as u64) * (hash_len as u64 + 16)
    };

    // Hash version: 1 for SHA-1, 2 for SHA-256 (matches the OID width already
    // used by the OIDL/CDAT chunks via `hash_len`).
    let hash_version: u8 = if oids[0].kind() == HashKind::Sha256 {
        2
    } else {
        1
    };

    let mut buf: Vec<u8> = Vec::with_capacity(trailer_off as usize + hash_len);
    // Header: "CGPH", version 1, hash version, N chunks, 0 base graphs.
    buf.extend_from_slice(b"CGPH");
    buf.extend_from_slice(&[1, hash_version, num_chunks, 0]);
    // Chunk table of contents.
    buf.extend_from_slice(b"OIDF");
    buf.extend_from_slice(&oidf_off.to_be_bytes());
    buf.extend_from_slice(b"OIDL");
    buf.extend_from_slice(&oidl_off.to_be_bytes());
    buf.extend_from_slice(b"CDAT");
    buf.extend_from_slice(&cdat_off.to_be_bytes());
    if has_edges {
        buf.extend_from_slice(b"EDGE");
        buf.extend_from_slice(&edge_off.to_be_bytes());
    }
    buf.extend_from_slice(&[0u8; 4]);
    buf.extend_from_slice(&trailer_off.to_be_bytes());
    // OIDF.
    for f in fanout {
        buf.extend_from_slice(&f.to_be_bytes());
    }
    // OIDL.
    for o in &oids {
        buf.extend_from_slice(o.as_ref());
    }
    // CDAT.
    for (o, (p1, p2)) in oids.iter().zip(&parent_slots) {
        let commit = &commits[o];
        buf.extend_from_slice(commit.tree_id.as_ref());
        buf.extend_from_slice(&p1.to_be_bytes());
        buf.extend_from_slice(&p2.to_be_bytes());
        // Last 8 bytes pack generation (top 30 bits) + commit time (34 bits).
        let g = generations[o] as u64;
        let t = commit.committer.timestamp as u64;
        let first = ((g << 2) | ((t >> 32) & 0x3)) as u32;
        let second = (t & 0xFFFF_FFFF) as u32;
        buf.extend_from_slice(&first.to_be_bytes());
        buf.extend_from_slice(&second.to_be_bytes());
    }
    // EDGE (octopus extra parents), when present.
    if has_edges {
        for slot in &edge_data {
            buf.extend_from_slice(&slot.to_be_bytes());
        }
    }
    // Trailer: checksum of everything written so far, in the repository's hash
    // algorithm (SHA-1 or SHA-256), matching the OID width used above.
    let digest: Vec<u8> = match oids[0].kind() {
        HashKind::Sha256 => sha2::Sha256::digest(&buf).to_vec(),
        HashKind::Sha1 => sha1::Sha1::digest(&buf).to_vec(),
    };
    buf.extend_from_slice(&digest);
    Some(buf)
}

// ---------------------------------------------------------------------------
// Prefetch task
// ---------------------------------------------------------------------------

async fn run_prefetch(
    _repo_path: &Path,
    dry_run: bool,
    _quiet: bool,
    output: &OutputConfig,
) -> CliResult<TaskResult> {
    let skip = |msg: &str| TaskResult {
        task: "prefetch".to_string(),
        success: true,
        objects_removed: 0,
        objects_packed: 0,
        refs_packed: 0,
        packs_repacked: 0,
        object_index_rows_removed: 0,
        message: msg.to_string(),
    };

    // Prefetch every configured remote so later fetches transfer less. Unlike
    // Git (which stages tips under `refs/prefetch/`), Libra reuses the normal
    // fetch path and refreshes the standard remote-tracking refs — an
    // intentional difference, since `maintenance` is an explicit, opt-in run.
    let remotes = ConfigKv::all_remote_configs()
        .await
        .map_err(|e| CliError::fatal(format!("failed to read remote configuration: {e}")))?;
    if remotes.is_empty() {
        return Ok(skip("no remotes configured; skipped"));
    }
    if dry_run {
        return Ok(TaskResult {
            refs_packed: remotes.len(),
            object_index_rows_removed: 0,
            message: format!("would prefetch from {} remote(s)", remotes.len()),
            ..skip("")
        });
    }

    let mut fetched = 0usize;
    let mut failures = Vec::new();
    for remote in remotes {
        let name = remote.name.clone();
        match fetch_repository_safe(remote, None, false, None, None, output).await {
            Ok(()) => fetched += 1,
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }

    // Report a hard failure only when nothing could be prefetched at all.
    if fetched == 0 && !failures.is_empty() {
        return Ok(TaskResult {
            success: false,
            object_index_rows_removed: 0,
            message: format!("prefetch failed: {}", failures.join("; ")),
            ..skip("")
        });
    }
    let message = if failures.is_empty() {
        format!("prefetched {fetched} remote(s)")
    } else {
        format!(
            "prefetched {fetched} remote(s); {} failed ({})",
            failures.len(),
            failures.join("; ")
        )
    };
    Ok(TaskResult {
        refs_packed: fetched,
        object_index_rows_removed: 0,
        message,
        ..skip("")
    })
}

// ---------------------------------------------------------------------------
// Register / Unregister / Status
// ---------------------------------------------------------------------------

async fn register(schedule: &str, output: &OutputConfig) -> CliResult<()> {
    try_get_storage_path(None).map_err(|e| CliError::repo_not_found().with_hint(e.to_string()))?;

    ConfigKv::set(MAINTENANCE_ENABLED_KEY, "true", false)
        .await
        .map_err(|e| CliError::fatal(format!("failed to set maintenance config: {e}")))?;

    ConfigKv::set(MAINTENANCE_SCHEDULE_KEY, schedule, false)
        .await
        .map_err(|e| CliError::fatal(format!("failed to set maintenance schedule: {e}")))?;

    if output.is_json() {
        return emit_json_data(
            "maintenance.register",
            &serde_json::json!({ "registered": true, "schedule": schedule }),
            output,
        );
    }

    info_println(
        output,
        &format!("Repository registered for maintenance (schedule: {schedule})"),
    );
    Ok(())
}

async fn unregister(output: &OutputConfig) -> CliResult<()> {
    try_get_storage_path(None).map_err(|e| CliError::repo_not_found().with_hint(e.to_string()))?;

    ConfigKv::set(MAINTENANCE_ENABLED_KEY, "false", false)
        .await
        .map_err(|e| CliError::fatal(format!("failed to unset maintenance config: {e}")))?;

    if output.is_json() {
        return emit_json_data(
            "maintenance.unregister",
            &serde_json::json!({ "registered": false }),
            output,
        );
    }

    info_println(output, "Repository unregistered from maintenance");
    Ok(())
}

async fn status(output: &OutputConfig) -> CliResult<()> {
    try_get_storage_path(None).map_err(|e| CliError::repo_not_found().with_hint(e.to_string()))?;

    let enabled = ConfigKv::get(MAINTENANCE_ENABLED_KEY)
        .await
        .ok()
        .flatten()
        .is_some_and(|entry| entry.value == "true");

    let schedule = ConfigKv::get(MAINTENANCE_SCHEDULE_KEY)
        .await
        .ok()
        .flatten()
        .map(|entry| entry.value);

    let last_run = ConfigKv::get(MAINTENANCE_LAST_RUN_KEY)
        .await
        .ok()
        .flatten()
        .map(|entry| entry.value);

    let data = MaintenanceStatusOutput {
        registered: enabled,
        schedule: schedule.clone(),
        last_run,
    };

    if output.is_json() {
        return emit_json_data("maintenance.status", &data, output);
    }

    if enabled {
        info_println(output, "Maintenance: registered");
        if let Some(s) = schedule {
            info_println(output, &format!("Schedule: {s}"));
        }
    } else {
        info_println(output, "Maintenance: not registered");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// OS scheduler integration (start / stop)
// ---------------------------------------------------------------------------

/// Overrides the directory the scheduler entry is written to. Tests set this to
/// a temp dir so `start`/`stop` never touch the real launchd/cron locations.
const MAINTENANCE_AGENT_DIR_ENV: &str = "LIBRA_MAINTENANCE_AGENT_DIR";

/// Resolve where the OS scheduler entry lives: the override env var, else the
/// per-user LaunchAgents dir on macOS, else `~/.config/libra/scheduler`.
fn scheduler_agent_dir() -> PathBuf {
    if let Ok(dir) = std::env::var(MAINTENANCE_AGENT_DIR_ENV) {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library").join("LaunchAgents")
    } else {
        PathBuf::from(home)
            .join(".config")
            .join("libra")
            .join("scheduler")
    }
}

/// Deterministic per-repository label/filename stem (sha1 of the repo path).
fn scheduler_label(repo: &Path) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(repo.to_string_lossy().as_bytes());
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("tools.libra.maintenance.{}", &digest[..12])
}

fn schedule_interval_secs(schedule: &str) -> u64 {
    match schedule {
        "weekly" => 604_800,
        "daily" => 86_400,
        _ => 3_600, // hourly (default / unknown)
    }
}

fn schedule_cron_expr(schedule: &str) -> &'static str {
    match schedule {
        "weekly" => "0 0 * * 0",
        "daily" => "0 0 * * *",
        _ => "0 * * * *",
    }
}

/// Write the OS scheduler entry into `dir`, returning its path. macOS gets a
/// launchd agent plist (LaunchAgents auto-load at the next login); other Unix
/// gets a cron fragment that runs `libra maintenance run`.
fn write_scheduler_entry(
    dir: &Path,
    label: &str,
    exe: &Path,
    repo: &Path,
    schedule: &str,
) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let exe = exe.to_string_lossy();
    let repo = repo.to_string_lossy();
    if cfg!(target_os = "macos") {
        let path = dir.join(format!("{label}.plist"));
        let interval = schedule_interval_secs(schedule);
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
 <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
 <plist version=\"1.0\">\n<dict>\n    \
 <key>Label</key>\n    <string>{label}</string>\n    \
 <key>ProgramArguments</key>\n    <array>\n        <string>{exe}</string>\n        \
 <string>maintenance</string>\n        <string>run</string>\n    </array>\n    \
 <key>WorkingDirectory</key>\n    <string>{repo}</string>\n    \
 <key>StartInterval</key>\n    <integer>{interval}</integer>\n    \
 <key>RunAtLoad</key>\n    <false/>\n</dict>\n</plist>\n"
        );
        fs::write(&path, plist)?;
        Ok(path)
    } else {
        let path = dir.join(format!("{label}.cron"));
        let expr = schedule_cron_expr(schedule);
        fs::write(
            &path,
            format!("{expr} cd \"{repo}\" && \"{exe}\" maintenance run\n"),
        )?;
        Ok(path)
    }
}

/// Remove a previously-written scheduler entry; returns whether anything existed.
fn remove_scheduler_entry(dir: &Path, label: &str) -> std::io::Result<bool> {
    let mut removed = false;
    for ext in ["plist", "cron"] {
        let path = dir.join(format!("{label}.{ext}"));
        if path.exists() {
            fs::remove_file(&path)?;
            removed = true;
        }
    }
    Ok(removed)
}

async fn start(schedule: &str, output: &OutputConfig) -> CliResult<()> {
    try_get_storage_path(None).map_err(|e| CliError::repo_not_found().with_hint(e.to_string()))?;

    ConfigKv::set(MAINTENANCE_ENABLED_KEY, "true", false)
        .await
        .map_err(|e| CliError::fatal(format!("failed to set maintenance config: {e}")))?;
    ConfigKv::set(MAINTENANCE_SCHEDULE_KEY, schedule, false)
        .await
        .map_err(|e| CliError::fatal(format!("failed to set maintenance schedule: {e}")))?;

    let repo = std::env::current_dir()
        .map_err(|e| CliError::fatal(format!("failed to resolve repository directory: {e}")))?;
    let exe = std::env::current_exe()
        .map_err(|e| CliError::fatal(format!("failed to resolve libra executable: {e}")))?;
    let dir = scheduler_agent_dir();
    let label = scheduler_label(&repo);
    let entry = write_scheduler_entry(&dir, &label, &exe, &repo, schedule)
        .map_err(|e| CliError::fatal(format!("failed to write scheduler entry: {e}")))?;

    if output.is_json() {
        return emit_json_data(
            "maintenance.start",
            &serde_json::json!({
                "registered": true,
                "schedule": schedule,
                "scheduler_entry": entry.display().to_string(),
            }),
            output,
        );
    }
    info_println(
        output,
        &format!(
            "Maintenance scheduled ({schedule}); scheduler entry written to {}",
            entry.display()
        ),
    );
    Ok(())
}

async fn stop(output: &OutputConfig) -> CliResult<()> {
    try_get_storage_path(None).map_err(|e| CliError::repo_not_found().with_hint(e.to_string()))?;

    ConfigKv::set(MAINTENANCE_ENABLED_KEY, "false", false)
        .await
        .map_err(|e| CliError::fatal(format!("failed to unset maintenance config: {e}")))?;

    let repo = std::env::current_dir()
        .map_err(|e| CliError::fatal(format!("failed to resolve repository directory: {e}")))?;
    let dir = scheduler_agent_dir();
    let label = scheduler_label(&repo);
    let removed = remove_scheduler_entry(&dir, &label)
        .map_err(|e| CliError::fatal(format!("failed to remove scheduler entry: {e}")))?;

    if output.is_json() {
        return emit_json_data(
            "maintenance.stop",
            &serde_json::json!({ "registered": false, "removed": removed }),
            output,
        );
    }
    info_println(output, "Maintenance scheduler stopped");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Collect all reachable objects from refs, index, and reflogs.
pub(crate) async fn collect_reachable_objects(
    storage: &ClientStorage,
) -> CliResult<HashSet<ObjectHash>> {
    let db_conn = db::get_db_conn_instance().await;
    collect_reachable_objects_with_conn(storage, &db_conn).await
}

/// [`collect_reachable_objects`] on a caller-supplied connection.
///
/// §C.4.3 writer-vs-deleter: the deletion phase runs this INSIDE its own
/// transaction. Because the repository database uses SQLite's rollback
/// journal, an open read transaction holds a SHARED lock, and a concurrent
/// ref publication cannot commit until it is released — so no reference can
/// appear between this revalidation and the unlink that follows it. Two
/// historical scans narrow the window; only this closes it.
pub(crate) async fn collect_reachable_objects_with_conn<C>(
    storage: &ClientStorage,
    db_conn: &C,
) -> CliResult<HashSet<ObjectHash>>
where
    C: sea_orm::ConnectionTrait,
{
    let mut reachable: HashSet<ObjectHash> = HashSet::new();
    // §C.4.2: ONE storage root for the whole collection, resolved from the
    // request pin at entry. Every file-backed collector below derives its
    // paths from this binding, so an in-process cwd move can never mix
    // repository A's indexes with repository B's registry or sidecars.
    let storage_root = crate::utils::util::request_storage_path();
    let storage_root = storage_root.as_path();
    // §C.4.3 Boundary: loaded ONCE for the whole walk. A shallow clone's
    // grafts stop parent traversal instead of demanding parents it was never
    // given; malformed metadata fails closed before anything is pruned.
    let boundaries = shallow_boundaries(storage_root)?;
    let boundaries = &boundaries;

    // Collect from refs
    let refs = reference::Entity::find()
        .all(db_conn)
        .await
        .map_err(|e| CliError::fatal(format!("failed to load refs: {e}")))?;

    for ref_entry in refs {
        if let Some(commit_hash_str) = &ref_entry.commit {
            let hash = parse_object_hash(commit_hash_str).ok_or_else(|| {
                CliError::fatal(format!(
                    "reference '{}' contains invalid object id '{}'",
                    ref_entry.name.as_deref().unwrap_or("<unnamed>"),
                    commit_hash_str
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            // Do NOT pre-insert `hash`: `walk_reachable` returns early when the
            // hash is already in the set, so pre-inserting would stop it from
            // descending into the commit's tree — leaving reachable trees/blobs
            // looking unreachable (gc could then prune live objects).
            walk_reachable(&hash, storage, boundaries, &mut reachable)?;
        }
    }

    // Collect from reflogs
    let reflogs = reflog::Entity::find()
        .all(db_conn)
        .await
        .map_err(|e| CliError::fatal(format!("failed to load reflogs: {e}")))?;

    let is_null_oid = |oid: &str| oid.chars().all(|c| c == '0');
    for entry in reflogs {
        for (field, oid) in [("old", &entry.old_oid), ("new", &entry.new_oid)] {
            if is_null_oid(oid) {
                continue;
            }
            let hash = parse_object_hash(oid).ok_or_else(|| {
                CliError::fatal(format!(
                    "reflog entry {} contains invalid {field} object id '{}'",
                    entry.id, oid
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            // As above: let `walk_reachable` perform the insert so it descends
            // into the commit's tree instead of returning early. Both sides of
            // every reflog entry are roots: the oldest retained `old_oid` need
            // not occur as another row's `new_oid`.
            walk_reachable(&hash, storage, boundaries, &mut reachable)?;
        }
    }

    // Held autostashes deliberately do not enter refs/stash while a merge or
    // rebase is in progress. Their fsynced sidecars are therefore first-class
    // GC roots; omitting them can irreversibly delete the user's dirty state.
    // Part C §C.9: the sidecars are worktree-LOCAL, so enumerate EVERY
    // worktree's gitdir — a linked worktree's held autostash is exactly as
    // live as main's.
    for gitdir in worktree_gitdir_roots(storage_root)? {
        let merge_autostash =
            crate::command::merge::MergeAutostash::load_optional_sync_in_gitdir(&gitdir)
                .map_err(|error| {
                    CliError::fatal(format!("failed to load merge autostash GC root: {error}"))
                        .with_stable_code(StableErrorCode::IoReadFailed)
                })?
                .map(|held| {
                    parse_object_hash(&held.stash_commit).ok_or_else(|| {
                        CliError::fatal(format!(
                            "merge-autostash.json contains invalid object id '{}'",
                            held.stash_commit
                        ))
                        .with_stable_code(StableErrorCode::RepoCorrupt)
                    })
                })
                .transpose()?;
        let rebase_autostash = crate::command::rebase::held_autostash_oid_in_gitdir(&gitdir)?;
        for held in [merge_autostash, rebase_autostash].into_iter().flatten() {
            walk_reachable(&held, storage, boundaries, &mut reachable)?;
        }
    }

    // plan-20260714 §C.9 item 10: an in-progress sequencer / rebase / bisect
    // row holds the ONLY anchors for its todo/stopped objects once refs and
    // reflogs move on — a maintenance run must not prune what `--continue`
    // needs. Every row of every worktree scope is a root; a row that cannot
    // be read or carries an invalid OID fails closed (callers never prune
    // against a partial root set).
    collect_sequencer_state_roots(db_conn, storage, boundaries, &mut reachable).await?;

    // W2 §C.4.3 typed inventory: note blobs, undo view snapshots, and AI
    // capture checkpoints anchor object-store OIDs nowhere else — plus the
    // worktree-local merge/revert/rebase-aux sidecars (NOT FETCH_HEAD —
    // §C.4.3 item 13 classifies it as a non-root).
    collect_registered_store_roots(db_conn, storage, boundaries, &mut reachable).await?;
    collect_worktree_sidecar_roots(storage_root, storage, boundaries, &mut reachable)?;

    // Ordinary stashes are file-backed rather than SQLite reference rows, and
    // older entries live only in logs/refs/stash. Trace the full reflog, not
    // just refs/stash, so stash@{1} and later remain recoverable.
    let stash_roots = crate::command::stash::gc_roots(storage_root).map_err(|error| {
        CliError::fatal(format!("failed to load stash GC roots: {error}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    for oid in stash_roots {
        walk_reachable(&oid, storage, boundaries, &mut reachable)?;
    }

    // Collect from EVERY worktree's index — every stage, not just stage 0, so a
    // blob referenced only by an unmerged conflict stage (1/2/3) is not treated
    // as garbage.
    //
    // plan-20260714 Part C §C.9: each worktree owns a PRIVATE index, so walking
    // only `path::index()` (this worktree's) would classify a blob staged in
    // another worktree as unreachable — reported as garbage by `fsck` and, before
    // the multi-worktree guard, deleted by `gc`. Every registered worktree's
    // index is a reachability root. A worktree whose index cannot be read fails
    // closed: callers must never prune against a partial root set.
    for index_path in worktree_index_roots(storage_root)? {
        let index_exists = index_path.try_exists().map_err(|error| {
            CliError::fatal(format!(
                "failed to inspect index GC root '{}': {error}",
                index_path.display()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        if !index_exists {
            continue;
        }
        let index = git_internal::internal::index::Index::load(&index_path).map_err(|error| {
            CliError::fatal(format!(
                "failed to read index GC root '{}': {error}",
                index_path.display()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        for stage in 0..=3 {
            for entry in index.tracked_entries(stage) {
                // Gitlinks (`160000`) reference a SUBMODULE's commit, which
                // legitimately does not exist in this repository's store.
                if entry.mode == 0o160000 {
                    continue;
                }
                // §C.11 W2: a staged or conflict-stage blob is MANDATORY
                // reachability — its index entry may be its only anchor, so
                // an entry naming a missing object fails the prune closed
                // rather than being carried as an unbacked root.
                match storage.get_object_type(&entry.hash) {
                    Ok(_) => {}
                    Err(git_internal::errors::GitError::ObjectNotFound(_)) => {
                        return Err(CliError::fatal(format!(
                            "index GC root '{}' entry '{}' (stage {stage}) names object {}, \
                              which does not exist — pruning would delete the remaining \
                              anchors of state that is already damaged",
                            index_path.display(),
                            entry.name,
                            entry.hash
                        ))
                        .with_stable_code(StableErrorCode::RepoCorrupt));
                    }
                    Err(error) => {
                        return Err(CliError::fatal(format!(
                            "failed to probe index GC root '{}' entry '{}' ({}): {error}",
                            index_path.display(),
                            entry.name,
                            entry.hash
                        ))
                        .with_stable_code(StableErrorCode::IoReadFailed));
                    }
                }
                reachable.insert(entry.hash);
            }
        }
    }

    Ok(reachable)
}

/// Every worktree's private index path — MAIN's, this worktree's, and each
/// registered linked worktree's `<path>/.libra/index` (plan-20260714 Part C
/// §C.9).
///
/// The current worktree's index always comes first so a single-worktree
/// repository behaves exactly as before. Registry entries whose directory is
/// gone are skipped (a pruned worktree holds nothing); the caller's
/// `try_exists` handles a registered-but-indexless worktree.
///
/// MAIN's index is seeded EXPLICITLY (its gitdir is the common storage
/// root), because the registry loop below skips the main entry and
/// `path::index()` resolves the INVOKING worktree: a gc run from a linked
/// worktree would otherwise walk every index except main's, and a blob
/// staged only in main would be pruned — the exact data-loss class this
/// root set exists to prevent.
pub(crate) fn worktree_index_roots(
    storage_root: &std::path::Path,
) -> CliResult<Vec<std::path::PathBuf>> {
    let mut roots = vec![path::index()];
    // §C.4.2: EVERYTHING below binds to `storage_root` — the storage the
    // caller resolved ONCE from the request pin. A first version compared
    // the pin against a fresh ambient resolution instead; that check is
    // TOCTOU-shaped (a cwd that moved away and back between collectors
    // passes it), so the binding is now by construction: the main index and
    // the registry are both derived from the given root, never re-resolved.
    let main_index = storage_root.join("index");
    if !roots.contains(&main_index) {
        roots.push(main_index);
    }
    // W2 §C.4.3: with the multi-worktree prune/repack skips lifted, a partial
    // root set is a DATA-LOSS vector — an unreadable registry or a registered
    // worktree whose directory is missing (unmounted volume, half-removed
    // tree) fails the walk CLOSED instead of silently narrowing the roots.
    let list =
        crate::command::worktree::run_list_worktrees_at(&storage_root.join("worktrees.json"))
            .map_err(|error| {
                CliError::fatal(format!(
             "cannot enumerate worktree GC roots: the worktree registry is unreadable: {error}"
         ))
         .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
    for entry in list.worktrees {
        if entry.is_main {
            continue;
        }
        // W3-s1b (§C.7): a TOMBSTONE proves the directory was durably
        // deleted — its private index no longer exists to be a root (the
        // scope's DB rows are still enumerated by the row-scan roots until
        // repair finishes the cleanup). Every other registered state with a
        // missing directory still fails the walk closed.
        if entry.state == "tombstone" {
            continue;
        }
        if !entry.exists {
            return Err(CliError::fatal(format!(
                "cannot enumerate worktree GC roots: registered worktree '{}' is missing on \
                  disk — its private index (a reachability root) cannot be read",
                entry.path
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
            .with_hint(
                "restore the worktree directory, or unregister it with `libra worktree \
                  prune` / `libra worktree remove` first",
            ));
        }
        let candidate = std::path::Path::new(&entry.path)
            .join(crate::utils::util::ROOT_DIR)
            .join("index");
        if !roots.contains(&candidate) {
            roots.push(candidate);
        }
    }
    Ok(roots)
}

/// Every worktree's private gitdir (the parent of its index) — used to
/// enumerate worktree-local GC-root sidecars (`merge-autostash.json`,
/// `rebase-aux.json`) across ALL worktrees (Part C §C.9).
fn worktree_gitdir_roots(storage_root: &std::path::Path) -> CliResult<Vec<std::path::PathBuf>> {
    Ok(worktree_index_roots(storage_root)?
        .into_iter()
        .filter_map(|index| index.parent().map(|dir| dir.to_path_buf()))
        .collect())
}

/// plan-20260714 §C.9 item 10: trace the OID columns of every
/// `sequence_state` / `rebase_state` / `bisect_state` row — across ALL
/// worktree scopes — as reachability roots. An interrupted cherry-pick's todo
/// commits, a stopped rebase's `stopped_sha`, or a bisect session's bounds
/// may have no other anchor once refs/reflogs move on.
///
/// Fail-closed contract: a row that cannot be read or a structured OID column
/// that does not parse is a hard error (callers never prune against a partial
/// root set). A MISSING table resolves to "no roots from that store" — all
/// three are migration-owned now (`bisect_state` since `2026072301`), but
/// bare test databases may lack any of them. The free-form `payload` column is scanned leniently: JSON
/// string values that parse as OIDs are walked only when the object exists
/// (payload content is op-specific, not a structured OID column).
/// plan-20260714 §C.4.3: the VERSIONED, TYPED inventory of every persistent
/// store that can hold object-store OIDs. GC/fsck reachability must account
/// for each entry — either as a traced root source or as a documented
/// non-root — before any prune runs. The `gc_object_source_inventory` guard
/// test scans the live schema and fails when an OID-shaped column appears
/// that this inventory does not know, so a new store cannot silently ship
/// un-traced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcSourceStatus {
    /// Contributes reachability roots in `collect_reachable_objects`.
    /// Objects reachable from here are KEPT.
    ///
    /// (`TracedRoot` is the historical spelling of §C.4.3's
    /// `ReachabilityRoot`; kept as the variant name so the DB inventory rows
    /// below and their guard test do not all churn.)
    TracedRoot,
    /// §C.4.3 `AntiRoot`: objects listed here are DELIBERATELY absent, and
    /// must never be resurrected by heal, hydrate, or an alternate. Missing
    /// payload is the expected state, not corruption.
    AntiRoot,
    /// §C.4.3 `Boundary`: a stop-list for the traversal. The boundary object
    /// itself is kept; the graph beyond it does not exist in this clone and
    /// must not be demanded.
    Boundary,
    /// §C.4.3 `IndexOnly`: a catalogue/visibility artifact, not the object.
    /// It keeps nothing alive, and it must be invalidated in step with the
    /// deletion it describes.
    IndexOnly,
    /// Deliberately NOT a root, with the reason documented alongside.
    NonRoot,
}

/// §C.4.3: where a source lives. File-backed sources are inventoried
/// alongside the DB columns because the four semantic kinds cut across both
/// — a shallow `Boundary` and an obliteration `AntiRoot` are files, and a
/// classification model that only knows about SQLite columns cannot express
/// either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcSourceOrigin {
    /// A SQLite column: `(table, column)`.
    Column,
    /// A file or directory under the repository storage.
    File,
}

/// §C.4.3: the STORAGE KIND a source lives in — one of the five shapes the
/// plan names. Declared per row so a reader can tell how to inspect it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcStorageKind {
    /// A column of a migration-owned SQLite table.
    SqliteColumn,
    /// A Libra-owned JSON document (in-progress operation sidecars, ledgers,
    /// findings manifests).
    JsonManifest,
    /// A loose ref-shaped file (`refs/stash`, `refs/replace`, their logs).
    LooseRef,
    /// A private worktree index.
    Index,
    /// A non-JSON sidecar/text file (shallow grafts, FETCH_HEAD, editor
    /// buffers, raw backups).
    Sidecar,
}

/// §C.4.3: what the collector does when a source cannot be READ or names a
/// missing object. Declared per row and structurally checked against the
/// row's root type by `gc_object_source_inventory_is_typed_across_all_four_kinds`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcCorruptionPolicy {
    /// Unreadable/corrupt/missing-object ⇒ the prune STOPS (mandatory
    /// reachability).
    FailClosed,
    /// Entries that cannot be read are skipped; the source keeps nothing
    /// alive, so losing it costs staleness, never an object.
    LenientSkip,
    /// A row whose liveness cannot be judged counts as LIVE and defers the
    /// destructive phase (traces-inflight markers).
    DeferLive,
    /// The source contributes no roots at all, so corruption of it cannot
    /// change what survives.
    NotApplicable,
}

/// One inventoried source: what it is, where it lives, how GC treats it —
/// carrying every declaration §C.4.3 requires (storage kind, schema/version,
/// read bound, corruption policy, root type).
#[derive(Debug, Clone, Copy)]
pub struct GcObjectSource {
    pub origin: GcSourceOrigin,
    /// Table name for `Column` origins; a path pattern for `File` origins.
    pub location: &'static str,
    /// Column name for `Column` origins; `""` for files.
    pub column: &'static str,
    pub status: GcSourceStatus,
    /// §C.4.3 storage kind.
    pub kind: GcStorageKind,
    /// §C.4.3 schema/version: which parser/migration owns this shape.
    pub schema: &'static str,
    /// §C.4.3 read bound: how much a collection pass reads from it.
    pub read_bound: &'static str,
    /// §C.4.3 corruption policy: what happens when it cannot be trusted.
    pub corruption: GcCorruptionPolicy,
    pub note: &'static str,
}

/// §C.4.3: the FILE-backed half of the inventory. Each entry names the
/// collector or gate that implements its classification, so a reader can
/// check the claim rather than trust it.
pub const GC_OBJECT_FILE_SOURCE_INVENTORY: &[GcObjectSource] = &[
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: "<gitdir>/merge-autostash.json, merge-state.json, revert-state.json, rebase-aux.json",
        column: "",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::JsonManifest,
        schema: "typed owner structs (MergeState/RevertState/RebaseAuxState + MergeAutostash), serde with tolerated defaults",
        read_bound: "one file per registered worktree, full read",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "collect_worktree_sidecar_roots — held autostash and in-progress operation state, across every worktree scope; corrupt JSON fails closed",
    },
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: ".libra/sessions/agent-runs/<id>/manifest.json",
        column: "",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::JsonManifest,
        schema: "RunManifest (findings_oid + manual_attach[].oid), structurally parsed",
        read_bound: "bounded manifest list; per-file size and total OID caps",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "collect_agent_run_manifest_roots — findings_oid and manual_attach[].oid parsed structurally and bounded; a missing manifest on a young run DEFERS the prune, an absent object fails closed",
    },
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: ".libra/shallow",
        column: "",
        status: GcSourceStatus::Boundary,
        kind: GcStorageKind::Sidecar,
        schema: "one OID per line (shallow graft list)",
        read_bound: "single file, full read",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "shallow_boundaries — parent traversal STOPS at these commits; the commits themselves are kept and their absent parents are never demanded. Unparseable metadata fails closed",
    },
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: "<gitdir>/FETCH_HEAD",
        column: "",
        status: GcSourceStatus::NonRoot,
        kind: GcStorageKind::Sidecar,
        schema: "tab-separated fetch records, one per advertised ref",
        read_bound: "single file per worktree, never read by GC",
        corruption: GcCorruptionPolicy::NotApplicable,
        note: "§C.4.3 item 13: explicitly NOT a root. fetch records advertised tips that are already up to date and have no local destination, so rooting them would pin objects nothing references. Safety comes from the writer-vs-deleter grace window (PRUNE_GRACE_SECS), not from root registration",
    },
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: ".libra/gc-prune-candidates.json",
        column: "",
        status: GcSourceStatus::IndexOnly,
        kind: GcStorageKind::JsonManifest,
        schema: "oid -> first-seen-epoch map (quarantine ledger)",
        read_bound: "single file, full read",
        corruption: GcCorruptionPolicy::LenientSkip,
        note: "read_prune_candidate_ledger — the writer-vs-deleter quarantine ledger: OIDs seen \
                unreachable, with when. Keeps nothing alive and confers no authority; losing it \
                costs one delayed prune cycle, never an object",
    },
    // ── W2 §C.4.3 re-verification: file-backed roots the walk ALREADY
    // collects, which this inventory did not name. An inventory that
    // understates the collector cannot be used to review it.
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: "<gitdir>/index",
        column: "",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::Index,
        schema: "git index v2 (git-internal parser), stages 0-3",
        read_bound: "every registered worktree's index, full read",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "collect_reachable_objects via worktree_index_roots — EVERY registered worktree's private index, at every stage: a blob that exists only as an unmerged stage 1/2/3 in a linked worktree is live. An index that cannot be read fails closed before any prune",
    },
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: "<storage>/refs/stash, <storage>/logs/refs/stash",
        column: "",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::LooseRef,
        schema: "ref file + reflog lines (stash stack)",
        read_bound: "tip + full reflog, bounded by stack depth",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "stash::gc_roots — the shared stack is file-backed, and entries below the tip live ONLY in the reflog, so the whole log is traced rather than just the tip; a stash commit's parents carry the pre-stash tree",
    },
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: "<storage>/refs/replace",
        column: "",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::LooseRef,
        schema: "<original-oid> filename -> replacement OID content",
        read_bound: "directory scan, one read per entry",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "collect_worktree_sidecar_roots — each file NAMES the replaced object and CONTAINS the replacement; both sides are roots and neither is anchored anywhere else, so a malformed entry fails closed rather than silently dropping a root",
    },
    // ── W1/W2 sidecars that are deliberately NOT roots. Each states the
    // reason, because "absent from the inventory" and "known not to hold
    // object ids" are indistinguishable to a reader otherwise.
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: "<gitdir>/MERGE_RR",
        column: "",
        status: GcSourceStatus::NonRoot,
        kind: GcStorageKind::Sidecar,
        schema: "id<TAB>path lines (rerere tracking list)",
        read_bound: "never read by GC (conflict-content ids, not OIDs)",
        corruption: GcCorruptionPolicy::NotApplicable,
        note: "rerere's per-worktree tracking list: its ids are CONFLICT-CONTENT hashes that key the rr-cache, not object-store ids. Rooting them would demand objects that were never written",
    },
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: "<storage>/rerere",
        column: "",
        status: GcSourceStatus::NonRoot,
        kind: GcStorageKind::Sidecar,
        schema: "pre/postimage file bytes under conflict-id dirs",
        read_bound: "never read by GC",
        corruption: GcCorruptionPolicy::NotApplicable,
        note: "the shared rr-cache stores pre/postimage FILE BYTES under conflict-id directories, outside the object store entirely; `rerere gc` ages them out on its own schedule",
    },
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: "<gitdir>/merge-file-backup",
        column: "",
        status: GcSourceStatus::NonRoot,
        kind: GcStorageKind::Sidecar,
        schema: "raw pre-merge file bytes",
        read_bound: "never read by GC",
        corruption: GcCorruptionPolicy::NotApplicable,
        note: "merge_file::backup_path — raw pre-merge file bytes for `libra merge-file`, never written to the object store; worktree-local so two worktrees cannot clean up each other's backups",
    },
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: "<gitdir>/stash-branch-journal.json",
        column: "",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::JsonManifest,
        schema: "StashBranchJournal (base/prior_detached OIDs + text fields), typed extractor",
        read_bound: "one file per worktree, full read",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "collect_worktree_sidecar_roots — the §C.10 rollback record for `stash branch`. Its `base` and `prior_detached` OIDs can be the ONLY anchors in a crash window: the HEAD switch rewrites the reference row without a reflog entry, so a worktree that was detached (e.g. created with `worktree add --detach`) loses its old HEAD's last anchor the moment the switch commits — recovery would then re-point HEAD at a pruned commit. Tracing the journal for its short life keeps the rollback target alive; corrupt JSON fails the prune closed like every sidecar",
    },
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: "<gitdir>/COMMIT_EDITMSG, MERGE_MSG, CHERRY_PICK_MSG, REVERT_EDITMSG, TAG_EDITMSG, NOTES_EDITMSG, BRANCH_DESCRIPTION_EDITMSG",
        column: "",
        status: GcSourceStatus::NonRoot,
        kind: GcStorageKind::Sidecar,
        schema: "free message text",
        read_bound: "never read by GC",
        corruption: GcCorruptionPolicy::NotApplicable,
        note: "worktree-local editor buffers (W2 §C.4.3). They hold MESSAGE TEXT: any object the message will eventually describe is rooted by the ref the command writes, not by the buffer",
    },
    GcObjectSource {
        origin: GcSourceOrigin::File,
        location: "<objects>/info/alternates, <objects>/info/borrowers",
        column: "",
        status: GcSourceStatus::NonRoot,
        kind: GcStorageKind::Sidecar,
        schema: "one path per line (object-store borrow declarations)",
        read_bound: "two files, full read at deletion gates",
        corruption: GcCorruptionPolicy::NotApplicable,
        note: "alternates::ensure_no_live_borrowers — a live borrower blocks every deletion entry point outright, rather than contributing roots this store cannot see",
    },
];

/// `(table, column, status, note)` — inventory version 1 (W2 §C.4.3).
pub const GC_OBJECT_SOURCE_INVENTORY: &[GcObjectSource] = &[
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "reference",
        column: "commit",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "shared refs loop",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "reflog",
        column: "old_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "reflog loop (both sides)",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "reflog",
        column: "new_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "reflog loop (both sides)",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "sequence_state",
        column: "head_orig",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "sequencer rows, all scopes",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "sequence_state",
        column: "current_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "sequencer rows, all scopes",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "sequence_state",
        column: "todo",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "newline OID list",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "sequence_state",
        column: "payload",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "lenient JSON OID scan",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "rebase_state",
        column: "onto",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "rebase rows, all scopes",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "rebase_state",
        column: "orig_head",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "rebase rows, all scopes",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "rebase_state",
        column: "current_head",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "rebase rows, all scopes",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "rebase_state",
        column: "stopped_sha",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "rebase rows, all scopes",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "rebase_state",
        column: "todo",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "newline OID list",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "rebase_state",
        column: "done",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "newline OID list",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "bisect_state",
        column: "orig_head",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "bisect rows, all scopes",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "bisect_state",
        column: "bad",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "bisect rows, all scopes",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "bisect_state",
        column: "good",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "list column",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "bisect_state",
        column: "current",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "bisect rows, all scopes",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "bisect_state",
        column: "skipped",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "list column",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "notes",
        column: "blob",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "note content blobs are anchored ONLY here",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "legacy_operation_view_ref",
        column: "target_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "undo/view snapshots must stay restorable",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "agent_checkpoint",
        column: "parent_commit",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "AI capture chain",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "agent_checkpoint",
        column: "tree_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "AI capture tree",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "agent_checkpoint",
        column: "metadata_blob_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "AI capture metadata",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "agent_checkpoint",
        column: "traces_commit",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "AI traces anchor",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "agent_coverage_claim",
        column: "traces_commit",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "AI coverage claim traces commit",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "agent_bridge_checkpoint",
        column: "target_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "DeepSeek Harness bridge checkpoint target object (2026081801)",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "agent_session",
        column: "parent_commit",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "AI session base commit",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "workspace_record",
        column: "base_commit",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "workspace sync-back baseline (W4 §C.8)",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "legacy_operation_view",
        column: "head_target",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "undo view HEAD pointer — rooted when it is an OID (a name is ref-anchored)",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "legacy_operation_view_workspace",
        column: "pointer_value",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "undo view workspace pointer — rooted when it is an OID",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "operation",
        column: "pre_view_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "v2 operation DAG",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "v2 operation pre-view manifest",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "operation",
        column: "post_view_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "v2 operation DAG",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "v2 operation post-view manifest",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "operation",
        column: "predecessor_map_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "v2 operation DAG",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "v2 operation predecessor map",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "operation_journal",
        column: "pre_view_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "v2 operation journal",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "in-flight v2 journal pre-view manifest",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "operation_journal",
        column: "target_view_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "v2 operation journal",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "in-flight v2 journal target manifest",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "change_revision",
        column: "commit_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "v2 change projection",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "v2 change revision commit",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "change_predecessor",
        column: "successor_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "v2 change genealogy",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "v2 change genealogy successor",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "change_predecessor",
        column: "predecessor_oid",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "v2 change genealogy",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "v2 change genealogy predecessor",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "metadata_kv",
        column: "value",
        status: GcSourceStatus::TracedRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "versioned TracesInflightMarker JSON in value (scope=agent_traces_inflight); unparseable rows count as LIVE",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::DeferLive,
        note: "traces-inflight scope via the dedicated live-marker contract (malformed rows fail closed, live markers also DEFER pruning); other scopes lenient-scanned",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "object_index",
        column: "o_id",
        status: GcSourceStatus::IndexOnly,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::LenientSkip,
        note: "catalog of what the store holds; derivable, never an anchor",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "working_dirty",
        column: "head_oid",
        status: GcSourceStatus::IndexOnly,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::LenientSkip,
        note: "advisory freshness key; the commit is ref/reflog-anchored",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "working_dirty_meta",
        column: "head_oid",
        status: GcSourceStatus::IndexOnly,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::LenientSkip,
        note: "advisory freshness key; the commit is ref/reflog-anchored",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "revision_ordinal",
        column: "oid",
        status: GcSourceStatus::IndexOnly,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::LenientSkip,
        note: "derivable ordinal cache over ref history; rebuilt on demand",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "revision_ordinal_meta",
        column: "tip_oid",
        status: GcSourceStatus::IndexOnly,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::LenientSkip,
        note: "cache validity key (the tip is ref-anchored); rebuilt on demand",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "object_obliteration",
        column: "hash_kind",
        status: GcSourceStatus::NonRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::NotApplicable,
        note: "not an OID — the hash-algorithm label of the tombstoned address",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "object_obliteration",
        column: "oid",
        status: GcSourceStatus::AntiRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::FailClosed,
        note: "intentional-absence tombstone — the opposite of a root",
    },
    GcObjectSource {
        origin: GcSourceOrigin::Column,
        location: "layer_path",
        column: "content_hash",
        status: GcSourceStatus::NonRoot,
        kind: GcStorageKind::SqliteColumn,
        schema: "migration-owned SQLite column",
        read_bound: "full table scan, one query per collection pass",
        corruption: GcCorruptionPolicy::NotApplicable,
        note: "identity of an on-disk overlay file; never enters the object store",
    },
];

/// W2 §C.4.3: roots from REGISTERED STORES that anchor object-store OIDs
/// outside refs/reflogs/sequencer rows — note blobs, undo view snapshots,
/// and AI capture checkpoints. Missing tables (pre-migration DBs) are
/// tolerated; unreadable rows or invalid OIDs fail CLOSED (callers must
/// never prune against a partial root set).
async fn collect_registered_store_roots<C: sea_orm::ConnectionTrait>(
    db: &C,
    storage: &ClientStorage,
    boundaries: &HashSet<ObjectHash>,
    reachable: &mut HashSet<ObjectHash>,
) -> CliResult<()> {
    use sea_orm::{DbBackend, Statement};
    let stmt_of = |sql: &'static str| Statement::from_string(DbBackend::Sqlite, sql.to_string());
    let missing_table = |err: &sea_orm::DbErr| err.to_string().contains("no such table");
    let is_null_oid = |oid: &str| !oid.is_empty() && oid.chars().all(|c| c == '0');

    /// How a source column's cells are interpreted.
    #[derive(Clone, Copy)]
    enum CellMode {
        /// A non-empty cell MUST be a valid OID (fail closed otherwise).
        StrictOid,
        /// The cell may hold a ref/branch NAME or an OID — only an
        /// OID-parsing value roots (names are anchored via repository refs).
        OidIfParses,
        /// An operation view manifest whose workspace snapshots must be
        /// expanded before the ordinary Git object walk.
        V2View,
    }
    type Source = (
        &'static str,
        &'static str,
        &'static [&'static str],
        CellMode,
    );
    let sources: &[Source] = &[
        (
            "notes",
            "SELECT blob FROM notes",
            &["blob"],
            CellMode::StrictOid,
        ),
        (
            "legacy_operation_view_ref",
            "SELECT target_oid FROM legacy_operation_view_ref",
            &["target_oid"],
            CellMode::StrictOid,
        ),
        (
            "legacy_operation_view",
            "SELECT head_target FROM legacy_operation_view",
            &["head_target"],
            CellMode::OidIfParses,
        ),
        (
            "legacy_operation_view_workspace",
            "SELECT pointer_value FROM legacy_operation_view_workspace",
            &["pointer_value"],
            CellMode::OidIfParses,
        ),
        (
            "operation",
            "SELECT pre_view_oid, post_view_oid FROM operation",
            &["pre_view_oid", "post_view_oid"],
            CellMode::V2View,
        ),
        (
            "operation",
            "SELECT predecessor_map_oid FROM operation",
            &["predecessor_map_oid"],
            CellMode::StrictOid,
        ),
        (
            "operation_journal",
            "SELECT pre_view_oid, target_view_oid FROM operation_journal",
            &["pre_view_oid", "target_view_oid"],
            CellMode::StrictOid,
        ),
        (
            "change_revision",
            "SELECT commit_oid FROM change_revision",
            &["commit_oid"],
            CellMode::StrictOid,
        ),
        (
            "change_predecessor",
            "SELECT successor_oid, predecessor_oid FROM change_predecessor",
            &["successor_oid", "predecessor_oid"],
            CellMode::StrictOid,
        ),
        (
            "agent_checkpoint",
            "SELECT parent_commit, tree_oid, metadata_blob_oid, traces_commit \
              FROM agent_checkpoint",
            &[
                "parent_commit",
                "tree_oid",
                "metadata_blob_oid",
                "traces_commit",
            ],
            CellMode::StrictOid,
        ),
        (
            "agent_coverage_claim",
            "SELECT traces_commit FROM agent_coverage_claim",
            &["traces_commit"],
            CellMode::StrictOid,
        ),
        (
            "agent_session",
            "SELECT parent_commit FROM agent_session",
            &["parent_commit"],
            CellMode::StrictOid,
        ),
        (
            "workspace_record",
            "SELECT base_commit FROM workspace_record WHERE base_commit IS NOT NULL \
              AND state IN ('provisioning', 'active', 'releasing', 'orphaned')",
            &["base_commit"],
            CellMode::StrictOid,
        ),
        (
            "agent_bridge_checkpoint",
            "SELECT target_oid FROM agent_bridge_checkpoint WHERE target_oid IS NOT NULL",
            &["target_oid"],
            CellMode::StrictOid,
        ),
    ];
    for (table, sql, columns, mode) in sources {
        match db.query_all_raw(stmt_of(sql)).await {
            Ok(rows) => {
                for row in rows {
                    for (idx, column) in columns.iter().enumerate() {
                        let cell: Option<String> = row.try_get_by_index(idx).map_err(|err| {
                            CliError::fatal(format!(
                                "{table}.{column} cannot be read while computing GC roots: {err}"
                            ))
                            .with_stable_code(StableErrorCode::RepoCorrupt)
                        })?;
                        let Some(raw) = cell else { continue };
                        let trimmed = raw.trim();
                        if trimmed.is_empty() || is_null_oid(trimmed) {
                            continue;
                        }
                        match mode {
                            CellMode::StrictOid => {
                                let hash = parse_object_hash(trimmed).ok_or_else(|| {
                                    CliError::fatal(format!(
                                        "{table}.{column} contains invalid object id \
                                          '{trimmed}' while computing GC roots"
                                    ))
                                    .with_stable_code(StableErrorCode::RepoCorrupt)
                                })?;
                                walk_reachable(&hash, storage, boundaries, reachable)?;
                            }
                            CellMode::OidIfParses => {
                                if let Some(hash) = parse_object_hash(trimmed) {
                                    walk_reachable(&hash, storage, boundaries, reachable)?;
                                }
                            }
                            CellMode::V2View => {
                                let hash = parse_object_hash(trimmed).ok_or_else(|| {
                                    CliError::fatal(format!(
                                        "{table}.{column} contains invalid object id \
                                          '{trimmed}' while computing GC roots"
                                    ))
                                    .with_stable_code(StableErrorCode::RepoCorrupt)
                                })?;
                                walk_reachable(&hash, storage, boundaries, reachable)?;
                                walk_v2_operation_view(&hash, storage, boundaries, reachable)?;
                            }
                        }
                    }
                }
            }
            Err(err) if missing_table(&err) => {}
            Err(err) => {
                return Err(
                    CliError::fatal(format!("failed to load {table} GC roots: {err}"))
                        .with_stable_code(StableErrorCode::IoReadFailed),
                );
            }
        }
    }

    // `metadata_kv` splits by ownership. Rows in the Libra-OWNED
    // `agent_traces_inflight` scope go through the dedicated prune-side
    // contract below (`list_live_traces_inflight_markers`): malformed rows
    // fail the listing CLOSED and every OID a live/cleanup-pending marker
    // names becomes a root. `run_gc` separately DEFERS pruning only for
    // LIVE ordinary markers (TTL-clamped); cleanup_pending rows list their
    // owned OIDs exhaustively, so rooting suffices and doctor retires them.
    // All other rows are arbitrary user metadata: scanned leniently
    // (non-JSON legal).
    let now_ms = chrono::Utc::now().timestamp_millis();
    let live_markers = crate::internal::ai::history::list_live_traces_inflight_markers(db, now_ms)
        .await
        .map_err(|err| {
            CliError::fatal(format!(
                "traces-inflight markers cannot be trusted while computing GC roots \
                      (destructive maintenance stops): {err:#}"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
    for marker in &live_markers {
        let value = serde_json::to_value(marker).map_err(|err| {
            CliError::fatal(format!(
                "traces-inflight marker cannot be re-encoded while computing GC roots: {err}"
            ))
            .with_stable_code(StableErrorCode::InternalInvariant)
        })?;
        walk_json_value_oids(value, storage, boundaries, reachable)?;
    }
    match db
        .query_all_raw(stmt_of(
            "SELECT scope, value FROM metadata_kv WHERE scope <> 'agent_traces_inflight'",
        ))
        .await
    {
        Ok(rows) => {
            for row in rows {
                let value: String = row.try_get_by_index(1).map_err(|err| {
                    CliError::fatal(format!(
                        "metadata_kv.value cannot be read while computing GC roots: {err}"
                    ))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
                })?;
                walk_payload_oids(&value, storage, boundaries, reachable)?;
            }
        }
        Err(err) if missing_table(&err) => {}
        Err(err) => {
            return Err(
                CliError::fatal(format!("failed to load metadata_kv GC roots: {err}"))
                    .with_stable_code(StableErrorCode::IoReadFailed),
            );
        }
    }
    Ok(())
}

/// W2 §C.4.3: roots from WORKTREE-LOCAL sidecars beyond the held autostash —
/// an in-progress merge/revert (`merge-state.json` / `revert-state.json`,
/// scanned leniently for OID-shaped strings like the sequencer payloads) and
/// `FETCH_HEAD` is deliberately EXCLUDED (§C.4.3 item 13).
/// Every registered worktree's gitdir is enumerated; unreadable sidecars
/// fail CLOSED.
fn collect_worktree_sidecar_roots(
    storage_root: &std::path::Path,
    storage: &ClientStorage,
    boundaries: &HashSet<ObjectHash>,
    reachable: &mut HashSet<ObjectHash>,
) -> CliResult<()> {
    // Each owner module exposes a TYPED extractor for its sidecar's
    // semantic OID fields. A generic "walk every OID-shaped JSON string"
    // scan is wrong in BOTH directions here: a branch name or conflict path
    // that happens to be 40 hex characters is not a reference (and failing
    // closed on its absence would block GC on a coincidence), while the
    // rewrites map's KEYS — real commit ids — are invisible to a
    // string-value walk. The schema knowledge stays with the owner.
    type SidecarExtractor =
        fn(&std::path::Path) -> Result<Option<Vec<(&'static str, String)>>, String>;
    const SIDECAR_EXTRACTORS: &[(&str, SidecarExtractor)] = &[
        (
            "merge-state.json",
            crate::command::merge::merge_state_gc_oids,
        ),
        (
            "revert-state.json",
            crate::command::revert::revert_state_gc_oids,
        ),
        (
            "rebase-aux.json",
            crate::command::rebase::rebase_aux_gc_oids,
        ),
        (
            "stash-branch-journal.json",
            crate::command::stash::stash_branch_journal_gc_oids,
        ),
    ];
    for gitdir in worktree_gitdir_roots(storage_root)? {
        for (name, extract) in SIDECAR_EXTRACTORS {
            let path = gitdir.join(name);
            // Corrupt/unreadable sidecars fail the prune CLOSED.
            let oids = extract(&gitdir).map_err(|error| {
                CliError::fatal(format!(
                    "sidecar GC root '{}' cannot be trusted: {error}",
                    path.display()
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            let Some(oids) = oids else { continue };
            for (field, oid) in oids {
                // §C.11 W2: every semantic OID of an in-progress operation
                // is MANDATORY reachability — invalid or missing objects
                // fail the prune closed rather than being skipped.
                let hash = parse_object_hash(oid.trim()).ok_or_else(|| {
                    CliError::fatal(format!(
                        "sidecar GC root '{}' field {field} holds an invalid object id \
                          '{oid}'",
                        path.display()
                    ))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
                })?;
                match storage.get_object_type(&hash) {
                    Ok(_) => walk_reachable(&hash, storage, boundaries, reachable)?,
                    Err(git_internal::errors::GitError::ObjectNotFound(_)) => {
                        return Err(CliError::fatal(format!(
                            "sidecar GC root '{}' field {field} names object {hash}, which \
                              does not exist — an in-progress operation's anchor is missing, \
                              so the prune stops rather than deleting its remaining ones",
                            path.display()
                        ))
                        .with_stable_code(StableErrorCode::RepoCorrupt));
                    }
                    Err(error) => {
                        return Err(CliError::fatal(format!(
                            "failed to probe sidecar GC root '{}' field {field} ({hash}): \
                              {error}",
                            path.display()
                        ))
                        .with_stable_code(StableErrorCode::IoReadFailed));
                    }
                }
            }
        }
        // §C.4.3 item 13: `FETCH_HEAD` is explicitly NOT a reachability
        // root. `fetch` records the advertised tip of every ref it
        // negotiated — INCLUDING refs that were already up to date and have
        // no local destination — so rooting it pins objects that nothing in
        // this repository references, indefinitely, on every fetch. Git does
        // not root it either. What keeps a just-fetched object alive between
        // its write and the ref update is the writer-vs-deleter grace window
        // (PRUNE_GRACE_SECS), not root registration; see the NonRoot entry
        // for it in GC_OBJECT_FILE_SOURCE_INVENTORY.
    }
    // `refs/replace/<original-oid>` files (repository-shared): the CONTENT
    // names the replacement object — anchored nowhere else. Both sides are
    // strict OIDs; malformed entries fail closed.
    let replace_dir = storage_root.join("refs/replace");
    match std::fs::read_dir(&replace_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|error| {
                    CliError::fatal(format!(
                        "failed to enumerate replace-ref GC roots in '{}': {error}",
                        replace_dir.display()
                    ))
                    .with_stable_code(StableErrorCode::IoReadFailed)
                })?;
                let path = entry.path();
                let name = entry.file_name();
                let original = parse_object_hash(&name.to_string_lossy()).ok_or_else(|| {
                    CliError::fatal(format!(
                        "replace ref '{}' has a non-OID name while computing GC roots",
                        path.display()
                    ))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
                })?;
                let content = std::fs::read_to_string(&path).map_err(|error| {
                    CliError::fatal(format!(
                        "failed to read replace-ref GC root '{}': {error}",
                        path.display()
                    ))
                    .with_stable_code(StableErrorCode::IoReadFailed)
                })?;
                let replacement = parse_object_hash(content.trim()).ok_or_else(|| {
                    CliError::fatal(format!(
                        "replace ref '{}' contains invalid object id '{}' while computing GC \
                          roots",
                        path.display(),
                        content.trim()
                    ))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
                })?;
                walk_reachable(&original, storage, boundaries, reachable)?;
                walk_reachable(&replacement, storage, boundaries, reachable)?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(CliError::fatal(format!(
                "failed to enumerate replace-ref GC roots in '{}': {error}",
                replace_dir.display()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed));
        }
    }

    // Review/investigate run manifests (`.libra/sessions/agent-runs/<id>/
    // manifest.json`) hold findings/attachment blob OIDs anchored nowhere
    // else, so they are MANDATORY reachability roots (§C.4.3): a manifest
    // this walk cannot read completely means roots it cannot enumerate, and
    // pruning past that deletes a run's findings.
    collect_agent_run_manifest_roots(storage_root, storage, boundaries, reachable)?;

    Ok(())
}

/// Mandatory `ReachabilityRoot` source: review/investigate run manifests.
///
/// Everything here is fail-CLOSED and bounded, because the previous lenient
/// scan had three ways to lose a run's only roots silently: a missing
/// `manifest.json` (which is exactly what an interrupted run leaves behind)
/// was skipped; the generic JSON walker skipped any OID whose object it could
/// not find; and both the directory enumeration and the JSON walk were
/// unbounded, so a large or adversarial tree could stall the prune with no
/// deadline.
fn collect_agent_run_manifest_roots(
    storage_root: &std::path::Path,
    storage: &ClientStorage,
    boundaries: &HashSet<ObjectHash>,
    reachable: &mut HashSet<ObjectHash>,
) -> CliResult<()> {
    /// Enough for any real installation; past this the scan is not bounded
    /// and prune must not proceed on a partial root set.
    const MAX_RUN_DIRS: usize = 50_000;
    /// A manifest is a small JSON document. A larger one is either corrupt
    /// or not ours.
    const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
    /// Attachment lists are human-scale.
    const MAX_ATTACHMENTS: usize = 10_000;

    let runs_dir = storage_root.join("sessions/agent-runs");
    let entries = match std::fs::read_dir(&runs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(CliError::fatal(format!(
                "failed to enumerate agent-run GC roots in '{}': {error}",
                runs_dir.display()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed));
        }
    };

    let mut scanned = 0usize;
    for entry in entries {
        let entry = entry.map_err(|error| {
            CliError::fatal(format!(
                "failed to enumerate agent-run GC roots in '{}': {error}",
                runs_dir.display()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        scanned += 1;
        if scanned > MAX_RUN_DIRS {
            return Err(CliError::fatal(format!(
                 "more than {MAX_RUN_DIRS} agent-run directories in '{}'; the mandatory root                  scan is no longer bounded, so pruning would proceed on a partial root set",
                 runs_dir.display()
             ))
             .with_stable_code(StableErrorCode::RepoStateInvalid)
             .with_hint("run `libra agent clean` to retire completed runs, then retry"));
        }
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest = dir.join("manifest.json");
        let text = match std::fs::metadata(&manifest) {
            Ok(meta) => {
                if meta.len() > MAX_MANIFEST_BYTES {
                    return Err(CliError::fatal(format!(
                         "agent-run manifest '{}' is {} bytes, past the {MAX_MANIFEST_BYTES}-byte                          cap; its roots cannot be enumerated safely",
                         manifest.display(),
                         meta.len()
                     ))
                     .with_stable_code(StableErrorCode::RepoCorrupt));
                }
                std::fs::read_to_string(&manifest).map_err(|error| {
                    CliError::fatal(format!(
                        "failed to read agent-run manifest GC root '{}': {error}",
                        manifest.display()
                    ))
                    .with_stable_code(StableErrorCode::IoReadFailed)
                })?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // An interrupted run leaves its directory without a manifest
                // while its blobs already exist. Treating that as "no roots"
                // is what deletes them — and AGE does not make it safe: an
                // abandoned run's blobs are exactly as unlisted after a day
                // as after a minute, so the old grace-bounded `continue`
                // turned "I cannot enumerate this run's roots" into "this run
                // has none" for every directory older than the window.
                //
                // A mandatory root that cannot be read fails the walk closed.
                // Retiring the directory is an explicit user action
                // (`libra agent clean`, which takes manifest-less runs past
                // the retention cutoff), so this can never wedge pruning
                // forever — it just refuses to guess.
                return Err(CliError::fatal(format!(
                    "agent-run directory '{}' has no manifest, so the objects it may still own \
                      cannot be enumerated; pruning would proceed on a partial root set",
                    dir.display()
                ))
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint(
                    "re-run once the agent run completes; if the run was interrupted and will \
                      not resume, retire it with `libra agent clean` and try again",
                ));
            }
            Err(error) => {
                return Err(CliError::fatal(format!(
                    "failed to stat agent-run manifest GC root '{}': {error}",
                    manifest.display()
                ))
                .with_stable_code(StableErrorCode::IoReadFailed));
            }
        };

        let document: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
            CliError::fatal(format!(
                "agent-run manifest '{}' holds corrupt JSON while computing GC roots: {error}",
                manifest.display()
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        // Well-formed JSON is not a well-formed manifest. `[]`, `null` and
        // `"…"` all parse, and every `document.get(...)` below then returns
        // `None` — which reads as "this run declares no roots" and prunes the
        // blob it owned. The shape is part of the contract.
        if !document.is_object() {
            return Err(CliError::fatal(format!(
                "agent-run manifest '{}' is not a JSON object, so its roots cannot be read",
                manifest.display()
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
            .with_hint(
                "restore or delete the manifest (`libra agent doctor` reports what a run \
                  should contain), then re-run",
            ));
        }

        // Structural, not a generic scan: these two fields ARE the contract.
        let mut oids: Vec<String> = Vec::new();
        // A FINALIZED run always writes the key, even when there were no
        // findings (then it is null). A finished manifest missing the key
        // entirely has lost a root, not declined to have one — and the blob
        // it pointed at is exactly what a prune would take next.
        let finalized = document
            .get("terminal_state")
            .is_some_and(|state| !state.is_null());
        match document.get("findings_oid") {
            Some(value) if value.is_null() => {}
            Some(value) => {
                let oid = value.as_str().ok_or_else(|| {
                    CliError::fatal(format!(
                        "agent-run manifest '{}' has a non-string findings_oid",
                        manifest.display()
                    ))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
                })?;
                oids.push(oid.to_string());
            }
            None if finalized => {
                return Err(CliError::fatal(format!(
                     "agent-run manifest '{}' is finalized but has no findings_oid field; its                      evidence blob has no root and pruning would take it",
                     manifest.display()
                 ))
                 .with_stable_code(StableErrorCode::RepoCorrupt)
                 .with_hint("run `libra agent doctor` to reconcile the run manifests"));
            }
            None => {}
        }
        if let Some(value) = document.get("manual_attach")
            && !value.is_null()
        {
            let list = value.as_array().ok_or_else(|| {
                CliError::fatal(format!(
                    "agent-run manifest '{}' has a non-array manual_attach",
                    manifest.display()
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            if list.len() > MAX_ATTACHMENTS {
                return Err(CliError::fatal(format!(
                     "agent-run manifest '{}' lists {} attachments, past the                      {MAX_ATTACHMENTS} cap",
                     manifest.display(),
                     list.len()
                 ))
                 .with_stable_code(StableErrorCode::RepoCorrupt));
            }
            for item in list {
                // An attachment entry exists BECAUSE something was attached.
                // Missing, null or non-string here is corruption — skipping
                // it would quietly surrender that attachment's only root.
                let oid = item
                     .get("oid")
                     .and_then(|value| value.as_str())
                     .ok_or_else(|| {
                         CliError::fatal(format!(
                             "agent-run manifest '{}' has a manual_attach entry without a usable                              oid; its attachment has no root and pruning would take it",
                             manifest.display()
                         ))
                         .with_stable_code(StableErrorCode::RepoCorrupt)
                         .with_hint("run `libra agent doctor` to reconcile the run manifests")
                     })?;
                oids.push(oid.to_string());
            }
        }

        for oid in oids {
            let hash = parse_object_hash(&oid).ok_or_else(|| {
                CliError::fatal(format!(
                    "agent-run manifest '{}' names '{oid}', which is not a valid object id",
                    manifest.display()
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            // A mandatory root whose object is ABSENT is not "nothing to
            // keep alive" — it means the run's evidence is already gone, and
            // continuing to prune would compound the damage.
            if !storage.exist(&hash) {
                return Err(CliError::fatal(format!(
                    "agent-run manifest '{}' names object {hash}, which is not in the store",
                    manifest.display()
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("run `libra agent doctor` to reconcile the run manifests"));
            }
            walk_reachable(&hash, storage, boundaries, reachable)?;
        }
    }

    Ok(())
}

async fn collect_sequencer_state_roots<C: sea_orm::ConnectionTrait>(
    db: &C,
    storage: &ClientStorage,
    boundaries: &HashSet<ObjectHash>,
    reachable: &mut HashSet<ObjectHash>,
) -> CliResult<()> {
    use sea_orm::{DbBackend, Statement};

    let is_null_oid = |oid: &str| !oid.is_empty() && oid.chars().all(|c| c == '0');
    let stmt_of = |sql: &'static str| Statement::from_string(DbBackend::Sqlite, sql.to_string());
    let missing_table = |err: &sea_orm::DbErr| err.to_string().contains("no such table");

    // Parse one structured OID cell (fail-closed) and walk it.
    fn walk_cell(
        table: &str,
        column: &str,
        raw: &str,
        storage: &ClientStorage,
        boundaries: &HashSet<ObjectHash>,
        reachable: &mut HashSet<ObjectHash>,
    ) -> CliResult<()> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let hash = parse_object_hash(trimmed).ok_or_else(|| {
            CliError::fatal(format!(
                "{table}.{column} contains invalid object id '{trimmed}' while computing GC \
                  roots"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        walk_reachable(&hash, storage, boundaries, reachable)
    }

    // ── sequence_state: head_orig / current_oid / todo (newline list) /
    //    payload (lenient JSON scan) ─────────────────────────────────────────
    match db
        .query_all_raw(stmt_of(
            "SELECT worktree_id, head_orig, current_oid, todo, payload FROM sequence_state",
        ))
        .await
    {
        Ok(rows) => {
            for row in rows {
                let head_orig: String = row.try_get_by_index(1).map_err(state_row_error)?;
                let current_oid: String = row.try_get_by_index(2).map_err(state_row_error)?;
                let todo: String = row.try_get_by_index(3).map_err(state_row_error)?;
                let payload: String = row.try_get_by_index(4).map_err(state_row_error)?;
                for cell in [&head_orig, &current_oid] {
                    if !is_null_oid(cell) {
                        walk_cell(
                            "sequence_state",
                            "oid",
                            cell,
                            storage,
                            boundaries,
                            reachable,
                        )?;
                    }
                }
                for line in todo.lines() {
                    walk_cell(
                        "sequence_state",
                        "todo",
                        line,
                        storage,
                        boundaries,
                        reachable,
                    )?;
                }
                walk_payload_oids(&payload, storage, boundaries, reachable)?;
            }
        }
        Err(err) if missing_table(&err) => {}
        Err(err) => {
            return Err(
                CliError::fatal(format!("failed to load sequence_state GC roots: {err}"))
                    .with_stable_code(StableErrorCode::IoReadFailed),
            );
        }
    }

    // ── rebase_state: onto / orig_head / current_head / stopped_sha +
    //    todo / done (newline lists) ─────────────────────────────────────────
    match db
        .query_all_raw(stmt_of(
            "SELECT worktree_id, onto, orig_head, current_head, todo, done, stopped_sha \
              FROM rebase_state",
        ))
        .await
    {
        Ok(rows) => {
            for row in rows {
                for (idx, column) in [(1, "onto"), (2, "orig_head"), (3, "current_head")] {
                    let cell: String = row.try_get_by_index(idx).map_err(state_row_error)?;
                    walk_cell(
                        "rebase_state",
                        column,
                        &cell,
                        storage,
                        boundaries,
                        reachable,
                    )?;
                }
                for (idx, column) in [(4, "todo"), (5, "done")] {
                    let list: String = row.try_get_by_index(idx).map_err(state_row_error)?;
                    for line in list.lines() {
                        walk_cell("rebase_state", column, line, storage, boundaries, reachable)?;
                    }
                }
                let stopped: Option<String> = row.try_get_by_index(6).map_err(state_row_error)?;
                if let Some(stopped) = stopped {
                    walk_cell(
                        "rebase_state",
                        "stopped_sha",
                        &stopped,
                        storage,
                        boundaries,
                        reachable,
                    )?;
                }
            }
        }
        Err(err) if missing_table(&err) => {}
        Err(err) => {
            return Err(
                CliError::fatal(format!("failed to load rebase_state GC roots: {err}"))
                    .with_stable_code(StableErrorCode::IoReadFailed),
            );
        }
    }

    // ── bisect_state: orig_head / bad / current + good / skipped (JSON) ─────
    match db
        .query_all_raw(stmt_of(
            "SELECT orig_head, bad, good, current, skipped FROM bisect_state",
        ))
        .await
    {
        Ok(rows) => {
            for row in rows {
                let orig_head: String = row.try_get_by_index(0).map_err(state_row_error)?;
                walk_cell(
                    "bisect_state",
                    "orig_head",
                    &orig_head,
                    storage,
                    boundaries,
                    reachable,
                )?;
                for (idx, column) in [(1, "bad"), (3, "current")] {
                    let cell: Option<String> =
                        row.try_get_by_index(idx).map_err(state_row_error)?;
                    if let Some(cell) = cell {
                        walk_cell(
                            "bisect_state",
                            column,
                            &cell,
                            storage,
                            boundaries,
                            reachable,
                        )?;
                    }
                }
                for (idx, column) in [(2, "good"), (4, "skipped")] {
                    let json: String = row.try_get_by_index(idx).map_err(state_row_error)?;
                    let oids: Vec<String> = serde_json::from_str(&json).map_err(|error| {
                        CliError::fatal(format!(
                            "bisect_state.{column} contains invalid JSON while computing GC \
                              roots: {error}"
                        ))
                        .with_stable_code(StableErrorCode::RepoCorrupt)
                    })?;
                    for oid in oids {
                        walk_cell("bisect_state", column, &oid, storage, boundaries, reachable)?;
                    }
                }
            }
        }
        Err(err) if missing_table(&err) => {}
        Err(err) => {
            return Err(
                CliError::fatal(format!("failed to load bisect_state GC roots: {err}"))
                    .with_stable_code(StableErrorCode::IoReadFailed),
            );
        }
    }

    Ok(())
}

fn state_row_error(err: sea_orm::DbErr) -> CliError {
    CliError::fatal(format!(
        "sequencer state row cannot be read while computing GC roots: {err}"
    ))
    .with_stable_code(StableErrorCode::RepoCorrupt)
}

/// Lenient payload scan: walk every JSON string value that parses as an OID
/// AND exists in the object store. Payload content is op-specific (e.g. the
/// cherry-pick commit-modifier), so unlike the structured columns a
/// non-OID-looking string is simply skipped rather than failing the run.
fn walk_payload_oids(
    payload: &str,
    storage: &ClientStorage,
    boundaries: &HashSet<ObjectHash>,
    reachable: &mut HashSet<ObjectHash>,
) -> CliResult<()> {
    if payload.trim().is_empty() {
        return Ok(());
    }
    // LENIENT: non-JSON is legal for this caller class (arbitrary user
    // values). Libra-OWNED sidecars go through their owners' TYPED
    // extractors in `collect_worktree_sidecar_roots` instead.
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Ok(());
    };
    walk_json_value_oids(value, storage, boundaries, reachable)
}

fn walk_json_value_oids(
    value: serde_json::Value,
    storage: &ClientStorage,
    boundaries: &HashSet<ObjectHash>,
    reachable: &mut HashSet<ObjectHash>,
) -> CliResult<()> {
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            serde_json::Value::String(text) => {
                if let Some(hash) = parse_object_hash(text.trim()) {
                    // Fallible probe: an OID-shaped string that names no
                    // object is normal for a scan, but a PROBE failure must
                    // fail closed rather than pass as absence.
                    match storage.get_object_type(&hash) {
                        Ok(_) => walk_reachable(&hash, storage, boundaries, reachable)?,
                        Err(git_internal::errors::GitError::ObjectNotFound(_)) => {}
                        Err(error) => {
                            return Err(CliError::fatal(format!(
                                "failed to probe JSON-scanned GC root {hash}: {error}"
                            ))
                            .with_stable_code(StableErrorCode::IoReadFailed));
                        }
                    }
                }
            }
            serde_json::Value::Array(items) => stack.extend(items),
            serde_json::Value::Object(map) => stack.extend(map.into_values()),
            _ => {}
        }
    }
    Ok(())
}

/// plan-20260714 §C.4.3 `Boundary`: the shallow stop-list for the roots walk.
///
/// A shallow clone deliberately lacks its boundary commits' parents. Without
/// this the walk follows `parent_commit_ids` off the end of the graft and
/// reports the absent parent as repository corruption — so `gc`, `repack`
/// and `prune` all failed outright on every shallow clone. Malformed shallow
/// metadata fails closed rather than silently degrading to "no boundaries",
/// because that reading would resume the corruption report.
fn shallow_boundaries(storage_root: &Path) -> CliResult<HashSet<ObjectHash>> {
    let raw = crate::command::fetch::read_shallow_boundaries_at(&storage_root.join("shallow"))
        .map_err(|error| {
            CliError::fatal(format!(
                "shallow metadata cannot be trusted before pruning: {error}"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
    raw.iter()
        .map(|oid| {
            parse_object_hash(oid).ok_or_else(|| {
                CliError::fatal(format!(
                    "shallow metadata lists '{oid}', which is not a valid object id"
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })
        })
        .collect()
}

/// Walk object references recursively, adding all transitive dependencies.
///
/// `boundaries` is the shallow stop-list (see [`shallow_boundaries`]): a
/// commit listed there is itself preserved, but the walk does not descend
/// into its parents, which this clone does not have.
fn walk_reachable(
    hash: &ObjectHash,
    storage: &ClientStorage,
    boundaries: &HashSet<ObjectHash>,
    reachable: &mut HashSet<ObjectHash>,
) -> CliResult<()> {
    if !reachable.insert(*hash) {
        return Ok(()); // Already visited
    }

    let obj_type = storage.get_object_type(hash).map_err(|error| {
        CliError::fatal(format!(
            "reachable object {hash} cannot be read while computing GC roots: {error}"
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;

    match obj_type {
        ObjectType::Commit => {
            let commit = load_object_raw::<Commit>(hash).map_err(|error| {
                CliError::fatal(format!(
                    "reachable commit {hash} is corrupt while computing GC roots: {error}"
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            walk_reachable(&commit.tree_id, storage, boundaries, reachable)?;
            // §C.4.3 Boundary: STOP here on a shallow graft. The commit
            // itself stays reachable (it is in `reachable` already); its
            // parents are not in this clone and must not be demanded.
            if !boundaries.contains(hash) {
                for parent in &commit.parent_commit_ids {
                    walk_reachable(parent, storage, boundaries, reachable)?;
                }
            }
        }
        ObjectType::Tree => {
            let tree = load_object_raw::<Tree>(hash).map_err(|error| {
                CliError::fatal(format!(
                    "reachable tree {hash} is corrupt while computing GC roots: {error}"
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            for item in &tree.tree_items {
                walk_reachable(&item.id, storage, boundaries, reachable)?;
            }
        }
        ObjectType::Tag => {
            let tag = load_object_raw::<GitTag>(hash).map_err(|error| {
                CliError::fatal(format!(
                    "reachable tag {hash} is corrupt while computing GC roots: {error}"
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            walk_reachable(&tag.object_hash, storage, boundaries, reachable)?;
        }
        ObjectType::Blob => {}
        other => {
            return Err(CliError::fatal(format!(
                "reachable object {hash} has unsupported stored type {other:?}"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt));
        }
    }

    Ok(())
}

/// Expand a v2 operation view manifest after its blob has been rooted.
///
/// Git's generic blob walk intentionally treats blobs as leaves. Operation
/// manifests are a typed exception: the repository view blob names workspace
/// snapshot manifests, and those snapshots name trees/blobs/facets that must
/// remain live as well. The typed recursive closure check fails closed before
/// any prune can act on a partial graph.
fn walk_v2_operation_view(
    hash: &ObjectHash,
    storage: &ClientStorage,
    boundaries: &HashSet<ObjectHash>,
    reachable: &mut HashSet<ObjectHash>,
) -> CliResult<()> {
    let bytes = storage.get(hash).map_err(|error| {
        CliError::fatal(format!(
            "operation view manifest {hash} cannot be read while computing GC roots: {error}"
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    let view =
        crate::internal::operation::RepoViewV2::from_canonical_bytes(&bytes).map_err(|error| {
            CliError::fatal(format!(
                "operation view manifest {hash} is invalid while computing GC roots: {error}"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
    view.validate_recursive_closure(|oid| storage.get(oid).ok())
        .map_err(|error| {
            CliError::fatal(format!(
                "operation view manifest {hash} has an incomplete closure while computing GC roots: {error}"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;

    for root in view.roots() {
        walk_reachable(&root, storage, boundaries, reachable)?;
    }
    for workspace_oid in view.workspaces.values() {
        let snapshot_bytes = storage.get(workspace_oid).map_err(|error| {
            CliError::fatal(format!(
                "workspace snapshot {workspace_oid} cannot be read from operation view {hash}: {error}"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        let snapshot =
            crate::internal::operation::WorkspaceSnapshotV2::from_canonical_bytes(&snapshot_bytes)
                .map_err(|error| {
                    CliError::fatal(format!(
                "workspace snapshot {workspace_oid} in operation view {hash} is invalid: {error}"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
                })?;
        for root in snapshot.roots() {
            walk_reachable(&root, storage, boundaries, reachable)?;
        }
    }
    Ok(())
}

/// List all loose objects in the repository, returning (hash, path) pairs.
pub(crate) fn list_loose_objects(repo_path: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let objects_dir = repo_path.join("objects");
    let mut result = Vec::new();

    for entry in fs::read_dir(&objects_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if dir_name.len() != 2 || dir_name == "pack" || dir_name == "info" {
            continue;
        }

        for sub in fs::read_dir(&path)? {
            let sub = sub?;
            let sub_path = sub.path();
            if sub_path.is_file() {
                let Some(file_name) = sub_path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let full_hash = format!("{dir_name}{file_name}");
                result.push((full_hash, sub_path));
            }
        }
    }

    Ok(result)
}

/// Parse a hex string into an ObjectHash.
///
/// The hash kind is inferred from the decoded byte length (20 → SHA-1, 32 →
/// SHA-256) rather than from `ObjectHash::from_bytes`, which reads the
/// thread-local hash kind and would reject a SHA-256 id (or misread it) if this
/// runs on a Tokio worker thread that never had the repository's kind set.
pub(crate) fn parse_object_hash(hex_str: &str) -> Option<ObjectHash> {
    let bytes = hex::decode(hex_str).ok()?;
    match bytes.len() {
        20 => Some(ObjectHash::Sha1(bytes.try_into().ok()?)),
        32 => Some(ObjectHash::Sha256(bytes.try_into().ok()?)),
        _ => None,
    }
}

/// Remove empty directories under the given path.
fn cleanup_empty_dirs(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir()
            && path.file_name() != Some("pack".as_ref())
            && path.file_name() != Some("info".as_ref())
            && let Ok(mut iter) = fs::read_dir(&path)
            && iter.next().is_none()
        {
            let _ = fs::remove_dir(&path);
        }
    }
    Ok(())
}

/// Collect all refs under `refs_dir`, storing them as (ref_name, hash) pairs.
fn collect_refs(base: &Path, current: &Path, refs: &mut HashMap<String, String>) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_refs(base, &path, refs)?;
        } else if path.is_file() {
            let hash = fs::read_to_string(&path)?.trim().to_string();
            let relative = path.strip_prefix(base).unwrap_or(&path);
            let name = relative.to_string_lossy().replace('\\', "/");
            if !hash.is_empty() {
                refs.insert(name, hash);
            }
        }
    }
    Ok(())
}

/// Remove loose ref files that have been packed.
#[allow(clippy::only_used_in_recursion)]
fn remove_packed_refs(base: &Path, current: &Path, count: &mut usize) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            remove_packed_refs(base, &path, count)?;
            // Remove empty directory
            if let Ok(mut iter) = fs::read_dir(&path)
                && iter.next().is_none()
            {
                let _ = fs::remove_dir(&path);
            }
        } else if path.is_file() {
            fs::remove_file(&path)?;
            *count += 1;
        }
    }
    Ok(())
}

/// Print an informational message unless output is quiet or JSON mode.
fn info_println(output: &OutputConfig, message: &str) {
    if !output.quiet && !output.is_json() {
        println!("{message}");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The 4 MiB ledger cap is enforced on the way OUT, not only on the way
    /// in. Checking it only on read lets THIS run write a ledger every later
    /// run then refuses to read — the quarantine clock stops for a file this
    /// code created, and nothing outside the repository caused it.
    #[test]
    fn an_oversized_ledger_is_refused_without_replacing_the_readable_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gc-prune-candidates.json");
        std::fs::write(&path, "{}").expect("seed a readable ledger");

        // ~90k entries is past the cap by construction: each entry is a
        // 40-hex key plus a timestamp, so 200k entries cannot fit in 4 MiB.
        let mut ledger = std::collections::HashMap::new();
        for i in 0..200_000u64 {
            ledger.insert(format!("{i:040x}"), i);
        }
        let error = write_prune_candidate_ledger(&path, &ledger)
            .expect_err("a ledger past the cap must be refused");
        assert_eq!(error.stable_code(), StableErrorCode::RepoStateInvalid);
        assert_eq!(
            std::fs::read_to_string(&path).expect("ledger still readable"),
            "{}",
            "the previous, readable ledger must be left in place"
        );
        // And what was left behind is still loadable, so the next run works.
        assert!(
            read_prune_candidate_ledger(&path).is_ok(),
            "refusing to write must not leave an unreadable ledger"
        );
    }

    /// A ledger that fits is written normally — the cap must not be a
    /// blanket refusal.
    #[test]
    fn a_ledger_within_the_cap_is_written() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gc-prune-candidates.json");
        let mut ledger = std::collections::HashMap::new();
        ledger.insert("a".repeat(40), 42u64);
        write_prune_candidate_ledger(&path, &ledger).expect("a small ledger is written");
        let loaded = read_prune_candidate_ledger(&path).expect("read back");
        assert_eq!(loaded.get(&"a".repeat(40)).copied(), Some(42));
    }

    #[test]
    fn test_parse_object_hash_valid() {
        let hash = "abc123def456789012345678901234567890abcd";
        let result = parse_object_hash(hash);
        assert!(result.is_some());
    }

    #[test]
    fn test_parse_object_hash_invalid_hex() {
        let hash = "xyz123";
        let result = parse_object_hash(hash);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_object_hash_empty() {
        let result = parse_object_hash("");
        assert!(result.is_none());
    }

    #[test]
    fn test_task_display() {
        assert_eq!(MaintenanceTask::Gc.to_string(), "gc");
        assert_eq!(MaintenanceTask::LooseObjects.to_string(), "loose-objects");
        assert_eq!(MaintenanceTask::PackRefs.to_string(), "pack-refs");
        assert_eq!(
            MaintenanceTask::IncrementalRepack.to_string(),
            "incremental-repack"
        );
        assert_eq!(MaintenanceTask::CommitGraph.to_string(), "commit-graph");
        assert_eq!(MaintenanceTask::Prefetch.to_string(), "prefetch");
    }

    #[test]
    fn test_cleanup_empty_dirs_nonexistent() {
        // Should not panic on non-existent directory
        let temp = tempfile::tempdir().unwrap();
        let result = cleanup_empty_dirs(temp.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_task_result_serialize() {
        let result = TaskResult {
            task: "gc".to_string(),
            success: true,
            objects_removed: 5,
            objects_packed: 0,
            refs_packed: 0,
            packs_repacked: 0,
            object_index_rows_removed: 0,
            message: "removed 5 objects".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("gc"));
        assert!(json.contains("removed 5 objects"));
    }

    #[test]
    fn test_maintenance_status_output_serialize() {
        let status = MaintenanceStatusOutput {
            registered: true,
            schedule: Some("hourly".to_string()),
            last_run: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("hourly"));
    }

    #[test]
    fn scheduler_entry_write_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Path::new("/tmp/example-repo");
        let exe = Path::new("/usr/local/bin/libra");
        let label = scheduler_label(repo);

        // Label is deterministic for a given repo path.
        assert_eq!(scheduler_label(repo), label);
        assert!(label.starts_with("tools.libra.maintenance."));

        let path = write_scheduler_entry(dir.path(), &label, exe, repo, "daily").unwrap();
        assert!(path.exists(), "scheduler entry should be written");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("maintenance") && content.contains("/tmp/example-repo"),
            "entry should invoke maintenance in the repo: {content}"
        );
        if cfg!(target_os = "macos") {
            assert_eq!(path.extension().unwrap(), "plist");
            assert!(content.contains("86400"), "daily => 86400s StartInterval");
        } else {
            assert!(content.contains("0 0 * * *"), "daily => daily cron expr");
        }

        // Removal is idempotent.
        assert!(remove_scheduler_entry(dir.path(), &label).unwrap());
        assert!(!path.exists());
        assert!(!remove_scheduler_entry(dir.path(), &label).unwrap());
    }

    #[test]
    fn commit_graph_build_roundtrip() {
        use std::str::FromStr;

        use git_internal::internal::object::signature::Signature;

        git_internal::hash::set_hash_kind(HashKind::Sha1);

        let tree = ObjectHash::from_str("1111111111111111111111111111111111111111").unwrap();
        let sig =
            Signature::from_data(b"committer t <t@example.com> 1000000000 +0000".to_vec()).unwrap();
        let root = Commit::new(sig.clone(), sig.clone(), tree, vec![], "root");
        let root_id = root.id;
        let child = Commit::new(sig.clone(), sig.clone(), tree, vec![root_id], "child");
        let child_id = child.id;

        let mut commits = HashMap::new();
        commits.insert(root_id, root);
        commits.insert(child_id, child);

        let bytes = build_commit_graph(&commits).expect("commit-graph bytes");

        // Header: signature + version 1 + hash version 1 + 3 chunks + 0 base graphs.
        assert_eq!(&bytes[0..4], b"CGPH");
        assert_eq!(&bytes[4..8], &[1, 1, 3, 0]);

        // Chunk TOC offsets (OIDF immediately follows the 8-byte header + 48-byte TOC).
        let oidf_off = u64::from_be_bytes(bytes[12..20].try_into().unwrap()) as usize;
        assert_eq!(oidf_off, 56);
        let cdat_off = u64::from_be_bytes(bytes[36..44].try_into().unwrap()) as usize;

        // Final fanout bucket equals the commit count.
        let last = oidf_off + 255 * 4;
        assert_eq!(
            u32::from_be_bytes(bytes[last..last + 4].try_into().unwrap()),
            2
        );

        // Trailing SHA-1 checksum covers everything before it.
        let body = &bytes[..bytes.len() - 20];
        assert_eq!(&sha1::Sha1::digest(body)[..], &bytes[bytes.len() - 20..]);

        // Verify CDAT parent linkage + generation numbers per sorted position.
        let mut oids: Vec<ObjectHash> = commits.keys().copied().collect();
        oids.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));
        let root_pos = oids.iter().position(|o| *o == root_id).unwrap() as u32;
        let stride = 20 + 16; // tree + parent1 + parent2 + gen/time
        for (i, o) in oids.iter().enumerate() {
            let base = cdat_off + i * stride;
            let p1 = u32::from_be_bytes(bytes[base + 20..base + 24].try_into().unwrap());
            let genhi = u32::from_be_bytes(bytes[base + 28..base + 32].try_into().unwrap());
            let time = u32::from_be_bytes(bytes[base + 32..base + 36].try_into().unwrap());
            assert_eq!(time, 1_000_000_000);
            if *o == child_id {
                assert_eq!(p1, root_pos, "child's first parent points at root");
                assert_eq!(genhi >> 2, 2, "child generation is 2");
            } else {
                assert_eq!(p1, 0x7000_0000, "root has no parent (GRAPH_PARENT_NONE)");
                assert_eq!(genhi >> 2, 1, "root generation is 1");
            }
        }
    }

    #[test]
    fn commit_graph_build_writes_octopus_edge_chunk() {
        use std::str::FromStr;

        use git_internal::internal::object::signature::Signature;

        git_internal::hash::set_hash_kind(HashKind::Sha1);

        let tree = ObjectHash::from_str("2222222222222222222222222222222222222222").unwrap();
        let sig =
            Signature::from_data(b"committer t <t@example.com> 1000000000 +0000".to_vec()).unwrap();
        // Three distinct roots (distinct messages → distinct ids) and a merge
        // that has all three as parents (an octopus merge, >2 parents).
        let p1 = Commit::new(sig.clone(), sig.clone(), tree, vec![], "p1");
        let p2 = Commit::new(sig.clone(), sig.clone(), tree, vec![], "p2");
        let p3 = Commit::new(sig.clone(), sig.clone(), tree, vec![], "p3");
        let (p1id, p2id, p3id) = (p1.id, p2.id, p3.id);
        let merge = Commit::new(
            sig.clone(),
            sig.clone(),
            tree,
            vec![p1id, p2id, p3id],
            "octopus",
        );
        let merge_id = merge.id;

        let mut commits = HashMap::new();
        for c in [p1, p2, p3, merge] {
            commits.insert(c.id, c);
        }
        let bytes = build_commit_graph(&commits).expect("commit-graph bytes");

        // Octopus merges add the EDGE chunk, so the header now has 4 chunks.
        assert_eq!(&bytes[0..4], b"CGPH");
        assert_eq!(&bytes[4..8], &[1, 1, 4, 0]);

        // The TOC (after the 8-byte header) lists OIDF/OIDL/CDAT/EDGE; read the
        // CDAT and EDGE offsets from it.
        let chunk_off = |id: &[u8; 4]| -> usize {
            let mut i = 8;
            loop {
                let tag = &bytes[i..i + 4];
                let off = u64::from_be_bytes(bytes[i + 4..i + 12].try_into().unwrap()) as usize;
                if tag == id {
                    return off;
                }
                assert_ne!(tag, &[0, 0, 0, 0], "chunk {id:?} present");
                i += 12;
            }
        };
        let cdat_off = chunk_off(b"CDAT");
        let edge_off = chunk_off(b"EDGE");

        let mut oids: Vec<ObjectHash> = commits.keys().copied().collect();
        oids.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));
        let position = |id: &ObjectHash| oids.iter().position(|o| o == id).unwrap() as u32;
        let merge_idx = oids.iter().position(|o| *o == merge_id).unwrap();

        // The merge's CDAT entry: first parent is p1's position; the second slot
        // has the EXTRA_EDGES_NEEDED high bit set, with an index into EDGE.
        let stride = 20 + 16;
        let base = cdat_off + merge_idx * stride;
        let mp1 = u32::from_be_bytes(bytes[base + 20..base + 24].try_into().unwrap());
        let mp2 = u32::from_be_bytes(bytes[base + 24..base + 28].try_into().unwrap());
        assert_eq!(mp1, position(&p1id), "octopus first parent is p1");
        assert_eq!(mp2 & 0x8000_0000, 0x8000_0000, "EXTRA_EDGES_NEEDED bit set");
        let edge_index = (mp2 & 0x7fff_ffff) as usize;

        // The EDGE chunk holds parents 2..N (p2, p3); the last entry has the
        // GRAPH_LAST_EDGE high bit set.
        let e0 = u32::from_be_bytes(
            bytes[edge_off + edge_index * 4..edge_off + edge_index * 4 + 4]
                .try_into()
                .unwrap(),
        );
        let e1 = u32::from_be_bytes(
            bytes[edge_off + (edge_index + 1) * 4..edge_off + (edge_index + 1) * 4 + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            e0,
            position(&p2id),
            "first extra edge is p2 (no terminator)"
        );
        assert_eq!(e1 & 0x7fff_ffff, position(&p3id), "second extra edge is p3");
        assert_eq!(e1 & 0x8000_0000, 0x8000_0000, "last extra edge terminated");

        // Trailer still covers the whole body including the EDGE chunk.
        let body = &bytes[..bytes.len() - 20];
        assert_eq!(&sha1::Sha1::digest(body)[..], &bytes[bytes.len() - 20..]);
    }

    #[test]
    fn commit_graph_build_handles_sha256_repository() {
        use git_internal::internal::object::signature::Signature;

        let sig =
            Signature::from_data(b"committer t <t@example.com> 1000000000 +0000".to_vec()).unwrap();
        // Craft SHA-256 OIDs directly (overriding the ids/tree/parents) so the
        // graph is built for a SHA-256 repository without touching the global
        // hash kind — `build_commit_graph` keys everything off the OID width and
        // kind, not the process-wide setting.
        let sha256 = |b: u8| ObjectHash::Sha256([b; 32]);
        let mut root = Commit::new(sig.clone(), sig.clone(), sha256(0x10), vec![], "root");
        root.id = sha256(0xA1);
        let mut child = Commit::new(
            sig.clone(),
            sig.clone(),
            sha256(0x11),
            vec![root.id],
            "child",
        );
        child.id = sha256(0xB2);

        let mut commits = HashMap::new();
        commits.insert(root.id, root);
        commits.insert(child.id, child);
        let bytes = build_commit_graph(&commits).expect("commit-graph bytes");

        // Header hash version is 2 (SHA-256); chunk count is 3 (no octopus).
        assert_eq!(&bytes[0..4], b"CGPH");
        assert_eq!(&bytes[4..8], &[1, 2, 3, 0]);

        // OIDL stores 32-byte object ids; the OIDF chunk follows the header+TOC.
        let oidf_off = u64::from_be_bytes(bytes[12..20].try_into().unwrap()) as usize;
        let oidl_off = u64::from_be_bytes(bytes[24..32].try_into().unwrap()) as usize;
        assert_eq!(
            oidl_off - (oidf_off + 1024),
            0,
            "OIDL right after OIDF+fanout"
        );
        let cdat_off = u64::from_be_bytes(bytes[36..44].try_into().unwrap()) as usize;
        assert_eq!(cdat_off - oidl_off, 2 * 32, "two 32-byte OIDs in OIDL");

        // Trailer is the SHA-256 of the body (32 bytes), not SHA-1.
        let body = &bytes[..bytes.len() - 32];
        assert_eq!(&sha2::Sha256::digest(body)[..], &bytes[bytes.len() - 32..]);
    }

    /// plan-20260714 W2 §C.4.3: EVERY file production code can create under a
    /// repository's storage roots is classified — as an object source in
    /// [`GC_OBJECT_FILE_SOURCE_INVENTORY`], or in the explicit
    /// `NOT_AN_OBJECT_SOURCE` list below.
    ///
    /// The inventory's DB half already has a forward guard
    /// (`gc_object_source_inventory_covers_every_oid_column`); its FILE half
    /// had none, and it showed: the walk collected every worktree's private
    /// index, the shared stash reflog and `refs/replace` while the inventory
    /// named none of the three, and no W1/W2 sidecar appeared at all. An
    /// inventory that understates its own collector cannot be used to review
    /// GC, which is what §C.4.3 asks it to be.
    ///
    /// KNOWN COVERAGE LIMITS (documented, not silent): the scan sees single
    /// string literals joined onto receivers it can RECOGNIZE as a storage
    /// root (identifiers containing `gitdir`/`git_dir`/`storage`, or an
    /// explicit `.join(".libra")` that is not home-rooted). It cannot see
    /// (a) paths built component-wise on a `&Path` parameter — the
    /// alternates/borrowers files (`objects_dir.join("info").join(…)`) are
    /// inventoried but guarded by their own borrower-gate tests, not by this
    /// scan; and (b) rows whose only visible join literal is in the GC
    /// collector itself (`refs/replace`, `sessions/agent-runs`) — for those
    /// this guard pins the name's continued existence, not writer coverage.
    #[test]
    fn every_storage_rooted_file_is_a_classified_gc_source() {
        /// Files under a storage root that are NOT object sources, with the
        /// reason. Listed by name so adding one is a reviewed decision.
        const NOT_AN_OBJECT_SOURCE: &[(&str, &str)] = &[
            (
                "HEAD",
                "the HEAD pointer file (dual-layout compatibility); ref OIDs are inventoried in the DB half (reference)",
            ),
            (
                "config",
                "repository configuration file (dual-layout compatibility); no object ids",
            ),
            (
                "packed-refs",
                "packed ref file (dual-layout compatibility); ref OIDs are inventoried in the DB half (reference)",
            ),
            (
                "objects/info/libra-tmp",
                "loose-object write STAGING inside the object store: objects are hardlinked out of it on publish, and an orphaned temp file is re-writable content, never the only copy",
            ),
            ("stash-stack.lock", "the shared stash stack's advisory lock"),
            ("contexts", "agent context files (§C.4.1.1 config surface)"),
            ("rules", "agent rule files (§C.4.1.1 config surface)"),
            (
                "skills",
                "agent skill definitions (§C.4.1.1 config surface; joined via the UnifiedResolver dynamic `storage.join(location)`, see resolver_joined below)",
            ),
            ("hooks.json", "hook configuration (§C.4.1.1 config surface)"),
            (
                "dagrs-checkpoints",
                "agent scheduler checkpoints; task graph state, no object ids",
            ),
            (
                "tmp/commit-preview",
                "scratch directory for commit previews",
            ),
            (
                "commands",
                "custom command definitions (§C.4.1.1 config surface; joined via the UnifiedResolver dynamic `storage.join(location)`, see resolver_joined below)",
            ),
            (
                "automations.toml",
                "automation rules (§C.4.1.1 config surface)",
            ),
            (
                "agents.toml",
                "agent registry file (§C.4.1.1 config surface)",
            ),
            (
                "agents",
                "agent definition files (§C.4.1.1 config surface; joined via the UnifiedResolver dynamic `storage.join(location)`, see resolver_joined below)",
            ),
            (
                "objects",
                "the object store itself — what GC collects, not a source of roots",
            ),
            ("pack", "packfiles inside the object store"),
            (
                "lost-found",
                "where `fsck` WRITES dangling objects it found; never an input",
            ),
            (
                "libra.db",
                "the database; its OID columns are the inventory's other half",
            ),
            (
                "info",
                "ignore/attributes sources (§C.4.1.1), no object ids",
            ),
            (
                "attributes",
                "leaf literal of the `info/attributes` source; the parent `info` directory is the actual storage-relative path",
            ),
            ("hooks", "hook scripts"),
            ("code", "`libra code` control/session surface (§C.4.1.1)"),
            ("service", "service control files (§C.4.1.1)"),
            (
                "sessions",
                "agent session surface; its findings manifests are inventoried separately",
            ),
            ("tasks", "agent task surface"),
            ("config.toml", "repository configuration (§C.4.1.1)"),
            ("commondir", "worktree layout pointer"),
            ("worktree_id", "worktree identity marker"),
            ("migrate-marker", "layout-migration lifecycle marker"),
            ("worktrees", "linked worktree gitdir container"),
            ("worktrees.json", "the worktree registry"),
            ("worktrees.lock", "registry advisory lock"),
            ("worktrees-fuse", "FUSE worktree container"),
            ("worktrees-fuse.json", "FUSE worktree registry"),
            ("maintenance.lock", "maintenance advisory lock"),
            ("branch-attach.lock", "branch-attach advisory lock"),
            (
                "merge-autostash.lock",
                "advisory lock for the inventoried autostash sidecar",
            ),
            ("publication-barrier", "publish barrier marker"),
            (
                "publication-barrier.attempted",
                "publish barrier attempt marker",
            ),
            (
                "lfs/objects",
                "the LFS content store — separate from the object store",
            ),
            (
                "media",
                "large-media content store — separate from the object store",
            ),
            (
                "chunks",
                "chunked content store — separate from the object store",
            ),
            (
                "manifests",
                "chunk manifests — describe the chunk store, not git objects",
            ),
            (
                "obliteration-audit.jsonl",
                "audit trail; the AntiRoot itself is the `object_obliteration` table",
            ),
        ];

        // Names the inventory classifies, normalized to storage-relative
        // paths: "<gitdir>/index" → "index", ".libra/shallow" → "shallow".
        let classified: Vec<String> = GC_OBJECT_FILE_SOURCE_INVENTORY
            .iter()
            .flat_map(|source| source.location.split(','))
            .map(|location| {
                let location = location.trim();
                // Strip a LEADING placeholder root only: `<id>` appears
                // mid-path in the agent-run manifest location, and splitting
                // on the first '>' anywhere would swallow the prefix.
                let location = match location
                    .strip_prefix('<')
                    .and_then(|rest| rest.split_once('>'))
                {
                    Some((_, rest)) => rest,
                    None => location,
                };
                let relative = location
                    .trim_start_matches('/')
                    .trim_start_matches(".libra/");
                // Truncate at an in-path placeholder so
                // `sessions/agent-runs/<id>/manifest.json` names the
                // directory production code actually joins.
                match relative.split_once('<') {
                    Some((head, _)) => head.trim_end_matches('/').to_string(),
                    None => relative.to_string(),
                }
            })
            .collect();

        let found = crate::internal::source_scan::storage_rooted_join_literals();
        // Self-check: an empty or broken scan would make the loop vacuous.
        assert!(
            found.contains("merge-state.json") && found.len() > 30,
            "the storage-rooted scan looks broken: {found:?}"
        );

        // The literal scanner cannot see DYNAMIC resolver joins: repo-local
        // config surfaces reach their copy through
        // `storage.join(location)` in `src/internal/ai/sources/resolver.rs`
        // (W4-06), driven by callsites such as
        // `resolved_dir_paths(working_dir, "skills")` or
        // `resolve_security_file(&request, SANDBOX_CONFIG_FILE)`. Collect the
        // location argument of every resolver callsite in the production
        // corpus (literal or single-const indirection) so the staleness
        // check below reflects what production actually resolves (W5-09 /
        // Codex r2-r3: `skills`/`commands`/`agents` are exactly this case).
        // A surface whose loader stops calling the resolver drops out of
        // this set and must then prove a literal join or lose its exclusion.
        let resolver_joined = resolver_callsite_locations();

        // Coverage direction (Codex r3): every UnifiedResolver surface
        // registered in CODE_AGENT_CONFIG_OWNERSHIP must still have an
        // actual resolver callsite (or a literal join the scanner can see) —
        // a registry row alone is NOT proof of a production join.
        for surface in crate::internal::config_ownership::CODE_AGENT_CONFIG_OWNERSHIP {
            if !matches!(
                surface.resolution,
                crate::internal::config_ownership::ReadResolution::UnifiedResolver
            ) {
                continue;
            }
            assert!(
                resolver_joined.contains(surface.location) || found.contains(surface.location),
                "UnifiedResolver surface `{}` ({}) has no resolver callsite and no \
                  literal storage-root join left in production — fix the loader or \
                  drop the registration and the NOT_AN_OBJECT_SOURCE entry",
                surface.location,
                surface.surface
            );
        }

        for name in &found {
            let inventoried = classified.iter().any(|entry| entry == name);
            let excluded = NOT_AN_OBJECT_SOURCE.iter().any(|(entry, _)| entry == name);
            assert!(
                inventoried || excluded,
                "production code creates `<storage>/{name}`, which is in neither \
                  GC_OBJECT_FILE_SOURCE_INVENTORY nor NOT_AN_OBJECT_SOURCE. Classify it: \
                  if it can hold object ids give it a row (TracedRoot / AntiRoot / \
                  Boundary / IndexOnly / NonRoot with the reason), otherwise add it to \
                  the exclusion list (plan-20260714 §C.4.3)"
            );
        }

        // And the exclusion list stays honest: an entry nothing creates is
        // stale, and an entry that is ALSO inventoried is a contradiction —
        // one that HIDES deletions, because the exclusion keeps answering
        // for a row somebody removed. (Found exactly that way: mutating the
        // `<gitdir>/index` row away left this guard green.) Stale entries are
        // aggregated into ONE failure so a mass decommission (e.g. the W5-06
        // TUI startup removal dropping several config-surface joins at once)
        // is reported in a single run instead of one entry per iteration.
        let stale: Vec<&str> = NOT_AN_OBJECT_SOURCE
            .iter()
            .filter(|(name, _)| !found.contains(*name) && !resolver_joined.contains(*name))
            .map(|(name, _)| *name)
            .collect();
        assert!(
            stale.is_empty(),
            "excluded as non-object-sources, but no production code joins them onto a \
              storage root any more — drop the entries: {stale:?}"
        );
        for (name, reason) in NOT_AN_OBJECT_SOURCE {
            assert!(
                !classified.iter().any(|entry| entry == name),
                "`{name}` is BOTH inventoried as a GC source and excluded as a \
                  non-source — the exclusion would keep this guard green if the \
                  inventory row were deleted. Keep one"
            );
            assert!(
                reason.len() > 10,
                "`{name}` is excluded without a substantive reason"
            );
        }
    }

    /// Location arguments of every unified-resolver callsite in the
    /// production corpus. Recognizes the resolver entry points
    /// (`resolved_dir_paths` / `resolved_file_paths` /
    /// `resolve_security_dir` / `resolve_security_file` and the
    /// `project_security_*` wrappers), taking the second call argument:
    /// a string literal directly, or a bare const ident resolved through
    /// the corpus-wide `const <IDENT>: &str = "<location>"` map (e.g.
    /// `SANDBOX_CONFIG_FILE`). Built on the AST-based
    /// [`crate::internal::source_scan::production_files`], which strips
    /// `#[cfg(test)]` items surgically — a line-truncating scan loses
    /// production callsites after INLINE `#[cfg(test)]` attributes (the
    /// `sandbox.toml` callsite in `sandbox/mod.rs` sits after one; the
    /// UnifiedResolver coverage loop in the guard above is the regression
    /// pin for that case).
    fn resolver_callsite_locations() -> std::collections::BTreeSet<String> {
        use syn::visit::Visit;

        const RESOLVER_FNS: &[&str] = &[
            "resolved_dir_paths",
            "resolved_file_paths",
            "resolve_security_dir",
            "resolve_security_file",
            "project_security_dir_paths",
            "project_security_file_paths",
        ];

        #[derive(Default)]
        struct Scan {
            consts: std::collections::BTreeMap<String, String>,
        }
        impl<'ast> Visit<'ast> for Scan {
            fn visit_item_const(&mut self, item: &'ast syn::ItemConst) {
                // The const's own type annotation is irrelevant (&str is a
                // Type::Reference, not a Type::Path) — a string-literal
                // initializer is the whole contract.
                if let syn::Expr::Lit(expr_lit) = &*item.expr
                    && let syn::Lit::Str(lit) = &expr_lit.lit
                {
                    self.consts.insert(item.ident.to_string(), lit.value());
                }
                syn::visit::visit_item_const(self, item);
            }
        }

        struct Calls<'a>(
            &'a std::collections::BTreeMap<String, String>,
            std::collections::BTreeSet<String>,
        );
        impl<'ast> Visit<'ast> for Calls<'_> {
            fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
                let callee = match &*call.func {
                    syn::Expr::Path(path) => path
                        .path
                        .segments
                        .last()
                        .map(|segment| segment.ident.to_string()),
                    _ => None,
                };
                if callee.is_some_and(|name| RESOLVER_FNS.contains(&name.as_str()))
                    && let Some(arg) = call.args.iter().nth(1)
                {
                    match arg {
                        syn::Expr::Lit(expr_lit) => {
                            if let syn::Lit::Str(lit) = &expr_lit.lit {
                                self.1.insert(lit.value());
                            }
                        }
                        syn::Expr::Path(path) => {
                            if let Some(ident) = path.path.get_ident()
                                && let Some(value) = self.0.get(&ident.to_string())
                            {
                                self.1.insert(value.clone());
                            }
                        }
                        _ => {}
                    }
                }
                syn::visit::visit_expr_call(self, call);
            }
        }

        let files = crate::internal::source_scan::production_files(&[]);
        let mut scan = Scan::default();
        for (_, ast) in &files {
            scan.visit_file(ast);
        }
        let mut calls = Calls(&scan.consts, std::collections::BTreeSet::new());
        for (_, ast) in &files {
            calls.visit_file(ast);
        }
        calls.1
    }
}
