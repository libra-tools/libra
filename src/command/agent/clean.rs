//! `libra agent clean [--all]` — drop temporary checkpoints from stopped
//! sessions per `docs/development/commands/_general.md` §7.4.
//!
//! The default form scopes cleanup to the most recently stopped session;
//! `--all` widens that to every stopped session. Active sessions are never
//! cleaned because a temporary checkpoint may still be part of an in-flight
//! external-agent turn.
//!
//! When checkpoint commits are present, cleanup rewrites
//! `refs/libra/traces` so pruned temporary checkpoints stop being
//! reachable. Older DB-only fixtures with an empty ref still get the catalog
//! cleanup without a rewrite.
//!
//! AG-20 prune safety: the underlying
//! [`HistoryManager::prune_checkpoint_commits`] fails closed while a
//! checkpoint write is in flight (live in-flight marker, window A/B) or when
//! `refs/libra/traces` reaches commits missing from the checkpoint catalog
//! (window-B residue — `libra agent doctor --repair` territory). It also
//! emits the `agent.clean.prune` span (deleted_objects / deleted_sessions /
//! window_guard / duration_ms) and drops `object_index` rows for OIDs the
//! prune made unreachable.
//!
//! AG-24a `stderr_days` window (Task A8.6): `--gc` additionally prunes the
//! reviewer **stderr** diagnostic blobs
//! (`.libra/sessions/agent-runs/<run_id>/reviewers/*.stderr.redacted.log`)
//! of terminal review/investigate runs older than
//! `agent.retention.stderr_days` (default 30), while preserving each run's
//! aggregate record (`state.json` / `manifest.json` / `findings.md`, including
//! the manifest's redaction-report summary) — matching `agent.md`'s retention
//! row "删除诊断 blob，保留聚合计数". Checkpoint capture has no separate stderr
//! blob (the E4-libra `redaction_report.json` is already aggregate-only and
//! content-hash-covered), so the checkpoint tree is not touched by this window.

use std::{collections::HashSet, fs, path::Path, str::FromStr, sync::Arc};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};

use super::CleanArgs;
use crate::{
    internal::{
        ai::history::{
            CheckpointPruneGuardError, HistoryManager, SubagentContentReservationPruneGuard,
        },
        branch::TRACES_BRANCH,
        db::get_db_conn_instance,
    },
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

#[derive(Debug, Serialize)]
struct CleanReport {
    sessions_inspected: i64,
    temporary_checkpoints_dropped: u64,
    retained_checkpoints_rewritten: usize,
    traces_ref_rewritten: bool,
    /// Which AG-20 window-guard path the prune took (`noop` /
    /// `markers_and_catalog_verified`).
    window_guard: &'static str,
    /// `object_index` rows dropped for OIDs the prune made unreachable.
    object_index_rows_dropped: u64,
    /// Reviewer stderr diagnostic logs pruned by the
    /// `agent.retention.stderr_days` window (only under `--gc`); the run's
    /// aggregate record is preserved.
    stderr_logs_pruned: u64,
    /// Terminal review/investigate runs whose stderr logs the stderr-window GC
    /// touched.
    stderr_runs_pruned: u64,
    /// A0-09: aged terminal review/investigate run directories removed by the
    /// `agent.retention.findings_days` window (only under `--gc`). The
    /// objectized findings blob is reclaimed with the run (PD-04), but only
    /// when no surviving run manifest names that oid and no ref reaches it —
    /// findings blobs are content-addressed, so one can share an oid with a
    /// blob history genuinely needs.
    findings_runs_pruned: u64,
    /// plan-20260713 DR-04b (GC-DR-12): expired `agent_export_job` rows
    /// scavenged by TTL under `--gc`.
    export_jobs_pruned: u64,
    /// Terminal import identities with no remaining coverage/capture state.
    import_identities_pruned: u64,
    /// A0-09: whether this was a `--dry-run` preview (nothing was deleted; all
    /// counts are what *would* be removed).
    dry_run: bool,
    note: &'static str,
}

const OWNERLESS_IMPORT_IDENTITY_PREDICATE: &str =
    "state IN ('discovered','partial','committed','failed')
 AND owner IS NULL
 AND NOT EXISTS (
     SELECT 1
     FROM agent_session s
     JOIN agent_coverage_claim c ON c.session_id = s.session_id
     WHERE s.agent_kind = agent_import_identity.agent_kind
       AND s.provider_session_id = agent_import_identity.provider_session_id
 )";

/// Count the ownerless import identities that would remain after the exact
/// checkpoint set selected by a dry-run loses its coverage revisions/claims.
/// The simulation is isolated in a rolled-back SQLite transaction, so the
/// preview shares the real GC's relational semantics without changing refs,
/// objects, or durable catalog rows.
async fn preview_import_identity_prune_count(
    conn: &DatabaseConnection,
    checkpoint_ids: &[String],
) -> CliResult<u64> {
    let txn = conn
        .begin()
        .await
        .map_err(|error| CliError::fatal(format!("begin import identity GC preview: {error}")))?;
    let backend = txn.get_database_backend();
    for checkpoint_id in checkpoint_ids {
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM agent_coverage_conflict
             WHERE incumbent_checkpoint_id = ?
                OR EXISTS (
                  SELECT 1 FROM agent_coverage_claim c
                  WHERE c.session_id = agent_coverage_conflict.session_id
                    AND c.logical_turn_key = agent_coverage_conflict.logical_turn_key
                    AND c.coverage_schema_version = agent_coverage_conflict.coverage_schema_version
                    AND c.checkpoint_id = ?
                )",
            [checkpoint_id.clone().into(), checkpoint_id.clone().into()],
        ))
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "simulate coverage conflict pruning for import identity preview: {error}"
            ))
        })?;
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM agent_coverage_revision WHERE checkpoint_id = ?",
            [checkpoint_id.clone().into()],
        ))
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "simulate coverage revision pruning for import identity preview: {error}"
            ))
        })?;
    }
    // Claims whose original current checkpoint is selected disappear iff no
    // unselected revision survives. Processing after all revision deletes is
    // equivalent to the real per-checkpoint repoint/delete loop for the final
    // existence of each claim.
    for checkpoint_id in checkpoint_ids {
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM agent_coverage_claim
             WHERE checkpoint_id = ?
               AND NOT EXISTS (
                 SELECT 1 FROM agent_coverage_revision r
                 WHERE r.session_id = agent_coverage_claim.session_id
                   AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                   AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
               )",
            [checkpoint_id.clone().into()],
        ))
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "simulate coverage claim pruning for import identity preview: {error}"
            ))
        })?;
    }
    let sql = format!(
        "SELECT COUNT(*) AS n FROM agent_import_identity WHERE {OWNERLESS_IMPORT_IDENTITY_PREDICATE}"
    );
    let count = txn
        .query_one_raw(Statement::from_string(backend, sql))
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "count import identities after dry-run coverage simulation: {error}"
            ))
        })?
        .and_then(|row| row.try_get_by::<i64, _>("n").ok())
        .unwrap_or(0) as u64;
    txn.rollback().await.map_err(|error| {
        CliError::fatal(format!("rollback import identity GC preview: {error}"))
    })?;
    Ok(count)
}

pub async fn execute_safe(args: CleanArgs, output: &OutputConfig) -> CliResult<()> {
    // §C.4.3 writer-vs-deleter: `agent clean` is a DELETER — it prunes
    // checkpoint objects and reclaims findings blobs — so it takes the
    // repository maintenance lock EXCLUSIVELY rather than the shared hold
    // every publishing command takes (`cli::command_holds_shared_maintenance_lock`
    // carves it out for exactly this reason). Held for the whole command, so
    // its ref rewrite and its unlinks are one uninterruptible sequence.
    // A preview deletes nothing and takes nothing.
    let _deletion_lock = if args.dry_run {
        None
    } else {
        Some(
            crate::internal::maintenance_lock::MaintenanceLock::exclusive_or_refuse(
                &util::storage_path(),
                "clean agent runs and checkpoints",
            )?,
        )
    };
    // lore.md 2.3 / W0 deletion hard gate: `agent clean` unlinks object
    // payloads (checkpoint prune, findings-blob reclamation), so it is a
    // DELETION entry point and passes the same borrower gate as `gc`. A
    // borrowing repository resolves objects through the alternates path, and
    // its reachability is not part of this store's walk — a blob that is
    // unreachable here may be exactly what it is reading. Evaluated UNDER the
    // exclusive hold, so a borrower cannot register between the check and the
    // unlinks (§C.4.3).
    if _deletion_lock.is_some() {
        crate::internal::alternates::ensure_no_live_borrowers(
            "clean agent runs and checkpoints",
            crate::utils::error::StableErrorCode::ConflictOperationBlocked,
        )?;
    }
    let conn = get_db_conn_instance().await;
    let backend = conn.get_database_backend();

    if !table_exists(&conn, "agent_checkpoint").await? {
        return emit_report(
            &CleanReport {
                sessions_inspected: 0,
                temporary_checkpoints_dropped: 0,
                retained_checkpoints_rewritten: 0,
                traces_ref_rewritten: false,
                window_guard: "noop",
                object_index_rows_dropped: 0,
                stderr_logs_pruned: 0,
                stderr_runs_pruned: 0,
                findings_runs_pruned: 0,
                export_jobs_pruned: 0,
                import_identities_pruned: 0,
                dry_run: args.dry_run,
                note: "agent_checkpoint table not present (run `libra init`?)",
            },
            output,
        );
    }

    // Retention GC (AG-24a) always spans every stopped session; the
    // default/temporary path keeps the `--all` scoping.
    let session_scope = session_scope_sql(args.all || args.gc);
    let session_filter = format!("SELECT COUNT(*) AS n FROM ({session_scope}) AS scoped_sessions");
    let row = conn
        .query_one_raw(Statement::from_string(backend, session_filter))
        .await
        .map_err(|e| CliError::fatal(format!("failed to count agent_session: {e}")))?
        .ok_or_else(|| CliError::fatal("agent_session count returned no rows".to_string()))?;
    let sessions_inspected: i64 = row.try_get_by("n").unwrap_or_default();

    let (checkpoint_ids, note): (Vec<String>, &'static str) = if args.gc {
        // Resolve the transcript retention window (default 90). The cutoff
        // is `created_at < now - window`. GC removes whole aged checkpoints
        // (transcript + stderr blobs) — the append-only `agent_audit_log`
        // is a separate table the prune engine never touches.
        let retention_days = match args.retention_days {
            Some(0) => {
                return Err(CliError::command_usage(
                    "--retention-days must be greater than 0".to_string(),
                ));
            }
            Some(days) => days,
            None => crate::internal::ai::observed_agents::compliance::retention_transcript_days()
                .await
                .map_err(|e| CliError::fatal(format!("read retention config: {e:#}")))?,
        };
        let cutoff_unix = chrono::Utc::now().timestamp() - i64::from(retention_days) * 86_400;
        (
            gc_expired_checkpoint_ids(&conn, session_scope, cutoff_unix).await?,
            "retention GC dropped checkpoints older than agent.retention.transcript_days from \
             stopped sessions; the append-only agent_audit_log was not touched",
        )
    } else {
        (
            temporary_checkpoint_ids(&conn, session_scope).await?,
            "temporary checkpoint rows were dropped; reachable traces history was \
             rewritten when checkpoint commits existed",
        )
    };
    let repo_path = util::try_get_storage_path(None)
        .map_err(|e| CliError::fatal(format!("failed to locate .libra directory: {e}")))?;
    let storage = Arc::new(ClientStorage::init(repo_path.join("objects")));
    // Resolve the run-state root before `repo_path` is moved into the history
    // manager; the stderr- and findings-window GCs below prune under it.
    let sessions_root = repo_path.join("sessions");

    // AG-24a stderr window (Task A8.6): resolve + validate the stderr cutoff
    // BEFORE any prune mutation, so an invalid/overflowing config fails closed
    // rather than aborting after the checkpoint/transcript GC already rewrote
    // the store. It uses its own `agent.retention.stderr_days` knob (the
    // `--retention-days` override targets the transcript window only, per its
    // clap doc). `Some(None)` = window larger than representable time (nothing
    // can be expired → GC is a no-op); `None` = not a `--gc` run.
    let stderr_cutoff: Option<Option<DateTime<Utc>>> = if args.gc {
        let stderr_days = crate::internal::ai::observed_agents::compliance::retention_stderr_days()
            .await
            .map_err(|e| CliError::fatal(format!("read stderr retention config: {e:#}")))?;
        Some(stderr_cutoff_for_days(stderr_days))
    } else {
        None
    };

    // A0-09 findings window: resolve + validate BEFORE any mutation too, so a
    // bad `agent.retention.findings_days` fails closed rather than aborting
    // mid-sweep. Its own knob (independent of `--retention-days`).
    let findings_cutoff: Option<Option<DateTime<Utc>>> = if args.gc {
        let findings_days =
            crate::internal::ai::observed_agents::compliance::retention_findings_days()
                .await
                .map_err(|e| CliError::fatal(format!("read findings retention config: {e:#}")))?;
        Some(stderr_cutoff_for_days(findings_days))
    } else {
        None
    };

    let history =
        HistoryManager::new_with_ref(storage, repo_path, Arc::new(conn.clone()), TRACES_BRANCH);
    // Under `--dry-run` nothing is mutated: the checkpoint prune is skipped and
    // its primary count is the number of checkpoints that WOULD be dropped.
    let prune = if args.dry_run {
        None
    } else {
        Some(
            history
                .prune_checkpoint_commits(&checkpoint_ids)
                .await
                .map_err(map_prune_error)?,
        )
    };

    let (stderr_logs_pruned, stderr_runs_pruned) = match stderr_cutoff {
        Some(Some(cutoff)) => gc_expired_stderr_logs(&sessions_root, cutoff, args.dry_run)?,
        // Not a `--gc` run, or a retention window so large nothing is expired.
        _ => (0, 0),
    };

    // plan-20260713 DR-04b: TTL-scavenge expired export-job rows under --gc
    // (bounded by the ttl index; dry-run counts without deleting).
    let export_jobs_pruned = if args.gc && table_exists(&conn, "agent_export_job").await? {
        let now_ms = chrono::Utc::now().timestamp_millis();
        if args.dry_run {
            let row = conn
                .query_one_raw(sea_orm::Statement::from_sql_and_values(
                    backend,
                    "SELECT COUNT(*) AS n FROM agent_export_job WHERE ttl_expires_at <= ?",
                    [now_ms.into()],
                ))
                .await
                .map_err(|err| {
                    CliError::fatal(format!("failed to count expired export jobs: {err}"))
                })?;
            row.and_then(|r| r.try_get_by::<i64, _>("n").ok())
                .unwrap_or(0) as u64
        } else {
            crate::internal::ai::export_job::scavenge_expired(&conn, now_ms)
                .await
                .map_err(|err| {
                    CliError::fatal(format!("failed to scavenge expired export jobs: {err:#}"))
                })?
        }
    } else {
        0
    };

    let swept_import_identities_pruned =
        if args.gc && table_exists(&conn, "agent_import_identity").await? {
            if args.dry_run {
                preview_import_identity_prune_count(&conn, &checkpoint_ids).await?
            } else {
                let sql = format!(
                    "DELETE FROM agent_import_identity WHERE {OWNERLESS_IMPORT_IDENTITY_PREDICATE}"
                );
                conn.execute_raw(Statement::from_string(backend, sql))
                    .await
                    .map_err(|error| {
                        CliError::fatal(format!("failed to prune stale import identities: {error}"))
                    })?
                    .rows_affected()
            }
        } else {
            0
        };
    let import_identities_pruned = swept_import_identities_pruned
        + prune
            .as_ref()
            .map(|prune| prune.deleted_import_identities)
            .unwrap_or(0);

    let findings_gc = match findings_cutoff {
        Some(Some(cutoff)) => {
            gc_expired_findings_runs(&sessions_root, cutoff, args.dry_run).await?
        }
        _ => FindingsGcOutcome::default(),
    };
    let findings_runs_pruned = findings_gc.runs_pruned;

    emit_report(
        &CleanReport {
            sessions_inspected,
            temporary_checkpoints_dropped: prune
                .as_ref()
                .map(|p| p.removed_checkpoints)
                .unwrap_or(checkpoint_ids.len() as u64),
            retained_checkpoints_rewritten: prune
                .as_ref()
                .map(|p| p.rewritten_checkpoints)
                .unwrap_or(0),
            traces_ref_rewritten: prune.as_ref().map(|p| p.ref_rewritten).unwrap_or(false),
            window_guard: prune.as_ref().map(|p| p.window_guard).unwrap_or("noop"),
            object_index_rows_dropped: prune
                .as_ref()
                .map(|p| p.deleted_object_index_rows)
                .unwrap_or(0),
            stderr_logs_pruned,
            stderr_runs_pruned,
            findings_runs_pruned,
            export_jobs_pruned,
            import_identities_pruned,
            dry_run: args.dry_run,
            note,
        },
        output,
    )
}

/// Map a prune failure to an actionable CLI error, keeping the AG-20
/// window-guard refusals distinguishable (they are deterministic and
/// user-resolvable, not storage corruption).
fn map_prune_error(err: anyhow::Error) -> CliError {
    if err
        .downcast_ref::<SubagentContentReservationPruneGuard>()
        .is_some()
    {
        return CliError::conflict(format!("{err}"))
            .with_hint("an external-agent subagent content write is still reserved")
            .with_hint("retry once the writer finishes or its reservation lease expires");
    }
    match err.downcast_ref::<CheckpointPruneGuardError>() {
        Some(CheckpointPruneGuardError::LiveWriterMarker { .. }) => {
            CliError::conflict(format!("{err}"))
                .with_hint("an external-agent checkpoint write is still in flight")
                .with_hint(
                    "retry once the write completes; a crashed writer's marker expires \
                     automatically after its TTL",
                )
        }
        Some(CheckpointPruneGuardError::RefCatalogOrphans { .. }) => {
            CliError::conflict(format!("{err}"))
                .with_hint("run 'libra agent doctor --repair' to backfill the checkpoint catalog")
                .with_hint("then re-run 'libra agent clean'")
        }
        None => CliError::fatal(format!("failed to prune traces checkpoints: {err:#}")),
    }
}

fn session_scope_sql(all: bool) -> &'static str {
    if all {
        return "SELECT session_id FROM agent_session WHERE state = 'stopped'";
    }
    "SELECT session_id FROM agent_session \
     WHERE state = 'stopped' \
     ORDER BY COALESCE(stopped_at, last_event_at, started_at) DESC, session_id DESC \
     LIMIT 1"
}

fn emit_report(report: &CleanReport, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("agent_clean", report, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!(
        "Sessions inspected            : {}",
        report.sessions_inspected
    );
    println!(
        "Temporary checkpoints dropped : {}",
        report.temporary_checkpoints_dropped
    );
    println!(
        "Object index rows dropped     : {}",
        report.object_index_rows_dropped
    );
    println!(
        "Reviewer stderr logs pruned   : {} (across {} run(s))",
        report.stderr_logs_pruned, report.stderr_runs_pruned
    );
    println!(
        "Findings runs pruned          : {}",
        report.findings_runs_pruned
    );
    if report.dry_run {
        println!("Mode                          : dry-run (nothing was deleted)");
    }
    println!("Note                          : {}", report.note);
    Ok(())
}

async fn table_exists(conn: &(impl ConnectionTrait + ?Sized), name: &str) -> CliResult<bool> {
    let backend = conn.get_database_backend();
    let stmt = Statement::from_sql_and_values(
        backend,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
        [name.into()],
    );
    conn.query_one_raw(stmt)
        .await
        .map(|row| row.is_some())
        .map_err(|e| CliError::fatal(format!("failed to query sqlite_master: {e}")))
}

/// Checkpoint ids from the scoped (stopped) sessions whose `created_at`
/// predates the retention cutoff — the AG-24a retention GC selection. All
/// scopes are eligible (a 90-day-old committed checkpoint is as expired as
/// a temporary one); the append-only `agent_audit_log` lives in a separate
/// table and is never named here.
async fn gc_expired_checkpoint_ids(
    conn: &(impl ConnectionTrait + ?Sized),
    session_scope: &str,
    cutoff_unix: i64,
) -> CliResult<Vec<String>> {
    let backend = conn.get_database_backend();
    let query = format!(
        "SELECT checkpoint_id FROM agent_checkpoint \
         WHERE created_at < ? \
         AND session_id IN (SELECT session_id FROM ({session_scope}) AS scoped_sessions) \
         ORDER BY created_at ASC, checkpoint_id ASC"
    );
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            &query,
            [cutoff_unix.into()],
        ))
        .await
        .map_err(|e| CliError::fatal(format!("failed to list expired checkpoints: {e}")))?;
    rows.into_iter()
        .map(|row| {
            row.try_get_by("checkpoint_id")
                .map_err(|e| CliError::fatal(format!("failed to decode checkpoint_id: {e}")))
        })
        .collect()
}

/// Compute the stderr-window cutoff (`now - stderr_days`) with checked date
/// math. Returns `None` when the window is larger than the representable date
/// range — in that case nothing can be older than the cutoff, so the stderr GC
/// is a no-op (this must never panic in production on a huge config value).
fn stderr_cutoff_for_days(stderr_days: u32) -> Option<DateTime<Utc>> {
    chrono::Duration::try_days(i64::from(stderr_days))
        .and_then(|window| Utc::now().checked_sub_signed(window))
}

/// Minimal retention view over a run's shared `manifest.json` (both review and
/// investigate write the E8 manifest with `terminal_state` + `updated_at`).
struct RunRetentionMeta {
    is_terminal: bool,
    updated_at: Option<DateTime<Utc>>,
    /// The run's objectified findings blob, if it has one.
    findings_oid: Option<String>,
}

/// Parse the retention-relevant fields from `<run_dir>/manifest.json`. Returns
/// `None` when the manifest is missing, unparseable, or not a review/investigate
/// run manifest (caller skips fail-safe). `terminal_state` is typed as a string,
/// so a corrupt/foreign value (object, bool, number) fails deserialization and
/// is skipped rather than being mistaken for a terminal state.
fn read_run_retention_meta(run_dir: &Path) -> Option<RunRetentionMeta> {
    #[derive(Deserialize)]
    struct ManifestMeta {
        #[serde(default)]
        kind: Option<String>,
        /// Exactly one of the five snake_case terminal states while terminal,
        /// `null` while running. Typed as `String` on purpose: a non-string
        /// value makes the whole manifest fail to deserialize → skipped.
        #[serde(default)]
        terminal_state: Option<String>,
        #[serde(default)]
        updated_at: Option<String>,
        /// A0-06 objectified findings blob. The run directory is not the
        /// only thing a run owns: deleting the directory alone leaves this
        /// blob and its `object_index` row behind forever.
        #[serde(default)]
        findings_oid: Option<String>,
    }
    let bytes = fs::read(run_dir.join("manifest.json")).ok()?;
    let meta: ManifestMeta = serde_json::from_slice(&bytes).ok()?;
    // Only the review/investigate run manifests own reviewer stderr logs; any
    // other/absent `kind` is out of scope for this GC and is skipped. And
    // `terminal_state` must be one of that kind's REAL terminal states — a
    // corrupt/foreign string (e.g. "garbage") is treated as non-terminal
    // (skipped), never mistaken for a completed run.
    let valid_terminal: &[&str] = match meta.kind.as_deref() {
        Some("review") => &["success", "error", "cancelled", "timeout", "partial"],
        Some("investigate") => &["quorum", "max_turns", "cancelled", "timeout", "error"],
        _ => return None,
    };
    let is_terminal = meta
        .terminal_state
        .as_deref()
        .is_some_and(|state| valid_terminal.contains(&state));
    let updated_at = meta
        .updated_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    Some(RunRetentionMeta {
        is_terminal,
        updated_at,
        findings_oid: meta.findings_oid,
    })
}

/// Delete every `*.stderr.redacted.log` regular file directly under
/// `reviewers_dir`, returning the count removed. A missing dir yields 0; only
/// the stderr-log suffix is matched, so stdout logs and the aggregate record
/// stay. Symlinks are never followed: a symlinked `reviewers` directory is
/// refused outright (so a hostile run dir cannot redirect deletion outside the
/// store), and within the dir only regular files are removed.
fn prune_stderr_logs_in(reviewers_dir: &Path, dry_run: bool) -> CliResult<u64> {
    // Refuse to descend a symlinked `reviewers` directory — `read_dir` would
    // otherwise follow it and delete matching files at the symlink target.
    match fs::symlink_metadata(reviewers_dir) {
        Ok(meta) if meta.file_type().is_symlink() => return Ok(0),
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => {
            return Err(CliError::fatal(format!(
                "failed to stat reviewers dir {}: {err}",
                reviewers_dir.display()
            )));
        }
    }
    let entries = match fs::read_dir(reviewers_dir) {
        Ok(read_dir) => read_dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => {
            return Err(CliError::fatal(format!(
                "failed to read reviewers dir {}: {err}",
                reviewers_dir.display()
            )));
        }
    };
    let mut removed = 0u64;
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.ends_with(".stderr.redacted.log") {
            if dry_run {
                // Preview: count what would be pruned without removing it.
                removed += 1;
                continue;
            }
            match fs::remove_file(entry.path()) {
                Ok(()) => removed += 1,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(CliError::fatal(format!(
                        "failed to remove stderr log {}: {err}",
                        entry.path().display()
                    )));
                }
            }
        }
    }
    Ok(removed)
}

/// Prune reviewer stderr diagnostic logs from terminal review/investigate runs
/// older than the `agent.retention.stderr_days` cutoff (Task A8.6). Returns
/// `(files_pruned, runs_pruned)`. Runs that are still in flight (non-terminal),
/// have an unreadable/undated manifest, or have an unparseable timestamp are
/// skipped fail-safe — a stderr blob is never deleted when the run's age is
/// unknown. The run's aggregate record is always preserved.
fn gc_expired_stderr_logs(
    sessions_root: &Path,
    cutoff: DateTime<Utc>,
    dry_run: bool,
) -> CliResult<(u64, u64)> {
    let runs_root = sessions_root.join("agent-runs");
    let entries = match fs::read_dir(&runs_root) {
        Ok(read_dir) => read_dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(err) => {
            return Err(CliError::fatal(format!(
                "failed to read agent-runs dir {}: {err}",
                runs_root.display()
            )));
        }
    };

    let mut files_pruned = 0u64;
    let mut runs_pruned = 0u64;
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(run_id) = entry.file_name().into_string() else {
            continue;
        };
        // Reuse the review store's path-traversal-safe run-id validator so a
        // stray/hostile directory name can never widen the delete scope.
        if !crate::internal::ai::review::store::is_valid_run_id(&run_id) {
            continue;
        }
        let run_dir = entry.path();
        let Some(meta) = read_run_retention_meta(&run_dir) else {
            continue; // missing/corrupt manifest → skip fail-safe
        };
        if !meta.is_terminal {
            continue; // never touch an in-flight run's diagnostics
        }
        let Some(updated) = meta.updated_at else {
            continue; // undatable → skip fail-safe
        };
        if updated >= cutoff {
            continue; // within the retention window
        }
        let pruned_here = prune_stderr_logs_in(&run_dir.join("reviewers"), dry_run)?;
        if pruned_here > 0 {
            files_pruned += pruned_here;
            runs_pruned += 1;
        }
    }
    Ok((files_pruned, runs_pruned))
}

/// A0-09: findings retention GC. Remove whole terminal review/investigate run
/// DIRECTORIES (`findings.md`, `manifest.json`, `state.json`, reviewer logs)
/// older than the `agent.retention.findings_days` cutoff. Returns the number of
/// run directories pruned. Runs still in flight (non-terminal), with an
/// unreadable/undated manifest, or an unparseable timestamp are skipped
/// fail-safe — a run is never removed when its age is unknown. The append-only
/// `agent_audit_log` is a separate table and is never touched. Idempotent (a
/// missing dir is a no-op).
///
/// This deliberately does NOT delete the objectized findings blob or drop its
/// `object_index` row: those are ordinary content-addressed git objects that
/// may be byte-identical to a blob reachable from a branch, index, reflog, or
/// another run. Repo-level reclamation is `libra maintenance run --task gc`'s
/// job (PD-04): its reachability walk keeps live-run manifest OIDs alive and
/// prunes orphaned findings blobs TOGETHER with their `object_index` rows, so
/// per-run retention here only removes the run directory and leaves the
/// shared object store intact.
async fn gc_expired_findings_runs(
    sessions_root: &Path,
    cutoff: DateTime<Utc>,
    dry_run: bool,
) -> CliResult<FindingsGcOutcome> {
    let runs_root = sessions_root.join("agent-runs");
    let mut pruned_oids: HashSet<String> = HashSet::new();
    let entries = match fs::read_dir(&runs_root) {
        Ok(read_dir) => read_dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FindingsGcOutcome::default());
        }
        Err(err) => {
            return Err(CliError::fatal(format!(
                "failed to read agent-runs dir {}: {err}",
                runs_root.display()
            )));
        }
    };

    let mut runs_pruned = 0u64;
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(run_id) = entry.file_name().into_string() else {
            continue;
        };
        // The same path-traversal-safe validator the stderr window uses: a
        // stray/hostile dir name can never widen the delete scope.
        if !crate::internal::ai::review::store::is_valid_run_id(&run_id) {
            continue;
        }
        let run_dir = entry.path();
        let Some(meta) = read_run_retention_meta(&run_dir) else {
            // NO manifest at all: an interrupted run. The maintenance root
            // walk now fails CLOSED on these rather than pruning objects the
            // run may still own (plan-20260714 §C.4.3 — a mandatory root that
            // cannot be enumerated is never "no roots"), so SOMETHING has to
            // be able to retire them; otherwise one crashed run makes the
            // repository permanently unprunable.
            //
            // This is that route, and it is explicit rather than automatic:
            // only under the user's `agent clean`, and only for directories
            // untouched since the same retention cutoff, so a run that is
            // merely mid-write is never taken. The objects such a directory
            // owned become unreachable and then face the ordinary two-scan
            // quarantine; nothing is deleted here but the directory.
            //
            // A manifest that IS a JSON object but that this GC declines to
            // act on (a foreign `kind`, a non-terminal state) is a different
            // case and is left alone, as before: the run is out of scope for
            // this GC, not abandoned — and the root walk can read it, so it
            // wedges nothing.
            let manifest_path = run_dir.join("manifest.json");
            let unenumerable = match fs::read(&manifest_path) {
                // No manifest at all.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
                // Unreadable: the walk cannot enumerate it either.
                Err(_) => true,
                // Present but not a JSON OBJECT — `[]`, `null`, a bare
                // string, or corrupt bytes. The GC root walk refuses these
                // outright (a field lookup on them returns nothing, which
                // would read as "this run declares no roots"), so they need
                // the same retirement route or they wedge pruning forever.
                Ok(bytes) => !serde_json::from_slice::<serde_json::Value>(&bytes)
                    .is_ok_and(|value| value.is_object()),
            };
            let abandoned = unenumerable
                && fs::metadata(&run_dir)
                    .and_then(|meta| meta.modified())
                    .map(DateTime::<Utc>::from)
                    .is_ok_and(|touched| touched < cutoff);
            if !abandoned {
                continue;
            }
            runs_pruned += 1;
            if dry_run {
                continue;
            }
            match fs::remove_dir_all(&run_dir) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(CliError::fatal(format!(
                        "failed to remove abandoned run dir {}: {err}",
                        run_dir.display()
                    )));
                }
            }
            continue;
        };
        if !meta.is_terminal {
            continue; // never delete an in-flight / paused run
        }
        let Some(updated) = meta.updated_at else {
            continue; // undatable → skip fail-safe
        };
        if updated >= cutoff {
            continue; // within the retention window
        }

        runs_pruned += 1;
        if let Some(oid) = meta.findings_oid.clone() {
            pruned_oids.insert(oid);
        }
        if dry_run {
            continue; // preview: count without removing
        }
        match fs::remove_dir_all(&run_dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(CliError::fatal(format!(
                    "failed to remove expired run dir {}: {err}",
                    run_dir.display()
                )));
            }
        }
    }

    let objects_pruned = if dry_run || pruned_oids.is_empty() {
        0
    } else {
        reclaim_findings_objects(&runs_root, pruned_oids).await?
    };
    Ok(FindingsGcOutcome {
        runs_pruned,
        objects_pruned,
    })
}

/// What one findings GC pass reclaimed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct FindingsGcOutcome {
    runs_pruned: u64,
    objects_pruned: u64,
}

/// Reclaim the findings blobs the pruned runs owned (A0-09 / PD-04).
///
/// Deleting a run directory used to be the whole story, which left every
/// objectified findings blob and its `object_index` row in the repository
/// forever — the retention window expired but the bytes never went away.
///
/// Two things make this delicate, and both are handled conservatively:
///
/// * Findings blobs are CONTENT-ADDRESSED, so two runs with byte-identical
///   findings share one oid. An oid is only a candidate once no SURVIVING
///   run manifest still names it.
/// * Content addressing also means a findings blob can collide with an
///   ordinary file blob that history genuinely references. Deleting that
///   would corrupt the repository, so anything reachable from a ref is
///   kept — the `object_index` row is still dropped, and `agent doctor`
///   re-inserts it if the blob is meant to stay visible.
async fn reclaim_findings_objects(
    runs_root: &Path,
    mut candidates: HashSet<String>,
) -> CliResult<u64> {
    // Whatever a surviving run still points at is not garbage.
    if let Ok(entries) = fs::read_dir(runs_root) {
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            if let Some(meta) = read_run_retention_meta(&entry.path())
                && let Some(oid) = meta.findings_oid
            {
                candidates.remove(&oid);
            }
        }
    }
    if candidates.is_empty() {
        return Ok(0);
    }

    let storage_path = util::try_get_storage_path(None)
        .map_err(|e| CliError::fatal(format!("not in a libra repository: {e}")))?;
    let storage = ClientStorage::init(storage_path.join("objects"));
    // One reachability sweep for the whole batch, and only when there is
    // something to reclaim — it is far too expensive to run per candidate.
    let reachable = crate::command::maintenance::collect_reachable_objects(&storage).await?;

    let db_conn = get_db_conn_instance().await;
    // The index row goes either way: it is a sync/visibility artifact, not
    // the object, and `agent doctor` rebuilds it from the run manifest.
    let oids: Vec<String> = candidates.iter().cloned().collect();
    crate::utils::client_storage::remove_object_index_rows_with_conn(&db_conn, &oids)
        .await
        .map_err(|e| CliError::fatal(format!("failed to drop findings object_index rows: {e}")))?;

    let mut pruned = 0u64;
    for oid in &oids {
        let Ok(hash) = git_internal::hash::ObjectHash::from_str(oid) else {
            continue; // a manifest we cannot parse is not a licence to delete
        };
        if reachable.contains(&hash) {
            continue; // history needs these bytes; only the row was ours
        }
        let object_path = storage_path.join("objects").join(&oid[..2]).join(&oid[2..]);
        match fs::remove_file(&object_path) {
            Ok(()) => pruned += 1,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(CliError::fatal(format!(
                    "failed to remove expired findings object {oid}: {err}"
                )));
            }
        }
    }
    Ok(pruned)
}

async fn temporary_checkpoint_ids(
    conn: &(impl ConnectionTrait + ?Sized),
    session_scope: &str,
) -> CliResult<Vec<String>> {
    let backend = conn.get_database_backend();
    let query = format!(
        "SELECT checkpoint_id FROM agent_checkpoint WHERE scope = 'temporary' \
         AND session_id IN (SELECT session_id FROM ({session_scope}) AS scoped_sessions) \
         ORDER BY created_at ASC, checkpoint_id ASC"
    );
    let rows = conn
        .query_all_raw(Statement::from_string(backend, query))
        .await
        .map_err(|e| CliError::fatal(format!("failed to list temporary checkpoints: {e}")))?;
    rows.into_iter()
        .map(|row| {
            row.try_get_by("checkpoint_id")
                .map_err(|e| CliError::fatal(format!("failed to decode checkpoint_id: {e}")))
        })
        .collect()
}

#[cfg(test)]
mod stderr_gc_tests {
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::utils::test::{ChangeDirGuard, ScopedEnvVar, setup_with_new_libra_in};

    fn write_run(sessions_root: &Path, run_id: &str, terminal: bool, updated_at: &str) {
        let run_dir = sessions_root.join("agent-runs").join(run_id);
        fs::create_dir_all(run_dir.join("reviewers")).unwrap();
        let terminal_state = if terminal { "\"success\"" } else { "null" };
        let manifest = format!(
            "{{\"schema_version\":1,\"run_id\":\"{run_id}\",\"kind\":\"review\",\
             \"terminal_state\":{terminal_state},\"updated_at\":\"{updated_at}\"}}"
        );
        fs::write(run_dir.join("manifest.json"), manifest).unwrap();
        fs::write(run_dir.join("reviewers/a.stderr.redacted.log"), "x").unwrap();
        fs::write(run_dir.join("reviewers/a.stdout.redacted.log"), "y").unwrap();
    }

    fn stderr_exists(sessions_root: &Path, run_id: &str) -> bool {
        sessions_root
            .join("agent-runs")
            .join(run_id)
            .join("reviewers/a.stderr.redacted.log")
            .exists()
    }

    #[test]
    fn prunes_only_aged_terminal_runs_and_keeps_aggregate() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        write_run(
            &sessions,
            "aged-terminal",
            true,
            "2000-01-01T00:00:00.000000Z",
        );
        let recent = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        write_run(&sessions, "recent-terminal", true, &recent);
        write_run(
            &sessions,
            "aged-running",
            false,
            "2000-01-01T00:00:00.000000Z",
        );

        let cutoff = Utc::now() - chrono::Duration::days(30);
        let (files, runs) = gc_expired_stderr_logs(&sessions, cutoff, false).unwrap();

        assert_eq!(
            (files, runs),
            (1, 1),
            "only the aged terminal run is pruned"
        );
        assert!(
            !stderr_exists(&sessions, "aged-terminal"),
            "aged stderr pruned"
        );
        assert!(
            sessions
                .join("agent-runs/aged-terminal/reviewers/a.stdout.redacted.log")
                .exists(),
            "stdout (aggregate provenance) preserved"
        );
        assert!(
            sessions
                .join("agent-runs/aged-terminal/manifest.json")
                .exists(),
            "manifest (aggregate record) preserved"
        );
        assert!(stderr_exists(&sessions, "recent-terminal"), "recent kept");
        assert!(stderr_exists(&sessions, "aged-running"), "in-flight kept");
    }

    #[test]
    fn missing_or_undatable_runs_are_skipped_fail_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");

        // Missing manifest → skip (do not delete when age is unknown).
        let no_manifest = sessions.join("agent-runs/no-manifest/reviewers");
        fs::create_dir_all(&no_manifest).unwrap();
        fs::write(no_manifest.join("a.stderr.redacted.log"), "x").unwrap();

        // Terminal but with an unparseable timestamp → skip.
        write_run(&sessions, "bad-ts", true, "not-a-timestamp");

        let cutoff = Utc::now() - chrono::Duration::days(30);
        let (files, runs) = gc_expired_stderr_logs(&sessions, cutoff, false).unwrap();

        assert_eq!((files, runs), (0, 0), "nothing pruned when age is unknown");
        assert!(no_manifest.join("a.stderr.redacted.log").exists());
        assert!(stderr_exists(&sessions, "bad-ts"));
    }

    #[test]
    fn missing_agent_runs_dir_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let (files, runs) =
            gc_expired_stderr_logs(&tmp.path().join("sessions"), Utc::now(), false).unwrap();
        assert_eq!((files, runs), (0, 0));
    }

    /// Write a run dir with an arbitrary raw manifest body plus a stderr log.
    /// PD-04: pruning an expired run must reclaim the findings BLOB it
    /// owned, not just its directory — otherwise the retention window
    /// expires while the bytes stay in the repository forever.
    ///
    /// Two safety properties matter as much as the reclamation itself, and
    /// both are asserted here: an oid a SURVIVING run still names is not
    /// garbage, and an oid that history reaches is never deleted (findings
    /// blobs are content-addressed, so one can collide with an ordinary
    /// committed file blob — removing it would corrupt the repository).
    #[test]
    #[serial]
    fn findings_gc_reclaims_unreferenced_blobs_but_never_reachable_ones() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        let sessions_root = repo.path().join(".libra").join("sessions");
        let storage_path = repo.path().join(".libra");

        // Three findings blobs: one only an expired run names (garbage), one
        // a live run also names (shared), one that history reaches.
        let write_blob = |content: &[u8]| -> String {
            use std::io::Write as _;
            let blob =
                git_internal::internal::object::blob::Blob::from_content_bytes(content.to_vec());
            let oid = blob.id.to_string();
            let dir = storage_path.join("objects").join(&oid[..2]);
            fs::create_dir_all(&dir).unwrap();
            let mut raw = format!("blob {}\0", content.len()).into_bytes();
            raw.extend_from_slice(content);
            let mut enc =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(&raw).unwrap();
            fs::write(dir.join(&oid[2..]), enc.finish().unwrap()).unwrap();
            oid
        };
        let garbage_oid = write_blob(b"findings only the expired run owns");
        let shared_oid = write_blob(b"findings a live run still points at");

        let write_run_with_oid = |run_id: &str, updated_at: &str, oid: &str| {
            let run_dir = sessions_root.join("agent-runs").join(run_id);
            fs::create_dir_all(&run_dir).unwrap();
            fs::write(
                run_dir.join("manifest.json"),
                format!(
                    "{{\"schema_version\":1,\"run_id\":\"{run_id}\",\"kind\":\"review\",\
                     \"terminal_state\":\"success\",\"updated_at\":\"{updated_at}\",\
                     \"findings_oid\":\"{oid}\"}}"
                ),
            )
            .unwrap();
        };
        // Expired runs.
        write_run_with_oid("run-garbage", "2020-01-01T00:00:00Z", &garbage_oid);
        write_run_with_oid("run-shared-old", "2020-01-01T00:00:00Z", &shared_oid);
        // A run inside the retention window that still names `shared_oid`.
        write_run_with_oid("run-shared-live", "2999-01-01T00:00:00Z", &shared_oid);

        let cutoff = DateTime::parse_from_rfc3339("2021-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let outcome = rt
            .block_on(gc_expired_findings_runs(&sessions_root, cutoff, false))
            .expect("gc");

        assert_eq!(outcome.runs_pruned, 2, "both expired runs are pruned");
        assert_eq!(
            outcome.objects_pruned, 1,
            "only the blob nothing else references is reclaimed"
        );
        let object_file = |oid: &str| storage_path.join("objects").join(&oid[..2]).join(&oid[2..]);
        assert!(
            !object_file(&garbage_oid).exists(),
            "the unreferenced findings blob is gone"
        );
        assert!(
            object_file(&shared_oid).exists(),
            "a blob a SURVIVING run still names must not be deleted"
        );
    }

    fn write_run_raw(sessions_root: &Path, run_id: &str, manifest_body: &str) {
        let run_dir = sessions_root.join("agent-runs").join(run_id);
        fs::create_dir_all(run_dir.join("reviewers")).unwrap();
        fs::write(run_dir.join("manifest.json"), manifest_body).unwrap();
        fs::write(run_dir.join("reviewers/a.stderr.redacted.log"), "x").unwrap();
    }

    #[test]
    fn foreign_kind_and_corrupt_terminal_state_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let old = "2000-01-01T00:00:00.000000Z";
        // kind is not review/investigate → out of scope for this GC → skipped.
        write_run_raw(
            &sessions,
            "foreign-kind",
            &format!(
                "{{\"kind\":\"other\",\"terminal_state\":\"success\",\"updated_at\":\"{old}\"}}"
            ),
        );
        // terminal_state is an object, not a string → manifest fails to
        // deserialize → skipped (never mistaken for a terminal state).
        write_run_raw(
            &sessions,
            "corrupt-terminal",
            &format!(
                "{{\"kind\":\"review\",\"terminal_state\":{{\"bad\":true}},\"updated_at\":\"{old}\"}}"
            ),
        );
        // terminal_state is a string but NOT one of review's real terminal
        // states → treated as non-terminal → skipped.
        write_run_raw(
            &sessions,
            "garbage-terminal",
            &format!(
                "{{\"kind\":\"review\",\"terminal_state\":\"garbage\",\"updated_at\":\"{old}\"}}"
            ),
        );

        let cutoff = Utc::now() - chrono::Duration::days(30);
        let (files, runs) = gc_expired_stderr_logs(&sessions, cutoff, false).unwrap();
        assert_eq!((files, runs), (0, 0));
        assert!(stderr_exists(&sessions, "foreign-kind"));
        assert!(stderr_exists(&sessions, "corrupt-terminal"));
        assert!(stderr_exists(&sessions, "garbage-terminal"));
    }

    /// The escape hatch the maintenance walk's new fail-closed posture
    /// requires: a run directory with NO manifest is abandoned, and
    /// `agent clean` retires it once it is past the retention cutoff.
    ///
    /// Without this, one crashed run makes the repository permanently
    /// unprunable — the walk refuses to guess at its roots, and nothing can
    /// remove it. With it, the removal is explicit (only under the user's
    /// `agent clean`) and bounded (only past the cutoff), which is what the
    /// two halves below pin: the SAME directory survives a cutoff it is
    /// newer than and is retired by one it is older than.
    #[test]
    fn a_manifestless_run_is_retired_only_when_past_the_cutoff() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let runs_root = sessions.join("agent-runs");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        // No manifest: an interrupted run.
        let abandoned = runs_root.join("run-abandoned");
        fs::create_dir_all(&abandoned).unwrap();
        fs::write(abandoned.join("findings.md"), "orphaned findings").unwrap();

        // A manifest EXISTS but names a foreign kind: out of scope for this
        // GC, NOT abandoned — it must survive both cutoffs.
        write_run_raw(
            &sessions,
            "run-foreign",
            "{\"kind\":\"other\",\"terminal_state\":\"success\",\"updated_at\":\"2000-01-01T00:00:00Z\"}",
        );
        let foreign = runs_root.join("run-foreign");

        // A manifest that is valid JSON but NOT an object. The root walk
        // refuses these, so without a retirement route they would wedge
        // pruning forever — they are retired on the same terms as a missing
        // manifest.
        write_run_raw(&sessions, "run-nonobject", "[]");
        let nonobject = runs_root.join("run-nonobject");

        // Cutoff in the past: the directory is NEWER than it — still in
        // flight as far as retention is concerned, so nothing is taken.
        let past_cutoff = Utc::now() - chrono::Duration::days(30);
        let outcome = rt
            .block_on(gc_expired_findings_runs(&sessions, past_cutoff, false))
            .expect("gc");
        assert_eq!(
            outcome.runs_pruned, 0,
            "a manifest-less run newer than the cutoff must be left alone"
        );
        assert!(abandoned.exists(), "still within the retention window");
        assert!(nonobject.exists(), "and so is an unreadable one");

        // Cutoff ahead of now: the same directory is past it and is retired.
        let future_cutoff = Utc::now() + chrono::Duration::days(1);
        let outcome = rt
            .block_on(gc_expired_findings_runs(&sessions, future_cutoff, false))
            .expect("gc");
        assert_eq!(
            outcome.runs_pruned, 2,
            "both unenumerable runs are retired: no manifest, and a non-object one"
        );
        assert!(
            !abandoned.exists(),
            "the abandoned run directory is removed"
        );
        assert!(
            !nonobject.exists(),
            "and so is the one whose manifest the root walk refuses to read"
        );
        assert!(
            foreign.exists(),
            "a run with a readable manifest this GC does not own is not 'abandoned'"
        );
    }

    /// `--dry-run` counts the abandoned run without removing it.
    #[test]
    fn a_manifestless_run_is_only_counted_under_dry_run() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let abandoned = sessions.join("agent-runs").join("run-abandoned");
        fs::create_dir_all(&abandoned).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let outcome = rt
            .block_on(gc_expired_findings_runs(
                &sessions,
                Utc::now() + chrono::Duration::days(1),
                true,
            ))
            .expect("gc");
        assert_eq!(outcome.runs_pruned, 1);
        assert!(abandoned.exists(), "a preview must not delete anything");
    }

    #[test]
    fn investigate_terminal_state_is_recognized() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let old = "2000-01-01T00:00:00.000000Z";
        // A real investigate terminal state ("quorum") IS eligible.
        write_run_raw(
            &sessions,
            "aged-investigate",
            &format!(
                "{{\"kind\":\"investigate\",\"terminal_state\":\"quorum\",\"updated_at\":\"{old}\"}}"
            ),
        );
        let cutoff = Utc::now() - chrono::Duration::days(30);
        let (files, runs) = gc_expired_stderr_logs(&sessions, cutoff, false).unwrap();
        assert_eq!((files, runs), (1, 1));
        assert!(!stderr_exists(&sessions, "aged-investigate"));
    }

    #[test]
    fn stderr_cutoff_for_days_never_panics_on_huge_window() {
        // A normal window resolves to a concrete cutoff in the past.
        let cutoff = stderr_cutoff_for_days(30).expect("30-day window is representable");
        assert!(cutoff < Utc::now());
        // A window larger than the representable date range yields None (the
        // GC treats it as a no-op) rather than panicking on date overflow.
        assert!(stderr_cutoff_for_days(u32::MAX).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_reviewers_dir_is_never_followed() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let old = "2000-01-01T00:00:00.000000Z";

        // A victim directory OUTSIDE the store with a matching stderr log.
        let victim = tmp.path().join("victim");
        fs::create_dir_all(&victim).unwrap();
        let victim_log = victim.join("a.stderr.redacted.log");
        fs::write(&victim_log, "secret").unwrap();

        // A legit-named terminal run whose `reviewers` is a symlink to victim.
        let run_dir = sessions.join("agent-runs").join("evil-run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(
            run_dir.join("manifest.json"),
            format!(
                "{{\"kind\":\"review\",\"terminal_state\":\"success\",\"updated_at\":\"{old}\"}}"
            ),
        )
        .unwrap();
        symlink(&victim, run_dir.join("reviewers")).unwrap();

        let cutoff = Utc::now() - chrono::Duration::days(30);
        let (files, runs) = gc_expired_stderr_logs(&sessions, cutoff, false).unwrap();
        assert_eq!(
            (files, runs),
            (0, 0),
            "a symlinked reviewers dir must never be followed"
        );
        assert!(victim_log.exists(), "a file outside the store must survive");
    }
}
