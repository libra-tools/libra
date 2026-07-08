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

use std::{fs, path::Path, sync::Arc};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, Statement};
use serde::{Deserialize, Serialize};

use super::CleanArgs;
use crate::{
    internal::{
        ai::history::{CheckpointPruneGuardError, HistoryManager},
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
    note: &'static str,
}

pub async fn execute_safe(args: CleanArgs, output: &OutputConfig) -> CliResult<()> {
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
        .query_one(Statement::from_string(backend, session_filter))
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
    // manager; the stderr-window GC below prunes reviewer diagnostics here.
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

    let history =
        HistoryManager::new_with_ref(storage, repo_path, Arc::new(conn.clone()), TRACES_BRANCH);
    let prune = history
        .prune_checkpoint_commits(&checkpoint_ids)
        .await
        .map_err(map_prune_error)?;

    let (stderr_logs_pruned, stderr_runs_pruned) = match stderr_cutoff {
        Some(Some(cutoff)) => gc_expired_stderr_logs(&sessions_root, cutoff)?,
        // Not a `--gc` run, or a retention window so large nothing is expired.
        _ => (0, 0),
    };

    emit_report(
        &CleanReport {
            sessions_inspected,
            temporary_checkpoints_dropped: prune.removed_checkpoints,
            retained_checkpoints_rewritten: prune.rewritten_checkpoints,
            traces_ref_rewritten: prune.ref_rewritten,
            window_guard: prune.window_guard,
            object_index_rows_dropped: prune.deleted_object_index_rows,
            stderr_logs_pruned,
            stderr_runs_pruned,
            note,
        },
        output,
    )
}

/// Map a prune failure to an actionable CLI error, keeping the AG-20
/// window-guard refusals distinguishable (they are deterministic and
/// user-resolvable, not storage corruption).
fn map_prune_error(err: anyhow::Error) -> CliError {
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
    conn.query_one(stmt)
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
        .query_all(Statement::from_sql_and_values(
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
    })
}

/// Delete every `*.stderr.redacted.log` regular file directly under
/// `reviewers_dir`, returning the count removed. A missing dir yields 0; only
/// the stderr-log suffix is matched, so stdout logs and the aggregate record
/// stay. Symlinks are never followed: a symlinked `reviewers` directory is
/// refused outright (so a hostile run dir cannot redirect deletion outside the
/// store), and within the dir only regular files are removed.
fn prune_stderr_logs_in(reviewers_dir: &Path) -> CliResult<u64> {
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
fn gc_expired_stderr_logs(sessions_root: &Path, cutoff: DateTime<Utc>) -> CliResult<(u64, u64)> {
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
        let pruned_here = prune_stderr_logs_in(&run_dir.join("reviewers"))?;
        if pruned_here > 0 {
            files_pruned += pruned_here;
            runs_pruned += 1;
        }
    }
    Ok((files_pruned, runs_pruned))
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
        .query_all(Statement::from_string(backend, query))
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
    use super::*;

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
        let (files, runs) = gc_expired_stderr_logs(&sessions, cutoff).unwrap();

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
        let (files, runs) = gc_expired_stderr_logs(&sessions, cutoff).unwrap();

        assert_eq!((files, runs), (0, 0), "nothing pruned when age is unknown");
        assert!(no_manifest.join("a.stderr.redacted.log").exists());
        assert!(stderr_exists(&sessions, "bad-ts"));
    }

    #[test]
    fn missing_agent_runs_dir_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let (files, runs) =
            gc_expired_stderr_logs(&tmp.path().join("sessions"), Utc::now()).unwrap();
        assert_eq!((files, runs), (0, 0));
    }

    /// Write a run dir with an arbitrary raw manifest body plus a stderr log.
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
        let (files, runs) = gc_expired_stderr_logs(&sessions, cutoff).unwrap();
        assert_eq!((files, runs), (0, 0));
        assert!(stderr_exists(&sessions, "foreign-kind"));
        assert!(stderr_exists(&sessions, "corrupt-terminal"));
        assert!(stderr_exists(&sessions, "garbage-terminal"));
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
        let (files, runs) = gc_expired_stderr_logs(&sessions, cutoff).unwrap();
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
        let (files, runs) = gc_expired_stderr_logs(&sessions, cutoff).unwrap();
        assert_eq!(
            (files, runs),
            (0, 0),
            "a symlinked reviewers dir must never be followed"
        );
        assert!(victim_log.exists(), "a file outside the store must survive");
    }
}
