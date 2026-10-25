//! `libra gc` — Git-compatible repository garbage collection (ADR-MX-07).
//!
//! Runs the maintenance steps Git's `gc` performs, in order, by reusing the
//! existing maintenance/reflog/repack/worktree primitives rather than
//! re-implementing any reachability traversal:
//!
//! 1. `pack-refs` — collapse loose refs into `packed-refs`;
//! 2. reflog expiry — `gc.reflogExpire` / `gc.reflogExpireUnreachable`;
//! 3. `repack -a -d` — pack all reachable objects and drop the now-packed
//!    loose copies;
//! 4. unreachable loose-object pruning — the shared `collect_reachable_objects`
//!    inventory plus the same alternates-borrower refusal the maintenance `gc`
//!    task uses (never a new traversal);
//! 5. `worktree prune` — retire registry entries whose path vanished.
//!
//! `--auto` gates the whole run on `gc.auto` (default 6700): below the
//! threshold the command exits 0 without writing anything.

use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use clap::Parser;
use serde::Serialize;

use crate::{
    command::{
        maintenance::{
            cleanup_empty_dirs, collect_reachable_objects, list_loose_objects, parse_object_hash,
            run_pack_refs,
        },
        reflog::expire_all_reflogs,
        repack::{self, RepackArgs},
        worktree::{self, WorktreeArgs, WorktreeSubcommand},
    },
    internal::{
        config::ConfigKv,
        db,
        maintenance_lock::{DELETION_LOCK_WAIT, MaintenanceLock},
        reflog::{ExpireCutoff, parse_expire_cutoff},
    },
    utils::{
        client_storage::{ClientStorage, remove_object_index_rows_with_conn},
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        path,
        util::try_get_storage_path,
    },
};

const DEFAULT_GC_AUTO: i64 = 6700;
const DEFAULT_PRUNE_EXPIRE: &str = "2.weeks.ago";

/// `--help` examples shown in `libra gc --help` output.
pub const GC_EXAMPLES: &str = "\
EXAMPLES:
    libra gc                        Run garbage collection with the default policy
    libra gc --prune=now            Delete unreachable loose objects immediately
    libra gc --no-prune             Pack and clean up without deleting any objects
    libra gc --auto                 Only run when loose objects exceed gc.auto (6700)
    libra gc --aggressive           Spend more time optimizing pack layout
    libra gc --json                 Emit a structured summary of every step";

/// Combine repository maintenance into one Git-compatible `gc` command.
#[derive(Parser, Debug)]
#[command(after_help = GC_EXAMPLES)]
pub struct GcArgs {
    /// Prune unreachable loose objects older than this date (default:
    /// `gc.pruneExpire`, falling back to `2.weeks.ago`; `now` deletes
    /// immediately, `never` disables pruning).
    #[arg(long, value_name = "DATE", conflicts_with = "no_prune")]
    pub prune: Option<String>,
    /// Do not prune any unreachable loose objects.
    #[arg(long)]
    pub no_prune: bool,
    /// Spend more time optimizing (a larger delta-compression window).
    #[arg(long)]
    pub aggressive: bool,
    /// Only run when the loose-object count exceeds `gc.auto` (default 6700);
    /// otherwise exit 0 without writing anything.
    #[arg(long)]
    pub auto: bool,
}

/// One executed `gc` step, surfaced in the human/JSON summary.
#[derive(Serialize)]
struct GcStep {
    task: String,
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    objects_removed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    objects_packed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refs_packed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reflog_entries_pruned: Option<usize>,
    message: String,
}

impl GcStep {
    fn simple(task: &str, success: bool, message: String) -> Self {
        Self {
            task: task.to_string(),
            success,
            objects_removed: None,
            objects_packed: None,
            refs_packed: None,
            reflog_entries_pruned: None,
            message,
        }
    }
}

/// JSON envelope for the whole `gc` invocation.
#[derive(Serialize)]
struct GcOutput {
    auto: bool,
    auto_skipped: bool,
    aggressive: bool,
    prune: String,
    steps: Vec<GcStep>,
}

/// Result of the unreachable-loose-object prune step.
struct PruneOutcome {
    removed: usize,
    message: String,
}

pub async fn execute(args: GcArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

/// Safe entry point returning structured errors (see `execute`).
///
/// # Side Effects
/// Packs refs, expires reflogs, repacks all reachable objects into a fresh
/// pack (deleting the now-packed loose copies), prunes unreachable loose
/// objects past the resolved prune expiry, and prunes vanished worktree
/// registry entries.
///
/// # Errors
/// Propagates the first failing step's [`CliError`]. An invalid `--prune`
/// value fails before any write; a live alternates borrower refuses the prune
/// step; a `--auto` run below `gc.auto` exits 0 with zero writes.
pub async fn execute_safe(args: GcArgs, output: &OutputConfig) -> CliResult<()> {
    let repo_path = try_get_storage_path(None)
        .map_err(|e| CliError::repo_not_found().with_hint(e.to_string()))?;

    let loose_count = list_loose_objects(&repo_path)
        .map_err(|e| CliError::fatal(format!("failed to list loose objects: {e}")))?
        .len();

    // `--auto`: below the threshold, exit 0 with zero writes. Git's
    // `too_many_loose_objects` treats `gc.auto <= 0` as "never auto-gc", so
    // non-positive thresholds also skip without writing.
    if args.auto {
        let threshold = gc_auto_threshold().await?;
        if threshold <= 0 || (loose_count as i64) < threshold {
            if output.is_json() {
                emit_json_data(
                    "gc",
                    &GcOutput {
                        auto: true,
                        auto_skipped: true,
                        aggressive: args.aggressive,
                        prune: prune_label(&args).await,
                        steps: Vec::new(),
                    },
                    output,
                )?;
            } else if !output.quiet {
                println!(
                    "Auto gc skipped: {loose_count} loose objects (gc.auto threshold: \
                     {threshold})."
                );
            }
            return Ok(());
        }
    }

    // Resolve the prune policy up front so an invalid `--prune` value fails
    // before any other step writes.
    let prune_cutoff = resolve_prune_cutoff(&args).await?;

    // Sub-steps render nothing themselves: gc owns the summary.
    let child = silent_child(output);
    let mut steps: Vec<GcStep> = Vec::new();

    // 1. pack-refs (shared maintenance hold, exactly like the maintenance
    //    `pack-refs` task takes around this same helper).
    {
        let _pack_refs_lock = MaintenanceLock::shared(&repo_path)?;
        match run_pack_refs(&repo_path, false, true, &child).await {
            Ok(result) => {
                let refs_packed = Some(result.refs_packed);
                steps.push(GcStep {
                    task: "pack-refs".to_string(),
                    success: result.success,
                    objects_removed: None,
                    objects_packed: None,
                    refs_packed,
                    reflog_entries_pruned: None,
                    message: result.message,
                });
            }
            Err(error) => return Err(error),
        }
    }

    // 2. Reflog expiry (default gc.reflogExpire / gc.reflogExpireUnreachable).
    match expire_all_reflogs().await {
        Ok(results) => {
            let pruned: usize = results.iter().map(|r| r.pruned).sum();
            steps.push(GcStep {
                task: "reflog-expire".to_string(),
                success: true,
                objects_removed: None,
                objects_packed: None,
                refs_packed: None,
                reflog_entries_pruned: Some(pruned),
                message: format!(
                    "expired {pruned} reflog entries across {} ref(s)",
                    results.len()
                ),
            });
        }
        Err(error) => return Err(error),
    }

    // 3. repack -a -d (the repack command takes its own shared/exclusive
    //    maintenance locks for the pack write and the loose-object deletion).
    //    `--aggressive` widens the delta window to Git's gc.aggressiveWindow
    //    default (250); otherwise repack resolves pack.window/pack.depth.
    match repack::execute_safe(
        RepackArgs {
            all: true,
            delete: true,
            quiet: true,
            local: false,
            window: args.aggressive.then_some(250),
            depth: args.aggressive.then_some(50),
        },
        &child,
    )
    .await
    {
        Ok(()) => steps.push(GcStep::simple(
            "repack",
            true,
            "repacked all reachable objects and removed the now-packed loose copies".to_string(),
        )),
        Err(error) => return Err(error),
    }

    // 4. Unreachable loose-object pruning (reuses the maintenance reachability
    //    inventory + alternates borrow protection; no new traversal).
    match prune_cutoff {
        Some(cutoff) => match prune_unreachable_loose(&repo_path, cutoff).await {
            Ok(outcome) => steps.push(GcStep {
                task: "prune".to_string(),
                success: true,
                objects_removed: Some(outcome.removed),
                objects_packed: None,
                refs_packed: None,
                reflog_entries_pruned: None,
                message: outcome.message,
            }),
            Err(error) => return Err(error),
        },
        None => steps.push(GcStep::simple(
            "prune",
            true,
            "skipped (--no-prune)".to_string(),
        )),
    }

    // 5. worktree prune.
    match worktree::execute_safe(
        WorktreeArgs {
            command: WorktreeSubcommand::Prune,
        },
        &child,
    )
    .await
    {
        Ok(()) => steps.push(GcStep::simple(
            "worktree-prune",
            true,
            "pruned worktree registry entries whose paths vanished".to_string(),
        )),
        Err(error) => return Err(error),
    }

    if output.is_json() {
        emit_json_data(
            "gc",
            &GcOutput {
                auto: args.auto,
                auto_skipped: false,
                aggressive: args.aggressive,
                prune: prune_label(&args).await,
                steps,
            },
            output,
        )?;
    } else if !output.quiet {
        for step in &steps {
            println!("gc {}: {}", step.task, step.message);
        }
    }

    Ok(())
}

/// A child output config that never renders its own output: `gc` owns the
/// human summary and the JSON envelope.
fn silent_child(output: &OutputConfig) -> OutputConfig {
    let mut child = output.clone();
    child.json_format = None;
    child.quiet = true;
    child
}

/// Resolve `gc.auto` (default 6700). A non-positive value disables auto-gc
/// (the caller skips without writing), matching Git.
async fn gc_auto_threshold() -> CliResult<i64> {
    match ConfigKv::get("gc.auto").await {
        Ok(Some(entry)) => Ok(entry.value.trim().parse::<i64>().unwrap_or(DEFAULT_GC_AUTO)),
        Ok(None) => Ok(DEFAULT_GC_AUTO),
        Err(error) => Err(CliError::fatal(format!(
            "failed to read gc.auto config: {error}"
        ))),
    }
}

/// Resolve the prune policy: `--no-prune` → `None` (skip); `--prune=<date>`
/// → parsed cutoff; otherwise `gc.pruneExpire` (default `2.weeks.ago`).
async fn resolve_prune_cutoff(args: &GcArgs) -> CliResult<Option<ExpireCutoff>> {
    if args.no_prune {
        return Ok(None);
    }
    let raw = match &args.prune {
        Some(value) => value.clone(),
        None => match ConfigKv::get("gc.pruneExpire").await {
            Ok(Some(entry)) => entry.value,
            Ok(None) => DEFAULT_PRUNE_EXPIRE.to_string(),
            Err(error) => {
                return Err(CliError::fatal(format!(
                    "failed to read gc.pruneExpire config: {error}"
                )));
            }
        },
    };
    let cutoff = parse_expire_cutoff(&raw).ok_or_else(|| {
        CliError::fatal(format!("invalid prune expiry '{raw}'"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_exit_code(129)
            .with_hint("use 'now', 'never', or a date such as '2.weeks.ago'")
    })?;
    Ok(Some(cutoff))
}

/// Human/JSON label for the resolved prune policy.
async fn prune_label(args: &GcArgs) -> String {
    if args.no_prune {
        return "never".to_string();
    }
    if let Some(value) = &args.prune {
        return value.clone();
    }
    match ConfigKv::get("gc.pruneExpire").await {
        Ok(Some(entry)) => entry.value,
        _ => DEFAULT_PRUNE_EXPIRE.to_string(),
    }
}

/// Prune unreachable loose objects older than `cutoff`, reusing the shared
/// reachability inventory and the alternates-borrower refusal verbatim.
async fn prune_unreachable_loose(
    repo_path: &Path,
    cutoff: ExpireCutoff,
) -> CliResult<PruneOutcome> {
    if matches!(cutoff, ExpireCutoff::Never) {
        return Ok(PruneOutcome {
            removed: 0,
            message: "prune disabled (expiry is 'never')".to_string(),
        });
    }

    // lore.md 2.3 deletion safety: refuse while another repository borrows
    // from this object store (this store's reachability does not include the
    // borrower's refs).
    crate::internal::alternates::ensure_no_live_borrowers(
        "prune loose objects",
        StableErrorCode::ConflictOperationBlocked,
    )?;

    let storage = ClientStorage::init(path::objects());
    let all_loose = list_loose_objects(repo_path)
        .map_err(|e| CliError::fatal(format!("failed to list loose objects: {e}")))?;
    let reachable = collect_reachable_objects(&storage).await?;

    let mut candidate_oids: Vec<String> = Vec::new();
    let mut candidate_paths: Vec<PathBuf> = Vec::new();
    for (hash_str, obj_path) in &all_loose {
        let Some(hash) = parse_object_hash(hash_str) else {
            continue;
        };
        if reachable.contains(&hash) {
            continue;
        }
        if loose_expired(obj_path, cutoff) {
            candidate_oids.push(hash_str.clone());
            candidate_paths.push(obj_path.clone());
        }
    }

    if candidate_oids.is_empty() {
        return Ok(PruneOutcome {
            removed: 0,
            message: "no unreachable loose objects past the prune expiry".to_string(),
        });
    }

    // Deletion phase: exclusive maintenance hold so no publisher can be
    // running while payloads are unlinked.
    let Some(_deletion_lock) = MaintenanceLock::try_exclusive(repo_path, DELETION_LOCK_WAIT)?
    else {
        return Ok(PruneOutcome {
            removed: 0,
            message: format!(
                "deferred the deletion of {} unreachable loose object(s): another command is \
                 still publishing objects in this repository",
                candidate_oids.len()
            ),
        });
    };

    // Re-checked under the exclusive hold: borrower registration is itself a
    // publication, so one that appeared after the first gate is caught here.
    crate::internal::alternates::ensure_no_live_borrowers(
        "prune loose objects",
        StableErrorCode::ConflictOperationBlocked,
    )?;

    // Final reachability re-scan under the exclusive hold: no publisher can be
    // running, so a candidate still unreachable here is safe to delete.
    let still_reachable = collect_reachable_objects(&storage).await?;
    let mut final_oids: Vec<String> = Vec::new();
    let mut final_paths: Vec<PathBuf> = Vec::new();
    for (oid, path) in candidate_oids.iter().zip(candidate_paths.iter()) {
        if let Some(hash) = parse_object_hash(oid)
            && !still_reachable.contains(&hash)
        {
            final_oids.push(oid.clone());
            final_paths.push(path.clone());
        }
    }

    if final_oids.is_empty() {
        return Ok(PruneOutcome {
            removed: 0,
            message: "all prune candidates became reachable; nothing deleted".to_string(),
        });
    }

    // Catalogue first, committed before the first unlink: dropping rows can
    // only under-advertise; unlinking first could leave rows advertising
    // bytes that no longer exist.
    let db_conn = db::get_db_conn_instance().await;
    remove_object_index_rows_with_conn(&db_conn, &final_oids)
        .await
        .map_err(|e| {
            CliError::fatal(format!(
                "failed to remove object_index rows for pruned objects: {e}"
            ))
        })?;

    let mut removed = 0usize;
    for obj_path in &final_paths {
        match std::fs::remove_file(obj_path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(CliError::fatal(format!(
                    "failed to remove unreachable object {}: {e}",
                    obj_path.display()
                )));
            }
        }
    }
    cleanup_empty_dirs(&path::objects())
        .map_err(|e| CliError::fatal(format!("failed to clean empty object directories: {e}")))?;

    Ok(PruneOutcome {
        removed,
        message: format!("removed {removed} unreachable loose objects"),
    })
}

/// Whether a loose object's mtime places it past `cutoff`.
fn loose_expired(path: &Path, cutoff: ExpireCutoff) -> bool {
    match cutoff {
        ExpireCutoff::Never => false,
        ExpireCutoff::All => true,
        // Git's prune keeps an object only while `st_mtime > expire` (prune.c),
        // i.e. deletes at `mtime <= expire` — inclusive, so `--prune=now` also
        // removes objects written within the current second (no same-second
        // race). Second-granularity mtimes are compared accordingly.
        ExpireCutoff::Before(cutoff_secs) => std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
            .is_some_and(|age| (age.as_secs() as i64) <= cutoff_secs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::reflog::ExpireCutoff;

    #[test]
    fn loose_expired_matches_git_prune_inclusive_cutoff() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("obj");
        std::fs::write(&path, b"payload").expect("write object");
        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("after epoch")
            .as_secs() as i64;

        // Git deletes at `mtime <= expire`: a same-second object is past a
        // `now` cutoff, so `--prune=now` removes freshly written objects.
        assert!(loose_expired(&path, ExpireCutoff::Before(now)));
        // A cutoff in the past keeps a freshly written object (its age is
        // smaller than the cutoff).
        assert!(!loose_expired(&path, ExpireCutoff::Before(now - 3_600)));
        assert!(!loose_expired(&path, ExpireCutoff::Never));
        assert!(loose_expired(&path, ExpireCutoff::All));
        // `All` expires regardless of the path (the unlink tolerates
        // NotFound), while a time-based cutoff on a missing file never
        // produces a deletion candidate.
        assert!(loose_expired(
            &dir.path().join("missing"),
            ExpireCutoff::All
        ));
        assert!(!loose_expired(
            &dir.path().join("missing"),
            ExpireCutoff::Before(now)
        ));
    }

    #[test]
    fn silent_child_disables_child_json_and_forces_quiet() {
        let output = OutputConfig::default();
        let child = silent_child(&output);
        assert!(child.json_format.is_none(), "children never emit JSON");
        assert!(child.quiet, "children never render their own output");
    }

    #[test]
    fn gc_args_parse_all_forms_and_reject_conflicts() {
        // No flags: every field defaults to its off/None value.
        let bare = GcArgs::try_parse_from(["gc"]).expect("bare gc parses");
        assert!(bare.prune.is_none());
        assert!(!bare.no_prune);
        assert!(!bare.aggressive);
        assert!(!bare.auto);

        // Each long flag parses into its own field.
        let full = GcArgs::try_parse_from(["gc", "--prune=now", "--aggressive", "--auto"])
            .expect("full gc parse");
        assert_eq!(full.prune.as_deref(), Some("now"));
        assert!(!full.no_prune);
        assert!(full.aggressive);
        assert!(full.auto);

        let no_prune = GcArgs::try_parse_from(["gc", "--no-prune"]).expect("--no-prune parses");
        assert!(no_prune.no_prune);
        assert!(no_prune.prune.is_none());

        // `--prune` and `--no-prune` are mutually exclusive (clap conflicts).
        assert!(
            GcArgs::try_parse_from(["gc", "--prune=now", "--no-prune"]).is_err(),
            "--prune conflicts with --no-prune"
        );

        // An unknown flag is a usage error.
        assert!(
            GcArgs::try_parse_from(["gc", "--bogus"]).is_err(),
            "an unknown flag is a usage error"
        );
    }
}
