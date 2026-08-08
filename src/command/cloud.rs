//! Cloud backup command for synchronizing repository data to Cloudflare D1 and R2.
//!
//! This module provides subcommands for:
//! - `libra cloud sync` - Sync local DB to D1, objects to R2
//! - `libra cloud restore` - Restore from D1/R2
//! - `libra cloud status` - Show sync status

use std::{
    collections::{BTreeMap, HashMap, HashSet, hash_map::DefaultHasher},
    fmt,
    hash::{Hash, Hasher},
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
    command::restore::{self as restore_cmd, RestoreArgs as RestoreWorktreeArgs},
    internal::{
        branch::Branch,
        config::ConfigKv,
        db,
        head::Head,
        model::{object_index, reference},
    },
    utils::{
        d1_client::{
            AgentCaptureGenerationManifest, AgentCaptureGenerationRow,
            AgentCaptureRestoreCatalogRows, AgentCheckpointPruneTombstoneRow, AgentCheckpointV2Row,
            AgentImportTombstoneRow, AgentSessionV2Row, AgentSubagentContentClaimRow,
            AgentSubagentContentRevisionRow, AgentSubagentLinkRow, D1Client, ObjectIndexRow,
        },
        error::{CliError, CliResult, StableErrorCode, emit_warning},
        output::{OutputConfig, ProgressMode, emit_json_data},
        path,
        storage::{
            Storage, local::LocalStorage, publish_storage::PublishStorage, remote::RemoteStorage,
        },
        util,
    },
};

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
    /// Hard error (table-existence query, ensure-table call, ...).
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
struct CloudRestoreOutput {
    repo_id: String,
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
    fn on_agent_capture_session_warning(&self, session_id: &str, err: &str) {
        eprintln!("warning: agent_session {session_id} upsert failed: {err}");
    }
    fn on_agent_capture_checkpoint_warning(&self, checkpoint_id: &str, err: &str) {
        eprintln!("warning: agent_checkpoint {checkpoint_id} upsert failed: {err}");
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
    fn on_agent_capture_session_warning(&self, session_id: &str, err: &str) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.agent_capture.session_warning",
            "session_id": session_id,
            "error": err,
        }));
    }
    fn on_agent_capture_checkpoint_warning(&self, checkpoint_id: &str, err: &str) {
        Self::emit(serde_json::json!({
            "event": "cloud_sync.agent_capture.checkpoint_warning",
            "checkpoint_id": checkpoint_id,
            "error": err,
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
                        CloudError::PartialTransfer(format!(
                            "agent capture mirror failed: {error}"
                        )),
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
    /// Anything else — kept as the original detail string.
    Generic(String),
}

type CloudResult<T> = std::result::Result<T, CloudError>;

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

fn render_cloud_restore_output(
    result: &CloudRestoreOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("cloud.restore", result, output);
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
        return Err(CloudError::PartialTransfer(format!(
            "agent capture mirror failed: {error}"
        )));
    }
    Ok(())
}

/// Phase 1 helper extracted from `execute_sync`.
///
/// Runs the full `libra cloud sync` flow without printing directly to
/// stdout / stderr: env validation → D1 / R2 init → object stream →
/// metadata refresh → agent_capture mirror. Human-readable progress
/// flows through the [`CloudSyncProgress`] trait so callers can plug
/// in their own renderer (`ConsoleCloudSyncProgress` for the legacy
/// CLI, a quieter or structured one for `libra publish` later).
///
/// Returns a [`CloudSyncReport`] for the completed run. Hard errors
/// (env, D1, R2, repo-id, db-query, metadata-sync) short-circuit as
/// `Err`. Per-object failures are captured in `failed_count` and skip
/// the metadata + agent_capture phases (preserving the pre-Phase-1
/// "block follow-up work on object failure" gate).
pub(crate) async fn run_cloud_sync(
    ctx: CloudSyncContext,
    progress: &dyn CloudSyncProgress,
) -> CloudResult<CloudSyncReport> {
    if ctx.batch_size < 1 {
        return Err(CloudError::Generic(
            "Batch size must be at least 1".to_string(),
        ));
    }

    progress.on_starting();

    validate_cloud_backup_env(false).await?;

    // Initialize D1 client.
    let d1_client = D1Client::from_env()
        .await
        .map_err(|e| CloudError::D1(format!("D1 client error: {}", e.message)))?;

    // Ensure D1 table exists before any operations.
    d1_client
        .ensure_object_index_table()
        .await
        .map_err(|e| CloudError::D1(format!("Failed to create D1 table: {}", e.message)))?;

    // Get database connection.
    let db_conn = db::get_db_conn_instance().await;

    // Check if object_index table exists locally, create if not.
    let builder = db_conn.get_database_backend();
    let schema = Schema::new(builder);
    let stmt = schema
        .create_table_from_entity(object_index::Entity)
        .if_not_exists()
        .to_owned();

    let _ = db_conn.execute_raw(builder.build(&stmt)).await;

    let repo_id = ensure_repo_id().await;

    // Determine project name from config 'cloud.name' or current directory name.
    let project_name = ConfigKv::get("cloud.name")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .unwrap_or_else(|| {
            util::working_dir()
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown-project".to_string())
        });

    // Ensure repositories table exists.
    d1_client.ensure_repositories_table().await.map_err(|e| {
        CloudError::D1(format!(
            "Failed to create repositories table: {}",
            e.message
        ))
    })?;

    // Upsert repository info.
    let repo_row = d1_client
        .upsert_repository(&repo_id, &project_name)
        .await
        .map_err(|e| {
            if e.message.contains("UNIQUE constraint failed: repositories.name") {
                CloudError::NameAlreadyTaken(format!(
                    "Project name '{}' is already taken by another repository. Please choose a different name in cloud.name config.",
                    project_name
                ))
            } else {
                CloudError::D1(format!("Failed to upsert repository: {}", e.message))
            }
        })?;

    // Verify repo_id matches (to detect name conflict).
    if repo_row.repo_id != repo_id {
        return Err(CloudError::NameAlreadyTaken(format!(
            "Project name '{}' is already taken by another repository (ID: {}). Please choose a different name in cloud.name config.",
            project_name, repo_row.repo_id
        )));
    }

    // Query unsynced objects.
    let query = if ctx.force {
        object_index::Entity::find().filter(object_index::Column::RepoId.eq(&repo_id))
    } else {
        object_index::Entity::find()
            .filter(object_index::Column::RepoId.eq(&repo_id))
            .filter(object_index::Column::IsSynced.eq(0))
    };

    let unsynced_objects = query
        .all(&db_conn)
        .await
        .map_err(|e| CloudError::Generic(format!("Database query failed: {}", e)))?;

    // Initialize R2 storage.
    let r2_storage = create_r2_storage(&repo_id).await?;

    let total_unsynced = unsynced_objects.len();

    if unsynced_objects.is_empty() {
        progress.on_no_objects();
        let metadata = sync_metadata(&db_conn, &r2_storage, progress).await?;
        // CEX-EntireIO §10.2: even when there are no new git objects to
        // ship, the agent_session/agent_checkpoint catalog may have new
        // rows from local hook ingestion. Mirror them on every sync.
        let agent_capture =
            match sync_agent_capture_tables(&db_conn, &d1_client, &r2_storage, &repo_id, progress)
                .await
            {
                Ok(outcome) => outcome,
                Err(err) => {
                    let err = err.to_string();
                    progress.on_agent_capture_warning(&err);
                    AgentCaptureSyncOutcome::Failed { error: err }
                }
            };
        return Ok(CloudSyncReport {
            repo_id,
            project_name,
            total_unsynced: 0,
            synced_count: 0,
            failed_count: 0,
            metadata,
            agent_capture,
        });
    }

    progress.on_object_total(total_unsynced);

    // Initialize local storage for reading objects.
    let objects_path = path::objects();
    let local_storage = LocalStorage::new(objects_path);

    let mut synced_count = 0usize;
    let mut failed_count = 0usize;

    // Process in batches.
    for batch in unsynced_objects.chunks(ctx.batch_size) {
        // Parse the batch's hashes once, then run ONE bounded-concurrency dedup
        // pre-check (`exist_batch`, lore.md §0.6) instead of a HEAD per object, so
        // objects already in R2 are skipped without a serial round-trip each.
        let parsed: Vec<CloudResult<ObjectHash>> =
            batch.iter().map(parse_object_index_hash).collect();
        let probe_hashes: Vec<ObjectHash> = parsed
            .iter()
            .filter_map(|r| r.as_ref().ok().copied())
            .collect();
        let already_in_remote: std::collections::HashSet<ObjectHash> = {
            let flags = r2_storage.exist_batch(&probe_hashes).await;
            probe_hashes
                .iter()
                .copied()
                .zip(flags)
                .filter_map(|(hash, exists)| exists.then_some(hash))
                .collect()
        };

        for (obj, hash_result) in batch.iter().zip(parsed) {
            let result = match hash_result {
                Ok(hash) => {
                    let remote_has = already_in_remote.contains(&hash);
                    sync_single_object(
                        obj,
                        &local_storage,
                        &r2_storage,
                        &d1_client,
                        hash,
                        remote_has,
                    )
                    .await
                }
                Err(err) => Err(err),
            };

            match result {
                Ok(_) => {
                    // Update local is_synced flag.
                    let mut active: object_index::ActiveModel = obj.clone().into();
                    active.is_synced = Set(1);
                    if let Err(e) = active.update(&db_conn).await {
                        progress.on_local_status_warning(&obj.o_id, &e.to_string());
                    }
                    synced_count += 1;
                }
                Err(e) => {
                    let err = e.to_string();
                    progress.on_object_error(&obj.o_id, &err);
                    failed_count += 1;
                }
            }
        }
        progress.on_batch_progress(synced_count, total_unsynced, failed_count);
    }

    progress.on_sync_complete(synced_count, failed_count);

    if failed_count > 0 {
        return Ok(CloudSyncReport {
            repo_id,
            project_name,
            total_unsynced,
            synced_count,
            failed_count,
            metadata: MetadataSyncOutcome::NotRun,
            agent_capture: AgentCaptureSyncOutcome::NotRun,
        });
    }

    let metadata = sync_metadata(&db_conn, &r2_storage, progress).await?;
    // CEX-EntireIO §10.2: append agent capture catalog mirroring at the
    // tail of the sync flow per the plan. The report retains the detailed
    // phase outcome; `execute_sync` turns a failed mirror into a non-zero,
    // actionable partial-transfer result after rendering progress.
    let agent_capture = match sync_agent_capture_tables(
        &db_conn,
        &d1_client,
        &r2_storage,
        &repo_id,
        progress,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(err) => {
            let err = err.to_string();
            progress.on_agent_capture_warning(&err);
            AgentCaptureSyncOutcome::Failed { error: err }
        }
    };

    Ok(CloudSyncReport {
        repo_id,
        project_name,
        total_unsynced,
        synced_count,
        failed_count,
        metadata,
        agent_capture,
    })
}

/// Parse an `object_index` model's hex `o_id` into an `ObjectHash`.
fn parse_object_index_hash(obj: &object_index::Model) -> CloudResult<ObjectHash> {
    let bytes =
        hex::decode(&obj.o_id).map_err(|e| CloudError::Generic(format!("Invalid hash: {}", e)))?;
    ObjectHash::from_bytes(&bytes)
        .map_err(|e| CloudError::Generic(format!("Invalid object hash: {}", e)))
}

/// Sync a single object: R2 first (idempotent), then D1.
///
/// `remote_has` is the result of the batch dedup pre-check (`exist_batch`,
/// lore.md §0.6), so this no longer issues a per-object HEAD — the whole batch's
/// existence is probed up front in one bounded-concurrency call.
async fn sync_single_object(
    obj: &object_index::Model,
    local_storage: &LocalStorage,
    r2_storage: &RemoteStorage,
    d1_client: &D1Client,
    hash: ObjectHash,
    remote_has: bool,
) -> CloudResult<()> {
    // Phase 1: Upload to R2 only if the dedup pre-check says it is absent
    // (idempotent - same hash would just overwrite).
    if !remote_has {
        let (data, obj_type) = local_storage
            .get(&hash)
            .await
            .map_err(|e| CloudError::Generic(format!("Failed to read local object: {}", e)))?;

        r2_storage
            .put(&hash, &data, obj_type)
            .await
            .map_err(|e| CloudError::R2(format!("R2 upload failed: {}", e)))?;
    }

    // Phase 2: Upsert to D1 (idempotent - will update if exists)
    d1_client
        .upsert_object_index(
            &obj.o_id,
            &obj.o_type,
            obj.o_size,
            &obj.repo_id,
            obj.created_at,
        )
        .await
        .map_err(|e| CloudError::D1(format!("D1 write failed: {}", e.message)))?;

    Ok(())
}

/// Restore the Git objects described by D1 `object_index` rows from R2
/// into local object storage.
///
/// The helper preserves the legacy cloud-restore semantics: object-level
/// transfer, hash, and local-write failures are accumulated in the report,
/// while malformed hex in D1 remains a hard metadata error.
pub(crate) async fn restore_indexed_objects_from_remote(
    indexes: &[ObjectIndexRow],
    r2_storage: &RemoteStorage,
    local_storage: &LocalStorage,
) -> CloudResult<ObjectRestoreReport> {
    let mut report = ObjectRestoreReport::default();

    for idx in indexes {
        let decoded = hex::decode(&idx.o_id)
            .map_err(|e| CloudError::Generic(format!("Invalid hash: {}", e)))?;
        let hash = match ObjectHash::from_bytes(&decoded) {
            Ok(hash) => hash,
            Err(e) => {
                report
                    .warnings
                    .push(format!("error: invalid object hash '{}': {}", idx.o_id, e));
                report.failed += 1;
                continue;
            }
        };

        if let Ok((data, object_type)) = local_storage.get(&hash).await
            && ObjectHash::from_type_and_data(object_type, &data) == hash
        {
            report.skipped += 1;
            continue;
        }

        // lore.md 2.5: never RESTORE an intentionally-obliterated object from
        // the durable tier (拒绝重建). Fail CLOSED (Codex P1): if the tombstone
        // table cannot be read, do NOT restore (an unreadable table must not
        // let an obliterated payload resurrect) — skip with a warning.
        match crate::internal::obliteration::ObliterationStore::lookup(&hash).await {
            Ok(Some(_)) => {
                report.skipped += 1;
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                report.warnings.push(format!(
                    "warning: cannot verify obliteration tombstone for {}; not restoring: {e}",
                    idx.o_id
                ));
                report.skipped += 1;
                continue;
            }
        }

        match r2_storage.get(&hash).await {
            Ok((data, obj_type)) => {
                let computed = ObjectHash::from_type_and_data(obj_type, &data);
                if computed != hash {
                    report.warnings.push(format!(
                        "warning: hash mismatch for {}: expected {}, got {}",
                        idx.o_id, hash, computed
                    ));
                    report.failed += 1;
                    continue;
                }

                if let Err(e) = local_storage.put(&hash, &data, obj_type).await {
                    report
                        .warnings
                        .push(format!("error: failed to save object {}: {}", idx.o_id, e));
                    report.failed += 1;
                    continue;
                }
                report.downloaded += 1;
            }
            Err(e) => {
                report
                    .warnings
                    .push(format!("error: failed to download {}: {}", idx.o_id, e));
                report.failed += 1;
            }
        }
    }

    Ok(report)
}

async fn run_cloud_restore(args: RestoreArgs) -> CloudResult<CloudRestoreOutput> {
    validate_cloud_backup_env(args.metadata_only).await?;

    let d1_client = D1Client::from_env()
        .await
        .map_err(|e| CloudError::D1(format!("D1 client error: {}", e.message)))?;

    let repo_id = if let Some(name) = &args.name {
        d1_client.ensure_repositories_table().await.map_err(|e| {
            CloudError::D1(format!(
                "Failed to ensure repositories table: {}",
                e.message
            ))
        })?;

        let id = d1_client
            .get_repo_id_by_name(name)
            .await
            .map_err(|e| CloudError::D1(format!("Failed to resolve repo name: {}", e.message)))?;
        id.ok_or_else(|| {
            CloudError::NameNotFound(format!("Repository with name '{}' not found", name))
        })?
    } else {
        args.repo_id
            .clone()
            .ok_or_else(|| CloudError::NameNotFound("repo_id is required".to_string()))?
    };

    let indexes = d1_client
        .get_object_indexes(&repo_id)
        .await
        .map_err(|e| CloudError::D1(format!("Failed to query D1: {}", e.message)))?;

    let db_conn = db::get_db_conn_instance().await;
    if !args.metadata_only {
        preflight_agent_capture_prune_fences(&db_conn, &d1_client, &repo_id).await?;
    }
    for idx in &indexes {
        let existing = object_index::Entity::find()
            .filter(object_index::Column::OId.eq(&idx.o_id))
            .filter(object_index::Column::RepoId.eq(&idx.repo_id))
            .one(&db_conn)
            .await
            .map_err(|e| CloudError::Generic(format!("DB error: {}", e)))?;

        if let Some(existing_model) = existing {
            let mut active: object_index::ActiveModel = existing_model.into();
            active.is_synced = Set(1);
            if let Err(e) = active.update(&db_conn).await {
                cli_error!(e, "warning: failed to update index for {}", idx.o_id);
            }
        } else {
            let entry = object_index::ActiveModel {
                o_id: Set(idx.o_id.clone()),
                o_type: Set(idx.o_type.clone()),
                o_size: Set(idx.o_size),
                repo_id: Set(idx.repo_id.clone()),
                created_at: Set(idx.created_at),
                is_synced: Set(1),
                ..Default::default()
            };

            if let Err(e) = entry.insert(&db_conn).await {
                cli_error!(e, "warning: failed to insert index for {}", idx.o_id);
            }
        }
    }

    let _ = ConfigKv::set("libra.repoid", &repo_id, false).await;

    if args.metadata_only {
        return Ok(CloudRestoreOutput {
            repo_id,
            metadata_only: true,
            total_objects: indexes.len(),
            indexes_restored: indexes.len(),
            object_restore: None,
            metadata: CloudRestoreMetadataOutput {
                status: "not_run".to_string(),
                warning: None,
            },
            agent_capture: CloudRestoreAgentCaptureOutput {
                status: "not_run".to_string(),
            },
        });
    }

    let r2_storage = create_r2_storage(&repo_id).await?;
    let objects_path = path::objects();
    let local_storage = LocalStorage::new(objects_path);

    let object_report =
        restore_indexed_objects_from_remote(&indexes, &r2_storage, &local_storage).await?;
    for warning in &object_report.warnings {
        eprintln!("{warning}");
    }
    if object_report.failed > 0 {
        return Err(CloudError::PartialTransfer(format!(
            "{} objects failed to restore",
            object_report.failed
        )));
    }

    let (metadata, deferred_capture_refs) = match restore_metadata(&db_conn, &r2_storage).await {
        Ok(deferred_capture_refs) => (
            CloudRestoreMetadataOutput {
                status: "restored".to_string(),
                warning: None,
            },
            deferred_capture_refs,
        ),
        Err(e) => {
            emit_warning(format!("failed to restore metadata: {}", e));
            (
                CloudRestoreMetadataOutput {
                    status: "warning".to_string(),
                    warning: Some(e.to_string()),
                },
                Vec::new(),
            )
        }
    };

    let head_commit = Head::current_commit_result()
        .await
        .map_err(|error| CloudError::Generic(format!("failed to resolve HEAD commit: {error}")))?;
    if head_commit.is_some() {
        let _ = restore_worktree_to_head(false).await;
    } else {
        let main_branch = Branch::find_branch_result("main", None)
            .await
            .map_err(|error| {
                CloudError::Generic(format!("failed to resolve main branch: {error}"))
            })?;
        if main_branch.is_some() {
            Head::update(Head::Branch("main".to_string()), None).await;
            let _ = restore_worktree_to_head(false).await;
        }
    }

    let capture_outcome = restore_agent_capture_from_d1(&db_conn, &d1_client, &repo_id, false)
        .await
        .map_err(|error| CloudError::D1(format!("agent capture restore failed: {error}")))?;
    restore_legacy_capture_refs_if_unowned(&db_conn, deferred_capture_refs, capture_outcome)
        .await?;

    Ok(CloudRestoreOutput {
        repo_id,
        metadata_only: false,
        total_objects: indexes.len(),
        indexes_restored: indexes.len(),
        object_restore: Some(CloudRestoreObjectOutput {
            downloaded: object_report.downloaded,
            skipped: object_report.skipped,
            failed: object_report.failed,
        }),
        metadata,
        agent_capture: CloudRestoreAgentCaptureOutput {
            status: "restored".to_string(),
        },
    })
}

/// Execute restore command - resolves project name (if provided) and restores from D1/R2.
async fn execute_restore(args: RestoreArgs) -> CloudResult<()> {
    validate_cloud_backup_env(args.metadata_only).await?;

    // Initialize D1 client
    let d1_client = D1Client::from_env()
        .await
        .map_err(|error| CloudError::D1(format!("D1 client error: {}", error.message)))?;

    let repo_id = if let Some(name) = &args.name {
        // Ensure repositories table exists before resolving name
        // This handles cases where the D1 database is old/uninitialized and missing the table
        d1_client
            .ensure_repositories_table()
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "Failed to ensure repositories table: {}",
                    error.message
                ))
            })?;

        let id = d1_client.get_repo_id_by_name(name).await.map_err(|error| {
            CloudError::D1(format!("Failed to resolve repo name: {}", error.message))
        })?;
        id.ok_or_else(|| {
            CloudError::NameNotFound(format!("Repository with name '{}' not found", name))
        })?
    } else {
        args.repo_id
            .clone()
            .ok_or_else(|| CloudError::NameNotFound("repo_id is required".to_string()))?
    };

    println!("Starting restore for repo: {}", repo_id);

    // Get object indexes from D1
    let indexes = d1_client
        .get_object_indexes(&repo_id)
        .await
        .map_err(|error| CloudError::D1(format!("Failed to query D1: {}", error.message)))?;

    println!("Found {} objects in cloud for repo.", indexes.len());

    if indexes.is_empty() {
        println!("No objects found for this repo.");
    }

    // Get database connection and insert indexes
    let db_conn = db::get_db_conn_instance().await;
    if !args.metadata_only {
        preflight_agent_capture_prune_fences(&db_conn, &d1_client, &repo_id).await?;
    }

    for idx in &indexes {
        // Check if exists
        let existing = object_index::Entity::find()
            .filter(object_index::Column::OId.eq(&idx.o_id))
            .filter(object_index::Column::RepoId.eq(&idx.repo_id))
            .one(&db_conn)
            .await
            .map_err(|error| CloudError::Generic(format!("DB error: {error}")))?;

        if let Some(existing_model) = existing {
            let mut active: object_index::ActiveModel = existing_model.into();
            active.is_synced = Set(1);
            if let Err(e) = active.update(&db_conn).await {
                cli_error!(e, "warning: failed to update index for {}", idx.o_id);
            }
        } else {
            let entry = object_index::ActiveModel {
                o_id: Set(idx.o_id.clone()),
                o_type: Set(idx.o_type.clone()),
                o_size: Set(idx.o_size),
                repo_id: Set(idx.repo_id.clone()),
                created_at: Set(idx.created_at),
                is_synced: Set(1), // Already synced since we're restoring from cloud
                ..Default::default()
            };

            if let Err(e) = entry.insert(&db_conn).await {
                cli_error!(e, "warning: failed to insert index for {}", idx.o_id);
            }
        }
    }

    println!(
        "Restored {} object indexes to local database.",
        indexes.len()
    );

    // Update local config with restored repo_id
    let _ = ConfigKv::set("libra.repoid", &repo_id, false).await;

    if args.metadata_only {
        println!("Metadata-only restore complete.");
        return Ok(());
    }

    // Download objects from R2
    let r2_storage = create_r2_storage(&repo_id).await?;
    let objects_path = path::objects();
    let local_storage = LocalStorage::new(objects_path);

    let report = restore_indexed_objects_from_remote(&indexes, &r2_storage, &local_storage).await?;
    for warning in &report.warnings {
        eprintln!("{warning}");
    }

    println!(
        "Restore complete: {} downloaded, {} skipped (already exist), {} failed",
        report.downloaded, report.skipped, report.failed
    );

    if report.failed > 0 {
        Err(CloudError::PartialTransfer(format!(
            "{} objects failed to restore",
            report.failed
        )))
    } else {
        // Restore metadata
        let deferred_capture_refs = match restore_metadata(&db_conn, &r2_storage).await {
            Ok(deferred) => deferred,
            Err(e) => {
                emit_warning(format!("failed to restore metadata: {}", e));
                Vec::new()
            }
        };

        // Post-restore: update HEAD and restore worktree if we're in a fresh repo state.
        // We do this BEFORE the agent-capture restore so that a strict
        // agent-capture failure (Codex Q2: hard-fail on partial restore)
        // doesn't leave the user with a populated objects/refs but no
        // worktree. The agent_session / agent_checkpoint catalogue is
        // metadata about external agent runs — it's not blocking for the
        // user to start working in the restored tree (Codex Q3).

        // Check if HEAD has a commit (either restored or existing)
        let head_commit = Head::current_commit_result().await.map_err(|error| {
            CloudError::Generic(format!("failed to resolve HEAD commit: {error}"))
        })?;

        if let Some(commit) = head_commit {
            println!("Restoring working directory to HEAD ({})", commit);
            let _ = restore_worktree_to_head(true).await;
        } else {
            println!("Restoring working directory (fallback)...");

            // Try to find 'main' branch in references
            // We look for 'main' branch in the reference table as a fallback
            let main_branch = Branch::find_branch_result("main", None)
                .await
                .map_err(|error| {
                    CloudError::Generic(format!("failed to resolve main branch: {error}"))
                })?;

            if let Some(branch) = main_branch {
                println!("Found main branch: {}", branch.commit);

                // Update HEAD to point to main
                Head::update(Head::Branch("main".to_string()), None).await;

                let _ = restore_worktree_to_head(true).await;
            } else {
                println!("No HEAD commit or main branch found. Skipping worktree restore.");
            }
        }

        // CEX-EntireIO §14.3 acceptance: pull `agent_session` /
        // `agent_checkpoint` rows back from D1 so the new machine sees the
        // captured-agent listing without having to re-ingest hooks. This
        // runs LAST (after worktree restore) per Codex Q3 — the inner
        // helper is strict (Q2), so propagating its error here surfaces
        // partial-restore problems to the caller without blocking the
        // worktree materialization that runs above.
        let capture_outcome = restore_agent_capture_from_d1(&db_conn, &d1_client, &repo_id, true)
            .await
            .map_err(|e| CloudError::D1(format!("agent capture restore failed: {}", e)))?;
        restore_legacy_capture_refs_if_unowned(&db_conn, deferred_capture_refs, capture_outcome)
            .await?;

        Ok(())
    }
}

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
            CloudError::D1(format!(
                "probe remote agent-capture generation before prune preflight: {}",
                error.message
            ))
        })?;
    if !generation_table_present {
        let has_capture_rows = d1_client
            .agent_capture_catalog_has_rows(repo_id)
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "probe unmanifested remote capture before prune preflight: {}",
                    error.message
                ))
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
                CloudError::D1(format!(
                    "read remote agent-capture generation before prune preflight: {}",
                    error.message
                ))
            })?;
        let Some(before) = before else {
            let has_capture_rows = d1_client
                .agent_capture_catalog_has_rows(repo_id)
                .await
                .map_err(|error| {
                    CloudError::D1(format!(
                        "probe remote capture without a repo generation before prune preflight: {}",
                        error.message
                    ))
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
                CloudError::D1(format!(
                    "compare local checkpoint prune fences with the remote capture: {}",
                    error.message
                ))
            })?;
        let after = d1_client
            .get_agent_capture_generation(repo_id)
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "recheck remote agent-capture generation after prune preflight: {}",
                    error.message
                ))
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
    if let Some(checkpoint_id) = local_checkpoint_ids
        .iter()
        .find(|checkpoint_id| remote_checkpoint_ids.contains(checkpoint_id.as_str()))
    {
        return Err(CloudError::PartialTransfer(format!(
            "remote checkpoint {checkpoint_id} was already pruned locally; run `libra cloud sync` to publish the prune tombstone before restoring"
        )));
    }
    Ok(())
}

async fn restore_worktree_to_head(render_human: bool) -> CloudResult<()> {
    let restore_args = RestoreWorktreeArgs {
        overlay: false,
        no_overlay: false,
        ours: false,
        theirs: false,
        ignore_unmerged: false,
        merge: false,
        conflict: None,
        pathspec: vec![".".to_string()], // restore everything
        source: Some("HEAD".to_string()),
        worktree: true,
        staged: true,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_progress: false,
    };

    if let Err(e) = restore_cmd::execute_checked(restore_args).await {
        emit_warning(format!("failed to restore worktree files: {}", e));
        Err(CloudError::Generic(format!(
            "failed to restore worktree files: {e}"
        )))
    } else {
        if render_human {
            println!("Successfully restored working directory files.");
        }
        Ok(())
    }
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

/// Create publish arbitrary-object storage from the same R2
/// environment/config surface used by `libra cloud sync`.
pub(crate) async fn create_publish_storage(
    repo_id: &str,
    site_id: &str,
) -> CloudResult<PublishStorage> {
    let local_db_path = cloud_local_db_path()?;
    let store = create_r2_object_store_for_db_path(&local_db_path).await?;
    PublishStorage::new(store, repo_id, site_id)
        .map_err(|e| CloudError::Generic(format!("failed to build publish storage prefix: {e}")))
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

fn calculate_metadata_hash(json: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    hasher.finish()
}

async fn sync_metadata(
    db_conn: &sea_orm::DatabaseConnection,
    r2_storage: &RemoteStorage,
    progress: &dyn CloudSyncProgress,
) -> CloudResult<MetadataSyncOutcome> {
    progress.on_metadata_starting();
    let references = reference::Entity::find()
        .all(db_conn)
        .await
        .map_err(|e| CloudError::Generic(format!("Failed to fetch references: {}", e)))?;

    // Sort to ensure deterministic output for hashing.
    let mut sorted_refs = references;
    sorted_refs.sort_by(|a, b| {
        let a_kind = format!("{:?}", a.kind);
        let b_kind = format!("{:?}", b.kind);
        let a_key = (&a.name, &a.remote, a_kind);
        let b_key = (&b.name, &b.remote, b_kind);
        a_key.cmp(&b_key)
    });

    let json = serde_json::to_vec(&sorted_refs)
        .map_err(|e| CloudError::Generic(format!("Failed to serialize metadata: {}", e)))?;

    let current_hash = calculate_metadata_hash(&json);

    // Check if hash matches last sync.
    if let Some(stored) = ConfigKv::get("cloud.metadata_hash")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        && let Ok(stored_hash) = stored.parse::<u64>()
        && stored_hash == current_hash
    {
        progress.on_metadata_skipped();
        return Ok(MetadataSyncOutcome::Skipped);
    }

    r2_storage
        .put_metadata(&json)
        .await
        .map_err(|e| CloudError::R2(format!("Failed to upload metadata: {}", e)))?;

    // Update stored hash.
    let _ = ConfigKv::set("cloud.metadata_hash", &current_hash.to_string(), false).await;

    progress.on_metadata_synced(sorted_refs.len());
    Ok(MetadataSyncOutcome::Synced {
        references: sorted_refs.len(),
    })
}

const AGENT_CAPTURE_LOCAL_PAGE_SIZE: usize = 256;
const AGENT_CAPTURE_MAX_ROWS_PER_TABLE: usize = 100_000;
const AGENT_CAPTURE_RESTORE_MAX_ROWS: usize = 100_000;
const AGENT_CAPTURE_D1_BATCH_SIZE: usize = 128;
const AGENT_CAPTURE_OBJECT_VERIFY_CONCURRENCY: usize = 32;
const AGENT_CAPTURE_CLOUD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

fn agent_capture_batches<T>(rows: &[T]) -> std::slice::Chunks<'_, T> {
    rows.chunks(AGENT_CAPTURE_D1_BATCH_SIZE)
}

fn agent_capture_object_verification_batches<T>(rows: &[T]) -> std::slice::Chunks<'_, T> {
    rows.chunks(AGENT_CAPTURE_OBJECT_VERIFY_CONCURRENCY)
}

async fn load_local_agent_capture_cloud_base(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
) -> CloudResult<Option<i64>> {
    use sea_orm::Statement;

    let backend = db_conn.get_database_backend();
    let table_present = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'agent_capture_cloud_base' LIMIT 1",
            [],
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!("probe local agent-capture cloud base: {error}"))
        })?
        .is_some();
    if !table_present {
        return Ok(None);
    }
    db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT remote_generation FROM agent_capture_cloud_base WHERE repo_id = ?",
            [repo_id.into()],
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!("read local agent-capture cloud base: {error}"))
        })?
        .map(|row| {
            row.try_get_by("remote_generation").map_err(|error| {
                CloudError::Generic(format!("decode local agent-capture cloud base: {error}"))
            })
        })
        .transpose()
}

async fn store_local_agent_capture_cloud_base(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    remote_generation: i64,
) -> CloudResult<()> {
    use sea_orm::Statement;

    db_conn
        .execute_raw(Statement::from_sql_and_values(
            db_conn.get_database_backend(),
            "INSERT INTO agent_capture_cloud_base (repo_id, remote_generation, updated_at)
             VALUES (?, ?, ?)
             ON CONFLICT(repo_id) DO UPDATE SET
                remote_generation = excluded.remote_generation,
                updated_at = excluded.updated_at
             WHERE excluded.remote_generation > agent_capture_cloud_base.remote_generation",
            [
                repo_id.into(),
                remote_generation.into(),
                chrono::Utc::now().timestamp_millis().into(),
            ],
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!("record local agent-capture cloud base: {error}"))
        })?;
    Ok(())
}

#[derive(Debug, Default, Eq, PartialEq)]
struct AgentCaptureSnapshot {
    sessions: Vec<AgentSessionV2Row>,
    checkpoints: Vec<AgentCheckpointV2Row>,
    claims: Vec<AgentSubagentContentClaimRow>,
    revisions: Vec<AgentSubagentContentRevisionRow>,
    links: Vec<AgentSubagentLinkRow>,
    prune_tombstones: Vec<AgentCheckpointPruneTombstoneRow>,
    /// PD-03 session-erasure tombstones from the local
    /// `agent_import_tombstone` table.
    import_tombstones: Vec<AgentImportTombstoneRow>,
    required_oids: HashSet<String>,
    traces_head: Option<String>,
}

/// PD-03: union local and remote session tombstones, keeping the newest
/// `erased_at` (and any known fingerprint) per provider identity —
/// delete/restore replays stay idempotent.
fn merge_import_tombstones(
    local: &[AgentImportTombstoneRow],
    remote: &[AgentImportTombstoneRow],
) -> Vec<AgentImportTombstoneRow> {
    let mut merged: std::collections::BTreeMap<(String, String), AgentImportTombstoneRow> =
        std::collections::BTreeMap::new();
    for row in remote.iter().chain(local.iter()) {
        let key = (row.agent_kind.clone(), row.provider_session_id.clone());
        match merged.get_mut(&key) {
            None => {
                merged.insert(key, row.clone());
            }
            Some(existing) => {
                if row.erased_at > existing.erased_at {
                    let fingerprint = existing
                        .source_fingerprint
                        .clone()
                        .or_else(|| row.source_fingerprint.clone());
                    *existing = row.clone();
                    existing.source_fingerprint = row.source_fingerprint.clone().or(fingerprint);
                } else if existing.source_fingerprint.is_none() {
                    existing.source_fingerprint = row.source_fingerprint.clone();
                }
            }
        }
    }
    merged.into_values().collect()
}

fn agent_capture_catalog_row_count(snapshot: &AgentCaptureSnapshot) -> CloudResult<usize> {
    [
        snapshot.sessions.len(),
        snapshot.checkpoints.len(),
        snapshot.prune_tombstones.len(),
        snapshot.import_tombstones.len(),
        snapshot.claims.len(),
        snapshot.revisions.len(),
        snapshot.links.len(),
    ]
    .into_iter()
    .try_fold(0_usize, |total, count| {
        total.checked_add(count).ok_or_else(|| {
            CloudError::PartialTransfer(
                "agent-capture catalog row count exceeds the platform size range".to_string(),
            )
        })
    })
}

fn validate_agent_capture_restore_row_budget(
    snapshot: &AgentCaptureSnapshot,
    object_index_rows: usize,
) -> CloudResult<()> {
    let total = agent_capture_catalog_row_count(snapshot)?
        .checked_add(object_index_rows)
        .ok_or_else(|| {
            CloudError::PartialTransfer(
                "agent-capture restore row count exceeds the platform size range".to_string(),
            )
        })?;
    if total > AGENT_CAPTURE_RESTORE_MAX_ROWS {
        return Err(CloudError::PartialTransfer(format!(
            "agent-capture catalog and object manifest require {total} rows, exceeding the aggregate {}-row restore safety bound",
            AGENT_CAPTURE_RESTORE_MAX_ROWS
        )));
    }
    Ok(())
}

struct AgentCaptureRestoreRows<'a> {
    sessions: &'a [AgentSessionV2Row],
    checkpoints: &'a [AgentCheckpointV2Row],
    claims: &'a [AgentSubagentContentClaimRow],
    revisions: &'a [AgentSubagentContentRevisionRow],
    links: &'a [AgentSubagentLinkRow],
    traces_head: Option<&'a str>,
    /// Numeric row revisions are meaningful only when this clone recorded the
    /// completed remote generation as its base. Without that lineage proof, a
    /// larger local counter may be an unrelated clone's divergent history.
    remote_is_known_ancestor: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentCaptureObjectManifestScope {
    CheckpointProjection,
    FullRemoteIndex,
}

impl AgentCaptureObjectManifestScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::CheckpointProjection => "checkpoint_projection",
            Self::FullRemoteIndex => "full_remote_index",
        }
    }

    fn parse(value: Option<&str>) -> CloudResult<Self> {
        match value {
            Some("checkpoint_projection") => Ok(Self::CheckpointProjection),
            Some("full_remote_index") => Ok(Self::FullRemoteIndex),
            _ => Err(CloudError::PartialTransfer(
                "agent-capture generation has no supported object-index scope; run a current-version cloud sync"
                    .to_string(),
            )),
        }
    }
}

fn agent_capture_object_index_digest(indexes: &[ObjectIndexRow]) -> CloudResult<(String, i64)> {
    let mut rows = indexes.iter().collect::<Vec<_>>();
    rows.sort_by(|left, right| left.o_id.cmp(&right.o_id));
    let mut previous: Option<&str> = None;
    let mut digest = Sha256::new();
    for row in rows {
        if previous == Some(row.o_id.as_str()) {
            return Err(CloudError::Generic(format!(
                "remote object index contains duplicate oid {}",
                row.o_id
            )));
        }
        previous = Some(row.o_id.as_str());
        for value in [row.o_id.as_bytes(), row.o_type.as_bytes()] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        }
        digest.update(row.o_size.to_be_bytes());
    }
    let count = i64::try_from(indexes.len()).map_err(|error| {
        CloudError::Generic(format!("object-index count cannot be represented: {error}"))
    })?;
    Ok((hex::encode(digest.finalize()), count))
}

fn validate_checkpoint_object_index_roots(
    checkpoints: &[AgentCheckpointV2Row],
    indexes: &[ObjectIndexRow],
    side: &str,
) -> CloudResult<()> {
    let indexed_oids = indexes
        .iter()
        .map(|row| row.o_id.as_str())
        .collect::<HashSet<_>>();
    for checkpoint in checkpoints {
        for (label, oid) in [
            ("traces commit", checkpoint.traces_commit.as_str()),
            ("tree", checkpoint.tree_oid.as_str()),
            ("metadata blob", checkpoint.metadata_blob_oid.as_str()),
        ] {
            if !indexed_oids.contains(oid) {
                return Err(CloudError::PartialTransfer(format!(
                    "{side} checkpoint {} references {label} object {oid}, but the fenced object index does not contain it",
                    checkpoint.checkpoint_id
                )));
            }
        }
    }
    Ok(())
}

async fn load_local_capture_pages<C: ConnectionTrait>(
    conn: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
    label: &str,
    remaining_rows: &mut usize,
) -> CloudResult<Vec<sea_orm::QueryResult>> {
    let mut rows = Vec::new();
    let mut offset = 0_usize;
    loop {
        // Read one sentinel row beyond the shared remaining budget so an
        // aggregate overflow fails before the generation can be advertised.
        let page_limit = AGENT_CAPTURE_LOCAL_PAGE_SIZE.min(remaining_rows.saturating_add(1));
        let mut page_values = values.clone();
        page_values.extend([
            i64::try_from(page_limit)
                .map_err(|error| CloudError::Generic(format!("encode {label} page size: {error}")))?
                .into(),
            i64::try_from(offset)
                .map_err(|error| {
                    CloudError::Generic(format!("encode {label} page offset: {error}"))
                })?
                .into(),
        ]);
        let page = conn
            .query_all_raw(sea_orm::Statement::from_sql_and_values(
                conn.get_database_backend(),
                format!("{sql} LIMIT ? OFFSET ?"),
                page_values,
            ))
            .await
            .map_err(|error| CloudError::Generic(format!("query {label} page: {error}")))?;
        let page_len = page.len();
        if page_len > *remaining_rows {
            return Err(CloudError::PartialTransfer(format!(
                "local agent-capture catalog exceeds the aggregate {}-row restore safety bound while reading {label}",
                AGENT_CAPTURE_RESTORE_MAX_ROWS
            )));
        }
        *remaining_rows -= page_len;
        rows.extend(page);
        if page_len < page_limit {
            break;
        }
        offset = offset.saturating_add(page_len);
    }
    Ok(rows)
}

async fn load_synced_required_object_oids<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    required_oids: &HashSet<String>,
) -> CloudResult<HashSet<String>> {
    if required_oids.len() > AGENT_CAPTURE_MAX_ROWS_PER_TABLE {
        return Err(CloudError::Generic(format!(
            "agent checkpoint reachability exceeds the {}-object cloud safety bound",
            AGENT_CAPTURE_MAX_ROWS_PER_TABLE
        )));
    }
    let mut required = required_oids.iter().cloned().collect::<Vec<_>>();
    required.sort();
    let mut synced = HashSet::with_capacity(required.len());
    for page in required.chunks(AGENT_CAPTURE_LOCAL_PAGE_SIZE) {
        let placeholders = vec!["?"; page.len()].join(", ");
        let mut values = Vec::with_capacity(page.len().saturating_add(1));
        values.push(repo_id.into());
        values.extend(page.iter().cloned().map(Into::into));
        let rows = conn
            .query_all_raw(sea_orm::Statement::from_sql_and_values(
                conn.get_database_backend(),
                format!(
                    "SELECT o_id FROM object_index
                     WHERE repo_id = ? AND is_synced = 1 AND o_id IN ({placeholders})"
                ),
                values,
            ))
            .await
            .map_err(|error| {
                CloudError::Generic(format!(
                    "query checkpoint-reachable synced object indexes: {error}"
                ))
            })?;
        for row in rows {
            synced.insert(row.try_get_by::<String, _>("o_id").map_err(|error| {
                CloudError::Generic(format!(
                    "decode checkpoint-reachable synced object index: {error}"
                ))
            })?);
        }
    }
    Ok(synced)
}

async fn load_required_local_object_indexes(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    required_oids: &HashSet<String>,
) -> CloudResult<HashMap<String, object_index::Model>> {
    if required_oids.len() > AGENT_CAPTURE_MAX_ROWS_PER_TABLE {
        return Err(CloudError::Generic(format!(
            "agent checkpoint reachability exceeds the {}-object cloud safety bound",
            AGENT_CAPTURE_MAX_ROWS_PER_TABLE
        )));
    }
    let mut required = required_oids.iter().cloned().collect::<Vec<_>>();
    required.sort();
    let mut rows = HashMap::with_capacity(required.len());
    for page in required.chunks(AGENT_CAPTURE_LOCAL_PAGE_SIZE) {
        let models = object_index::Entity::find()
            .filter(object_index::Column::RepoId.eq(repo_id))
            .filter(object_index::Column::OId.is_in(page.iter().cloned()))
            .all(db_conn)
            .await
            .map_err(|error| {
                CloudError::Generic(format!(
                    "load checkpoint-reachable local object indexes: {error}"
                ))
            })?;
        rows.extend(models.into_iter().map(|model| (model.o_id.clone(), model)));
    }
    Ok(rows)
}

async fn load_agent_capture_catalog_snapshot(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    subagent_content_present: bool,
) -> CloudResult<AgentCaptureSnapshot> {
    use sea_orm::Statement;

    let txn = db_conn
        .begin()
        .await
        .map_err(|error| CloudError::Generic(format!("begin agent capture snapshot: {error}")))?;
    let backend = txn.get_database_backend();
    let unsynced = txn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM object_index
             WHERE repo_id = ? AND COALESCE(is_synced, 0) = 0",
            [repo_id.into()],
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!("verify agent capture object generation: {error}"))
        })?
        .ok_or_else(|| CloudError::Generic("object generation count returned no row".into()))?
        .try_get_by::<i64, _>("n")
        .map_err(|error| CloudError::Generic(format!("decode unsynced object count: {error}")))?;
    if unsynced != 0 {
        return Err(CloudError::PartialTransfer(format!(
            "agent capture snapshot found {unsynced} object(s) outside the completed object upload generation; retry `libra cloud sync`"
        )));
    }
    let mut remaining_restore_rows = AGENT_CAPTURE_RESTORE_MAX_ROWS;

    let session_rows = load_local_capture_pages(
        &txn,
        "SELECT session_id, agent_kind, provider_session_id, state, working_dir,
                worktree_id, parent_commit, parent_session_id, metadata_json,
                redaction_report, started_at, last_event_at, stopped_at, schema_version,
                sync_revision
         FROM agent_session ORDER BY session_id",
        Vec::new(),
        "agent session",
        &mut remaining_restore_rows,
    )
    .await?;
    let sessions: Vec<AgentSessionV2Row> = session_rows
        .into_iter()
        .map(|row| {
            Ok(AgentSessionV2Row {
                session_id: row.try_get_by("session_id")?,
                agent_kind: row.try_get_by("agent_kind")?,
                provider_session_id: row.try_get_by("provider_session_id")?,
                state: row.try_get_by("state")?,
                working_dir: row.try_get_by("working_dir")?,
                worktree_id: row.try_get_by("worktree_id")?,
                parent_commit: row.try_get_by("parent_commit")?,
                parent_session_id: row.try_get_by("parent_session_id")?,
                metadata_json: row.try_get_by("metadata_json")?,
                redaction_report: row.try_get_by("redaction_report")?,
                started_at: row.try_get_by("started_at")?,
                last_event_at: row.try_get_by("last_event_at")?,
                stopped_at: row.try_get_by("stopped_at")?,
                schema_version: row.try_get_by("schema_version")?,
                sync_revision: row.try_get_by("sync_revision")?,
            })
        })
        .collect::<Result<_, sea_orm::DbErr>>()
        .map_err(|error| CloudError::Generic(format!("decode agent session snapshot: {error}")))?;

    let checkpoint_rows = load_local_capture_pages(
        &txn,
        "SELECT checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at, sync_revision
         FROM agent_checkpoint ORDER BY created_at, checkpoint_id",
        Vec::new(),
        "agent checkpoint",
        &mut remaining_restore_rows,
    )
    .await?;
    let checkpoints: Vec<AgentCheckpointV2Row> = checkpoint_rows
        .into_iter()
        .map(|row| {
            Ok(AgentCheckpointV2Row {
                checkpoint_id: row.try_get_by("checkpoint_id")?,
                session_id: row.try_get_by("session_id")?,
                parent_checkpoint_id: row.try_get_by("parent_checkpoint_id")?,
                scope: row.try_get_by("scope")?,
                parent_commit: row.try_get_by("parent_commit")?,
                tree_oid: row.try_get_by("tree_oid")?,
                metadata_blob_oid: row.try_get_by("metadata_blob_oid")?,
                traces_commit: row.try_get_by("traces_commit")?,
                tool_use_id: row.try_get_by("tool_use_id")?,
                subagent_session_id: row.try_get_by("subagent_session_id")?,
                description: row.try_get_by("description")?,
                created_at: row.try_get_by("created_at")?,
                sync_revision: row.try_get_by("sync_revision")?,
            })
        })
        .collect::<Result<_, sea_orm::DbErr>>()
        .map_err(|error| {
            CloudError::Generic(format!("decode agent checkpoint snapshot: {error}"))
        })?;

    let prune_tombstones = if subagent_content_present {
        let tombstone_rows = load_local_capture_pages(
            &txn,
            "SELECT checkpoint_id, session_id, pruned_at
             FROM agent_checkpoint_prune_tombstone ORDER BY checkpoint_id",
            Vec::new(),
            "checkpoint prune tombstone",
            &mut remaining_restore_rows,
        )
        .await?;
        tombstone_rows
            .into_iter()
            .map(|row| {
                Ok(AgentCheckpointPruneTombstoneRow {
                    checkpoint_id: row.try_get_by("checkpoint_id")?,
                    session_id: row.try_get_by("session_id")?,
                    pruned_at: row.try_get_by("pruned_at")?,
                })
            })
            .collect::<Result<Vec<_>, sea_orm::DbErr>>()
            .map_err(|error| CloudError::Generic(format!("decode prune tombstones: {error}")))?
    } else {
        Vec::new()
    };
    let traces_head = txn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT `commit` FROM reference
             WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
            [crate::internal::branch::TRACES_BRANCH.into()],
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("resolve traces snapshot head: {error}")))?
        .map(|row| row.try_get_by::<Option<String>, _>("commit"))
        .transpose()
        .map_err(|error| CloudError::Generic(format!("decode traces snapshot head: {error}")))?
        .flatten();

    let import_tombstone_present = txn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'agent_import_tombstone' LIMIT 1"
                .to_string(),
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("query import-tombstone schema: {error}")))?
        .is_some();
    let import_tombstones = if import_tombstone_present {
        let tombstone_rows = load_local_capture_pages(
            &txn,
            "SELECT agent_kind, provider_session_id, erased_session_id,
                    source_fingerprint, erased_at
             FROM agent_import_tombstone ORDER BY agent_kind, provider_session_id",
            Vec::new(),
            "agent import tombstone",
            &mut remaining_restore_rows,
        )
        .await?;
        tombstone_rows
            .into_iter()
            .map(|row| {
                Ok(AgentImportTombstoneRow {
                    agent_kind: row.try_get_by("agent_kind")?,
                    provider_session_id: row.try_get_by("provider_session_id")?,
                    erased_session_id: row.try_get_by("erased_session_id")?,
                    source_fingerprint: row.try_get_by("source_fingerprint")?,
                    erased_at: row.try_get_by("erased_at")?,
                })
            })
            .collect::<Result<Vec<_>, sea_orm::DbErr>>()
            .map_err(|error| CloudError::Generic(format!("decode import tombstones: {error}")))?
    } else {
        Vec::new()
    };
    let mut snapshot = AgentCaptureSnapshot {
        sessions,
        checkpoints,
        prune_tombstones,
        import_tombstones,
        traces_head,
        ..AgentCaptureSnapshot::default()
    };
    if subagent_content_present {
        let claim_rows = load_local_capture_pages(
            &txn,
            "SELECT parent_session_id, provider_kind, source_key,
                    content_schema_version, revision_cursor, sync_revision, current_revision,
                    current_checkpoint_id, current_digest, fence_token, created_at, updated_at
             FROM agent_subagent_content_claim
             ORDER BY parent_session_id, provider_kind, source_key, content_schema_version",
            Vec::new(),
            "subagent content claim",
            &mut remaining_restore_rows,
        )
        .await?;
        snapshot.claims = claim_rows
            .into_iter()
            .map(|row| {
                Ok(AgentSubagentContentClaimRow {
                    parent_session_id: row.try_get_by("parent_session_id")?,
                    provider_kind: row.try_get_by("provider_kind")?,
                    source_key: row.try_get_by("source_key")?,
                    content_schema_version: row.try_get_by("content_schema_version")?,
                    revision_cursor: row.try_get_by("revision_cursor")?,
                    sync_revision: row.try_get_by("sync_revision")?,
                    current_revision: row.try_get_by("current_revision")?,
                    current_checkpoint_id: row.try_get_by("current_checkpoint_id")?,
                    current_digest: row.try_get_by("current_digest")?,
                    fence_token: row.try_get_by("fence_token")?,
                    created_at: row.try_get_by("created_at")?,
                    updated_at: row.try_get_by("updated_at")?,
                })
            })
            .collect::<Result<_, sea_orm::DbErr>>()
            .map_err(|error| {
                CloudError::Generic(format!("decode subagent claim snapshot: {error}"))
            })?;
        let revision_rows = load_local_capture_pages(
            &txn,
            "SELECT parent_session_id, provider_kind, source_key,
                    content_schema_version, revision, checkpoint_id, content_digest,
                    source_channel, partial, created_at
             FROM agent_subagent_content_revision
             ORDER BY parent_session_id, provider_kind, source_key,
                      content_schema_version, revision",
            Vec::new(),
            "subagent content revision",
            &mut remaining_restore_rows,
        )
        .await?;
        snapshot.revisions = revision_rows
            .into_iter()
            .map(|row| {
                Ok(AgentSubagentContentRevisionRow {
                    parent_session_id: row.try_get_by("parent_session_id")?,
                    provider_kind: row.try_get_by("provider_kind")?,
                    source_key: row.try_get_by("source_key")?,
                    content_schema_version: row.try_get_by("content_schema_version")?,
                    revision: row.try_get_by("revision")?,
                    checkpoint_id: row.try_get_by("checkpoint_id")?,
                    content_digest: row.try_get_by("content_digest")?,
                    source_channel: row.try_get_by("source_channel")?,
                    partial: row.try_get_by("partial")?,
                    created_at: row.try_get_by("created_at")?,
                })
            })
            .collect::<Result<_, sea_orm::DbErr>>()
            .map_err(|error| {
                CloudError::Generic(format!("decode subagent revision snapshot: {error}"))
            })?;
        let link_rows = load_local_capture_pages(
            &txn,
            "SELECT content_checkpoint_id, parent_session_id, link_state,
                    boundary_checkpoint_id, stable_subagent_id, sync_revision,
                    created_at, updated_at
             FROM agent_subagent_link ORDER BY created_at, content_checkpoint_id",
            Vec::new(),
            "subagent association link",
            &mut remaining_restore_rows,
        )
        .await?;
        snapshot.links = link_rows
            .into_iter()
            .map(|row| {
                Ok(AgentSubagentLinkRow {
                    content_checkpoint_id: row.try_get_by("content_checkpoint_id")?,
                    parent_session_id: row.try_get_by("parent_session_id")?,
                    link_state: row.try_get_by("link_state")?,
                    boundary_checkpoint_id: row.try_get_by("boundary_checkpoint_id")?,
                    stable_subagent_id: row.try_get_by("stable_subagent_id")?,
                    sync_revision: row.try_get_by("sync_revision")?,
                    created_at: row.try_get_by("created_at")?,
                    updated_at: row.try_get_by("updated_at")?,
                })
            })
            .collect::<Result<_, sea_orm::DbErr>>()
            .map_err(|error| {
                CloudError::Generic(format!("decode subagent link snapshot: {error}"))
            })?;
    }

    validate_agent_capture_companions(
        &snapshot.checkpoints,
        &snapshot.claims,
        &snapshot.revisions,
        &snapshot.links,
        "local",
        CompanionValidationMode::Complete,
    )?;
    validate_agent_capture_session_dependencies(
        &snapshot.sessions,
        &snapshot.checkpoints,
        &snapshot.claims,
        "local",
    )?;
    txn.commit()
        .await
        .map_err(|error| CloudError::Generic(format!("commit agent capture snapshot: {error}")))?;
    Ok(snapshot)
}

fn validate_agent_capture_traces_shape(
    checkpoints: &[AgentCheckpointV2Row],
    traces_head: Option<&str>,
    side: &str,
) -> CloudResult<()> {
    match (checkpoints.is_empty(), traces_head) {
        (true, Some(_)) => Err(CloudError::PartialTransfer(format!(
            "{side} agent-capture catalog is empty but its fenced traces head is nonempty; run `libra agent doctor --repair` before retrying"
        ))),
        (false, None) => Err(CloudError::PartialTransfer(format!(
            "{side} agent-capture catalog has checkpoints but no fenced traces head; run `libra agent doctor --repair` before retrying"
        ))),
        _ => Ok(()),
    }
}

async fn load_agent_capture_snapshot(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    subagent_content_present: bool,
) -> CloudResult<AgentCaptureSnapshot> {
    // Keep the rollback-journal read transaction limited to SQLite paging.
    // Object decoding can take up to 110 seconds and must not hold a SHARED
    // lock that makes hook/import commits exhaust their busy timeout.
    let mut snapshot =
        load_agent_capture_catalog_snapshot(db_conn, repo_id, subagent_content_present).await?;
    validate_agent_capture_traces_shape(
        &snapshot.checkpoints,
        snapshot.traces_head.as_deref(),
        "local",
    )?;

    if cfg!(debug_assertions)
        && let Ok(delay) = std::env::var("LIBRA_TEST_CLOUD_AGENT_SNAPSHOT_DELAY_MS")
        && let Ok(delay) = delay.parse::<u64>()
        && delay > 0
    {
        tokio::time::sleep(std::time::Duration::from_millis(delay.min(5_000))).await;
    }

    let durability_specs = snapshot
        .checkpoints
        .iter()
        .map(
            |checkpoint| crate::internal::ai::history::CheckpointDurabilitySpec {
                checkpoint_id: &checkpoint.checkpoint_id,
                traces_commit: &checkpoint.traces_commit,
                tree_oid: &checkpoint.tree_oid,
                metadata_blob_oid: &checkpoint.metadata_blob_oid,
            },
        )
        .collect::<Vec<_>>();
    let required_oids = if durability_specs.is_empty() {
        HashSet::new()
    } else {
        let traces_head = snapshot.traces_head.as_deref().ok_or_else(|| {
            CloudError::PartialTransfer(
                "local agent-capture snapshot lost its fenced traces head".to_string(),
            )
        })?;
        let cataloged_commits = snapshot
            .checkpoints
            .iter()
            .map(|row| row.traces_commit.clone())
            .collect::<Vec<_>>();
        crate::internal::ai::history::checkpoint_rows_snapshot_durable_oids_from_head(
            &util::storage_path(),
            traces_head,
            &cataloged_commits,
            &durability_specs,
            std::time::Instant::now().checked_add(std::time::Duration::from_secs(110)),
        )
        .await
        .map_err(|error| {
            CloudError::PartialTransfer(format!(
                "agent checkpoint snapshot is not fully reachable and durable: {error:#}; run `libra agent doctor --repair`, then retry cloud sync"
            ))
        })?
    };
    let synced_oids = load_synced_required_object_oids(db_conn, repo_id, &required_oids).await?;
    for oid in &required_oids {
        if !synced_oids.contains(oid) {
            return Err(CloudError::PartialTransfer(format!(
                "agent capture cannot be published because reachable object {oid} is not in the completed local object upload generation; run `libra agent doctor --repair`, then retry cloud sync"
            )));
        }
    }
    validate_agent_capture_restore_row_budget(&snapshot, required_oids.len())?;

    let rechecked =
        load_agent_capture_catalog_snapshot(db_conn, repo_id, subagent_content_present).await?;
    if snapshot != rechecked {
        return Err(CloudError::PartialTransfer(
            "local agent-capture catalog changed during durability verification; retry cloud sync"
                .to_string(),
        ));
    }
    snapshot.required_oids = required_oids;
    Ok(snapshot)
}

async fn ensure_agent_capture_objects_remote(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
    required_oids: &HashSet<String>,
) -> CloudResult<(Vec<ObjectIndexRow>, i64)> {
    let local_map = load_required_local_object_indexes(db_conn, repo_id, required_oids).await?;
    let mut required = required_oids.iter().cloned().collect::<Vec<_>>();
    required.sort();
    let mut hashes = Vec::with_capacity(required.len());
    for oid in &required {
        if !local_map.contains_key(oid.as_str()) {
            return Err(CloudError::PartialTransfer(format!(
                "agent capture requires object {oid}, but its local object_index row is missing; run `libra agent doctor --repair`, then retry cloud sync"
            )));
        }
        let bytes = hex::decode(oid).map_err(|error| {
            CloudError::Generic(format!("invalid required agent-capture oid {oid}: {error}"))
        })?;
        hashes.push(ObjectHash::from_bytes(&bytes).map_err(|error| {
            CloudError::Generic(format!("invalid required agent-capture oid {oid}: {error}"))
        })?);
    }

    let remote_rows = d1_client
        .get_object_indexes_by_oids(repo_id, &required)
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "list remote object indexes for agent capture: {}",
                error.message
            ))
        })?;
    let remote_map = remote_rows
        .iter()
        .map(|row| (row.o_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    let local_storage = LocalStorage::new(path::objects());
    let verification_rows = required.iter().zip(&hashes).collect::<Vec<_>>();
    for page in agent_capture_object_verification_batches(&verification_rows) {
        // Full content verification remains mandatory, but a fixed-size page
        // overlaps R2 latency without allowing an unbounded fan-out for large
        // histories. Each page completes before the next one starts.
        futures::future::try_join_all(page.iter().map(|(oid, hash)| {
            let local_map = &local_map;
            let remote_map = &remote_map;
            let local_storage = &local_storage;
            async move {
                let local = local_map.get(oid.as_str()).ok_or_else(|| {
                    CloudError::Generic(format!(
                        "local object index {oid} disappeared during cloud sync"
                    ))
                })?;
                let remote_index_matches = remote_map.get(oid.as_str()).is_some_and(|remote| {
                    remote.o_type == local.o_type
                        && remote.o_size == local.o_size
                        && remote.is_synced == 1
                });
                publish_validated_agent_capture_object(local_storage, r2_storage, oid, hash)
                    .await?;
                if !remote_index_matches {
                    d1_client
                        .upsert_object_index(
                            &local.o_id,
                            &local.o_type,
                            local.o_size,
                            &local.repo_id,
                            local.created_at,
                        )
                        .await
                        .map_err(|error| {
                            CloudError::D1(format!(
                                "publish required agent-capture object index {oid}: {}",
                                error.message
                            ))
                        })?;
                }
                Ok::<(), CloudError>(())
            }
        }))
        .await?;
    }
    let verified_rows = d1_client
        .get_object_indexes_by_oids_with_generation(repo_id, &required)
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "verify remote object indexes for agent capture: {}",
                error.message
            ))
        })?;
    let verified_map = verified_rows
        .0
        .iter()
        .map(|row| (row.o_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    for oid in &required {
        let local = local_map.get(oid.as_str()).ok_or_else(|| {
            CloudError::Generic(format!(
                "local object index {oid} disappeared during verification"
            ))
        })?;
        let valid = verified_map.get(oid.as_str()).is_some_and(|remote| {
            remote.o_type == local.o_type && remote.o_size == local.o_size && remote.is_synced == 1
        });
        if !valid {
            return Err(CloudError::PartialTransfer(format!(
                "required agent-capture object index {oid} is absent or inconsistent in D1"
            )));
        }
    }
    Ok(verified_rows)
}

/// Verify a required capture object before its D1 manifest row can participate
/// in a completed generation. An existence probe is not a content proof: a
/// previous interrupted or corrupted upload may leave bytes under the right
/// key whose hash no longer matches that key. Valid remote payloads avoid a
/// rewrite; missing or corrupt payloads are replaced from validated local data
/// and read back once before publication continues.
async fn publish_validated_agent_capture_object(
    local_storage: &LocalStorage,
    r2_storage: &RemoteStorage,
    oid: &str,
    hash: &ObjectHash,
) -> CloudResult<()> {
    if let Ok((remote_bytes, remote_type)) = r2_storage.get(hash).await
        && ObjectHash::from_type_and_data(remote_type, &remote_bytes) == *hash
    {
        return Ok(());
    }
    let (bytes, object_type) = local_storage.get(hash).await.map_err(|error| {
        CloudError::PartialTransfer(format!(
            "read required agent-capture object {oid} for cloud publication: {error}"
        ))
    })?;
    let local_hash = ObjectHash::from_type_and_data(object_type, &bytes);
    if local_hash != *hash {
        return Err(CloudError::PartialTransfer(format!(
            "required local agent-capture object {oid} failed content verification: computed {local_hash}"
        )));
    }
    r2_storage
        .put(hash, &bytes, object_type)
        .await
        .map_err(|error| {
            CloudError::R2(format!(
                "upload required agent-capture object {oid}: {error}"
            ))
        })?;
    let (remote_bytes, remote_type) = r2_storage.get(hash).await.map_err(|error| {
        CloudError::R2(format!(
            "read back required agent-capture object {oid}: {error}"
        ))
    })?;
    let remote_hash = ObjectHash::from_type_and_data(remote_type, &remote_bytes);
    if remote_hash != *hash {
        return Err(CloudError::PartialTransfer(format!(
            "required remote agent-capture object {oid} failed post-upload verification: computed {remote_hash}"
        )));
    }
    Ok(())
}

async fn load_full_remote_object_manifest(
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
) -> CloudResult<(Vec<ObjectIndexRow>, i64)> {
    let (rows, generation) = d1_client
        .get_object_indexes_bounded_with_generation(repo_id, AGENT_CAPTURE_MAX_ROWS_PER_TABLE)
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "read full retained agent-capture object manifest: {}",
                error.message
            ))
        })?;
    for page in rows.chunks(AGENT_CAPTURE_LOCAL_PAGE_SIZE) {
        let mut hashes = Vec::with_capacity(page.len());
        for row in page {
            if row.is_synced != 1 {
                return Err(CloudError::PartialTransfer(format!(
                    "retained remote object {} is not marked synced",
                    row.o_id
                )));
            }
            let bytes = hex::decode(&row.o_id).map_err(|error| {
                CloudError::Generic(format!(
                    "invalid retained remote object id {}: {error}",
                    row.o_id
                ))
            })?;
            hashes.push(ObjectHash::from_bytes(&bytes).map_err(|error| {
                CloudError::Generic(format!(
                    "invalid retained remote object id {}: {error}",
                    row.o_id
                ))
            })?);
        }
        let exists = r2_storage.exist_batch(&hashes).await;
        if let Some((missing, _)) = page.iter().zip(exists).find(|(_, exists)| !*exists) {
            return Err(CloudError::PartialTransfer(format!(
                "retained remote object {} is absent from remote storage",
                missing.o_id
            )));
        }
    }
    Ok((rows, generation))
}

#[cfg(test)]
async fn project_agent_capture_object_indexes(
    db_conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    required_oids: &HashSet<String>,
) -> CloudResult<Vec<ObjectIndexRow>> {
    let local_map = load_required_local_object_indexes(db_conn, repo_id, required_oids).await?;
    let mut projected = Vec::with_capacity(required_oids.len());
    for oid in required_oids {
        let local = local_map.get(oid.as_str()).ok_or_else(|| {
            CloudError::PartialTransfer(format!(
                "agent capture requires object {oid}, but its local object_index row is missing; run `libra agent doctor --repair`, then retry cloud sync"
            ))
        })?;
        let row = ObjectIndexRow {
            o_id: local.o_id.clone(),
            o_type: local.o_type.clone(),
            o_size: local.o_size,
            repo_id: local.repo_id.clone(),
            created_at: local.created_at,
            is_synced: 1,
        };
        projected.push(row);
    }
    Ok(projected)
}

type SubagentSourceKey = (String, String, String, i64);
type SubagentRevisionKey = (String, String, String, i64, i64);

fn claim_key(row: &AgentSubagentContentClaimRow) -> SubagentSourceKey {
    (
        row.parent_session_id.clone(),
        row.provider_kind.clone(),
        row.source_key.clone(),
        row.content_schema_version,
    )
}

fn revision_key(row: &AgentSubagentContentRevisionRow) -> SubagentRevisionKey {
    (
        row.parent_session_id.clone(),
        row.provider_kind.clone(),
        row.source_key.clone(),
        row.content_schema_version,
        row.revision,
    )
}

fn claim_same_generation(
    left: &AgentSubagentContentClaimRow,
    right: &AgentSubagentContentClaimRow,
) -> bool {
    claim_key(left) == claim_key(right)
        && left.sync_revision == right.sync_revision
        && left.revision_cursor == right.revision_cursor
        && left.current_revision == right.current_revision
        && left.current_checkpoint_id == right.current_checkpoint_id
        && left.current_digest == right.current_digest
}

fn should_publish_claim(
    local: &AgentSubagentContentClaimRow,
    remote: Option<&AgentSubagentContentClaimRow>,
    remote_is_known_ancestor: bool,
) -> CloudResult<bool> {
    let Some(remote) = remote else {
        return Ok(true);
    };
    if claim_same_generation(remote, local) && remote.fence_token >= local.fence_token {
        return Ok(false);
    }
    if !remote_is_known_ancestor {
        return Err(CloudError::Generic(
            "subagent claim differs from a remote generation that is not this clone's known ancestor; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }
    if remote.revision_cursor > local.revision_cursor {
        return Err(CloudError::Generic(
            "subagent claim revision high-water would regress; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }
    if remote.sync_revision > local.sync_revision {
        return Err(CloudError::Generic(
            "subagent claim is older than its recorded remote ancestor; restore the current cloud snapshot before syncing"
                .to_string(),
        ));
    }
    if remote.sync_revision == local.sync_revision && !claim_same_generation(remote, local) {
        return Err(CloudError::Generic(
            "subagent claim conflicts with the remote at the same sync generation".to_string(),
        ));
    }
    Ok(remote.sync_revision < local.sync_revision || remote.fence_token < local.fence_token)
}

fn should_publish_session(
    local: &AgentSessionV2Row,
    remote: Option<&AgentSessionV2Row>,
    remote_is_known_ancestor: bool,
) -> CloudResult<bool> {
    let Some(remote) = remote else {
        return Ok(true);
    };
    if remote == local {
        return Ok(false);
    }
    if !remote_is_known_ancestor {
        return Err(CloudError::Generic(format!(
            "agent session {} differs from a remote generation that is not this clone's known ancestor; restore the current cloud snapshot before syncing",
            local.session_id
        )));
    }
    if remote.sync_revision < local.sync_revision {
        return Ok(true);
    }
    Err(CloudError::Generic(format!(
        "agent session {} does not descend monotonically from its recorded remote ancestor",
        local.session_id
    )))
}

fn remote_catalog_is_legacy_generation_zero_bootstrap(
    has_remote_generation: bool,
    local_cloud_base: Option<i64>,
    rows: &AgentCaptureRestoreCatalogRows,
) -> bool {
    !has_remote_generation
        && local_cloud_base.is_none()
        && (!rows.sessions.is_empty() || !rows.checkpoints.is_empty())
        && rows.sessions.iter().all(|row| row.sync_revision == 0)
        && rows.checkpoints.iter().all(|row| row.sync_revision == 0)
        && rows.prune_tombstones.is_empty()
        && rows.claims.is_empty()
        && rows.revisions.is_empty()
        && rows.links.is_empty()
}

fn remote_generation_is_known_ancestor(
    remote: Option<&AgentCaptureGenerationRow>,
    local_cloud_base: Option<i64>,
) -> bool {
    remote.is_some_and(|generation| match generation.state.as_str() {
        "complete" => local_cloud_base == Some(generation.generation),
        // A publishing generation is the immediate child of the last
        // completed base observed by its writer. Allow preflight to reconcile
        // that staged catalog so the server-side lease/CAS can eventually
        // resume an abandoned publication. This does not let an active writer
        // be displaced: begin_agent_capture_generation_from still enforces the
        // five-minute server-timestamped lease before issuing a new token.
        "publishing" => match local_cloud_base {
            Some(base) => base.checked_add(1) == Some(generation.generation),
            None => generation.generation == 1,
        },
        _ => false,
    })
}

fn should_publish_link(
    local: &AgentSubagentLinkRow,
    remote: Option<&AgentSubagentLinkRow>,
    remote_is_known_ancestor: bool,
) -> CloudResult<bool> {
    let Some(remote) = remote else {
        return Ok(true);
    };
    if remote == local {
        return Ok(false);
    }
    if !remote_is_known_ancestor {
        return Err(CloudError::Generic(format!(
            "subagent link {} differs from a remote generation that is not this clone's known ancestor; restore the current cloud snapshot before syncing",
            local.content_checkpoint_id
        )));
    }
    if remote.sync_revision < local.sync_revision {
        return Ok(true);
    }
    Err(CloudError::Generic(format!(
        "subagent link {} does not descend monotonically from its recorded remote ancestor",
        local.content_checkpoint_id
    )))
}

fn checkpoint_rewrite_compatible(
    left: &AgentCheckpointV2Row,
    right: &AgentCheckpointV2Row,
) -> bool {
    left.checkpoint_id == right.checkpoint_id
        && left.session_id == right.session_id
        && left.parent_checkpoint_id == right.parent_checkpoint_id
        && left.scope == right.scope
        && left.parent_commit == right.parent_commit
        && left.tool_use_id == right.tool_use_id
        && left.subagent_session_id == right.subagent_session_id
        && left.description == right.description
        && left.created_at == right.created_at
}

fn should_publish_checkpoint(
    local: &AgentCheckpointV2Row,
    remote: Option<&AgentCheckpointV2Row>,
    remote_is_known_ancestor: bool,
) -> CloudResult<bool> {
    let Some(remote) = remote else {
        return Ok(true);
    };
    if remote.sync_revision == local.sync_revision {
        if remote == local {
            return Ok(false);
        }
        return Err(CloudError::Generic(format!(
            "agent checkpoint {} diverges from the remote at the same sync generation",
            local.checkpoint_id
        )));
    }
    if !checkpoint_rewrite_compatible(local, remote) {
        return Err(CloudError::Generic(format!(
            "agent checkpoint {} conflicts with the remote immutable identity",
            local.checkpoint_id
        )));
    }
    if local.sync_revision > remote.sync_revision && !remote_is_known_ancestor {
        return Err(CloudError::Generic(format!(
            "agent checkpoint {} differs from a remote generation that is not this clone's known ancestor; restore the current cloud snapshot before syncing",
            local.checkpoint_id
        )));
    }
    Ok(local.sync_revision > remote.sync_revision)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompanionValidationMode {
    Complete,
    Publishing,
}

fn validate_agent_capture_companions(
    checkpoints: &[AgentCheckpointV2Row],
    claims: &[AgentSubagentContentClaimRow],
    revisions: &[AgentSubagentContentRevisionRow],
    links: &[AgentSubagentLinkRow],
    side: &str,
    mode: CompanionValidationMode,
) -> CloudResult<()> {
    let checkpoint_map: HashMap<&str, &AgentCheckpointV2Row> = checkpoints
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect();
    let claim_keys: HashSet<SubagentSourceKey> = claims.iter().map(claim_key).collect();
    let revision_map: HashMap<SubagentRevisionKey, &AgentSubagentContentRevisionRow> = revisions
        .iter()
        .map(|row| (revision_key(row), row))
        .collect();
    let link_map: HashMap<&str, &AgentSubagentLinkRow> = links
        .iter()
        .map(|row| (row.content_checkpoint_id.as_str(), row))
        .collect();
    let revision_checkpoint_ids: HashSet<&str> = revisions
        .iter()
        .map(|row| row.checkpoint_id.as_str())
        .collect();
    for revision in revisions {
        let source_key = (
            revision.parent_session_id.clone(),
            revision.provider_kind.clone(),
            revision.source_key.clone(),
            revision.content_schema_version,
        );
        if !claim_keys.contains(&source_key) && mode == CompanionValidationMode::Complete {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision {} has no source claim dependency",
                revision.checkpoint_id
            )));
        }
        let Some(checkpoint) = checkpoint_map.get(revision.checkpoint_id.as_str()) else {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision {} has no checkpoint dependency",
                revision.checkpoint_id
            )));
        };
        if checkpoint.session_id != revision.parent_session_id {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision {} disagrees with its checkpoint parent",
                revision.checkpoint_id
            )));
        }
        if checkpoint.scope != "subagent" {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision {} references a non-subagent checkpoint",
                revision.checkpoint_id
            )));
        }
        if mode == CompanionValidationMode::Complete {
            let link = link_map
                .get(revision.checkpoint_id.as_str())
                .ok_or_else(|| {
                    CloudError::Generic(format!(
                        "{side} subagent revision {} has no association link dependency",
                        revision.checkpoint_id
                    ))
                })?;
            if link.parent_session_id != revision.parent_session_id {
                return Err(CloudError::Generic(format!(
                    "{side} subagent revision {} disagrees with its association parent",
                    revision.checkpoint_id
                )));
            }
        }
        if mode == CompanionValidationMode::Complete
            && let Some(claim) = claims.iter().find(|claim| claim_key(claim) == source_key)
            && revision.revision > claim.revision_cursor
        {
            return Err(CloudError::Generic(format!(
                "{side} subagent revision {} is newer than its completed source claim",
                revision.checkpoint_id
            )));
        }
    }
    for link in links {
        let Some(checkpoint) = checkpoint_map.get(link.content_checkpoint_id.as_str()) else {
            return Err(CloudError::Generic(format!(
                "{side} subagent link {} has no checkpoint dependency",
                link.content_checkpoint_id
            )));
        };
        if checkpoint.session_id != link.parent_session_id {
            return Err(CloudError::Generic(format!(
                "{side} subagent link {} disagrees with its checkpoint parent",
                link.content_checkpoint_id
            )));
        }
        if checkpoint.scope != "subagent" {
            return Err(CloudError::Generic(format!(
                "{side} subagent link {} references a non-subagent content checkpoint",
                link.content_checkpoint_id
            )));
        }
        if mode == CompanionValidationMode::Complete
            && !revision_checkpoint_ids.contains(link.content_checkpoint_id.as_str())
        {
            return Err(CloudError::Generic(format!(
                "{side} subagent link {} has no immutable revision dependency",
                link.content_checkpoint_id
            )));
        }
        if let Some(boundary) = link.boundary_checkpoint_id.as_deref() {
            let boundary_checkpoint = checkpoint_map.get(boundary).ok_or_else(|| {
                CloudError::Generic(format!(
                    "{side} resolved subagent link {} has no boundary checkpoint dependency",
                    link.content_checkpoint_id
                ))
            })?;
            if boundary_checkpoint.scope != "subagent"
                || boundary_checkpoint.session_id != link.parent_session_id
                || revision_checkpoint_ids.contains(boundary)
            {
                return Err(CloudError::Generic(format!(
                    "{side} resolved subagent link {} references an invalid boundary checkpoint",
                    link.content_checkpoint_id
                )));
            }
        }
    }
    for claim in claims {
        if claim.revision_cursor < claim.current_revision {
            return Err(CloudError::Generic(format!(
                "{side} subagent claim cursor is behind its current revision"
            )));
        }
        if claim.current_revision == 0 {
            if claim.current_checkpoint_id.is_some() || claim.current_digest.is_some() {
                return Err(CloudError::Generic(format!(
                    "{side} zero-revision subagent claim has a materialized current leaf"
                )));
            }
            continue;
        }
        let checkpoint_id = claim.current_checkpoint_id.as_deref().ok_or_else(|| {
            CloudError::Generic(format!("{side} current subagent claim has no checkpoint"))
        })?;
        let digest = claim.current_digest.as_deref().ok_or_else(|| {
            CloudError::Generic(format!("{side} current subagent claim has no digest"))
        })?;
        let revision = revision_map
            .get(&(
                claim.parent_session_id.clone(),
                claim.provider_kind.clone(),
                claim.source_key.clone(),
                claim.content_schema_version,
                claim.current_revision,
            ))
            .ok_or_else(|| {
                CloudError::Generic(format!(
                    "{side} current subagent claim has no immutable revision dependency"
                ))
            })?;
        if revision.checkpoint_id != checkpoint_id || revision.content_digest != digest {
            return Err(CloudError::Generic(format!(
                "{side} current subagent claim disagrees with its immutable revision"
            )));
        }
        let link = link_map.get(checkpoint_id).ok_or_else(|| {
            CloudError::Generic(format!(
                "{side} current subagent claim has no association link dependency"
            ))
        })?;
        if link.parent_session_id != claim.parent_session_id {
            return Err(CloudError::Generic(format!(
                "{side} current subagent claim disagrees with its association parent"
            )));
        }
    }
    Ok(())
}

fn validate_agent_capture_session_dependencies(
    sessions: &[AgentSessionV2Row],
    checkpoints: &[AgentCheckpointV2Row],
    claims: &[AgentSubagentContentClaimRow],
    side: &str,
) -> CloudResult<()> {
    let session_ids = sessions
        .iter()
        .map(|row| row.session_id.as_str())
        .collect::<HashSet<_>>();
    for checkpoint in checkpoints {
        if !session_ids.contains(checkpoint.session_id.as_str()) {
            return Err(CloudError::Generic(format!(
                "{side} checkpoint {} has no session dependency",
                checkpoint.checkpoint_id
            )));
        }
    }
    for claim in claims {
        if !session_ids.contains(claim.parent_session_id.as_str()) {
            return Err(CloudError::Generic(format!(
                "{side} subagent claim has no parent session dependency"
            )));
        }
    }
    Ok(())
}

fn object_manifest_scope_for_remote_catalog(
    local: &[AgentCheckpointV2Row],
    effective: &[AgentCheckpointV2Row],
) -> AgentCaptureObjectManifestScope {
    let local_rows = local
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    if effective
        .iter()
        .any(|row| local_rows.get(row.checkpoint_id.as_str()).copied() != Some(row))
    {
        AgentCaptureObjectManifestScope::FullRemoteIndex
    } else {
        AgentCaptureObjectManifestScope::CheckpointProjection
    }
}

fn build_effective_checkpoint_catalog(
    local: &[AgentCheckpointV2Row],
    remote: &[AgentCheckpointV2Row],
    local_tombstones: &[AgentCheckpointPruneTombstoneRow],
    remote_tombstones: &[AgentCheckpointPruneTombstoneRow],
    remote_is_known_ancestor: bool,
) -> CloudResult<(Vec<AgentCheckpointV2Row>, Vec<AgentCheckpointV2Row>)> {
    let local_map = local
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    let remote_map = remote
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    let local_tombstone_ids = local_tombstones
        .iter()
        .map(|row| row.checkpoint_id.as_str())
        .collect::<HashSet<_>>();
    let remote_tombstone_ids = remote_tombstones
        .iter()
        .map(|row| row.checkpoint_id.as_str())
        .collect::<HashSet<_>>();

    if let Some(row) = local
        .iter()
        .find(|row| remote_tombstone_ids.contains(row.checkpoint_id.as_str()))
    {
        return Err(CloudError::Generic(format!(
            "agent checkpoint {} was already pruned by another cloud writer; restore the current cloud snapshot before syncing this stale clone",
            row.checkpoint_id
        )));
    }

    let mut pending = Vec::new();
    let mut effective = Vec::new();
    for remote_row in remote {
        if local_tombstone_ids.contains(remote_row.checkpoint_id.as_str()) {
            continue;
        }
        let Some(local_row) = local_map.get(remote_row.checkpoint_id.as_str()).copied() else {
            return Err(CloudError::Generic(format!(
                "remote checkpoint {} is absent locally without an ordinary-prune tombstone; cloud session-erasure propagation is deferred, so restore or purge the remote capture before publishing a new generation",
                remote_row.checkpoint_id
            )));
        };
        if remote_row.sync_revision > local_row.sync_revision {
            return Err(CloudError::Generic(format!(
                "remote checkpoint {} is newer than this clone's traces history; restore the current cloud snapshot before syncing",
                remote_row.checkpoint_id
            )));
        }
        if should_publish_checkpoint(local_row, Some(remote_row), remote_is_known_ancestor)? {
            pending.push(local_row.clone());
            effective.push(local_row.clone());
        } else {
            effective.push(remote_row.clone());
        }
    }
    for local_row in local {
        if !remote_map.contains_key(local_row.checkpoint_id.as_str()) {
            pending.push(local_row.clone());
            effective.push(local_row.clone());
        }
    }
    effective.sort_by(|left, right| left.checkpoint_id.cmp(&right.checkpoint_id));
    Ok((pending, effective))
}

fn checkpoint_catalog_matches(
    left: &[AgentCheckpointV2Row],
    right: &[AgentCheckpointV2Row],
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let right_map = right
        .iter()
        .map(|row| (row.checkpoint_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    left.iter()
        .all(|row| right_map.get(row.checkpoint_id.as_str()).copied() == Some(row))
}

/// Mirror one coherent local agent-capture/object generation to D1. Remote
/// state is paged and used as an incremental high-water mark; only missing or
/// strictly newer rows are sent, in bounded multi-row requests. Immutable
/// conflicts fail before publication. Dependencies publish first and claims
/// publish last.
async fn sync_agent_capture_tables(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
    progress: &dyn CloudSyncProgress,
) -> CloudResult<AgentCaptureSyncOutcome> {
    tokio::time::timeout(
        AGENT_CAPTURE_CLOUD_DEADLINE,
        sync_agent_capture_tables_inner(db_conn, d1_client, r2_storage, repo_id, progress),
    )
    .await
    .map_err(|_| {
        CloudError::PartialTransfer(
            "agent capture cloud sync exceeded its 120-second deadline; retry the operation"
                .to_string(),
        )
    })?
}

async fn sync_agent_capture_tables_inner(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    r2_storage: &RemoteStorage,
    repo_id: &str,
    progress: &dyn CloudSyncProgress,
) -> CloudResult<AgentCaptureSyncOutcome> {
    use sea_orm::Statement;

    let backend = db_conn.get_database_backend();
    let session_present = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_session' LIMIT 1",
            [],
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("query sqlite_master: {error}")))?
        .is_some();
    if !session_present {
        return Ok(AgentCaptureSyncOutcome::SkippedLegacySchema);
    }
    let subagent_content_present = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'agent_subagent_content_claim' LIMIT 1",
            [],
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("query subagent-content schema: {error}")))?
        .is_some();

    progress.on_agent_capture_starting();
    let snapshot = load_agent_capture_snapshot(db_conn, repo_id, subagent_content_present).await?;

    d1_client
        .ensure_agent_session_table()
        .await
        .map_err(|error| {
            CloudError::D1(format!("ensure_agent_session_table: {}", error.message))
        })?;
    d1_client
        .ensure_agent_checkpoint_table()
        .await
        .map_err(|error| {
            CloudError::D1(format!("ensure_agent_checkpoint_table: {}", error.message))
        })?;
    d1_client
        .ensure_agent_capture_generation_table()
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "ensure_agent_capture_generation_table: {}",
                error.message
            ))
        })?;
    d1_client
        .ensure_agent_checkpoint_prune_tombstone_table()
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "ensure checkpoint prune tombstones: {}",
                error.message
            ))
        })?;
    d1_client
        .ensure_agent_import_tombstone_table()
        .await
        .map_err(|error| {
            CloudError::D1(format!("ensure agent import tombstones: {}", error.message))
        })?;
    if subagent_content_present {
        d1_client
            .ensure_agent_subagent_content_tables()
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "ensure_agent_subagent_content_tables: {}",
                    error.message
                ))
            })?;
    }

    let remote_generation = d1_client
        .get_agent_capture_generation(repo_id)
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "read agent-capture generation before sync: {}",
                error.message
            ))
        })?;
    let local_cloud_base = load_local_agent_capture_cloud_base(db_conn, repo_id).await?;
    let remote_catalog = d1_client
        .list_agent_capture_restore_catalog_rows(
            repo_id,
            subagent_content_present,
            AGENT_CAPTURE_RESTORE_MAX_ROWS,
        )
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "list aggregate-bounded remote agent-capture catalog before sync: {}",
                error.message
            ))
        })?;
    // The one-time legacy adoption copies only session/checkpoint rows at
    // generation zero and deliberately has no completed generation manifest.
    // Treat that exact projection as the bootstrap ancestor so the first
    // current client can replace revision zero under its first fenced
    // generation. Any current-only row type or nonzero revision fails closed.
    let remote_is_known_ancestor =
        remote_generation_is_known_ancestor(remote_generation.as_ref(), local_cloud_base)
            || remote_catalog_is_legacy_generation_zero_bootstrap(
                remote_generation.is_some(),
                local_cloud_base,
                &remote_catalog,
            );
    let AgentCaptureRestoreCatalogRows {
        sessions: remote_sessions,
        checkpoints: remote_checkpoints,
        prune_tombstones: remote_prune_tombstones,
        import_tombstones: remote_import_tombstones,
        claims: remote_claims,
        revisions: remote_revisions,
        links: remote_links,
        remaining_rows: _,
    } = remote_catalog;
    // PD-03: the effective tombstone set is the UNION of local and remote
    // (newest erased_at per provider identity), and every remote catalog
    // row belonging to an erased session id is dropped BEFORE conflict/
    // effective-set computation — the publish cascade deletes the same
    // rows remotely, so the post-publish verification stays coherent.
    let merged_import_tombstones =
        merge_import_tombstones(&snapshot.import_tombstones, &remote_import_tombstones);
    let erased_session_ids: HashSet<&str> = merged_import_tombstones
        .iter()
        .map(|row| row.erased_session_id.as_str())
        .collect();
    let remote_sessions: Vec<AgentSessionV2Row> = remote_sessions
        .into_iter()
        .filter(|row| !erased_session_ids.contains(row.session_id.as_str()))
        .collect();
    let erased_checkpoint_ids: HashSet<&str> = remote_checkpoints
        .iter()
        .filter(|row| erased_session_ids.contains(row.session_id.as_str()))
        .map(|row| row.checkpoint_id.as_str())
        .collect();
    let remote_checkpoints: Vec<AgentCheckpointV2Row> = remote_checkpoints
        .iter()
        .filter(|row| !erased_session_ids.contains(row.session_id.as_str()))
        .cloned()
        .collect();
    let remote_claims: Vec<AgentSubagentContentClaimRow> = remote_claims
        .into_iter()
        .filter(|row| !erased_session_ids.contains(row.parent_session_id.as_str()))
        .collect();
    let remote_revisions: Vec<AgentSubagentContentRevisionRow> = remote_revisions
        .into_iter()
        .filter(|row| !erased_checkpoint_ids.contains(row.checkpoint_id.as_str()))
        .collect();
    let remote_links: Vec<AgentSubagentLinkRow> = remote_links
        .into_iter()
        .filter(|row| !erased_checkpoint_ids.contains(row.content_checkpoint_id.as_str()))
        .collect();
    validate_agent_capture_companions(
        &remote_checkpoints,
        &remote_claims,
        &remote_revisions,
        &remote_links,
        "remote",
        CompanionValidationMode::Publishing,
    )?;
    validate_agent_capture_session_dependencies(
        &remote_sessions,
        &remote_checkpoints,
        &remote_claims,
        "remote",
    )?;
    let remote_session_map: HashMap<&str, &AgentSessionV2Row> = remote_sessions
        .iter()
        .map(|row| (row.session_id.as_str(), row))
        .collect();
    let mut pending_sessions = Vec::new();
    for row in &snapshot.sessions {
        if should_publish_session(
            row,
            remote_session_map.get(row.session_id.as_str()).copied(),
            remote_is_known_ancestor,
        )? {
            pending_sessions.push(row.clone());
        }
    }
    let (pending_checkpoints, effective_checkpoints) = build_effective_checkpoint_catalog(
        &snapshot.checkpoints,
        &remote_checkpoints,
        &snapshot.prune_tombstones,
        &remote_prune_tombstones,
        remote_is_known_ancestor,
    )?;

    let object_manifest_scope =
        object_manifest_scope_for_remote_catalog(&snapshot.checkpoints, &effective_checkpoints);
    let (required_object_indexes, required_object_generation) =
        ensure_agent_capture_objects_remote(
            db_conn,
            d1_client,
            r2_storage,
            repo_id,
            &snapshot.required_oids,
        )
        .await?;
    let (object_manifest_rows, object_index_generation) = match object_manifest_scope {
        AgentCaptureObjectManifestScope::CheckpointProjection => {
            (required_object_indexes, required_object_generation)
        }
        AgentCaptureObjectManifestScope::FullRemoteIndex => {
            load_full_remote_object_manifest(d1_client, r2_storage, repo_id).await?
        }
    };
    validate_agent_capture_restore_row_budget(&snapshot, object_manifest_rows.len())?;
    validate_checkpoint_object_index_roots(
        &effective_checkpoints,
        &object_manifest_rows,
        "projected remote",
    )?;
    let (object_index_digest, object_index_count) =
        agent_capture_object_index_digest(&object_manifest_rows)?;

    let remote_revision_map: HashMap<SubagentRevisionKey, &AgentSubagentContentRevisionRow> =
        remote_revisions
            .iter()
            .map(|row| (revision_key(row), row))
            .collect();
    let mut pending_revisions = Vec::new();
    for row in &snapshot.revisions {
        match remote_revision_map.get(&revision_key(row)) {
            None => pending_revisions.push(row.clone()),
            Some(remote) if *remote == row => {}
            Some(_) => {
                return Err(CloudError::Generic(format!(
                    "immutable subagent revision {} conflicts with the remote",
                    row.checkpoint_id
                )));
            }
        }
    }

    let remote_link_map: HashMap<&str, &AgentSubagentLinkRow> = remote_links
        .iter()
        .map(|row| (row.content_checkpoint_id.as_str(), row))
        .collect();
    let mut pending_links = Vec::new();
    for row in &snapshot.links {
        if should_publish_link(
            row,
            remote_link_map
                .get(row.content_checkpoint_id.as_str())
                .copied(),
            remote_is_known_ancestor,
        )? {
            pending_links.push(row.clone());
        }
    }
    let prune_ids = snapshot
        .prune_tombstones
        .iter()
        .map(|row| row.checkpoint_id.as_str())
        .collect::<HashSet<_>>();
    let (pre_prune_links, pending_links): (Vec<_>, Vec<_>) =
        pending_links.into_iter().partition(|row| {
            !prune_ids.contains(row.content_checkpoint_id.as_str())
                && row.boundary_checkpoint_id.is_none()
                && remote_link_map
                    .get(row.content_checkpoint_id.as_str())
                    .and_then(|remote| remote.boundary_checkpoint_id.as_deref())
                    .is_some_and(|boundary| prune_ids.contains(boundary))
        });
    if let Some(link) = remote_links.iter().find(|remote| {
        !prune_ids.contains(remote.content_checkpoint_id.as_str())
            && remote
                .boundary_checkpoint_id
                .as_deref()
                .is_some_and(|boundary| prune_ids.contains(boundary))
            && !pre_prune_links
                .iter()
                .any(|local| local.content_checkpoint_id == remote.content_checkpoint_id)
    }) {
        return Err(CloudError::Generic(format!(
            "remote subagent link {} still resolves through a checkpoint being pruned, but this clone has no newer unresolved link generation; restore the current cloud snapshot before syncing",
            link.content_checkpoint_id
        )));
    }

    let remote_claim_map: HashMap<SubagentSourceKey, &AgentSubagentContentClaimRow> = remote_claims
        .iter()
        .map(|row| (claim_key(row), row))
        .collect();
    let mut pending_claims = Vec::new();
    for row in &snapshot.claims {
        if should_publish_claim(
            row,
            remote_claim_map.get(&claim_key(row)).copied(),
            remote_is_known_ancestor,
        )? {
            pending_claims.push(row.clone());
        }
    }

    // All remote conflict and object-durability checks happen before this
    // transition. A transient preflight failure therefore leaves the last
    // complete manifest restorable instead of needlessly wedging it in
    // `publishing` before any fenced capture-catalog mutation.
    let publish_token = Uuid::new_v4().to_string();
    d1_client
        .begin_agent_capture_generation_from(
            repo_id,
            &publish_token,
            remote_generation
                .as_ref()
                .map(|generation| generation.generation),
            AgentCaptureGenerationManifest {
                object_index_digest: &object_index_digest,
                object_index_count,
                object_index_scope: object_manifest_scope.as_str(),
                object_index_generation,
                traces_head: snapshot.traces_head.as_deref(),
            },
        )
        .await
        .map_err(|error| {
            CloudError::D1(format!("begin agent capture generation: {}", error.message))
        })?;
    for rows in agent_capture_batches(&pending_sessions) {
        d1_client
            .sync_agent_sessions_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                let row_id = rows
                    .first()
                    .map(|row| row.session_id.as_str())
                    .unwrap_or("agent-session-batch");
                progress.on_agent_capture_session_warning(row_id, &error.message);
                CloudError::D1(format!("sync agent session batch: {}", error.message))
            })?;
    }
    // Boundary associations must become unresolved before their boundary
    // checkpoint is deleted. This preserves a Publishing-valid graph at every
    // request boundary; new content links still publish after checkpoints.
    for rows in agent_capture_batches(&pre_prune_links) {
        d1_client
            .sync_agent_subagent_links_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "sync pre-prune subagent link batch: {}",
                    error.message
                ))
            })?;
    }
    for rows in agent_capture_batches(&snapshot.prune_tombstones) {
        d1_client
            .sync_agent_checkpoint_prune_tombstones_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "sync checkpoint prune tombstones: {}",
                    error.message
                ))
            })?;
    }
    for rows in agent_capture_batches(&pending_checkpoints) {
        d1_client
            .sync_agent_checkpoints_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                let row_id = rows
                    .first()
                    .map(|row| row.checkpoint_id.as_str())
                    .unwrap_or("agent-checkpoint-batch");
                progress.on_agent_capture_checkpoint_warning(row_id, &error.message);
                CloudError::D1(format!("sync agent checkpoint batch: {}", error.message))
            })?;
    }
    for rows in agent_capture_batches(&pending_revisions) {
        d1_client
            .sync_agent_subagent_revisions_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                let row_id = rows
                    .first()
                    .map(|row| row.checkpoint_id.as_str())
                    .unwrap_or("subagent-revision-batch");
                progress.on_agent_capture_checkpoint_warning(row_id, &error.message);
                CloudError::D1(format!("sync subagent revision batch: {}", error.message))
            })?;
    }
    for rows in agent_capture_batches(&pending_links) {
        d1_client
            .sync_agent_subagent_links_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                let row_id = rows
                    .first()
                    .map(|row| row.content_checkpoint_id.as_str())
                    .unwrap_or("subagent-link-batch");
                progress.on_agent_capture_checkpoint_warning(row_id, &error.message);
                CloudError::D1(format!("sync subagent link batch: {}", error.message))
            })?;
    }
    for rows in agent_capture_batches(&pending_claims) {
        d1_client
            .sync_agent_subagent_claims_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                progress.on_agent_capture_warning(&error.message);
                CloudError::D1(format!("sync subagent claim batch: {}", error.message))
            })?;
    }
    // PD-03: the session tombstones publish LAST so their cascade delete
    // is final regardless of what earlier batches upserted.
    for rows in agent_capture_batches(&merged_import_tombstones) {
        d1_client
            .sync_agent_import_tombstones_batch(repo_id, &publish_token, rows)
            .await
            .map_err(|error| {
                CloudError::D1(format!("sync agent import tombstones: {}", error.message))
            })?;
    }

    let AgentCaptureRestoreCatalogRows {
        sessions: completed_sessions,
        checkpoints: completed_checkpoints,
        prune_tombstones: _,
        import_tombstones: _,
        claims: completed_claims,
        revisions: completed_revisions,
        links: completed_links,
        remaining_rows: completed_remaining_rows,
    } = d1_client
        .list_agent_capture_restore_catalog_rows(
            repo_id,
            subagent_content_present,
            AGENT_CAPTURE_RESTORE_MAX_ROWS,
        )
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "verify aggregate-bounded agent-capture catalog: {}",
                error.message
            ))
        })?;
    if !checkpoint_catalog_matches(&completed_checkpoints, &effective_checkpoints) {
        return Err(CloudError::PartialTransfer(
            "remote checkpoint catalog changed during agent-capture publication; retry cloud sync"
                .to_string(),
        ));
    }
    validate_agent_capture_companions(
        &completed_checkpoints,
        &completed_claims,
        &completed_revisions,
        &completed_links,
        "completed remote",
        CompanionValidationMode::Complete,
    )?;
    validate_agent_capture_session_dependencies(
        &completed_sessions,
        &completed_checkpoints,
        &completed_claims,
        "completed remote",
    )?;
    let completed_scope =
        object_manifest_scope_for_remote_catalog(&snapshot.checkpoints, &effective_checkpoints);
    if completed_scope != object_manifest_scope {
        return Err(CloudError::PartialTransfer(
            "remote checkpoint catalog changed its object-manifest scope during publication; retry cloud sync"
                .to_string(),
        ));
    }
    let mut required_oids = snapshot.required_oids.iter().cloned().collect::<Vec<_>>();
    required_oids.sort();
    let (completed_object_indexes, completed_object_generation) = match object_manifest_scope {
        AgentCaptureObjectManifestScope::CheckpointProjection => {
            if required_oids.len() > completed_remaining_rows {
                return Err(CloudError::PartialTransfer(format!(
                    "completed remote agent-capture verification exceeds its aggregate {}-row safety bound before reading object indexes",
                    AGENT_CAPTURE_RESTORE_MAX_ROWS
                )));
            }
            d1_client
                .get_object_indexes_by_oids_with_generation(repo_id, &required_oids)
                .await
                .map_err(|error| {
                    CloudError::D1(format!(
                        "verify fenced agent-capture object indexes: {}",
                        error.message
                    ))
                })?
        }
        AgentCaptureObjectManifestScope::FullRemoteIndex => d1_client
            .get_object_indexes_bounded_with_generation(repo_id, completed_remaining_rows)
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "verify full agent-capture object manifest within the aggregate row budget: {}",
                    error.message
                ))
            })?,
    };
    completed_remaining_rows
        .checked_sub(completed_object_indexes.len())
        .ok_or_else(|| {
            CloudError::PartialTransfer(format!(
                "completed remote agent-capture verification exceeds its aggregate {}-row safety bound while reading object indexes",
                AGENT_CAPTURE_RESTORE_MAX_ROWS
            ))
        })?;
    validate_checkpoint_object_index_roots(
        &completed_checkpoints,
        &completed_object_indexes,
        "completed remote",
    )?;
    let completed_object_manifest = agent_capture_object_index_digest(&completed_object_indexes)?;
    if completed_object_manifest != (object_index_digest.clone(), object_index_count)
        || completed_object_generation != object_index_generation
    {
        return Err(CloudError::PartialTransfer(
            "remote object indexes changed during agent-capture publication; retry cloud sync"
                .to_string(),
        ));
    }
    let completed_generation = d1_client
        .complete_agent_capture_generation(repo_id, &publish_token, object_index_generation)
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "complete agent capture generation: {}",
                error.message
            ))
        })?;
    store_local_agent_capture_cloud_base(db_conn, repo_id, completed_generation.generation).await?;

    let sessions_synced = pending_sessions.len();
    let checkpoints_synced = pending_checkpoints.len();
    let subagent_rows_synced = pending_claims
        .len()
        .saturating_add(pending_revisions.len())
        .saturating_add(pending_links.len())
        .saturating_add(pre_prune_links.len());
    progress.on_agent_capture_done_with_subagents(
        sessions_synced,
        0,
        checkpoints_synced,
        0,
        subagent_rows_synced,
        0,
    );
    Ok(AgentCaptureSyncOutcome::Completed {
        sessions_synced,
        sessions_failed: 0,
        checkpoints_synced,
        checkpoints_failed: 0,
    })
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
async fn restore_agent_capture_from_d1(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    repo_id: &str,
    render_human: bool,
) -> CloudResult<AgentCaptureRestoreOutcome> {
    tokio::time::timeout(
        AGENT_CAPTURE_CLOUD_DEADLINE,
        restore_agent_capture_from_d1_inner(db_conn, d1_client, repo_id, render_human),
    )
    .await
    .map_err(|_| {
        CloudError::PartialTransfer(
            "agent capture cloud restore exceeded its 120-second deadline; retry the operation"
                .to_string(),
        )
    })?
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentCaptureRestoreOutcome {
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

async fn restore_agent_capture_from_d1_inner(
    db_conn: &sea_orm::DatabaseConnection,
    d1_client: &D1Client,
    repo_id: &str,
    render_human: bool,
) -> CloudResult<AgentCaptureRestoreOutcome> {
    use sea_orm::{ConnectionTrait, Statement};

    // Codex round-2 follow-up: check BOTH tables locally — a partial
    // schema (e.g. `agent_session` exists but `agent_checkpoint` does not
    // because a half-applied legacy migration left things mid-flight)
    // would otherwise bypass the warning and either fail loudly later or
    // silently succeed with no checkpoint rows. Warn and bail in that
    // case so the user gets a single actionable hint.
    let backend = db_conn.get_database_backend();
    let session_present = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_session' LIMIT 1",
            [],
        ))
        .await
        .map_err(|e| CloudError::Generic(format!("query sqlite_master: {e}")))?
        .is_some();
    let checkpoint_present = db_conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'agent_checkpoint' LIMIT 1",
            [],
        ))
        .await
        .map_err(|e| CloudError::Generic(format!("query sqlite_master: {e}")))?
        .is_some();
    if !session_present || !checkpoint_present {
        // Codex review Q4: emit an actionable hint instead of silently
        // succeeding so a user on an old binary knows why their session
        // list is empty after restore. Round-2 expanded this check to
        // include `agent_checkpoint` so a partial schema can't sneak past.
        emit_warning(
            "agent_session / agent_checkpoint table absent locally — restore skipped. \
             Run `libra init` (or upgrade libra) to create the schema, \
             then rerun `libra cloud restore`.",
        );
        return Ok(AgentCaptureRestoreOutcome::NoGeneration);
    }

    if render_human {
        println!("Restoring agent capture catalog from D1...");
    }

    // Restore is a read-only consumer of remote capture schema. In
    // particular, it must not call `ensure_agent_capture_generation_table`:
    // that sync-only migration installs persistent old-writer barriers and
    // adopts legacy rows. A failed restore must never change which clients can
    // write the backup. Legacy remotes with no capture rows are valid Git-only
    // backups and simply have no capture layer to restore.
    let generation_table_present = d1_client
        .agent_capture_generation_table_exists()
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "probe agent-capture generation schema: {}",
                error.message
            ))
        })?;
    if !generation_table_present {
        let has_capture_rows = d1_client
            .agent_capture_catalog_has_rows(repo_id)
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "probe legacy agent-capture catalog: {}",
                    error.message
                ))
            })?;
        validate_missing_capture_manifest(has_capture_rows)?;
        if render_human {
            println!("Agent capture restore: remote catalog is empty (skipped).");
        }
        return Ok(AgentCaptureRestoreOutcome::NoGeneration);
    }
    let subagent_content_present = d1_client
        .agent_subagent_content_tables_exist()
        .await
        .map_err(|error| {
            CloudError::D1(format!(
                "probe remote subagent-content schema: {}",
                error.message
            ))
        })?;

    let local_cloud_base = load_local_agent_capture_cloud_base(db_conn, repo_id).await?;
    let mut coherent = None;
    for _ in 0..3 {
        let before = d1_client
            .get_agent_capture_generation(repo_id)
            .await
            .map_err(|error| {
                CloudError::D1(format!("read agent capture generation: {}", error.message))
            })?;
        let Some(before) = before else {
            let has_capture_rows = d1_client
                .agent_capture_catalog_has_rows(repo_id)
                .await
                .map_err(|error| {
                    CloudError::D1(format!(
                        "probe unmanifested agent-capture catalog: {}",
                        error.message
                    ))
                })?;
            validate_missing_capture_manifest(has_capture_rows)?;
            if render_human {
                println!("Agent capture restore: remote catalog is empty (skipped).");
            }
            return Ok(AgentCaptureRestoreOutcome::NoGeneration);
        };
        if before.state != "complete" {
            return Err(CloudError::PartialTransfer(
                "remote agent capture publication is incomplete; retry `libra cloud sync`, then restore"
                    .to_string(),
            ));
        }
        let expected_object_digest = before.object_index_digest.as_deref().ok_or_else(|| {
            CloudError::PartialTransfer(
                "remote agent capture manifest predates object-index fencing; run `libra cloud sync` with the current version, then restore"
                    .to_string(),
            )
        })?;
        let expected_object_count = before.object_index_count.ok_or_else(|| {
            CloudError::PartialTransfer(
                "remote agent capture manifest has no object-index count; run `libra cloud sync` with the current version, then restore"
                .to_string(),
            )
        })?;
        let object_manifest_scope =
            AgentCaptureObjectManifestScope::parse(before.object_index_scope.as_deref())?;
        let expected_object_generation = before.object_index_generation.ok_or_else(|| {
            CloudError::PartialTransfer(
                "remote agent capture manifest has no object-index catalog generation; run `libra cloud sync` with the current version, then restore"
                    .to_string(),
            )
        })?;
        let (rows, remaining_restore_rows) =
            load_remote_agent_capture_rows(d1_client, repo_id, subagent_content_present).await?;
        validate_agent_capture_traces_shape(
            &rows.checkpoints,
            before.traces_head.as_deref(),
            "remote",
        )?;
        validate_agent_capture_companions(
            &rows.checkpoints,
            &rows.claims,
            &rows.revisions,
            &rows.links,
            "remote restore",
            CompanionValidationMode::Complete,
        )?;
        validate_agent_capture_session_dependencies(
            &rows.sessions,
            &rows.checkpoints,
            &rows.claims,
            "remote restore",
        )?;
        let durability_specs = rows
            .checkpoints
            .iter()
            .map(
                |checkpoint| crate::internal::ai::history::CheckpointDurabilitySpec {
                    checkpoint_id: &checkpoint.checkpoint_id,
                    traces_commit: &checkpoint.traces_commit,
                    tree_oid: &checkpoint.tree_oid,
                    metadata_blob_oid: &checkpoint.metadata_blob_oid,
                },
            )
            .collect::<Vec<_>>();
        let durability_deadline =
            std::time::Instant::now().checked_add(std::time::Duration::from_secs(110));
        let durable_oids = if durability_specs.is_empty() {
            HashSet::new()
        } else {
            let traces_head = before.traces_head.as_deref().ok_or_else(|| {
                CloudError::PartialTransfer(
                    "remote agent capture manifest has checkpoints but no fenced traces head; run `libra cloud sync` with the current version, then restore"
                        .to_string(),
                )
            })?;
            let cataloged_commits = rows
                .checkpoints
                .iter()
                .map(|row| row.traces_commit.clone())
                .collect::<Vec<_>>();
            crate::internal::ai::history::checkpoint_rows_snapshot_durable_oids_from_head(
                &util::storage_path(),
                traces_head,
                &cataloged_commits,
                &durability_specs,
                durability_deadline,
            )
            .await
            .map_err(|error| {
                CloudError::PartialTransfer(format!(
                    "restored agent-capture objects failed content and reachability validation: {error:#}; retry cloud restore, or run `libra agent doctor --repair` if the local object store remains damaged"
                ))
            })?
        };
        let mut required_oids = durable_oids.iter().cloned().collect::<Vec<_>>();
        required_oids.sort();
        let (object_indexes, observed_object_generation) = match object_manifest_scope {
            AgentCaptureObjectManifestScope::CheckpointProjection => {
                if required_oids.len() > remaining_restore_rows {
                    return Err(CloudError::PartialTransfer(format!(
                        "remote agent-capture restore exceeds its aggregate {}-row safety bound before reading object indexes",
                        AGENT_CAPTURE_RESTORE_MAX_ROWS
                    )));
                }
                d1_client
                    .get_object_indexes_by_oids_with_generation(repo_id, &required_oids)
                    .await
                    .map_err(|error| {
                        CloudError::D1(format!(
                            "read fenced agent-capture object indexes: {}",
                            error.message
                        ))
                    })?
            }
            AgentCaptureObjectManifestScope::FullRemoteIndex => d1_client
                .get_object_indexes_bounded_with_generation(repo_id, remaining_restore_rows)
                .await
                .map_err(|error| {
                    CloudError::D1(format!(
                        "read full retained agent-capture object manifest within the aggregate restore row budget: {}",
                        error.message
                    ))
                })?,
        };
        remaining_restore_rows
            .checked_sub(object_indexes.len())
            .ok_or_else(|| {
                CloudError::PartialTransfer(format!(
                    "remote agent-capture restore exceeds its aggregate {}-row safety bound while reading object indexes",
                    AGENT_CAPTURE_RESTORE_MAX_ROWS
                ))
            })?;
        let fenced_oids = object_indexes
            .iter()
            .map(|index| index.o_id.as_str())
            .collect::<HashSet<_>>();
        if let Some(unsynced) = object_indexes.iter().find(|index| index.is_synced != 1) {
            return Err(CloudError::PartialTransfer(format!(
                "agent-capture manifest includes object {} that is not marked synced",
                unsynced.o_id
            )));
        }
        if let Some(missing) = durable_oids
            .iter()
            .find(|oid| !fenced_oids.contains(oid.as_str()))
        {
            return Err(CloudError::PartialTransfer(format!(
                "agent-capture generation requires object {missing}, but its fenced object-index row is missing; retry `libra cloud sync`, then restore"
            )));
        }
        let (observed_object_digest, observed_object_count) =
            agent_capture_object_index_digest(&object_indexes)?;
        let after = d1_client
            .get_agent_capture_generation(repo_id)
            .await
            .map_err(|error| {
                CloudError::D1(format!(
                    "recheck agent capture generation: {}",
                    error.message
                ))
            })?;
        if after.as_ref() == Some(&before)
            && observed_object_digest == expected_object_digest
            && observed_object_count == expected_object_count
            && observed_object_generation >= expected_object_generation
        {
            coherent = Some((rows, before.traces_head.clone(), before.generation));
            break;
        }
    }
    let (rows, traces_head, remote_generation) = coherent.ok_or_else(|| {
        CloudError::PartialTransfer(
            "remote agent capture changed during three bounded restore reads; retry when cloud sync is idle"
                .to_string(),
        )
    })?;

    restore_agent_capture_from_rows_with_subagents(
        db_conn,
        AgentCaptureRestoreRows {
            sessions: &rows.sessions,
            checkpoints: &rows.checkpoints,
            claims: &rows.claims,
            revisions: &rows.revisions,
            links: &rows.links,
            traces_head: traces_head.as_deref(),
            remote_is_known_ancestor: local_cloud_base == Some(remote_generation),
        },
        render_human,
    )
    .await?;
    // PD-03: propagate the remote session tombstones to THIS machine, so
    // a later local import cannot resurrect a session erased elsewhere.
    persist_local_import_tombstones(db_conn, &rows.import_tombstones).await?;
    store_local_agent_capture_cloud_base(db_conn, repo_id, remote_generation).await?;
    Ok(AgentCaptureRestoreOutcome::GenerationInstalled)
}

/// PD-03: UPSERT restored session tombstones into the local
/// `agent_import_tombstone` table (same idempotent shape the local erase
/// writes: newest `erased_at` wins, known fingerprints are kept). A
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
            CloudError::Generic(format!("query local import-tombstone schema: {error}"))
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
                    source_fingerprint = COALESCE(
                        excluded.source_fingerprint,
                        agent_import_tombstone.source_fingerprint
                    ),
                    erased_at = MAX(agent_import_tombstone.erased_at, excluded.erased_at)",
                [
                    uuid::Uuid::new_v4().to_string().into(),
                    row.agent_kind.clone().into(),
                    row.provider_session_id.clone().into(),
                    row.erased_session_id.clone().into(),
                    Value::from(row.source_fingerprint.clone()),
                    row.erased_at.into(),
                ],
            ))
            .await
            .map_err(|error| {
                CloudError::Generic(format!(
                    "persist restored session tombstone for {}/{}: {error}",
                    row.agent_kind, row.provider_session_id
                ))
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
        .map_err(|error| {
            CloudError::D1(format!(
                "list aggregate-bounded agent-capture restore catalog: {}",
                error.message
            ))
        })?;
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
#[cfg(test)]
async fn restore_agent_capture_from_rows(
    db_conn: &sea_orm::DatabaseConnection,
    session_rows: &[AgentSessionV2Row],
    checkpoint_rows: &[AgentCheckpointV2Row],
    render_human: bool,
) -> CloudResult<()> {
    restore_agent_capture_from_rows_with_subagents(
        db_conn,
        AgentCaptureRestoreRows {
            sessions: session_rows,
            checkpoints: checkpoint_rows,
            claims: &[],
            revisions: &[],
            links: &[],
            traces_head: None,
            remote_is_known_ancestor: true,
        },
        render_human,
    )
    .await
}

async fn restore_agent_capture_from_rows_with_subagents(
    db_conn: &sea_orm::DatabaseConnection,
    rows: AgentCaptureRestoreRows<'_>,
    render_human: bool,
) -> CloudResult<()> {
    use sea_orm::Statement;

    let AgentCaptureRestoreRows {
        sessions: session_rows,
        checkpoints: checkpoint_rows,
        claims: claim_rows,
        revisions: revision_rows,
        links: link_rows,
        traces_head,
        remote_is_known_ancestor,
    } = rows;

    validate_agent_capture_companions(
        checkpoint_rows,
        claim_rows,
        revision_rows,
        link_rows,
        "restored remote",
        CompanionValidationMode::Complete,
    )?;
    validate_agent_capture_session_dependencies(
        session_rows,
        checkpoint_rows,
        claim_rows,
        "restored remote",
    )?;
    // Reads the existing refs/rows before rewriting them (`restore fenced
    // traces ref` below), so the write lock is taken up front.
    let txn = crate::internal::db::begin_write_transaction(db_conn)
        .await
        .map_err(|error| {
            CloudError::Generic(format!("begin atomic agent capture restore: {error}"))
        })?;
    let backend = txn.get_database_backend();

    // An ordinary retention prune is a durable local deletion intent. The
    // remote may still expose its previous complete generation until the next
    // sync publishes that tombstone, so fail before changing the traces ref or
    // catalog instead of resurrecting a checkpoint and its companion rows.
    if !checkpoint_rows.is_empty() {
        let tombstone_rows = txn
            .query_all_raw(Statement::from_string(
                backend,
                format!(
                    "SELECT checkpoint_id FROM agent_checkpoint_prune_tombstone \
                     ORDER BY checkpoint_id LIMIT {}",
                    AGENT_CAPTURE_MAX_ROWS_PER_TABLE.saturating_add(1)
                ),
            ))
            .await
            .map_err(|error| {
                CloudError::Generic(format!(
                    "inspect local checkpoint prune tombstones before restore: {error}"
                ))
            })?;
        if tombstone_rows.len() > AGENT_CAPTURE_MAX_ROWS_PER_TABLE {
            return Err(CloudError::Generic(format!(
                "local checkpoint prune tombstones exceed the {}-row restore safety bound; \
                 run `libra cloud sync` before restoring",
                AGENT_CAPTURE_MAX_ROWS_PER_TABLE
            )));
        }
        let remote_checkpoint_ids = checkpoint_rows
            .iter()
            .map(|row| row.checkpoint_id.as_str())
            .collect::<HashSet<_>>();
        for row in tombstone_rows {
            let checkpoint_id = row
                .try_get_by::<String, _>("checkpoint_id")
                .map_err(|error| {
                    CloudError::Generic(format!(
                        "decode local checkpoint prune tombstone before restore: {error}"
                    ))
                })?;
            if remote_checkpoint_ids.contains(checkpoint_id.as_str()) {
                return Err(CloudError::PartialTransfer(format!(
                    "remote checkpoint {checkpoint_id} was already pruned locally; run `libra cloud sync` to publish the prune tombstone before restoring"
                )));
            }
        }
    }

    let existing_traces_ref = txn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT id, `commit` FROM reference
             WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
            [crate::internal::branch::TRACES_BRANCH.into()],
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("inspect local traces ref: {error}")))?;
    let existing_head = existing_traces_ref
        .as_ref()
        .map(|row| row.try_get_by::<Option<String>, _>("commit"))
        .transpose()
        .map_err(|error| CloudError::Generic(format!("decode local traces ref: {error}")))?
        .flatten();
    if existing_head.as_deref() != traces_head {
        let local_checkpoint_count = txn
            .query_one_raw(Statement::from_string(
                backend,
                "SELECT COUNT(*) AS n FROM agent_checkpoint".to_string(),
            ))
            .await
            .map_err(|error| {
                CloudError::Generic(format!("count local checkpoints before restore: {error}"))
            })?
            .ok_or_else(|| {
                CloudError::Generic("local checkpoint count returned no row".to_string())
            })?
            .try_get_by::<i64, _>("n")
            .map_err(|error| CloudError::Generic(format!("decode checkpoint count: {error}")))?;
        if local_checkpoint_count != 0 {
            return Err(CloudError::Generic(
                "the fenced cloud traces head conflicts with existing local checkpoint history; restore into an empty repository or sync the newer local history first"
                    .to_string(),
            ));
        }
    }
    if let Some(row) = existing_traces_ref {
        let ref_id: i64 = row
            .try_get_by("id")
            .map_err(|error| CloudError::Generic(format!("decode traces ref id: {error}")))?;
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "UPDATE reference SET `commit` = ? WHERE id = ?",
            [traces_head.map(str::to_string).into(), ref_id.into()],
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("restore fenced traces ref: {error}")))?;
    } else {
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO reference (name, kind, `commit`, remote, worktree_id)
             VALUES (?, 'Branch', ?, NULL, NULL)",
            [
                crate::internal::branch::TRACES_BRANCH.into(),
                traces_head.map(str::to_string).into(),
            ],
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("create fenced traces ref: {error}")))?;
    }

    // PD-03 tombstone-first, LOCAL side. The remote catalog is filtered by
    // the tombstones the mirror knows about, but a session erased here and
    // not yet propagated is invisible to that filter — restoring from a
    // stale mirror would otherwise bring it back. The schema triggers would
    // abort such a write, which is the right outcome but the wrong
    // experience: the whole restore fails on an opaque RAISE(ABORT) instead
    // of quietly declining the rows the user already erased. Read the local
    // tombstones inside the SAME transaction and skip them.
    let mut locally_erased_sessions: HashSet<(String, String)> = HashSet::new();
    let mut locally_erased_session_ids: HashSet<String> = HashSet::new();
    for row in txn
        .query_all_raw(Statement::from_string(
            backend,
            "SELECT agent_kind, provider_session_id, erased_session_id \
             FROM agent_import_tombstone",
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("read local erasure tombstones: {error}")))?
    {
        let agent_kind: String = row
            .try_get_by("agent_kind")
            .map_err(|error| CloudError::Generic(format!("decode local tombstone: {error}")))?;
        let provider_session_id: String = row
            .try_get_by("provider_session_id")
            .map_err(|error| CloudError::Generic(format!("decode local tombstone: {error}")))?;
        let erased_session_id: String = row
            .try_get_by("erased_session_id")
            .map_err(|error| CloudError::Generic(format!("decode local tombstone: {error}")))?;
        locally_erased_sessions.insert((agent_kind, provider_session_id));
        locally_erased_session_ids.insert(erased_session_id);
    }
    // Collect the dropped sessions' REMOTE ids before filtering: a
    // checkpoint references its session by id, and the mirror's id for an
    // erased session need not match the local `erased_session_id`.
    let dropped_session_ids: HashSet<String> = session_rows
        .iter()
        .filter(|row| {
            locally_erased_sessions
                .contains(&(row.agent_kind.clone(), row.provider_session_id.clone()))
        })
        .map(|row| row.session_id.clone())
        .collect();
    let session_rows: Vec<_> = session_rows
        .iter()
        .filter(|row| {
            !locally_erased_sessions
                .contains(&(row.agent_kind.clone(), row.provider_session_id.clone()))
        })
        .cloned()
        .collect();
    let session_rows = &session_rows[..];
    let checkpoint_rows: Vec<_> = checkpoint_rows
        .iter()
        .filter(|row| {
            !locally_erased_session_ids.contains(&row.session_id)
                && !dropped_session_ids.contains(&row.session_id)
        })
        .cloned()
        .collect();
    let checkpoint_rows = &checkpoint_rows[..];

    // Validate every immutable/mutable companion conflict before applying any
    // row. The surrounding transaction guarantees a later SQL/FK failure also
    // rolls back sessions, checkpoints, skeleton claims, revisions, and links.
    let mut newer_local_sessions = HashSet::new();
    for row in session_rows {
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT session_id, agent_kind, provider_session_id, state, working_dir,
                        worktree_id, parent_commit, parent_session_id, metadata_json,
                        redaction_report, started_at, last_event_at, stopped_at,
                        schema_version, sync_revision
                 FROM agent_session WHERE agent_kind = ? AND provider_session_id = ?",
                [
                    row.agent_kind.clone().into(),
                    row.provider_session_id.clone().into(),
                ],
            ))
            .await
            .map_err(|error| {
                CloudError::Generic(format!("inspect local agent session: {error}"))
            })?;
        if let Some(existing) = existing {
            let local = AgentSessionV2Row {
                session_id: existing
                    .try_get_by("session_id")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                agent_kind: existing
                    .try_get_by("agent_kind")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                provider_session_id: existing
                    .try_get_by("provider_session_id")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                state: existing
                    .try_get_by("state")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                working_dir: existing
                    .try_get_by("working_dir")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                worktree_id: existing
                    .try_get_by("worktree_id")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                parent_commit: existing
                    .try_get_by("parent_commit")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                parent_session_id: existing
                    .try_get_by("parent_session_id")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                metadata_json: existing
                    .try_get_by("metadata_json")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                redaction_report: existing
                    .try_get_by("redaction_report")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                started_at: existing
                    .try_get_by("started_at")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                last_event_at: existing
                    .try_get_by("last_event_at")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                stopped_at: existing
                    .try_get_by("stopped_at")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                schema_version: existing
                    .try_get_by("schema_version")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
                sync_revision: existing
                    .try_get_by("sync_revision")
                    .map_err(|error| CloudError::Generic(format!("decode session: {error}")))?,
            };
            if local.sync_revision > row.sync_revision {
                if !remote_is_known_ancestor {
                    return Err(CloudError::Generic(format!(
                        "local agent session {} has a larger divergent sync revision without a recorded cloud ancestor; sync or restore from the clone that owns the current cloud lineage",
                        local.session_id
                    )));
                }
                newer_local_sessions
                    .insert((row.agent_kind.clone(), row.provider_session_id.clone()));
            } else if local.sync_revision == row.sync_revision && local != *row {
                return Err(CloudError::Generic(format!(
                    "restored agent session {} conflicts with local state at the same sync generation",
                    row.session_id
                )));
            } else if local.session_id != row.session_id {
                return Err(CloudError::Generic(format!(
                    "restored agent session {} conflicts with local provider ownership",
                    row.session_id
                )));
            }
        }
    }
    for row in checkpoint_rows {
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT session_id, parent_checkpoint_id, scope, parent_commit, tree_oid,
                        metadata_blob_oid, traces_commit, tool_use_id, subagent_session_id,
                        description, created_at, sync_revision
                 FROM agent_checkpoint WHERE checkpoint_id = ?",
                [row.checkpoint_id.clone().into()],
            ))
            .await
            .map_err(|error| CloudError::Generic(format!("inspect local checkpoint: {error}")))?;
        if let Some(existing) = existing {
            let local = AgentCheckpointV2Row {
                checkpoint_id: row.checkpoint_id.clone(),
                session_id: existing
                    .try_get_by("session_id")
                    .map_err(|error| CloudError::Generic(format!("decode checkpoint: {error}")))?,
                parent_checkpoint_id: existing
                    .try_get_by("parent_checkpoint_id")
                    .map_err(|error| CloudError::Generic(format!("decode checkpoint: {error}")))?,
                scope: existing
                    .try_get_by("scope")
                    .map_err(|error| CloudError::Generic(format!("decode checkpoint: {error}")))?,
                parent_commit: existing
                    .try_get_by("parent_commit")
                    .map_err(|error| CloudError::Generic(format!("decode checkpoint: {error}")))?,
                tree_oid: existing
                    .try_get_by("tree_oid")
                    .map_err(|error| CloudError::Generic(format!("decode checkpoint: {error}")))?,
                metadata_blob_oid: existing
                    .try_get_by("metadata_blob_oid")
                    .map_err(|error| CloudError::Generic(format!("decode checkpoint: {error}")))?,
                traces_commit: existing
                    .try_get_by("traces_commit")
                    .map_err(|error| CloudError::Generic(format!("decode checkpoint: {error}")))?,
                tool_use_id: existing
                    .try_get_by("tool_use_id")
                    .map_err(|error| CloudError::Generic(format!("decode checkpoint: {error}")))?,
                subagent_session_id: existing
                    .try_get_by("subagent_session_id")
                    .map_err(|error| CloudError::Generic(format!("decode checkpoint: {error}")))?,
                description: existing
                    .try_get_by("description")
                    .map_err(|error| CloudError::Generic(format!("decode checkpoint: {error}")))?,
                created_at: existing
                    .try_get_by("created_at")
                    .map_err(|error| CloudError::Generic(format!("decode checkpoint: {error}")))?,
                sync_revision: existing
                    .try_get_by("sync_revision")
                    .map_err(|error| CloudError::Generic(format!("decode checkpoint: {error}")))?,
            };
            if local.sync_revision == row.sync_revision && local != *row {
                return Err(CloudError::Generic(format!(
                    "restored checkpoint {} conflicts with local history at the same sync generation",
                    row.checkpoint_id
                )));
            }
            if local.sync_revision > row.sync_revision && !remote_is_known_ancestor {
                return Err(CloudError::Generic(format!(
                    "local checkpoint {} has a larger divergent sync revision without a recorded cloud ancestor; sync or restore from the clone that owns the current cloud lineage",
                    row.checkpoint_id
                )));
            }
            if local.sync_revision != row.sync_revision
                && !checkpoint_rewrite_compatible(&local, row)
            {
                return Err(CloudError::Generic(format!(
                    "restored checkpoint {} conflicts with immutable local history",
                    row.checkpoint_id
                )));
            }
        }
    }
    let mut newer_local_claims = HashSet::new();
    for row in revision_rows {
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT checkpoint_id, content_digest, source_channel, partial, created_at
                 FROM agent_subagent_content_revision
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ? AND revision = ?",
                [
                    row.parent_session_id.clone().into(),
                    row.provider_kind.clone().into(),
                    row.source_key.clone().into(),
                    row.content_schema_version.into(),
                    row.revision.into(),
                ],
            ))
            .await
            .map_err(|error| {
                CloudError::Generic(format!("inspect local subagent revision: {error}"))
            })?;
        if let Some(existing) = existing {
            let exact = existing
                .try_get_by::<String, _>("checkpoint_id")
                .map_err(|error| CloudError::Generic(format!("decode revision: {error}")))?
                == row.checkpoint_id
                && existing
                    .try_get_by::<String, _>("content_digest")
                    .map_err(|error| CloudError::Generic(format!("decode revision: {error}")))?
                    == row.content_digest
                && existing
                    .try_get_by::<String, _>("source_channel")
                    .map_err(|error| CloudError::Generic(format!("decode revision: {error}")))?
                    == row.source_channel
                && existing
                    .try_get_by::<i64, _>("partial")
                    .map_err(|error| CloudError::Generic(format!("decode revision: {error}")))?
                    == row.partial
                && existing
                    .try_get_by::<i64, _>("created_at")
                    .map_err(|error| CloudError::Generic(format!("decode revision: {error}")))?
                    == row.created_at;
            if !exact {
                return Err(CloudError::Generic(format!(
                    "restored subagent revision {} conflicts with immutable local history",
                    row.checkpoint_id
                )));
            }
        }
    }
    for row in claim_rows {
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT revision_cursor, sync_revision, current_revision, current_checkpoint_id,
                        current_digest, state
                 FROM agent_subagent_content_claim
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ?",
                [
                    row.parent_session_id.clone().into(),
                    row.provider_kind.clone().into(),
                    row.source_key.clone().into(),
                    row.content_schema_version.into(),
                ],
            ))
            .await
            .map_err(|error| {
                CloudError::Generic(format!("inspect local subagent claim: {error}"))
            })?;
        if let Some(existing) = existing {
            let state: String = existing
                .try_get_by("state")
                .map_err(|error| CloudError::Generic(format!("decode claim state: {error}")))?;
            if state != "idle" {
                return Err(CloudError::Generic(
                    "cannot restore a subagent claim while a local writer reservation is active"
                        .to_string(),
                ));
            }
            let sync_revision: i64 = existing.try_get_by("sync_revision").map_err(|error| {
                CloudError::Generic(format!("decode claim sync generation: {error}"))
            })?;
            if sync_revision > row.sync_revision {
                if !remote_is_known_ancestor {
                    return Err(CloudError::Generic(format!(
                        "local subagent claim for {} has a larger divergent sync revision without a recorded cloud ancestor",
                        row.source_key
                    )));
                }
                newer_local_claims.insert(claim_key(row));
            } else if sync_revision == row.sync_revision {
                let exact = existing
                    .try_get_by::<i64, _>("revision_cursor")
                    .map_err(|error| {
                        CloudError::Generic(format!("decode claim cursor: {error}"))
                    })?
                    == row.revision_cursor
                    && existing
                        .try_get_by::<i64, _>("current_revision")
                        .map_err(|error| {
                            CloudError::Generic(format!("decode claim revision: {error}"))
                        })?
                        == row.current_revision
                    && existing
                        .try_get_by::<Option<String>, _>("current_checkpoint_id")
                        .map_err(|error| {
                            CloudError::Generic(format!("decode claim checkpoint: {error}"))
                        })?
                        == row.current_checkpoint_id
                    && existing
                        .try_get_by::<Option<String>, _>("current_digest")
                        .map_err(|error| {
                            CloudError::Generic(format!("decode claim digest: {error}"))
                        })?
                        == row.current_digest;
                if !exact {
                    return Err(CloudError::Generic(
                        "restored subagent claim conflicts with local state at the same sync generation"
                            .to_string(),
                    ));
                }
            }
        }
    }
    let mut newer_local_links = HashSet::new();
    for row in link_rows {
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT parent_session_id, link_state, boundary_checkpoint_id,
                        stable_subagent_id, sync_revision, created_at, updated_at
                 FROM agent_subagent_link WHERE content_checkpoint_id = ?",
                [row.content_checkpoint_id.clone().into()],
            ))
            .await
            .map_err(|error| {
                CloudError::Generic(format!("inspect local subagent link: {error}"))
            })?;
        if let Some(existing) = existing {
            let sync_revision: i64 = existing
                .try_get_by("sync_revision")
                .map_err(|error| CloudError::Generic(format!("decode link revision: {error}")))?;
            if sync_revision > row.sync_revision {
                if !remote_is_known_ancestor {
                    return Err(CloudError::Generic(format!(
                        "local subagent link {} has a larger divergent sync revision without a recorded cloud ancestor",
                        row.content_checkpoint_id
                    )));
                }
                newer_local_links.insert(row.content_checkpoint_id.clone());
            } else if sync_revision == row.sync_revision {
                let exact = existing
                    .try_get_by::<String, _>("parent_session_id")
                    .map_err(|error| CloudError::Generic(format!("decode link: {error}")))?
                    == row.parent_session_id
                    && existing
                        .try_get_by::<String, _>("link_state")
                        .map_err(|error| CloudError::Generic(format!("decode link: {error}")))?
                        == row.link_state
                    && existing
                        .try_get_by::<Option<String>, _>("boundary_checkpoint_id")
                        .map_err(|error| CloudError::Generic(format!("decode link: {error}")))?
                        == row.boundary_checkpoint_id
                    && existing
                        .try_get_by::<Option<String>, _>("stable_subagent_id")
                        .map_err(|error| CloudError::Generic(format!("decode link: {error}")))?
                        == row.stable_subagent_id
                    && existing
                        .try_get_by::<i64, _>("created_at")
                        .map_err(|error| CloudError::Generic(format!("decode link: {error}")))?
                        == row.created_at
                    && existing
                        .try_get_by::<i64, _>("updated_at")
                        .map_err(|error| CloudError::Generic(format!("decode link: {error}")))?
                        == row.updated_at;
                if !exact {
                    return Err(CloudError::Generic(format!(
                        "restored subagent link {} conflicts with local state at the same generation",
                        row.content_checkpoint_id
                    )));
                }
            }
        }
    }

    let mut skipped_scoped_sessions = 0usize;
    for row in session_rows {
        if newer_local_sessions.contains(&(row.agent_kind.clone(), row.provider_session_id.clone()))
        {
            continue;
        }
        // §C.4.1.1: a LOCALLY SCOPED session belongs to a live workspace
        // owner; cloud content carries no scope columns, so overwriting the
        // row would graft foreign material under an immutable owner claim.
        // The UPSERT below skips such rows via its scope predicate — count
        // them so the user learns the restore was partial.
        let scoped_conflict = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT 1 AS hit FROM agent_session \
                 WHERE agent_kind = ? AND provider_session_id = ? \
                   AND scope_state IS 'scoped' AND sync_revision < ?",
                [
                    row.agent_kind.clone().into(),
                    row.provider_session_id.clone().into(),
                    row.sync_revision.into(),
                ],
            ))
            .await
            .map_err(|error| {
                CloudError::Generic(format!("probe scoped session conflict: {error}"))
            })?
            .is_some();
        if scoped_conflict {
            skipped_scoped_sessions += 1;
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                worktree_id, parent_commit, parent_session_id, metadata_json,
                redaction_report, started_at, last_event_at, stopped_at, schema_version,
                sync_revision
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(agent_kind, provider_session_id) DO UPDATE SET
                state = excluded.state, working_dir = excluded.working_dir,
                worktree_id = excluded.worktree_id, parent_commit = excluded.parent_commit,
                parent_session_id = excluded.parent_session_id,
                metadata_json = excluded.metadata_json,
                redaction_report = excluded.redaction_report,
                started_at = excluded.started_at,
                last_event_at = excluded.last_event_at,
                stopped_at = excluded.stopped_at,
                schema_version = excluded.schema_version,
                sync_revision = excluded.sync_revision
             WHERE excluded.sync_revision > agent_session.sync_revision
               AND agent_session.scope_state IS NOT 'scoped'",
            [
                row.session_id.clone().into(),
                row.agent_kind.clone().into(),
                row.provider_session_id.clone().into(),
                row.state.clone().into(),
                row.working_dir.clone().into(),
                row.worktree_id.clone().into(),
                row.parent_commit.clone().into(),
                row.parent_session_id.clone().into(),
                row.metadata_json.clone().into(),
                row.redaction_report.clone().into(),
                row.started_at.into(),
                row.last_event_at.into(),
                row.stopped_at.into(),
                row.schema_version.into(),
                row.sync_revision.into(),
            ],
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!("restore agent session {}: {error}", row.session_id))
        })?;
    }
    if skipped_scoped_sessions > 0 {
        emit_warning(format!(
            "cloud restore left {skipped_scoped_sessions} locally SCOPED agent session(s) \
             untouched: their rows are owned by a live workspace and cloud content carries \
             no ownership; inspect with `libra worktree doctor`"
        ));
    }
    for row in checkpoint_rows {
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at, sync_revision
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(checkpoint_id) DO UPDATE SET
                tree_oid = excluded.tree_oid,
                metadata_blob_oid = excluded.metadata_blob_oid,
                traces_commit = excluded.traces_commit,
                sync_revision = excluded.sync_revision
             WHERE excluded.sync_revision > agent_checkpoint.sync_revision",
            [
                row.checkpoint_id.clone().into(),
                row.session_id.clone().into(),
                row.parent_checkpoint_id.clone().into(),
                row.scope.clone().into(),
                row.parent_commit.clone().into(),
                row.tree_oid.clone().into(),
                row.metadata_blob_oid.clone().into(),
                row.traces_commit.clone().into(),
                row.tool_use_id.clone().into(),
                row.subagent_session_id.clone().into(),
                row.description.clone().into(),
                row.created_at.into(),
                row.sync_revision.into(),
            ],
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!(
                "restore agent checkpoint {}: {error}",
                row.checkpoint_id
            ))
        })?;
    }

    // Skeleton claims satisfy the revision FK but remain invisible outside the
    // transaction. Current leaves are advanced only after revisions and links.
    for row in claim_rows {
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_subagent_content_claim (
                parent_session_id, provider_kind, source_key, content_schema_version,
                revision_cursor, sync_revision, current_revision, current_checkpoint_id, current_digest,
                state, attempt_digest, attempt_checkpoint_id, owner, lease_expires_at,
                fence_token, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 0, 0, 0, NULL, NULL, 'idle', NULL, NULL, NULL, NULL,
                       ?, ?, ?)
             ON CONFLICT(parent_session_id, provider_kind, source_key,
                         content_schema_version) DO NOTHING",
            [
                row.parent_session_id.clone().into(),
                row.provider_kind.clone().into(),
                row.source_key.clone().into(),
                row.content_schema_version.into(),
                row.fence_token.into(),
                row.created_at.into(),
                row.updated_at.into(),
            ],
        ))
        .await
        .map_err(|error| CloudError::Generic(format!("stage restored claim: {error}")))?;
    }
    for row in revision_rows {
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_subagent_content_revision (
                parent_session_id, provider_kind, source_key, content_schema_version,
                revision, checkpoint_id, content_digest, source_channel, partial, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(parent_session_id, provider_kind, source_key,
                         content_schema_version, revision) DO NOTHING",
            [
                row.parent_session_id.clone().into(),
                row.provider_kind.clone().into(),
                row.source_key.clone().into(),
                row.content_schema_version.into(),
                row.revision.into(),
                row.checkpoint_id.clone().into(),
                row.content_digest.clone().into(),
                row.source_channel.clone().into(),
                row.partial.into(),
                row.created_at.into(),
            ],
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!(
                "restore subagent revision {}: {error}",
                row.checkpoint_id
            ))
        })?;
    }
    for row in link_rows {
        if newer_local_links.contains(&row.content_checkpoint_id) {
            continue;
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_subagent_link (
                content_checkpoint_id, parent_session_id, link_state,
                boundary_checkpoint_id, stable_subagent_id, sync_revision,
                created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(content_checkpoint_id) DO UPDATE SET
                parent_session_id = excluded.parent_session_id,
                link_state = excluded.link_state,
                boundary_checkpoint_id = excluded.boundary_checkpoint_id,
                stable_subagent_id = excluded.stable_subagent_id,
                sync_revision = excluded.sync_revision,
                created_at = excluded.created_at, updated_at = excluded.updated_at
             WHERE excluded.sync_revision > agent_subagent_link.sync_revision",
            [
                row.content_checkpoint_id.clone().into(),
                row.parent_session_id.clone().into(),
                row.link_state.clone().into(),
                row.boundary_checkpoint_id.clone().into(),
                row.stable_subagent_id.clone().into(),
                row.sync_revision.into(),
                row.created_at.into(),
                row.updated_at.into(),
            ],
        ))
        .await
        .map_err(|error| {
            CloudError::Generic(format!(
                "restore subagent link {}: {error}",
                row.content_checkpoint_id
            ))
        })?;
    }
    for row in claim_rows {
        if newer_local_claims.contains(&claim_key(row)) {
            continue;
        }
        let result = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE agent_subagent_content_claim
                 SET revision_cursor = ?, sync_revision = ?, current_revision = ?, current_checkpoint_id = ?,
                     current_digest = ?, fence_token = MAX(fence_token, ?),
                     created_at = MIN(created_at, ?), updated_at = MAX(updated_at, ?)
                 WHERE parent_session_id = ? AND provider_kind = ? AND source_key = ?
                   AND content_schema_version = ? AND state = 'idle'
                   AND (sync_revision < ? OR (
                        sync_revision = ? AND revision_cursor = ? AND current_revision = ?
                        AND current_checkpoint_id IS ? AND current_digest IS ?))",
                [
                    row.revision_cursor.into(),
                    row.sync_revision.into(),
                    row.current_revision.into(),
                    row.current_checkpoint_id.clone().into(),
                    row.current_digest.clone().into(),
                    row.fence_token.into(),
                    row.created_at.into(),
                    row.updated_at.into(),
                    row.parent_session_id.clone().into(),
                    row.provider_kind.clone().into(),
                    row.source_key.clone().into(),
                    row.content_schema_version.into(),
                    row.sync_revision.into(),
                    row.sync_revision.into(),
                    row.revision_cursor.into(),
                    row.current_revision.into(),
                    row.current_checkpoint_id.clone().into(),
                    row.current_digest.clone().into(),
                ],
            ))
            .await
            .map_err(|error| CloudError::Generic(format!("advance restored claim: {error}")))?;
        if result.rows_affected() != 1 {
            return Err(CloudError::Generic(
                "restored subagent claim lost its atomic monotonic update fence".to_string(),
            ));
        }
    }

    txn.commit().await.map_err(|error| {
        CloudError::Generic(format!("commit atomic agent capture restore: {error}"))
    })?;
    if render_human {
        println!(
            "Agent capture restore: {}/{} sessions, {}/{} checkpoints, {}/{} subagent claims, {}/{} subagent revisions, {}/{} subagent links (0 failed).",
            session_rows.len(),
            session_rows.len(),
            checkpoint_rows.len(),
            checkpoint_rows.len(),
            claim_rows.len(),
            claim_rows.len(),
            revision_rows.len(),
            revision_rows.len(),
            link_rows.len(),
            link_rows.len()
        );
    }
    Ok(())
}

async fn restore_metadata(
    db_conn: &sea_orm::DatabaseConnection,
    r2_storage: &RemoteStorage,
) -> CloudResult<Vec<reference::Model>> {
    println!("Restoring metadata...");

    let data = match r2_storage.get_metadata().await {
        Ok(data) => data,
        Err(e) => {
            println!("warning: failed to download metadata: {}", e);
            return Ok(Vec::new());
        }
    };
    let deferred_capture_refs = restore_metadata_from_bytes(db_conn, &data).await?;
    println!("Metadata restored.");
    Ok(deferred_capture_refs)
}

/// Restore refs metadata and fail hard when the metadata object is missing.
///
/// `libra cloud restore` keeps its historical warning-only behavior through
/// [`restore_metadata`]. Cloud clone restore needs a stricter contract: without
/// refs metadata it cannot set HEAD/branches safely, so the caller must fail and
/// clean up the just-created destination.
pub(crate) async fn restore_metadata_strict(
    db_conn: &sea_orm::DatabaseConnection,
    r2_storage: &RemoteStorage,
) -> CloudResult<()> {
    let data = r2_storage
        .get_metadata()
        .await
        .map_err(|e| CloudError::Generic(format!("failed to download metadata: {}", e)))?;
    restore_metadata_from_bytes_strict(db_conn, &data).await
}

async fn restore_metadata_from_bytes(
    db_conn: &sea_orm::DatabaseConnection,
    data: &[u8],
) -> CloudResult<Vec<reference::Model>> {
    let references: Vec<reference::Model> = serde_json::from_slice(data)
        .map_err(|e| CloudError::Generic(format!("Failed to deserialize metadata: {}", e)))?;
    restore_metadata_models(db_conn, references, false).await
}

async fn restore_metadata_from_bytes_strict(
    db_conn: &sea_orm::DatabaseConnection,
    data: &[u8],
) -> CloudResult<()> {
    let references: Vec<reference::Model> = serde_json::from_slice(data)
        .map_err(|e| CloudError::Generic(format!("Failed to deserialize metadata: {}", e)))?;
    validate_strict_refs_metadata(&references)?;
    restore_metadata_models_with_capture_policy(db_conn, references, true, false)
        .await
        .map(|_| ())
}

fn validate_strict_refs_metadata(references: &[reference::Model]) -> CloudResult<()> {
    if !references
        .iter()
        .any(|model| model.kind == reference::ConfigKind::Head && model.remote.is_none())
    {
        return Err(CloudError::Generic(
            "metadata does not contain local HEAD reference".to_string(),
        ));
    }
    Ok(())
}

async fn restore_metadata_models(
    db_conn: &sea_orm::DatabaseConnection,
    references: Vec<reference::Model>,
    strict: bool,
) -> CloudResult<Vec<reference::Model>> {
    restore_metadata_models_with_capture_policy(db_conn, references, strict, true).await
}

async fn restore_metadata_models_with_capture_policy(
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
    fn merge_import_tombstones_keeps_newest_and_fingerprints() {
        use crate::utils::d1_client::AgentImportTombstoneRow;

        let row = |erased_at: i64, fp: Option<&str>| AgentImportTombstoneRow {
            agent_kind: "claude_code".to_string(),
            provider_session_id: "prov-1".to_string(),
            erased_session_id: format!("sess-{erased_at}"),
            source_fingerprint: fp.map(str::to_string),
            erased_at,
        };
        // Newest erased_at wins; an older row's fingerprint survives when
        // the newer one lacks it.
        let merged = super::merge_import_tombstones(&[row(5, None)], &[row(3, Some("aa"))]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].erased_at, 5);
        assert_eq!(merged[0].erased_session_id, "sess-5");
        assert_eq!(merged[0].source_fingerprint.as_deref(), Some("aa"));
        // Distinct provider identities stay distinct.
        let mut other = row(7, None);
        other.provider_session_id = "prov-2".to_string();
        let merged = super::merge_import_tombstones(&[row(5, None)], &[other]);
        assert_eq!(merged.len(), 2);
        // Idempotent under replay: merging the merged set changes nothing.
        let replay = super::merge_import_tombstones(&merged, &merged);
        assert_eq!(replay, merged);
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
        let report = CloudSyncReport {
            repo_id: "repo-2".to_string(),
            project_name: "project-2".to_string(),
            total_unsynced: 0,
            synced_count: 0,
            failed_count: 0,
            metadata: MetadataSyncOutcome::Skipped,
            agent_capture: AgentCaptureSyncOutcome::Failed {
                error: "network timeout".to_string(),
            },
        };

        let output = cloud_sync_output_from_report(&report);
        assert_eq!(output.metadata.status, "skipped");
        assert!(output.metadata.references.is_none());
        assert_eq!(output.agent_capture.status, "failed");
        assert_eq!(
            output.agent_capture.error.as_deref(),
            Some("network timeout")
        );
        assert!(output.agent_capture.sessions_synced.is_none());
        assert!(output.agent_capture.sessions_failed.is_none());
        assert!(output.agent_capture.checkpoints_synced.is_none());
        assert!(output.agent_capture.checkpoints_failed.is_none());
        let wire = serde_json::to_value(&output).expect("serialize failed cloud sync");
        assert_eq!(wire["agent_capture"]["error"], "network timeout");
    }

    /// Scenario: metadata restore into a freshly initialized repo where local refs
    /// have `remote = NULL`. This is the edge hit by live cloud restore: SQL
    /// `remote = NULL` does not match existing rows, so the restore must use
    /// `IS NULL` and update the existing HEAD/branch rows instead of inserting
    /// duplicates that leave HEAD pointing at the init-time repository state.
    #[test]
    #[serial]
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
    #[serial]
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
    #[serial]
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
        let local = vec!["checkpoint-pruned-locally".to_string()];
        let remote = HashSet::from(["checkpoint-pruned-locally".to_string()]);
        let error = reject_local_prune_conflicts(&local, &remote)
            .expect_err("matching completed remote checkpoint must fail preflight");
        let message = error.to_string();
        assert!(message.contains("checkpoint-pruned-locally"));
        assert!(message.contains("libra cloud sync"));
        assert!(message.contains("already pruned locally"));
    }

    #[test]
    #[serial]
    fn restore_metadata_strict_fails_when_metadata_object_is_missing() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let remote = RemoteStorage::new(Arc::new(InMemory::new()));

            let error = restore_metadata_strict(&db_conn, &remote)
                .await
                .expect_err("strict metadata restore must fail on missing metadata.json");

            let message = error.to_string();
            assert!(
                message.contains("failed to download metadata"),
                "error should explain metadata download failure: {message}",
            );
        });
    }

    #[test]
    #[serial]
    fn restore_metadata_strict_fails_when_metadata_has_no_local_head() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        rt.block_on(async {
            let db_conn = db::get_db_conn_instance().await;
            let remote = RemoteStorage::new(Arc::new(InMemory::new()));
            let refs = vec![reference::Model {
                id: 0,
                name: Some("main".to_string()),
                kind: reference::ConfigKind::Branch,
                commit: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
                remote: None,
                worktree_id: None,
            }];
            let metadata = serde_json::to_vec(&refs).expect("metadata should serialize");
            remote.put_metadata(&metadata).await.unwrap();

            let error = restore_metadata_strict(&db_conn, &remote)
                .await
                .expect_err("strict metadata restore must reject metadata without HEAD");

            let message = error.to_string();
            assert!(
                message.contains("metadata does not contain local HEAD reference"),
                "error should explain missing HEAD: {message}",
            );
        });
    }

    #[tokio::test]
    #[serial]
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

        let report = restore_indexed_objects_from_remote(&[row], &remote, &local)
            .await
            .expect("restore should download a valid remote object");

        assert_eq!(report.downloaded, 1);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.failed, 0);
        assert!(report.warnings.is_empty());
        assert!(local.exist(&hash).await);

        let row = test_object_index_row(hash, data.len() as i64);
        let report = restore_indexed_objects_from_remote(&[row], &remote, &local)
            .await
            .expect("restore should skip an existing local object");

        assert_eq!(report.downloaded, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.failed, 0);
        assert!(report.warnings.is_empty());
    }

    #[tokio::test]
    #[serial]
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

        let report = restore_indexed_objects_from_remote(&[row], &remote, &local)
            .await
            .expect("hash mismatch should be reported, not panic");

        assert_eq!(report.downloaded, 0);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(report.warnings.len(), 1);
        assert!(
            report.warnings[0].contains("hash mismatch"),
            "warning should explain the hash mismatch: {:?}",
            report.warnings
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
    #[serial]
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
        let source_key = format!("source/sha256/{}", "a".repeat(64));
        (
            AgentSubagentContentClaimRow {
                parent_session_id: "sess-A".to_string(),
                provider_kind: "claude_code".to_string(),
                source_key: source_key.clone(),
                content_schema_version: 1,
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
                content_schema_version: 1,
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
            let checkpoints = vec![fixture_checkpoint_row("ckpt-A", "sess-A", None)];
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
                    ["ckpt-A".into(), "sess-A".into(), 1_i64.into()],
                ))
                .await
                .expect("record ordinary local prune fence");
            db_conn
                .execute_raw(sea_orm::Statement::from_string(
                    backend,
                    "DELETE FROM agent_checkpoint WHERE checkpoint_id = 'ckpt-A'".to_string(),
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
            assert!(message.contains("ckpt-A"));
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
    #[serial]
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
            pruned_claim.source_key = format!("source/sha256/{}", "b".repeat(64));
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
    #[serial]
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
    fn checkpoint_projection_rejects_deferred_erasure_but_applies_ordinary_prune() {
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
        .expect_err("an unmarked remote-only checkpoint is deferred session erasure");
        assert!(
            error
                .to_string()
                .contains("erasure propagation is deferred")
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
            },
            ObjectIndexRow {
                o_id: "tree".to_string(),
                o_type: "tree".to_string(),
                o_size: 1,
                repo_id: "repo".to_string(),
                created_at: 1,
                is_synced: 1,
            },
        ];
        let mut retained = remote_only;
        retained.traces_commit = "traces".to_string();
        retained.tree_oid = "tree".to_string();
        retained.metadata_blob_oid = "metadata".to_string();
        let error = validate_checkpoint_object_index_roots(&[retained], &indexes, "remote")
            .expect_err("a full manifest must include every retained checkpoint root");
        assert!(error.to_string().contains("metadata blob object metadata"));
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
        };
        let second = ObjectIndexRow {
            o_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            o_type: "tree".to_string(),
            o_size: 1,
            repo_id: "repo".to_string(),
            created_at: 100,
            is_synced: 1,
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
    #[serial]
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
            tokio::time::timeout(std::time::Duration::from_millis(150), writer)
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
    #[serial]
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
        });
    }

    /// Codex round-2 follow-up Q4: when the local `agent_checkpoint`
    /// table is missing (partial schema), `restore_agent_capture_from_d1`
    /// must take the warning-and-bail path rather than proceed to insert
    /// rows into a half-built catalogue. This test simulates that
    /// scenario by dropping the checkpoint table after init.
    #[test]
    #[serial]
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
    #[serial]
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
