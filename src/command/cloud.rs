//! Cloud backup command for synchronizing repository data to Cloudflare D1 and R2.
//!
//! This module provides subcommands for:
//! - `libra cloud sync` - Sync local DB to D1, objects to R2
//! - `libra cloud restore` - Restore from D1/R2
//! - `libra cloud status` - Show sync status

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    path::PathBuf,
    sync::Arc,
};

use clap::{Parser, Subcommand};
use git_internal::hash::ObjectHash;
#[cfg(test)]
use sea_orm::TransactionTrait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Schema, Set,
    sea_query::Expr,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    internal::{
        config::ConfigKv,
        db,
        model::{object_index, reference},
    },
    utils::{
        d1_client::{
            AgentCaptureGenerationManifest, AgentCaptureGenerationRow,
            AgentCaptureRestoreCatalogRows, AgentCheckpointPruneTombstoneRow, AgentCheckpointV2Row,
            AgentImportTombstoneRow, AgentSessionV2Row, AgentSubagentContentClaimRow,
            AgentSubagentContentRevisionRow, AgentSubagentLinkRow, D1Client, D1Error,
            ObjectIndexRow,
        },
        error::{CliError, CliResult, StableErrorCode, emit_warning},
        output::{OutputConfig, ProgressMode, emit_json_data},
        path,
        storage::{Storage, local::LocalStorage, remote::RemoteStorage},
        util,
    },
};

mod agent_capture;
mod metadata;
mod object_format;
mod restore;
mod snapshot;
mod sync;

use agent_capture::*;
use object_format::REBACKUP_HINT;
pub(crate) use restore::*;
pub(crate) use sync::run_cloud_sync;

/// One legacy Claude reasoning revision and the old checkpoint it cites.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ClaudeLegacyReasoningMirrorRef {
    pub checkpoint_id: String,
    pub session_id: String,
    pub content_digest: String,
    pub tree_oid: String,
}

/// Read local Claude legacy reasoning revisions and require each old checkpoint.
pub async fn claude_legacy_reasoning_mirror_refs(
    db_conn: &sea_orm::DatabaseConnection,
) -> Result<Vec<ClaudeLegacyReasoningMirrorRef>, String> {
    agent_capture::validated_claude_legacy_reasoning_mirror_refs(db_conn).await
}

/// `--help` examples shown in `libra cloud --help` output.
///
/// `cloud` exposes three sub-commands: `sync` (push local repo to
/// D1 + R2), `restore` (pull a remote repo back down), `status`
/// (compare local objects against the cloud manifest). The banner pins
/// the most common invocation per sub-command plus a force-sync and a
/// JSON variant so users can map intent to invocation without reading
/// the design doc. Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
pub const CLOUD_EXAMPLES: &str = "\
EXAMPLES:
    libra cloud status                            Show cloud sync coverage for current repo
    libra cloud status --verbose                  Per-object detail of synced/missing objects
    libra cloud sync                              Sync only objects missing from R2
    libra cloud sync --force                      Re-upload every object regardless of cloud state
    libra cloud restore --name my-project         Restore by repository name
    libra cloud restore --repo-id <uuid>          Restore by repository ID
    libra cloud restore --name my-project --metadata-only
                                                  Restore object index only (no blob payloads)
    libra cloud --json sync                       Structured JSON output for agents
    libra cloud sync --progress=json              NDJSON progress events for automation";

#[derive(Parser, Debug)]
#[command(about = "Cloud backup and restore operations", after_help = CLOUD_EXAMPLES)]
pub struct CloudArgs {
    #[command(subcommand)]
    pub command: CloudCommand,
}

#[derive(Subcommand, Debug)]
pub enum CloudCommand {
    /// Sync local repository to cloud (D1 + R2)
    Sync(SyncArgs),
    /// Restore repository from cloud
    Restore(RestoreArgs),
    /// Show cloud sync status
    Status(StatusArgs),
}

#[derive(Parser, Debug)]
pub struct SyncArgs {
    /// Force sync all objects, not just unsynced ones
    #[arg(long)]
    pub force: bool,

    /// Number of objects to upload per D1/R2 batch (default: 50)
    #[arg(long, value_name = "N", default_value = "50")]
    pub batch_size: usize,
}

#[derive(Parser, Debug)]
pub struct RestoreArgs {
    /// Repository ID (UUID) to restore from the cloud (mutually exclusive with --name)
    #[arg(
        long,
        value_name = "UUID",
        required_unless_present = "name",
        conflicts_with = "name"
    )]
    pub repo_id: Option<String>,

    /// Repository name to restore from the cloud (mutually exclusive with --repo-id)
    #[arg(
        long,
        value_name = "NAME",
        required_unless_present = "repo_id",
        conflicts_with = "repo_id"
    )]
    pub name: Option<String>,

    /// Only restore metadata (object index), not objects
    #[arg(long)]
    pub metadata_only: bool,
}

#[derive(Parser, Debug)]
pub struct StatusArgs {
    /// Show detailed status for each object
    #[arg(long)]
    pub verbose: bool,
}

// ───────────────────────────────────────────────────────────────────
// Phase 1 (publish.md) — structured `cloud sync` helper.
//
// `run_cloud_sync` is the headless entry that `libra publish` will
// reuse in Phase 4+. It performs the full object + metadata + agent
// capture sync but emits human-readable progress through a callback
// trait instead of `println!`/`eprintln!` directly. The legacy
// `execute_sync` wraps this helper with `ConsoleCloudSyncProgress` so
// `libra cloud sync` keeps its original output verbatim.

/// Inputs for `run_cloud_sync`.
#[derive(Debug, Clone)]
pub struct CloudSyncContext {
    /// Number of objects per batch when streaming to R2 / D1.
    pub batch_size: usize,
    /// Re-sync every object regardless of `is_synced`.
    pub force: bool,
}

/// Metadata-sync outcome surfaced in [`CloudSyncReport`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum MetadataSyncOutcome {
    /// Skipped because object failures preceded it.
    NotRun,
    /// Refs payload uploaded; references emitted = refs count.
    Synced { references: usize },
    /// Metadata hash unchanged since the last sync; nothing uploaded.
    Skipped,
}

/// Agent-capture mirroring outcome surfaced in [`CloudSyncReport`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum AgentCaptureSyncOutcome {
    /// Skipped because object failures preceded it.
    NotRun,
    /// Local schema predates the agent-session/checkpoint catalog
    /// migration; nothing to mirror.
    SkippedLegacySchema,
    /// All applicable catalog and subagent-companion rows were mirrored.
    /// A per-row failure makes the aggregate outcome [`Self::Failed`], so
    /// completed counts are expected to have zero failures.
    Completed {
        sessions_synced: usize,
        sessions_failed: usize,
        checkpoints_synced: usize,
        checkpoints_failed: usize,
    },
    /// Hard error (table-existence query, ensure-table call, ...). `error`
    /// is the fixed, content-free reason from
    /// `agent_capture_sync_failure_reason`.
    Failed { error: String },
}

/// Final outcome of a `run_cloud_sync` call. Hard errors short-
/// circuit and surface as `Err`; recoverable per-object failures live
/// in `failed_count` and the metadata/agent_capture variants.
#[derive(Debug, Clone)]
pub struct CloudSyncReport {
    pub repo_id: String,
    pub project_name: String,
    pub total_unsynced: usize,
    pub synced_count: usize,
    pub failed_count: usize,
    pub metadata: MetadataSyncOutcome,
    pub agent_capture: AgentCaptureSyncOutcome,
    /// One note per snapshot spool directory left behind: this run's own,
    /// when it could not be removed (PI-09), and each leftover of an earlier
    /// run that was kept, not inspected or not removable (PI-25).
    pub spool_warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CloudSyncOutput {
    repo_id: String,
    project_name: String,
    total_unsynced: usize,
    synced_count: usize,
    failed_count: usize,
    metadata: CloudMetadataSyncOutput,
    agent_capture: CloudAgentCaptureSyncOutput,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CloudMetadataSyncOutput {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    references: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct CloudAgentCaptureSyncOutput {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sessions_synced: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sessions_failed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checkpoints_synced: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checkpoints_failed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CloudRestoreOutput {
    repo_id: String,
    /// Authoritative repository object-format from D1 `repositories`
    /// (`sha1` / `sha256` / `blake3`) when the backup wrote one (B3-09).
    #[serde(skip_serializing_if = "Option::is_none")]
    object_format: Option<String>,
    metadata_only: bool,
    total_objects: usize,
    indexes_restored: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    object_restore: Option<CloudRestoreObjectOutput>,
    metadata: CloudRestoreMetadataOutput,
    agent_capture: CloudRestoreAgentCaptureOutput,
}

#[derive(Debug, Clone, Serialize)]
struct CloudRestoreObjectOutput {
    downloaded: usize,
    skipped: usize,
    failed: usize,
}

#[derive(Debug, Clone, Serialize)]
struct CloudRestoreMetadataOutput {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CloudRestoreAgentCaptureOutput {
    status: String,
}

/// Summary returned after restoring Git objects listed in D1 `object_index`.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub(crate) struct ObjectRestoreReport {
    pub downloaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub warnings: Vec<String>,
}

/// Progress callbacks fired during a `run_cloud_sync` call.
///
/// All methods have empty default impls — implementors only override
/// the events they care about. `ConsoleCloudSyncProgress` mirrors the
/// pre-Phase-1 `libra cloud sync` output verbatim. Phase 4+ publish
/// callers may pass a quieter or structured implementation.
pub trait CloudSyncProgress: Send + Sync {
    fn on_starting(&self) {}
    fn on_no_objects(&self) {}
    fn on_object_total(&self, total: usize) {
        let _ = total;
    }
    fn on_batch_progress(&self, synced: usize, total: usize, failed: usize) {
        let _ = (synced, total, failed);
    }
    fn on_object_error(&self, oid: &str, err: &str) {
        let _ = (oid, err);
    }
    fn on_local_status_warning(&self, oid: &str, err: &str) {
        let _ = (oid, err);
    }
    /// The object phase ended (metadata and the agent-capture mirror still
    /// follow); never a statement that the sync succeeded (PI-28).
    fn on_sync_complete(&self, synced: usize, failed: usize) {
        let _ = (synced, failed);
    }
    /// The whole sync succeeded: every object, the metadata and the
    /// agent-capture mirror. Called only after the result is known.
    fn on_sync_succeeded(&self) {}
    fn on_metadata_starting(&self) {}
    fn on_metadata_skipped(&self) {}
    fn on_metadata_synced(&self, references: usize) {
        let _ = references;
    }
    fn on_agent_capture_starting(&self) {}
    fn on_agent_capture_session_warning(&self, session_id: &str, err: &str) {
        let _ = (session_id, err);
    }
    fn on_agent_capture_checkpoint_warning(&self, checkpoint_id: &str, err: &str) {
        let _ = (checkpoint_id, err);
    }
    fn on_agent_capture_done(
        &self,
        sessions_synced: usize,
        sessions_failed: usize,
        checkpoints_synced: usize,
        checkpoints_failed: usize,
    ) {
        let _ = (
            sessions_synced,
            sessions_failed,
            checkpoints_synced,
            checkpoints_failed,
        );
    }
    /// Additive M5 completion callback. The default forwards the original
    /// four counters so existing embedders keep receiving completion events.
    fn on_agent_capture_done_with_subagents(
        &self,
        sessions_synced: usize,
        sessions_failed: usize,
        checkpoints_synced: usize,
        checkpoints_failed: usize,
        subagent_rows_synced: usize,
        subagent_rows_failed: usize,
    ) {
        self.on_agent_capture_done(
            sessions_synced,
            sessions_failed,
            checkpoints_synced,
            checkpoints_failed,
        );
        let _ = (subagent_rows_synced, subagent_rows_failed);
    }
    fn on_agent_capture_warning(&self, err: &str) {
        let _ = err;
    }
    /// A snapshot spool directory could not be removed (PI-09). The default
    /// goes through [`emit_warning`]: stderr for humans, `data.warnings[]`
    /// under `--json`.
    fn on_spool_warning(&self, note: &str) {
        emit_warning(note);
    }
    /// Whether this renderer owns an NDJSON stream, so a spool dropped on a
    /// panic or cancellation must report through it too (PI-09).
    fn spool_warnings_as_ndjson(&self) -> bool {
        false
    }
}

/// The `--progress=json` event for a snapshot spool that could not be
/// removed; one NDJSON line, so plain text never lands on that stream.
fn spool_warning_event(note: &str) -> serde_json::Value {
    serde_json::json!({
        "event": "cloud_sync.spool.warning",
        "message": note,
    })
}

/// The `--progress=json` event for a warning from a background cleanup of
/// the metadata upload (PI-27); one NDJSON line, like the spool warning.
fn metadata_warning_event(message: &str) -> serde_json::Value {
    serde_json::json!({
        "event": "cloud_sync.metadata.warning",
        "message": message,
    })
}

/// Test-only capture of human progress output (stdout and stderr lines), in
/// place of the terminal (PI-28).
#[cfg(test)]
static CONSOLE_PROGRESS_CAPTURE: std::sync::Mutex<Option<Vec<String>>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn capture_console_progress(line: &str) -> bool {
    if let Ok(mut capture) = CONSOLE_PROGRESS_CAPTURE.lock()
        && let Some(lines) = capture.as_mut()
    {
        lines.push(line.to_string());
        return true;
    }
    false
}

/// One human progress line on stdout.
fn console_progress_line(line: &str) {
    #[cfg(test)]
    if capture_console_progress(line) {
        return;
    }
    println!("{line}");
}

/// One human progress note on stderr that is shown but, as before, not
/// counted for `--exit-code-on-warning` (the agent-capture mirror warning).
fn console_progress_note(line: String) {
    #[cfg(test)]
    if capture_console_progress(&line) {
        return;
    }
    eprintln!("{line}");
}

/// One human progress error or warning on stderr, through the legacy
/// stderr renderer (a `warning: ` line is also counted for
/// `--exit-code-on-warning`).
fn console_progress_stderr(line: String) {
    #[cfg(test)]
    if capture_console_progress(&line) {
        return;
    }
    crate::utils::error::emit_legacy_stderr(line);
}

/// Console implementation of the human `libra cloud sync` output: the
/// legacy progress lines, except that the object phase ends with
/// `Objects: N synced, M failed` and only a fully successful sync ends with
/// `Sync complete.` (PI-28).
pub struct ConsoleCloudSyncProgress;

impl CloudSyncProgress for ConsoleCloudSyncProgress {
    fn on_starting(&self) {
        console_progress_line("Starting cloud sync...");
    }
    fn on_no_objects(&self) {
        console_progress_line("No objects to sync.");
    }
    fn on_object_total(&self, total: usize) {
        console_progress_line(&format!("Found {total} objects to sync."));
    }
    fn on_batch_progress(&self, synced: usize, total: usize, failed: usize) {
        console_progress_line(&format!(
            "Progress: {synced}/{total} synced, {failed} failed"
        ));
    }
    fn on_object_error(&self, oid: &str, err: &str) {
        console_progress_stderr(format!("error: failed to sync {oid}: {err}"));
    }
    fn on_local_status_warning(&self, oid: &str, err: &str) {
        console_progress_stderr(format!(
            "warning: failed to update local sync status for {oid}: {err}"
        ));
    }
    fn on_sync_complete(&self, synced: usize, failed: usize) {
        // The object phase only: metadata and the agent-capture mirror can
        // still fail, so this never says the sync is complete (PI-28).
        console_progress_line(&format!("Objects: {synced} synced, {failed} failed"));
    }
    fn on_sync_succeeded(&self) {
        console_progress_line("Sync complete.");
    }
    fn on_metadata_starting(&self) {
        console_progress_line("Syncing metadata...");
    }
    fn on_metadata_skipped(&self) {
        console_progress_line("Metadata unchanged, skipping upload.");
    }
    fn on_metadata_synced(&self, references: usize) {
        console_progress_line(&format!("Metadata synced ({references} references)."));
    }
    fn on_agent_capture_starting(&self) {
        console_progress_line("Syncing agent capture catalog to D1...");
    }
    fn on_agent_capture_session_warning(&self, _session_id: &str, _err: &str) {
        self.on_agent_capture_warning(AGENT_CAPTURE_SYNC_FAILURE_MESSAGE);
    }
    fn on_agent_capture_checkpoint_warning(&self, _checkpoint_id: &str, _err: &str) {
        self.on_agent_capture_warning(AGENT_CAPTURE_SYNC_FAILURE_MESSAGE);
    }
    fn on_agent_capture_done_with_subagents(
        &self,
        sessions_synced: usize,
        sessions_failed: usize,
        checkpoints_synced: usize,
        checkpoints_failed: usize,
        subagent_rows_synced: usize,
        subagent_rows_failed: usize,
    ) {
        console_progress_line(&format!(
            "Agent capture sync: {sessions_synced} sessions ({sessions_failed} failed), \
             {checkpoints_synced} checkpoints ({checkpoints_failed} failed), \
             {subagent_rows_synced} subagent companion rows ({subagent_rows_failed} failed)."
        ));
    }
    fn on_agent_capture_warning(&self, err: &str) {
        console_progress_note(format!("warning: agent capture sync incomplete: {err}"));
    }
}

struct SilentCloudSyncProgress;

impl CloudSyncProgress for SilentCloudSyncProgress {}

struct JsonCloudSyncProgress;

/// Test-only capture of `--progress=json` lines, in place of stderr.
#[cfg(test)]
static JSON_PROGRESS_CAPTURE: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

impl JsonCloudSyncProgress {
    fn emit(event: serde_json::Value) {
        #[cfg(test)]
        if let Ok(mut capture) = JSON_PROGRESS_CAPTURE.lock()
            && let Some(lines) = capture.as_mut()
        {
            lines.push(event.to_string());
            return;
        }
        // Never panics: this also runs from `Drop` during unwinding.
        let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("{event}\n"));
    }
}

impl CloudSyncProgress for JsonCloudSyncProgress {
    fn on_starting(&self) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.start",
        }));
    }
    fn on_no_objects(&self) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.objects.none",
        }));
    }
    fn on_object_total(&self, total: usize) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.objects.total",
            "total": total,
        }));
    }
    fn on_batch_progress(&self, synced: usize, total: usize, failed: usize) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.objects.progress",
            "synced": synced,
            "total": total,
            "failed": failed,
        }));
    }
    fn on_object_error(&self, oid: &str, err: &str) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.objects.error",
            "oid": oid,
            "error": err,
        }));
    }
    fn on_local_status_warning(&self, oid: &str, err: &str) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.objects.warning",
            "oid": oid,
            "error": err,
        }));
    }
    fn on_sync_complete(&self, synced: usize, failed: usize) {
        // The object phase only; metadata and the agent-capture mirror can
        // still fail, so it is "finished", never "complete" (PI-28).
        Self::emit(serde_json::json!({
            "event": "cloud_sync.objects.finished",
            "synced": synced,
            "failed": failed,
        }));
    }
    fn on_sync_succeeded(&self) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.complete",
        }));
    }
    fn on_metadata_starting(&self) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.metadata.start",
        }));
    }
    fn on_metadata_skipped(&self) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.metadata.skipped",
        }));
    }
    fn on_metadata_synced(&self, references: usize) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.metadata.synced",
            "references": references,
        }));
    }
    fn on_agent_capture_starting(&self) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.agent_capture.start",
        }));
    }
    fn on_agent_capture_session_warning(&self, _session_id: &str, _err: &str) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.agent_capture.warning",
            "error": AGENT_CAPTURE_SYNC_FAILURE_MESSAGE,
        }));
    }
    fn on_agent_capture_checkpoint_warning(&self, _checkpoint_id: &str, _err: &str) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.agent_capture.warning",
            "error": AGENT_CAPTURE_SYNC_FAILURE_MESSAGE,
        }));
    }
    fn on_agent_capture_done_with_subagents(
        &self,
        sessions_synced: usize,
        sessions_failed: usize,
        checkpoints_synced: usize,
        checkpoints_failed: usize,
        subagent_rows_synced: usize,
        subagent_rows_failed: usize,
    ) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.agent_capture.finished",
            "sessions_synced": sessions_synced,
            "sessions_failed": sessions_failed,
            "checkpoints_synced": checkpoints_synced,
            "checkpoints_failed": checkpoints_failed,
            "subagent_rows_synced": subagent_rows_synced,
            "subagent_rows_failed": subagent_rows_failed,
        }));
    }
    fn on_agent_capture_warning(&self, err: &str) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.agent_capture.warning",
            "error": err,
        }));
    }
    fn on_spool_warning(&self, note: &str) {
        // Counted for `--exit-code-on-warning` and kept for a `--json`
        // envelope, but printed only as an NDJSON event.
        crate::utils::output::record_warning_message(note.to_string());
        Self::emit(spool_warning_event(note));
    }
    fn spool_warnings_as_ndjson(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Serialize)]
struct CloudStatusOutput {
    repo_id: String,
    total_objects: usize,
    synced: usize,
    pending: usize,
    synced_percent: usize,
    by_type: Vec<CloudStatusTypeOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unsynced_objects: Vec<CloudStatusObjectOutput>,
}

#[derive(Debug, Clone, Serialize)]
struct CloudStatusTypeOutput {
    object_type: String,
    total: usize,
    synced: usize,
    pending: usize,
}

#[derive(Debug, Clone, Serialize)]
struct CloudStatusObjectOutput {
    oid: String,
    object_type: String,
    size: i64,
}

/// Execute cloud command
pub async fn execute(args: CloudArgs) -> CliResult<()> {
    match args.command {
        CloudCommand::Sync(sync_args) => execute_sync(sync_args)
            .await
            .map_err(|e| cloud_cli_error_typed("sync", e))?,
        CloudCommand::Restore(restore_args) => execute_restore(restore_args)
            .await
            .map_err(|e| cloud_cli_error_typed("restore", e))?,
        CloudCommand::Status(status_args) => execute_status(status_args)
            .await
            .map_err(|e| cloud_cli_error_typed("status", e))?,
    }

    Ok(())
}

pub async fn execute_safe(args: CloudArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    match args.command {
        CloudCommand::Sync(sync_args) => {
            let ctx = CloudSyncContext {
                batch_size: sync_args.batch_size,
                force: sync_args.force,
            };
            let progress = sync_progress_for(output);
            let outcome = run_cloud_sync(ctx, progress).await;
            if let Some(structured) = conclude_sync(outcome, progress, output)? {
                emit_json_data("cloud.sync", &structured, output)?;
            }
        }
        CloudCommand::Restore(restore_args) => {
            if output.is_json() || output.quiet {
                let report = run_cloud_restore(restore_args)
                    .await
                    .map_err(|e| cloud_cli_error_typed("restore", e))?;
                render_cloud_restore_output(&report, output)?;
            } else {
                execute_restore(restore_args)
                    .await
                    .map_err(|e| cloud_cli_error_typed("restore", e))?;
            }
        }
        CloudCommand::Status(status_args) => {
            let status = run_cloud_status(status_args).await?;
            render_cloud_status_output(&status, output)?;
        }
    }

    Ok(())
}

/// Typed classification of cloud operation failures, derived from the raw error
/// string emitted by the underlying D1/R2/repo-name/metadata/agent-capture
/// helpers. Centralises the string-matching previously scattered through
/// [`cloud_cli_error`] so the mapping to [`StableErrorCode`] has a single
/// source of truth and is unit-testable in isolation.
///
/// Variants document the trigger conditions; the contained `String` carries
/// the original detail so the human / JSON error envelope can preserve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CloudError {
    /// Required env / vault key set missing — carries the comma-separated key
    /// list parsed out of the underlying "Missing: …" message.
    MissingEnv {
        detail: String,
        missing_keys: Vec<String>,
        /// Notes added by [`CloudError::with_note`], shown with the error.
        notes: Vec<String>,
    },
    /// Repository name is already claimed by another repository in D1.
    NameAlreadyTaken(String),
    /// Repository name is not registered in D1.
    NameNotFound(String),
    /// Some objects failed to sync or restore during a bulk transfer. Both
    /// directions surface as the same conflict-blocked stable code.
    PartialTransfer(String),
    /// D1 (control-plane / metadata DB) protocol / API failure.
    D1(String),
    /// R2 (object store) transport / reachability failure.
    R2(String),
    /// Cloud catalog lacks authoritative `object_format` (or conflicts with
    /// OID widths). Maps to `LBR-REPO-002` with a re-backup hint (B3-14).
    AmbiguousObjectFormat(String),
    /// An existing D1 capture table's `agent_kind` CHECK rejects a kind that
    /// local capture rows use, or cannot be read (PI-18). Raised before any
    /// remote write; maps to `LBR-CONFLICT-002` with the backfill hint.
    RemoteCaptureKindRefused(String),
    /// A local capture session uses a kind outside the cloud capture
    /// allowlist (PI-05). Raised before credentials, D1 or R2; maps to
    /// `LBR-CONFLICT-002` with the local-only capture hints.
    LocalCaptureKindRefused(String),
    /// Anything else — kept as the original detail string.
    Generic(String),
}

impl From<std::io::Error> for CloudError {
    fn from(error: std::io::Error) -> Self {
        CloudError::Generic(error.to_string())
    }
}

/// Recovery for [`CloudError::RemoteCaptureKindRefused`]. Default sync only
/// uploads rows with `is_synced = 0`, so a new database needs `--force`.
pub(super) const REMOTE_CAPTURE_KIND_RECOVERY_HINT: &str = "create a new D1 database, point LIBRA_D1_DATABASE_ID at it, and run `libra cloud sync --force` to backfill it completely";

/// First hint for [`CloudError::LocalCaptureKindRefused`]: the refusal
/// lasts while local-only capture exists, until cloud support for that kind
/// ships (plan-20260911 DEFER-PI-08). It never suggests deleting sessions.
pub(super) const LOCAL_CAPTURE_KIND_REFUSAL_HINT: &str = "while this repository holds local-only capture sessions of a kind the cloud mirror does not support, `libra cloud sync` keeps refusing until a later Libra release adds complete cloud support for that kind; `--force` does not bypass it";

/// Second hint for [`CloudError::LocalCaptureKindRefused`]: the interim
/// backup path.
pub(super) const LOCAL_CAPTURE_KIND_BACKUP_HINT: &str = "until then, back up the whole repository, including its `.libra` directory, with an external backup tool of your choice";

type CloudResult<T> = std::result::Result<T, CloudError>;

/// Convert an untrusted D1 failure into a stable public diagnostic.
///
/// Cloudflare may include SQL fragments or submitted values in `message`.
/// Agent-capture rows include privacy-sensitive commitments, so neither the
/// original message nor its Debug representation may cross a CLI, progress, or
/// JSON boundary. The numeric D1 code is deliberately retained as a safe
/// correlation value for support and retry decisions.
pub(super) fn cloud_d1_failure(operation: &'static str, error: &D1Error) -> CloudError {
    tracing::warn!(
        d1_operation = operation,
        d1_error_code = error.code,
        "cloud D1 request failed"
    );
    CloudError::D1(format!(
        "cloud D1 {operation} failed (D1 code {}); verify cloud connectivity and credentials, then retry",
        error.code
    ))
}

/// Fixed reason for a failed agent-capture row batch (the user-approved
/// `cloud_sync.agent_capture.warning` text). It carries no session or
/// checkpoint id and no remote D1 text.
pub(super) const AGENT_CAPTURE_SYNC_FAILURE_MESSAGE: &str = "agent-capture catalog sync failed; inspect the local capture catalog and retry `libra cloud sync`";

/// Fixed success-path warning: legacy V1 subagent evidence the remote catalog
/// never held stays local-only (see `PendingSubagentRows`).
pub(super) const AGENT_CAPTURE_LEGACY_LOCAL_ONLY_MESSAGE: &str = "legacy V1 subagent evidence keyed by an unkeyed source digest stays local-only and was not mirrored; its checkpoints and all other agent-capture rows were synced";

/// Map a failed agent-capture row batch to the fixed row-failure reason. The
/// D1 code goes to tracing only; the D1 message may quote submitted rows.
pub(super) fn agent_capture_row_failure(operation: &'static str, error: &D1Error) -> CloudError {
    tracing::warn!(
        d1_operation = operation,
        d1_error_code = error.code,
        "cloud D1 agent-capture row batch failed"
    );
    CloudError::D1(AGENT_CAPTURE_SYNC_FAILURE_MESSAGE.to_string())
}

/// The reason reported for a failed agent-capture sync phase: its progress
/// warning, `--json` `agent_capture.error`, and final error. Every
/// agent-capture `CloudError` is a fixed diagnostic parameterised only by
/// static labels and counts (D1 and local driver text is redacted at its
/// source), so the closed set of reasons stays content-free. A missing-env
/// detail can embed arbitrary configuration text and keeps the fixed message.
pub(super) fn agent_capture_sync_failure_reason(error: &CloudError) -> String {
    match error {
        CloudError::MissingEnv { .. } => AGENT_CAPTURE_SYNC_FAILURE_MESSAGE.to_string(),
        other => other.to_string(),
    }
}

/// Final `cloud sync` error for a failed agent-capture phase, after its
/// progress has been rendered (`LBR-CONFLICT-002`).
fn agent_capture_mirror_failure(reason: &str) -> CloudError {
    CloudError::PartialTransfer(format!("agent capture mirror failed: {reason}"))
}

impl From<String> for CloudError {
    fn from(error: String) -> Self {
        if let Some(missing_keys) = parse_missing_cloud_env_keys(&error) {
            CloudError::MissingEnv {
                detail: error,
                missing_keys,
                notes: Vec::new(),
            }
        } else if error.contains("already taken by another repository") {
            CloudError::NameAlreadyTaken(error)
        } else if error.contains("Repository with name '") && error.contains("not found") {
            CloudError::NameNotFound(error)
        } else if error.contains("objects failed to sync")
            || error.contains("objects failed to restore")
        {
            CloudError::PartialTransfer(error)
        } else if error.contains("D1") {
            CloudError::D1(error)
        } else if error.contains("R2") {
            CloudError::R2(error)
        } else {
            CloudError::Generic(error)
        }
    }
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CloudError::MissingEnv {
                detail,
                missing_keys,
                notes,
            } => {
                if !detail.is_empty() {
                    write!(f, "{detail}")?;
                } else if missing_keys.is_empty() {
                    write!(f, "missing cloud environment configuration")?;
                } else {
                    write!(f, "Missing: {}", missing_keys.join(", "))?;
                }
                for note in notes {
                    write!(f, "; {note}")?;
                }
                Ok(())
            }
            CloudError::NameAlreadyTaken(detail)
            | CloudError::NameNotFound(detail)
            | CloudError::PartialTransfer(detail)
            | CloudError::D1(detail)
            | CloudError::R2(detail)
            | CloudError::AmbiguousObjectFormat(detail)
            | CloudError::RemoteCaptureKindRefused(detail)
            | CloudError::LocalCaptureKindRefused(detail)
            | CloudError::Generic(detail) => write!(f, "{detail}"),
        }
    }
}

impl CloudError {
    /// Append `note` to the error's detail, keeping its variant (and so its
    /// stable code).
    pub(super) fn with_note(self, note: &str) -> Self {
        let add = |detail: String| format!("{detail}; {note}");
        match self {
            // Kept apart from the detail, which the CLI error replaces with
            // the missing keys, so the note stays visible (PI-25).
            CloudError::MissingEnv {
                detail,
                missing_keys,
                mut notes,
            } => {
                notes.push(note.to_string());
                CloudError::MissingEnv {
                    detail,
                    missing_keys,
                    notes,
                }
            }
            CloudError::NameAlreadyTaken(detail) => CloudError::NameAlreadyTaken(add(detail)),
            CloudError::NameNotFound(detail) => CloudError::NameNotFound(add(detail)),
            CloudError::PartialTransfer(detail) => CloudError::PartialTransfer(add(detail)),
            CloudError::D1(detail) => CloudError::D1(add(detail)),
            CloudError::R2(detail) => CloudError::R2(add(detail)),
            CloudError::AmbiguousObjectFormat(detail) => {
                CloudError::AmbiguousObjectFormat(add(detail))
            }
            CloudError::RemoteCaptureKindRefused(detail) => {
                CloudError::RemoteCaptureKindRefused(add(detail))
            }
            CloudError::LocalCaptureKindRefused(detail) => {
                CloudError::LocalCaptureKindRefused(add(detail))
            }
            CloudError::Generic(detail) => CloudError::Generic(add(detail)),
        }
    }

    /// Map the typed cloud error onto a [`CliError`] for the given top-level
    /// `operation` ("sync" / "restore" / "status").
    fn into_cli_error(self, operation: &str) -> CliError {
        match self {
            CloudError::MissingEnv {
                detail,
                missing_keys,
                notes,
            } => {
                let message = if missing_keys.is_empty() {
                    format!("missing cloud configuration for {operation}")
                } else {
                    format!(
                        "missing cloud configuration for {operation}: {}",
                        missing_keys.join(", ")
                    )
                };
                notes.into_iter().fold(
                    CliError::auth(message)
                        .with_stable_code(StableErrorCode::AuthMissingCredentials)
                        .with_detail("missing_keys", missing_keys)
                        .with_detail("raw_detail", detail)
                        .with_hint(
                            "set the missing variables in env or vault.env.* before retrying.",
                        ),
                    |error, note| error.with_hint(note),
                )
            }
            CloudError::NameAlreadyTaken(detail) => CliError::conflict(detail)
                .with_stable_code(StableErrorCode::ConflictOperationBlocked),
            CloudError::NameNotFound(detail) => {
                CliError::fatal(detail).with_stable_code(StableErrorCode::CliInvalidTarget)
            }
            CloudError::PartialTransfer(detail) => CliError::conflict(detail)
                .with_stable_code(StableErrorCode::ConflictOperationBlocked),
            CloudError::D1(detail) => {
                CliError::network(detail).with_stable_code(StableErrorCode::NetworkProtocol)
            }
            CloudError::R2(detail) => {
                CliError::network(detail).with_stable_code(StableErrorCode::NetworkUnavailable)
            }
            CloudError::AmbiguousObjectFormat(detail) => CliError::fatal(detail)
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint(REBACKUP_HINT),
            CloudError::RemoteCaptureKindRefused(detail) => CliError::conflict(detail)
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint(REMOTE_CAPTURE_KIND_RECOVERY_HINT),
            CloudError::LocalCaptureKindRefused(detail) => CliError::conflict(detail)
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint(LOCAL_CAPTURE_KIND_REFUSAL_HINT)
                .with_hint(LOCAL_CAPTURE_KIND_BACKUP_HINT),
            CloudError::Generic(detail) => CliError::fatal(format!("{operation} failed: {detail}")),
        }
    }
}

#[cfg(test)]
fn cloud_cli_error(operation: &str, error: String) -> CliError {
    cloud_cli_error_typed(operation, error.into())
}

/// Map an already-typed [`CloudError`] onto a [`CliError`] for the given
/// top-level `operation` without re-running the String classification path.
///
/// Prefer this at call sites that already know which CloudError variant they
/// want to surface (e.g. a partial-sync result builder constructing
/// `CloudError::PartialTransfer` directly). String-shaped error sites should
/// continue to use [`cloud_cli_error`] until their callee is migrated to
/// return `CloudError` natively.
fn cloud_cli_error_typed(operation: &str, error: CloudError) -> CliError {
    error
        .into_cli_error(operation)
        .with_detail("operation", operation)
        .with_detail("component", "cloud")
}

fn parse_missing_cloud_env_keys(error: &str) -> Option<Vec<String>> {
    let (_, missing_raw) = error.split_once("Missing: ")?;
    let keys = missing_raw
        .split(',')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if keys.is_empty() { None } else { Some(keys) }
}

fn cloud_sync_output_from_report(report: &CloudSyncReport) -> CloudSyncOutput {
    let metadata = match &report.metadata {
        MetadataSyncOutcome::NotRun => CloudMetadataSyncOutput {
            status: "not_run".to_string(),
            references: None,
        },
        MetadataSyncOutcome::Synced { references } => CloudMetadataSyncOutput {
            status: "synced".to_string(),
            references: Some(*references),
        },
        MetadataSyncOutcome::Skipped => CloudMetadataSyncOutput {
            status: "skipped".to_string(),
            references: None,
        },
    };
    let agent_capture = match &report.agent_capture {
        AgentCaptureSyncOutcome::NotRun => CloudAgentCaptureSyncOutput {
            status: "not_run".to_string(),
            sessions_synced: None,
            sessions_failed: None,
            checkpoints_synced: None,
            checkpoints_failed: None,
            error: None,
        },
        AgentCaptureSyncOutcome::SkippedLegacySchema => CloudAgentCaptureSyncOutput {
            status: "skipped_legacy_schema".to_string(),
            sessions_synced: None,
            sessions_failed: None,
            checkpoints_synced: None,
            checkpoints_failed: None,
            error: None,
        },
        AgentCaptureSyncOutcome::Completed {
            sessions_synced,
            sessions_failed,
            checkpoints_synced,
            checkpoints_failed,
        } => CloudAgentCaptureSyncOutput {
            status: "completed".to_string(),
            sessions_synced: Some(*sessions_synced),
            sessions_failed: Some(*sessions_failed),
            checkpoints_synced: Some(*checkpoints_synced),
            checkpoints_failed: Some(*checkpoints_failed),
            error: None,
        },
        AgentCaptureSyncOutcome::Failed { error } => CloudAgentCaptureSyncOutput {
            status: "failed".to_string(),
            sessions_synced: None,
            sessions_failed: None,
            checkpoints_synced: None,
            checkpoints_failed: None,
            error: Some(error.clone()),
        },
    };
    CloudSyncOutput {
        repo_id: report.repo_id.clone(),
        project_name: report.project_name.clone(),
        total_unsynced: report.total_unsynced,
        synced_count: report.synced_count,
        failed_count: report.failed_count,
        metadata,
        agent_capture,
        warnings: report.spool_warnings.clone(),
    }
}

/// Progress renderer for an output mode: NDJSON events for
/// `--progress=json`, nothing for `--json`/`--quiet`, console lines otherwise.
fn sync_progress_for(output: &OutputConfig) -> &'static dyn CloudSyncProgress {
    if matches!(output.progress, ProgressMode::Json) {
        &JsonCloudSyncProgress
    } else if output.is_json() || output.quiet {
        &SilentCloudSyncProgress
    } else {
        &ConsoleCloudSyncProgress
    }
}

/// What a finished sync renders in an output mode: the error it stands for
/// (see [`sync_report_failure`]), the structured `cloud.sync` data under
/// `--json`, or nothing more (human and quiet modes have already printed
/// progress, and any warning went through `emit_warning`).
fn finish_sync(
    report: &CloudSyncReport,
    output: &OutputConfig,
) -> Result<Option<CloudSyncOutput>, CloudError> {
    if let Some(error) = sync_report_failure(report) {
        return Err(error);
    }
    Ok(output
        .is_json()
        .then(|| cloud_sync_output_from_report(report)))
}

/// Conclude a sync run for `output`: the CLI error it stands for (a hard
/// error, failed objects or a failed agent-capture mirror), or — only then,
/// once the whole run is known to have succeeded — the success announced
/// through `progress` and the `--json` data to emit. Shared by
/// `execute_safe` and the tests, so a failed sync never reports success
/// (PI-28).
fn conclude_sync(
    outcome: CloudResult<CloudSyncReport>,
    progress: &dyn CloudSyncProgress,
    output: &OutputConfig,
) -> CliResult<Option<CloudSyncOutput>> {
    let report = outcome.map_err(|e| cloud_cli_error_typed("sync", e))?;
    let structured = finish_sync(&report, output).map_err(|e| cloud_cli_error_typed("sync", e))?;
    progress.on_sync_succeeded();
    Ok(structured)
}

/// Execute sync command - uploads objects to R2, indexes to D1, and registers project name
async fn execute_sync(args: SyncArgs) -> CloudResult<()> {
    let ctx = CloudSyncContext {
        batch_size: args.batch_size,
        force: args.force,
    };
    let report = run_cloud_sync(ctx, &ConsoleCloudSyncProgress).await?;

    // Preserve the pre-Phase-1 exit semantics: per-object failures
    // surface as a hard error after the human-readable summary has
    // already been emitted by `ConsoleCloudSyncProgress`.
    match sync_report_failure(&report) {
        Some(error) => Err(error),
        None => {
            ConsoleCloudSyncProgress.on_sync_succeeded();
            Ok(())
        }
    }
}

/// The error a finished sync report stands for, if any: object failures,
/// then a failed agent-capture mirror. A snapshot spool that could not be
/// removed is named in that error, since no success output is rendered.
fn sync_report_failure(report: &CloudSyncReport) -> Option<CloudError> {
    let error = if report.failed_count > 0 {
        CloudError::PartialTransfer(format!("{} objects failed to sync", report.failed_count))
    } else if let AgentCaptureSyncOutcome::Failed { error } = &report.agent_capture {
        agent_capture_mirror_failure(error)
    } else {
        return None;
    };
    Some(
        report
            .spool_warnings
            .iter()
            .fold(error, |error, note| error.with_note(note)),
    )
}

/// Restore the Git objects described by D1 `object_index` rows from R2
/// into local object storage.
///
/// The helper preserves the legacy cloud-restore semantics: object-level
/// transfer, hash, and local-write failures are accumulated in the report,
/// while malformed hex in D1 remains a hard metadata error.
/// Execute restore command - resolves project name (if provided) and restores from D1/R2.
/// Reject a stale remote capture generation before generic cloud restore can
/// download its objects or apply refs metadata. A local prune tombstone is a
/// durable deletion intent; until the next sync publishes it, the previous
/// complete generation must not be allowed to resurrect that checkpoint.
async fn preflight_agent_capture_prune_fences(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    repo_id: &str,
) -> CloudResult<()> {
    use sea_orm::Statement;

    // Prune tombstones are catalog rows: `capture.db` after the split.
    let catalog = agent_capture::local_capture_catalog(db_conn).await?;
    let db_conn = &catalog;
    let backend = db_conn.get_database_backend();
    let tombstone_table_present = db_conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'agent_checkpoint_prune_tombstone' LIMIT 1"
                .to_string(),
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!(
                "inspect local checkpoint prune schema before cloud restore: {error}"
            ))
        })?
        .is_some();
    if !tombstone_table_present {
        return Ok(());
    }
    let rows = db_conn
        .query_all_raw(Statement::from_string(
            backend,
            format!(
                "SELECT checkpoint_id FROM agent_checkpoint_prune_tombstone
                 ORDER BY checkpoint_id LIMIT {}",
                AGENT_CAPTURE_RESTORE_MAX_ROWS.saturating_add(1)
            ),
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!(
                "read local checkpoint prune fences before cloud restore: {error}"
            ))
        })?;
    if rows.len() > AGENT_CAPTURE_RESTORE_MAX_ROWS {
        return Err(CloudError::PartialTransfer(format!(
            "local checkpoint prune fences exceed the {}-row restore safety bound; run `libra cloud sync` before restoring",
            AGENT_CAPTURE_RESTORE_MAX_ROWS
        )));
    }
    let checkpoint_ids = rows
        .into_iter()
        .map(|row| {
            row.try_get_by::<String, _>("checkpoint_id")
                .map_err(|error| {
                    CloudError::Generic(format!(
                        "decode local checkpoint prune fence before cloud restore: {error}"
                    ))
                })
        })
        .collect::<CloudResult<Vec<_>>>()?;
    if checkpoint_ids.is_empty() {
        return Ok(());
    }

    let generation_table_present = d1_client
        .agent_capture_generation_table_exists()
        .await
        .map_err(|error| {
            cloud_d1_failure(
                "probe agent-capture generation before prune preflight",
                &error,
            )
        })?;
    if !generation_table_present {
        let has_capture_rows = d1_client
            .agent_capture_catalog_has_rows(repo_id)
            .await
            .map_err(|error| {
                cloud_d1_failure(
                    "probe unmanifested agent capture before prune preflight",
                    &error,
                )
            })?;
        if !has_capture_rows {
            return Ok(());
        }
        return Err(CloudError::PartialTransfer(
            "cannot verify local checkpoint prune fences because the remote capture has no generation manifest; run `libra cloud sync` before restoring"
                .to_string(),
        ));
    }

    for _ in 0..3 {
        let before = d1_client
            .get_agent_capture_generation(repo_id)
            .await
            .map_err(|error| {
                cloud_d1_failure(
                    "read agent-capture generation before prune preflight",
                    &error,
                )
            })?;
        let Some(before) = before else {
            let has_capture_rows = d1_client
                .agent_capture_catalog_has_rows(repo_id)
                .await
                .map_err(|error| {
                    cloud_d1_failure(
                        "probe agent capture without a generation before prune preflight",
                        &error,
                    )
                })?;
            if !has_capture_rows {
                return Ok(());
            }
            return Err(CloudError::PartialTransfer(
                "cannot verify local checkpoint prune fences because the remote capture has no completed generation; run `libra cloud sync` before restoring"
                    .to_string(),
            ));
        };
        if before.state != "complete" {
            return Err(CloudError::PartialTransfer(
                "remote agent capture publication is incomplete; retry `libra cloud sync`, then restore"
                    .to_string(),
            ));
        }
        let conflicts = d1_client
            .find_agent_checkpoint_ids_by_ids(repo_id, &checkpoint_ids)
            .await
            .map_err(|error| {
                cloud_d1_failure("compare checkpoint prune fences with agent capture", &error)
            })?;
        let after = d1_client
            .get_agent_capture_generation(repo_id)
            .await
            .map_err(|error| {
                cloud_d1_failure(
                    "recheck agent-capture generation after prune preflight",
                    &error,
                )
            })?;
        if after.as_ref() != Some(&before) {
            continue;
        }
        reject_local_prune_conflicts(&checkpoint_ids, &conflicts)?;
        return Ok(());
    }
    Err(CloudError::PartialTransfer(
        "remote agent capture changed during three checkpoint-prune preflight reads; retry when cloud sync is idle"
            .to_string(),
    ))
}

fn reject_local_prune_conflicts(
    local_checkpoint_ids: &[String],
    remote_checkpoint_ids: &HashSet<String>,
) -> CloudResult<()> {
    if local_checkpoint_ids
        .iter()
        .any(|checkpoint_id| remote_checkpoint_ids.contains(checkpoint_id.as_str()))
    {
        return Err(CloudError::PartialTransfer(
            "a remote checkpoint was already pruned locally; run `libra cloud sync` to publish the prune tombstone before restoring"
                .to_string(),
        ));
    }
    Ok(())
}

/// Execute status command - shows sync status
async fn execute_status(args: StatusArgs) -> CloudResult<()> {
    let status = run_cloud_status(args)
        .await
        .map_err(|error| CloudError::Generic(error.to_string()))?;
    render_cloud_status_human(&status);
    Ok(())
}

async fn run_cloud_status(args: StatusArgs) -> CliResult<CloudStatusOutput> {
    // Get database connection
    let db_conn = db::get_db_conn_instance().await;

    // Count total and synced objects
    let repo_id = ConfigKv::get("libra.repoid")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .unwrap_or_else(|| "unknown-repo".to_string());

    let all_objects = object_index::Entity::find()
        .filter(object_index::Column::RepoId.eq(&repo_id))
        .all(&db_conn)
        .await
        .map_err(|e| {
            CliError::fatal(format!("failed to query cloud object index: {e}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;

    let synced_count = all_objects.iter().filter(|o| o.is_synced == 1).count();
    let unsynced_count = all_objects.len() - synced_count;

    // Group by type
    let mut by_type: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for obj in &all_objects {
        let entry = by_type.entry(obj.o_type.clone()).or_insert((0, 0));
        entry.0 += 1;
        if obj.is_synced == 1 {
            entry.1 += 1;
        }
    }
    let by_type = by_type
        .into_iter()
        .map(|(object_type, (total, synced))| CloudStatusTypeOutput {
            object_type,
            total,
            synced,
            pending: total - synced,
        })
        .collect();
    let unsynced_objects = if args.verbose {
        all_objects
            .iter()
            .filter(|o| o.is_synced == 0)
            .take(20)
            .map(|obj| CloudStatusObjectOutput {
                oid: obj.o_id.clone(),
                object_type: obj.o_type.clone(),
                size: obj.o_size,
            })
            .collect()
    } else {
        Vec::new()
    };

    Ok(CloudStatusOutput {
        repo_id,
        total_objects: all_objects.len(),
        synced: synced_count,
        pending: unsynced_count,
        synced_percent: if all_objects.is_empty() {
            0
        } else {
            synced_count * 100 / all_objects.len()
        },
        by_type,
        unsynced_objects,
    })
}

fn render_cloud_status_output(status: &CloudStatusOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("cloud.status", status, output);
    }
    if output.quiet {
        return Ok(());
    }

    render_cloud_status_human(status);
    Ok(())
}

fn render_cloud_status_human(status: &CloudStatusOutput) {
    println!("Cloud Sync Status:");
    println!("  Repo ID:       {}", status.repo_id);
    println!("  Total objects: {}", status.total_objects);
    println!(
        "  Synced:        {} ({}%)",
        status.synced, status.synced_percent
    );
    println!("  Pending:       {}", status.pending);

    println!("\nBy object type:");
    for entry in &status.by_type {
        println!(
            "  {}: {}/{} synced",
            entry.object_type, entry.synced, entry.total
        );
    }

    if !status.unsynced_objects.is_empty() {
        println!("\nUnsynced objects:");
        for obj in &status.unsynced_objects {
            println!("  {} ({}, {} bytes)", obj.oid, obj.object_type, obj.size);
        }
        if status.pending > status.unsynced_objects.len() {
            println!(
                "  ... and {} more",
                status.pending - status.unsynced_objects.len()
            );
        }
    }
}

fn cloud_local_db_path() -> CloudResult<PathBuf> {
    let storage = util::try_get_storage_path(None).map_err(|e| {
        CloudError::Generic(format!("failed to resolve current repository storage: {e}"))
    })?;
    Ok(storage.join(util::DATABASE))
}

async fn resolve_cloud_env(
    name: &str,
    local_db_path: Option<&std::path::Path>,
) -> CloudResult<Option<String>> {
    let local_target = match local_db_path {
        Some(db_path) => crate::internal::config::LocalIdentityTarget::ExplicitDb(db_path),
        None => crate::internal::config::LocalIdentityTarget::CurrentRepo,
    };

    crate::internal::config::resolve_env_for_target(name, local_target)
        .await
        .map_err(|e| {
            CloudError::Generic(format!(
                "failed to resolve '{name}' from env or config: {e}"
            ))
        })
}

async fn resolve_required_cloud_env(
    name: &str,
    local_db_path: Option<&std::path::Path>,
) -> CloudResult<String> {
    match resolve_cloud_env(name, local_db_path).await? {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(CloudError::MissingEnv {
            detail: format!("Missing: {name}"),
            missing_keys: vec![name.to_string()],
            notes: Vec::new(),
        }),
    }
}

/// Create R2 remote storage from environment variables and config.
async fn create_r2_storage(repo_id: &str) -> CloudResult<RemoteStorage> {
    let local_db_path = cloud_local_db_path()?;
    create_r2_storage_for_db_path(repo_id, &local_db_path).await
}

async fn create_r2_storage_for_db_path(
    repo_id: &str,
    local_db_path: &std::path::Path,
) -> CloudResult<RemoteStorage> {
    let store = create_r2_object_store_for_db_path(local_db_path).await?;
    Ok(RemoteStorage::new_with_prefix(store, repo_id.to_string()))
}

async fn create_r2_object_store_for_db_path(
    local_db_path: &std::path::Path,
) -> CloudResult<Arc<dyn object_store::ObjectStore>> {
    let endpoint =
        resolve_required_cloud_env("LIBRA_STORAGE_ENDPOINT", Some(local_db_path)).await?;
    let bucket = resolve_required_cloud_env("LIBRA_STORAGE_BUCKET", Some(local_db_path)).await?;
    let access_key =
        resolve_required_cloud_env("LIBRA_STORAGE_ACCESS_KEY", Some(local_db_path)).await?;
    let secret_key =
        resolve_required_cloud_env("LIBRA_STORAGE_SECRET_KEY", Some(local_db_path)).await?;
    let region = resolve_cloud_env("LIBRA_STORAGE_REGION", Some(local_db_path))
        .await?
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "auto".to_string());

    let s3 = object_store::aws::AmazonS3Builder::new()
        .with_bucket_name(&bucket)
        .with_region(&region)
        .with_endpoint(&endpoint)
        .with_access_key_id(&access_key)
        .with_secret_access_key(&secret_key)
        .with_virtual_hosted_style_request(false)
        .build()
        .map_err(|e| CloudError::R2(format!("Failed to build R2 client: {}", e)))?;

    Ok(Arc::new(s3))
}

async fn validate_cloud_backup_env(skip_r2: bool) -> CloudResult<()> {
    let mut required = vec![
        "LIBRA_D1_ACCOUNT_ID",
        "LIBRA_D1_API_TOKEN",
        "LIBRA_D1_DATABASE_ID",
    ];

    if !skip_r2 {
        required.extend_from_slice(&[
            "LIBRA_STORAGE_ENDPOINT",
            "LIBRA_STORAGE_BUCKET",
            "LIBRA_STORAGE_ACCESS_KEY",
            "LIBRA_STORAGE_SECRET_KEY",
        ]);
    }

    let local_db_path = cloud_local_db_path()?;
    let mut missing: Vec<String> = Vec::new();
    for key in required {
        match resolve_cloud_env(key, Some(&local_db_path)).await? {
            Some(value) if !value.is_empty() => {}
            _ => missing.push(key.to_string()),
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        let detail = format!("Missing: {}", missing.join(", "));
        Err(CloudError::MissingEnv {
            detail,
            missing_keys: missing,
            notes: Vec::new(),
        })
    }
}

/// Resolve or mint the repository's stable `libra.repoid` identifier.
///
/// Always returns a value (mints a fresh UUIDv4 when no usable id is on file
/// and ignores best-effort persistence failures), so the return type is bare
/// `String` rather than `Result<String, _>`. Cloud sync uses this as the
/// stable key for D1 + R2 namespacing.
async fn ensure_repo_id() -> String {
    if let Some(entry) = ConfigKv::get("libra.repoid").await.ok().flatten()
        && !entry.value.is_empty()
        && entry.value != "unknown-repo"
    {
        return entry.value;
    }

    let repo_id = Uuid::new_v4().to_string();
    let _ = ConfigKv::set("libra.repoid", &repo_id, false).await;

    let db_conn = db::get_db_conn_instance().await;
    let _ = object_index::Entity::update_many()
        .filter(object_index::Column::RepoId.eq("unknown-repo"))
        .col_expr(object_index::Column::RepoId, Expr::value(repo_id.clone()))
        .exec(&db_conn)
        .await;

    repo_id
}

/// CEX-EntireIO §10.2 / §14.3: restore the local agent-session/checkpoint
/// catalog and applicable M5 subagent companion relations from D1.
///
/// Mirrors [`sync_frozen_agent_capture_tables`] in reverse: lists D1 rows for the
/// repo and inserts them into the local SQLite catalog.
///
/// Behaviour, refined per Codex Phase-3.5b review:
/// - Bails with an explicit warning when the local schema predates the
///   migration that creates these tables (was a silent `Ok(())` previously
///   — Codex Q4).
/// - Hard-fails the aggregate when any row can't be restored — restore is
///   stricter than the upload-side soft-fail because a missing session
///   would leave orphan checkpoints in the local catalog (Codex Q2).
/// - Checkpoint upserts use explicit `ON CONFLICT(checkpoint_id) DO UPDATE
///   SET …` rather than `INSERT OR REPLACE` so the row's CASCADE delete
///   semantics are preserved on conflict (Codex Q1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentCaptureRestoreOutcome {
    /// A coherent completed generation was validated and atomically installed,
    /// including its authoritative traces ref (which may intentionally be
    /// empty).
    GenerationInstalled,
    /// No generation owns the capture ref, so legacy refs metadata remains the
    /// only durable pointer to any pre-manifest traces history.
    NoGeneration,
}

fn validate_missing_capture_manifest(has_capture_rows: bool) -> CloudResult<()> {
    if has_capture_rows {
        Err(CloudError::Generic(
            "remote agent capture has rows but no completed generation manifest; run `libra cloud sync` with the current Libra version before restoring"
                .to_string(),
        ))
    } else {
        Ok(())
    }
}

/// PD-03: UPSERT restored session tombstones into the local
/// `agent_import_tombstone` table (same idempotent shape the local erase
/// writes: newest `erased_at` wins and retired fingerprints are cleared). A
/// legacy local schema without the table skips with a warning — the
/// restore itself already filtered the erased rows.
async fn persist_local_import_tombstones(
    db_conn: &sea_orm::DatabaseConnection,
    tombstones: &[AgentImportTombstoneRow],
) -> CloudResult<()> {
    use sea_orm::{ConnectionTrait, Statement, Value};

    if tombstones.is_empty() {
        return Ok(());
    }
    let backend = db_conn.get_database_backend();
    let table_present = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'agent_import_tombstone' LIMIT 1",
            [],
        ))
        .await
        .map_err(|error| {
            local_agent_capture_restore_failure("probe import-tombstone schema", error)
        })?
        .is_some();
    if !table_present {
        emit_warning(
            "local agent_import_tombstone table absent — restored erasure fences were \
             not persisted locally; upgrade libra and rerun `libra cloud restore`",
        );
        return Ok(());
    }
    for row in tombstones {
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO agent_import_tombstone (
                    tombstone_id, agent_kind, provider_session_id,
                    erased_session_id, source_fingerprint, erased_at
                 ) VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT(agent_kind, provider_session_id) DO UPDATE SET
                    erased_session_id = excluded.erased_session_id,
                    source_fingerprint = NULL,
                    erased_at = MAX(agent_import_tombstone.erased_at, excluded.erased_at)",
                [
                    uuid::Uuid::new_v4().to_string().into(),
                    row.agent_kind.clone().into(),
                    row.provider_session_id.clone().into(),
                    row.erased_session_id.clone().into(),
                    Value::from(Option::<String>::None),
                    row.erased_at.into(),
                ],
            ))
            .await
            .map_err(|_| {
                CloudError::Generic(
                    "failed to persist a restored session tombstone; repair the local catalog and retry cloud restore"
                        .to_string(),
                )
            })?;
    }
    Ok(())
}

async fn load_remote_agent_capture_rows(
    d1_client: &D1Client,
    repo_id: &str,
    subagent_content_present: bool,
) -> CloudResult<(AgentCaptureSnapshot, usize)> {
    let AgentCaptureRestoreCatalogRows {
        sessions,
        checkpoints,
        prune_tombstones: _,
        import_tombstones,
        claims,
        revisions,
        links,
        remaining_rows,
    } = d1_client
        .list_agent_capture_restore_catalog_rows(
            repo_id,
            subagent_content_present,
            AGENT_CAPTURE_RESTORE_MAX_ROWS,
        )
        .await
        .map_err(|error| cloud_d1_failure("list agent-capture restore catalog", &error))?;
    // PD-03 tombstone-first: an erased session never restores, even when
    // a stale mirror still carries its rows.
    let erased_session_ids: HashSet<&str> = import_tombstones
        .iter()
        .map(|row| row.erased_session_id.as_str())
        .collect();
    let erased_checkpoint_ids: HashSet<String> = checkpoints
        .iter()
        .filter(|row| erased_session_ids.contains(row.session_id.as_str()))
        .map(|row| row.checkpoint_id.clone())
        .collect();
    let sessions: Vec<AgentSessionV2Row> = sessions
        .into_iter()
        .filter(|row| !erased_session_ids.contains(row.session_id.as_str()))
        .collect();
    let checkpoints: Vec<AgentCheckpointV2Row> = checkpoints
        .into_iter()
        .filter(|row| !erased_session_ids.contains(row.session_id.as_str()))
        .collect();
    let claims: Vec<AgentSubagentContentClaimRow> = claims
        .into_iter()
        .filter(|row| !erased_session_ids.contains(row.parent_session_id.as_str()))
        .collect();
    let revisions: Vec<AgentSubagentContentRevisionRow> = revisions
        .into_iter()
        .filter(|row| !erased_checkpoint_ids.contains(row.checkpoint_id.as_str()))
        .collect();
    let links: Vec<AgentSubagentLinkRow> = links
        .into_iter()
        .filter(|row| !erased_checkpoint_ids.contains(row.content_checkpoint_id.as_str()))
        .collect();
    Ok((
        AgentCaptureSnapshot {
            sessions,
            checkpoints,
            import_tombstones,
            claims,
            revisions,
            links,
            required_oids: HashSet::new(),
            ..AgentCaptureSnapshot::default()
        },
        remaining_rows,
    ))
}

/// Connection-bound core of [`restore_agent_capture_from_d1`]. Extracted
/// per Codex Phase-3.5b review Q5 so the per-row INSERT logic is
/// testable against an in-memory SQLite without a live D1 endpoint.
///
/// Returns aggregate counts via the printed report and a hard error if
/// any row failed to insert. Caller decides what to do with the error
/// (e.g. defer it past the worktree restore).
pub(crate) async fn restore_metadata_models_with_capture_policy(
    db_conn: &sea_orm::DatabaseConnection,
    references: Vec<reference::Model>,
    strict: bool,
    defer_capture_refs: bool,
) -> CloudResult<Vec<reference::Model>> {
    let mut deferred = Vec::new();
    for ref_model in references {
        // The capture ref is fenced by the agent-capture generation and must
        // only move atomically with its validated checkpoint catalog. Generic
        // metadata may outlive a local prune or belong to an older generation.
        if defer_capture_refs
            && ref_model.kind == reference::ConfigKind::Branch
            && ref_model.remote.is_none()
            && ref_model.name.as_deref().is_some_and(|name| {
                name == crate::internal::branch::TRACES_BRANCH
                    || name == crate::internal::branch::LEGACY_TRACES_BRANCH
            })
        {
            deferred.push(ref_model);
            continue;
        }
        // Build query to find matching reference
        let remote_filter = match &ref_model.remote {
            Some(remote) => reference::Column::Remote.eq(remote),
            None => reference::Column::Remote.is_null(),
        };
        let mut query = reference::Entity::find()
            .filter(reference::Column::Kind.eq(ref_model.kind.clone()))
            .filter(remote_filter);

        // Head references are unique by kind and remote, name is the mutable current branch.
        // For other types, match by name as well.
        if ref_model.kind != reference::ConfigKind::Head {
            query = match &ref_model.name {
                Some(name) => query.filter(reference::Column::Name.eq(name)),
                None => query.filter(reference::Column::Name.is_null()),
            };
        }

        let existing = query
            .one(db_conn)
            .await
            .map_err(|e| CloudError::Generic(format!("DB error: {}", e)))?;

        if let Some(existing_model) = existing {
            let mut active: reference::ActiveModel = existing_model.into();
            // Keep mutable HEAD name (attached branch) consistent during restore.
            active.name = Set(ref_model.name.clone());
            active.commit = Set(ref_model.commit.clone());
            active.remote = Set(ref_model.remote.clone());
            if let Err(e) = active.update(db_conn).await {
                let message = format!("failed to update reference {:?}: {}", ref_model.name, e);
                if strict {
                    return Err(CloudError::Generic(message));
                }
                eprintln!("warning: {message}");
            }
        } else {
            let active = reference::ActiveModel {
                name: Set(ref_model.name.clone()),
                kind: Set(ref_model.kind.clone()),
                commit: Set(ref_model.commit.clone()),
                remote: Set(ref_model.remote.clone()),
                ..Default::default()
            };
            if let Err(e) = active.insert(db_conn).await {
                let message = format!("failed to insert reference {:?}: {}", ref_model.name, e);
                if strict {
                    return Err(CloudError::Generic(message));
                }
                eprintln!("warning: {message}");
            }
        }
    }
    Ok(deferred)
}

async fn restore_legacy_capture_refs_if_unowned(
    db_conn: &sea_orm::DatabaseConnection,
    deferred_capture_refs: Vec<reference::Model>,
    capture_outcome: AgentCaptureRestoreOutcome,
) -> CloudResult<()> {
    if capture_outcome == AgentCaptureRestoreOutcome::NoGeneration
        && !deferred_capture_refs.is_empty()
    {
        restore_metadata_models_with_capture_policy(db_conn, deferred_capture_refs, false, false)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[serial_test::serial(cwd, env)]
    fn merge_import_tombstones_keeps_newest_and_fingerprints() {
        use crate::utils::d1_client::AgentImportTombstoneRow;

        let row = |erased_at: i64, fp: Option<&str>| AgentImportTombstoneRow {
            agent_kind: "claude_code".to_string(),
            provider_session_id: "prov-1".to_string(),
            erased_session_id: format!("sess-{erased_at}"),
            source_fingerprint: fp.map(str::to_string),
            erased_at,
        };
        // Newest erased_at wins, but retired source fingerprints never
        // survive a local/remote merge.
        let merged = super::merge_import_tombstones(&[row(5, None)], &[row(3, Some("aa"))]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].erased_at, 5);
        assert_eq!(merged[0].erased_session_id, "sess-5");
        assert_eq!(merged[0].source_fingerprint, None);
        // Distinct provider identities stay distinct.
        let mut other = row(7, None);
        other.provider_session_id = "prov-2".to_string();
        let merged = super::merge_import_tombstones(&[row(5, None)], &[other]);
        assert_eq!(merged.len(), 2);
        // Idempotent under replay: merging the merged set changes nothing.
        let replay = super::merge_import_tombstones(&merged, &merged);
        assert_eq!(replay, merged);
    }

    #[tokio::test]
    async fn persist_local_import_tombstones_scrubs_legacy_source_fingerprints() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open local tombstone fixture");
        for sql in [
            "CREATE TABLE agent_import_tombstone (
                tombstone_id TEXT PRIMARY KEY,
                agent_kind TEXT NOT NULL,
                provider_session_id TEXT NOT NULL,
                erased_session_id TEXT NOT NULL,
                source_fingerprint TEXT,
                erased_at INTEGER NOT NULL,
                UNIQUE(agent_kind, provider_session_id)
             )",
            "INSERT INTO agent_import_tombstone VALUES (
                'legacy-local', 'claude_code', 'provider', 'old-session',
                'legacy-local-fingerprint', 3
             )",
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                sql.to_string(),
            ))
            .await
            .expect("prepare local tombstone fixture");
        }

        persist_local_import_tombstones(
            &conn,
            &[AgentImportTombstoneRow {
                agent_kind: "claude_code".to_string(),
                provider_session_id: "provider".to_string(),
                erased_session_id: "restored-session".to_string(),
                source_fingerprint: Some("remote-legacy-fingerprint".to_string()),
                erased_at: 7,
            }],
        )
        .await
        .expect("restore tombstone while clearing retired metadata");

        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT erased_session_id, source_fingerprint, erased_at
                 FROM agent_import_tombstone
                 WHERE agent_kind = 'claude_code' AND provider_session_id = 'provider'"
                    .to_string(),
            ))
            .await
            .expect("read restored tombstone")
            .expect("restored tombstone row");
        assert_eq!(
            row.try_get_by::<String, _>("erased_session_id")
                .expect("restored session id"),
            "restored-session"
        );
        assert_eq!(
            row.try_get_by::<Option<String>, _>("source_fingerprint")
                .expect("retired fingerprint column"),
            None,
            "restore must clear both a remote legacy value and any local value it replaces"
        );
        assert_eq!(
            row.try_get_by::<i64, _>("erased_at")
                .expect("newest erasure timestamp"),
            7
        );
    }

    fn legacy_import_ownership_metadata(schema_version: Option<i64>) -> String {
        let mut metadata = serde_json::json!({
            "repository_identity": "legacy-repository-proof",
            "source_kind": "file",
            "source_id": "/private/provider/session.jsonl",
            "source_fingerprint": "legacy-unkeyed-fingerprint",
            "import_provisional": false,
            "imported": true,
        });
        if let Some(schema_version) = schema_version {
            metadata["import_source_schema_version"] = serde_json::Value::from(schema_version);
        }
        metadata.to_string()
    }

    fn malformed_unversioned_legacy_ownership_metadata() -> String {
        let mut metadata: serde_json::Value =
            serde_json::from_str(&legacy_import_ownership_metadata(None))
                .expect("legacy ownership fixture is valid JSON");
        metadata["source_fingerprint"] = serde_json::Value::Null;
        metadata.to_string()
    }

    const LEGACY_OWNERSHIP_SENTINEL: &str = "LEGACY_OWNERSHIP_SENTINEL_MUST_NOT_CROSS_CLOUD";

    fn partial_legacy_ownership_metadata(field: &str) -> String {
        let value = if field == "import_source_schema_version" {
            // A V2 version without the rest of the closed V2 record must be
            // rejected by the catalog validator too.
            serde_json::Value::from(
                crate::internal::ai::agent_import::IMPORT_IDENTITY_SCHEMA_VERSION_V2,
            )
        } else {
            serde_json::Value::String(LEGACY_OWNERSHIP_SENTINEL.to_string())
        };
        let mut metadata = serde_json::Map::new();
        metadata.insert(field.to_string(), value);
        serde_json::Value::Object(metadata).to_string()
    }

    fn partial_legacy_ownership_metadata_cases() -> Vec<(String, String)> {
        [
            "repository_identity",
            "source_kind",
            "source_id",
            "source_fingerprint",
            "import_source_schema_version",
            "import_provisional",
            "imported",
            "transcript_snapshot",
        ]
        .into_iter()
        .map(|field| {
            (
                format!("partial reserved field {field}"),
                partial_legacy_ownership_metadata(field),
            )
        })
        .collect()
    }

    fn assert_cloud_ownership_rejection(label: &str, error: &CloudError) {
        let message = error.to_string();
        assert!(
            message.contains("import ownership metadata")
                || message.contains("unsupported import ownership schema"),
            "unexpected {label} ownership error: {message}"
        );
        assert!(
            !message.contains("/private/provider/session.jsonl"),
            "{label} diagnostic must not disclose the legacy locator: {message}"
        );
        assert!(
            !message.contains(LEGACY_OWNERSHIP_SENTINEL),
            "{label} diagnostic must not disclose partial ownership metadata: {message}"
        );
    }

    async fn cloud_snapshot_fixture_with_metadata(
        metadata_json: &str,
    ) -> sea_orm::DatabaseConnection {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open cloud snapshot fixture");
        for sql in [
            "CREATE TABLE object_index (repo_id TEXT NOT NULL, is_synced INTEGER NOT NULL)",
            "CREATE TABLE agent_session (
                session_id TEXT NOT NULL,
                agent_kind TEXT NOT NULL,
                provider_session_id TEXT NOT NULL,
                state TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                worktree_id TEXT,
                parent_commit TEXT,
                parent_session_id TEXT,
                metadata_json TEXT NOT NULL,
                redaction_report TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                last_event_at INTEGER NOT NULL,
                stopped_at INTEGER,
                schema_version INTEGER NOT NULL,
                sync_revision INTEGER NOT NULL
             )",
            "CREATE TABLE agent_checkpoint (
                checkpoint_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                parent_checkpoint_id TEXT,
                scope TEXT NOT NULL,
                parent_commit TEXT,
                tree_oid TEXT NOT NULL,
                metadata_blob_oid TEXT NOT NULL,
                traces_commit TEXT NOT NULL,
                tool_use_id TEXT,
                subagent_session_id TEXT,
                description TEXT,
                created_at INTEGER NOT NULL,
                sync_revision INTEGER NOT NULL
             )",
            "CREATE TABLE reference (name TEXT, kind TEXT, remote TEXT, `commit` TEXT)",
            "CREATE TABLE agent_capture_cloud_base (
                repo_id TEXT PRIMARY KEY,
                remote_generation INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             )",
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                sql.to_string(),
            ))
            .await
            .expect("create cloud snapshot fixture table");
        }
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at,
                schema_version, sync_revision
             ) VALUES (?, ?, ?, 'active', '/repo', ?, '{}', 1, 1, 1, 1)",
            [
                "session".into(),
                "claude_code".into(),
                "provider".into(),
                metadata_json.to_string().into(),
            ],
        ))
        .await
        .expect("seed cloud snapshot session");
        conn
    }

    async fn cloud_snapshot_fixture_with_companion_source_keys(
        claim_schema_version: i64,
        claim_source_key: &str,
        revision: Option<(i64, &str)>,
    ) -> sea_orm::DatabaseConnection {
        cloud_snapshot_fixture_with_session_and_companions(
            "{}",
            claim_schema_version,
            claim_source_key,
            revision,
        )
        .await
    }

    async fn cloud_snapshot_fixture_with_session_and_companions(
        metadata_json: &str,
        claim_schema_version: i64,
        claim_source_key: &str,
        revision: Option<(i64, &str)>,
    ) -> sea_orm::DatabaseConnection {
        use sea_orm::{ConnectionTrait, Statement};

        let conn = cloud_snapshot_fixture_with_metadata(metadata_json).await;
        for sql in [
            "CREATE TABLE agent_checkpoint_prune_tombstone (
                checkpoint_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                pruned_at INTEGER NOT NULL
             )",
            "CREATE TABLE agent_subagent_content_claim (
                parent_session_id TEXT NOT NULL,
                provider_kind TEXT NOT NULL,
                source_key TEXT NOT NULL,
                content_schema_version INTEGER NOT NULL,
                revision_cursor INTEGER NOT NULL,
                sync_revision INTEGER NOT NULL,
                current_revision INTEGER NOT NULL,
                current_checkpoint_id TEXT,
                current_digest TEXT,
                fence_token INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             )",
            "CREATE TABLE agent_subagent_content_revision (
                parent_session_id TEXT NOT NULL,
                provider_kind TEXT NOT NULL,
                source_key TEXT NOT NULL,
                content_schema_version INTEGER NOT NULL,
                revision INTEGER NOT NULL,
                checkpoint_id TEXT NOT NULL,
                content_digest TEXT NOT NULL,
                source_channel TEXT NOT NULL,
                partial INTEGER NOT NULL,
                created_at INTEGER NOT NULL
             )",
            "CREATE TABLE agent_subagent_link (
                content_checkpoint_id TEXT NOT NULL,
                parent_session_id TEXT NOT NULL,
                link_state TEXT NOT NULL,
                boundary_checkpoint_id TEXT,
                stable_subagent_id TEXT,
                sync_revision INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             )",
        ] {
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                sql.to_string(),
            ))
            .await
            .expect("create companion cloud snapshot fixture table");
        }
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_subagent_content_claim (
                parent_session_id, provider_kind, source_key, content_schema_version,
                revision_cursor, sync_revision, current_revision, current_checkpoint_id,
                current_digest, fence_token, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 0, 0, 0, NULL, NULL, 0, 1, 1)",
            [
                "session".into(),
                "claude_code".into(),
                claim_source_key.to_string().into(),
                claim_schema_version.into(),
            ],
        ))
        .await
        .expect("seed companion claim fixture");
        if let Some((revision_schema_version, revision_source_key)) = revision {
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO agent_subagent_content_revision (
                    parent_session_id, provider_kind, source_key, content_schema_version,
                    revision, checkpoint_id, content_digest, source_channel, partial, created_at
                 ) VALUES (?, ?, ?, ?, 1, 'unused-checkpoint', 'digest', 'import', 0, 1)",
                [
                    "session".into(),
                    "claude_code".into(),
                    revision_source_key.to_string().into(),
                    revision_schema_version.into(),
                ],
            ))
            .await
            .expect("seed companion revision fixture");
        }
        conn
    }

    fn invalid_subagent_companion_source_key_cases() -> Vec<(&'static str, i64, String, i64, String)>
    {
        let valid_v1 = format!("source/sha256/{}", "a".repeat(64));
        let valid_v2 = format!("source/subagent-hmac-v2/{}", "b".repeat(64));
        let raw_locator = "/private/provider/subagents/session.jsonl".to_string();
        let malformed_v2 = "source/subagent-hmac-v2/not-a-hex-commitment".to_string();
        let v2 = crate::internal::ai::subagent_content::SUBAGENT_CONTENT_SCHEMA_VERSION;

        vec![
            (
                "claim raw locator",
                1,
                raw_locator.clone(),
                1,
                valid_v1.clone(),
            ),
            (
                "claim malformed V2 commitment",
                v2,
                malformed_v2.clone(),
                v2,
                valid_v2.clone(),
            ),
            ("revision raw locator", 1, valid_v1, 1, raw_locator),
            (
                "revision malformed V2 commitment",
                v2,
                valid_v2,
                v2,
                malformed_v2,
            ),
        ]
    }

    const RESERVED_OWNERSHIP_FIELDS: [&str; 8] = [
        "repository_identity",
        "source_kind",
        "source_id",
        "source_fingerprint",
        "import_source_schema_version",
        "import_provisional",
        "imported",
        "transcript_snapshot",
    ];

    const LEGACY_OWNERSHIP_VALUES: [&str; 4] = [
        "/private/provider/session.jsonl",
        "legacy-unkeyed-fingerprint",
        "legacy-repository-proof",
        LEGACY_OWNERSHIP_SENTINEL,
    ];

    fn valid_v2_import_session_row(
        session_id: &str,
        provider_session_id: &str,
    ) -> AgentSessionV2Row {
        let source_id = format!("source/hmac-v2/{}", "a".repeat(64));
        let mut row = fixture_session_row(session_id, provider_session_id);
        row.metadata_json = format!(
            r#"{{"repository_identity":"not_retained:v1","source_kind":"file","source_id":"{source_id}","source_fingerprint":"{source_id}","import_source_schema_version":2,"import_provisional":false,"transcript_snapshot":null,"imported":true}}"#
        );
        row.redaction_report = r#"{"import":{"pipeline":"typed_allowlist","snapshot_redaction":true,"raw_persisted":false,"matches":[],"bytes_scanned":0,"bytes_redacted":0}}"#.to_string();
        row
    }

    /// Legacy ownership crosses the cloud boundary only as its projection:
    /// every ownership member is omitted and every other column is unchanged.
    fn assert_legacy_ownership_projected(
        label: &str,
        original: &AgentSessionV2Row,
        projected: &AgentSessionV2Row,
    ) {
        let metadata: serde_json::Value = serde_json::from_str(&projected.metadata_json)
            .unwrap_or_else(|error| panic!("{label}: projected metadata is JSON: {error}"));
        let object = metadata
            .as_object()
            .unwrap_or_else(|| panic!("{label}: projected metadata stays a JSON object"));
        for field in RESERVED_OWNERSHIP_FIELDS {
            assert!(
                !object.contains_key(field),
                "{label}: ownership member {field} must not cross cloud"
            );
        }
        for value in LEGACY_OWNERSHIP_VALUES {
            assert!(
                !projected.metadata_json.contains(value),
                "{label}: projected metadata disclosed legacy ownership"
            );
        }
        let expected = AgentSessionV2Row {
            metadata_json: projected.metadata_json.clone(),
            ..original.clone()
        };
        assert_eq!(
            projected, &expected,
            "{label}: only ownership metadata may change"
        );
    }

    #[derive(Default)]
    struct RecordingCaptureProgress {
        warnings: std::sync::Mutex<Vec<String>>,
        completions: std::sync::Mutex<Vec<(usize, usize, usize)>>,
    }

    impl RecordingCaptureProgress {
        fn warnings(&self) -> Vec<String> {
            self.warnings.lock().unwrap().clone()
        }

        fn completions(&self) -> Vec<(usize, usize, usize)> {
            self.completions.lock().unwrap().clone()
        }
    }

    impl CloudSyncProgress for RecordingCaptureProgress {
        // The console and JSON renderers turn all three callbacks into a
        // `cloud_sync.agent_capture.warning`; record them as one event stream.
        fn on_agent_capture_session_warning(&self, _session_id: &str, err: &str) {
            self.warnings.lock().unwrap().push(err.to_string());
        }
        fn on_agent_capture_checkpoint_warning(&self, _checkpoint_id: &str, err: &str) {
            self.warnings.lock().unwrap().push(err.to_string());
        }
        fn on_agent_capture_warning(&self, err: &str) {
            self.warnings.lock().unwrap().push(err.to_string());
        }
        fn on_agent_capture_done_with_subagents(
            &self,
            sessions_synced: usize,
            _sessions_failed: usize,
            checkpoints_synced: usize,
            _checkpoints_failed: usize,
            subagent_rows_synced: usize,
            _subagent_rows_failed: usize,
        ) {
            self.completions.lock().unwrap().push((
                sessions_synced,
                checkpoints_synced,
                subagent_rows_synced,
            ));
        }
    }

    async fn mock_d1_with_object_index() -> crate::utils::d1_client::test_support::MockD1 {
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        // `run_cloud_sync` converges the object index before the capture
        // phase; the capture tests start from that same remote state.
        mock.client()
            .ensure_object_index_table()
            .await
            .expect("prepare mock D1 object index");
        mock
    }

    async fn local_session_metadata(
        conn: &sea_orm::DatabaseConnection,
        session_id: &str,
    ) -> String {
        use sea_orm::Statement;

        conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT metadata_json FROM agent_session WHERE session_id = ?",
            [session_id.into()],
        ))
        .await
        .expect("read local session metadata")
        .expect("local session row")
        .try_get_by::<String, _>("metadata_json")
        .expect("decode local session metadata")
    }

    #[test]
    fn cloud_session_projection_omits_legacy_ownership_and_keeps_live_and_v2_rows() {
        let (v2_version_only, partial_legacy): (Vec<_>, Vec<_>) =
            partial_legacy_ownership_metadata_cases()
                .into_iter()
                .partition(|(label, _)| label.ends_with("import_source_schema_version"));
        let mut legacy_cases = vec![
            (
                "explicit V1".to_string(),
                legacy_import_ownership_metadata(Some(1)),
            ),
            (
                "unversioned".to_string(),
                legacy_import_ownership_metadata(None),
            ),
            (
                "unversioned null fingerprint".to_string(),
                malformed_unversioned_legacy_ownership_metadata(),
            ),
        ];
        legacy_cases.extend(partial_legacy);
        for (label, metadata_json) in legacy_cases {
            let mut row = fixture_session_row("legacy-session", "legacy-provider");
            row.metadata_json = metadata_json;
            let projected = project_agent_session_for_cloud(&row, "test").unwrap_or_else(|error| {
                panic!("{label} legacy ownership must still sync: {error}")
            });
            assert_legacy_ownership_projected(&label, &row, &projected);
            assert_eq!(
                project_agent_session_for_cloud(&projected, "test").expect("re-project"),
                projected,
                "{label}: a projected row is a fixed point, so a mirrored copy compares equal"
            );
        }

        let mut mixed = fixture_session_row("mixed-session", "mixed-provider");
        let mut metadata: serde_json::Value =
            serde_json::from_str(&legacy_import_ownership_metadata(None))
                .expect("legacy ownership fixture is valid JSON");
        metadata["event"] = serde_json::Value::from("SessionStart");
        mixed.metadata_json = metadata.to_string();
        let projected =
            project_agent_session_for_cloud(&mixed, "test").expect("mixed legacy metadata syncs");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&projected.metadata_json)
                .expect("projected metadata is JSON"),
            serde_json::json!({"event": "SessionStart"}),
            "non-ownership metadata next to legacy ownership must survive the projection"
        );

        for (label, metadata_json) in [
            ("ordinary event", r#"{"event":"SessionStart","live":true}"#),
            (
                "ordinary live diagnostic",
                r#"{"event":"SessionStart","attempt":1}"#,
            ),
        ] {
            let mut live = fixture_session_row("live-session", "live-provider");
            live.metadata_json = metadata_json.to_string();
            assert_eq!(
                project_agent_session_for_cloud(&live, "test")
                    .unwrap_or_else(|error| panic!("{label} metadata must stay syncable: {error}")),
                live,
                "{label} metadata crosses cloud byte-identically"
            );
        }

        let v2 = valid_v2_import_session_row("v2-session", "v2-provider");
        assert_eq!(
            project_agent_session_for_cloud(&v2, "test").expect("valid V2 ownership syncs"),
            v2,
            "a complete V2 ownership record crosses cloud byte-identically"
        );

        let mut rejected = v2_version_only;
        rejected.extend([
            (
                "non-integer schema".to_string(),
                format!(
                    r#"{{"import_source_schema_version":"1","source_id":"{LEGACY_OWNERSHIP_SENTINEL}"}}"#
                ),
            ),
            (
                "unknown schema".to_string(),
                format!(
                    r#"{{"import_source_schema_version":3,"source_id":"{LEGACY_OWNERSHIP_SENTINEL}"}}"#
                ),
            ),
            (
                "duplicate keys".to_string(),
                format!(
                    r#"{{"source_id":"{LEGACY_OWNERSHIP_SENTINEL}","source_id":"/private/provider/session.jsonl"}}"#
                ),
            ),
            ("non-object".to_string(), "[]".to_string()),
            (
                "not JSON".to_string(),
                format!("{LEGACY_OWNERSHIP_SENTINEL} /private/provider/session.jsonl"),
            ),
        ]);
        for (label, metadata_json) in rejected {
            let mut row = fixture_session_row("rejected-session", "rejected-provider");
            row.metadata_json = metadata_json;
            let error = project_agent_session_for_cloud(&row, "test")
                .expect_err("malformed or invalid ownership must stay a fixed error");
            assert_cloud_ownership_rejection(&label, &error);
        }
    }

    #[tokio::test]
    async fn cloud_snapshot_projects_legacy_ownership_without_mutating_local_evidence() {
        for (label, metadata_json) in [
            ("explicit V1", legacy_import_ownership_metadata(Some(1))),
            ("unversioned", legacy_import_ownership_metadata(None)),
            (
                "unversioned null fingerprint",
                malformed_unversioned_legacy_ownership_metadata(),
            ),
        ] {
            let conn = cloud_snapshot_fixture_with_metadata(&metadata_json).await;
            let snapshot = load_agent_capture_snapshot(&conn, "repo", false)
                .await
                .unwrap_or_else(|error| panic!("{label} legacy ownership must sync: {error}"));
            assert_eq!(snapshot.sessions.len(), 1);
            let original = AgentSessionV2Row {
                metadata_json: metadata_json.clone(),
                ..snapshot.sessions[0].clone()
            };
            assert_legacy_ownership_projected(label, &original, &snapshot.sessions[0]);
            assert_eq!(
                local_session_metadata(&conn, "session").await,
                metadata_json,
                "{label}: the projection must not rewrite immutable local evidence"
            );
        }

        let conn = cloud_snapshot_fixture_with_metadata(&partial_legacy_ownership_metadata(
            "import_source_schema_version",
        ))
        .await;
        let error = load_agent_capture_snapshot(&conn, "repo", false)
            .await
            .expect_err("an incomplete V2 record must fail before a D1 client is involved");
        assert_cloud_ownership_rejection("V2 version only", &error);
    }

    #[tokio::test]
    async fn cloud_sync_projects_legacy_ownership_and_withholds_unmirrored_v1_companions() {
        let mock = mock_d1_with_object_index().await;
        let d1_client = mock.client();
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let legacy_metadata = legacy_import_ownership_metadata(Some(1));
        let legacy_source_key = format!("source/sha256/{}", "c".repeat(64));
        // A zero-revision claim is a complete local companion on its own, so
        // the never-mirrored V1 source identity is the behavior under test.
        let conn = cloud_snapshot_fixture_with_session_and_companions(
            &legacy_metadata,
            1,
            &legacy_source_key,
            None,
        )
        .await;

        for (attempt, expected_sessions) in [("first", 1), ("repeat", 0)] {
            let progress = RecordingCaptureProgress::default();
            let outcome = sync_agent_capture_tables(&conn, &d1_client, &remote, "repo", &progress)
                .await
                .unwrap_or_else(|error| {
                    panic!("{attempt} sync of an upgraded repository must succeed: {error}")
                });
            assert_eq!(
                outcome,
                AgentCaptureSyncOutcome::Completed {
                    sessions_synced: expected_sessions,
                    sessions_failed: 0,
                    checkpoints_synced: 0,
                    checkpoints_failed: 0,
                },
                "{attempt} sync"
            );
            assert_eq!(
                progress.warnings(),
                vec![AGENT_CAPTURE_LEGACY_LOCAL_ONLY_MESSAGE.to_string()],
                "{attempt} sync reports withheld legacy evidence with exactly one fixed warning"
            );
            assert_eq!(progress.completions(), vec![(expected_sessions, 0, 0)]);
        }

        let wire = serde_json::to_string(&mock.request_bodies().await)
            .expect("serialize recorded D1 requests");
        for value in LEGACY_OWNERSHIP_VALUES {
            assert!(
                !wire.contains(value),
                "legacy session ownership must not reach any D1 request"
            );
        }
        assert!(
            !wire.contains(&legacy_source_key),
            "a never-mirrored V1 source digest must not become a new cloud value"
        );
        let remote_rows = d1_client
            .list_agent_capture_restore_catalog_rows("repo", true, 100)
            .await
            .expect("read mirrored catalog");
        assert!(remote_rows.claims.is_empty());
        assert_eq!(remote_rows.sessions.len(), 1);
        let original = AgentSessionV2Row {
            metadata_json: legacy_metadata.clone(),
            ..remote_rows.sessions[0].clone()
        };
        assert_legacy_ownership_projected("mirrored session", &original, &remote_rows.sessions[0]);
        assert_eq!(
            local_session_metadata(&conn, "session").await,
            legacy_metadata,
            "sync must leave local legacy evidence untouched"
        );
        assert_eq!(
            scalar_count(
                &conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim"
            )
            .await
            .expect("count local claims"),
            1,
            "withheld legacy evidence stays local"
        );
    }

    #[tokio::test]
    async fn cloud_sync_accepts_head_mirrored_legacy_rows_and_restores_their_projection() {
        let mock = mock_d1_with_object_index().await;
        let d1_client = mock.client();
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let legacy_metadata = legacy_import_ownership_metadata(None);
        let legacy_source_key = format!("source/sha256/{}", "d".repeat(64));
        let conn = cloud_snapshot_fixture_with_session_and_companions(
            &legacy_metadata,
            1,
            &legacy_source_key,
            None,
        )
        .await;

        // Mirror the local rows verbatim, as releases up to HEAD did.
        d1_client.ensure_agent_session_table().await.unwrap();
        d1_client.ensure_agent_checkpoint_table().await.unwrap();
        d1_client
            .ensure_agent_capture_generation_table()
            .await
            .unwrap();
        d1_client
            .ensure_agent_checkpoint_prune_tombstone_table()
            .await
            .unwrap();
        d1_client
            .ensure_agent_import_tombstone_table()
            .await
            .unwrap();
        d1_client
            .ensure_agent_subagent_content_tables()
            .await
            .unwrap();
        let snapshot_session = AgentSessionV2Row {
            session_id: "session".to_string(),
            agent_kind: "claude_code".to_string(),
            provider_session_id: "provider".to_string(),
            state: "active".to_string(),
            working_dir: "/repo".to_string(),
            worktree_id: None,
            parent_commit: None,
            parent_session_id: None,
            metadata_json: legacy_metadata.clone(),
            redaction_report: "{}".to_string(),
            started_at: 1,
            last_event_at: 1,
            stopped_at: None,
            schema_version: 1,
            sync_revision: 1,
        };
        let legacy_claim = AgentSubagentContentClaimRow {
            parent_session_id: "session".to_string(),
            provider_kind: "claude_code".to_string(),
            source_key: legacy_source_key.clone(),
            content_schema_version: 1,
            revision_cursor: 0,
            sync_revision: 0,
            current_revision: 0,
            current_checkpoint_id: None,
            current_digest: None,
            fence_token: 0,
            created_at: 1,
            updated_at: 1,
        };
        let (object_index_digest, object_index_count) =
            agent_capture_object_index_digest(&[]).expect("empty object manifest");
        let writer = "head-release-writer";
        d1_client
            .begin_agent_capture_generation_from(
                "repo",
                writer,
                None,
                AgentCaptureGenerationManifest {
                    object_index_digest: &object_index_digest,
                    object_index_count,
                    object_index_scope: "checkpoint_projection",
                    object_index_generation: 0,
                    traces_head: None,
                },
            )
            .await
            .expect("begin HEAD-era generation");
        d1_client
            .sync_agent_sessions_batch("repo", writer, std::slice::from_ref(&snapshot_session))
            .await
            .expect("mirror HEAD-era legacy session");
        d1_client
            .sync_agent_subagent_claims_batch("repo", writer, std::slice::from_ref(&legacy_claim))
            .await
            .expect("mirror HEAD-era V1 claim");
        let generation = d1_client
            .complete_agent_capture_generation("repo", writer, 0)
            .await
            .expect("complete HEAD-era generation");
        store_local_agent_capture_cloud_base(&conn, "repo", generation.generation)
            .await
            .expect("record HEAD-era cloud base");

        let progress = RecordingCaptureProgress::default();
        let outcome = sync_agent_capture_tables(&conn, &d1_client, &remote, "repo", &progress)
            .await
            .expect("rows mirrored by an earlier release must not block sync");
        assert_eq!(
            outcome,
            AgentCaptureSyncOutcome::Completed {
                sessions_synced: 0,
                sessions_failed: 0,
                checkpoints_synced: 0,
                checkpoints_failed: 0,
            }
        );
        assert!(
            progress.warnings().is_empty(),
            "already mirrored legacy evidence is not withheld"
        );
        assert_eq!(progress.completions(), vec![(0, 0, 0)]);

        let restore_root = tempdir().unwrap();
        let restore_db = restore_root.path().join("restore.db");
        let restore_conn = crate::internal::db::create_database(restore_db.to_str().unwrap())
            .await
            .expect("create restore target catalog");
        assert_eq!(
            restore_agent_capture_from_d1(&restore_conn, &d1_client, "repo", false)
                .await
                .expect("a HEAD-mirrored legacy remote stays restorable"),
            AgentCaptureRestoreOutcome::GenerationInstalled
        );
        let restored_metadata = local_session_metadata(&restore_conn, "session").await;
        let restored = AgentSessionV2Row {
            metadata_json: restored_metadata,
            ..snapshot_session.clone()
        };
        assert_legacy_ownership_projected("restored session", &snapshot_session, &restored);
        assert_eq!(
            scalar_count(
                &restore_conn,
                "SELECT COUNT(*) AS n FROM agent_subagent_content_claim"
            )
            .await
            .expect("count restored claims"),
            1,
            "mirrored legacy subagent evidence remains restorable"
        );
    }

    /// plan-20261116 FIX-CQ-02: restoring into a split repository writes the
    /// catalog rows into `capture.db` and the traces ref into `libra.db`;
    /// `libra.db` gains no `agent_*` table.
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn cloud_restore_writes_a_split_catalog_into_the_capture_database() {
        use sea_orm::Statement;

        let _env = crate::utils::test::ConfigDbFixture::new().unwrap();
        let repo = tempdir().unwrap();
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;
        let _cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        let storage = repo.path().join(".libra");
        let capture = crate::internal::ai::capture::store::open(
            &storage,
            crate::internal::ai::capture::store::COMMAND_BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        let repository = crate::internal::db::get_db_conn_instance().await;
        let remote = fixture_session_row("split-restore-session", "split-restore-provider");
        restore_agent_capture_from_rows(&repository, std::slice::from_ref(&remote), &[], false)
            .await
            .expect("restore into a split repository");
        let restored = capture
            .query_one_raw(Statement::from_string(
                capture.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_session WHERE session_id = 'split-restore-session'"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by::<i64, _>("n")
            .unwrap();
        assert_eq!(restored, 1, "the restored session must be in capture.db");
        assert!(
            !crate::internal::db::connection_has_table(&repository, "agent_session")
                .await
                .unwrap(),
            "restore must not recreate catalog tables in libra.db"
        );
        let traces_refs = repository
            .query_one_raw(Statement::from_sql_and_values(
                repository.get_database_backend(),
                "SELECT COUNT(*) AS n FROM reference WHERE name = ? AND kind = 'Branch' AND remote IS NULL",
                [crate::internal::branch::TRACES_BRANCH.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by::<i64, _>("n")
            .unwrap();
        assert_eq!(traces_refs, 1, "the fenced traces ref stays in libra.db");
    }

    #[tokio::test]
    async fn cloud_restore_projects_legacy_ownership_and_keeps_equal_generation_local_evidence() {
        use sea_orm::Statement;

        let root = tempdir().unwrap();
        let mut remote = fixture_session_row("remote-session", "remote-provider");
        remote.metadata_json = legacy_import_ownership_metadata(Some(1));

        let fresh_path = root.path().join("fresh.db");
        let fresh = crate::internal::db::create_database(fresh_path.to_str().unwrap())
            .await
            .expect("create fresh restore catalog");
        restore_agent_capture_from_rows(&fresh, std::slice::from_ref(&remote), &[], false)
            .await
            .expect("legacy remote ownership must restore as its projection");
        let restored = AgentSessionV2Row {
            metadata_json: local_session_metadata(&fresh, "remote-session").await,
            ..remote.clone()
        };
        assert_legacy_ownership_projected("restored session", &remote, &restored);

        // The clone that captured the row still holds the raw legacy row at
        // the same generation; the projected remote copy is not a conflict.
        let owner_path = root.path().join("owner.db");
        let owner = crate::internal::db::create_database(owner_path.to_str().unwrap())
            .await
            .expect("create owner catalog");
        owner
            .execute_raw(Statement::from_sql_and_values(
                owner.get_database_backend(),
                "INSERT INTO agent_session (
                    session_id, agent_kind, provider_session_id, state, working_dir,
                    metadata_json, redaction_report, started_at, last_event_at,
                    schema_version, sync_revision
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                [
                    remote.session_id.clone().into(),
                    remote.agent_kind.clone().into(),
                    remote.provider_session_id.clone().into(),
                    remote.state.clone().into(),
                    remote.working_dir.clone().into(),
                    remote.metadata_json.clone().into(),
                    remote.redaction_report.clone().into(),
                    remote.started_at.into(),
                    remote.last_event_at.into(),
                    remote.schema_version.into(),
                    remote.sync_revision.into(),
                ],
            ))
            .await
            .expect("seed local legacy evidence");
        restore_agent_capture_from_rows(&owner, std::slice::from_ref(&remote), &[], false)
            .await
            .expect("an equal-generation legacy row is the same session, not a conflict");
        assert_eq!(
            local_session_metadata(&owner, "remote-session").await,
            remote.metadata_json,
            "restore must leave equal-generation local legacy evidence untouched"
        );

        let mut divergent = remote.clone();
        divergent.state = "stopped".to_string();
        let error = restore_agent_capture_from_rows(&owner, &[divergent], &[], false)
            .await
            .expect_err("a real equal-generation divergence still fails closed");
        assert!(
            error
                .to_string()
                .contains("conflicts with local state at the same sync generation"),
            "unexpected divergence error: {error}"
        );
    }

    #[tokio::test]
    async fn agent_capture_batch_failure_emits_one_fixed_warning() {
        const REMOTE_SENTINEL: &str = "REMOTE_D1_BATCH_SENTINEL=/private/provider/session.jsonl";
        let mock = mock_d1_with_object_index().await;
        let d1_client = mock.client();
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let conn = cloud_snapshot_fixture_with_metadata(r#"{"event":"SessionStart"}"#).await;
        // Only the session row batch extracts `metadata_json` from its payload.
        mock.fail_once_when_sql_contains("json_extract(value, '$.metadata_json')", REMOTE_SENTINEL);

        let progress = RecordingCaptureProgress::default();
        let outcome =
            sync::mirror_agent_capture(&conn, &d1_client, &remote, "repo", &progress).await;

        assert_eq!(
            progress.warnings(),
            vec![AGENT_CAPTURE_SYNC_FAILURE_MESSAGE.to_string()],
            "a failed row batch must emit exactly one fixed agent-capture warning"
        );
        assert_eq!(
            outcome,
            AgentCaptureSyncOutcome::Failed {
                error: AGENT_CAPTURE_SYNC_FAILURE_MESSAGE.to_string(),
            }
        );
        assert!(
            serde_json::to_string(&mock.request_bodies().await)
                .expect("serialize recorded D1 requests")
                .contains("$.metadata_json"),
            "the injected failure must come from the session row batch"
        );
        let report = CloudSyncReport {
            repo_id: "repo".to_string(),
            project_name: "project".to_string(),
            total_unsynced: 0,
            synced_count: 0,
            failed_count: 0,
            metadata: MetadataSyncOutcome::Skipped,
            agent_capture: outcome,
            spool_warnings: Vec::new(),
        };
        let wire = serde_json::to_value(cloud_sync_output_from_report(&report))
            .expect("serialize failed cloud sync");
        assert_eq!(
            wire["agent_capture"]["error"],
            AGENT_CAPTURE_SYNC_FAILURE_MESSAGE
        );
        assert!(!wire.to_string().contains(REMOTE_SENTINEL));
    }

    #[tokio::test]
    async fn agent_capture_validation_failure_reports_one_specific_reason() {
        let mock = mock_d1_with_object_index().await;
        let requests_before = mock.request_bodies().await.len();
        let d1_client = mock.client();
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let valid_v1 = format!("source/sha256/{}", "a".repeat(64));
        let raw_locator = "/private/provider/subagents/session.jsonl";
        let conn =
            cloud_snapshot_fixture_with_companion_source_keys(1, &valid_v1, Some((1, raw_locator)))
                .await;

        let progress = RecordingCaptureProgress::default();
        let outcome =
            sync::mirror_agent_capture(&conn, &d1_client, &remote, "repo", &progress).await;
        let AgentCaptureSyncOutcome::Failed { error } = outcome else {
            panic!("an invalid companion source key must fail the capture phase: {outcome:?}");
        };
        assert_eq!(
            error, "local subagent companion has an invalid source commitment",
            "the specific fixed reason must survive, not a retry-only placeholder"
        );
        assert_eq!(progress.warnings(), vec![error.clone()]);
        assert_eq!(
            mock.request_bodies().await.len(),
            requests_before,
            "local validation must fail before any agent-capture D1 request"
        );

        let cli = cloud_cli_error_typed("sync", agent_capture_mirror_failure(&error));
        assert_eq!(
            cli.message(),
            "agent capture mirror failed: local subagent companion has an invalid source commitment"
        );
        assert_eq!(cli.stable_code(), StableErrorCode::ConflictOperationBlocked);
        assert!(!format!("{:?}", cli.details()).contains(raw_locator));
    }

    #[tokio::test]
    async fn cloud_sync_rejects_invalid_companion_source_keys_before_any_d1_request() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use tokio::{net::TcpListener, sync::oneshot};

        // The snapshot must validate both companion row types before sync
        // creates a remote table or emits a payload containing a raw locator.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local D1 request trap");
        let base_url = format!("http://{}/client/v4", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let trap_requests = Arc::clone(&requests);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let trap = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        if accepted.is_err() {
                            break;
                        }
                        trap_requests.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        });
        let d1_client = D1Client::new_with_api_base_url(
            "test-account".to_string(),
            "test-token".to_string(),
            "test-database".to_string(),
            &base_url,
        )
        .expect("build local D1 trap client");
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));

        for (
            label,
            claim_schema_version,
            claim_source_key,
            revision_schema_version,
            revision_source_key,
        ) in invalid_subagent_companion_source_key_cases()
        {
            let conn = cloud_snapshot_fixture_with_companion_source_keys(
                claim_schema_version,
                &claim_source_key,
                Some((revision_schema_version, &revision_source_key)),
            )
            .await;
            let error = sync_agent_capture_tables(
                &conn,
                &d1_client,
                &remote,
                "repo",
                &SilentCloudSyncProgress,
            )
            .await
            .expect_err("invalid companion source key must fail before D1 publication");
            let message = error.to_string();
            assert!(
                message.contains("invalid source commitment"),
                "unexpected {label} sync error: {message}"
            );
            assert!(
                !message.contains(&claim_source_key) && !message.contains(&revision_source_key),
                "{label} diagnostic must not disclose a companion source key: {message}"
            );
        }

        shutdown_tx.send(()).expect("stop local D1 request trap");
        trap.await.expect("join local D1 request trap");
        assert_eq!(
            requests.load(Ordering::SeqCst),
            0,
            "invalid companion source keys must make zero D1 requests"
        );
    }

    #[tokio::test]
    async fn cloud_restore_rejects_invalid_companion_source_keys_before_local_writes() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        for (
            label,
            claim_schema_version,
            claim_source_key,
            revision_schema_version,
            revision_source_key,
        ) in invalid_subagent_companion_source_key_cases()
        {
            let conn = Database::connect("sqlite::memory:")
                .await
                .expect("open remote companion restore fixture");
            let session = fixture_session_row("remote-session", "remote-provider");
            let (mut claim, mut revision, _) = fixture_subagent_rows();
            claim.parent_session_id = session.session_id.clone();
            claim.content_schema_version = claim_schema_version;
            claim.source_key = claim_source_key.clone();
            revision.parent_session_id = session.session_id.clone();
            revision.content_schema_version = revision_schema_version;
            revision.source_key = revision_source_key.clone();

            let error = restore_agent_capture_from_rows_with_subagents(
                &conn,
                AgentCaptureRestoreRows {
                    sessions: std::slice::from_ref(&session),
                    checkpoints: &[],
                    claims: std::slice::from_ref(&claim),
                    revisions: std::slice::from_ref(&revision),
                    links: &[],
                    traces_head: None,
                    remote_is_known_ancestor: true,
                },
                false,
            )
            .await
            .expect_err("invalid remote companion source key must fail before restore writes");
            let message = error.to_string();
            assert!(
                message.contains("invalid source commitment"),
                "unexpected {label} restore error: {message}"
            );
            assert!(
                !message.contains(&claim_source_key) && !message.contains(&revision_source_key),
                "{label} diagnostic must not disclose a companion source key: {message}"
            );
            let writes = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM sqlite_master
                     WHERE type = 'table' AND name IN (
                        'agent_session', 'agent_subagent_content_claim',
                        'agent_subagent_content_revision', 'agent_subagent_link'
                     )"
                    .to_string(),
                ))
                .await
                .expect("query remote companion restore fixture")
                .expect("remote companion restore count")
                .try_get_by::<i64, _>("n")
                .expect("decode remote companion restore count");
            assert_eq!(
                writes, 0,
                "{label} remote restore must not create local capture rows"
            );
        }
    }

    use std::{env, ffi::OsString, fs, sync::Arc};

    use git_internal::internal::object::types::ObjectType;
    use object_store::memory::InMemory;
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        internal::config::ConfigKv,
        utils::test::{ChangeDirGuard, ScopedEnvVar, setup_with_new_libra_in},
    };

    struct LegacyCaptureProgress {
        completions: std::sync::atomic::AtomicUsize,
    }

    impl CloudSyncProgress for LegacyCaptureProgress {
        fn on_agent_capture_done(
            &self,
            _sessions_synced: usize,
            _sessions_failed: usize,
            _checkpoints_synced: usize,
            _checkpoints_failed: usize,
        ) {
            self.completions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn additive_subagent_progress_forwards_to_legacy_callback() {
        let progress = LegacyCaptureProgress {
            completions: std::sync::atomic::AtomicUsize::new(0),
        };
        progress.on_agent_capture_done_with_subagents(1, 0, 2, 0, 3, 0);
        assert_eq!(
            progress
                .completions
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    fn test_object_index_row(hash: ObjectHash, size: i64) -> ObjectIndexRow {
        ObjectIndexRow {
            o_id: hash.to_string(),
            o_type: "blob".to_string(),
            o_size: size,
            repo_id: "test-repo".to_string(),
            created_at: 0,
            is_synced: 1,
            object_format: None,
        }
    }

    /// Pre-GK-01 D1 `agent_kind` CHECK: the original seven kinds, no `grok`.
    const PRE_GROK_KINDS: &str =
        "'claude_code', 'cursor', 'codex', 'gemini', 'opencode', 'copilot', 'factory_ai'";

    async fn insert_local_capture_session(kind: &str) {
        use sea_orm::Statement;

        let conn = db::get_db_conn_instance().await;
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
              state, working_dir, started_at, last_event_at, metadata_json,
              redaction_report, sync_revision, repo_id, worktree_id, scope_state)
             VALUES (?, ?, 'provider-session', 'stopped', '/fixture', 1, 2, '{}', '{}',
               1, 'fixture-repo', '', 'scoped')",
            [format!("{kind}__session").into(), kind.into()],
        ))
        .await
        .expect("insert local capture session");
    }

    async fn remote_table_present(
        mock: &crate::utils::d1_client::test_support::MockD1,
        table: &str,
    ) -> bool {
        #[derive(serde::Deserialize)]
        struct Name {
            #[allow(dead_code)]
            name: String,
        }
        let rows: Vec<Name> = mock
            .client()
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
                Some(vec![serde_json::json!(table)]),
            )
            .await
            .expect("query mock sqlite_master");
        !rows.is_empty()
    }

    /// Index one local object as not yet synced, so a sync that got past the
    /// probe would upload it to R2.
    async fn insert_unsynced_local_object() {
        index_local_object(b"pi-18 unsynced object\n").await;
    }

    /// Store `data` as a local blob and index it as not yet synced.
    async fn index_local_object(data: &[u8]) -> ObjectHash {
        use crate::internal::model::object_index;

        let repo_id = ensure_repo_id().await;
        let hash = ObjectHash::from_type_and_data(ObjectType::Blob, data);
        LocalStorage::try_new(path::objects())
            .expect("storage")
            .put(&hash, data, ObjectType::Blob)
            .await
            .expect("write local object");
        object_index::ActiveModel {
            o_id: Set(hash.to_string()),
            o_type: Set("blob".to_string()),
            o_size: Set(data.len() as i64),
            repo_id: Set(repo_id),
            created_at: Set(0),
            is_synced: Set(0),
            ..Default::default()
        }
        .insert(&db::get_db_conn_instance().await)
        .await
        .expect("index unsynced local object");
        hash
    }

    async fn r2_object_count(store: &InMemory) -> usize {
        use futures::TryStreamExt;
        use object_store::ObjectStore;

        store
            .list(None)
            .try_collect::<Vec<_>>()
            .await
            .expect("list in-memory R2")
            .len()
    }

    /// Run a sync against `mock` that must be refused by the PI-18 probe, and
    /// assert that D1 saw exactly the `sqlite_master` probes of
    /// `probed_tables`, in order, and that nothing reached R2.
    async fn assert_sync_refused_before_remote_write(
        mock: &crate::utils::d1_client::test_support::MockD1,
        probed_tables: &[&str],
    ) -> String {
        let before = mock.request_bodies().await.len();
        let r2 = Arc::new(InMemory::new());

        let error = super::sync::run_cloud_sync_with_clients(
            CloudSyncContext {
                batch_size: 50,
                force: false,
            },
            &SilentCloudSyncProgress,
            mock.client(),
            Some(RemoteStorage::new(r2.clone())),
        )
        .await
        .expect_err("the PI-18 probe must refuse this sync");

        let CloudError::RemoteCaptureKindRefused(detail) = &error else {
            panic!("unexpected error: {error:?}");
        };
        let detail = detail.clone();
        assert!(detail.contains("before any remote write"), "{detail}");
        assert_eq!(
            error.into_cli_error("sync").stable_code(),
            StableErrorCode::ConflictOperationBlocked
        );
        let requests = mock.request_bodies().await[before..].to_vec();
        let expected: Vec<serde_json::Value> = probed_tables
            .iter()
            .map(|table| {
                serde_json::json!({
                    "sql": "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    "params": [table],
                })
            })
            .collect();
        assert_eq!(
            requests, expected,
            "only the read-only schema probes may precede the refusal"
        );
        assert_eq!(
            r2_object_count(&r2).await,
            0,
            "no object may reach R2 before the refusal"
        );
        assert!(
            !remote_table_present(mock, "object_index").await,
            "the refusal must precede the first remote write"
        );
        detail
    }

    async fn assert_pre_grok_table_refuses_local_grok(table: &str) {
        let _repo = enter_isolated_libra_repo().await;
        insert_local_capture_session("grok").await;
        insert_unsynced_local_object().await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        mock.client()
            .execute(
                &format!(
                    "CREATE TABLE {table} (session_id TEXT NOT NULL, agent_kind TEXT NOT NULL CHECK(agent_kind IN ({PRE_GROK_KINDS})))"
                ),
                None,
            )
            .await
            .expect("create pre-GK-01 remote table");

        let probed: &[&str] = match table {
            "agent_session" => &["agent_session"],
            _ => &["agent_session", table],
        };
        let detail = assert_sync_refused_before_remote_write(&mock, probed).await;
        assert!(detail.contains(table), "{detail}");
        assert!(detail.contains("grok"), "{detail}");
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn d1_effective_kind_check_refuses_grok_on_pre_gk01_agent_session() {
        assert_pre_grok_table_refuses_local_grok("agent_session").await;
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn d1_effective_kind_check_refuses_grok_on_pre_gk01_capture_session_v2() {
        assert_pre_grok_table_refuses_local_grok("agent_capture_session_v2").await;
    }

    #[test]
    fn d1_effective_kind_check_refusal_is_conflict_with_backfill_hint() {
        let error =
            CloudError::RemoteCaptureKindRefused("refused".to_string()).into_cli_error("sync");
        assert_eq!(
            error.stable_code(),
            StableErrorCode::ConflictOperationBlocked
        );
        let hints: Vec<&str> = error.hints().iter().map(|hint| hint.as_str()).collect();
        assert_eq!(hints, [REMOTE_CAPTURE_KIND_RECOVERY_HINT]);
        assert!(REMOTE_CAPTURE_KIND_RECOVERY_HINT.contains("new D1 database"));
        assert!(REMOTE_CAPTURE_KIND_RECOVERY_HINT.contains("`libra cloud sync --force`"));
    }

    #[test]
    fn local_capture_kind_refusal_is_conflict_with_two_hints() {
        let error =
            CloudError::LocalCaptureKindRefused("refused".to_string()).into_cli_error("sync");
        assert_eq!(
            error.stable_code(),
            StableErrorCode::ConflictOperationBlocked
        );
        let hints: Vec<&str> = error.hints().iter().map(|hint| hint.as_str()).collect();
        assert_eq!(
            hints,
            [
                LOCAL_CAPTURE_KIND_REFUSAL_HINT,
                LOCAL_CAPTURE_KIND_BACKUP_HINT
            ]
        );
        assert!(LOCAL_CAPTURE_KIND_REFUSAL_HINT.contains("`--force` does not bypass it"));
        for forbidden in ["delete", "drop", "clear"] {
            assert!(
                !LOCAL_CAPTURE_KIND_REFUSAL_HINT.contains(forbidden)
                    && !LOCAL_CAPTURE_KIND_BACKUP_HINT.contains(forbidden),
                "the hints must not suggest removing capture data ({forbidden})"
            );
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn d1_effective_kind_check_fails_closed_on_unparseable_check() {
        let _repo = enter_isolated_libra_repo().await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        mock.client()
            .execute(
                "CREATE TABLE agent_session (session_id TEXT NOT NULL, agent_kind TEXT NOT NULL)",
                None,
            )
            .await
            .expect("create remote table without a kind CHECK");

        let error = super::sync::refuse_kinds_rejected_by_remote_check(
            &mock.client(),
            &["claude_code".to_string()],
        )
        .await
        .expect_err("an unreadable CHECK must fail closed");
        match error {
            CloudError::RemoteCaptureKindRefused(detail) => {
                assert!(detail.contains("could not be read"), "{detail}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn d1_effective_kind_check_fails_closed_without_local_sessions() {
        let _repo = enter_isolated_libra_repo().await;
        insert_unsynced_local_object().await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        mock.client()
            .execute(
                "CREATE TABLE agent_capture_session_v2 (session_id TEXT NOT NULL, agent_kind TEXT NOT NULL)",
                None,
            )
            .await
            .expect("create remote table without a kind CHECK");

        let detail = assert_sync_refused_before_remote_write(
            &mock,
            &["agent_session", "agent_capture_session_v2"],
        )
        .await;
        assert!(detail.contains("agent_capture_session_v2"), "{detail}");
        assert!(detail.contains("could not be read"), "{detail}");
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn d1_effective_kind_check_fails_closed_on_failed_check_read() {
        const REMOTE_SENTINEL: &str = "REMOTE_D1_SCHEMA_TEXT=/private/remote/detail";

        let _repo = enter_isolated_libra_repo().await;
        insert_local_capture_session("claude_code").await;
        insert_unsynced_local_object().await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        mock.fail_once_when_sql_contains("sqlite_master", REMOTE_SENTINEL);

        let detail = assert_sync_refused_before_remote_write(&mock, &["agent_session"]).await;
        assert!(detail.contains("could not be read"), "{detail}");
        assert!(detail.contains("D1 code 7500"), "{detail}");
        assert!(!detail.contains(REMOTE_SENTINEL), "{detail}");
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn d1_effective_kind_check_accepts_absent_tables_and_full_check() {
        let _repo = enter_isolated_libra_repo().await;
        let local = ["claude_code".to_string(), "grok".to_string()];

        let absent = crate::utils::d1_client::test_support::MockD1::spawn().await;
        super::sync::refuse_kinds_rejected_by_remote_check(&absent.client(), &local)
            .await
            .expect("absent remote tables are created with today's DDL");

        let current = crate::utils::d1_client::test_support::MockD1::spawn().await;
        current
            .client()
            .ensure_agent_session_table()
            .await
            .expect("create current remote agent_session");
        super::sync::refuse_kinds_rejected_by_remote_check(&current.client(), &local)
            .await
            .expect("a CHECK that already lists every local kind must not refuse");
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn d1_effective_kind_check_force_backfill_covers_local_object_index() {
        use crate::internal::model::object_index;

        let _repo = enter_isolated_libra_repo().await;
        let repo_id = ensure_repo_id().await;
        let local = LocalStorage::try_new(path::objects()).expect("storage");
        let conn = db::get_db_conn_instance().await;
        let mut expected = Vec::new();
        for data in [
            &b"pi-18 one\n"[..],
            &b"pi-18 two\n"[..],
            &b"pi-18 three\n"[..],
        ] {
            let hash = ObjectHash::from_type_and_data(ObjectType::Blob, data);
            local
                .put(&hash, data, ObjectType::Blob)
                .await
                .expect("write local object");
            object_index::ActiveModel {
                o_id: Set(hash.to_string()),
                o_type: Set("blob".to_string()),
                o_size: Set(data.len() as i64),
                repo_id: Set(repo_id.clone()),
                created_at: Set(0),
                // Already mirrored to the old database; default sync skips it.
                is_synced: Set(1),
                ..Default::default()
            }
            .insert(&conn)
            .await
            .expect("index local object");
            expected.push(hash.to_string());
        }
        expected.sort();

        let indexed = |mock: &crate::utils::d1_client::test_support::MockD1| {
            let client = mock.client();
            let repo_id = repo_id.clone();
            async move {
                let mut ids: Vec<String> = client
                    .get_object_indexes(&repo_id)
                    .await
                    .expect("read new D1 object index")
                    .into_iter()
                    .map(|row| row.o_id)
                    .collect();
                ids.sort();
                ids
            }
        };

        let without_force = crate::utils::d1_client::test_support::MockD1::spawn().await;
        super::sync::run_cloud_sync_with_clients(
            CloudSyncContext {
                batch_size: 50,
                force: false,
            },
            &SilentCloudSyncProgress,
            without_force.client(),
            Some(RemoteStorage::new(Arc::new(InMemory::new()))),
        )
        .await
        .expect("default sync against a new database");
        assert!(
            indexed(&without_force).await.is_empty(),
            "default sync skips rows already marked synced"
        );

        let with_force = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        super::sync::run_cloud_sync_with_clients(
            CloudSyncContext {
                batch_size: 50,
                force: true,
            },
            &SilentCloudSyncProgress,
            with_force.client(),
            Some(RemoteStorage::new(r2.clone())),
        )
        .await
        .expect("--force sync against a new database");
        assert_eq!(indexed(&with_force).await, expected);
        // The injected store observes real uploads, so the refusal tests'
        // zero-object assertion is meaningful.
        assert!(r2_object_count(&r2).await >= expected.len());
    }

    // ---- PI-09: frozen snapshot spool --------------------------------------

    async fn insert_branch_ref(name: &str) {
        use crate::internal::model::reference::{self, ConfigKind};

        reference::ActiveModel {
            name: Set(Some(name.to_owned())),
            kind: Set(ConfigKind::Branch),
            commit: Set(Some("a".repeat(40))),
            remote: Set(None),
            worktree_id: Set(None),
            ..Default::default()
        }
        .insert(&db::get_db_conn_instance().await)
        .await
        .expect("insert local ref");
    }

    async fn local_is_synced(hash: &ObjectHash) -> i32 {
        use crate::internal::model::object_index;

        object_index::Entity::find()
            .filter(object_index::Column::OId.eq(hash.to_string()))
            .one(&db::get_db_conn_instance().await)
            .await
            .expect("read local object_index")
            .expect("indexed object")
            .is_synced
    }

    fn spool_runs() -> Vec<PathBuf> {
        let snapshots = util::storage_path()
            .join(super::snapshot::SPOOL_ROOT_DIR)
            .join(super::snapshot::SPOOL_SNAPSHOTS_DIR);
        let Ok(entries) = fs::read_dir(&snapshots) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("run-"))
            })
            .collect()
    }

    /// Run a sync against fresh mocks with `tuning`; the after-freeze hook
    /// receives the run directory.
    async fn sync_with_tuning(
        mock: &crate::utils::d1_client::test_support::MockD1,
        r2: &Arc<InMemory>,
        tuning: &super::snapshot::SnapshotTuning,
    ) -> CloudResult<CloudSyncReport> {
        super::sync::run_cloud_sync_from_snapshot(
            CloudSyncContext {
                batch_size: 2,
                force: false,
            },
            &SilentCloudSyncProgress,
            mock.client(),
            Some(RemoteStorage::new(r2.clone())),
            tuning,
        )
        .await
    }

    /// Hook that records the run directory and then runs `extra`.
    fn recording_hook(
        seen: Arc<std::sync::Mutex<Vec<PathBuf>>>,
        extra: impl Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync + 'static,
    ) -> super::snapshot::AfterFreezeHook {
        Arc::new(move |dir: PathBuf| {
            if let Ok(mut seen) = seen.lock() {
                seen.push(dir);
            }
            extra()
        })
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_excludes_writes_after_the_freeze() {
        let _repo = enter_isolated_libra_repo().await;
        let repo_id = ensure_repo_id().await;
        let frozen_a = index_local_object(b"pi-09 frozen a\n").await;
        let frozen_b = index_local_object(b"pi-09 frozen b\n").await;
        insert_branch_ref("refs/heads/frozen").await;
        ConfigKv::set("cloud.name", "pi09-frozen-name", false)
            .await
            .expect("set cloud.name");
        let late = ObjectHash::from_type_and_data(ObjectType::Blob, b"pi-09 late\n");

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(recording_hook(seen.clone(), || {
                Box::pin(async {
                    index_local_object(b"pi-09 late\n").await;
                    insert_branch_ref("refs/heads/late").await;
                    ConfigKv::set("cloud.name", "pi09-late-name", false)
                        .await
                        .expect("change cloud.name after the freeze");
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let report = sync_with_tuning(&mock, &r2, &tuning)
            .await
            .expect("sync from the frozen snapshot");
        assert_eq!(report.total_unsynced, 2);
        assert_eq!(report.synced_count, 2);
        assert_eq!(seen.lock().map(|seen| seen.len()).unwrap_or(0), 1);

        let mut indexed: Vec<String> = mock
            .client()
            .get_object_indexes(&repo_id)
            .await
            .expect("read D1 object index")
            .into_iter()
            .map(|row| row.o_id)
            .collect();
        indexed.sort();
        let mut expected = vec![frozen_a.to_string(), frozen_b.to_string()];
        expected.sort();
        assert_eq!(indexed, expected, "only frozen identities reach D1");

        let remote = RemoteStorage::new(r2.clone());
        assert_eq!(
            remote.exist_batch(&[frozen_a, frozen_b, late]).await,
            vec![true, true, false],
            "only frozen objects reach R2"
        );
        let metadata = String::from_utf8(remote.get_metadata().await.expect("read metadata"))
            .expect("utf-8 metadata");
        assert!(metadata.contains("refs/heads/frozen"), "{metadata}");
        assert!(
            !metadata.contains("refs/heads/late"),
            "a ref written after the freeze must wait for the next sync: {metadata}"
        );

        #[derive(serde::Deserialize)]
        struct NameRow {
            name: String,
        }
        let names: Vec<NameRow> = mock
            .client()
            .query(
                "SELECT name FROM repositories WHERE repo_id = ?1",
                Some(vec![serde_json::json!(repo_id)]),
            )
            .await
            .expect("read D1 repositories");
        assert_eq!(
            names.into_iter().map(|row| row.name).collect::<Vec<_>>(),
            vec!["pi09-frozen-name".to_string()],
            "the project name comes from the frozen snapshot"
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_uses_a_unique_run_dir_under_snapshots() {
        let _repo = enter_isolated_libra_repo().await;
        index_local_object(b"pi-09 unique\n").await;
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(recording_hook(seen.clone(), || Box::pin(async {}))),
            ..Default::default()
        };
        for _ in 0..2 {
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
                .await
                .expect("sync");
        }
        let seen = seen.lock().map(|seen| seen.clone()).unwrap_or_default();
        assert_eq!(seen.len(), 2);
        assert_ne!(seen[0], seen[1], "each run gets its own directory");
        let snapshots = util::storage_path()
            .canonicalize()
            .expect("canonical storage")
            .join(super::snapshot::SPOOL_ROOT_DIR)
            .join(super::snapshot::SPOOL_SNAPSHOTS_DIR);
        for dir in &seen {
            let parent = dir
                .parent()
                .expect("run dir parent")
                .canonicalize()
                .expect("canonical parent");
            assert_eq!(parent, snapshots, "{}", dir.display());
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
            assert!(name.starts_with("run-"), "{name}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let _repo = enter_isolated_libra_repo().await;
        index_local_object(b"pi-09 private\n").await;
        let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = modes.clone();
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(Arc::new(move |dir: PathBuf| {
                let recorded = recorded.clone();
                Box::pin(async move {
                    let mut found = vec![(
                        "run".to_string(),
                        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                    )];
                    for entry in fs::read_dir(&dir).unwrap() {
                        let entry = entry.unwrap();
                        found.push((
                            entry.file_name().to_string_lossy().into_owned(),
                            entry.metadata().unwrap().permissions().mode() & 0o777,
                        ));
                    }
                    recorded.lock().unwrap().extend(found);
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        let mut modes = modes.lock().unwrap().clone();
        modes.sort();
        assert_eq!(
            modes,
            vec![
                ("catalog.json".to_string(), 0o600),
                ("objects.jsonl".to_string(), 0o600),
                ("owner.json".to_string(), 0o600),
                ("refs.json".to_string(), 0o600),
                ("repository.json".to_string(), 0o600),
                ("run".to_string(), 0o700),
            ]
        );
        let root = util::storage_path().join(super::snapshot::SPOOL_ROOT_DIR);
        for dir in [
            root.clone(),
            root.join(super::snapshot::SPOOL_SNAPSHOTS_DIR),
        ] {
            assert_eq!(
                fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700,
                "{}",
                dir.display()
            );
        }
    }

    /// Give `dir` a protected DACL granting only the current user and SYSTEM
    /// and make the current user its owner, exactly as the refusal's remedy
    /// says: grant, set the owner (an elevated runner's new files are owned
    /// by the Administrators group), then remove any other account `icacls`
    /// still lists. The final listing goes to stderr for a failing test.
    #[cfg(windows)]
    fn make_private_with_icacls(dir: &std::path::Path) {
        let user = env::var("USERNAME").expect("USERNAME");
        let icacls = |args: &[&str]| {
            let output = std::process::Command::new("icacls")
                .arg(dir)
                .args(args)
                .output()
                .expect("run icacls");
            assert!(
                output.status.success(),
                "icacls {args:?} failed for {}: {}",
                dir.display(),
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        icacls(&[
            "/inheritance:r",
            "/grant:r",
            &format!("{user}:(OI)(CI)F"),
            "SYSTEM:(OI)(CI)F",
        ]);
        icacls(&["/setowner", &user]);
        // `icacls <dir>` prints `<dir> <principal>:(...)` and then one
        // `<principal>:(...)` per line.
        let shown = dir.display().to_string();
        let listing = icacls(&[]);
        let lowered_user = user.to_lowercase();
        for line in listing.lines() {
            let entry = line.strip_prefix(shown.as_str()).unwrap_or(line).trim();
            let Some((principal, _)) = entry.split_once(":(") else {
                continue;
            };
            let principal = principal.trim();
            let lowered = principal.to_lowercase();
            if principal.is_empty()
                || lowered == lowered_user
                || lowered.ends_with(&format!("\\{lowered_user}"))
                || lowered == "nt authority\\system"
                || lowered == "system"
            {
                continue;
            }
            let target = if principal.starts_with("S-1-") {
                format!("*{principal}")
            } else {
                principal.to_string()
            };
            icacls(&["/remove:g", &target]);
        }
        eprintln!("icacls {shown} after making it private:\n{}", icacls(&[]));
    }

    #[cfg(windows)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_refuses_an_inherited_acl_root_on_windows() {
        let _repo = enter_isolated_libra_repo().await;
        let status = std::process::Command::new("icacls")
            .arg(util::storage_path())
            .arg("/reset")
            .status()
            .expect("run icacls");
        assert!(status.success());
        index_local_object(b"pi-09 inherited windows\n").await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let error = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &Default::default())
            .await
            .expect_err("a .libra with inherited permissions must be refused");
        let detail = error.to_string();
        assert!(detail.contains("icacls"), "{detail}");
        assert!(
            detail.contains("/setowner"),
            "the remedy also makes the current user the owner: {detail}"
        );
        assert!(
            detail.contains(&util::storage_path().display().to_string()),
            "{detail}"
        );
        assert!(!remote_table_present(&mock, "object_index").await);
    }

    #[cfg(windows)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_is_owner_only_on_windows() {
        let _repo = enter_isolated_libra_repo().await;
        make_private_with_icacls(&util::storage_path());
        index_local_object(b"pi-09 private windows\n").await;
        let checked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = checked.clone();
        let file_acls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded_files = file_acls.clone();
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(Arc::new(move |dir: PathBuf| {
                let recorded = recorded.clone();
                let recorded_files = recorded_files.clone();
                Box::pin(async move {
                    for name in [
                        "owner.json",
                        "objects.jsonl",
                        "refs.json",
                        "repository.json",
                        "catalog.json",
                    ] {
                        let output = std::process::Command::new("icacls")
                            .arg(dir.join(name))
                            .output()
                            .expect("run icacls");
                        recorded_files.lock().unwrap().push((
                            name.to_string(),
                            String::from_utf8_lossy(&output.stdout).into_owned(),
                        ));
                    }
                    let mut results = Vec::new();
                    let mut current = Some(dir.as_path());
                    for _ in 0..3 {
                        let Some(path) = current else { break };
                        results.push(
                            crate::utils::windows_fs::open_private_directory(path)
                                .map(|_| ())
                                .map_err(|error| format!("{}: {error}", path.display())),
                        );
                        current = path.parent();
                    }
                    recorded.lock().unwrap().extend(results);
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        let checked = checked.lock().unwrap().clone();
        assert_eq!(
            checked.len(),
            3,
            "run, snapshots and cloud-sync were checked"
        );
        for result in checked {
            result.expect("spool directories carry an owner-only protected DACL");
        }
        // `icacls <file>` lists one principal per line after the path line;
        // only the current user and SYSTEM may appear.
        let files = file_acls.lock().unwrap().clone();
        assert_eq!(files.len(), 5, "{files:?}");
        let user = env::var("USERNAME").expect("USERNAME").to_lowercase();
        for (name, acl) in files {
            let acl = acl.to_lowercase();
            assert!(acl.contains(&user), "{name}: {acl}");
            for line in acl.lines() {
                let Some((_, principal)) = line.split_once(name.as_str()) else {
                    let entry = line.trim();
                    if entry.is_empty() || entry.starts_with("successfully") {
                        continue;
                    }
                    assert!(
                        entry.contains(&user) || entry.contains("system"),
                        "{name} grants another principal: {acl}"
                    );
                    continue;
                };
                let entry = principal.trim();
                assert!(
                    entry.contains(&user) || entry.contains("system"),
                    "{name} grants another principal: {acl}"
                );
            }
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_refuses_an_inherited_acl_parent_on_windows() {
        let _repo = enter_isolated_libra_repo().await;
        make_private_with_icacls(&util::storage_path());
        index_local_object(b"pi-09 inherited parent windows\n").await;
        // A pre-existing `cloud-sync` that inherits ordinary permissions.
        let cloud_sync = util::storage_path().join(super::snapshot::SPOOL_ROOT_DIR);
        fs::create_dir(&cloud_sync).unwrap();
        let status = std::process::Command::new("icacls")
            .arg(&cloud_sync)
            .args(["/inheritance:e", "/grant", "*S-1-5-11:(OI)(CI)M"])
            .status()
            .expect("run icacls");
        assert!(status.success());
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let error = sync_with_tuning(&mock, &r2, &Default::default())
            .await
            .expect_err("a spool parent without a protected DACL must be refused");
        let detail = error.to_string();
        assert!(detail.contains("icacls"), "{detail}");
        assert!(
            detail.contains(&cloud_sync.display().to_string()),
            "{detail}"
        );
        assert!(!remote_table_present(&mock, "object_index").await);
        assert_eq!(r2_object_count(&r2).await, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_refuses_unsafe_parents() {
        use std::os::unix::fs::PermissionsExt;

        let _repo = enter_isolated_libra_repo().await;
        index_local_object(b"pi-09 unsafe parent\n").await;
        let storage = util::storage_path();
        let root = storage.join(super::snapshot::SPOOL_ROOT_DIR);

        // A `.libra` that is itself a symlink is refused, although repository
        // discovery resolves it to a valid storage directory.
        let workdir = util::working_dir();
        let raw_root = workdir.join(util::ROOT_DIR);
        let moved = workdir.join(".libra-real");
        fs::rename(&raw_root, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &raw_root).unwrap();
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let outcome =
            sync_with_tuning(&mock, &Arc::new(InMemory::new()), &Default::default()).await;
        fs::remove_file(&raw_root).unwrap();
        fs::rename(&moved, &raw_root).unwrap();
        let error = outcome.expect_err("a symlinked .libra must be refused");
        assert!(error.to_string().contains("symbolic link"), "{error}");
        assert!(!remote_table_present(&mock, "object_index").await);

        // A group-writable `.libra` is not a trusted spool root.
        let original = fs::metadata(&storage).unwrap().permissions().mode() & 0o777;
        fs::set_permissions(&storage, fs::Permissions::from_mode(0o775)).unwrap();
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let error = sync_with_tuning(&mock, &r2, &Default::default())
            .await
            .expect_err("a group-writable repository storage directory must be refused");
        fs::set_permissions(&storage, fs::Permissions::from_mode(original)).unwrap();
        assert!(
            error
                .to_string()
                .contains("repository storage directory is writable by group"),
            "{error}"
        );
        assert!(!root.exists(), "nothing is created under an untrusted root");
        assert!(!remote_table_present(&mock, "object_index").await);
        assert_eq!(r2_object_count(&r2).await, 0);

        // A symlinked `cloud-sync` is never followed.
        let outside = tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), &root).unwrap();
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let error = sync_with_tuning(&mock, &r2, &Default::default())
            .await
            .expect_err("a symlinked spool parent must be refused");
        assert!(error.to_string().contains("symbolic link"), "{error}");
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
        assert!(!remote_table_present(&mock, "object_index").await);
        assert_eq!(r2_object_count(&r2).await, 0);
        fs::remove_file(&root).unwrap();

        // A group-writable existing parent is refused too.
        let snapshots = root.join(super::snapshot::SPOOL_SNAPSHOTS_DIR);
        fs::create_dir_all(&snapshots).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&snapshots, fs::Permissions::from_mode(0o770)).unwrap();
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let error = sync_with_tuning(&mock, &r2, &Default::default())
            .await
            .expect_err("a group-writable spool parent must be refused");
        assert!(error.to_string().contains("writable by group"), "{error}");
        assert!(!remote_table_present(&mock, "object_index").await);
        assert!(spool_runs().is_empty());

        // Safe existing parents are reused, so a second sync still works.
        fs::set_permissions(&snapshots, fs::Permissions::from_mode(0o700)).unwrap();
        for _ in 0..2 {
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            sync_with_tuning(&mock, &Arc::new(InMemory::new()), &Default::default())
                .await
                .expect("sync with safe existing parents");
        }

        // A parent another sync created first is reused after validation,
        // unlike the run directory.
        super::snapshot::create_parent_or_reuse(&snapshots)
            .expect("an existing parent is accepted for validation");

        // The run directory itself is created exclusively: a sync whose run
        // name already exists refuses and leaves that directory alone.
        let taken = snapshots.join("run-taken");
        fs::create_dir(&taken).unwrap();
        fs::write(taken.join("sentinel"), b"keep").unwrap();
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let tuning = super::snapshot::SnapshotTuning {
            run_name: Some("run-taken".to_string()),
            ..Default::default()
        };
        sync_with_tuning(&mock, &r2, &tuning)
            .await
            .expect_err("an existing run directory must not be reused");
        assert_eq!(fs::read(taken.join("sentinel")).unwrap(), b"keep");
        assert_eq!(fs::read_dir(&taken).unwrap().count(), 1);
        assert!(!remote_table_present(&mock, "object_index").await);
        assert_eq!(r2_object_count(&r2).await, 0);
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_writes_owner_marker() {
        let _repo = enter_isolated_libra_repo().await;
        index_local_object(b"pi-09 marker\n").await;
        let markers = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = markers.clone();
        let before = chrono::Utc::now().timestamp_millis();
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(Arc::new(move |dir: PathBuf| {
                let recorded = recorded.clone();
                Box::pin(async move {
                    let bytes = fs::read(dir.join(super::snapshot::OWNER_MARKER_FILE)).unwrap();
                    let marker: super::snapshot::OwnerMarker =
                        serde_json::from_slice(&bytes).unwrap();
                    recorded.lock().unwrap().push(marker);
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        let markers = markers.lock().unwrap().clone();
        assert_eq!(markers.len(), 1);
        let marker = &markers[0];
        assert_eq!(marker.tool, super::snapshot::OWNER_MARKER_TOOL);
        assert_eq!(marker.pid, std::process::id());
        assert_eq!(marker.libra_version, env!("CARGO_PKG_VERSION"));
        assert!(marker.started_at_unix_ms >= before, "{marker:?}");
        assert_eq!(marker.pid_namespace, super::snapshot::pid_namespace());
        assert_eq!(marker.boot_id, super::snapshot::boot_id());
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(marker.boot_id.is_some(), "{marker:?}");
        #[cfg(unix)]
        assert!(!marker.host.is_empty(), "{marker:?}");
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_is_released_on_success_and_failure() {
        let _repo = enter_isolated_libra_repo().await;
        index_local_object(b"pi-09 release\n").await;
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(recording_hook(seen.clone(), || Box::pin(async {}))),
            ..Default::default()
        };

        // A failed object upload (its D1 index write fails) ends the run
        // with a failure count.
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        mock.fail_once_when_sql_contains(
            "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced, object_format)",
            "injected upload failure",
        );
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("an object failure is reported in the result");
        assert_eq!(report.failed_count, 1);
        assert!(spool_runs().is_empty(), "the failed run released its spool");

        // A hard error after the freeze returns early.
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        mock.fail_once_when_sql_contains("repositories", "injected D1 failure");
        sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect_err("the injected D1 failure stops the run");
        assert!(
            spool_runs().is_empty(),
            "the aborted run released its spool"
        );

        // A removal failure keeps the sync result but is reported with the
        // directory to delete.
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let retained_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let retaining = super::snapshot::SnapshotTuning {
            fail_spool_release: true,
            after_freeze: Some(recording_hook(retained_seen.clone(), || Box::pin(async {}))),
            ..Default::default()
        };
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &retaining)
            .await
            .expect("a removal failure does not fail the sync");
        assert_eq!(report.failed_count, 0);
        let retained = retained_seen.lock().unwrap()[0].clone();
        assert!(
            retained.exists(),
            "the injected failure keeps the directory"
        );
        let warning = crate::utils::output::pending_warning_messages()
            .into_iter()
            .find(|message| message.contains(&retained.display().to_string()))
            .expect("a warning names the retained spool");
        assert!(
            warning.contains("delete that directory manually"),
            "{warning}"
        );
        // Through the CLI mode selector: human and `--progress=json` render
        // nothing more (the warning went through `emit_warning`), `--json`
        // carries the same note in `data.warnings[]`.
        assert_eq!(report.spool_warnings, vec![warning.clone()]);
        let json_mode = OutputConfig {
            json_format: Some(crate::utils::output::JsonFormat::Pretty),
            ..OutputConfig::default()
        };
        let progress_mode = OutputConfig {
            progress: ProgressMode::Json,
            ..OutputConfig::default()
        };
        assert!(
            finish_sync(&report, &OutputConfig::default())
                .unwrap()
                .is_none()
        );
        assert!(finish_sync(&report, &progress_mode).unwrap().is_none());
        let structured = finish_sync(&report, &json_mode)
            .unwrap()
            .expect("--json renders the cloud.sync data");
        let json = serde_json::to_value(structured).unwrap();
        assert_eq!(json["warnings"], serde_json::json!([warning]));
        fs::remove_dir_all(&retained).unwrap();

        // On a failed run the note joins the returned error instead.
        index_local_object(b"pi-09 release retained on failure\n").await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        mock.fail_once_when_sql_contains("repositories", "injected D1 failure");
        retained_seen.lock().unwrap().clear();
        let error = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &retaining)
            .await
            .expect_err("the injected D1 failure stops the run");
        let retained = retained_seen.lock().unwrap()[0].clone();
        let detail = error.to_string();
        assert!(detail.contains(&retained.display().to_string()), "{detail}");
        assert!(
            detail.contains("delete that directory manually"),
            "{detail}"
        );
        fs::remove_dir_all(&retained).unwrap();

        // When objects fail, the report becomes an error that names it too.
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        mock.fail_once_when_sql_contains(
            "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced, object_format)",
            "injected upload failure",
        );
        retained_seen.lock().unwrap().clear();
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &retaining)
            .await
            .expect("object failures are reported in the result");
        let retained = retained_seen.lock().unwrap()[0].clone();
        for mode in [
            OutputConfig::default(),
            json_mode.clone(),
            progress_mode.clone(),
        ] {
            let error =
                finish_sync(&report, &mode).expect_err("failed objects make the run an error");
            let detail = error.to_string();
            assert!(detail.contains("objects failed to sync"), "{detail}");
            assert!(detail.contains(&retained.display().to_string()), "{detail}");
        }
        fs::remove_dir_all(&retained).unwrap();

        // A spool write failure stops the run before the hook.
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let failing = super::snapshot::SnapshotTuning {
            fail_spool_write: true,
            ..Default::default()
        };
        sync_with_tuning(&mock, &Arc::new(InMemory::new()), &failing)
            .await
            .expect_err("the injected spool write failure stops the run");
        assert!(spool_runs().is_empty(), "the failed spool is released");

        // Success.
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        assert_eq!(report.failed_count, 0);
        assert!(
            spool_runs().is_empty(),
            "the successful run released its spool"
        );
        assert_eq!(seen.lock().map(|seen| seen.len()).unwrap_or(0), 3);
        for dir in seen.lock().map(|seen| seen.clone()).unwrap_or_default() {
            assert!(!dir.exists(), "{}", dir.display());
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_write_failure_precedes_remote_side_effects() {
        let _repo = enter_isolated_libra_repo().await;
        index_local_object(b"pi-09 disk full\n").await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let tuning = super::snapshot::SnapshotTuning {
            fail_spool_write: true,
            ..Default::default()
        };
        let error = sync_with_tuning(&mock, &r2, &tuning)
            .await
            .expect_err("a spool write failure stops the sync");
        let detail = error.to_string();
        assert!(detail.contains("no space left on device"), "{detail}");
        assert!(
            detail.contains("nothing was written to D1 or R2"),
            "{detail}"
        );
        let probe = "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1";
        let expected: Vec<serde_json::Value> = ["agent_session", "agent_capture_session_v2"]
            .iter()
            .map(|table| serde_json::json!({ "sql": probe, "params": [table] }))
            .collect();
        assert_eq!(
            mock.request_bodies().await,
            expected,
            "only the read-only probes may precede a spool failure"
        );
        assert_eq!(r2_object_count(&r2).await, 0);
        assert!(spool_runs().is_empty(), "the failed spool is released");
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_marks_only_frozen_identities() {
        let _repo = enter_isolated_libra_repo().await;
        let repo_id = ensure_repo_id().await;
        // Three frozen identities with a page size of two span two pages.
        let mut frozen = Vec::new();
        for data in [
            &b"pi-09 frozen synced 1\n"[..],
            &b"pi-09 frozen synced 2\n"[..],
            &b"pi-09 frozen synced 3\n"[..],
        ] {
            frozen.push(index_local_object(data).await);
        }
        let late = ObjectHash::from_type_and_data(ObjectType::Blob, b"pi-09 late synced\n");
        let tuning = super::snapshot::SnapshotTuning {
            page_size: Some(2),
            after_freeze: Some(Arc::new(|_dir: PathBuf| {
                Box::pin(async {
                    index_local_object(b"pi-09 late synced\n").await;
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let report = sync_with_tuning(&mock, &r2, &tuning).await.expect("sync");
        assert_eq!(report.total_unsynced, 3);
        assert_eq!(report.synced_count, 3);
        for hash in &frozen {
            assert_eq!(local_is_synced(hash).await, 1, "{hash}");
        }
        let mut indexed: Vec<String> = mock
            .client()
            .get_object_indexes(&repo_id)
            .await
            .expect("read D1 object index")
            .into_iter()
            .map(|row| row.o_id)
            .collect();
        indexed.sort();
        let mut expected: Vec<String> = frozen.iter().map(ToString::to_string).collect();
        expected.sort();
        assert_eq!(indexed, expected, "every frozen identity is uploaded once");
        assert_eq!(
            RemoteStorage::new(r2.clone()).exist_batch(&frozen).await,
            vec![true; 3]
        );
        assert_eq!(
            local_is_synced(&late).await,
            0,
            "a row indexed after the freeze stays unsynced"
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_leaves_failed_identities_unsynced() {
        use crate::internal::model::object_index;

        let _repo = enter_isolated_libra_repo().await;
        let first = index_local_object(b"pi-09 upload fails\n").await;
        let second = index_local_object(b"pi-09 upload works\n").await;

        // The first object's D1 index write fails: only the second is marked.
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        mock.fail_once_when_sql_contains(
            "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced, object_format)",
            "injected upload failure",
        );
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &Default::default())
            .await
            .expect("an object failure is reported in the result");
        assert_eq!((report.synced_count, report.failed_count), (1, 1));
        assert_eq!(
            local_is_synced(&first).await,
            0,
            "a failed upload stays unsynced"
        );
        assert_eq!(local_is_synced(&second).await, 1);

        // Reset, then make every local status update fail: nothing counts as
        // synced although the uploads themselves succeed.
        object_index::Entity::update_many()
            .col_expr(object_index::Column::IsSynced, Expr::value(0))
            .exec(&db::get_db_conn_instance().await)
            .await
            .expect("reset local sync flags");
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let failing = super::snapshot::SnapshotTuning {
            fail_status_update: true,
            ..Default::default()
        };
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &failing)
            .await
            .expect("status failures are reported in the result");
        assert_eq!((report.synced_count, report.failed_count), (0, 2));
        assert_eq!(local_is_synced(&first).await, 0);
        assert_eq!(local_is_synced(&second).await, 0);

        // A frozen row deleted after the freeze updates zero rows: that is a
        // failure, not a synced object.
        let deleted = index_local_object(b"pi-09 row deleted after freeze\n").await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let deleting = super::snapshot::SnapshotTuning {
            after_freeze: Some(Arc::new(move |_dir: PathBuf| {
                Box::pin(async move {
                    object_index::Entity::delete_many()
                        .filter(object_index::Column::OId.eq(deleted.to_string()))
                        .exec(&db::get_db_conn_instance().await)
                        .await
                        .expect("delete the frozen row");
                })
            })),
            ..Default::default()
        };
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &deleting)
            .await
            .expect("a vanished row is reported in the result");
        assert_eq!((report.synced_count, report.failed_count), (2, 1));
    }

    /// Progress that records spool warnings instead of printing them.
    struct RecordingSpoolProgress {
        notes: std::sync::Mutex<Vec<String>>,
    }

    impl CloudSyncProgress for RecordingSpoolProgress {
        fn on_spool_warning(&self, note: &str) {
            self.notes.lock().unwrap().push(note.to_string());
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_routes_its_warning_through_progress() {
        let _repo = enter_isolated_libra_repo().await;
        index_local_object(b"pi-09 progress warning\n").await;
        let retaining = super::snapshot::SnapshotTuning {
            fail_spool_release: true,
            ..Default::default()
        };

        // The renderer, not stderr, receives the note.
        let progress = RecordingSpoolProgress {
            notes: std::sync::Mutex::new(Vec::new()),
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = super::sync::run_cloud_sync_from_snapshot(
            CloudSyncContext {
                batch_size: 2,
                force: false,
            },
            &progress,
            mock.client(),
            Some(RemoteStorage::new(Arc::new(InMemory::new()))),
            &retaining,
        )
        .await
        .expect("sync");
        let notes = progress.notes.lock().unwrap().clone();
        assert_eq!(notes.len(), 1);
        assert_eq!(report.spool_warnings, vec![notes[0].clone()]);
        for run in spool_runs() {
            fs::remove_dir_all(run).unwrap();
        }

        // Under `--progress=json` every emitted line is one JSON event, and
        // the note is one of them.
        index_local_object(b"pi-09 ndjson warning\n").await;
        *JSON_PROGRESS_CAPTURE.lock().unwrap() = Some(Vec::new());
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let outcome = super::sync::run_cloud_sync_from_snapshot(
            CloudSyncContext {
                batch_size: 2,
                force: false,
            },
            &JsonCloudSyncProgress,
            mock.client(),
            Some(RemoteStorage::new(Arc::new(InMemory::new()))),
            &retaining,
        )
        .await;
        let lines = JSON_PROGRESS_CAPTURE
            .lock()
            .unwrap()
            .take()
            .unwrap_or_default();
        outcome.expect("sync");
        assert!(!lines.is_empty());
        let events: Vec<serde_json::Value> = lines
            .iter()
            .map(|line| {
                assert!(!line.contains('\n'), "{line}");
                serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line}"))
            })
            .collect();
        assert!(
            events
                .iter()
                .any(|event| event["event"] == "cloud_sync.spool.warning"
                    && event["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("delete that directory manually"))),
            "{lines:?}"
        );
        for run in spool_runs() {
            fs::remove_dir_all(run).unwrap();
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_drop_reports_a_retained_directory() {
        let _repo = enter_isolated_libra_repo().await;
        let storage = util::storage_path();
        let raw_root = util::working_dir().join(util::ROOT_DIR);
        let tuning = super::snapshot::SnapshotTuning {
            fail_spool_release: true,
            ..Default::default()
        };
        // Dropped without `finish`, as on a panic or a cancelled future. The
        // plain-text sink prints in every mode, `--json` included.
        *super::snapshot::DROP_WARNING_CAPTURE.lock().unwrap() = Some(Vec::new());
        let spool = super::snapshot::SnapshotSpool::create(
            &storage,
            &raw_root,
            &tuning,
            super::snapshot::SpoolWarningSink::Stderr,
        )
        .expect("create the spool");
        drop(spool);
        let printed = super::snapshot::DROP_WARNING_CAPTURE
            .lock()
            .unwrap()
            .take()
            .unwrap_or_default();
        let retained = spool_runs();
        assert_eq!(retained.len(), 1);
        let path = retained[0].display().to_string();
        assert_eq!(printed.len(), 1, "{printed:?}");
        assert!(
            printed[0].starts_with("warning: ") && printed[0].contains(&path),
            "{printed:?}"
        );
        assert!(
            crate::utils::output::pending_warning_messages()
                .iter()
                .any(|message| message.contains(&path)
                    && message.contains("delete that directory manually")),
            "the dropped spool must still be reported"
        );
        fs::remove_dir_all(&retained[0]).unwrap();
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_rejects_extra_or_unterminated_records() {
        let _repo = enter_isolated_libra_repo().await;
        let edit = |change: fn(&str) -> String| super::snapshot::SnapshotTuning {
            after_freeze: Some(Arc::new(move |dir: PathBuf| {
                Box::pin(async move {
                    let path = dir.join("objects.jsonl");
                    let text = fs::read_to_string(&path).unwrap_or_default();
                    fs::write(&path, change(&text)).unwrap();
                })
            })),
            ..Default::default()
        };
        // Zero frozen objects, one appended record.
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let tuning = edit(|_| {
            r#"{"id":999999,"o_id":"00","o_type":"blob","o_size":1,"repo_id":"x","created_at":0}
"#
            .to_string()
        });
        let error = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect_err("an object spool that should be empty must be refused");
        assert!(
            error
                .to_string()
                .contains("more identities than the freeze recorded"),
            "{error}"
        );
        assert!(spool_runs().is_empty());

        // One frozen object, one extra record.
        index_local_object(b"pi-09 extra record\n").await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let tuning = edit(|text| {
            format!(
                "{text}{}",
                r#"{"id":999999,"o_id":"00","o_type":"blob","o_size":1,"repo_id":"x","created_at":0}
"#
            )
        });
        let error = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect_err("an extra spool record must be refused");
        assert!(
            error
                .to_string()
                .contains("more identities than the freeze recorded"),
            "{error}"
        );

        // The last record without its newline.
        index_local_object(b"pi-09 unterminated record\n").await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let tuning = edit(|text| text.trim_end_matches('\n').to_string());
        let error = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect_err("an unterminated spool record must be refused");
        assert!(
            error.to_string().contains("not newline-terminated"),
            "{error}"
        );
        assert!(spool_runs().is_empty());
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_warning_follows_each_output_mode() {
        let _repo = enter_isolated_libra_repo().await;
        let retaining = super::snapshot::SnapshotTuning {
            fail_spool_release: true,
            ..Default::default()
        };
        let json_mode = OutputConfig {
            json_format: Some(crate::utils::output::JsonFormat::Pretty),
            ..OutputConfig::default()
        };
        let progress_mode = OutputConfig {
            progress: ProgressMode::Json,
            ..OutputConfig::default()
        };
        let quiet_mode = OutputConfig {
            quiet: true,
            ..OutputConfig::default()
        };
        let modes = [
            ("human", OutputConfig::default()),
            ("quiet", quiet_mode),
            ("json", json_mode),
            ("progress-json", progress_mode),
        ];
        for (index, (label, mode)) in modes.iter().enumerate() {
            index_local_object(format!("pi-09 mode {label}\n").as_bytes()).await;
            // The renderer `execute_safe` would pick for this mode.
            let progress = sync_progress_for(mode);
            assert_eq!(
                progress.spool_warnings_as_ndjson(),
                *label == "progress-json",
                "{label}"
            );
            *JSON_PROGRESS_CAPTURE.lock().unwrap() = Some(Vec::new());
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            let outcome = super::sync::run_cloud_sync_from_snapshot(
                CloudSyncContext {
                    batch_size: 2,
                    force: false,
                },
                progress,
                mock.client(),
                Some(RemoteStorage::new(Arc::new(InMemory::new()))),
                &retaining,
            )
            .await;
            let lines = JSON_PROGRESS_CAPTURE
                .lock()
                .unwrap()
                .take()
                .unwrap_or_default();
            let report = outcome.unwrap_or_else(|error| panic!("{label}: {error}"));
            let note = report
                .spool_warnings
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("{label}: no spool warning"));
            match *label {
                "progress-json" => {
                    // Every line on the progress stream is JSON, the note
                    // included.
                    assert!(!lines.is_empty());
                    for line in &lines {
                        let event: serde_json::Value = serde_json::from_str(line)
                            .unwrap_or_else(|_| panic!("not JSON: {line}"));
                        assert!(event.get("event").is_some(), "{line}");
                    }
                    assert!(
                        lines
                            .iter()
                            .any(|line| line.contains("cloud_sync.spool.warning"))
                    );
                    assert!(finish_sync(&report, mode).unwrap().is_none());
                }
                "json" => {
                    assert!(lines.is_empty(), "--json prints no progress events");
                    let structured = finish_sync(&report, mode)
                        .unwrap()
                        .expect("--json renders cloud.sync data");
                    let json = serde_json::to_value(structured).unwrap();
                    assert_eq!(json["warnings"], serde_json::json!([note]));
                }
                _ => {
                    // Human and `--quiet`: `emit_warning` keeps it on stderr.
                    assert!(lines.is_empty());
                    assert!(finish_sync(&report, mode).unwrap().is_none());
                    assert!(!crate::utils::output::structured_output_active());
                    assert!(
                        crate::utils::output::pending_warning_messages().contains(&note),
                        "{label} goes through emit_warning"
                    );
                }
            }
            let runs = spool_runs();
            assert_eq!(runs.len(), 1, "run {index}: {runs:?}");
            fs::remove_dir_all(&runs[0]).unwrap();
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_cancelled_run_keeps_ndjson_clean() {
        let _repo = enter_isolated_libra_repo().await;
        index_local_object(b"pi-09 cancelled\n").await;
        let progress_mode = OutputConfig {
            progress: ProgressMode::Json,
            ..OutputConfig::default()
        };
        // Freeze, then stall until the caller gives up on the future.
        let tuning = super::snapshot::SnapshotTuning {
            fail_spool_release: true,
            after_freeze: Some(Arc::new(|_dir: PathBuf| {
                Box::pin(async {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                })
            })),
            ..Default::default()
        };
        *JSON_PROGRESS_CAPTURE.lock().unwrap() = Some(Vec::new());
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            super::sync::run_cloud_sync_from_snapshot(
                CloudSyncContext {
                    batch_size: 2,
                    force: false,
                },
                sync_progress_for(&progress_mode),
                mock.client(),
                Some(RemoteStorage::new(Arc::new(InMemory::new()))),
                &tuning,
            ),
        )
        .await;
        let lines = JSON_PROGRESS_CAPTURE
            .lock()
            .unwrap()
            .take()
            .unwrap_or_default();
        assert!(cancelled.is_err(), "the sync future was cancelled");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("cloud_sync.spool.warning")),
            "{lines:?}"
        );
        for line in &lines {
            let _: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line}"));
        }
        let runs = spool_runs();
        assert_eq!(runs.len(), 1);
        fs::remove_dir_all(&runs[0]).unwrap();
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_failures_follow_each_output_mode() {
        let _repo = enter_isolated_libra_repo().await;
        let json_mode = OutputConfig {
            json_format: Some(crate::utils::output::JsonFormat::Pretty),
            ..OutputConfig::default()
        };
        let progress_mode = OutputConfig {
            progress: ProgressMode::Json,
            ..OutputConfig::default()
        };
        let quiet_mode = OutputConfig {
            quiet: true,
            ..OutputConfig::default()
        };
        let modes = [
            ("human", OutputConfig::default()),
            ("quiet", quiet_mode),
            ("json", json_mode),
            ("progress-json", progress_mode),
        ];
        let failures = ["upload", "hard-error", "spool-write", "marker-write"];
        let mut run = 0usize;
        for (label, mode) in &modes {
            for failure in failures {
                run += 1;
                index_local_object(format!("pi-09 failure {run}\n").as_bytes()).await;
                let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
                let mut tuning = super::snapshot::SnapshotTuning {
                    fail_spool_release: true,
                    ..Default::default()
                };
                match failure {
                    "upload" => mock.fail_once_when_sql_contains(
                        "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced, object_format)",
                        "injected upload failure",
                    ),
                    "hard-error" => {
                        mock.fail_once_when_sql_contains("repositories", "injected D1 failure")
                    }
                    "spool-write" => tuning.fail_spool_write = true,
                    _ => tuning.fail_marker_write = true,
                }
                // The renderer `execute_safe` picks for this mode, then the
                // decision it makes on the result.
                *JSON_PROGRESS_CAPTURE.lock().unwrap() = Some(Vec::new());
                let outcome = super::sync::run_cloud_sync_from_snapshot(
                    CloudSyncContext {
                        batch_size: 2,
                        force: false,
                    },
                    sync_progress_for(mode),
                    mock.client(),
                    Some(RemoteStorage::new(Arc::new(InMemory::new()))),
                    &tuning,
                )
                .await;
                let lines = JSON_PROGRESS_CAPTURE
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap_or_default();
                let error = match outcome {
                    Ok(report) => finish_sync(&report, mode)
                        .expect_err("a failed upload must not be rendered as success"),
                    Err(error) => error,
                };
                let retained = spool_runs();
                assert_eq!(retained.len(), 1, "{label}/{failure}: {retained:?}");
                let detail = error.to_string();
                assert!(
                    detail.contains(&retained[0].display().to_string())
                        && detail.contains("delete that directory manually"),
                    "{label}/{failure}: {detail}"
                );
                if *label == "progress-json" {
                    for line in &lines {
                        let _: serde_json::Value = serde_json::from_str(line)
                            .unwrap_or_else(|_| panic!("{label}/{failure} not JSON: {line}"));
                    }
                } else {
                    assert!(lines.is_empty(), "{label}/{failure}: {lines:?}");
                }
                fs::remove_dir_all(&retained[0]).unwrap();
            }
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_freeze_does_not_block_other_writers() {
        use crate::internal::model::object_index;

        let _repo = enter_isolated_libra_repo().await;
        let repo_id = ensure_repo_id().await;
        for n in 0..200 {
            index_local_object(format!("pi-09 writer page {n}\n").as_bytes()).await;
        }
        let db_path = util::storage_path().join(util::DATABASE);
        let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = writes.clone();
        let repo = repo_id.clone();
        // While the first attempt walks twenty pages, another connection (as
        // another process would) commits after every page with a 200 ms busy
        // timeout. The freeze holds no lock between statements, so each write
        // lands at once; the changed database then makes the freeze retry,
        // and the second attempt (no writer) is one consistent state that
        // includes those rows.
        let tuning = super::snapshot::SnapshotTuning {
            page_size: Some(10),
            between_pages: Some(Arc::new(move |attempt| {
                let recorded = recorded.clone();
                let db_path = db_path.clone();
                let repo = repo.clone();
                Box::pin(async move {
                    if attempt != 1 {
                        return;
                    }
                    let other = crate::internal::db::establish_connection_with_busy_timeout(
                        db_path.to_str().expect("utf-8 path"),
                        std::time::Duration::from_millis(200),
                    )
                    .await
                    .expect("open a second connection");
                    let n = recorded.lock().unwrap().len();
                    let data = format!("pi-09 concurrent writer {n}\n");
                    let hash = ObjectHash::from_type_and_data(ObjectType::Blob, data.as_bytes());
                    let result = object_index::ActiveModel {
                        o_id: Set(hash.to_string()),
                        o_type: Set("blob".to_string()),
                        o_size: Set(data.len() as i64),
                        repo_id: Set(repo),
                        created_at: Set(0),
                        is_synced: Set(0),
                        ..Default::default()
                    }
                    .insert(&other)
                    .await
                    .map(|_| hash)
                    .map_err(|error| error.to_string());
                    let _ = other.close().await;
                    recorded.lock().unwrap().push(result);
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        let writes = writes.lock().unwrap().clone();
        assert_eq!(writes.len(), 20, "one write after each of twenty pages");
        for write in &writes {
            write
                .as_ref()
                .unwrap_or_else(|error| panic!("a concurrent writer was blocked: {error}"));
        }
        assert_eq!(
            report.total_unsynced, 220,
            "the retried freeze is one state that includes the committed rows"
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_ref_pages_do_not_block_other_writers() {
        use crate::internal::model::reference::{self, ConfigKind};

        let _repo = enter_isolated_libra_repo().await;
        for n in 0..30 {
            insert_branch_ref(&format!("refs/heads/pi-09-ref-{n:02}")).await;
        }
        let db_path = util::storage_path().join(util::DATABASE);
        let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = writes.clone();
        // Refs are read a page per statement too: another connection commits
        // a new ref after every ref page of the first attempt with a 200 ms
        // busy timeout, and every write lands at once. The retried freeze
        // then uploads one consistent ref set that includes them.
        let tuning = super::snapshot::SnapshotTuning {
            page_size: Some(10),
            between_ref_pages: Some(Arc::new(move |attempt| {
                let recorded = recorded.clone();
                let db_path = db_path.clone();
                Box::pin(async move {
                    if attempt != 1 {
                        return;
                    }
                    let other = crate::internal::db::establish_connection_with_busy_timeout(
                        db_path.to_str().expect("utf-8 path"),
                        std::time::Duration::from_millis(200),
                    )
                    .await
                    .expect("open a second connection");
                    let name = format!(
                        "refs/heads/pi-09-concurrent-{}",
                        recorded.lock().unwrap().len()
                    );
                    let result = reference::ActiveModel {
                        name: Set(Some(name.clone())),
                        kind: Set(ConfigKind::Branch),
                        commit: Set(Some("b".repeat(40))),
                        remote: Set(None),
                        worktree_id: Set(None),
                        ..Default::default()
                    }
                    .insert(&other)
                    .await
                    .map(|_| name)
                    .map_err(|error| error.to_string());
                    let _ = other.close().await;
                    recorded.lock().unwrap().push(result);
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        sync_with_tuning(&mock, &r2, &tuning).await.expect("sync");
        let writes = writes.lock().unwrap().clone();
        assert!(
            writes.len() >= 3,
            "thirty refs at ten per page give at least three ref pages: {writes:?}"
        );
        for write in &writes {
            write
                .as_ref()
                .unwrap_or_else(|error| panic!("a concurrent writer was blocked: {error}"));
        }
        // The uploaded projection is exactly the ref table as the retried
        // freeze saw it (nothing was written after the first attempt): every
        // row once, none skipped between pages.
        let metadata: Vec<serde_json::Value> = serde_json::from_slice(
            &RemoteStorage::new(r2.clone())
                .get_metadata()
                .await
                .expect("read metadata"),
        )
        .expect("metadata is a JSON array");
        let mut uploaded: Vec<i64> = metadata
            .iter()
            .map(|row| row["id"].as_i64().expect("ref id"))
            .collect();
        uploaded.sort_unstable();
        let mut local: Vec<i64> = reference::Entity::find()
            .all(&db::get_db_conn_instance().await)
            .await
            .expect("read local refs")
            .into_iter()
            .map(|row| row.id)
            .collect();
        local.sort_unstable();
        assert_eq!(uploaded, local, "every ref uploaded exactly once");
        let names: Vec<&str> = metadata
            .iter()
            .filter_map(|row| row["name"].as_str())
            .collect();
        for n in 0..30 {
            let name = format!("refs/heads/pi-09-ref-{n:02}");
            assert!(names.contains(&name.as_str()), "{name} missing: {names:?}");
        }
        for name in writes.iter().flatten() {
            assert!(names.contains(&name.as_str()), "{name} missing: {names:?}");
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_ref_pages_stop_when_refs_keep_changing() {
        let _repo = enter_isolated_libra_repo().await;
        index_local_object(b"pi-09 ref churn object\n").await;
        for n in 0..3 {
            insert_branch_ref(&format!("refs/heads/pi-09-churn-{n}")).await;
        }
        let inserted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = inserted.clone();
        // A new ref lands after every one-ref page of every attempt. Each
        // attempt reads only up to the ref boundary it started with, so it
        // ends; the changed database retries it until the run stops, still
        // before any remote write.
        let tuning = super::snapshot::SnapshotTuning {
            page_size: Some(1),
            between_ref_pages: Some(Arc::new(move |_attempt| {
                let counter = counter.clone();
                Box::pin(async move {
                    let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    insert_branch_ref(&format!("refs/heads/pi-09-churn-writer-{n}")).await;
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let error = sync_with_tuning(&mock, &r2, &tuning)
            .await
            .expect_err("refs that keep changing stop the run");
        let detail = error.to_string();
        assert!(detail.contains("kept changing"), "{detail}");
        assert!(
            detail.contains("nothing was written to D1 or R2"),
            "{detail}"
        );
        let pages = inserted.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            pages < 3 * 64,
            "every attempt ends at its ref boundary instead of chasing new refs: {pages} pages"
        );
        let probe = "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1";
        let probes: Vec<serde_json::Value> = ["agent_session", "agent_capture_session_v2"]
            .iter()
            .map(|table| serde_json::json!({ "sql": probe, "params": [table] }))
            .collect();
        assert_eq!(mock.request_bodies().await, probes);
        assert_eq!(r2_object_count(&r2).await, 0);
        assert!(spool_runs().is_empty());
    }

    async fn insert_ref_row(
        name: Option<&str>,
        kind: crate::internal::model::reference::ConfigKind,
        remote: Option<&str>,
        worktree_id: Option<&str>,
    ) -> i64 {
        use crate::internal::model::reference;

        reference::ActiveModel {
            name: Set(name.map(str::to_owned)),
            kind: Set(kind),
            commit: Set(Some("c".repeat(40))),
            remote: Set(remote.map(str::to_owned)),
            worktree_id: Set(worktree_id.map(str::to_owned)),
            ..Default::default()
        }
        .insert(&db::get_db_conn_instance().await)
        .await
        .expect("insert ref row")
        .id
    }

    /// The refs projection as the pre-PI-27 whole-collection path built it.
    async fn whole_reference_projection() -> Vec<u8> {
        use crate::internal::model::reference;

        let references = reference::Entity::find()
            .all(&db::get_db_conn_instance().await)
            .await
            .expect("read local refs");
        super::metadata::serialize_reference_projection(references)
            .expect("serialize the whole projection")
            .1
    }

    async fn uploaded_metadata(r2: &Arc<InMemory>) -> Vec<u8> {
        RemoteStorage::new(r2.clone())
            .get_metadata()
            .await
            .expect("read metadata")
    }

    /// The `snapshots` directory, created owner-only as PI-09 creates it.
    fn private_snapshots_dir() -> PathBuf {
        let root = util::storage_path().join(super::snapshot::SPOOL_ROOT_DIR);
        let snapshots = root.join(super::snapshot::SPOOL_SNAPSHOTS_DIR);
        for dir in [&root, &snapshots] {
            if !dir.exists() {
                super::snapshot::create_private_dir(dir).expect("create a spool parent");
            }
        }
        snapshots
    }

    /// An owner marker as this process would write it, for `pid`.
    fn marker_for(pid: u32) -> super::snapshot::OwnerMarker {
        super::snapshot::OwnerMarker {
            tool: super::snapshot::OWNER_MARKER_TOOL.to_string(),
            pid,
            pid_namespace: super::snapshot::pid_namespace(),
            boot_id: super::snapshot::boot_id(),
            host: super::snapshot::host_name(),
            started_at_unix_ms: 0,
            libra_version: "test".to_string(),
        }
    }

    /// Plant a leftover run directory as PI-09 leaves one: an owner-only
    /// directory with `marker` as its owner-only marker when given.
    fn plant_residue(name: &str, marker: Option<&[u8]>) -> PathBuf {
        let run = private_snapshots_dir().join(name);
        super::snapshot::create_private_dir(&run).expect("create a run directory");
        if let Some(bytes) = marker {
            let mut file = super::snapshot::create_private_file(&run.join("owner.json"))
                .expect("create the owner marker");
            std::io::Write::write_all(&mut file, bytes).expect("write the owner marker");
        }
        run
    }

    fn marker_bytes(marker: &super::snapshot::OwnerMarker) -> Vec<u8> {
        serde_json::to_vec(marker).expect("serialize a marker")
    }

    /// A pid whose process has exited (and been reaped).
    #[cfg(unix)]
    fn exited_pid() -> u32 {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn a short-lived process");
        let pid = child.id();
        child.wait().expect("reap it");
        pid
    }

    fn note_for<'a>(notes: &'a [String], run: &std::path::Path) -> &'a str {
        let path = run.display().to_string();
        notes
            .iter()
            .find(|note| note.contains(&path))
            .unwrap_or_else(|| panic!("no note names {path}: {notes:#?}"))
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_spool_owner_marker_is_valid_before_any_snapshot_data() {
        let _repo = enter_isolated_libra_repo().await;
        // On Windows the host comes from the operating system, not from a
        // rewritten COMPUTERNAME.
        #[cfg(windows)]
        let _computer = ScopedEnvVar::set("COMPUTERNAME", "not-this-host");
        let storage = util::storage_path();
        let raw_root = util::working_dir().join(util::ROOT_DIR);
        let tuning = super::snapshot::SnapshotTuning::default();
        let before = chrono::Utc::now().timestamp_millis();
        let spool = super::snapshot::SnapshotSpool::create(
            &storage,
            &raw_root,
            &tuning,
            super::snapshot::SpoolWarningSink::Stderr,
        )
        .expect("create a spool");
        let after = chrono::Utc::now().timestamp_millis();
        // The marker is the only thing in the run directory before any
        // snapshot data is written.
        let names: Vec<String> = fs::read_dir(spool.dir())
            .expect("list the run directory")
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["owner.json".to_string()]);
        let marker: super::snapshot::OwnerMarker =
            serde_json::from_slice(&fs::read(spool.dir().join("owner.json")).unwrap())
                .expect("a valid marker");
        assert_eq!(marker.tool, super::snapshot::OWNER_MARKER_TOOL);
        assert_eq!(marker.pid, std::process::id());
        assert_eq!(marker.pid_namespace, super::snapshot::pid_namespace());
        assert_eq!(marker.boot_id, super::snapshot::boot_id());
        assert!(!marker.host.is_empty());
        assert_eq!(marker.host, super::snapshot::host_name());
        assert_ne!(marker.host, "not-this-host");
        assert!((before..=after).contains(&marker.started_at_unix_ms));
        assert_eq!(marker.libra_version, env!("CARGO_PKG_VERSION"));
        assert!(spool.finish().is_none());

        // A sync in flight carries the same marker.
        index_local_object(b"pi-25 marker in flight\n").await;
        let seen = Arc::new(std::sync::Mutex::new(None));
        let recorded = seen.clone();
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(Arc::new(move |dir| {
                let recorded = recorded.clone();
                Box::pin(async move {
                    *recorded.lock().unwrap() = Some(fs::read(dir.join("owner.json")).unwrap());
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        let bytes = seen.lock().unwrap().clone().expect("marker read mid-run");
        let in_flight: super::snapshot::OwnerMarker = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(in_flight.pid, std::process::id());
        assert_eq!(in_flight.host, super::snapshot::host_name());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_spool_owner_removes_only_runs_whose_owner_exited() {
        let _repo = enter_isolated_libra_repo().await;
        let gone = plant_residue("run-gone", Some(&marker_bytes(&marker_for(exited_pid()))));
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &Default::default())
            .await
            .expect("sync");
        assert!(!gone.exists(), "a run whose owner exited is removed");
        assert!(
            report.spool_warnings.is_empty(),
            "{:#?}",
            report.spool_warnings
        );
        assert!(spool_runs().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_spool_owner_keeps_runs_it_cannot_prove_exited() {
        let _repo = enter_isolated_libra_repo().await;
        let mut alive = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn a live process");
        let exited = exited_pid();
        let with = |change: &dyn Fn(&mut super::snapshot::OwnerMarker)| {
            let mut marker = marker_for(exited);
            change(&mut marker);
            marker_bytes(&marker)
        };
        #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
        let mut cases: Vec<(PathBuf, &str)> = vec![
            (plant_residue("run-missing", None), "marker is missing"),
            (plant_residue("run-garbage", Some(b"not json")), "not valid"),
            (
                plant_residue("run-tool", Some(&with(&|m| m.tool = "other-tool".into()))),
                "another tool",
            ),
            (
                plant_residue("run-host", Some(&with(&|m| m.host = "another-host".into()))),
                "another host",
            ),
            (
                plant_residue(
                    "run-boot",
                    Some(&with(&|m| m.boot_id = Some("another-boot".into()))),
                ),
                "another boot",
            ),
            (
                plant_residue("run-no-boot", Some(&with(&|m| m.boot_id = None))),
                "cannot be identified",
            ),
            (
                plant_residue(
                    "run-namespace",
                    Some(&with(&|m| m.pid_namespace = Some("pid:[1]".into()))),
                ),
                "namespace",
            ),
            (
                plant_residue("run-alive", Some(&with(&|m| m.pid = alive.id()))),
                "still running",
            ),
            (
                plant_residue("run-self", Some(&with(&|m| m.pid = std::process::id()))),
                "this very process",
            ),
            // pid 1 is never ours: either it cannot be signalled
            // (permission denied) or, as root, it is running.
            (
                plant_residue("run-init", Some(&with(&|m| m.pid = 1))),
                "process 1",
            ),
        ];
        #[cfg(target_os = "linux")]
        cases.push((
            plant_residue("run-no-namespace", Some(&with(&|m| m.pid_namespace = None))),
            "cannot be identified",
        ));
        // Cleanup reads only the marker: a FIFO elsewhere in a run would
        // block any reader.
        let fifo = cases[7].0.join("objects.jsonl");
        let fifo_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path; mkfifo only creates the node.
        assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);

        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &Default::default())
            .await
            .expect("sync");
        let _ = alive.kill();
        let _ = alive.wait();
        for (run, reason) in &cases {
            assert!(run.exists(), "{} was kept", run.display());
            let note = note_for(&report.spool_warnings, run);
            assert!(note.contains(reason), "{reason}: {note}");
            assert!(note.contains("kept the leftover"), "{note}");
            assert!(note.contains("confirmed that run has stopped"), "{note}");
        }
        assert_eq!(
            report.spool_warnings.len(),
            cases.len(),
            "{:#?}",
            report.spool_warnings
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_spool_owner_keeps_every_run_on_windows() {
        // Windows cannot prove that a run's owner exited, so even a marker
        // of this host is kept and reported.
        let _repo = enter_isolated_libra_repo().await;
        let run = plant_residue("run-windows", Some(&marker_bytes(&marker_for(1))));
        let missing = plant_residue("run-missing", None);
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &Default::default())
            .await
            .expect("sync");
        for kept in [&run, &missing] {
            assert!(kept.exists());
            let note = note_for(&report.spool_warnings, kept);
            assert!(note.contains("kept the leftover"), "{note}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_spool_owner_reports_every_kept_run() {
        // Every kept leftover is reported in every output mode: a cross-boot
        // marker, a missing marker, a live owner, and an exited owner whose
        // directory cannot be removed; a sync that then fails carries them
        // in its error. The sync's own outcome is unchanged.
        for (label, mode) in report_modes() {
            let _repo = enter_isolated_libra_repo().await;
            let mut alive = std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .expect("spawn a live process");
            let mut cross_boot = marker_for(exited_pid());
            cross_boot.boot_id = Some("another-boot".into());
            let runs = [
                plant_residue("run-boot", Some(&marker_bytes(&cross_boot))),
                plant_residue("run-missing", None),
                plant_residue("run-alive", Some(&marker_bytes(&marker_for(alive.id())))),
                plant_residue("run-stuck", Some(&marker_bytes(&marker_for(exited_pid())))),
            ];
            let tuning = super::snapshot::SnapshotTuning {
                fail_residue_removal: true,
                ..Default::default()
            };
            index_local_object(format!("pi-25 report {label}\n").as_bytes()).await;
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            let (concluded, _console, ndjson) =
                drive_sync_in_mode(&mode, &mock, Arc::new(InMemory::new()), &tuning).await;
            let structured = concluded
                .unwrap_or_else(|error| panic!("{label}: the sync still succeeds: {error}"));
            let notes: Vec<String> = runs
                .iter()
                .map(|run| {
                    let path = run.display().to_string();
                    let pending = crate::utils::output::pending_warning_messages();
                    let from_ndjson: Vec<String> = ndjson
                        .iter()
                        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                        .filter(|event| event["event"] == "cloud_sync.spool.warning")
                        .filter_map(|event| event["message"].as_str().map(str::to_string))
                        .collect();
                    match label {
                        "human" | "quiet" => pending
                            .into_iter()
                            .find(|note| note.contains(&path))
                            .unwrap_or_else(|| panic!("{label}: {path} not warned")),
                        _ => from_ndjson
                            .into_iter()
                            .find(|note| note.contains(&path))
                            .unwrap_or_else(|| panic!("{label}: {path} not an NDJSON warning")),
                    }
                })
                .collect();
            assert!(notes[3].contains("could not remove"), "{}", notes[3]);
            assert!(
                notes[3].contains("delete that directory by hand"),
                "{}",
                notes[3]
            );
            if label == "json" {
                let data = serde_json::to_value(structured.expect("--json data")).unwrap();
                let warnings = data["warnings"].as_array().expect("data.warnings");
                for note in &notes {
                    assert!(
                        warnings.iter().any(|w| w == note.as_str()),
                        "{label}: {note}"
                    );
                }
            }
            for run in &runs {
                assert!(run.exists(), "{label}: {} kept", run.display());
            }

            // The same leftovers, then a failing sync: its error names them.
            index_local_object(format!("pi-25 failing {label}\n").as_bytes()).await;
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            mock.fail_once_when_sql_contains(
                "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced, object_format)",
                "injected upload failure",
            );
            let (concluded, _, _) =
                drive_sync_in_mode(&mode, &mock, Arc::new(InMemory::new()), &tuning).await;
            let error = concluded.expect_err("the failing sync fails");
            let detail = error.to_string();
            for run in &runs {
                assert!(
                    detail.contains(&run.display().to_string()),
                    "{label}: the error names {}: {detail}",
                    run.display()
                );
            }
            let _ = alive.kill();
            let _ = alive.wait();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_spool_owner_never_follows_foreign_paths() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let outside = tempdir().unwrap();
        let exited = marker_bytes(&marker_for(exited_pid()));
        let outside_run = outside.path().join("run-outside");
        fs::create_dir(&outside_run).unwrap();
        fs::write(outside_run.join("owner.json"), &exited).unwrap();
        let outside_file = outside.path().join("precious");
        fs::write(&outside_file, b"keep me").unwrap();

        // A symlinked or group-writable spool parent: nothing is enumerated,
        // and the sync, which refuses that parent too, names it.
        for case in ["linked snapshots", "group-writable cloud-sync"] {
            let _repo = enter_isolated_libra_repo().await;
            let root = util::storage_path().join(super::snapshot::SPOOL_ROOT_DIR);
            super::snapshot::create_private_dir(&root).unwrap();
            let reason = if case == "linked snapshots" {
                symlink(
                    outside.path(),
                    root.join(super::snapshot::SPOOL_SNAPSHOTS_DIR),
                )
                .unwrap();
                "symbolic link"
            } else {
                fs::set_permissions(&root, fs::Permissions::from_mode(0o770)).unwrap();
                "accessible to group or others"
            };
            let notes = super::snapshot::clean_residues(
                &util::storage_path(),
                &util::working_dir().join(util::ROOT_DIR),
                &Default::default(),
            );
            assert_eq!(notes.len(), 1, "{case}: {notes:#?}");
            assert!(notes[0].contains("did not look"), "{case}: {}", notes[0]);
            assert!(notes[0].contains(reason), "{case}: {}", notes[0]);
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            let error = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &Default::default())
                .await
                .expect_err("an unsafe spool parent stops the sync");
            assert!(
                error.to_string().contains("did not look"),
                "{case}: {error}"
            );
            assert!(outside_run.join("owner.json").exists(), "{case}");
        }

        // Unsafe candidates are reported as not inspected and left alone; a
        // link inside a removable run is removed without touching its target.
        let _repo = enter_isolated_libra_repo().await;
        let snapshots = private_snapshots_dir();
        let linked_run = snapshots.join("run-linked");
        symlink(&outside_run, &linked_run).unwrap();
        let open_run = plant_residue("run-open", Some(&exited));
        fs::set_permissions(&open_run, fs::Permissions::from_mode(0o770)).unwrap();
        let forged = plant_residue("run-forged", Some(&exited));
        fs::set_permissions(forged.join("owner.json"), fs::Permissions::from_mode(0o644)).unwrap();
        let linked_marker = plant_residue("run-linked-marker", None);
        symlink(
            outside_run.join("owner.json"),
            linked_marker.join("owner.json"),
        )
        .unwrap();
        let removable = plant_residue("run-removable", Some(&exited));
        symlink(&outside_file, removable.join("objects.jsonl")).unwrap();

        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &Default::default())
            .await
            .expect("sync");
        for (run, reason) in [
            (&linked_run, "symbolic link"),
            (&open_run, "accessible to group or others"),
            (&forged, "could have been written by someone else"),
            (&linked_marker, "symbolic link"),
        ] {
            assert!(
                fs::symlink_metadata(run).is_ok(),
                "{} left alone",
                run.display()
            );
            let note = note_for(&report.spool_warnings, run);
            assert!(note.contains("did not inspect"), "{note}");
            assert!(note.contains(reason), "{reason}: {note}");
            assert!(note.contains("nothing in it was read or deleted"), "{note}");
        }
        assert!(!removable.exists(), "the exited run is removed");
        assert_eq!(fs::read(&outside_file).unwrap(), b"keep me");
        assert!(outside_run.join("owner.json").exists());
    }

    #[cfg(windows)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_spool_owner_never_follows_foreign_paths_on_windows() {
        let reset = |path: &std::path::Path| {
            let status = std::process::Command::new("icacls")
                .arg(path)
                .arg("/reset")
                .status()
                .expect("run icacls");
            assert!(status.success());
        };
        // An inherited ACL on `snapshots`: nothing is enumerated.
        let _repo = enter_isolated_libra_repo().await;
        let snapshots = private_snapshots_dir();
        reset(&snapshots);
        let notes = super::snapshot::clean_residues(
            &util::storage_path(),
            &util::working_dir().join(util::ROOT_DIR),
            &Default::default(),
        );
        assert_eq!(notes.len(), 1, "{notes:#?}");
        assert!(notes[0].contains("did not look"), "{}", notes[0]);
        assert!(notes[0].contains("icacls"), "{}", notes[0]);

        // An inherited ACL on a run directory or on its marker: the run is
        // reported as not inspected and left alone.
        let _repo = enter_isolated_libra_repo().await;
        let open_run = plant_residue("run-open", Some(&marker_bytes(&marker_for(1))));
        reset(&open_run);
        let open_marker = plant_residue("run-open-marker", Some(&marker_bytes(&marker_for(1))));
        reset(&open_marker.join("owner.json"));
        let notes = super::snapshot::clean_residues(
            &util::storage_path(),
            &util::working_dir().join(util::ROOT_DIR),
            &Default::default(),
        );
        for run in [&open_run, &open_marker] {
            assert!(run.exists());
            let note = note_for(&notes, run);
            assert!(note.contains("did not inspect"), "{note}");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_spool_owner_icacls_names_the_token_sid_on_windows() {
        // The remedy names the process token's user SID, whatever USERNAME
        // says.
        let _repo = enter_isolated_libra_repo().await;
        let _user = ScopedEnvVar::set("USERNAME", "someone-else");
        let status = std::process::Command::new("icacls")
            .arg(util::storage_path())
            .arg("/reset")
            .status()
            .expect("run icacls");
        assert!(status.success());
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let error = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &Default::default())
            .await
            .expect_err("an inherited .libra is refused");
        let detail = error.to_string();
        let sid = crate::utils::windows_fs::current_user_sid_string().expect("token SID");
        assert!(detail.contains(&format!("*{sid}")), "{detail}");
        assert!(!detail.contains("someone-else"), "{detail}");
        assert!(
            detail.contains(&util::storage_path().display().to_string()),
            "{detail}"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn capture_spool_owner_exit_proof_is_fail_closed() {
        use super::snapshot::{OwnerMarker, ProcessIdentity, owner_exited};

        fn gone(_: i32) -> std::io::Result<()> {
            Err(std::io::Error::from_raw_os_error(libc::ESRCH))
        }
        fn denied(_: i32) -> std::io::Result<()> {
            Err(std::io::Error::from_raw_os_error(libc::EPERM))
        }
        fn alive(_: i32) -> std::io::Result<()> {
            Ok(())
        }
        fn unexpected(pid: i32) -> std::io::Result<()> {
            panic!("signal 0 must not be sent for pid {pid}")
        }
        let me = ProcessIdentity {
            host: "this-host".to_string(),
            boot_id: Some("boot-1".to_string()),
            pid_namespace: cfg!(target_os = "linux").then(|| "pid:[4026531836]".to_string()),
            pid: 4242,
        };
        let marker = |change: &dyn Fn(&mut OwnerMarker)| {
            let mut marker = OwnerMarker {
                tool: super::snapshot::OWNER_MARKER_TOOL.to_string(),
                pid: 77,
                pid_namespace: me.pid_namespace.clone(),
                boot_id: me.boot_id.clone(),
                host: me.host.clone(),
                started_at_unix_ms: 0,
                libra_version: "test".to_string(),
            };
            change(&mut marker);
            marker
        };
        // Only a matching marker whose pid reports "no such process" is gone.
        assert_eq!(owner_exited(&marker(&|_| {}), &me, gone), Ok(()));
        let refused: Vec<(
            OwnerMarker,
            ProcessIdentity,
            super::snapshot::SignalProbe,
            &str,
        )> = vec![
            (marker(&|_| {}), me.clone(), denied, "permission denied"),
            (marker(&|_| {}), me.clone(), alive, "still running"),
            (
                marker(&|m| m.pid = 0),
                me.clone(),
                unexpected,
                "names no single process",
            ),
            (
                marker(&|m| m.pid = u32::MAX),
                me.clone(),
                unexpected,
                "names no single process",
            ),
            (
                marker(&|m| m.pid = 4242),
                me.clone(),
                unexpected,
                "this very process",
            ),
            (
                marker(&|m| m.tool = "other".into()),
                me.clone(),
                unexpected,
                "another tool",
            ),
            (
                marker(&|m| m.host = "other".into()),
                me.clone(),
                unexpected,
                "another host",
            ),
            (
                marker(&|m| m.boot_id = None),
                me.clone(),
                unexpected,
                "cannot be identified",
            ),
            (
                marker(&|m| m.boot_id = Some(String::new())),
                me.clone(),
                unexpected,
                "cannot be identified",
            ),
            (
                marker(&|m| m.boot_id = Some("boot-2".into())),
                me.clone(),
                unexpected,
                "another boot",
            ),
            (
                marker(&|_| {}),
                ProcessIdentity {
                    boot_id: None,
                    ..me.clone()
                },
                unexpected,
                "cannot be identified",
            ),
            (
                marker(&|_| {}),
                ProcessIdentity {
                    host: String::new(),
                    ..me.clone()
                },
                unexpected,
                "another host",
            ),
        ];
        #[cfg(target_os = "linux")]
        let refused = {
            let mut refused = refused;
            refused.push((
                marker(&|_| {}),
                ProcessIdentity {
                    pid_namespace: None,
                    ..me.clone()
                },
                unexpected,
                "cannot be identified",
            ));
            refused.push((
                marker(&|m| m.pid_namespace = None),
                me.clone(),
                unexpected,
                "cannot be identified",
            ));
            refused.push((
                marker(&|m| m.pid_namespace = Some("pid:[1]".into())),
                me.clone(),
                unexpected,
                "another PID namespace",
            ));
            refused
        };
        #[cfg(target_os = "macos")]
        let refused = {
            let mut refused = refused;
            refused.push((
                marker(&|m| m.pid_namespace = Some("pid:[1]".into())),
                me.clone(),
                unexpected,
                "PID namespace",
            ));
            refused
        };
        for (marker, me, probe, reason) in refused {
            let outcome = owner_exited(&marker, &me, probe);
            let error = outcome.expect_err(reason);
            assert!(error.contains(reason), "{reason}: {error}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_spool_owner_cleanup_follows_no_linked_libra_and_reports_partial_listing() {
        // A `.libra` that is itself a link (repository discovery resolves
        // it): nothing under it is enumerated or removed.
        let _repo = enter_isolated_libra_repo().await;
        let removable = plant_residue(
            "run-removable",
            Some(&marker_bytes(&marker_for(exited_pid()))),
        );
        let workdir = util::working_dir();
        let raw_root = workdir.join(util::ROOT_DIR);
        let moved = workdir.join(".libra-real");
        fs::rename(&raw_root, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &raw_root).unwrap();
        let notes =
            super::snapshot::clean_residues(&util::storage_path(), &raw_root, &Default::default());
        fs::remove_file(&raw_root).unwrap();
        fs::rename(&moved, &raw_root).unwrap();
        assert_eq!(notes.len(), 1, "{notes:#?}");
        assert!(notes[0].contains("did not look"), "{}", notes[0]);
        assert!(notes[0].contains("symbolic link"), "{}", notes[0]);
        assert!(
            removable.exists(),
            "nothing under a linked .libra is removed"
        );

        // A listing that fails part-way is reported; what was listed is
        // still judged.
        let tuning = super::snapshot::SnapshotTuning {
            fail_residue_listing: true,
            ..Default::default()
        };
        let notes = super::snapshot::clean_residues(&util::storage_path(), &raw_root, &tuning);
        assert!(
            notes
                .iter()
                .any(|note| note.contains("could not list every leftover")),
            "{notes:#?}"
        );
        assert!(
            !removable.exists(),
            "the listed exited run is still removed"
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_spool_owner_notes_survive_an_early_failure() {
        // The cleanup runs at the start of every sync: a sync that then
        // fails before its snapshot — on missing cloud configuration, or on
        // the read-only D1 probe — still names each leftover it kept.
        let _repo = enter_isolated_libra_repo().await;
        let kept = plant_residue("run-kept", None);
        let path = kept.display().to_string();

        let _cleared: Vec<ClearedEnvVarGuard> = [
            "LIBRA_D1_ACCOUNT_ID",
            "LIBRA_D1_API_TOKEN",
            "LIBRA_D1_DATABASE_ID",
            "LIBRA_STORAGE_ENDPOINT",
            "LIBRA_STORAGE_BUCKET",
            "LIBRA_STORAGE_ACCESS_KEY",
            "LIBRA_STORAGE_SECRET_KEY",
        ]
        .into_iter()
        .map(ClearedEnvVarGuard::new)
        .collect();
        let error = super::sync::run_cloud_sync(
            CloudSyncContext {
                batch_size: 2,
                force: false,
            },
            &SilentCloudSyncProgress,
        )
        .await
        .expect_err("missing cloud configuration fails the sync");
        assert!(matches!(error, CloudError::MissingEnv { .. }), "{error:?}");
        assert!(error.to_string().contains(&path), "{error}");
        // The CLI error keeps its code and shows the note.
        let cli = cloud_cli_error_typed("sync", error);
        assert_eq!(cli.stable_code(), StableErrorCode::AuthMissingCredentials);
        // Parsed, since the envelope escapes a Windows path's backslashes.
        let envelope: serde_json::Value =
            serde_json::from_str(&cli.render_json()).expect("error envelope");
        let hints = envelope["hints"].as_array().expect("hints");
        assert!(
            hints
                .iter()
                .any(|hint| hint.as_str().is_some_and(|hint| hint.contains(&path))),
            "{envelope}"
        );
        assert!(kept.exists());

        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        mock.fail_once_when_sql_contains("sqlite_master", "injected probe failure");
        let error = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &Default::default())
            .await
            .expect_err("a failed probe fails the sync");
        assert!(error.to_string().contains(&path), "{error}");
    }

    /// An in-memory R2 that rejects uploads of objects or of `metadata.json`
    /// and counts each rejection, so a test can see the fault fired (PI-28).
    #[derive(Debug)]
    struct FailingR2 {
        inner: Arc<InMemory>,
        fail_objects: bool,
        fail_metadata: bool,
        rejected: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FailingR2 {
        fn rejects(&self, location: &object_store::path::Path) -> bool {
            let metadata = location.as_ref().ends_with("metadata.json");
            let rejected = if metadata {
                self.fail_metadata
            } else {
                self.fail_objects
            };
            if rejected {
                self.rejected
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            rejected
        }
    }

    impl std::fmt::Display for FailingR2 {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("FailingR2")
        }
    }

    fn injected_r2_failure() -> object_store::Error {
        object_store::Error::Generic {
            store: "failing-r2",
            source: "injected R2 upload failure".into(),
        }
    }

    #[async_trait::async_trait]
    impl object_store::ObjectStore for FailingR2 {
        async fn put_opts(
            &self,
            location: &object_store::path::Path,
            payload: object_store::PutPayload,
            opts: object_store::PutOptions,
        ) -> object_store::Result<object_store::PutResult> {
            if self.rejects(location) {
                return Err(injected_r2_failure());
            }
            self.inner.put_opts(location, payload, opts).await
        }

        async fn put_multipart_opts(
            &self,
            location: &object_store::path::Path,
            opts: object_store::PutMultipartOptions,
        ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
            if self.rejects(location) {
                return Err(injected_r2_failure());
            }
            self.inner.put_multipart_opts(location, opts).await
        }

        async fn get_opts(
            &self,
            location: &object_store::path::Path,
            options: object_store::GetOptions,
        ) -> object_store::Result<object_store::GetResult> {
            self.inner.get_opts(location, options).await
        }

        fn list(
            &self,
            prefix: Option<&object_store::path::Path>,
        ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
        {
            self.inner.list(prefix)
        }

        async fn list_with_delimiter(
            &self,
            prefix: Option<&object_store::path::Path>,
        ) -> object_store::Result<object_store::ListResult> {
            self.inner.list_with_delimiter(prefix).await
        }

        async fn copy_opts(
            &self,
            from: &object_store::path::Path,
            to: &object_store::path::Path,
            options: object_store::CopyOptions,
        ) -> object_store::Result<()> {
            self.inner.copy_opts(from, to, options).await
        }

        fn delete_stream(
            &self,
            locations: futures::stream::BoxStream<
                'static,
                object_store::Result<object_store::path::Path>,
            >,
        ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::path::Path>>
        {
            self.inner.delete_stream(locations)
        }
    }

    /// The output configuration each mode resolves to from its command
    /// line, as `main` resolves it (`--json` alone also turns on NDJSON
    /// progress).
    fn report_modes() -> [(&'static str, OutputConfig); 4] {
        let resolve = |flags: &[&str]| {
            let argv: Vec<String> = std::iter::once("libra")
                .chain(flags.iter().copied())
                .chain(["cloud", "sync"])
                .map(str::to_string)
                .collect();
            OutputConfig::resolve_from_argv(&argv)
        };
        [
            ("human", resolve(&[])),
            ("quiet", resolve(&["--quiet"])),
            ("json", resolve(&["--json"])),
            ("progress-json", resolve(&["--progress=json"])),
        ]
    }

    /// Run a sync from the snapshot as `execute_safe` does in `mode`
    /// (renderer from `sync_progress_for`, decision from `conclude_sync`),
    /// capturing human and NDJSON progress.
    async fn drive_sync_in_mode(
        mode: &OutputConfig,
        mock: &crate::utils::d1_client::test_support::MockD1,
        store: Arc<dyn object_store::ObjectStore>,
        tuning: &super::snapshot::SnapshotTuning,
    ) -> (CliResult<Option<CloudSyncOutput>>, Vec<String>, Vec<String>) {
        *CONSOLE_PROGRESS_CAPTURE.lock().unwrap() = Some(Vec::new());
        *JSON_PROGRESS_CAPTURE.lock().unwrap() = Some(Vec::new());
        let progress = sync_progress_for(mode);
        let outcome = super::sync::run_cloud_sync_from_snapshot(
            CloudSyncContext {
                batch_size: 2,
                force: false,
            },
            progress,
            mock.client(),
            Some(RemoteStorage::new(store)),
            tuning,
        )
        .await;
        let concluded = conclude_sync(outcome, progress, mode);
        let console = CONSOLE_PROGRESS_CAPTURE
            .lock()
            .unwrap()
            .take()
            .unwrap_or_default();
        let ndjson = JSON_PROGRESS_CAPTURE
            .lock()
            .unwrap()
            .take()
            .unwrap_or_default();
        (concluded, console, ndjson)
    }

    /// Whether `text` announces success: a whole word "complete",
    /// "completed", "success", "succeeded" or "successful(ly)" ("incomplete"
    /// is not one).
    fn claims_success(text: &str) -> bool {
        text.to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| {
                matches!(
                    word,
                    "complete"
                        | "completed"
                        | "success"
                        | "succeeded"
                        | "successful"
                        | "successfully"
                )
            })
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_report_never_claims_success_on_failure() {
        // Each failure — a failed object upload to R2 (and, separately, a
        // failed object index write to D1), a failed local status update, a
        // failed metadata upload and a failed agent-capture mirror — ends
        // the run with its stable error code in every output mode, and
        // nothing printed on the way announces success. The metadata and
        // mirror failures also run with nothing to upload, through the
        // sync's no-objects branch. Every case runs in a fresh repository.
        let failures = [
            (
                "object upload",
                StableErrorCode::ConflictOperationBlocked,
                true,
            ),
            (
                "object index write",
                StableErrorCode::ConflictOperationBlocked,
                true,
            ),
            (
                "status update",
                StableErrorCode::ConflictOperationBlocked,
                true,
            ),
            ("metadata upload", StableErrorCode::NetworkUnavailable, true),
            (
                "agent capture mirror",
                StableErrorCode::ConflictOperationBlocked,
                true,
            ),
            (
                "metadata upload",
                StableErrorCode::NetworkUnavailable,
                false,
            ),
            (
                "agent capture mirror",
                StableErrorCode::ConflictOperationBlocked,
                false,
            ),
        ];
        for (label, mode) in report_modes() {
            for (failure, code, with_objects) in failures {
                let _repo = enter_isolated_libra_repo().await;
                let case = if with_objects {
                    index_local_object(format!("pi-28 {label} {failure}\n").as_bytes()).await;
                    failure.to_string()
                } else {
                    format!("{failure} (no objects)")
                };
                insert_branch_ref("refs/heads/pi-28").await;
                let pending = {
                    use sea_orm::PaginatorTrait;
                    object_index::Entity::find()
                        .filter(object_index::Column::IsSynced.eq(0))
                        .count(&db::get_db_conn_instance().await)
                        .await
                        .expect("count pending objects")
                };
                assert_eq!(pending, u64::from(with_objects), "{label}/{case}");
                let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
                let mut tuning = super::snapshot::SnapshotTuning::default();
                let rejected = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let r2 = |fail_objects: bool,
                          fail_metadata: bool|
                 -> Arc<dyn object_store::ObjectStore> {
                    Arc::new(FailingR2 {
                        inner: Arc::new(InMemory::new()),
                        fail_objects,
                        fail_metadata,
                        rejected: rejected.clone(),
                    })
                };
                let mut store = r2(false, false);
                match failure {
                    "object upload" => store = r2(true, false),
                    "object index write" => mock.fail_once_when_sql_contains(
                        "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced, object_format)",
                        "injected upload failure",
                    ),
                    "status update" => tuning.fail_status_update = true,
                    "metadata upload" => store = r2(false, true),
                    _ => mock.fail_once_when_sql_contains(
                        "CREATE TABLE IF NOT EXISTS agent_session (",
                        "injected agent capture mirror failure",
                    ),
                }
                let (concluded, console, ndjson) =
                    drive_sync_in_mode(&mode, &mock, store, &tuning).await;
                if matches!(failure, "object upload" | "metadata upload") {
                    assert!(
                        rejected.load(std::sync::atomic::Ordering::SeqCst) > 0,
                        "{label}/{case}: the injected R2 failure fired"
                    );
                }
                let error = match concluded {
                    Ok(_) => panic!("{label}/{case}: a failed sync must not succeed"),
                    Err(error) => error,
                };
                assert_eq!(error.stable_code(), code, "{label}/{case}: {error}");
                // What --json prints instead of the cloud.sync data: the
                // error envelope with that code.
                let envelope: serde_json::Value =
                    serde_json::from_str(&error.render_json()).expect("error envelope");
                assert_eq!(envelope["ok"], false, "{label}/{case}: {envelope}");
                assert_eq!(
                    envelope["error_code"],
                    code.as_str(),
                    "{label}/{case}: {envelope}"
                );
                for line in &console {
                    assert!(
                        !claims_success(line),
                        "{label}/{case}: success claimed: {line}\n{console:#?}"
                    );
                }
                let mut events = Vec::new();
                for line in &ndjson {
                    let event: serde_json::Value = serde_json::from_str(line)
                        .unwrap_or_else(|e| panic!("{label}/{case}: not NDJSON ({e}): {line}"));
                    let name = event["event"].as_str().unwrap_or_default().to_string();
                    assert!(
                        !claims_success(&name),
                        "{label}/{case}: success event {name}\n{ndjson:#?}"
                    );
                    events.push(name);
                }
                match label {
                    "human" => {
                        assert!(!console.is_empty(), "{case}: human progress");
                        assert_eq!(
                            console.iter().any(|line| line == "No objects to sync."),
                            !with_objects,
                            "{case}: {console:#?}"
                        );
                    }
                    "quiet" => assert!(
                        console.is_empty() && ndjson.is_empty(),
                        "{case}: quiet prints no progress: {console:?} {ndjson:?}"
                    ),
                    // --json and --progress=json both stream NDJSON progress.
                    _ => {
                        assert!(console.is_empty(), "{label}/{case}: {console:?}");
                        assert_eq!(
                            events.iter().any(|name| name == "cloud_sync.objects.none"),
                            !with_objects,
                            "{label}/{case}: {events:?}"
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_report_announces_success_only_at_the_end() {
        // A successful sync says so once, after metadata and the
        // agent-capture mirror: the last human line is "Sync complete." and
        // the last NDJSON event is `cloud_sync.complete`; --json returns the
        // structured result and --quiet prints nothing.
        let _repo = enter_isolated_libra_repo().await;
        let mut run = 0usize;
        for (label, mode) in report_modes() {
            run += 1;
            index_local_object(format!("pi-28 success {run}\n").as_bytes()).await;
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            let (concluded, console, ndjson) =
                drive_sync_in_mode(&mode, &mock, Arc::new(InMemory::new()), &Default::default())
                    .await;
            let structured = concluded.unwrap_or_else(|e| panic!("{label}: {e}"));
            match label {
                "human" => {
                    assert_eq!(console.last().map(String::as_str), Some("Sync complete."));
                    let successes = console.iter().filter(|line| claims_success(line)).count();
                    assert_eq!(successes, 1, "{console:#?}");
                    assert!(
                        console
                            .iter()
                            .any(|line| line.starts_with("Objects: 1 synced")),
                        "{console:#?}"
                    );
                }
                "quiet" => {
                    assert!(console.is_empty() && ndjson.is_empty() && structured.is_none())
                }
                _ => {
                    // --json and --progress=json: the last NDJSON event is
                    // the only success event; --json also returns the data.
                    let last: serde_json::Value =
                        serde_json::from_str(ndjson.last().expect("events")).unwrap();
                    assert_eq!(last["event"], "cloud_sync.complete", "{label}: {ndjson:#?}");
                    let successes = ndjson
                        .iter()
                        .filter(|line| {
                            let event: serde_json::Value = serde_json::from_str(line).unwrap();
                            claims_success(event["event"].as_str().unwrap_or_default())
                        })
                        .count();
                    assert_eq!(successes, 1, "{label}: {ndjson:#?}");
                    assert_eq!(structured.is_some(), label == "json", "{label}");
                }
            }
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_paging_reads_objects_page_by_page() {
        let _repo = enter_isolated_libra_repo().await;
        let repo_id = ensure_repo_id().await;
        for n in 0..10 {
            index_local_object(format!("pi-27 page {n}\n").as_bytes()).await;
        }
        let storage = util::storage_path();
        let raw_root = util::working_dir().join(util::ROOT_DIR);
        // After every object page the spool file has grown by that page, and
        // no more than one page of identities is alive at a time.
        let sizes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = sizes.clone();
        let _ = super::snapshot::take_peak_pages(super::snapshot::PageKind::Objects);
        let tuning = super::snapshot::SnapshotTuning {
            page_size: Some(3),
            between_pages: Some(Arc::new(move |_attempt| {
                let recorded = recorded.clone();
                Box::pin(async move {
                    let runs = spool_runs();
                    let len = fs::metadata(runs[0].join("objects.jsonl"))
                        .expect("objects spool")
                        .len();
                    recorded.lock().unwrap().push(len);
                })
            })),
            ..Default::default()
        };
        let mut spool = super::snapshot::SnapshotSpool::create(
            &storage,
            &raw_root,
            &tuning,
            super::snapshot::SpoolWarningSink::Stderr,
        )
        .expect("create the spool");
        spool
            .freeze(&storage.join(util::DATABASE), &repo_id, false, &tuning)
            .await
            .expect("freeze");
        assert_eq!(
            super::snapshot::take_peak_pages(super::snapshot::PageKind::Objects),
            1,
            "one page of identities in memory at a time"
        );
        let sizes = sizes.lock().unwrap().clone();
        assert_eq!(sizes.len(), 4, "one hook call per object page: {sizes:?}");
        assert!(
            sizes.windows(2).all(|pair| pair[0] < pair[1]),
            "the spool grows page by page: {sizes:?}"
        );
        // Ten identities written three per page come back three per page;
        // nothing counts or caps the whole collection.
        let mut pages = spool.object_pages(3).expect("open the object spool");
        let mut sizes = Vec::new();
        while let Some(page) = pages.next_page().expect("read a page") {
            sizes.push(page.len());
        }
        assert_eq!(sizes, vec![3, 3, 3, 1]);
        assert!(spool.finish().is_none());
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_paging_syncs_far_beyond_one_page() {
        use sea_orm::PaginatorTrait;

        use crate::internal::model::object_index;

        let _repo = enter_isolated_libra_repo().await;
        let repo_id = ensure_repo_id().await;
        for n in 0..40 {
            index_local_object(format!("pi-27 large {n}\n").as_bytes()).await;
        }
        let tuning = super::snapshot::SnapshotTuning {
            page_size: Some(3),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        assert_eq!(
            (
                report.total_unsynced,
                report.synced_count,
                report.failed_count
            ),
            (40, 40, 0)
        );
        let unsynced = object_index::Entity::find()
            .filter(object_index::Column::RepoId.eq(repo_id.clone()))
            .filter(object_index::Column::IsSynced.eq(0))
            .count(&db::get_db_conn_instance().await)
            .await
            .expect("count unsynced");
        assert_eq!(unsynced, 0, "every frozen identity is marked");
        let rows = mock
            .client()
            .get_object_indexes(&repo_id)
            .await
            .expect("read D1 object index");
        assert_eq!(rows.len(), 40);
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_paging_streams_refs_one_page_at_a_time() {
        use sea_orm::PaginatorTrait;

        use crate::internal::model::reference;

        let _repo = enter_isolated_libra_repo().await;
        for n in 0..25 {
            insert_branch_ref(&format!("refs/heads/pi-27-stream-{n:02}")).await;
        }
        let total = reference::Entity::find()
            .count(&db::get_db_conn_instance().await)
            .await
            .expect("count refs") as usize;
        let sizes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = sizes.clone();
        let frozen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = frozen.clone();
        // After every projection page the spool file has grown by that page:
        // refs are written as they are read, not collected and written at the
        // end, and no more than one page of refs is alive at a time.
        let _ = super::snapshot::take_peak_pages(super::snapshot::PageKind::Refs);
        let tuning = super::snapshot::SnapshotTuning {
            page_size: Some(4),
            metadata_chunk_size: Some(256),
            between_projection_pages: Some(Arc::new(move |_attempt| {
                let recorded = recorded.clone();
                Box::pin(async move {
                    let runs = spool_runs();
                    let len = fs::metadata(runs[0].join("refs.json"))
                        .expect("refs spool")
                        .len();
                    recorded.lock().unwrap().push(len);
                })
            })),
            after_freeze: Some(Arc::new(move |dir| {
                let captured = captured.clone();
                Box::pin(async move {
                    *captured.lock().unwrap() =
                        fs::read(dir.join("refs.json")).expect("read refs spool");
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        sync_with_tuning(&mock, &r2, &tuning).await.expect("sync");
        assert_eq!(
            super::snapshot::take_peak_pages(super::snapshot::PageKind::Refs),
            1,
            "one page of refs in memory at a time"
        );
        let sizes = sizes.lock().unwrap().clone();
        assert_eq!(sizes.len(), total.div_ceil(4), "one hook call per ref page");
        assert!(
            sizes.windows(2).all(|pair| pair[0] < pair[1]),
            "the spool grows page by page: {sizes:?}"
        );
        let frozen = frozen.lock().unwrap().clone();
        assert!(frozen.len() > 256, "the upload spans several chunks");
        assert_eq!(
            uploaded_metadata(&r2).await,
            frozen,
            "the chunked upload is the spool file"
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_paging_metadata_matches_whole_serialization() {
        use crate::internal::model::reference::ConfigKind;

        let _repo = enter_isolated_libra_repo().await;
        // Names that order differently by bytes than by case or locale,
        // a remote/local split of one name, a tag, and two detached HEADs of
        // linked worktrees that tie on the sort key (inserted out of
        // worktree order, so only the id breaks the tie).
        for name in [
            "refs/heads/b",
            "refs/heads/a",
            "refs/heads/a/b",
            "refs/heads/B",
        ] {
            insert_branch_ref(name).await;
        }
        insert_branch_ref("refs/heads/\u{e9}t\u{e9}").await;
        insert_ref_row(Some("main"), ConfigKind::Branch, Some("upstream"), None).await;
        insert_ref_row(Some("main"), ConfigKind::Branch, Some("origin"), None).await;
        insert_ref_row(Some("v1.0"), ConfigKind::Tag, None, None).await;
        insert_ref_row(None, ConfigKind::Head, None, Some("wt-b")).await;
        insert_ref_row(None, ConfigKind::Head, None, Some("wt-a")).await;
        let whole = whole_reference_projection().await;

        let tuning = super::snapshot::SnapshotTuning {
            page_size: Some(2),
            metadata_chunk_size: Some(64),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let report = sync_with_tuning(&mock, &r2, &tuning).await.expect("sync");
        assert!(matches!(
            report.metadata,
            MetadataSyncOutcome::Synced { .. }
        ));
        assert_eq!(
            uploaded_metadata(&r2).await,
            whole,
            "the paged, streamed projection is byte-identical"
        );
        let stored = ConfigKv::get("cloud.metadata_hash")
            .await
            .expect("read the stored hash")
            .expect("hash stored")
            .value;
        assert_eq!(
            stored,
            super::metadata::calculate_metadata_hash(&whole).to_string(),
            "cloud.metadata_hash is the whole-buffer value"
        );

        // Unchanged refs hash the same through the streamed path: the next
        // sync skips the upload.
        let report = sync_with_tuning(&mock, &r2, &tuning)
            .await
            .expect("sync again");
        assert!(matches!(report.metadata, MetadataSyncOutcome::Skipped));
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_paging_rewrites_ref_pages_on_retry() {
        use crate::internal::model::reference;

        let _repo = enter_isolated_libra_repo().await;
        let mut first = None;
        for n in 0..6 {
            let id = insert_ref_row(
                Some(&format!("aaa-pi-27-retry-{n}")),
                reference::ConfigKind::Branch,
                None,
                None,
            )
            .await;
            first.get_or_insert(id);
        }
        let deleted = first.expect("a first ref");
        let renamed = deleted + 3;
        let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let once = changed.clone();
        // After the first projection page of the first attempt (already in
        // the spool) one of its refs is deleted, a new one added, and a ref
        // not yet written is renamed so it moves behind the page cursor; the
        // retry rewrites the file, so the upload holds no stale, missing or
        // duplicate ref.
        let tuning = super::snapshot::SnapshotTuning {
            page_size: Some(2),
            between_projection_pages: Some(Arc::new(move |attempt| {
                let once = once.clone();
                Box::pin(async move {
                    if attempt != 1 || once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    reference::Entity::delete_by_id(deleted)
                        .exec(&db::get_db_conn_instance().await)
                        .await
                        .expect("delete a frozen ref");
                    insert_branch_ref("aaa-pi-27-retry-added").await;
                    let mut moved: reference::ActiveModel = reference::Entity::find_by_id(renamed)
                        .one(&db::get_db_conn_instance().await)
                        .await
                        .expect("read a ref")
                        .expect("the ref exists")
                        .into();
                    moved.name = Set(Some("aaa-pi-27-retry-0-renamed".to_string()));
                    moved
                        .update(&db::get_db_conn_instance().await)
                        .await
                        .expect("rename a ref");
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        sync_with_tuning(&mock, &r2, &tuning).await.expect("sync");
        assert!(changed.load(std::sync::atomic::Ordering::SeqCst));
        let uploaded = uploaded_metadata(&r2).await;
        assert_eq!(
            uploaded,
            whole_reference_projection().await,
            "the retried projection is the current ref table, once"
        );
        let uploaded = String::from_utf8(uploaded).expect("utf-8 metadata");
        assert!(
            uploaded.contains("\"aaa-pi-27-retry-0-renamed\""),
            "{uploaded}"
        );
        assert!(!uploaded.contains("\"aaa-pi-27-retry-3\""), "{uploaded}");

        // Refs that change during every attempt stop the run before any
        // remote write.
        let churn = super::snapshot::SnapshotTuning {
            page_size: Some(2),
            between_ref_pages: Some(Arc::new(|_attempt| {
                Box::pin(async {
                    let n = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|elapsed| elapsed.as_nanos())
                        .unwrap_or_default();
                    insert_branch_ref(&format!("refs/heads/pi-27-churn-{n}")).await;
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let error = sync_with_tuning(&mock, &r2, &churn)
            .await
            .expect_err("refs that keep changing stop the run");
        assert!(error.to_string().contains("kept changing"), "{error}");
        let probe = "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1";
        let probes: Vec<serde_json::Value> = ["agent_session", "agent_capture_session_v2"]
            .iter()
            .map(|table| serde_json::json!({ "sql": probe, "params": [table] }))
            .collect();
        assert_eq!(mock.request_bodies().await, probes);
        assert_eq!(r2_object_count(&r2).await, 0);
        assert!(spool_runs().is_empty());
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_paging_retries_when_the_connection_is_replaced_during_ref_setup() {
        // The pool replaces the freeze connection while the attempt marks it
        // or between creating the TEMP ref copy and its index: the statement
        // on the missing TEMP table fails, and that is retried like any other
        // change.
        for window in ["mark", "copy setup"] {
            let _repo = enter_isolated_libra_repo().await;
            for n in 0..4 {
                insert_branch_ref(&format!("refs/heads/pi-27-setup-{n}")).await;
            }
            let attempts = Arc::new(std::sync::Mutex::new(Vec::new()));
            let recorded = attempts.clone();
            let tuning = super::snapshot::SnapshotTuning {
                page_size: Some(3),
                simulate_reconnect_during_mark: window == "mark",
                simulate_reconnect_during_ref_setup: window == "copy setup",
                between_ref_pages: Some(Arc::new(move |attempt| {
                    let recorded = recorded.clone();
                    Box::pin(async move {
                        recorded.lock().unwrap().push(attempt);
                    })
                })),
                ..Default::default()
            };
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            let r2 = Arc::new(InMemory::new());
            sync_with_tuning(&mock, &r2, &tuning)
                .await
                .unwrap_or_else(|error| {
                    panic!("{window}: a replaced connection is retried: {error}")
                });
            let attempts = attempts.lock().unwrap().clone();
            assert!(!attempts.is_empty(), "{window}: the refs were copied");
            assert!(
                attempts.iter().all(|attempt| *attempt == 2),
                "{window}: the first attempt stopped and the second copied: {attempts:?}"
            );
            assert_eq!(
                uploaded_metadata(&r2).await,
                whole_reference_projection().await,
                "{window}: the retried projection is complete"
            );
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_paging_retries_when_the_connection_is_replaced_during_refs() {
        // The pool replaces the freeze connection in the middle of the ref
        // copy: the TEMP copy and marker are gone, the next ref read fails on
        // the missing table, and that is retried like any other change.
        let _repo = enter_isolated_libra_repo().await;
        for n in 0..10 {
            insert_branch_ref(&format!("refs/heads/pi-27-replaced-{n}")).await;
        }
        let attempts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = attempts.clone();
        let tuning = super::snapshot::SnapshotTuning {
            page_size: Some(3),
            simulate_reconnect_during_refs: true,
            between_ref_pages: Some(Arc::new(move |attempt| {
                let recorded = recorded.clone();
                Box::pin(async move {
                    recorded.lock().unwrap().push(attempt);
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        sync_with_tuning(&mock, &r2, &tuning)
            .await
            .expect("a replaced connection is retried");
        let attempts = attempts.lock().unwrap().clone();
        assert_eq!(attempts.first(), Some(&1), "{attempts:?}");
        assert!(attempts.contains(&2), "the freeze retried: {attempts:?}");
        assert_eq!(
            uploaded_metadata(&r2).await,
            whole_reference_projection().await,
            "the retried projection is complete"
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_paging_reports_a_marker_write_failure() {
        // A TEMP marker that cannot be written (here an injected I/O error)
        // is not a replaced connection: the run stops with that error at
        // once, before any remote write, instead of retrying into "kept
        // changing".
        let _repo = enter_isolated_libra_repo().await;
        index_local_object(b"pi-27 marker failure\n").await;
        let tuning = super::snapshot::SnapshotTuning {
            fail_mark_write: true,
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let error = sync_with_tuning(&mock, &r2, &tuning)
            .await
            .expect_err("a marker write failure stops the run");
        let detail = error.to_string();
        assert!(
            detail.contains("disk I/O error writing the TEMP marker (injected)"),
            "{detail}"
        );
        assert!(
            detail.contains("nothing was written to D1 or R2"),
            "{detail}"
        );
        assert!(!detail.contains("kept changing"), "{detail}");
        assert_eq!(r2_object_count(&r2).await, 0);
        assert!(spool_runs().is_empty());
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_paging_records_the_hash_only_after_upload() {
        // The stored hash is cleared before the upload and recorded after it:
        // when recording fails, the run says so and no hash is left behind,
        // so the next sync uploads the metadata again instead of skipping on
        // a stale hash.
        let _repo = enter_isolated_libra_repo().await;
        insert_branch_ref("refs/heads/pi-27-hash").await;
        ConfigKv::set("cloud.metadata_hash", "1", false)
            .await
            .expect("store an old hash");
        let tuning = super::snapshot::SnapshotTuning {
            fail_metadata_hash_record: true,
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let error = sync_with_tuning(&mock, &r2, &tuning)
            .await
            .expect_err("a failed hash record is reported");
        let detail = error.to_string();
        assert!(
            detail.contains("recording cloud.metadata_hash failed"),
            "{detail}"
        );
        assert!(detail.contains("uploads the metadata again"), "{detail}");
        assert_eq!(
            uploaded_metadata(&r2).await,
            whole_reference_projection().await
        );
        assert!(
            ConfigKv::get("cloud.metadata_hash")
                .await
                .expect("read the stored hash")
                .is_none(),
            "no stale hash is left"
        );

        let report = sync_with_tuning(&mock, &r2, &Default::default())
            .await
            .expect("the next sync");
        assert!(matches!(
            report.metadata,
            MetadataSyncOutcome::Synced { .. }
        ));
        assert_eq!(
            ConfigKv::get("cloud.metadata_hash")
                .await
                .expect("read the stored hash")
                .expect("hash recorded")
                .value,
            super::metadata::calculate_metadata_hash(&whole_reference_projection().await)
                .to_string()
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_paging_rejects_a_changed_refs_spool() {
        // A refs spool cut short, grown, or rewritten to the same length after
        // the freeze is a read failure: the run fails and no metadata is
        // uploaded from it.
        for (case, change) in [("truncated", -1i64), ("extended", 1), ("rewritten", 0)] {
            let _repo = enter_isolated_libra_repo().await;
            for n in 0..3 {
                insert_branch_ref(&format!("refs/heads/pi-27-length-{n}")).await;
            }
            let tuning = super::snapshot::SnapshotTuning {
                after_freeze: Some(Arc::new(move |dir| {
                    Box::pin(async move {
                        let path = dir.join("refs.json");
                        let mut bytes = fs::read(&path).expect("read refs spool");
                        match change {
                            -1 => {
                                bytes.pop();
                            }
                            1 => bytes.push(b' '),
                            _ => {
                                let last = bytes.len() - 2;
                                bytes[last] ^= 1;
                            }
                        }
                        fs::write(&path, bytes).expect("change refs spool");
                    })
                })),
                ..Default::default()
            };
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            let r2 = Arc::new(InMemory::new());
            let error = sync_with_tuning(&mock, &r2, &tuning)
                .await
                .expect_err("a changed refs spool fails the run");
            let detail = error.to_string();
            assert!(
                detail.contains("could not read its local snapshot spool"),
                "{case}: {detail}"
            );
            assert!(detail.contains("refs.json"), "{case}: {detail}");
            assert!(
                RemoteStorage::new(r2.clone()).get_metadata().await.is_err(),
                "{case}: no metadata is uploaded"
            );
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_paging_refuses_refs_changed_before_upload() {
        // The refs spool changes after its hash decided the upload and before
        // the upload reads it: whether sent in one PUT or in parts, the bytes
        // are checked against the frozen length and hash first, so nothing is
        // uploaded and cloud.metadata_hash is not recorded.
        for chunk in [None, Some(16)] {
            let _repo = enter_isolated_libra_repo().await;
            for n in 0..3 {
                insert_branch_ref(&format!("refs/heads/pi-27-late-{n}")).await;
            }
            let tuning = super::snapshot::SnapshotTuning {
                metadata_chunk_size: chunk,
                before_metadata_upload: Some(Arc::new(|path| {
                    Box::pin(async move {
                        let mut bytes = fs::read(&path).expect("read refs spool");
                        let last = bytes.len() - 2;
                        bytes[last] ^= 1;
                        fs::write(&path, bytes).expect("change refs spool");
                    })
                })),
                ..Default::default()
            };
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            let r2 = Arc::new(InMemory::new());
            let error = sync_with_tuning(&mock, &r2, &tuning)
                .await
                .expect_err("changed refs are not uploaded");
            let detail = error.to_string();
            assert!(
                detail.contains("changed after it was hashed"),
                "{chunk:?}: {detail}"
            );
            assert!(
                RemoteStorage::new(r2.clone()).get_metadata().await.is_err(),
                "{chunk:?}: no metadata is uploaded"
            );
            assert!(
                ConfigKv::get("cloud.metadata_hash")
                    .await
                    .expect("read the stored hash")
                    .is_none(),
                "{chunk:?}: no hash is recorded"
            );
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_paging_routes_a_background_upload_warning() {
        // A background cleanup failure is shown in every output mode: a
        // `cloud_sync.metadata.warning` NDJSON line under --progress=json,
        // otherwise a warning line on stderr.
        *JSON_PROGRESS_CAPTURE.lock().unwrap() = Some(Vec::new());
        super::snapshot::background_warning_report(
            super::snapshot::SpoolWarningSink::NdjsonProgress,
        )("abort failed for metadata.json".to_string());
        let lines = JSON_PROGRESS_CAPTURE
            .lock()
            .unwrap()
            .take()
            .expect("captured progress");
        assert_eq!(lines.len(), 1, "{lines:?}");
        let event: serde_json::Value = serde_json::from_str(&lines[0]).expect("one JSON line");
        assert_eq!(event["event"], "cloud_sync.metadata.warning");
        assert_eq!(event["message"], "abort failed for metadata.json");

        *super::snapshot::DROP_WARNING_CAPTURE.lock().unwrap() = Some(Vec::new());
        super::snapshot::background_warning_report(super::snapshot::SpoolWarningSink::Stderr)(
            "abort failed for metadata.json".to_string(),
        );
        let printed = super::snapshot::DROP_WARNING_CAPTURE
            .lock()
            .unwrap()
            .take()
            .expect("captured stderr");
        assert_eq!(
            printed,
            vec!["warning: abort failed for metadata.json".to_string()]
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_retries_or_stops_when_rows_change() {
        use crate::internal::model::object_index;

        let _repo = enter_isolated_libra_repo().await;
        let repo_id = ensure_repo_id().await;
        let mut hashes = Vec::new();
        for n in 0..5 {
            hashes.push(index_local_object(format!("pi-09 consistent {n}\n").as_bytes()).await);
        }
        let deleted = hashes[4].to_string();
        let promoted = hashes[3].to_string();

        // Rows change once, during the first attempt: the freeze retries and
        // the run uploads the second attempt's consistent state.
        let tuning = super::snapshot::SnapshotTuning {
            page_size: Some(2),
            between_pages: Some(Arc::new(move |attempt| {
                let deleted = deleted.clone();
                let promoted = promoted.clone();
                Box::pin(async move {
                    if attempt != 1 {
                        return;
                    }
                    let conn = db::get_db_conn_instance().await;
                    object_index::Entity::delete_many()
                        .filter(object_index::Column::OId.eq(deleted))
                        .exec(&conn)
                        .await
                        .expect("delete a covered row");
                    // Same length and first letter as `blob`: no aggregate
                    // could tell the two apart.
                    object_index::Entity::update_many()
                        .col_expr(object_index::Column::OType, Expr::value("bolb"))
                        .filter(object_index::Column::OId.eq(promoted))
                        .exec(&conn)
                        .await
                        .expect("change a covered row");
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("one change is absorbed by a retry");
        assert_eq!((report.total_unsynced, report.failed_count), (4, 0));
        let rows = mock
            .client()
            .get_object_indexes(&repo_id)
            .await
            .expect("read D1 object index");
        assert!(!rows.iter().any(|row| row.o_id == hashes[4].to_string()));
        let changed = rows
            .iter()
            .find(|row| row.o_id == hashes[3].to_string())
            .expect("the changed row is uploaded");
        assert_eq!(
            changed.o_type, "bolb",
            "the uploaded metadata is the consistent one"
        );

        // Rows that keep changing stop the run before any remote write.
        for n in 0..3 {
            index_local_object(format!("pi-09 churn {n}\n").as_bytes()).await;
        }
        let churn = super::snapshot::SnapshotTuning {
            page_size: Some(1),
            between_pages: Some(Arc::new(|_attempt| {
                Box::pin(async {
                    let n = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|elapsed| elapsed.as_nanos())
                        .unwrap_or_default();
                    index_local_object(format!("pi-09 churn writer {n}\n").as_bytes()).await;
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let error = sync_with_tuning(&mock, &r2, &churn)
            .await
            .expect_err("a database that keeps changing stops the run");
        let detail = error.to_string();
        assert!(detail.contains("kept changing"), "{detail}");
        assert!(
            detail.contains("nothing was written to D1 or R2"),
            "{detail}"
        );
        let probe = "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1";
        let probes: Vec<serde_json::Value> = ["agent_session", "agent_capture_session_v2"]
            .iter()
            .map(|table| serde_json::json!({ "sql": probe, "params": [table] }))
            .collect();
        assert_eq!(mock.request_bodies().await, probes);
        assert_eq!(r2_object_count(&r2).await, 0);
        assert!(spool_runs().is_empty());
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_keeps_a_row_promoted_after_the_freeze_unsynced() {
        use crate::internal::model::object_index;

        let _repo = enter_isolated_libra_repo().await;
        let promoted = index_local_object(b"pi-09 promoted after freeze\n").await;
        let kept = index_local_object(b"pi-09 unchanged after freeze\n").await;
        let target = promoted.to_string();
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(Arc::new(move |_dir: PathBuf| {
                let target = target.clone();
                Box::pin(async move {
                    object_index::Entity::update_many()
                        .col_expr(object_index::Column::OType, Expr::value("agent_transcript"))
                        .col_expr(object_index::Column::IsSynced, Expr::value(0))
                        .filter(object_index::Column::OId.eq(target))
                        .exec(&db::get_db_conn_instance().await)
                        .await
                        .expect("promote a frozen row");
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("the run completes and reports the changed row");
        assert_eq!((report.synced_count, report.failed_count), (1, 1));
        assert_eq!(
            local_is_synced(&promoted).await,
            0,
            "the promoted metadata still needs an upload"
        );
        assert_eq!(local_is_synced(&kept).await, 1);
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_retries_when_the_freeze_connection_is_replaced() {
        let _repo = enter_isolated_libra_repo().await;
        for n in 0..3 {
            index_local_object(format!("pi-09 reconnect {n}\n").as_bytes()).await;
        }
        let attempts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = attempts.clone();
        // No other connection commits, so `data_version` alone would match;
        // the first attempt still ends on what a replaced connection looks
        // like and must not be trusted.
        let tuning = super::snapshot::SnapshotTuning {
            page_size: Some(2),
            simulate_reconnect: true,
            between_pages: Some(Arc::new(move |attempt| {
                let recorded = recorded.clone();
                Box::pin(async move {
                    recorded.lock().unwrap().push(attempt);
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("the retried freeze succeeds");
        assert_eq!(report.synced_count, 3);
        let attempts = attempts.lock().unwrap().clone();
        assert!(
            attempts.contains(&1) && attempts.contains(&2),
            "{attempts:?}"
        );
        assert!(!attempts.contains(&3), "{attempts:?}");
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_freezes_the_fallback_project_name() {
        let _repo = enter_isolated_libra_repo().await;
        let repo_id = ensure_repo_id().await;
        index_local_object(b"pi-09 fallback name\n").await;
        let fallback = util::working_dir()
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .expect("working directory name");
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(Arc::new(|_dir: PathBuf| {
                Box::pin(async {
                    ConfigKv::set("cloud.name", "pi09-set-after-freeze", false)
                        .await
                        .expect("set cloud.name after the freeze");
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        #[derive(serde::Deserialize)]
        struct NameRow {
            name: String,
        }
        let names: Vec<NameRow> = mock
            .client()
            .query(
                "SELECT name FROM repositories WHERE repo_id = ?1",
                Some(vec![serde_json::json!(repo_id)]),
            )
            .await
            .expect("read D1 repositories");
        assert_eq!(
            names.into_iter().map(|row| row.name).collect::<Vec<_>>(),
            vec![fallback],
            "the fallback name resolved at the freeze is registered"
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_config_read_failure_precedes_remote_side_effects() {
        let _repo = enter_isolated_libra_repo().await;
        index_local_object(b"pi-09 config read\n").await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let tuning = super::snapshot::SnapshotTuning {
            fail_config_read: true,
            ..Default::default()
        };
        let error = sync_with_tuning(&mock, &r2, &tuning)
            .await
            .expect_err("a cloud.name read failure stops the sync");
        assert!(error.to_string().contains("read cloud.name"), "{error}");
        let probe = "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1";
        let expected: Vec<serde_json::Value> = ["agent_session", "agent_capture_session_v2"]
            .iter()
            .map(|table| serde_json::json!({ "sql": probe, "params": [table] }))
            .collect();
        assert_eq!(mock.request_bodies().await, expected);
        assert_eq!(r2_object_count(&r2).await, 0);
        assert!(spool_runs().is_empty());
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_spool_reports_marker_and_read_failures() {
        let _repo = enter_isolated_libra_repo().await;
        index_local_object(b"pi-09 marker failure\n").await;

        // A marker write failure removes the half-made run; when that removal
        // fails too, the error names the directory.
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let tuning = super::snapshot::SnapshotTuning {
            fail_marker_write: true,
            fail_spool_release: true,
            ..Default::default()
        };
        let error = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect_err("a marker write failure stops the sync");
        let detail = error.to_string();
        assert!(detail.contains("injected marker write failure"), "{detail}");
        let retained = spool_runs();
        assert_eq!(
            retained.len(),
            1,
            "the injected removal failure keeps one run"
        );
        assert!(
            detail.contains(&retained[0].display().to_string()),
            "{detail}"
        );
        assert!(
            detail.contains("delete that directory manually"),
            "{detail}"
        );
        assert!(!remote_table_present(&mock, "object_index").await);
        fs::remove_dir_all(&retained[0]).unwrap();

        // A spool read after the freeze does not claim that nothing was sent.
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(Arc::new(|dir: PathBuf| {
                Box::pin(async move {
                    fs::remove_file(dir.join("objects.jsonl")).expect("drop the object spool");
                })
            })),
            ..Default::default()
        };
        let error = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect_err("a missing object spool stops the sync");
        let detail = error.to_string();
        assert!(detail.contains("may already have been sent"), "{detail}");
        assert!(!detail.contains("nothing was written"), "{detail}");
        assert!(spool_runs().is_empty());

        // A spool cut to a valid prefix is a read failure, not a smaller
        // successful upload.
        index_local_object(b"pi-09 truncated a\n").await;
        index_local_object(b"pi-09 truncated b\n").await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(Arc::new(|dir: PathBuf| {
                Box::pin(async move {
                    let path = dir.join("objects.jsonl");
                    let text = fs::read_to_string(&path).unwrap();
                    let first = text.split_inclusive('\n').next().unwrap().to_string();
                    fs::write(&path, first).unwrap();
                })
            })),
            ..Default::default()
        };
        let error = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect_err("a truncated spool stops the sync");
        assert!(
            error.to_string().contains("short of the frozen count"),
            "{error}"
        );
        assert!(spool_runs().is_empty());
    }

    // ---- PI-26: the agent-capture catalog in the frozen snapshot -----------

    const PI26_SESSION: &str = "pi26-session";
    /// What a note about a kept run directory says about its catalog file.
    const PI26_CATALOG_LISTING: &str = "catalog.json (agent-capture catalog: sessions, checkpoints, prune and import tombstones, the traces head, subagent claims, revisions and links, legacy Claude mirror rows)";

    /// Every loose object id under the repository's `objects` directory.
    fn loose_object_ids() -> std::collections::BTreeSet<String> {
        let mut ids = std::collections::BTreeSet::new();
        let objects = util::storage_path().join("objects");
        for prefix in fs::read_dir(objects).into_iter().flatten().flatten() {
            let name = prefix.file_name().to_string_lossy().into_owned();
            if name.len() != 2 || !prefix.path().is_dir() {
                continue;
            }
            for file in fs::read_dir(prefix.path()).into_iter().flatten().flatten() {
                ids.insert(format!("{name}{}", file.file_name().to_string_lossy()));
            }
        }
        ids
    }

    /// Write the traces commit of checkpoint `checkpoint_id` over `tree`.
    fn write_traces_commit(
        tree: &str,
        parent: Option<ObjectHash>,
        checkpoint_id: &str,
    ) -> ObjectHash {
        use git_internal::internal::object::{
            ObjectTrait,
            commit::Commit,
            signature::{Signature, SignatureType},
        };

        let signature = |kind| Signature::new(kind, "Libra".into(), "traces@libra".into());
        let commit = Commit::new(
            signature(SignatureType::Author),
            signature(SignatureType::Committer),
            tree.parse().expect("tree id"),
            parent.into_iter().collect(),
            &format!(
                "traces: committed checkpoint {checkpoint_id}\n\nLibra-Session: {PI26_SESSION}\nLibra-Agent: claude_code\nLibra-Checkpoint-ID: {checkpoint_id}\nLibra-Scope: committed\n"
            ),
        );
        crate::utils::object::write_git_object(
            &util::storage_path(),
            "commit",
            &commit.to_data().expect("encode traces commit"),
        )
        .expect("write traces commit")
    }

    /// Index on `conn`, with `is_synced`, every loose object not in `before`.
    async fn index_loose_objects_since<C: sea_orm::ConnectionTrait>(
        conn: &C,
        repo_id: &str,
        before: &std::collections::BTreeSet<String>,
        is_synced: i32,
    ) -> Result<Vec<String>, sea_orm::DbErr> {
        use sea_orm::Statement;

        let storage = util::storage_path();
        let mut indexed = Vec::new();
        for oid in loose_object_ids().difference(before) {
            let hash = crate::internal::object_format::parse_repo_oid(oid).expect("object id");
            let (kind, bytes) = crate::utils::object::read_git_object_bounded_validated(
                &storage,
                &hash,
                4 * 1024 * 1024,
            )
            .expect("read fixture object");
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced)
                 VALUES (?, ?, ?, ?, 1, ?)",
                [
                    oid.clone().into(),
                    kind.into(),
                    (bytes.len() as i64).into(),
                    repo_id.into(),
                    is_synced.into(),
                ],
            ))
            .await?;
            indexed.push(oid.clone());
        }
        Ok(indexed)
    }

    /// One committed agent-capture checkpoint in the isolated repository
    /// (PI-26): its session and checkpoint rows, the traces ref at its
    /// commit, and every object it added indexed with `is_synced`. Returns
    /// the traces commit, the root tree and the indexed objects.
    async fn seed_agent_capture_checkpoint(is_synced: i32) -> (ObjectHash, String, Vec<String>) {
        use sea_orm::Statement;

        use crate::internal::ai::checkpoint_reader::{
            read_tree_object, subtree, test_helpers::*, tree_entry,
        };

        let storage = util::storage_path();
        let conn = db::get_db_conn_instance().await;
        let repo_id = ensure_repo_id().await;
        let before = loose_object_ids();
        let (root, _artifact) = rg03_read_fixture(&storage, "valid", false);
        let tree = read_tree_object(&storage, &root).expect("fixture root");
        let checkpoint = subtree(&storage, &tree, "checkpoint").expect("checkpoint tree");
        let prefix = subtree(&storage, &checkpoint, &RG03_CHECKPOINT_ID[..2]).expect("prefix");
        let inner = subtree(&storage, &prefix, &RG03_CHECKPOINT_ID[2..]).expect("checkpoint");
        let metadata = tree_entry(&inner, "metadata.json")
            .expect("metadata entry")
            .id;
        let commit = write_traces_commit(&root, None, RG03_CHECKPOINT_ID);
        insert_pi26_session(&conn, PI26_SESSION)
            .await
            .expect("insert the session");
        for (sql, values) in [
            (
                "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, tree_oid,
                   metadata_blob_oid, traces_commit, created_at)
                 VALUES (?, ?, 'committed', ?, ?, ?, 3)",
                vec![
                    RG03_CHECKPOINT_ID.into(),
                    PI26_SESSION.into(),
                    root.clone().into(),
                    metadata.to_string().into(),
                    commit.to_string().into(),
                ],
            ),
            (
                "UPDATE reference SET `commit` = ? WHERE name = ? AND remote IS NULL",
                vec![
                    commit.to_string().into(),
                    crate::internal::branch::TRACES_BRANCH.into(),
                ],
            ),
        ] {
            let result = conn
                .execute_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    sql,
                    values,
                ))
                .await
                .expect("seed the checkpoint");
            assert_eq!(result.rows_affected(), 1, "{sql}");
        }
        let indexed = index_loose_objects_since(&conn, &repo_id, &before, is_synced)
            .await
            .expect("index the checkpoint objects");
        (commit, root, indexed)
    }

    async fn insert_pi26_session<C: sea_orm::ConnectionTrait>(
        conn: &C,
        session_id: &str,
    ) -> Result<(), sea_orm::DbErr> {
        use sea_orm::Statement;

        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state,
               working_dir, started_at, last_event_at, metadata_json, redaction_report)
             VALUES (?, 'claude_code', ?, 'stopped', '/tmp/pi26-fixture', 1, 2, '{}', '{}')",
            [session_id.into(), format!("{session_id}-provider").into()],
        ))
        .await
        .map(|_| ())
    }

    /// Run `sql` on a second connection, as another process would.
    async fn run_on_other_connection(sql: &str) {
        use sea_orm::ConnectionTrait;

        let other = other_connection().await;
        other
            .execute_unprepared(sql)
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let _ = other.close().await;
    }

    /// A second connection to the repository database with a 200 ms busy
    /// timeout, as another process would open one.
    async fn other_connection() -> sea_orm::DatabaseConnection {
        let db_path = util::storage_path().join(util::DATABASE);
        crate::internal::db::establish_connection_with_busy_timeout(
            db_path.to_str().expect("utf-8 path"),
            std::time::Duration::from_millis(200),
        )
        .await
        .expect("open a second connection")
    }

    /// Hook that records the `catalog.json` each successful freeze wrote.
    fn catalog_recorder(
        seen: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        extra: impl Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync + 'static,
    ) -> super::snapshot::AfterFreezeHook {
        Arc::new(move |dir: PathBuf| {
            let catalog: serde_json::Value =
                serde_json::from_slice(&fs::read(dir.join("catalog.json")).expect("catalog.json"))
                    .expect("catalog.json is JSON");
            seen.lock().unwrap().push(catalog);
            extra()
        })
    }

    async fn remote_catalog(
        mock: &crate::utils::d1_client::test_support::MockD1,
        repo_id: &str,
    ) -> (Vec<String>, Vec<String>) {
        let rows = mock
            .client()
            .list_agent_capture_restore_catalog_rows(repo_id, true, 100)
            .await
            .expect("read the mirrored catalog");
        let mut sessions: Vec<String> = rows
            .sessions
            .into_iter()
            .map(|row| row.session_id)
            .collect();
        let mut checkpoints: Vec<String> = rows
            .checkpoints
            .into_iter()
            .map(|row| row.checkpoint_id)
            .collect();
        sessions.sort();
        checkpoints.sort();
        (sessions, checkpoints)
    }

    /// No agent-capture catalog row, and no generation, reached D1 (its
    /// tables may exist).
    async fn assert_no_remote_catalog(
        label: &str,
        mock: &crate::utils::d1_client::test_support::MockD1,
    ) {
        #[derive(serde::Deserialize)]
        struct Count {
            n: i64,
        }
        for table in [
            "agent_session",
            "agent_checkpoint",
            "agent_capture_generation",
            "agent_checkpoint_prune_tombstone",
            "agent_import_tombstone",
            "agent_subagent_content_claim",
            "agent_subagent_content_revision",
            "agent_subagent_link",
        ] {
            if !remote_table_present(mock, table).await {
                continue;
            }
            let rows: Vec<Count> = mock
                .client()
                .query(&format!("SELECT COUNT(*) AS n FROM {table}"), None)
                .await
                .expect("count remote rows");
            assert_eq!(rows[0].n, 0, "{label}: rows reached D1 table {table}");
        }
    }

    /// The live catalog as a sync would freeze it.
    async fn frozen_catalog(
        conn: &sea_orm::DatabaseConnection,
    ) -> Box<super::agent_capture::FrozenAgentCaptureCatalog> {
        match super::agent_capture::freeze_agent_capture(conn, None).await {
            super::agent_capture::FrozenAgentCapture::Catalog(catalog) => catalog,
            other => panic!("the seeded catalog freezes: {other:?}"),
        }
    }

    fn refused_with(label: &str, report: &CloudSyncReport, expected: &str) {
        match &report.agent_capture {
            AgentCaptureSyncOutcome::Failed { error } => {
                assert!(error.contains(expected), "{label}: {error}");
                assert!(error.contains("retry"), "{label}: {error}");
            }
            other => panic!("{label}: publication was not refused: {other:?}"),
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_catalog_is_frozen_in_the_same_view() {
        let _repo = enter_isolated_libra_repo().await;
        let repo_id = ensure_repo_id().await;
        let (_, _, indexed) = seed_agent_capture_checkpoint(0).await;
        let late_object = Arc::new(std::sync::Mutex::new(None));
        let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let catalogs = Arc::new(std::sync::Mutex::new(Vec::new()));
        // While the first attempt reads the catalog, another connection
        // commits a session after its first page and an object row after its
        // second. The freeze retries, and the second attempt is one view
        // holding both, with the objects the catalog needs still pending.
        let (recorded, late) = (writes.clone(), late_object.clone());
        let tuning = super::snapshot::SnapshotTuning {
            between_catalog_pages: Some(Arc::new(move |attempt| {
                let (recorded, late) = (recorded.clone(), late.clone());
                Box::pin(async move {
                    if attempt != 1 {
                        return;
                    }
                    let n = recorded.lock().unwrap().len();
                    match n {
                        0 => {
                            let other = other_connection().await;
                            let result = insert_pi26_session(&other, "pi26-late-session").await;
                            let _ = other.close().await;
                            recorded
                                .lock()
                                .unwrap()
                                .push(result.map_err(|e| e.to_string()));
                        }
                        1 => {
                            let hash = index_local_object(b"pi-26 late object\n").await;
                            *late.lock().unwrap() = Some(hash);
                            recorded.lock().unwrap().push(Ok(()));
                        }
                        _ => {}
                    }
                })
            })),
            after_freeze: Some(catalog_recorder(catalogs.clone(), || Box::pin(async {}))),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        let writes = writes.lock().unwrap().clone();
        assert_eq!(writes.len(), 2, "{writes:?}");
        for write in &writes {
            write.as_ref().expect("the concurrent write commits");
        }
        let catalogs = catalogs.lock().unwrap().clone();
        assert_eq!(catalogs.len(), 1, "one freeze is adopted");
        let frozen = catalogs[0].to_string();
        assert!(frozen.contains("pi26-late-session"), "{frozen}");
        assert_eq!(
            report.total_unsynced,
            indexed.len() + 1,
            "the late object is frozen with the catalog"
        );
        assert_eq!(report.failed_count, 0);
        assert_eq!(
            report.agent_capture,
            AgentCaptureSyncOutcome::Completed {
                sessions_synced: 2,
                sessions_failed: 0,
                checkpoints_synced: 1,
                checkpoints_failed: 0,
            }
        );
        let (sessions, checkpoints) = remote_catalog(&mock, &repo_id).await;
        assert_eq!(sessions, vec!["pi26-late-session", PI26_SESSION]);
        assert_eq!(
            checkpoints,
            vec![crate::internal::ai::checkpoint_reader::test_helpers::RG03_CHECKPOINT_ID]
        );
        // Every object the frozen catalog needs, pending at the freeze, is in
        // the remote object index.
        let remote: std::collections::HashSet<String> = mock
            .client()
            .get_object_indexes(&repo_id)
            .await
            .expect("read the D1 object index")
            .into_iter()
            .map(|row| row.o_id)
            .collect();
        for oid in &indexed {
            assert!(remote.contains(oid), "{oid} missing from the remote index");
        }
        assert!(
            remote.contains(
                &late_object
                    .lock()
                    .unwrap()
                    .expect("late object")
                    .to_string()
            )
        );

        // A catalog that keeps changing stops the run before any remote write.
        let churn = super::snapshot::SnapshotTuning {
            between_catalog_pages: Some(Arc::new(|_attempt| {
                Box::pin(async {
                    let n = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|elapsed| elapsed.as_nanos())
                        .unwrap_or_default();
                    let other = other_connection().await;
                    insert_pi26_session(&other, &format!("pi26-churn-{n}"))
                        .await
                        .expect("insert a churn session");
                    let _ = other.close().await;
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let error = sync_with_tuning(&mock, &r2, &churn)
            .await
            .expect_err("a catalog that keeps changing stops the run");
        let detail = error.to_string();
        assert!(detail.contains("kept changing"), "{detail}");
        assert!(
            detail.contains("nothing was written to D1 or R2"),
            "{detail}"
        );
        assert!(!remote_table_present(&mock, "object_index").await);
        assert_no_remote_catalog("churn", &mock).await;
        assert_eq!(r2_object_count(&r2).await, 0);
        assert!(spool_runs().is_empty());
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_catalog_pages_do_not_block_other_writers() {
        let _repo = enter_isolated_libra_repo().await;
        seed_agent_capture_checkpoint(1).await;
        let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let catalogs = Arc::new(std::sync::Mutex::new(Vec::new()));
        // After every catalog page of the first attempt another connection
        // commits a session with a 200 ms busy timeout. The freeze holds no
        // lock between its statements, so every write lands at once; the
        // retried freeze is then one consistent catalog holding all of them.
        let recorded = writes.clone();
        let tuning = super::snapshot::SnapshotTuning {
            between_catalog_pages: Some(Arc::new(move |attempt| {
                let recorded = recorded.clone();
                Box::pin(async move {
                    if attempt != 1 {
                        return;
                    }
                    let n = recorded.lock().unwrap().len();
                    let other = other_connection().await;
                    let result = insert_pi26_session(&other, &format!("pi26-writer-{n}"))
                        .await
                        .map_err(|error| error.to_string());
                    let _ = other.close().await;
                    recorded.lock().unwrap().push(result);
                })
            })),
            after_freeze: Some(catalog_recorder(catalogs.clone(), || Box::pin(async {}))),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        let writes = writes.lock().unwrap().clone();
        assert!(
            writes.len() >= 3,
            "the catalog is read as several pages: {writes:?}"
        );
        for write in &writes {
            write
                .as_ref()
                .unwrap_or_else(|error| panic!("a concurrent writer was blocked: {error}"));
        }
        let catalogs = catalogs.lock().unwrap().clone();
        let sessions = catalogs[0]["Catalog"]["sessions"]
            .as_array()
            .expect("frozen sessions")
            .len();
        assert_eq!(
            sessions,
            1 + writes.len(),
            "the retried freeze holds every write"
        );
        assert_eq!(
            report.agent_capture,
            AgentCaptureSyncOutcome::Completed {
                sessions_synced: 1 + writes.len(),
                sessions_failed: 0,
                checkpoints_synced: 1,
                checkpoints_failed: 0,
            }
        );
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_catalog_never_publishes_erased_or_changed_rows() {
        use crate::internal::ai::checkpoint_reader::test_helpers::RG03_CHECKPOINT_ID;

        // Each change lands after the freeze and before publication; the
        // frozen rows are never published over it.
        let erase = format!(
            "DELETE FROM agent_checkpoint WHERE checkpoint_id = '{RG03_CHECKPOINT_ID}';
             DELETE FROM agent_session WHERE session_id = '{PI26_SESSION}';
             INSERT INTO agent_import_tombstone (tombstone_id, agent_kind, provider_session_id,
               erased_session_id, erased_at)
             VALUES ('pi26-erased', 'claude_code', '{PI26_SESSION}-provider', '{PI26_SESSION}', 5);"
        );
        let revoke = format!(
            "INSERT INTO agent_checkpoint_prune_tombstone (checkpoint_id, session_id, pruned_at)
             VALUES ('{RG03_CHECKPOINT_ID}', '{PI26_SESSION}', 5);"
        );
        let modify = format!(
            "UPDATE agent_session SET last_event_at = 3 WHERE session_id = '{PI26_SESSION}';"
        );
        for (label, sql) in [
            ("erased", erase),
            ("tombstoned", revoke),
            ("modified", modify),
        ] {
            let _repo = enter_isolated_libra_repo().await;
            seed_agent_capture_checkpoint(0).await;
            let tuning = super::snapshot::SnapshotTuning {
                after_freeze: Some(Arc::new(move |_dir: PathBuf| {
                    let sql = sql.clone();
                    Box::pin(async move { run_on_other_connection(&sql).await })
                })),
                ..Default::default()
            };
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
                .await
                .unwrap_or_else(|error| panic!("{label}: {error}"));
            assert_eq!(report.failed_count, 0, "{label}");
            refused_with(label, &report, "catalog changed");
            assert_no_remote_catalog(label, &mock).await;
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_catalog_excludes_uncommitted_trace_intents() {
        use crate::internal::ai::checkpoint_reader::test_helpers::RG03_CHECKPOINT_ID;

        const LATER: &str = "aabbccdd-eeff-0011-2233-445566778800";
        let _repo = enter_isolated_libra_repo().await;
        let repo_id = ensure_repo_id().await;
        let (committed, root, _) = seed_agent_capture_checkpoint(0).await;
        // A later checkpoint's writer wrote its traces commit, then holds its
        // intent — the catalog row, the traces ref move and the object rows —
        // in one open transaction while the freeze reads the catalog, and
        // crashes before committing it.
        let before = loose_object_ids();
        let pending = write_traces_commit(&root, Some(committed), LATER);
        let intent = format!(
            "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, tree_oid,
               metadata_blob_oid, traces_commit, created_at)
             SELECT '{LATER}', session_id, scope, tree_oid, metadata_blob_oid, '{pending}', 4
             FROM agent_checkpoint WHERE checkpoint_id = '{RG03_CHECKPOINT_ID}';
             UPDATE reference SET `commit` = '{pending}'
             WHERE name = '{}' AND remote IS NULL;",
            crate::internal::branch::TRACES_BRANCH
        );
        let writer: Arc<tokio::sync::Mutex<Option<sea_orm::DatabaseTransaction>>> =
            Arc::new(tokio::sync::Mutex::new(None));
        let opened = writer.clone();
        let (repo, intent_sql) = (repo_id.clone(), intent.clone());
        let catalogs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let crashed = writer.clone();
        let tuning = super::snapshot::SnapshotTuning {
            between_catalog_pages: Some(Arc::new(move |attempt| {
                let (opened, repo, intent_sql, before) = (
                    opened.clone(),
                    repo.clone(),
                    intent_sql.clone(),
                    before.clone(),
                );
                Box::pin(async move {
                    use sea_orm::{ConnectionTrait, TransactionTrait};

                    let mut slot = opened.lock().await;
                    if attempt != 1 || slot.is_some() {
                        return;
                    }
                    let txn = other_connection()
                        .await
                        .begin()
                        .await
                        .expect("begin the writer's transaction");
                    txn.execute_unprepared(&intent_sql)
                        .await
                        .expect("stage the intent");
                    index_loose_objects_since(&txn, &repo, &before, 0)
                        .await
                        .expect("stage the object rows");
                    *slot = Some(txn);
                })
            })),
            after_freeze: Some(catalog_recorder(catalogs.clone(), move || {
                let crashed = crashed.clone();
                Box::pin(async move {
                    let txn = crashed
                        .lock()
                        .await
                        .take()
                        .expect("the writer staged its intent");
                    txn.rollback().await.expect("the writer crashes");
                })
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let r2 = Arc::new(InMemory::new());
        let report = sync_with_tuning(&mock, &r2, &tuning).await.expect("sync");
        let frozen = catalogs.lock().unwrap()[0].to_string();
        assert!(!frozen.contains(LATER), "{frozen}");
        assert!(!frozen.contains(&pending.to_string()), "{frozen}");
        assert!(frozen.contains(&committed.to_string()), "{frozen}");
        assert_eq!(
            report.agent_capture,
            AgentCaptureSyncOutcome::Completed {
                sessions_synced: 1,
                sessions_failed: 0,
                checkpoints_synced: 1,
                checkpoints_failed: 0,
            }
        );
        let (_, checkpoints) = remote_catalog(&mock, &repo_id).await;
        assert_eq!(checkpoints, vec![RG03_CHECKPOINT_ID]);
        let remote = mock
            .client()
            .get_object_indexes(&repo_id)
            .await
            .expect("read the D1 object index");
        assert!(!remote.iter().any(|row| row.o_id == pending.to_string()));
        assert!(!RemoteStorage::new(r2.clone()).exist(&pending).await);

        // The same writer committing after the freeze instead: its intent is
        // not in the snapshot, and the snapshot is not published over it.
        let committed_writer = Arc::new(tokio::sync::Mutex::new(None));
        let opened = committed_writer.clone();
        let tuning = super::snapshot::SnapshotTuning {
            between_catalog_pages: Some(Arc::new(move |attempt| {
                let opened = opened.clone();
                Box::pin(async move {
                    use sea_orm::{ConnectionTrait, TransactionTrait};

                    let mut slot = opened.lock().await;
                    if attempt != 1 || slot.is_some() {
                        return;
                    }
                    let txn = other_connection()
                        .await
                        .begin()
                        .await
                        .expect("begin the writer's transaction");
                    txn.execute_unprepared(&format!(
                        "UPDATE agent_session SET last_event_at = 7 WHERE session_id = '{PI26_SESSION}'"
                    ))
                    .await
                    .expect("stage the write");
                    *slot = Some(txn);
                })
            })),
            after_freeze: Some({
                let writer = committed_writer.clone();
                Arc::new(move |_dir: PathBuf| {
                    let writer = writer.clone();
                    Box::pin(async move {
                        let txn: sea_orm::DatabaseTransaction = writer
                            .lock()
                            .await
                            .take()
                            .expect("the writer staged its write");
                        txn.commit().await.expect("the writer commits");
                    })
                })
            }),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        refused_with("committed after the freeze", &report, "catalog changed");
        assert_no_remote_catalog("committed after the freeze", &mock).await;
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_catalog_publishes_only_after_every_check() {
        // A required object whose upload fails: nothing of the catalog is
        // published.
        {
            let _repo = enter_isolated_libra_repo().await;
            seed_agent_capture_checkpoint(0).await;
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            mock.fail_once_when_sql_contains(
                "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced, object_format)",
                "injected upload failure",
            );
            let report = sync_with_tuning(
                &mock,
                &Arc::new(InMemory::new()),
                &super::snapshot::SnapshotTuning::default(),
            )
            .await
            .expect("sync");
            assert_eq!(report.failed_count, 1);
            assert_eq!(report.agent_capture, AgentCaptureSyncOutcome::NotRun);
            assert_no_remote_catalog("upload failed", &mock).await;
        }
        // An unrelated object indexed after the freeze and left unsynced: the
        // repository-wide generation gate refuses.
        {
            let _repo = enter_isolated_libra_repo().await;
            seed_agent_capture_checkpoint(0).await;
            let tuning = super::snapshot::SnapshotTuning {
                after_freeze: Some(Arc::new(|_dir: PathBuf| {
                    Box::pin(async {
                        index_local_object(b"pi-26 unrelated after the freeze\n").await;
                    })
                })),
                ..Default::default()
            };
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
                .await
                .expect("sync");
            assert_eq!(report.failed_count, 0);
            refused_with(
                "unrelated unsynced object",
                &report,
                "outside the completed object upload generation",
            );
            assert_no_remote_catalog("unrelated unsynced object", &mock).await;
        }
        // A required object marked synced but missing locally: the
        // reachability and durability check refuses.
        {
            let _repo = enter_isolated_libra_repo().await;
            let (commit, _, _) = seed_agent_capture_checkpoint(1).await;
            let id = commit.to_string();
            fs::remove_file(
                util::storage_path()
                    .join("objects")
                    .join(&id[..2])
                    .join(&id[2..]),
            )
            .expect("lose the traces commit");
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            let report = sync_with_tuning(
                &mock,
                &Arc::new(InMemory::new()),
                &super::snapshot::SnapshotTuning::default(),
            )
            .await
            .expect("sync");
            refused_with("missing object", &report, "not fully reachable and durable");
            assert_no_remote_catalog("missing object", &mock).await;
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_catalog_kept_runs_list_their_catalog_file() {
        // A run directory that stays on disk — this run's own, when it cannot
        // be removed, or a leftover the next sync keeps — is reported with
        // the catalog file it holds, in every output mode.
        for (label, mode) in report_modes() {
            let _repo = enter_isolated_libra_repo().await;
            seed_agent_capture_checkpoint(0).await;
            let live = plant_residue(
                "run-live",
                Some(&marker_bytes(&marker_for(std::process::id()))),
            );
            let mut file = super::snapshot::create_private_file(&live.join("catalog.json"))
                .expect("create a leftover catalog");
            std::io::Write::write_all(&mut file, b"{}").expect("write a leftover catalog");
            drop(file);
            let tuning = super::snapshot::SnapshotTuning {
                fail_spool_release: true,
                ..Default::default()
            };
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            let (concluded, _console, ndjson) =
                drive_sync_in_mode(&mode, &mock, Arc::new(InMemory::new()), &tuning).await;
            let structured = concluded
                .unwrap_or_else(|error| panic!("{label}: the sync still succeeds: {error}"));
            let own = spool_runs()
                .into_iter()
                .find(|run| run != &live)
                .unwrap_or_else(|| panic!("{label}: this run's directory is kept"));
            let pending = crate::utils::output::pending_warning_messages();
            let events: Vec<String> = ndjson
                .iter()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|event| event["event"] == "cloud_sync.spool.warning")
                .filter_map(|event| event["message"].as_str().map(str::to_string))
                .collect();
            let notes: Vec<String> = [&live, &own]
                .iter()
                .map(|run| {
                    let path = run.display().to_string();
                    let found = match label {
                        "human" | "quiet" => pending.iter().find(|note| note.contains(&path)),
                        _ => events.iter().find(|note| note.contains(&path)),
                    };
                    found
                        .unwrap_or_else(|| panic!("{label}: {path} not reported"))
                        .clone()
                })
                .collect();
            for note in &notes {
                assert!(note.contains(PI26_CATALOG_LISTING), "{label}: {note}");
            }
            // Only the files a directory actually holds are listed: the
            // leftover holds its marker and its catalog alone.
            for absent in ["objects.jsonl", "refs.json", "repository.json"] {
                assert!(!notes[0].contains(absent), "{label}: {}", notes[0]);
                assert!(
                    notes[1].contains(&format!("{absent} (")),
                    "{label}: {}",
                    notes[1]
                );
            }
            if label == "json" {
                let data = serde_json::to_value(structured.expect("--json data")).unwrap();
                let warnings = data["warnings"].as_array().expect("data.warnings");
                for note in &notes {
                    assert!(
                        warnings.iter().any(|w| w == note.as_str()),
                        "{label}: {note}"
                    );
                }
            }

            // A failing sync adds the same notes to its error.
            index_local_object(format!("pi-26 failing {label}\n").as_bytes()).await;
            let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
            mock.fail_once_when_sql_contains(
                "INSERT INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced, object_format)",
                "injected upload failure",
            );
            let (concluded, _, _) =
                drive_sync_in_mode(&mode, &mock, Arc::new(InMemory::new()), &tuning).await;
            let detail = concluded.expect_err("the failing sync fails").to_string();
            let runs = spool_runs();
            assert_eq!(runs.len(), 3, "{label}: {runs:?}");
            for run in &runs {
                let path = run.display().to_string();
                let at = detail
                    .find(&path)
                    .unwrap_or_else(|| panic!("{label}: the error names {path}: {detail}"));
                // This run's note ends where a note naming another run
                // directory begins.
                let rest = &detail[at + path.len()..];
                let end = runs
                    .iter()
                    .filter(|other| *other != run)
                    .filter_map(|other| rest.find(&other.display().to_string()))
                    .min()
                    .unwrap_or(rest.len());
                let note = &rest[..end];
                assert!(note.contains(PI26_CATALOG_LISTING), "{label}: {note}");
                for absent in ["objects.jsonl", "refs.json", "repository.json"] {
                    assert_eq!(
                        note.contains(absent),
                        run != &live,
                        "{label}: {path} lists {absent} exactly when it holds it: {note}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_catalog_rechecks_the_generation_before_publishing() {
        let _repo = enter_isolated_libra_repo().await;
        seed_agent_capture_checkpoint(0).await;
        // A whole sync. The first object read from R2 is the agent-capture
        // phase's remote object check: after its first generation gate, and
        // before the recheck that precedes the first remote catalog
        // mutation. Only an unrelated object is indexed there and left
        // unsynced — no catalog row changes — so only the generation gate
        // repeated before publication can refuse it.
        let store = Arc::new(ObjectReadHookR2 {
            inner: Arc::new(InMemory::new()),
            hook: std::sync::Mutex::new(Some(Box::pin(async {
                index_local_object(b"pi-26 unrelated during the remote object check\n").await;
            }))),
        });
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = super::sync::run_cloud_sync_from_snapshot(
            CloudSyncContext {
                batch_size: 2,
                force: false,
            },
            &SilentCloudSyncProgress,
            mock.client(),
            Some(RemoteStorage::new(store.clone())),
            &super::snapshot::SnapshotTuning::default(),
        )
        .await
        .expect("sync");
        assert!(
            store.hook.lock().unwrap().is_none(),
            "the remote object check read an object"
        );
        assert_eq!(report.failed_count, 0);
        refused_with(
            "unsynced object during the remote checks",
            &report,
            "outside the completed object upload generation",
        );
        assert_no_remote_catalog("unsynced object during the remote checks", &mock).await;
    }

    /// R2 that runs `hook` once, at the first object read that is neither a
    /// HEAD nor the metadata JSON (PI-26).
    struct ObjectReadHookR2 {
        inner: Arc<InMemory>,
        hook: std::sync::Mutex<Option<futures::future::BoxFuture<'static, ()>>>,
    }

    impl std::fmt::Debug for ObjectReadHookR2 {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("ObjectReadHookR2")
        }
    }

    impl std::fmt::Display for ObjectReadHookR2 {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("ObjectReadHookR2")
        }
    }

    #[async_trait::async_trait]
    impl object_store::ObjectStore for ObjectReadHookR2 {
        async fn put_opts(
            &self,
            location: &object_store::path::Path,
            payload: object_store::PutPayload,
            opts: object_store::PutOptions,
        ) -> object_store::Result<object_store::PutResult> {
            self.inner.put_opts(location, payload, opts).await
        }

        async fn put_multipart_opts(
            &self,
            location: &object_store::path::Path,
            opts: object_store::PutMultipartOptions,
        ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
            self.inner.put_multipart_opts(location, opts).await
        }

        async fn get_opts(
            &self,
            location: &object_store::path::Path,
            options: object_store::GetOptions,
        ) -> object_store::Result<object_store::GetResult> {
            let hook = if options.head || location.as_ref().ends_with("metadata.json") {
                None
            } else {
                self.hook.lock().unwrap().take()
            };
            if let Some(hook) = hook {
                hook.await;
            }
            self.inner.get_opts(location, options).await
        }

        fn list(
            &self,
            prefix: Option<&object_store::path::Path>,
        ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>>
        {
            self.inner.list(prefix)
        }

        async fn list_with_delimiter(
            &self,
            prefix: Option<&object_store::path::Path>,
        ) -> object_store::Result<object_store::ListResult> {
            self.inner.list_with_delimiter(prefix).await
        }

        async fn copy_opts(
            &self,
            from: &object_store::path::Path,
            to: &object_store::path::Path,
            options: object_store::CopyOptions,
        ) -> object_store::Result<()> {
            self.inner.copy_opts(from, to, options).await
        }

        fn delete_stream(
            &self,
            locations: futures::stream::BoxStream<
                'static,
                object_store::Result<object_store::path::Path>,
            >,
        ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::path::Path>>
        {
            self.inner.delete_stream(locations)
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_catalog_pages_every_table_by_key() {
        use sea_orm::{ConnectionTrait, Statement};

        let _repo = enter_isolated_libra_repo().await;
        let conn = db::get_db_conn_instance().await;
        let plan_of = |sql: String, values: Vec<sea_orm::Value>| {
            let conn = conn.clone();
            async move {
                conn.query_all_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    format!("EXPLAIN QUERY PLAN {sql}"),
                    values,
                ))
                .await
                .expect("explain a page query")
                .iter()
                .map(|row| row.try_get_by::<String, _>("detail").expect("plan detail"))
                .collect::<Vec<String>>()
            }
        };

        // Every page of every catalog table is one indexed keyset statement:
        // the index provides the key order (no sort of the table), wherever
        // the page starts (no OFFSET).
        for query in super::agent_capture::CATALOG_QUERIES {
            for after in [false, true] {
                let mut values: Vec<sea_orm::Value> = Vec::new();
                if after {
                    for column in query.key {
                        values.push(match column {
                            super::agent_capture::KeyColumn::Text(_) => "key".into(),
                            super::agent_capture::KeyColumn::Integer(_) => 1_i64.into(),
                        });
                    }
                }
                values.push(256_i64.into());
                let plan = plan_of(
                    super::agent_capture::catalog_query_sql(query, after, true),
                    values,
                )
                .await;
                assert!(
                    !plan.iter().any(|step| step.contains("TEMP B-TREE")),
                    "{} after={after}: {plan:?}",
                    query.label
                );
                assert!(
                    plan.first().is_some_and(|step| step.contains("INDEX")),
                    "{} after={after}: {plan:?}",
                    query.label
                );
            }
        }
        // The legacy Claude rows: one page of the revision table's primary
        // key per statement, filtered after it is read.
        for after in [false, true] {
            let mut values: Vec<sea_orm::Value> = Vec::new();
            if after {
                values.extend(["key".into(), "key".into(), 2_i64.into(), 1_i64.into()]);
            }
            values.push(256_i64.into());
            let plan = plan_of(super::agent_capture::claude_legacy_page_sql(after), values).await;
            assert!(
                !plan.iter().any(|step| step.contains("TEMP B-TREE")),
                "legacy after={after}: {plan:?}"
            );
            assert!(
                plan.iter()
                    .any(|step| step.contains("agent_coverage_revision")
                        && step.contains("INDEX sqlite_autoindex_agent_coverage_revision")),
                "legacy after={after}: {plan:?}"
            );
        }

        // A multi-page catalog: 600 sessions, 300 checkpoints whose creation
        // order runs against their id order, and 600 legacy Claude revisions
        // among 2,000 ordinary coverage revisions the legacy read skips.
        conn.execute_unprepared(
            "CREATE TABLE pi26_writer (n INTEGER NOT NULL);
             WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 599)
             INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state,
               working_dir, started_at, last_event_at, metadata_json, redaction_report)
             SELECT printf('pi26-s-%04d', i), 'claude_code', printf('pi26-p-%04d', i),
               'stopped', '/tmp/pi26-fixture', 1, 2, '{}', '{}' FROM n;
             WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 299)
             INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, tree_oid,
               metadata_blob_oid, traces_commit, created_at)
             SELECT printf('pi26-c-%04d', i), 'pi26-s-0000', 'committed', 'tree', 'meta',
               printf('commit-%04d', i), 1000 - i FROM n;
             WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 1999)
             INSERT INTO agent_coverage_revision (session_id, logical_turn_key,
               coverage_schema_version, revision, checkpoint_id, coverage_digest,
               completeness, source_channel, created_at)
             SELECT 'pi26-s-0001', 'claude-legacy:' || printf('cp-%04d', i), 2, 1,
               printf('cp-%04d', i), 'digest', 'complete', 'import', 1 FROM n WHERE i < 600
             UNION ALL
             SELECT 'pi26-s-0001', printf('turn-%04d', i), 2, 1,
               printf('cp-%04d', i), 'digest', 'complete', 'import', 1 FROM n;",
        )
        .await
        .expect("seed a multi-page catalog");

        // Another connection commits after every catalog page with a 200 ms
        // busy timeout; none of the writes waits on the read.
        let freeze_with_writer = || async {
            let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
            let recorded = writes.clone();
            let hook: super::agent_capture::CatalogPageHook = Arc::new(move || {
                let recorded = recorded.clone();
                Box::pin(async move {
                    let other = other_connection().await;
                    let result = other
                        .execute_unprepared("INSERT INTO pi26_writer (n) VALUES (1)")
                        .await
                        .map(|_| ())
                        .map_err(|error| error.to_string());
                    let _ = other.close().await;
                    recorded.lock().unwrap().push(result);
                })
            });
            let frozen = match super::agent_capture::freeze_agent_capture(&conn, Some(&hook)).await
            {
                super::agent_capture::FrozenAgentCapture::Catalog(catalog) => catalog,
                other => panic!("the catalog freezes: {other:?}"),
            };
            let writes = writes.lock().unwrap().clone();
            for write in &writes {
                write
                    .as_ref()
                    .unwrap_or_else(|error| panic!("a concurrent writer was blocked: {error}"));
            }
            (frozen, writes.len())
        };
        let (frozen, writes) = freeze_with_writer().await;
        // Sessions take three pages, checkpoints two, the 2,600 coverage
        // revisions eleven, every other table one.
        assert!(writes >= 3 + 2 + 11 + 4, "{writes} pages");
        assert_eq!(frozen.sessions.len(), 600, "every session once");
        let checkpoints: Vec<&str> = frozen
            .checkpoints
            .iter()
            .map(|row| row.checkpoint_id.as_str())
            .collect();
        let expected: Vec<String> = (0..300).rev().map(|i| format!("pi26-c-{i:04}")).collect();
        assert_eq!(checkpoints, expected, "checkpoints in creation order");
        let rows = serde_json::to_value(&frozen.claude_legacy_rows).expect("serialize the rows");
        let legacy: Vec<&str> = rows
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| row["checkpoint_id"].as_str().expect("checkpoint id"))
            .collect();
        let expected: Vec<String> = (0..600).map(|i| format!("cp-{i:04}")).collect();
        assert_eq!(legacy, expected, "every legacy row once, in key order");

        // No legacy row at all among 2,000 ordinary ones: still one page of
        // the revision table per statement, and writers still land.
        conn.execute_unprepared(
            "DELETE FROM agent_coverage_revision WHERE logical_turn_key LIKE 'claude-legacy:%'",
        )
        .await
        .expect("drop the legacy rows");
        let (frozen, writes) = freeze_with_writer().await;
        assert!(writes >= 3 + 2 + 8 + 4, "{writes} pages");
        assert!(frozen.claude_legacy_rows.is_empty());
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_catalog_recheck_reads_one_consistent_view() {
        use crate::internal::ai::checkpoint_reader::test_helpers::RG03_CHECKPOINT_ID;

        // An erasure committed once the re-read has passed the session,
        // checkpoint and tombstone pages: the pages read after it alone look
        // unchanged, so only the read window catches it — whether the writer
        // is another process (a second connection) or another task of this
        // process committing through the shared connection, which `PRAGMA
        // data_version` alone would not reveal.
        let erase = format!(
            "DELETE FROM agent_checkpoint WHERE checkpoint_id = '{RG03_CHECKPOINT_ID}';
             DELETE FROM agent_session WHERE session_id = '{PI26_SESSION}';
             INSERT INTO agent_import_tombstone (tombstone_id, agent_kind, provider_session_id,
               erased_session_id, erased_at)
             VALUES ('pi26-erased', 'claude_code', '{PI26_SESSION}-provider', '{PI26_SESSION}', 5);"
        );
        for shared in [false, true] {
            let _repo = enter_isolated_libra_repo().await;
            let repo_id = ensure_repo_id().await;
            seed_agent_capture_checkpoint(1).await;
            let conn = db::get_db_conn_instance().await;
            let frozen = frozen_catalog(&conn).await;
            super::agent_capture::recheck_frozen_agent_capture(&frozen, &conn, &repo_id, None)
                .await
                .expect("an unchanged catalog passes the recheck");
            let pages = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counted = pages.clone();
            let erase = erase.clone();
            let hook: super::agent_capture::CatalogPageHook = Arc::new(move || {
                let (counted, erase) = (counted.clone(), erase.clone());
                Box::pin(async move {
                    if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 != 4 {
                        return;
                    }
                    if shared {
                        use sea_orm::ConnectionTrait;

                        db::get_db_conn_instance()
                            .await
                            .execute_unprepared(&erase)
                            .await
                            .expect("erase through the shared connection");
                    } else {
                        run_on_other_connection(&erase).await;
                    }
                })
            });
            let error = super::agent_capture::recheck_frozen_agent_capture(
                &frozen,
                &conn,
                &repo_id,
                Some(&hook),
            )
            .await
            .expect_err("an erasure during the re-read refuses publication");
            assert!(
                error.to_string().contains("catalog changed"),
                "shared={shared}: {error}"
            );
            assert!(
                pages.load(std::sync::atomic::Ordering::SeqCst) > 4,
                "shared={shared}: pages were read after the erasure"
            );
        }
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn capture_snapshot_catalog_spools_only_the_cloud_projection() {
        use sea_orm::{ConnectionTrait, Statement};

        let _repo = enter_isolated_libra_repo().await;
        let repo_id = ensure_repo_id().await;
        let conn = db::get_db_conn_instance().await;
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state,
               working_dir, started_at, last_event_at, metadata_json, redaction_report)
             VALUES ('pi26-legacy', 'claude_code', 'pi26-legacy-provider', 'stopped',
               '/tmp/pi26-fixture', 1, 2, ?, '{}')",
            [legacy_import_ownership_metadata(Some(1)).into()],
        ))
        .await
        .expect("insert a session with legacy ownership metadata");
        index_local_object(b"pi-26 projection object\n").await;
        let spooled = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = spooled.clone();
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(Arc::new(move |dir: PathBuf| {
                recorded
                    .lock()
                    .unwrap()
                    .push(fs::read_to_string(dir.join("catalog.json")).expect("catalog.json"));
                Box::pin(async {})
            })),
            ..Default::default()
        };
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        let spooled = spooled.lock().unwrap().clone();
        assert!(spooled[0].contains("pi26-legacy"), "{}", spooled[0]);
        for value in LEGACY_OWNERSHIP_VALUES {
            assert!(
                !spooled[0].contains(value),
                "the spool holds only the cloud projection: {value}"
            );
        }
        assert!(
            matches!(
                report.agent_capture,
                AgentCaptureSyncOutcome::Completed {
                    sessions_synced: 1,
                    ..
                }
            ),
            "{:?}",
            report.agent_capture
        );
        let (sessions, _) = remote_catalog(&mock, &repo_id).await;
        assert_eq!(sessions, vec!["pi26-legacy"]);

        // A catalog file changed after the freeze is never used: the phase
        // fails with a fixed reason and publishes nothing.
        let tuning = super::snapshot::SnapshotTuning {
            after_freeze: Some(Arc::new(|dir: PathBuf| {
                let path = dir.join("catalog.json");
                let mut bytes = fs::read(&path).expect("catalog.json");
                let at = bytes.len() / 2;
                bytes[at] ^= 1;
                fs::write(&path, bytes).expect("change the frozen catalog");
                Box::pin(async {})
            })),
            ..Default::default()
        };
        index_local_object(b"pi-26 tampered spool object\n").await;
        let mock = crate::utils::d1_client::test_support::MockD1::spawn().await;
        let report = sync_with_tuning(&mock, &Arc::new(InMemory::new()), &tuning)
            .await
            .expect("sync");
        assert_eq!(
            report.agent_capture,
            AgentCaptureSyncOutcome::Failed {
                error: super::sync::FROZEN_CATALOG_UNREADABLE.to_string()
            }
        );
        assert_no_remote_catalog("changed spool", &mock).await;
    }

    async fn enter_isolated_libra_repo() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        ScopedEnvVar,
        ScopedEnvVar,
        ChangeDirGuard,
    ) {
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let home_env = ScopedEnvVar::set("HOME", home.path());
        let test_home_env = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        setup_with_new_libra_in(repo.path()).await;
        let cwd = ChangeDirGuard::new(repo.path());
        // The snapshot spool requires a private `.libra` on Windows (PI-09).
        #[cfg(windows)]
        make_private_with_icacls(&util::storage_path());
        (repo, home, home_env, test_home_env, cwd)
    }

    struct ClearedEnvVarGuard {
        key: String,
        previous: Option<OsString>,
    }

    impl ClearedEnvVarGuard {
        fn new(key: &str) -> Self {
            let previous = env::var_os(key);
            // SAFETY: unit tests mutate process env in a controlled serial context.
            unsafe {
                env::remove_var(key);
            }
            Self {
                key: key.to_string(),
                previous,
            }
        }
    }

    impl Drop for ClearedEnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: this restores the exact previous value for the same env key.
            unsafe {
                if let Some(value) = &self.previous {
                    env::set_var(&self.key, value);
                } else {
                    env::remove_var(&self.key);
                }
            }
        }
    }

    #[test]
    fn test_restore_args_repo_id() {
        let args = RestoreArgs::try_parse_from(["restore", "--repo-id", "123"]).unwrap();
        assert_eq!(args.repo_id, Some("123".to_string()));
        assert_eq!(args.name, None);
    }

    #[test]
    fn test_restore_args_name() {
        let args = RestoreArgs::try_parse_from(["restore", "--name", "test-repo"]).unwrap();
        assert_eq!(args.name, Some("test-repo".to_string()));
        assert_eq!(args.repo_id, None);
    }

    #[test]
    fn test_restore_args_missing() {
        let result = RestoreArgs::try_parse_from(["restore"]);
        assert!(result.is_err());
    }

    #[test]
    fn cloud_cli_error_maps_missing_env_to_auth_missing_credentials() {
        let err = cloud_cli_error(
            "sync",
            "Cloud backup requires D1 + R2 configuration. Missing: LIBRA_D1_API_TOKEN, LIBRA_STORAGE_BUCKET".to_string(),
        );
        assert_eq!(err.stable_code(), StableErrorCode::AuthMissingCredentials);
        assert_eq!(
            err.details().get("missing_keys"),
            Some(&serde_json::json!([
                "LIBRA_D1_API_TOKEN",
                "LIBRA_STORAGE_BUCKET"
            ]))
        );
    }

    #[test]
    fn cloud_cli_error_maps_missing_repo_name_to_invalid_target() {
        let err = cloud_cli_error(
            "restore",
            "Repository with name 'demo' not found".to_string(),
        );
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidTarget);
    }

    #[test]
    fn cloud_cli_error_maps_d1_failure_to_network_protocol() {
        let err = cloud_cli_error("sync", "Failed to query D1: upstream timeout".to_string());
        assert_eq!(err.stable_code(), StableErrorCode::NetworkProtocol);
    }

    #[test]
    fn cloud_error_classifies_each_failure_shape() {
        assert_eq!(
            CloudError::from(
                "Cloud backup requires D1 + R2 configuration. Missing: A, B".to_string()
            ),
            CloudError::MissingEnv {
                detail: "Cloud backup requires D1 + R2 configuration. Missing: A, B".to_string(),
                missing_keys: vec!["A".to_string(), "B".to_string()],
                notes: Vec::new(),
            }
        );
        assert!(matches!(
            CloudError::from(
                "Repository name 'demo' already taken by another repository".to_string()
            ),
            CloudError::NameAlreadyTaken(_)
        ));
        assert!(matches!(
            CloudError::from("Repository with name 'demo' not found".to_string()),
            CloudError::NameNotFound(_)
        ));
        assert!(matches!(
            CloudError::from("2 objects failed to sync".to_string()),
            CloudError::PartialTransfer(_)
        ));
        assert!(matches!(
            CloudError::from("1 objects failed to restore".to_string()),
            CloudError::PartialTransfer(_)
        ));
        assert!(matches!(
            CloudError::from("Failed to query D1: timeout".to_string()),
            CloudError::D1(_)
        ));
        assert!(matches!(
            CloudError::from("R2 PUT failed".to_string()),
            CloudError::R2(_)
        ));
        assert!(matches!(
            CloudError::from("something unexpected".to_string()),
            CloudError::Generic(_)
        ));
    }

    #[test]
    fn cloud_error_into_cli_error_attaches_stable_codes() {
        assert_eq!(
            CloudError::MissingEnv {
                detail: "Cloud backup requires D1 + R2 configuration. Missing: KEY".to_string(),
                missing_keys: vec!["KEY".to_string()],
                notes: Vec::new(),
            }
            .into_cli_error("sync")
            .stable_code(),
            StableErrorCode::AuthMissingCredentials
        );
        assert_eq!(
            CloudError::NameAlreadyTaken("x".to_string())
                .into_cli_error("sync")
                .stable_code(),
            StableErrorCode::ConflictOperationBlocked
        );
        assert_eq!(
            CloudError::NameNotFound("x".to_string())
                .into_cli_error("restore")
                .stable_code(),
            StableErrorCode::CliInvalidTarget
        );
        assert_eq!(
            CloudError::PartialTransfer("x".to_string())
                .into_cli_error("sync")
                .stable_code(),
            StableErrorCode::ConflictOperationBlocked
        );
        assert_eq!(
            CloudError::D1("x".to_string())
                .into_cli_error("sync")
                .stable_code(),
            StableErrorCode::NetworkProtocol
        );
        assert_eq!(
            CloudError::R2("x".to_string())
                .into_cli_error("sync")
                .stable_code(),
            StableErrorCode::NetworkUnavailable
        );
    }

    /// Regression: `cloud_cli_error("sync", "N objects failed to sync")` and the
    /// equivalent typed-path `cloud_cli_error_typed("sync", CloudError::
    /// PartialTransfer(...))` must produce identical envelopes — same stable
    /// code, same message, same `details` map. Locks in the v0.17.209
    /// `cloud_cli_error_typed` cleanup against future drift.
    #[test]
    fn cloud_cli_error_string_and_typed_paths_produce_identical_envelope() {
        let from_string = cloud_cli_error("sync", "3 objects failed to sync".to_string());
        let from_variant = cloud_cli_error_typed(
            "sync",
            CloudError::PartialTransfer("3 objects failed to sync".to_string()),
        );
        assert_eq!(from_string.stable_code(), from_variant.stable_code());
        assert_eq!(from_string.message(), from_variant.message());
        assert_eq!(from_string.details(), from_variant.details());
    }

    #[test]
    fn cloud_sync_output_maps_synced_and_completed_outcomes() {
        let report = CloudSyncReport {
            repo_id: "repo-1".to_string(),
            project_name: "project-1".to_string(),
            total_unsynced: 4,
            synced_count: 4,
            failed_count: 0,
            metadata: MetadataSyncOutcome::Synced { references: 3 },
            agent_capture: AgentCaptureSyncOutcome::Completed {
                sessions_synced: 2,
                sessions_failed: 0,
                checkpoints_synced: 5,
                checkpoints_failed: 0,
            },
            spool_warnings: Vec::new(),
        };

        let output = cloud_sync_output_from_report(&report);
        assert_eq!(output.repo_id, "repo-1");
        assert_eq!(output.project_name, "project-1");
        assert_eq!(output.total_unsynced, 4);
        assert_eq!(output.synced_count, 4);
        assert_eq!(output.failed_count, 0);
        assert_eq!(output.metadata.status, "synced");
        assert_eq!(output.metadata.references, Some(3));
        assert_eq!(output.agent_capture.status, "completed");
        assert_eq!(output.agent_capture.sessions_synced, Some(2));
        assert_eq!(output.agent_capture.sessions_failed, Some(0));
        assert_eq!(output.agent_capture.checkpoints_synced, Some(5));
        assert_eq!(output.agent_capture.checkpoints_failed, Some(0));
        assert!(output.agent_capture.error.is_none());
        let wire = serde_json::to_value(&output).expect("serialize successful cloud sync");
        assert!(
            wire["agent_capture"].get("error").is_none(),
            "the established success JSON shape omits an empty error member"
        );
    }

    #[test]
    fn cloud_sync_output_maps_skipped_and_failed_outcomes() {
        const REMOTE_SENTINEL: &str = "REMOTE_D1_SOURCE_KEY=/private/provider/session.jsonl";
        let d1_reason = agent_capture_sync_failure_reason(&cloud_d1_failure(
            "list agent-capture catalog before sync",
            &D1Error {
                code: 7500,
                message: REMOTE_SENTINEL.to_string(),
            },
        ));
        let row_reason = agent_capture_sync_failure_reason(&agent_capture_row_failure(
            "sync agent-session batch",
            &D1Error {
                code: 7500,
                message: REMOTE_SENTINEL.to_string(),
            },
        ));
        let validation_reason = agent_capture_sync_failure_reason(&CloudError::Generic(
            "remote subagent companion has an invalid source commitment".to_string(),
        ));
        let env_reason = agent_capture_sync_failure_reason(&CloudError::MissingEnv {
            detail: REMOTE_SENTINEL.to_string(),
            missing_keys: vec![REMOTE_SENTINEL.to_string()],
            notes: Vec::new(),
        });
        for (reason, expected) in [
            (
                d1_reason,
                "cloud D1 list agent-capture catalog before sync failed (D1 code 7500); verify cloud connectivity and credentials, then retry",
            ),
            (row_reason, AGENT_CAPTURE_SYNC_FAILURE_MESSAGE),
            (
                validation_reason,
                "remote subagent companion has an invalid source commitment",
            ),
            (env_reason, AGENT_CAPTURE_SYNC_FAILURE_MESSAGE),
        ] {
            let report = CloudSyncReport {
                repo_id: "repo-2".to_string(),
                project_name: "project-2".to_string(),
                total_unsynced: 0,
                synced_count: 0,
                failed_count: 0,
                metadata: MetadataSyncOutcome::Skipped,
                agent_capture: AgentCaptureSyncOutcome::Failed { error: reason },
                spool_warnings: Vec::new(),
            };

            let output = cloud_sync_output_from_report(&report);
            assert_eq!(output.metadata.status, "skipped");
            assert!(output.metadata.references.is_none());
            assert_eq!(output.agent_capture.status, "failed");
            assert_eq!(output.agent_capture.error.as_deref(), Some(expected));
            assert!(output.agent_capture.sessions_synced.is_none());
            assert!(output.agent_capture.sessions_failed.is_none());
            assert!(output.agent_capture.checkpoints_synced.is_none());
            assert!(output.agent_capture.checkpoints_failed.is_none());
            let wire = serde_json::to_value(&output).expect("serialize failed cloud sync");
            let rendered = wire.to_string();
            assert_eq!(wire["agent_capture"]["error"], expected);
            assert!(
                !rendered.contains(REMOTE_SENTINEL),
                "cloud-sync JSON must not serialize remote failure text: {rendered}"
            );
            let cli = cloud_cli_error_typed("sync", agent_capture_mirror_failure(expected));
            assert_eq!(
                cli.message(),
                format!("agent capture mirror failed: {expected}")
            );
            assert_eq!(cli.stable_code(), StableErrorCode::ConflictOperationBlocked);
        }
    }

    #[test]
    fn cloud_d1_failure_redacts_remote_text_from_cli_error() {
        const REMOTE_SENTINEL: &str = "REMOTE_D1_SOURCE_KEY=/private/provider/session.jsonl";
        let failure = cloud_d1_failure(
            "sync agent-capture catalog",
            &D1Error {
                code: 3999,
                message: REMOTE_SENTINEL.to_string(),
            },
        );
        let cli = cloud_cli_error_typed("sync", failure);
        assert!(
            !cli.message().contains(REMOTE_SENTINEL),
            "human CLI error must not echo a D1-controlled message: {}",
            cli.message()
        );
        assert!(
            !format!("{:?}", cli.details()).contains(REMOTE_SENTINEL),
            "structured CLI details must not retain a D1-controlled message"
        );
        assert!(cli.message().contains("D1 code 3999"));
    }

    #[test]
    fn local_agent_capture_failure_redacts_database_text_from_cli_error() {
        const LOCAL_SENTINEL: &str = "LOCAL_SQLITE_ROW=/private/provider/session-capture.jsonl";
        let failure = local_agent_capture_failure(
            "decode local agent-session snapshot",
            std::io::Error::other(LOCAL_SENTINEL),
        );
        let detail = failure.to_string();
        let cli = cloud_cli_error_typed("sync", failure);
        assert!(detail.contains("decode local agent-session snapshot"));
        assert!(
            !detail.contains(LOCAL_SENTINEL),
            "CloudError must not echo a database error payload: {detail}"
        );
        assert!(
            !cli.message().contains(LOCAL_SENTINEL),
            "human CLI error must not echo a database error payload"
        );
        assert!(
            !format!("{:?}", cli.details()).contains(LOCAL_SENTINEL),
            "structured CLI details must not retain a database error payload"
        );
    }

    /// Scenario: metadata restore into a freshly initialized repo where local refs
    /// have `remote = NULL`. This is the edge hit by live cloud restore: SQL
    /// `remote = NULL` does not match existing rows, so the restore must use
    /// `IS NULL` and update the existing HEAD/branch rows instead of inserting
    /// duplicates that leave HEAD pointing at the init-time repository state.
    #[test]
    #[serial(cwd, env)]
    fn restore_metadata_updates_existing_null_remote_references() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let restored_commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
            let restored_refs = vec![
                reference::Model {
                    id: 0,
                    name: Some("restored-main".to_string()),
                    kind: reference::ConfigKind::Head,
                    commit: None,
                    remote: None,
                    worktree_id: None,
                },
                reference::Model {
                    id: 0,
                    name: Some("intent".to_string()),
                    kind: reference::ConfigKind::Branch,
                    commit: Some(restored_commit.clone()),
                    remote: None,
                    worktree_id: None,
                },
            ];
            let remote = RemoteStorage::new(Arc::new(InMemory::new()));
            let metadata = serde_json::to_vec(&restored_refs).unwrap();
            remote.put_metadata(&metadata).await.unwrap();

            restore_metadata(&db_conn, &remote)
                .await
                .expect("metadata restore should update existing NULL-remote refs");

            let heads = reference::Entity::find()
                .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
                .filter(reference::Column::Remote.is_null())
                .all(&db_conn)
                .await
                .unwrap();
            assert_eq!(heads.len(), 1);
            assert_eq!(heads[0].name.as_deref(), Some("restored-main"));

            let intent_refs = reference::Entity::find()
                .filter(reference::Column::Kind.eq(reference::ConfigKind::Branch))
                .filter(reference::Column::Name.eq("intent"))
                .filter(reference::Column::Remote.is_null())
                .all(&db_conn)
                .await
                .unwrap();
            assert_eq!(intent_refs.len(), 1);
            assert_eq!(intent_refs[0].commit.as_ref(), Some(&restored_commit));
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn restore_metadata_never_moves_generation_fenced_traces_ref() {
        let rt = tokio::runtime::Runtime::new().expect("create test runtime");
        let repo = tempdir().expect("create repo tempdir");
        let home = tempdir().expect("create home tempdir");
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let local_commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
            let stale_remote_commit = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
            db_conn
                .execute_raw(sea_orm::Statement::from_sql_and_values(
                    db_conn.get_database_backend(),
                    "UPDATE reference SET `commit` = ?
                     WHERE name = ? AND kind = 'Branch' AND remote IS NULL",
                    [
                        local_commit.into(),
                        crate::internal::branch::TRACES_BRANCH.into(),
                    ],
                ))
                .await
                .expect("seed local traces ref");
            let metadata = vec![reference::Model {
                id: 0,
                name: Some(crate::internal::branch::TRACES_BRANCH.to_string()),
                kind: reference::ConfigKind::Branch,
                commit: Some(stale_remote_commit.to_string()),
                remote: None,
                worktree_id: None,
            }];

            let deferred = restore_metadata_models(&db_conn, metadata, false)
                .await
                .expect("generic metadata restore skips the capture ref");
            assert_eq!(deferred.len(), 1);
            restore_legacy_capture_refs_if_unowned(
                &db_conn,
                deferred,
                AgentCaptureRestoreOutcome::GenerationInstalled,
            )
            .await
            .expect("validated capture generation owns the traces ref");

            let row = db_conn
                .query_one_raw(sea_orm::Statement::from_sql_and_values(
                    db_conn.get_database_backend(),
                    "SELECT `commit` FROM reference
                     WHERE name = ? AND kind = 'Branch' AND remote IS NULL",
                    [crate::internal::branch::TRACES_BRANCH.into()],
                ))
                .await
                .expect("query local traces ref")
                .expect("local traces ref remains present");
            assert_eq!(
                row.try_get_by::<String, _>("commit")
                    .expect("decode traces commit"),
                local_commit
            );
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn restore_metadata_reinstates_legacy_traces_ref_without_a_generation() {
        let rt = tokio::runtime::Runtime::new().expect("create test runtime");
        let repo = tempdir().expect("create repo tempdir");
        let home = tempdir().expect("create home tempdir");
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let legacy_commit = "cccccccccccccccccccccccccccccccccccccccc";
            let metadata = vec![reference::Model {
                id: 0,
                name: Some(crate::internal::branch::TRACES_BRANCH.to_string()),
                kind: reference::ConfigKind::Branch,
                commit: Some(legacy_commit.to_string()),
                remote: None,
                worktree_id: None,
            }];
            let deferred = restore_metadata_models(&db_conn, metadata, false)
                .await
                .expect("defer legacy capture ref until generation validation");
            restore_legacy_capture_refs_if_unowned(
                &db_conn,
                deferred,
                AgentCaptureRestoreOutcome::NoGeneration,
            )
            .await
            .expect("legacy metadata owns traces when no generation exists");
            let restored = db_conn
                .query_one_raw(sea_orm::Statement::from_sql_and_values(
                    db_conn.get_database_backend(),
                    "SELECT `commit` FROM reference
                     WHERE name = ? AND kind = 'Branch' AND remote IS NULL",
                    [crate::internal::branch::TRACES_BRANCH.into()],
                ))
                .await
                .expect("query restored legacy traces ref")
                .expect("legacy traces ref row")
                .try_get_by::<String, _>("commit")
                .expect("decode legacy traces commit");
            assert_eq!(restored, legacy_commit);
        });
    }

    #[test]
    fn checkpoint_prune_preflight_rejects_a_remote_match() {
        const REMOTE_SENTINEL: &str =
            "REMOTE_PRUNED_CHECKPOINT=/private/provider/session-capture.jsonl";
        let local = vec![REMOTE_SENTINEL.to_string()];
        let remote = HashSet::from([REMOTE_SENTINEL.to_string()]);
        let error = reject_local_prune_conflicts(&local, &remote)
            .expect_err("matching completed remote checkpoint must fail preflight");
        let message = error.to_string();
        let cli = cloud_cli_error_typed("restore", error);
        assert!(
            !message.contains(REMOTE_SENTINEL),
            "CloudError must not echo a remote checkpoint id: {message}"
        );
        assert!(
            !cli.message().contains(REMOTE_SENTINEL),
            "human CLI error must not echo a remote checkpoint id"
        );
        assert!(
            !format!("{:?}", cli.details()).contains(REMOTE_SENTINEL),
            "structured CLI details must not retain a remote checkpoint id"
        );
        assert!(message.contains("libra cloud sync"));
        assert!(message.contains("already pruned locally"));
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn cloud_restore_indexed_objects_downloads_skips_and_verifies_hash() {
        let _repo = enter_isolated_libra_repo().await;
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let local_dir = tempdir().unwrap();
        let local = LocalStorage::try_new(local_dir.path().to_path_buf()).expect("storage");
        let data = b"hello\n";
        let hash = ObjectHash::from_type_and_data(ObjectType::Blob, data);
        let row = test_object_index_row(hash, data.len() as i64);

        remote
            .put(&hash, data, ObjectType::Blob)
            .await
            .expect("test object should upload to in-memory remote");

        let report = restore_indexed_objects_from_remote(
            &[row],
            &remote,
            &local,
            git_internal::hash::HashKind::Sha1,
        )
        .await
        .expect("restore should download a valid remote object");

        assert_eq!(report.downloaded, 1);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.failed, 0);
        assert!(report.warnings.is_empty());
        assert!(local.exist(&hash).await);

        let row = test_object_index_row(hash, data.len() as i64);
        let report = restore_indexed_objects_from_remote(
            &[row],
            &remote,
            &local,
            git_internal::hash::HashKind::Sha1,
        )
        .await
        .expect("restore should skip an existing local object");

        assert_eq!(report.downloaded, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.failed, 0);
        assert!(report.warnings.is_empty());
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn cloud_restore_indexed_objects_reports_hash_mismatch() {
        let _repo = enter_isolated_libra_repo().await;
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let local_dir = tempdir().unwrap();
        let local = LocalStorage::try_new(local_dir.path().to_path_buf()).expect("storage");
        let expected_data = b"expected\n";
        let wrong_data = b"wrong\n";
        let expected_hash = ObjectHash::from_type_and_data(ObjectType::Blob, expected_data);
        let row = test_object_index_row(expected_hash, expected_data.len() as i64);

        remote
            .put(&expected_hash, wrong_data, ObjectType::Blob)
            .await
            .expect("test object should upload under the expected key");

        let report = restore_indexed_objects_from_remote(
            &[row],
            &remote,
            &local,
            git_internal::hash::HashKind::Sha1,
        )
        .await
        .expect("hash mismatch should be reported, not panic");

        assert_eq!(report.downloaded, 0);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(
            report.warnings[0],
            "warning: restored object hash does not match the cloud object index"
        );
        assert!(
            !report.warnings[0].contains(&expected_hash.to_string()),
            "hash-mismatch warning must not disclose the remote object id"
        );
        assert!(!local.exist(&expected_hash).await);
    }

    #[tokio::test]
    async fn capture_publication_replaces_and_verifies_corrupt_existing_remote_object() {
        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        let local_dir = tempdir().expect("create local object directory");
        let local = LocalStorage::try_new(local_dir.path().to_path_buf()).expect("storage");
        let expected = b"validated capture object\n";
        let hash = ObjectHash::from_type_and_data(ObjectType::Blob, expected);
        local
            .put(&hash, expected, ObjectType::Blob)
            .await
            .expect("seed validated local capture object");
        remote
            .put(&hash, b"corrupt payload\n", ObjectType::Blob)
            .await
            .expect("seed corrupt remote payload under expected key");

        publish_validated_agent_capture_object(&local, &remote, &hash.to_string(), &hash)
            .await
            .expect("publication must replace corrupt existing remote payload");

        let (restored, object_type) = remote
            .get(&hash)
            .await
            .expect("read back repaired remote object");
        assert_eq!(restored, expected);
        assert_eq!(ObjectHash::from_type_and_data(object_type, &restored), hash);
    }

    #[test]
    #[serial(cwd, env)]
    fn create_r2_storage_reads_values_from_local_config() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());
        let _endpoint = ClearedEnvVarGuard::new("LIBRA_STORAGE_ENDPOINT");
        let _bucket = ClearedEnvVarGuard::new("LIBRA_STORAGE_BUCKET");
        let _access = ClearedEnvVarGuard::new("LIBRA_STORAGE_ACCESS_KEY");
        let _secret = ClearedEnvVarGuard::new("LIBRA_STORAGE_SECRET_KEY");
        let _region = ClearedEnvVarGuard::new("LIBRA_STORAGE_REGION");

        let repo_db_path = repo.path().join(".libra").join(util::DATABASE);

        rt.block_on(crate::internal::vault::lazy_init_vault_for_scope("local"))
            .unwrap();

        let encrypted_endpoint = rt
            .block_on(crate::internal::config::encrypt_value(
                "https://storage.example.com",
                "local",
            ))
            .unwrap();
        let encrypted_bucket = rt
            .block_on(crate::internal::config::encrypt_value(
                "test-bucket",
                "local",
            ))
            .unwrap();
        let encrypted_access = rt
            .block_on(crate::internal::config::encrypt_value(
                "test-access",
                "local",
            ))
            .unwrap();
        let encrypted_secret = rt
            .block_on(crate::internal::config::encrypt_value(
                "test-secret",
                "local",
            ))
            .unwrap();
        let encrypted_region = rt
            .block_on(crate::internal::config::encrypt_value("auto", "local"))
            .unwrap();

        rt.block_on(async {
            ConfigKv::set(
                "vault.env.LIBRA_STORAGE_ENDPOINT",
                &encrypted_endpoint,
                true,
            )
            .await
            .unwrap();
            ConfigKv::set("vault.env.LIBRA_STORAGE_BUCKET", &encrypted_bucket, true)
                .await
                .unwrap();
            ConfigKv::set(
                "vault.env.LIBRA_STORAGE_ACCESS_KEY",
                &encrypted_access,
                true,
            )
            .await
            .unwrap();
            ConfigKv::set(
                "vault.env.LIBRA_STORAGE_SECRET_KEY",
                &encrypted_secret,
                true,
            )
            .await
            .unwrap();
            ConfigKv::set("vault.env.LIBRA_STORAGE_REGION", &encrypted_region, true)
                .await
                .unwrap();
        });

        let _manifest_dir = ChangeDirGuard::new(env!("CARGO_MANIFEST_DIR"));

        rt.block_on(create_r2_storage_for_db_path(
            "repo-from-config",
            &repo_db_path,
        ))
        .expect("R2 storage should initialize from local config values even after cwd drift");
    }

    /// Build a minimum-viable `AgentSessionV2Row` for restore-fixture tests.
    /// Defaults to a kind/state pair that satisfies the schema's CHECK
    /// constraints; tests override fields they care about.
    fn fixture_session_row(session_id: &str, provider_session_id: &str) -> AgentSessionV2Row {
        AgentSessionV2Row {
            session_id: session_id.to_string(),
            agent_kind: "claude_code".to_string(),
            provider_session_id: provider_session_id.to_string(),
            state: "active".to_string(),
            working_dir: "/tmp/fixture".to_string(),
            worktree_id: None,
            parent_commit: None,
            parent_session_id: None,
            metadata_json: "{}".to_string(),
            redaction_report: "{}".to_string(),
            started_at: 1_700_000_000,
            last_event_at: 1_700_000_001,
            stopped_at: None,
            schema_version: 1,
            sync_revision: 1,
        }
    }

    fn fixture_checkpoint_row(
        checkpoint_id: &str,
        session_id: &str,
        description: Option<&str>,
    ) -> AgentCheckpointV2Row {
        AgentCheckpointV2Row {
            checkpoint_id: checkpoint_id.to_string(),
            session_id: session_id.to_string(),
            parent_checkpoint_id: None,
            scope: "committed".to_string(),
            parent_commit: None,
            tree_oid: "0000000000000000000000000000000000000000".to_string(),
            metadata_blob_oid: "1111111111111111111111111111111111111111".to_string(),
            traces_commit: "2222222222222222222222222222222222222222".to_string(),
            tool_use_id: None,
            subagent_session_id: None,
            description: description.map(String::from),
            created_at: 1_700_000_010,
            sync_revision: 1,
        }
    }

    #[test]
    fn legacy_generation_zero_catalog_is_the_only_manifestless_bootstrap_ancestor() {
        let mut session = fixture_session_row("legacy-session", "legacy-provider-session");
        session.sync_revision = 0;
        let mut checkpoint = fixture_checkpoint_row("legacy-checkpoint", "legacy-session", None);
        checkpoint.sync_revision = 0;
        let rows = AgentCaptureRestoreCatalogRows {
            sessions: vec![session.clone()],
            checkpoints: vec![checkpoint],
            ..AgentCaptureRestoreCatalogRows::default()
        };

        assert!(remote_catalog_is_legacy_generation_zero_bootstrap(
            false, None, &rows
        ));
        assert!(
            should_publish_session(
                &fixture_session_row("legacy-session", "legacy-provider-session"),
                Some(&session),
                true,
            )
            .expect("revision-one local row replaces adopted generation zero")
        );
        assert!(!remote_catalog_is_legacy_generation_zero_bootstrap(
            true, None, &rows
        ));
        assert!(!remote_catalog_is_legacy_generation_zero_bootstrap(
            false,
            Some(0),
            &rows
        ));

        let mut current_only_rows = rows;
        current_only_rows.prune_tombstones = vec![AgentCheckpointPruneTombstoneRow {
            checkpoint_id: "pruned".to_string(),
            session_id: "legacy-session".to_string(),
            pruned_at: 1,
        }];
        assert!(!remote_catalog_is_legacy_generation_zero_bootstrap(
            false,
            None,
            &current_only_rows
        ));
    }

    #[test]
    fn abandoned_publishing_generation_remains_a_preflight_ancestor_of_its_base() {
        let generation = |generation, state: &str| AgentCaptureGenerationRow {
            repo_id: "repo".to_string(),
            generation,
            state: state.to_string(),
            writer_token: Some("writer".to_string()),
            object_index_digest: Some("digest".to_string()),
            object_index_count: Some(0),
            object_index_scope: Some("checkpoint_projection".to_string()),
            object_index_generation: Some(1),
            traces_head: None,
            started_at: 1,
            completed_at: (state == "complete").then_some(2),
        };

        assert!(remote_generation_is_known_ancestor(
            Some(&generation(8, "complete")),
            Some(8)
        ));
        assert!(remote_generation_is_known_ancestor(
            Some(&generation(9, "publishing")),
            Some(8)
        ));
        assert!(remote_generation_is_known_ancestor(
            Some(&generation(1, "publishing")),
            None
        ));
        assert!(!remote_generation_is_known_ancestor(
            Some(&generation(9, "publishing")),
            Some(7)
        ));
        assert!(!remote_generation_is_known_ancestor(
            Some(&generation(9, "complete")),
            Some(8)
        ));
        assert!(!remote_generation_is_known_ancestor(
            Some(&generation(9, "corrupt")),
            Some(8)
        ));
    }

    #[test]
    fn checkpoint_rewrite_generation_publishes_prune_and_fences_stale_clone() {
        let remote = fixture_checkpoint_row("ckpt-A", "sess-A", None);
        let mut rewritten = remote.clone();
        rewritten.tree_oid = "3333333333333333333333333333333333333333".to_string();
        rewritten.metadata_blob_oid = "5555555555555555555555555555555555555555".to_string();
        rewritten.traces_commit = "4444444444444444444444444444444444444444".to_string();
        rewritten.sync_revision = 2;

        assert!(
            should_publish_checkpoint(&rewritten, Some(&remote), true)
                .expect("newer prune rewrite publishes")
        );
        assert!(
            !should_publish_checkpoint(&remote, Some(&rewritten), false)
                .expect("older clone is fenced")
        );

        let mut corrupt = rewritten.clone();
        corrupt.description = Some("changed immutable identity".to_string());
        corrupt.sync_revision = 3;
        assert!(should_publish_checkpoint(&corrupt, Some(&rewritten), true).is_err());
    }

    fn fixture_subagent_rows() -> (
        AgentSubagentContentClaimRow,
        AgentSubagentContentRevisionRow,
        AgentSubagentLinkRow,
    ) {
        let source_key = format!("source/subagent-hmac-v2/{}", "a".repeat(64));
        let content_schema_version =
            crate::internal::ai::subagent_content::SUBAGENT_CONTENT_SCHEMA_VERSION;
        (
            AgentSubagentContentClaimRow {
                parent_session_id: "sess-A".to_string(),
                provider_kind: "claude_code".to_string(),
                source_key: source_key.clone(),
                content_schema_version,
                revision_cursor: 1,
                sync_revision: 1,
                current_revision: 1,
                current_checkpoint_id: Some("child-A".to_string()),
                current_digest: Some("digest-A".to_string()),
                fence_token: 3,
                created_at: 1_700_000_020,
                updated_at: 1_700_000_021,
            },
            AgentSubagentContentRevisionRow {
                parent_session_id: "sess-A".to_string(),
                provider_kind: "claude_code".to_string(),
                source_key,
                content_schema_version,
                revision: 1,
                checkpoint_id: "child-A".to_string(),
                content_digest: "digest-A".to_string(),
                source_channel: "import".to_string(),
                partial: 0,
                created_at: 1_700_000_020,
            },
            AgentSubagentLinkRow {
                content_checkpoint_id: "child-A".to_string(),
                parent_session_id: "sess-A".to_string(),
                link_state: "unresolved".to_string(),
                boundary_checkpoint_id: None,
                stable_subagent_id: None,
                sync_revision: 1,
                created_at: 1_700_000_020,
                updated_at: 1_700_000_021,
            },
        )
    }

    #[test]
    fn unmirrored_legacy_companions_stay_local_with_their_links() {
        let (v2_claim, v2_revision, v2_link) = fixture_subagent_rows();
        let legacy = |digit: &str, checkpoint: &str| {
            let source_key = format!("source/sha256/{}", digit.repeat(64));
            let mut claim = v2_claim.clone();
            claim.source_key = source_key.clone();
            claim.content_schema_version = 1;
            claim.current_checkpoint_id = Some(checkpoint.to_string());
            let mut revision = v2_revision.clone();
            revision.source_key = source_key;
            revision.content_schema_version = 1;
            revision.checkpoint_id = checkpoint.to_string();
            let mut link = v2_link.clone();
            link.content_checkpoint_id = checkpoint.to_string();
            (claim, revision, link)
        };
        let (local_claim, local_revision, local_link) = legacy("1", "legacy-local");
        let (claim_proven, revision_of_claim_proven, link_of_claim_proven) =
            legacy("2", "legacy-claim-proven");
        let (revision_proven_claim, revision_proven, link_of_revision_proven) =
            legacy("3", "legacy-revision-proven");
        let local_revisions = [
            v2_revision.clone(),
            local_revision.clone(),
            revision_of_claim_proven.clone(),
            revision_proven.clone(),
        ];
        let mut pending = PendingSubagentRows {
            claims: vec![
                v2_claim.clone(),
                local_claim,
                claim_proven.clone(),
                revision_proven_claim.clone(),
            ],
            revisions: local_revisions.to_vec(),
            links: vec![
                v2_link.clone(),
                local_link.clone(),
                link_of_claim_proven.clone(),
                link_of_revision_proven.clone(),
            ],
            pre_prune_links: vec![local_link],
        };

        // The remote proves a legacy source identity through its claim or
        // through one of its revisions; only the never-mirrored one stays local.
        let withheld = pending.withhold_unmirrored_legacy(
            &local_revisions,
            std::slice::from_ref(&claim_proven),
            std::slice::from_ref(&revision_proven),
        );
        assert_eq!(withheld, 4, "claim, revision, link and pre-prune link");
        assert_eq!(
            pending,
            PendingSubagentRows {
                claims: vec![v2_claim, claim_proven, revision_proven_claim],
                revisions: vec![v2_revision, revision_of_claim_proven, revision_proven],
                links: vec![v2_link, link_of_claim_proven, link_of_revision_proven],
                pre_prune_links: Vec::new(),
            }
        );
    }

    #[test]
    fn interrupted_remote_dependency_publication_is_resumable_but_not_restorable() {
        let mut checkpoint = fixture_checkpoint_row("child-A", "sess-A", Some("subagent"));
        checkpoint.scope = "subagent".to_string();
        let (claim, revision, link) = fixture_subagent_rows();
        validate_agent_capture_companions(
            std::slice::from_ref(&checkpoint),
            &[],
            std::slice::from_ref(&revision),
            std::slice::from_ref(&link),
            "staged remote",
            CompanionValidationMode::Publishing,
        )
        .expect("a fenced next sync can resume staged dependency rows");
        assert!(
            validate_agent_capture_companions(
                std::slice::from_ref(&checkpoint),
                &[],
                std::slice::from_ref(&revision),
                std::slice::from_ref(&link),
                "staged remote",
                CompanionValidationMode::Complete,
            )
            .is_err(),
            "restore must reject a generation before the current claim is durable"
        );
        validate_agent_capture_companions(
            &[checkpoint],
            &[claim],
            &[revision],
            &[link],
            "completed remote",
            CompanionValidationMode::Complete,
        )
        .expect("claim completion closes the staged generation");
    }

    #[test]
    fn every_checkpoint_prune_cleanup_boundary_is_resumable() {
        let mut checkpoint = fixture_checkpoint_row("child-A", "sess-A", Some("subagent"));
        checkpoint.scope = "subagent".to_string();
        let (claim, revision, link) = fixture_subagent_rows();

        for (claims, revisions, links, checkpoints) in [
            (
                std::slice::from_ref(&claim),
                std::slice::from_ref(&revision),
                std::slice::from_ref(&link),
                std::slice::from_ref(&checkpoint),
            ),
            (
                &[][..],
                std::slice::from_ref(&revision),
                std::slice::from_ref(&link),
                std::slice::from_ref(&checkpoint),
            ),
            (
                &[][..],
                &[][..],
                std::slice::from_ref(&link),
                std::slice::from_ref(&checkpoint),
            ),
            (&[][..], &[][..], &[][..], std::slice::from_ref(&checkpoint)),
            (&[][..], &[][..], &[][..], &[][..]),
        ] {
            validate_agent_capture_companions(
                checkpoints,
                claims,
                revisions,
                links,
                "interrupted prune",
                CompanionValidationMode::Publishing,
            )
            .expect("each ordered cleanup boundary must be safe for generation takeover");
        }

        let boundary = fixture_checkpoint_row("boundary-A", "sess-A", Some("boundary"));
        let mut resolved = link.clone();
        resolved.link_state = "resolved".to_string();
        resolved.boundary_checkpoint_id = Some(boundary.checkpoint_id.clone());
        resolved.stable_subagent_id = Some("stable-child-A".to_string());
        let mut unresolved = resolved;
        unresolved.link_state = "unresolved".to_string();
        unresolved.boundary_checkpoint_id = None;
        unresolved.sync_revision += 1;
        validate_agent_capture_companions(
            &[checkpoint.clone(), boundary.clone()],
            std::slice::from_ref(&claim),
            std::slice::from_ref(&revision),
            std::slice::from_ref(&unresolved),
            "pre-prune boundary unlink",
            CompanionValidationMode::Publishing,
        )
        .expect("unresolving the association before boundary deletion is resumable");
        validate_agent_capture_companions(
            &[checkpoint],
            &[claim],
            &[revision],
            &[unresolved],
            "post-prune boundary unlink",
            CompanionValidationMode::Publishing,
        )
        .expect("deleting an already-unlinked boundary is resumable");
    }

    #[test]
    fn capture_manifest_rejects_empty_catalog_with_nonempty_traces_head() {
        let checkpoint = fixture_checkpoint_row("ckpt-A", "sess-A", Some("first"));
        assert!(validate_agent_capture_traces_shape(&[], Some(&"a".repeat(40)), "remote").is_err());
        assert!(validate_agent_capture_traces_shape(&[checkpoint], None, "remote").is_err());
        validate_agent_capture_traces_shape(&[], None, "remote")
            .expect("an empty capture has no traces head");
    }

    /// PD-03: a session erased HERE must not come back from a mirror that
    /// has not yet seen the erasure.
    ///
    /// `restore` filtered only the tombstones the remote catalog carried,
    /// so the window between a local `agent erase` and the next `cloud
    /// sync` was a resurrection hole: restoring from the stale mirror
    /// re-UPSERTed the session. The schema triggers would have aborted the
    /// write — correct, but it takes the whole restore down with an opaque
    /// `RAISE(ABORT)` instead of quietly declining rows the user already
    /// erased. Both the session and its checkpoints must be dropped, and
    /// everything else must still restore.
    #[test]
    #[serial(cwd, env)]
    fn restore_skips_locally_tombstoned_sessions_and_their_checkpoints() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let erased = fixture_session_row("sess-ERASED", "prov-ERASED");
            let kept = fixture_session_row("sess-KEPT", "prov-KEPT");

            // The local tombstone `agent erase` leaves behind. Note the
            // mirror's session id need NOT equal `erased_session_id`, which
            // is why checkpoints are dropped by the id the ROWS carry.
            db_conn
                .execute_raw(sea_orm::Statement::from_sql_and_values(
                    db_conn.get_database_backend(),
                    "INSERT INTO agent_import_tombstone
                       (agent_kind, provider_session_id, erased_session_id, erased_at)
                     VALUES (?, ?, ?, ?)",
                    [
                        erased.agent_kind.clone().into(),
                        erased.provider_session_id.clone().into(),
                        "sess-LOCAL-ID".into(),
                        1_i64.into(),
                    ],
                ))
                .await
                .expect("write the local erasure tombstone");

            let sessions = vec![erased.clone(), kept.clone()];
            let checkpoints = vec![
                fixture_checkpoint_row("ckpt-ERASED", "sess-ERASED", Some("gone")),
                fixture_checkpoint_row("ckpt-KEPT", "sess-KEPT", Some("stays")),
            ];

            restore_agent_capture_from_rows_with_subagents(
                &db_conn,
                AgentCaptureRestoreRows {
                    sessions: &sessions,
                    checkpoints: &checkpoints,
                    claims: &[],
                    revisions: &[],
                    links: &[],
                    traces_head: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                    remote_is_known_ancestor: true,
                },
                true,
            )
            .await
            .expect("restore must SUCCEED, declining the erased rows rather than aborting");

            let restored_session = scalar_count(
                &db_conn,
                "SELECT COUNT(*) AS n FROM agent_session WHERE session_id = 'sess-ERASED'",
            )
            .await
            .unwrap();
            assert_eq!(
                restored_session, 0,
                "an erased session must not be resurrected by a stale mirror"
            );
            let restored_checkpoint = scalar_count(
                &db_conn,
                "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'ckpt-ERASED'",
            )
            .await
            .unwrap();
            assert_eq!(
                restored_checkpoint, 0,
                "nor may its checkpoints come back without it"
            );

            // Everything unrelated still restores: the filter is targeted,
            // not a blanket refusal.
            let kept_session = scalar_count(
                &db_conn,
                "SELECT COUNT(*) AS n FROM agent_session WHERE session_id = 'sess-KEPT'",
            )
            .await
            .unwrap();
            assert_eq!(kept_session, 1, "an untombstoned session still restores");
            let kept_checkpoint = scalar_count(
                &db_conn,
                "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'ckpt-KEPT'",
            )
            .await
            .unwrap();
            assert_eq!(kept_checkpoint, 1, "and so do its checkpoints");
        });
    }

    /// Codex Q5 fixture: a fresh restore inserts both sessions and
    /// checkpoints into the local catalog. Smoke-tests the happy path
    /// without spinning up a D1 client.
    #[test]
    #[serial(cwd, env)]
    fn restore_agent_capture_inserts_fresh_rows() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let sessions = vec![fixture_session_row("sess-A", "prov-A")];
            let checkpoints = vec![fixture_checkpoint_row("ckpt-A", "sess-A", Some("first"))];

            let fenced_head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
            restore_agent_capture_from_rows_with_subagents(
                &db_conn,
                AgentCaptureRestoreRows {
                    sessions: &sessions,
                    checkpoints: &checkpoints,
                    claims: &[],
                    revisions: &[],
                    links: &[],
                    traces_head: Some(fenced_head),
                    remote_is_known_ancestor: true,
                },
                true,
            )
            .await
            .expect("fresh restore should succeed");

            let session_count = scalar_count(&db_conn, "SELECT COUNT(*) AS n FROM agent_session")
                .await
                .unwrap();
            let checkpoint_count =
                scalar_count(&db_conn, "SELECT COUNT(*) AS n FROM agent_checkpoint")
                    .await
                    .unwrap();
            assert_eq!(session_count, 1);
            assert_eq!(checkpoint_count, 1);
            let restored_head = db_conn
                .query_one_raw(sea_orm::Statement::from_sql_and_values(
                    db_conn.get_database_backend(),
                    "SELECT `commit` FROM reference
                     WHERE name = ? AND kind = 'Branch' AND remote IS NULL",
                    [crate::internal::branch::TRACES_BRANCH.into()],
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by::<String, _>("commit")
                .unwrap();
            assert_eq!(restored_head, fenced_head);
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn restore_agent_capture_rejects_locally_pruned_remote_checkpoint() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let sessions = vec![fixture_session_row("sess-A", "prov-A")];
            const REMOTE_SENTINEL: &str =
                "REMOTE_PRUNED_CHECKPOINT=/private/provider/session-capture.jsonl";
            let checkpoints = vec![fixture_checkpoint_row(REMOTE_SENTINEL, "sess-A", None)];
            let fenced_head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
            restore_agent_capture_from_rows_with_subagents(
                &db_conn,
                AgentCaptureRestoreRows {
                    sessions: &sessions,
                    checkpoints: &checkpoints,
                    claims: &[],
                    revisions: &[],
                    links: &[],
                    traces_head: Some(fenced_head),
                    remote_is_known_ancestor: true,
                },
                false,
            )
            .await
            .expect("seed the stale completed remote generation locally");

            let backend = db_conn.get_database_backend();
            db_conn
                .execute_raw(sea_orm::Statement::from_sql_and_values(
                    backend,
                    "INSERT INTO agent_checkpoint_prune_tombstone \
                     (checkpoint_id, session_id, pruned_at) VALUES (?, ?, ?)",
                    [REMOTE_SENTINEL.into(), "sess-A".into(), 1_i64.into()],
                ))
                .await
                .expect("record ordinary local prune fence");
            db_conn
                .execute_raw(sea_orm::Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
                    [REMOTE_SENTINEL.into()],
                ))
                .await
                .expect("delete locally pruned checkpoint");
            db_conn
                .execute_raw(sea_orm::Statement::from_sql_and_values(
                    backend,
                    "UPDATE reference SET `commit` = NULL
                     WHERE name = ? AND kind = 'Branch' AND remote IS NULL",
                    [crate::internal::branch::TRACES_BRANCH.into()],
                ))
                .await
                .expect("simulate completed local prune before cloud sync");

            let error = restore_agent_capture_from_rows_with_subagents(
                &db_conn,
                AgentCaptureRestoreRows {
                    sessions: &sessions,
                    checkpoints: &checkpoints,
                    claims: &[],
                    revisions: &[],
                    links: &[],
                    traces_head: Some(fenced_head),
                    remote_is_known_ancestor: true,
                },
                false,
            )
            .await
            .expect_err("stale remote checkpoint must not cross the local prune fence");
            let message = error.to_string();
            let cli = error.into_cli_error("restore");
            assert!(
                !message.contains(REMOTE_SENTINEL),
                "CloudError must not echo the remote checkpoint id: {message}"
            );
            assert!(
                !cli.message().contains(REMOTE_SENTINEL),
                "human CLI error must not echo the remote checkpoint id"
            );
            assert!(
                !format!("{:?}", cli.details()).contains(REMOTE_SENTINEL),
                "structured CLI details must not retain the remote checkpoint id"
            );
            assert!(message.contains("already pruned locally"));
            assert!(message.contains("libra cloud sync"));
            assert_eq!(
                scalar_count(&db_conn, "SELECT COUNT(*) AS n FROM agent_checkpoint")
                    .await
                    .unwrap(),
                0,
                "restore must not resurrect the pruned catalog row"
            );
            let traces_head = db_conn
                .query_one_raw(sea_orm::Statement::from_sql_and_values(
                    backend,
                    "SELECT `commit` FROM reference
                     WHERE name = ? AND kind = 'Branch' AND remote IS NULL",
                    [crate::internal::branch::TRACES_BRANCH.into()],
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by::<Option<String>, _>("commit")
                .unwrap();
            assert_eq!(
                traces_head, None,
                "restore must leave the locally pruned traces ref untouched"
            );
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn restore_agent_capture_round_trips_subagent_companion_relations() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let sessions = vec![fixture_session_row("sess-A", "prov-A")];
            let mut child = fixture_checkpoint_row("child-A", "sess-A", None);
            child.scope = "subagent".to_string();
            let checkpoints = vec![child];
            let (claim, revision, link) = fixture_subagent_rows();
            let mut pruned_claim = claim.clone();
            pruned_claim.source_key = format!("source/subagent-hmac-v2/{}", "b".repeat(64));
            pruned_claim.revision_cursor = 2;
            pruned_claim.current_revision = 0;
            pruned_claim.current_checkpoint_id = None;
            pruned_claim.current_digest = None;
            let claims = vec![claim, pruned_claim];

            for _ in 0..2 {
                restore_agent_capture_from_rows_with_subagents(
                    &db_conn,
                    AgentCaptureRestoreRows {
                        sessions: &sessions,
                        checkpoints: &checkpoints,
                        claims: &claims,
                        revisions: std::slice::from_ref(&revision),
                        links: std::slice::from_ref(&link),
                        traces_head: None,
                        remote_is_known_ancestor: true,
                    },
                    false,
                )
                .await
                .expect("subagent companion restore should be idempotent");
            }

            assert_eq!(
                scalar_count(
                    &db_conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                     WHERE state = 'idle' AND revision_cursor = 1 AND current_revision = 1
                       AND current_checkpoint_id = 'child-A'",
                )
                .await
                .unwrap(),
                1
            );
            assert_eq!(
                scalar_count(
                    &db_conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                     WHERE current_revision = 0 AND current_checkpoint_id IS NULL
                       AND current_digest IS NULL AND revision_cursor = 2",
                )
                .await
                .unwrap(),
                1,
                "cloud restore must preserve an empty claim's revision high-water"
            );
            assert_eq!(
                scalar_count(
                    &db_conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_revision
                     WHERE checkpoint_id = 'child-A' AND revision = 1
                       AND source_channel = 'import'",
                )
                .await
                .unwrap(),
                1
            );
            assert_eq!(
                scalar_count(
                    &db_conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_link
                     WHERE content_checkpoint_id = 'child-A'
                       AND link_state = 'unresolved'",
                )
                .await
                .unwrap(),
                1
            );
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn restore_subagent_revision_conflict_rolls_back_claim_advance_atomically() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let sessions = vec![fixture_session_row("sess-A", "prov-A")];
            let mut child = fixture_checkpoint_row("child-A", "sess-A", None);
            child.scope = "subagent".to_string();
            let checkpoints = vec![child];
            let (claim, revision, link) = fixture_subagent_rows();
            restore_agent_capture_from_rows_with_subagents(
                &db_conn,
                AgentCaptureRestoreRows {
                    sessions: &sessions,
                    checkpoints: &checkpoints,
                    claims: std::slice::from_ref(&claim),
                    revisions: std::slice::from_ref(&revision),
                    links: std::slice::from_ref(&link),
                    traces_head: None,
                    remote_is_known_ancestor: true,
                },
                false,
            )
            .await
            .expect("seed local companion generation");

            let mut conflicting_claim = claim.clone();
            conflicting_claim.revision_cursor = 2;
            conflicting_claim.current_digest = Some("digest-conflict".to_string());
            let mut conflicting_revision = revision.clone();
            conflicting_revision.content_digest = "digest-conflict".to_string();
            let error = restore_agent_capture_from_rows_with_subagents(
                &db_conn,
                AgentCaptureRestoreRows {
                    sessions: &sessions,
                    checkpoints: &checkpoints,
                    claims: &[conflicting_claim],
                    revisions: &[conflicting_revision],
                    links: &[link],
                    traces_head: None,
                    remote_is_known_ancestor: true,
                },
                false,
            )
            .await
            .expect_err("immutable revision conflict must abort restore");
            assert!(error.to_string().contains("immutable local history"));
            assert_eq!(
                scalar_count(
                    &db_conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_claim
                     WHERE revision_cursor = 1 AND current_revision = 1
                       AND current_digest = 'digest-A'",
                )
                .await
                .unwrap(),
                1,
                "claim must remain at the pre-restore generation"
            );
            assert_eq!(
                scalar_count(
                    &db_conn,
                    "SELECT COUNT(*) AS n FROM agent_subagent_content_revision
                     WHERE content_digest = 'digest-A'",
                )
                .await
                .unwrap(),
                1
            );
        });
    }

    #[test]
    fn subagent_claim_sync_is_monotonic_across_stale_and_divergent_clones() {
        let (local, _, _) = fixture_subagent_rows();
        assert!(should_publish_claim(&local, None, false).expect("new source publishes"));

        let mut remote_newer = local.clone();
        remote_newer.sync_revision += 1;
        remote_newer.current_revision = 0;
        remote_newer.current_checkpoint_id = None;
        remote_newer.current_digest = None;
        assert!(
            should_publish_claim(&local, Some(&remote_newer), false).is_err(),
            "an independently advanced remote must require restore"
        );

        let mut local_newer = local.clone();
        local_newer.sync_revision += 1;
        local_newer.revision_cursor = 2;
        assert!(
            should_publish_claim(&local_newer, Some(&local), true)
                .expect("strictly newer sync generation publishes")
        );

        let mut cursor_regression = local_newer.clone();
        cursor_regression.revision_cursor = local.revision_cursor - 1;
        let error = should_publish_claim(&cursor_regression, Some(&local), true)
            .expect_err("revision allocation high-water must never regress");
        assert!(error.to_string().contains("high-water"));

        let mut divergent = local.clone();
        divergent.current_digest = Some("same-generation-conflict".to_string());
        let error = should_publish_claim(&local, Some(&divergent), true)
            .expect_err("same-generation divergence must fail closed");
        assert!(error.to_string().contains("same sync generation"));

        let mut higher_fence = local.clone();
        higher_fence.fence_token += 1;
        assert!(
            should_publish_claim(&higher_fence, Some(&local), true)
                .expect("fence high-water advances for the same durable generation")
        );
    }

    #[test]
    fn session_and_link_sync_generations_ignore_wall_clock_skew() {
        let local_session = fixture_session_row("sess-A", "prov-A");
        let mut remote_session = local_session.clone();
        remote_session.sync_revision = 0;
        remote_session.last_event_at = i64::MAX;
        assert!(
            should_publish_session(&local_session, Some(&remote_session), true)
                .expect("explicit session generation outranks a skewed timestamp")
        );
        let mut divergent_session = local_session.clone();
        divergent_session.state = "stopped".to_string();
        assert!(
            should_publish_session(&local_session, Some(&divergent_session), true).is_err(),
            "equal explicit session generations must agree"
        );
        let mut independently_advanced = local_session.clone();
        independently_advanced.sync_revision += 10;
        independently_advanced.state = "stopped".to_string();
        let error = should_publish_session(&independently_advanced, Some(&local_session), false)
            .expect_err("a larger clone-local counter needs remote ancestry");
        assert!(
            error
                .to_string()
                .contains("not this clone's known ancestor")
        );

        let (_, _, local_link) = fixture_subagent_rows();
        let mut remote_link = local_link.clone();
        remote_link.sync_revision = 0;
        remote_link.updated_at = i64::MAX;
        assert!(
            should_publish_link(&local_link, Some(&remote_link), true)
                .expect("explicit link generation outranks a skewed timestamp")
        );
        let mut divergent_link = local_link.clone();
        divergent_link.link_state = "resolved".to_string();
        assert!(
            should_publish_link(&local_link, Some(&divergent_link), true).is_err(),
            "equal explicit link generations must agree"
        );
    }

    #[tokio::test]
    async fn local_cloud_base_only_advances_to_completed_remote_generations() {
        let conn = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("open lineage test database");
        conn.execute_raw(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            "CREATE TABLE agent_capture_cloud_base (
                repo_id TEXT PRIMARY KEY,
                remote_generation INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             )"
            .to_string(),
        ))
        .await
        .expect("create lineage table");

        store_local_agent_capture_cloud_base(&conn, "repo", 7)
            .await
            .expect("record completed generation");
        store_local_agent_capture_cloud_base(&conn, "repo", 5)
            .await
            .expect("ignore an older completion");
        assert_eq!(
            load_local_agent_capture_cloud_base(&conn, "repo")
                .await
                .expect("read lineage"),
            Some(7)
        );
    }

    #[test]
    fn completed_companion_snapshot_requires_revision_link_bijection() {
        let sessions = vec![fixture_session_row("sess-A", "prov-A")];
        let mut child = fixture_checkpoint_row("child-A", "sess-A", None);
        child.scope = "subagent".to_string();
        let checkpoints = vec![child];
        let (claim, revision, link) = fixture_subagent_rows();
        validate_agent_capture_session_dependencies(
            &sessions,
            &checkpoints,
            std::slice::from_ref(&claim),
            "fixture",
        )
        .expect("session dependencies");

        let missing_link = validate_agent_capture_companions(
            &checkpoints,
            std::slice::from_ref(&claim),
            std::slice::from_ref(&revision),
            &[],
            "fixture",
            CompanionValidationMode::Complete,
        )
        .expect_err("every completed revision must have a link");
        assert!(missing_link.to_string().contains("no association link"));

        let missing_revision = validate_agent_capture_companions(
            &checkpoints,
            &[],
            &[],
            std::slice::from_ref(&link),
            "fixture",
            CompanionValidationMode::Complete,
        )
        .expect_err("every completed link must have a revision");
        assert!(
            missing_revision
                .to_string()
                .contains("no immutable revision")
        );
    }

    #[test]
    fn remote_companion_validation_redacts_untrusted_checkpoint_ids() {
        const REMOTE_SENTINEL: &str =
            "REMOTE_COMPANION_CHECKPOINT=/private/provider/session-capture.jsonl";
        let (_, mut revision, _) = fixture_subagent_rows();
        revision.checkpoint_id = REMOTE_SENTINEL.to_string();

        let error = validate_agent_capture_companions(
            &[],
            &[],
            &[revision],
            &[],
            "remote",
            CompanionValidationMode::Complete,
        )
        .expect_err("a remote revision without its claim must fail");
        let detail = error.to_string();
        let cli = error.into_cli_error("restore");
        assert!(detail.contains("no source claim dependency"));
        assert!(
            !detail.contains(REMOTE_SENTINEL),
            "CloudError must not echo remote companion metadata: {detail}"
        );
        assert!(
            !cli.message().contains(REMOTE_SENTINEL),
            "human CLI error must not echo remote companion metadata"
        );
        assert!(
            !format!("{:?}", cli.details()).contains(REMOTE_SENTINEL),
            "structured CLI details must not retain remote companion metadata"
        );
    }

    #[test]
    fn companion_snapshot_rejects_non_subagent_content_and_boundary_targets() {
        let committed = fixture_checkpoint_row("child-A", "sess-A", None);
        let (claim, revision, link) = fixture_subagent_rows();
        let error = validate_agent_capture_companions(
            std::slice::from_ref(&committed),
            std::slice::from_ref(&claim),
            std::slice::from_ref(&revision),
            std::slice::from_ref(&link),
            "fixture",
            CompanionValidationMode::Complete,
        )
        .expect_err("content revisions must not target committed checkpoints");
        assert!(error.to_string().contains("non-subagent checkpoint"));

        let mut content = committed;
        content.scope = "subagent".to_string();
        let boundary = fixture_checkpoint_row("boundary-A", "sess-A", None);
        let mut resolved = link;
        resolved.link_state = "resolved".to_string();
        resolved.boundary_checkpoint_id = Some("boundary-A".to_string());
        let error = validate_agent_capture_companions(
            &[content, boundary],
            &[claim],
            &[revision],
            &[resolved],
            "fixture",
            CompanionValidationMode::Complete,
        )
        .expect_err("boundary references must target subagent checkpoints");
        assert!(error.to_string().contains("invalid boundary checkpoint"));
    }

    #[test]
    fn checkpoint_projection_rejects_unreconciled_remote_rows_but_applies_ordinary_prune() {
        let local = fixture_checkpoint_row("local-A", "sess-A", None);
        assert_eq!(
            object_manifest_scope_for_remote_catalog(
                std::slice::from_ref(&local),
                std::slice::from_ref(&local),
            ),
            AgentCaptureObjectManifestScope::CheckpointProjection
        );
        let remote_only = fixture_checkpoint_row("remote-only", "sess-A", None);
        let error = build_effective_checkpoint_catalog(
            std::slice::from_ref(&local),
            &[local.clone(), remote_only.clone()],
            &[],
            &[],
            true,
        )
        .expect_err("an unmarked remote-only checkpoint must be reconciled before publication");
        assert!(error.to_string().contains("absent from this local catalog"));
        assert!(
            !error.to_string().contains(&remote_only.checkpoint_id),
            "the reconciliation error must not disclose the remote checkpoint identifier"
        );

        let tombstone = AgentCheckpointPruneTombstoneRow {
            checkpoint_id: remote_only.checkpoint_id.clone(),
            session_id: remote_only.session_id.clone(),
            pruned_at: 1,
        };
        let (_, effective) = build_effective_checkpoint_catalog(
            std::slice::from_ref(&local),
            &[local.clone(), remote_only.clone()],
            &[tombstone],
            &[],
            true,
        )
        .expect("ordinary prune tombstone projects the remote deletion");
        assert_eq!(effective, vec![local.clone()]);
        assert_eq!(
            object_manifest_scope_for_remote_catalog(std::slice::from_ref(&local), &effective),
            AgentCaptureObjectManifestScope::CheckpointProjection
        );

        let indexes = [
            ObjectIndexRow {
                o_id: "traces".to_string(),
                o_type: "commit".to_string(),
                o_size: 1,
                repo_id: "repo".to_string(),
                created_at: 1,
                is_synced: 1,
                object_format: None,
            },
            ObjectIndexRow {
                o_id: "tree".to_string(),
                o_type: "tree".to_string(),
                o_size: 1,
                repo_id: "repo".to_string(),
                created_at: 1,
                is_synced: 1,
                object_format: None,
            },
        ];
        let mut retained = remote_only;
        retained.traces_commit = "traces".to_string();
        retained.tree_oid = "tree".to_string();
        const REMOTE_SENTINEL: &str =
            "REMOTE_CHECKPOINT_OID=/private/provider/session-capture.jsonl";
        retained.metadata_blob_oid = REMOTE_SENTINEL.to_string();
        let error = validate_checkpoint_object_index_roots(&[retained], &indexes, "remote")
            .expect_err("a full manifest must include every retained checkpoint root");
        let detail = error.to_string();
        let cli = error.into_cli_error("sync");
        assert!(detail.contains("references an object absent from the fenced object index"));
        assert!(
            !detail.contains(REMOTE_SENTINEL),
            "CloudError must not echo remote checkpoint metadata: {detail}"
        );
        assert!(
            !cli.message().contains(REMOTE_SENTINEL),
            "human CLI error must not echo remote checkpoint metadata"
        );
        assert!(
            !format!("{:?}", cli.details()).contains(REMOTE_SENTINEL),
            "structured CLI details must not retain remote checkpoint metadata"
        );
        assert_eq!(cli.stable_code(), StableErrorCode::ConflictOperationBlocked);
    }

    #[test]
    fn capture_manifest_scope_requires_an_explicit_supported_version() {
        assert_eq!(
            AgentCaptureObjectManifestScope::parse(Some("checkpoint_projection"))
                .expect("current projection scope"),
            AgentCaptureObjectManifestScope::CheckpointProjection
        );
        assert_eq!(
            AgentCaptureObjectManifestScope::parse(Some("full_remote_index"))
                .expect("current full-index scope"),
            AgentCaptureObjectManifestScope::FullRemoteIndex
        );
        for unsupported in [None, Some(""), Some("future_scope")] {
            assert!(
                AgentCaptureObjectManifestScope::parse(unsupported).is_err(),
                "legacy and unknown manifest scopes must fail closed"
            );
        }
    }

    #[test]
    fn agent_capture_object_manifest_digest_is_order_stable_and_content_sensitive() {
        let first = ObjectIndexRow {
            o_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            o_type: "blob".to_string(),
            o_size: 2,
            repo_id: "repo".to_string(),
            created_at: 200,
            is_synced: 1,
            object_format: None,
        };
        let second = ObjectIndexRow {
            o_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            o_type: "tree".to_string(),
            o_size: 1,
            repo_id: "repo".to_string(),
            created_at: 100,
            is_synced: 1,
            object_format: None,
        };
        let left = agent_capture_object_index_digest(&[first.clone(), second.clone()])
            .expect("digest indexes");
        let right = agent_capture_object_index_digest(&[second.clone(), first.clone()])
            .expect("digest reordered indexes");
        assert_eq!(left, right, "D1 row order must not affect the manifest");

        let mut changed = second;
        changed.o_size += 1;
        assert_ne!(
            left,
            agent_capture_object_index_digest(&[first, changed]).expect("digest changed indexes"),
            "object identity metadata must be fenced by the manifest"
        );
    }

    #[test]
    fn agent_capture_object_manifest_errors_redact_remote_object_ids() {
        const REMOTE_SENTINEL: &str = "REMOTE_OBJECT_ID=/private/provider/session-capture.jsonl";
        let remote_row = ObjectIndexRow {
            o_id: REMOTE_SENTINEL.to_string(),
            o_type: "blob".to_string(),
            o_size: 1,
            repo_id: "repo".to_string(),
            created_at: 1,
            is_synced: 1,
            object_format: None,
        };

        let error = agent_capture_object_index_digest(&[remote_row.clone(), remote_row])
            .expect_err("duplicate remote object ids must fail");
        let detail = error.to_string();
        let cli = error.into_cli_error("sync");
        assert!(detail.contains("duplicate object ids"));
        assert!(
            !detail.contains(REMOTE_SENTINEL),
            "CloudError must not echo remote object ids: {detail}"
        );
        assert!(
            !cli.message().contains(REMOTE_SENTINEL),
            "human CLI error must not echo remote object ids"
        );
        assert!(
            !format!("{:?}", cli.details()).contains(REMOTE_SENTINEL),
            "structured CLI details must not retain remote object ids"
        );
        assert_eq!(cli.stable_code(), StableErrorCode::ConflictOperationBlocked);
    }

    #[test]
    fn legacy_git_only_backup_does_not_require_capture_manifest() {
        validate_missing_capture_manifest(false)
            .expect("an empty legacy capture layer is a valid Git-only backup");
        let error = validate_missing_capture_manifest(true)
            .expect_err("legacy capture rows require current-version sync adoption");
        assert!(
            error
                .to_string()
                .contains("no completed generation manifest")
        );
    }

    #[tokio::test]
    async fn synced_checkpoint_index_lookup_ignores_large_unrelated_object_history() {
        let conn = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("open object-index scale fixture");
        conn.execute_raw(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            "CREATE TABLE object_index (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                o_id TEXT NOT NULL, o_type TEXT NOT NULL, o_size INTEGER NOT NULL,
                repo_id TEXT NOT NULL, created_at INTEGER NOT NULL,
                is_synced INTEGER NOT NULL
             )"
            .to_string(),
        ))
        .await
        .expect("create object-index scale fixture");
        conn.execute_raw(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            "WITH digits(d) AS (
                 VALUES (0),(1),(2),(3),(4),(5),(6),(7),(8),(9)
             ), numbers(n) AS (
                 SELECT a.d + 10*b.d + 100*c.d + 1000*d.d + 10000*e.d + 100000*f.d
                 FROM digits a, digits b, digits c, digits d, digits e, digits f
                 LIMIT 100001
             )
             INSERT INTO object_index
               (o_id, o_type, o_size, repo_id, created_at, is_synced)
             SELECT printf('%040x', n), 'blob', 1, 'large-repo', n, 1 FROM numbers"
                .to_string(),
        ))
        .await
        .expect("seed more unrelated indexes than the capture history bound");
        let required_oid = "ffffffffffffffffffffffffffffffffffffffff".to_string();
        conn.execute_raw(sea_orm::Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO object_index
               (o_id, o_type, o_size, repo_id, created_at, is_synced)
             VALUES (?, 'tree', 7, ?, 9, 1)",
            [required_oid.clone().into(), "large-repo".into()],
        ))
        .await
        .expect("seed one checkpoint-reachable index");

        let required = HashSet::from([required_oid.clone()]);
        let synced = load_synced_required_object_oids(&conn, "large-repo", &required)
            .await
            .expect("unrelated repository objects must not consume the capture bound");
        assert_eq!(synced, HashSet::from([required_oid.clone()]));
        let projection = project_agent_capture_object_indexes(&conn, "large-repo", &required)
            .await
            .expect("manifest projection reads only required object indexes");
        assert_eq!(projection.len(), 1);
        assert_eq!(projection[0].o_id, required_oid);
    }

    #[test]
    fn agent_capture_request_count_scales_by_bounded_batches() {
        let rows = vec![(); 257];
        let page_lengths = agent_capture_batches(&rows)
            .map(<[_]>::len)
            .collect::<Vec<_>>();
        assert_eq!(page_lengths, [128, 128, 1]);
        assert_eq!(
            page_lengths.len(),
            3,
            "257 changed rows must produce three D1 writes, not 257"
        );
    }

    #[test]
    fn agent_capture_object_verification_uses_fixed_concurrency_pages() {
        let rows = vec![(); 65];
        let page_lengths = agent_capture_object_verification_batches(&rows)
            .map(<[_]>::len)
            .collect::<Vec<_>>();
        assert_eq!(page_lengths, [32, 32, 1]);
    }

    #[tokio::test]
    async fn local_capture_tables_share_one_restore_row_budget() {
        let conn = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("open aggregate budget fixture");
        conn.execute_raw(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            "CREATE TABLE capture_budget (kind TEXT NOT NULL, value INTEGER NOT NULL)".to_string(),
        ))
        .await
        .expect("create aggregate budget fixture");
        conn.execute_raw(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO capture_budget VALUES
                ('session', 1), ('session', 2),
                ('checkpoint', 3), ('checkpoint', 4)"
                .to_string(),
        ))
        .await
        .expect("seed aggregate budget fixture");
        let mut remaining = 3_usize;
        let sessions = load_local_capture_keyset_pages(
            &conn,
            &CatalogQuery {
                label: "session fixture",
                select: "SELECT value FROM capture_budget",
                filter: "kind = 'session'",
                key: &[KeyColumn::Integer("value")],
            },
            &mut remaining,
            None,
        )
        .await
        .expect("first table fits shared budget");
        assert_eq!(sessions.len(), 2);
        assert_eq!(remaining, 1);
        let error = load_local_capture_keyset_pages(
            &conn,
            &CatalogQuery {
                label: "checkpoint fixture",
                select: "SELECT value FROM capture_budget",
                filter: "kind = 'checkpoint'",
                key: &[KeyColumn::Integer("value")],
            },
            &mut remaining,
            None,
        )
        .await
        .expect_err("second table must not reset the shared budget");
        assert!(error.to_string().contains("aggregate"));
    }

    #[test]
    #[serial(cwd, env)]
    fn agent_capture_snapshot_never_publishes_catalog_beyond_object_generation() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        let _delay = ScopedEnvVar::set("LIBRA_TEST_CLOUD_AGENT_SNAPSHOT_DELAY_MS", "200");
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let snapshot_conn = db_conn.clone();
            let writer_conn = db_conn.clone();
            let snapshot_task = tokio::spawn(async move {
                load_agent_capture_snapshot(&snapshot_conn, "snapshot-repo", true).await
            });
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            let writer = tokio::spawn(async move {
                let txn = writer_conn.begin().await.expect("begin concurrent capture");
                txn.execute_raw(sea_orm::Statement::from_string(
                    txn.get_database_backend(),
                    "INSERT INTO object_index (
                        o_id, o_type, o_size, repo_id, created_at, is_synced
                     ) VALUES
                        ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'commit', 1,
                         'snapshot-repo', 1, 0),
                        ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'tree', 1,
                         'snapshot-repo', 1, 0),
                        ('cccccccccccccccccccccccccccccccccccccccc', 'blob', 1,
                         'snapshot-repo', 1, 0)"
                        .to_string(),
                ))
                .await
                .expect("insert concurrent objects");
                txn.execute_raw(sea_orm::Statement::from_string(
                    txn.get_database_backend(),
                    "INSERT INTO agent_session (
                        session_id, agent_kind, provider_session_id, state, working_dir,
                        metadata_json, redaction_report, started_at, last_event_at, schema_version
                     ) VALUES ('concurrent-session', 'claude_code', 'concurrent-provider',
                               'active', '/repo', '{}', '{}', 1, 1, 1)"
                        .to_string(),
                ))
                .await
                .expect("insert concurrent session");
                txn.execute_raw(sea_orm::Statement::from_string(
                    txn.get_database_backend(),
                    "INSERT INTO agent_checkpoint (
                        checkpoint_id, session_id, scope, tree_oid, metadata_blob_oid,
                        traces_commit, created_at
                     ) VALUES ('concurrent-checkpoint', 'concurrent-session', 'committed',
                               'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                               'cccccccccccccccccccccccccccccccccccccccc',
                               'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1)"
                        .to_string(),
                ))
                .await
                .expect("insert concurrent checkpoint");
                txn.commit().await.expect("commit concurrent capture");
            });
            // Bound is wall-clock: under a saturated nextest host, 150ms was
            // too tight even when the writer was not blocked on the scan.
            tokio::time::timeout(std::time::Duration::from_secs(2), writer)
                .await
                .expect("concurrent writer must not wait for the durability scan")
                .expect("join concurrent capture");
            let snapshot_error = snapshot_task
                .await
                .expect("join snapshot")
                .expect_err("the catalog recheck must reject a mixed generation");
            let snapshot_message = snapshot_error.to_string();
            assert!(
                snapshot_message.contains("changed during durability verification")
                    || snapshot_message.contains("outside the completed object upload generation"),
                "unexpected snapshot rejection: {snapshot_message}"
            );
            assert_eq!(
                scalar_count(
                    &db_conn,
                    "SELECT COUNT(*) AS n FROM object_index
                     WHERE repo_id = 'snapshot-repo' AND is_synced = 0",
                )
                .await
                .unwrap(),
                3,
                "the next sync generation must pick up the concurrent objects"
            );
        });
    }

    /// Codex Q5 fixture: re-running restore over an existing session row
    /// with the same `(agent_kind, provider_session_id)` MUST update the
    /// existing row in place rather than inserting a duplicate or erroring
    /// on the unique index (`idx_agent_session_provider`).
    #[test]
    #[serial(cwd, env)]
    fn restore_agent_capture_upserts_existing_session_on_conflict() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let initial = vec![fixture_session_row("sess-A", "prov-A")];
            restore_agent_capture_from_rows(&db_conn, &initial, &[], true)
                .await
                .expect("first restore");

            let mut updated = fixture_session_row("sess-A", "prov-A");
            updated.state = "stopped".to_string();
            updated.last_event_at = 1_800_000_000;
            updated.stopped_at = Some(1_800_000_000);
            updated.sync_revision = 2;

            restore_agent_capture_from_rows(&db_conn, &[updated], &[], true)
                .await
                .expect("conflict update");

            let stale = fixture_session_row("sess-A", "prov-A");
            restore_agent_capture_from_rows(&db_conn, &[stale], &[], true)
                .await
                .expect("stale restore is skipped");
            let divergent = fixture_session_row("sess-A", "prov-A");
            let error = restore_agent_capture_from_rows_with_subagents(
                &db_conn,
                AgentCaptureRestoreRows {
                    sessions: &[divergent],
                    checkpoints: &[],
                    claims: &[],
                    revisions: &[],
                    links: &[],
                    traces_head: None,
                    remote_is_known_ancestor: false,
                },
                false,
            )
            .await
            .expect_err("an unrelated larger local counter is not cloud lineage");
            assert!(error.to_string().contains("divergent sync revision"));

            let session_count = scalar_count(&db_conn, "SELECT COUNT(*) AS n FROM agent_session")
                .await
                .unwrap();
            assert_eq!(session_count, 1, "no duplicate row");

            let stopped_count = scalar_count(
                &db_conn,
                "SELECT COUNT(*) AS n FROM agent_session WHERE state = 'stopped'",
            )
            .await
            .unwrap();
            assert_eq!(stopped_count, 1, "state column reflects updated row");
        });
    }

    /// Immutable checkpoint fields never change merely because a remote row
    /// carries a generation number.
    #[test]
    #[serial(cwd, env)]
    fn restore_agent_capture_rejects_immutable_checkpoint_conflict() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let session = vec![fixture_session_row("sess-A", "prov-A")];
            let initial = vec![fixture_checkpoint_row("ckpt-A", "sess-A", Some("v1"))];
            restore_agent_capture_from_rows(&db_conn, &session, &initial, true)
                .await
                .expect("first restore");

            let updated = vec![fixture_checkpoint_row("ckpt-A", "sess-A", Some("v2"))];
            let error = restore_agent_capture_from_rows(&db_conn, &session, &updated, true)
                .await
                .expect_err("immutable conflict must fail closed");
            assert!(error.to_string().contains("same sync generation"));

            use sea_orm::Statement;
            let backend = db_conn.get_database_backend();
            let row = db_conn
                .query_one_raw(Statement::from_sql_and_values(
                    backend,
                    "SELECT description FROM agent_checkpoint WHERE checkpoint_id = ?",
                    ["ckpt-A".into()],
                ))
                .await
                .unwrap()
                .expect("row present");
            let description: Option<String> = row.try_get_by(0).unwrap();
            assert_eq!(
                description.as_deref(),
                Some("v1"),
                "immutable checkpoint remains unchanged"
            );

            let count = scalar_count(&db_conn, "SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await
                .unwrap();
            assert_eq!(count, 1, "no duplicate checkpoint row");
        });
    }

    #[test]
    #[serial(cwd, env)]
    fn restore_agent_capture_applies_newer_checkpoint_prune_rewrite() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let session = vec![fixture_session_row("sess-A", "prov-A")];
            let initial = fixture_checkpoint_row("ckpt-A", "sess-A", None);
            restore_agent_capture_from_rows(
                &db_conn,
                &session,
                std::slice::from_ref(&initial),
                true,
            )
            .await
            .expect("first restore");

            let mut rewritten = initial.clone();
            rewritten.tree_oid = "3333333333333333333333333333333333333333".to_string();
            rewritten.traces_commit = "4444444444444444444444444444444444444444".to_string();
            rewritten.sync_revision = 2;
            restore_agent_capture_from_rows(&db_conn, &session, &[rewritten.clone()], true)
                .await
                .expect("newer prune rewrite restores");
            restore_agent_capture_from_rows(&db_conn, &session, &[initial], true)
                .await
                .expect("stale pre-prune row is skipped");

            let row = db_conn
                .query_one_raw(sea_orm::Statement::from_string(
                    db_conn.get_database_backend(),
                    "SELECT traces_commit, sync_revision FROM agent_checkpoint
                     WHERE checkpoint_id = 'ckpt-A'"
                        .to_string(),
                ))
                .await
                .expect("query restored checkpoint")
                .expect("restored checkpoint row");
            assert_eq!(
                row.try_get_by::<String, _>("traces_commit")
                    .expect("traces commit"),
                rewritten.traces_commit
            );
            assert_eq!(
                row.try_get_by::<i64, _>("sync_revision")
                    .expect("checkpoint generation"),
                2
            );
        });
    }

    /// A row-level failure must roll back the entire local restore generation;
    /// otherwise claims/checkpoints from a partially applied remote snapshot
    /// can become visible together with stale local companions.
    #[test]
    #[serial(cwd, env)]
    fn restore_agent_capture_partial_failure_returns_err() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let mut bad = fixture_session_row("sess-bad", "prov-bad");
            bad.agent_kind = "not_a_real_kind".to_string(); // violates CHECK
            let good = fixture_session_row("sess-good", "prov-good");

            let err = restore_agent_capture_from_rows(&db_conn, &[bad, good], &[], true)
                .await
                .expect_err("strict restore should bubble the failure");
            let message = err.to_string();
            assert!(
                message.contains("session") || message.contains("checkpoint"),
                "error message identifies the failing kind: {message}"
            );

            // The valid sibling is rolled back with the invalid row.
            let good_count = scalar_count(
                &db_conn,
                "SELECT COUNT(*) AS n FROM agent_session WHERE session_id = 'sess-good'",
            )
            .await
            .unwrap();
            assert_eq!(good_count, 0);

            // `serde_json::Value` otherwise accepts this raw-locator/safe-id
            // pair with last-key-wins semantics. Restore must reject it before
            // it opens its local write transaction.
            let source_id = format!("source/hmac-v2/{}", "a".repeat(64));
            let mut duplicate_metadata = fixture_session_row("sess-duplicate-meta", "prov-meta");
            duplicate_metadata.metadata_json = format!(
                r#"{{"repository_identity":"not-retained","source_kind":"file","source_id":"/private/raw/session.jsonl","source_fingerprint":"{source_id}","import_source_schema_version":2,"import_provisional":false,"transcript_snapshot":null,"imported":true,"source_id":"{source_id}"}}"#
            );
            duplicate_metadata.redaction_report = r#"{"import":{"pipeline":"typed_allowlist","snapshot_redaction":true,"raw_persisted":false,"matches":[],"bytes_scanned":0,"bytes_redacted":0}}"#.to_string();

            let err = restore_agent_capture_from_rows(
                &db_conn,
                &[duplicate_metadata],
                &[],
                true,
            )
            .await
            .expect_err("duplicate top-level ownership metadata must be rejected");
            assert!(
                err.to_string().contains("malformed import ownership metadata"),
                "duplicate metadata error must identify the ownership boundary: {err}"
            );
            let session_count =
                scalar_count(&db_conn, "SELECT COUNT(*) AS n FROM agent_session")
                    .await
                    .unwrap();
            assert_eq!(session_count, 0, "rejected metadata must not write locally");

            // The redaction report is a separate persisted JSON column. A
            // duplicate nested under its import wrapper must be caught before
            // serde can turn true/false into a safe-looking final value.
            let mut duplicate_redaction =
                fixture_session_row("sess-duplicate-redaction", "prov-redaction");
            duplicate_redaction.metadata_json = format!(
                r#"{{"repository_identity":"not-retained","source_kind":"file","source_id":"{source_id}","source_fingerprint":"{source_id}","import_source_schema_version":2,"import_provisional":false,"transcript_snapshot":null,"imported":true}}"#
            );
            duplicate_redaction.redaction_report = r#"{"import":{"pipeline":"typed_allowlist","snapshot_redaction":true,"raw_persisted":true,"raw_persisted":false,"matches":[],"bytes_scanned":0,"bytes_redacted":0}}"#.to_string();

            let err = restore_agent_capture_from_rows(
                &db_conn,
                &[duplicate_redaction],
                &[],
                true,
            )
            .await
            .expect_err("duplicate nested redaction metadata must be rejected");
            assert!(
                err.to_string()
                    .contains("invalid V2 import ownership metadata"),
                "duplicate redaction error must identify the V2 boundary: {err}"
            );
            let session_count =
                scalar_count(&db_conn, "SELECT COUNT(*) AS n FROM agent_session")
                    .await
                    .unwrap();
            assert_eq!(session_count, 0, "rejected redaction must not write locally");
        });
    }

    /// Codex round-2 follow-up Q4: when the local `agent_checkpoint`
    /// table is missing (partial schema), `restore_agent_capture_from_d1`
    /// must take the warning-and-bail path rather than proceed to insert
    /// rows into a half-built catalogue. This test simulates that
    /// scenario by dropping the checkpoint table after init.
    #[test]
    #[serial(cwd, env)]
    fn restore_agent_capture_warns_when_checkpoint_table_missing() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            use sea_orm::Statement;
            let backend = db_conn.get_database_backend();
            // Drop the checkpoint table to simulate a partial schema. We
            // exercise the local-presence guard, not the D1 list call —
            // the helper bails before either ensure_*_table fires.
            db_conn
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "DROP TABLE agent_checkpoint",
                    [],
                ))
                .await
                .expect("drop checkpoint table");

            // Build a stub D1Client that we never actually call. The
            // helper short-circuits on the local-schema check before
            // touching the network, so the stub credentials are never
            // dereferenced.
            let d1_client = D1Client::new(
                "stub-account".to_string(),
                "stub-token".to_string(),
                "stub-database".to_string(),
            );

            let result =
                restore_agent_capture_from_d1(&db_conn, &d1_client, "fixture-repo", true).await;
            assert!(
                result.is_ok(),
                "partial-schema path returns Ok with a warning, not Err: {:?}",
                result.err()
            );
        });
    }

    /// Tiny helper for the fixture tests above. Mirrors the shape of
    /// `agent::doctor::scalar_count` but lives in this module so the cloud
    /// tests don't depend on a binary-only helper.
    async fn scalar_count(
        conn: &sea_orm::DatabaseConnection,
        sql: &str,
    ) -> Result<i64, sea_orm::DbErr> {
        use sea_orm::Statement;
        let backend = conn.get_database_backend();
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(backend, sql, []))
            .await?
            .ok_or(sea_orm::DbErr::Custom("count returned no rows".to_string()))?;
        row.try_get_by::<i64, _>("n")
    }

    #[test]
    #[serial(cwd, env)]
    fn validate_cloud_backup_env_surfaces_config_resolution_errors() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());
        let _account = ClearedEnvVarGuard::new("LIBRA_D1_ACCOUNT_ID");
        let _token = ClearedEnvVarGuard::new("LIBRA_D1_API_TOKEN");
        let _database = ClearedEnvVarGuard::new("LIBRA_D1_DATABASE_ID");

        let bad_global_dir = tempdir().unwrap();
        let bad_global_db = bad_global_dir.path().join("bad-global.db");
        fs::write(&bad_global_db, "not sqlite").unwrap();
        let _global_db = ScopedEnvVar::set("LIBRA_CONFIG_GLOBAL_DB", &bad_global_db);

        let err = rt
            .block_on(validate_cloud_backup_env(true))
            .expect_err("global config resolution failure should surface");
        let message = err.to_string();
        assert!(
            message.contains("failed to open config database")
                || message.contains("failed to connect to global config"),
            "unexpected error: {message}"
        );
    }
}
