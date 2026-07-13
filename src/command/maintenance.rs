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
    command::{fetch::fetch_repository_safe, load_object, log::get_reachable_commits},
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
        message,
    })
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
    if !dry_run && crate::internal::alternates::has_live_borrowers(&path::objects()) {
        return Ok(TaskResult {
            task: "gc".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: 0,
            refs_packed: 0,
            packs_repacked: 0,
            message: "skipped loose-object prune: this store is shared (other repos borrow from                       it via alternates); have borrowers run 'libra alternates remove' first"
                .to_string(),
        });
    }
    let reachable = collect_reachable_objects(&storage).await?;
    let all_loose = list_loose_objects(repo_path)
        .map_err(|e| CliError::fatal(format!("failed to list loose objects: {e}")))?;

    let mut removed = 0;
    for (hash_str, obj_path) in &all_loose {
        if let Some(hash) = parse_object_hash(hash_str)
            && !reachable.contains(&hash)
        {
            if dry_run {
                if !quiet {
                    info_println(
                        output,
                        &format!("  would remove unreachable object {hash_str}"),
                    );
                }
            } else {
                if let Err(e) = fs::remove_file(obj_path) {
                    // A concurrent cache eviction may have removed it first —
                    // the goal state (file gone) is reached either way.
                    if e.kind() != std::io::ErrorKind::NotFound {
                        return Err(CliError::fatal(format!(
                            "failed to remove unreachable object {}: {e}",
                            hash_str
                        )));
                    }
                }
                removed += 1;
            }
        }
    }

    // Clean up empty object directories
    if !dry_run {
        let _ = cleanup_empty_dirs(&path::objects());
    }

    let message = if dry_run {
        format!("would remove {} unreachable loose objects", removed)
    } else {
        format!("removed {} unreachable loose objects", removed)
    };

    Ok(TaskResult {
        task: "gc".to_string(),
        success: true,
        objects_removed: removed,
        objects_packed: 0,
        refs_packed: 0,
        packs_repacked: 0,
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

    let pack_path =
        match pack_writer::write_pack_with_index(&storage, &hashes, &pack_dir, get_hash_kind())
            .await
        {
            Ok(Some(path)) => path,
            Ok(None) => {
                return Ok(TaskResult {
                    task: "loose-objects".to_string(),
                    success: true,
                    objects_removed: 0,
                    objects_packed: 0,
                    refs_packed: 0,
                    packs_repacked: 0,
                    message: "no old loose objects to pack".to_string(),
                });
            }
            Err(e) => {
                return Err(CliError::fatal(format!("failed to create pack file: {e}")));
            }
        };

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
    let pack_name = pack_path
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
    let pack_dir = repo_path.join("objects").join("pack");
    if !pack_dir.exists() {
        return Ok(TaskResult {
            task: "incremental-repack".to_string(),
            success: true,
            objects_removed: 0,
            objects_packed: 0,
            refs_packed: 0,
            packs_repacked: 0,
            message: "no pack directory".to_string(),
        });
    }

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
            message: format!("would repack {} pack files", packs.len()),
        });
    }

    // Consolidate into a single new pack. The set MUST include objects that
    // currently live only inside the existing packs — `list_all_objects_in_storage`
    // scans only loose shards, so packing that alone and then deleting the old
    // packs would drop every packed-only object. `collect_reachable_objects`
    // walks refs/reflogs/index through storage (which reads the packs too), so
    // the new pack contains all reachable objects before the old packs go.
    let storage = ClientStorage::init(path::objects());
    let all_hashes: Vec<ObjectHash> = collect_reachable_objects(&storage)
        .await?
        .into_iter()
        .collect();

    let new_pack_path =
        match pack_writer::write_pack_with_index(&storage, &all_hashes, &pack_dir, get_hash_kind())
            .await
        {
            Ok(Some(path)) => path,
            Ok(None) => {
                return Ok(TaskResult {
                    task: "incremental-repack".to_string(),
                    success: true,
                    objects_removed: 0,
                    objects_packed: 0,
                    refs_packed: 0,
                    packs_repacked: 0,
                    message: "no objects to repack".to_string(),
                });
            }
            Err(e) => {
                return Err(CliError::fatal(format!(
                    "failed to create consolidated pack: {e}"
                )));
            }
        };

    // Remove the old packs (their objects now live in the consolidated pack).
    // `packs` was captured before the new pack was written, so it never names it.
    for old_pack in &packs {
        let _ = fs::remove_file(old_pack);
        let idx_path = old_pack.with_extension("idx");
        let _ = fs::remove_file(idx_path);
    }
    let repacked = all_hashes.len();
    let new_pack_name = new_pack_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "the consolidated pack".to_string());

    if !quiet {
        info_println(
            output,
            &format!("  consolidated into {new_pack_name} with {repacked} objects"),
        );
    }

    Ok(TaskResult {
        task: "incremental-repack".to_string(),
        success: true,
        objects_removed: 0,
        objects_packed: repacked,
        refs_packed: 0,
        packs_repacked: packs.len(),
        message: format!(
            "repacked {} packs into {} with {repacked} objects",
            packs.len(),
            new_pack_name
        ),
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
    let mut reachable: HashSet<ObjectHash> = HashSet::new();
    let db_conn = db::get_db_conn_instance().await;

    // Collect from refs
    let refs = reference::Entity::find()
        .all(&db_conn)
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
            walk_reachable(&hash, storage, &mut reachable)?;
        }
    }

    // Collect from reflogs
    let reflogs = reflog::Entity::find()
        .all(&db_conn)
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
            walk_reachable(&hash, storage, &mut reachable)?;
        }
    }

    // Held autostashes deliberately do not enter refs/stash while a merge or
    // rebase is in progress. Their fsynced sidecars are therefore first-class
    // GC roots; omitting them can irreversibly delete the user's dirty state.
    let merge_autostash = crate::command::merge::MergeAutostash::load_optional_sync()
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
    let rebase_autostash = crate::command::rebase::held_autostash_oid()?;
    for held in [merge_autostash, rebase_autostash].into_iter().flatten() {
        walk_reachable(&held, storage, &mut reachable)?;
    }

    // Ordinary stashes are file-backed rather than SQLite reference rows, and
    // older entries live only in logs/refs/stash. Trace the full reflog, not
    // just refs/stash, so stash@{1} and later remain recoverable.
    let stash_roots = crate::command::stash::gc_roots().map_err(|error| {
        CliError::fatal(format!("failed to load stash GC roots: {error}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    for oid in stash_roots {
        walk_reachable(&oid, storage, &mut reachable)?;
    }

    // Collect from index — every stage, not just stage 0, so a blob referenced
    // only by an unmerged conflict stage (1/2/3) is not treated as garbage.
    let index_path = path::index();
    let index_exists = index_path.try_exists().map_err(|error| {
        CliError::fatal(format!(
            "failed to inspect index GC root '{}': {error}",
            index_path.display()
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    if index_exists {
        let index = git_internal::internal::index::Index::load(&index_path).map_err(|error| {
            CliError::fatal(format!(
                "failed to read index GC root '{}': {error}",
                index_path.display()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        for stage in 0..=3 {
            for entry in index.tracked_entries(stage) {
                reachable.insert(entry.hash);
            }
        }
    }

    Ok(reachable)
}

/// Walk object references recursively, adding all transitive dependencies.
fn walk_reachable(
    hash: &ObjectHash,
    storage: &ClientStorage,
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
            let commit = load_object::<Commit>(hash).map_err(|error| {
                CliError::fatal(format!(
                    "reachable commit {hash} is corrupt while computing GC roots: {error}"
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            walk_reachable(&commit.tree_id, storage, reachable)?;
            for parent in &commit.parent_commit_ids {
                walk_reachable(parent, storage, reachable)?;
            }
        }
        ObjectType::Tree => {
            let tree = load_object::<Tree>(hash).map_err(|error| {
                CliError::fatal(format!(
                    "reachable tree {hash} is corrupt while computing GC roots: {error}"
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            for item in &tree.tree_items {
                walk_reachable(&item.id, storage, reachable)?;
            }
        }
        ObjectType::Tag => {
            let tag = load_object::<GitTag>(hash).map_err(|error| {
                CliError::fatal(format!(
                    "reachable tag {hash} is corrupt while computing GC roots: {error}"
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            walk_reachable(&tag.object_hash, storage, reachable)?;
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
}
