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
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Schema, Set,
    TransactionTrait, sea_query::Expr,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    cli_error,
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
mod sync;

use agent_capture::*;
use object_format::REBACKUP_HINT;
pub(crate) use restore::*;
pub(crate) use sync::run_cloud_sync;

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

/// Inputs for [`run_cloud_sync`].
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
    fn on_sync_complete(&self, synced: usize, failed: usize) {
        let _ = (synced, failed);
    }
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
}

/// Console implementation that reproduces the legacy `libra cloud
/// sync` output verbatim.
pub struct ConsoleCloudSyncProgress;

impl CloudSyncProgress for ConsoleCloudSyncProgress {
    fn on_starting(&self) {
        println!("Starting cloud sync...");
    }
    fn on_no_objects(&self) {
        println!("No objects to sync.");
    }
    fn on_object_total(&self, total: usize) {
        println!("Found {total} objects to sync.");
    }
    fn on_batch_progress(&self, synced: usize, total: usize, failed: usize) {
        println!("Progress: {synced}/{total} synced, {failed} failed");
    }
    fn on_object_error(&self, oid: &str, err: &str) {
        cli_error!(err => format!("error: failed to sync {oid}"));
    }
    fn on_local_status_warning(&self, oid: &str, err: &str) {
        cli_error!(err => format!("warning: failed to update local sync status for {oid}"));
    }
    fn on_sync_complete(&self, synced: usize, failed: usize) {
        println!("Sync complete: {synced} synced, {failed} failed");
    }
    fn on_metadata_starting(&self) {
        println!("Syncing metadata...");
    }
    fn on_metadata_skipped(&self) {
        println!("Metadata unchanged, skipping upload.");
    }
    fn on_metadata_synced(&self, references: usize) {
        println!("Metadata synced ({references} references).");
    }
    fn on_agent_capture_starting(&self) {
        println!("Syncing agent capture catalog to D1...");
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
        println!(
            "Agent capture sync: {sessions_synced} sessions ({sessions_failed} failed), \
             {checkpoints_synced} checkpoints ({checkpoints_failed} failed), \
             {subagent_rows_synced} subagent companion rows ({subagent_rows_failed} failed)."
        );
    }
    fn on_agent_capture_warning(&self, err: &str) {
        eprintln!("warning: agent capture sync incomplete: {err}");
    }
}

struct SilentCloudSyncProgress;

impl CloudSyncProgress for SilentCloudSyncProgress {}

struct JsonCloudSyncProgress;

impl JsonCloudSyncProgress {
    fn emit(event: serde_json::Value) {
        eprintln!("{event}");
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
        Self::emit(serde_json::json!({
            "event": "cloud_sync.objects.complete",
            "synced": synced,
            "failed": failed,
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
            "event": "cloud_sync.agent_capture.complete",
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
            if output.is_json() || output.quiet || matches!(output.progress, ProgressMode::Json) {
                let ctx = CloudSyncContext {
                    batch_size: sync_args.batch_size,
                    force: sync_args.force,
                };
                let progress: &dyn CloudSyncProgress =
                    if matches!(output.progress, ProgressMode::Json) {
                        &JsonCloudSyncProgress
                    } else {
                        &SilentCloudSyncProgress
                    };
                let report = run_cloud_sync(ctx, progress)
                    .await
                    .map_err(|e| cloud_cli_error_typed("sync", e))?;
                if report.failed_count > 0 {
                    // Variant is known here — skip the String -> CloudError
                    // classification round-trip and surface PartialTransfer directly.
                    return Err(cloud_cli_error_typed(
                        "sync",
                        CloudError::PartialTransfer(format!(
                            "{} objects failed to sync",
                            report.failed_count
                        )),
                    ));
                }
                if let AgentCaptureSyncOutcome::Failed { error } = &report.agent_capture {
                    return Err(cloud_cli_error_typed(
                        "sync",
                        agent_capture_mirror_failure(error),
                    ));
                }
                render_cloud_sync_output(&report, output)?;
            } else {
                execute_sync(sync_args)
                    .await
                    .map_err(|e| cloud_cli_error_typed("sync", e))?;
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
    /// Anything else — kept as the original detail string.
    Generic(String),
}

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
            } => {
                if !detail.is_empty() {
                    write!(f, "{detail}")
                } else if missing_keys.is_empty() {
                    write!(f, "missing cloud environment configuration")
                } else {
                    write!(f, "Missing: {}", missing_keys.join(", "))
                }
            }
            CloudError::NameAlreadyTaken(detail)
            | CloudError::NameNotFound(detail)
            | CloudError::PartialTransfer(detail)
            | CloudError::D1(detail)
            | CloudError::R2(detail)
            | CloudError::AmbiguousObjectFormat(detail)
            | CloudError::Generic(detail) => write!(f, "{detail}"),
        }
    }
}

impl CloudError {
    /// Map the typed cloud error onto a [`CliError`] for the given top-level
    /// `operation` ("sync" / "restore" / "status").
    fn into_cli_error(self, operation: &str) -> CliError {
        match self {
            CloudError::MissingEnv {
                detail,
                missing_keys,
            } => {
                let message = if missing_keys.is_empty() {
                    format!("missing cloud configuration for {operation}")
                } else {
                    format!(
                        "missing cloud configuration for {operation}: {}",
                        missing_keys.join(", ")
                    )
                };
                CliError::auth(message)
                    .with_stable_code(StableErrorCode::AuthMissingCredentials)
                    .with_detail("missing_keys", missing_keys)
                    .with_detail("raw_detail", detail)
                    .with_hint("set the missing variables in env or vault.env.* before retrying.")
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
    }
}

fn render_cloud_sync_output(report: &CloudSyncReport, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        let cloud_output = cloud_sync_output_from_report(report);
        return emit_json_data("cloud.sync", &cloud_output, output);
    }
    Ok(())
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
    if report.failed_count > 0 {
        return Err(CloudError::PartialTransfer(format!(
            "{} objects failed to sync",
            report.failed_count
        )));
    }
    if let AgentCaptureSyncOutcome::Failed { error } = report.agent_capture {
        return Err(agent_capture_mirror_failure(&error));
    }
    Ok(())
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
/// Mirrors [`sync_agent_capture_tables`] in reverse: lists D1 rows for the
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
        let local = LocalStorage::new(local_dir.path().to_path_buf());
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
        let local = LocalStorage::new(local_dir.path().to_path_buf());
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
        let local = LocalStorage::new(local_dir.path().to_path_buf());
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
        let sessions = load_local_capture_pages(
            &conn,
            "SELECT value FROM capture_budget WHERE kind = ? ORDER BY value",
            vec!["session".into()],
            "session fixture",
            &mut remaining,
        )
        .await
        .expect("first table fits shared budget");
        assert_eq!(sessions.len(), 2);
        assert_eq!(remaining, 1);
        let error = load_local_capture_pages(
            &conn,
            "SELECT value FROM capture_budget WHERE kind = ? ORDER BY value",
            vec!["checkpoint".into()],
            "checkpoint fixture",
            &mut remaining,
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
