//! `redb 5.0` implementation of [`CaptureStore`] and [`AgentBridgeStore`] (`RB-06`).
//!
//! Stores all **25** canonical Capture catalog source tables, segregated
//! internal metadata (`meta_schema`, `meta_generation`,
//! `queue_highwater_priority`, `queue_highwater_normal`, `capture_dedup_keys`,
//! `capture_sequences`), and same-transaction secondary indexes
//! (`sessions_by_time`, `checkpoints_by_session`, `events_by_time`,
//! `bridge_events_by_seq`) inside `.libra/private/capture.gen-<id>.redb` using
//! versioned `[schema_ver: u16_le][postcard]` envelopes (`crate::storage::codec`).
//!
//! Every read and write transaction validates `META_SCHEMA` (`DatabaseRole::Capture`,
//! `min_reader_version <= MAX_SUPPORTED_READER_VERSION` mapping future schemas to
//! `LBR-AGENT-048`, and the `u32::MAX` handle migration tombstone fence) as step 1
//! (`ADR-RB-03` #3, `ADR-RB-04` #4).

use std::{
    collections::BTreeSet,
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
#[cfg(unix)]
use std::{
    ffi::{CString, OsStr},
    os::unix::{
        ffi::OsStrExt,
        io::{AsRawFd, FromRawFd, OwnedFd},
    },
};

use async_trait::async_trait;
use redb::{
    Database, ReadOnlyDatabase, ReadTransaction, ReadableTable, TableDefinition, TableHandle,
    WriteTransaction,
};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use crate::storage::{
    capture_store::{CAPTURE_DB_FILE_MODE, CAPTURE_PRIVATE_DIR_MODE},
    opener::CACHE_SIZE_READ_ONLY,
    redb::{map_capture_owner_mismatch_err, map_capture_symlink_err, map_redb_database_err},
};
#[cfg(not(unix))]
use crate::storage::{
    manifest::fsync_dir,
    opener::{create_redb_writable, open_redb_read_only, open_redb_writable},
};
use crate::{
    internal::db::schema::{CAPTURE_SCHEMA_NAME, CAPTURE_SCHEMA_VERSION, DatabaseRole},
    storage::{
        backend::StorageBackendKind,
        capture_store::{
            AgentAuditLogRecord, AgentBridgeCheckpointRecord, AgentBridgeEventRecord,
            AgentBridgeLinkRecord, AgentBridgeOperationRecord, AgentBridgeSessionRecord,
            AgentBridgeStore, AgentCaptureCloudBaseRecord, AgentCaptureIncarnationRecord,
            AgentCheckpointPruneTombstoneRecord, AgentCheckpointRecord, AgentCoverageClaimRecord,
            AgentCoverageConflictRecord, AgentCoverageRevisionRecord, AgentExportJobRecord,
            AgentImportIdentityRecord, AgentImportTombstoneRecord, AgentSessionCommitRecord,
            AgentSessionRecord, AgentSubagentContentClaimRecord,
            AgentSubagentContentRevisionRecord, AgentSubagentLinkRecord, AgentUsageStatsRecord,
            AgentWorkspaceScopeAuditRecord, BRIDGE_SOURCE_DEEPSEEK_HARNESS, BridgeEventAppendInput,
            BridgeEventAppendOutcome, BridgeEventStatus, BridgeLinkWrite, BridgeSessionOpenInput,
            BridgeSessionState, CAPTURE_SOURCE_TABLES, CaptureBatchItem, CaptureBatchOutcome,
            CaptureBatchPayload, CaptureCatalogCopyReceiptRecord, CaptureHashKind,
            CaptureQueueHighwater, CaptureQueueLane, CaptureSchemaVersionRecord, CaptureStore,
            CaptureSubagentLinkState, MAX_BRIDGE_EVENT_PAYLOAD_BYTES, capture_sha256_hex,
            invert_capture_timestamp, validate_agent_checkpoint_record,
            validate_agent_session_record, validate_audit_log_record,
            validate_bridge_checkpoint_record, validate_bridge_link_write,
            validate_bridge_operation_record, validate_bridge_session_open,
            validate_capture_batch_item, validate_checkpoint_prune_tombstone,
            validate_cloud_base_record, validate_copy_receipt_record,
            validate_coverage_claim_record, validate_coverage_conflict_record,
            validate_coverage_revision_record, validate_export_job_record,
            validate_import_identity_record, validate_import_tombstone_record,
            validate_incarnation_record, validate_non_empty_no_nul, validate_schema_version_record,
            validate_scope_immutability, validate_session_commit_record,
            validate_subagent_content_claim, validate_subagent_content_revision,
            validate_subagent_link_record, validate_usage_stats_record,
            validate_workspace_scope_audit_record,
        },
        codec::{decode_versioned, encode_versioned},
        error::{StorageBackendError, StorageResult},
        meta::{begin_checked_read, begin_checked_write_blocking, init_meta_schema},
        opener::{RedbRepairContext, begin_write_bounded_blocking},
        redb::{
            map_append_only_err, map_redb_commit_err, map_redb_storage_err, map_redb_table_err,
            map_tombstone_protected_err,
        },
    },
};

/// Current postcard codec version for `RedbCaptureStore` values.
pub const CAPTURE_CODEC_VERSION: u16 = 1;

/// Default bounded write-lock timeout for [`RedbCaptureStore`] (`30s`).
pub const DEFAULT_CAPTURE_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

const SINGLETON_KEY: &str = "singleton";
const SEQ_KEY_BRIDGE_EVENT: &str = "agent_bridge_event";
const SEQ_KEY_BRIDGE_LINK: &str = "agent_bridge_link";

static CAPTURE_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

// ============================================================================
// 25 Capture Source Table Definitions
// ============================================================================

pub const SCHEMA_VERSIONS_TABLE: TableDefinition<'static, i64, &[u8]> =
    TableDefinition::new("schema_versions");
pub const CAPTURE_CATALOG_COPY_RECEIPT_TABLE: TableDefinition<'static, i64, &[u8]> =
    TableDefinition::new("capture_catalog_copy_receipt");
pub const AGENT_AUDIT_LOG_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_audit_log");
pub const AGENT_BRIDGE_CHECKPOINT_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_bridge_checkpoint");
pub const AGENT_BRIDGE_EVENT_TABLE: TableDefinition<'static, i64, &[u8]> =
    TableDefinition::new("agent_bridge_event");
pub const AGENT_BRIDGE_LINK_TABLE: TableDefinition<'static, i64, &[u8]> =
    TableDefinition::new("agent_bridge_link");
pub const AGENT_BRIDGE_OPERATION_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_bridge_operation");
pub const AGENT_BRIDGE_SESSION_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_bridge_session");
pub const AGENT_CAPTURE_CLOUD_BASE_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_capture_cloud_base");
pub const AGENT_CAPTURE_INCARNATION_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("agent_capture_incarnation");
pub const AGENT_CHECKPOINT_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_checkpoint");
pub const AGENT_CHECKPOINT_PRUNE_TOMBSTONE_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_checkpoint_prune_tombstone");
pub const AGENT_COVERAGE_CLAIM_TABLE: TableDefinition<'static, (&str, &str, i64), &[u8]> =
    TableDefinition::new("agent_coverage_claim");
pub const AGENT_COVERAGE_CONFLICT_TABLE: TableDefinition<'static, (&str, &str, i64), &[u8]> =
    TableDefinition::new("agent_coverage_conflict");
pub const AGENT_COVERAGE_REVISION_TABLE: TableDefinition<'static, (&str, &str, i64, i64), &[u8]> =
    TableDefinition::new("agent_coverage_revision");
pub const AGENT_EXPORT_JOB_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_export_job");
pub const AGENT_IMPORT_IDENTITY_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_import_identity");
pub const AGENT_IMPORT_TOMBSTONE_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_import_tombstone");
pub const AGENT_SESSION_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_session");
pub const AGENT_SESSION_COMMIT_TABLE: TableDefinition<'static, (&str, &str, &str), &[u8]> =
    TableDefinition::new("agent_session_commit");
pub const AGENT_SUBAGENT_CONTENT_CLAIM_TABLE: TableDefinition<
    'static,
    (&str, &str, &str, i64),
    &[u8],
> = TableDefinition::new("agent_subagent_content_claim");
pub const AGENT_SUBAGENT_CONTENT_REVISION_TABLE: TableDefinition<
    'static,
    (&str, &str, &str, i64, i64),
    &[u8],
> = TableDefinition::new("agent_subagent_content_revision");
pub const AGENT_SUBAGENT_LINK_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_subagent_link");
pub const AGENT_USAGE_STATS_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_usage_stats");
pub const AGENT_WORKSPACE_SCOPE_AUDIT_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_workspace_scope_audit");

// ============================================================================
// Segregated Internal Metadata / Queue Highwater / Dedup / Sequence Tables
// ============================================================================

/// Cross-store monotonic generation ledger (`ADR-RB-06`, wired in `RB-06b`).
pub const META_GENERATION_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("meta_generation");
/// Authoritative priority-lane queue highwater (`ADR-RB-05` #4).
pub const QUEUE_HIGHWATER_PRIORITY_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("queue_highwater_priority");
/// Authoritative normal-lane queue highwater (`ADR-RB-05` #4).
pub const QUEUE_HIGHWATER_NORMAL_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("queue_highwater_normal");
/// Full UTF-8 semantic `dedup_key` ledger for `append_capture_batch`.
pub const CAPTURE_DEDUP_KEYS_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("capture_dedup_keys");
/// Monotonic sequence counters for autoincrement primary keys.
pub const CAPTURE_SEQUENCES_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("capture_sequences");

// ============================================================================
// Segregated Same-Transaction Secondary Index Tables
// ============================================================================

/// `SESSIONS_BY_TIME`: `(invert_capture_timestamp(started_at), session_id) -> marker`.
pub const SESSIONS_BY_TIME_TABLE: TableDefinition<'static, (u64, &str), &[u8]> =
    TableDefinition::new("sessions_by_time");
/// `CHECKPOINTS_BY_SESSION`: `(session_id, created_at, checkpoint_id) -> marker`.
pub const CHECKPOINTS_BY_SESSION_TABLE: TableDefinition<'static, (&str, i64, &str), &[u8]> =
    TableDefinition::new("checkpoints_by_session");
/// `EVENTS_BY_TIME`: `(created_at, bridge_session_id, event_seq) -> event_id`.
pub const EVENTS_BY_TIME_TABLE: TableDefinition<'static, (i64, &str, i64), &[u8]> =
    TableDefinition::new("events_by_time");
/// `BRIDGE_EVENTS_BY_SEQ`: `(bridge_session_id, event_seq) -> BridgeEventSeqIndexEntry`.
pub const BRIDGE_EVENTS_BY_SEQ_TABLE: TableDefinition<'static, (&str, i64), &[u8]> =
    TableDefinition::new("bridge_events_by_seq");
/// `USAGE_STATS_BY_SESSION_EVENT`: `(session_id, event_id) -> UsageStatsIndexEntry`
/// enforcing `UNIQUE(session_id, event_id)` in O(log N) per batch item.
pub const USAGE_STATS_BY_SESSION_EVENT_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("usage_stats_by_session_event");
/// `SESSIONS_BY_PROVIDER`: `(agent_kind, provider_session_id) -> SessionProviderIndexEntry`
/// enforcing `UNIQUE(agent_kind, provider_session_id)` in O(log N) per session upsert.
pub const SESSIONS_BY_PROVIDER_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("sessions_by_provider");
/// `BRIDGE_LINKS_BY_KEY`: `(source_type, source_id, target_type, target_id) -> BridgeLinkIndexEntry`
/// enforcing `UNIQUE(source_type, source_id, target_type, target_id)` in O(log N).
pub const BRIDGE_LINKS_BY_KEY_TABLE: TableDefinition<'static, (&str, &str, &str, &str), &[u8]> =
    TableDefinition::new("bridge_links_by_key");
/// `IMPORT_TOMBSTONES_BY_PROVIDER`: `(agent_kind, provider_session_id) -> TombstoneIndexEntry`
/// enforcing same-transaction re-import rejection and unique constraints in O(log N) (`RB-06c`).
pub const IMPORT_TOMBSTONES_BY_PROVIDER_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("import_tombstones_by_provider");
/// `IMPORT_TOMBSTONES_BY_SESSION`: `erased_session_id -> TombstoneIndexEntry`
/// enforcing same-transaction checkpoint/session re-import rejection in O(log N) (`RB-06c`).
pub const IMPORT_TOMBSTONES_BY_SESSION_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("import_tombstones_by_session");

// ============================================================================
// Internal Stored Envelope Structs
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MarkerValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct MetaGenerationValue {
    cross_store_gen: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct CaptureSequenceValue {
    last_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BridgeEventSeqIndexEntry {
    event_id: i64,
    payload_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct EventTimeIndexEntry {
    event_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct UsageStatsIndexEntry {
    usage_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SessionProviderIndexEntry {
    session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct BridgeLinkIndexEntry {
    link_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TombstoneIndexEntry {
    tombstone_id: String,
}

enum RedbCaptureHandle {
    ReadWrite(Arc<Database>),
    ReadOnly(Arc<ReadOnlyDatabase>),
}

impl fmt::Debug for RedbCaptureHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadWrite(_) => f.write_str("RedbCaptureHandle::ReadWrite(..)"),
            Self::ReadOnly(_) => f.write_str("RedbCaptureHandle::ReadOnly(..)"),
        }
    }
}

/// `redb 5.0`-backed implementation of [`CaptureStore`] and [`AgentBridgeStore`].
#[derive(Debug, Clone)]
pub struct RedbCaptureStore {
    handle: Arc<RedbCaptureHandle>,
    path: PathBuf,
    write_timeout: Duration,
    write_tx_count: Arc<AtomicU64>,
}

fn enc<T: Serialize>(value: &T) -> StorageResult<Vec<u8>> {
    encode_versioned(CAPTURE_CODEC_VERSION, value)
}

fn dec<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> StorageResult<T> {
    decode_versioned(bytes, CAPTURE_CODEC_VERSION)
}

fn open_all_capture_tables_for_init(wtx: &WriteTransaction) -> StorageResult<()> {
    let _ = wtx
        .open_table(SCHEMA_VERSIONS_TABLE)
        .map_err(|e| map_redb_table_err(e, "init schema_versions"))?;
    let _ = wtx
        .open_table(CAPTURE_CATALOG_COPY_RECEIPT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init capture_catalog_copy_receipt"))?;
    let _ = wtx
        .open_table(AGENT_AUDIT_LOG_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_audit_log"))?;
    let _ = wtx
        .open_table(AGENT_BRIDGE_CHECKPOINT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_bridge_checkpoint"))?;
    let _ = wtx
        .open_table(AGENT_BRIDGE_EVENT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_bridge_event"))?;
    let _ = wtx
        .open_table(AGENT_BRIDGE_LINK_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_bridge_link"))?;
    let _ = wtx
        .open_table(AGENT_BRIDGE_OPERATION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_bridge_operation"))?;
    let _ = wtx
        .open_table(AGENT_BRIDGE_SESSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_bridge_session"))?;
    let _ = wtx
        .open_table(AGENT_CAPTURE_CLOUD_BASE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_capture_cloud_base"))?;
    let _ = wtx
        .open_table(AGENT_CAPTURE_INCARNATION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_capture_incarnation"))?;
    let _ = wtx
        .open_table(AGENT_CHECKPOINT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_checkpoint"))?;
    let _ = wtx
        .open_table(AGENT_CHECKPOINT_PRUNE_TOMBSTONE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_checkpoint_prune_tombstone"))?;
    let _ = wtx
        .open_table(AGENT_COVERAGE_CLAIM_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_coverage_claim"))?;
    let _ = wtx
        .open_table(AGENT_COVERAGE_CONFLICT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_coverage_conflict"))?;
    let _ = wtx
        .open_table(AGENT_COVERAGE_REVISION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_coverage_revision"))?;
    let _ = wtx
        .open_table(AGENT_EXPORT_JOB_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_export_job"))?;
    let _ = wtx
        .open_table(AGENT_IMPORT_IDENTITY_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_import_identity"))?;
    let _ = wtx
        .open_table(AGENT_IMPORT_TOMBSTONE_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_import_tombstone"))?;
    let _ = wtx
        .open_table(AGENT_SESSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_session"))?;
    let _ = wtx
        .open_table(AGENT_SESSION_COMMIT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_session_commit"))?;
    let _ = wtx
        .open_table(AGENT_SUBAGENT_CONTENT_CLAIM_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_subagent_content_claim"))?;
    let _ = wtx
        .open_table(AGENT_SUBAGENT_CONTENT_REVISION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_subagent_content_revision"))?;
    let _ = wtx
        .open_table(AGENT_SUBAGENT_LINK_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_subagent_link"))?;
    let _ = wtx
        .open_table(AGENT_USAGE_STATS_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_usage_stats"))?;
    let _ = wtx
        .open_table(AGENT_WORKSPACE_SCOPE_AUDIT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init agent_workspace_scope_audit"))?;

    // Segregated internal / metadata / queue / dedup / sequence tables
    {
        let mut gen_tbl = wtx
            .open_table(META_GENERATION_TABLE)
            .map_err(|e| map_redb_table_err(e, "init meta_generation"))?;
        if gen_tbl
            .get(SINGLETON_KEY)
            .map_err(|e| map_redb_storage_err(e, "read meta_generation"))?
            .is_none()
        {
            let bytes = enc(&MetaGenerationValue { cross_store_gen: 0 })?;
            gen_tbl
                .insert(SINGLETON_KEY, bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "write meta_generation"))?;
        }
    }
    let _ = wtx
        .open_table(QUEUE_HIGHWATER_PRIORITY_TABLE)
        .map_err(|e| map_redb_table_err(e, "init queue_highwater_priority"))?;
    let _ = wtx
        .open_table(QUEUE_HIGHWATER_NORMAL_TABLE)
        .map_err(|e| map_redb_table_err(e, "init queue_highwater_normal"))?;
    let _ = wtx
        .open_table(CAPTURE_DEDUP_KEYS_TABLE)
        .map_err(|e| map_redb_table_err(e, "init capture_dedup_keys"))?;
    let _ = wtx
        .open_table(CAPTURE_SEQUENCES_TABLE)
        .map_err(|e| map_redb_table_err(e, "init capture_sequences"))?;

    // Segregated secondary index tables
    let _ = wtx
        .open_table(SESSIONS_BY_TIME_TABLE)
        .map_err(|e| map_redb_table_err(e, "init sessions_by_time"))?;
    let _ = wtx
        .open_table(CHECKPOINTS_BY_SESSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init checkpoints_by_session"))?;
    let _ = wtx
        .open_table(EVENTS_BY_TIME_TABLE)
        .map_err(|e| map_redb_table_err(e, "init events_by_time"))?;
    let _ = wtx
        .open_table(BRIDGE_EVENTS_BY_SEQ_TABLE)
        .map_err(|e| map_redb_table_err(e, "init bridge_events_by_seq"))?;
    let _ = wtx
        .open_table(USAGE_STATS_BY_SESSION_EVENT_TABLE)
        .map_err(|e| map_redb_table_err(e, "init usage_stats_by_session_event"))?;
    let _ = wtx
        .open_table(SESSIONS_BY_PROVIDER_TABLE)
        .map_err(|e| map_redb_table_err(e, "init sessions_by_provider"))?;
    let _ = wtx
        .open_table(BRIDGE_LINKS_BY_KEY_TABLE)
        .map_err(|e| map_redb_table_err(e, "init bridge_links_by_key"))?;
    let _ = wtx
        .open_table(IMPORT_TOMBSTONES_BY_PROVIDER_TABLE)
        .map_err(|e| map_redb_table_err(e, "init import_tombstones_by_provider"))?;
    let _ = wtx
        .open_table(IMPORT_TOMBSTONES_BY_SESSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "init import_tombstones_by_session"))?;

    // Seed initial capture schema_versions receipt matching SQLite bootstrap
    {
        let mut sv = wtx
            .open_table(SCHEMA_VERSIONS_TABLE)
            .map_err(|e| map_redb_table_err(e, "open schema_versions"))?;
        if sv
            .get(CAPTURE_SCHEMA_VERSION)
            .map_err(|e| map_redb_storage_err(e, "read initial schema_version"))?
            .is_none()
        {
            let receipt = CaptureSchemaVersionRecord {
                version: CAPTURE_SCHEMA_VERSION,
                name: CAPTURE_SCHEMA_NAME.to_string(),
                applied_at: chrono::Utc::now().to_rfc3339(),
            };
            let bytes = enc(&receipt)?;
            sv.insert(CAPTURE_SCHEMA_VERSION, bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert initial schema_version"))?;
        }
    }
    Ok(())
}

fn verify_required_capture_tables(rtx: &ReadTransaction) -> StorageResult<()> {
    let present: BTreeSet<String> = rtx
        .list_tables()
        .map_err(|e| map_redb_storage_err(e, "list redb capture tables"))?
        .map(|h| h.name().to_string())
        .collect();
    for required in CAPTURE_SOURCE_TABLES {
        if !present.contains(required) {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "capture redb database is missing required source table `{required}`"
                ),
            });
        }
    }
    Ok(())
}

fn next_sequence_id(wtx: &WriteTransaction, key: &str) -> StorageResult<i64> {
    let mut seq = wtx
        .open_table(CAPTURE_SEQUENCES_TABLE)
        .map_err(|e| map_redb_table_err(e, "open capture_sequences"))?;
    let current = seq
        .get(key)
        .map_err(|e| map_redb_storage_err(e, "read capture sequence"))?
        .map(|g| dec::<CaptureSequenceValue>(g.value()))
        .transpose()?
        .map_or(0, |v| v.last_id);
    let next = current
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "capture sequence counter overflow".to_string(),
        })?;
    let bytes = enc(&CaptureSequenceValue { last_id: next })?;
    seq.insert(key, bytes.as_slice())
        .map_err(|e| map_redb_storage_err(e, "write capture sequence"))?;
    Ok(next)
}

fn read_highwater_from_rtx(
    rtx: &ReadTransaction,
    lane: CaptureQueueLane,
) -> StorageResult<CaptureQueueHighwater> {
    let def = match lane {
        CaptureQueueLane::Priority => QUEUE_HIGHWATER_PRIORITY_TABLE,
        CaptureQueueLane::Normal => QUEUE_HIGHWATER_NORMAL_TABLE,
    };
    let table = rtx
        .open_table(def)
        .map_err(|e| map_redb_table_err(e, "open queue_highwater table"))?;
    match table
        .get(SINGLETON_KEY)
        .map_err(|e| map_redb_storage_err(e, "read queue_highwater"))?
    {
        Some(g) => dec(g.value()),
        None => Ok(CaptureQueueHighwater::default()),
    }
}

fn read_highwater_from_wtx(
    wtx: &WriteTransaction,
    lane: CaptureQueueLane,
) -> StorageResult<CaptureQueueHighwater> {
    let def = match lane {
        CaptureQueueLane::Priority => QUEUE_HIGHWATER_PRIORITY_TABLE,
        CaptureQueueLane::Normal => QUEUE_HIGHWATER_NORMAL_TABLE,
    };
    let table = wtx
        .open_table(def)
        .map_err(|e| map_redb_table_err(e, "open queue_highwater in write tx"))?;
    match table
        .get(SINGLETON_KEY)
        .map_err(|e| map_redb_storage_err(e, "read queue_highwater in write tx"))?
    {
        Some(g) => dec(g.value()),
        None => Ok(CaptureQueueHighwater::default()),
    }
}

fn write_highwater_in_wtx(
    wtx: &WriteTransaction,
    lane: CaptureQueueLane,
    hw: CaptureQueueHighwater,
) -> StorageResult<()> {
    let def = match lane {
        CaptureQueueLane::Priority => QUEUE_HIGHWATER_PRIORITY_TABLE,
        CaptureQueueLane::Normal => QUEUE_HIGHWATER_NORMAL_TABLE,
    };
    let mut table = wtx
        .open_table(def)
        .map_err(|e| map_redb_table_err(e, "open queue_highwater for write"))?;
    let bytes = enc(&hw)?;
    table
        .insert(SINGLETON_KEY, bytes.as_slice())
        .map_err(|e| map_redb_storage_err(e, "write queue_highwater"))?;
    Ok(())
}

fn ensure_tombstone_indexes_synced(db: &Database) -> StorageResult<()> {
    let needs_backfill = {
        let rtx = begin_checked_read(db, DatabaseRole::Capture)?;
        let present: BTreeSet<String> = rtx
            .list_tables()
            .map_err(|e| map_redb_storage_err(e, "list redb capture tables for tombstone sync"))?
            .map(|h| h.name().to_string())
            .collect();
        !present.contains(IMPORT_TOMBSTONES_BY_PROVIDER_TABLE.name())
            || !present.contains(IMPORT_TOMBSTONES_BY_SESSION_TABLE.name())
    };
    if !needs_backfill {
        return Ok(());
    }

    let wtx =
        begin_checked_write_blocking(db, DatabaseRole::Capture, DEFAULT_CAPTURE_WRITE_TIMEOUT)?;
    let rows: Vec<AgentImportTombstoneRecord> = {
        let tbl = wtx
            .open_table(AGENT_IMPORT_TOMBSTONE_TABLE)
            .map_err(|e| map_redb_table_err(e, "open agent_import_tombstone for backfill"))?;
        let mut out = Vec::new();
        for entry in tbl
            .iter()
            .map_err(|e| map_redb_storage_err(e, "iter agent_import_tombstone for backfill"))?
        {
            let (_, v) =
                entry.map_err(|e| map_redb_storage_err(e, "read tombstone for backfill"))?;
            out.push(dec(v.value())?);
        }
        out
    };
    let mut by_provider = wtx
        .open_table(IMPORT_TOMBSTONES_BY_PROVIDER_TABLE)
        .map_err(|e| map_redb_table_err(e, "open import_tombstones_by_provider for backfill"))?;
    let mut by_session = wtx
        .open_table(IMPORT_TOMBSTONES_BY_SESSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "open import_tombstones_by_session for backfill"))?;
    for row in rows {
        let idx = enc(&TombstoneIndexEntry {
            tombstone_id: row.tombstone_id,
        })?;
        by_provider
            .insert(
                (row.agent_kind.as_str(), row.provider_session_id.as_str()),
                idx.as_slice(),
            )
            .map_err(|e| map_redb_storage_err(e, "backfill import_tombstones_by_provider"))?;
        by_session
            .insert(row.erased_session_id.as_str(), idx.as_slice())
            .map_err(|e| map_redb_storage_err(e, "backfill import_tombstones_by_session"))?;
    }
    drop(by_provider);
    drop(by_session);
    wtx.commit()
        .map_err(|e| map_redb_commit_err(e, "commit tombstone index backfill"))?;
    Ok(())
}

fn ensure_not_tombstoned_by_provider(
    wtx: &WriteTransaction,
    agent_kind: &str,
    provider_session_id: &str,
    entity: &'static str,
) -> StorageResult<()> {
    let by_provider = wtx
        .open_table(IMPORT_TOMBSTONES_BY_PROVIDER_TABLE)
        .map_err(|e| map_redb_table_err(e, "open import_tombstones_by_provider"))?;
    if by_provider
        .get((agent_kind, provider_session_id))
        .map_err(|e| map_redb_storage_err(e, "check import_tombstones_by_provider"))?
        .is_some()
    {
        return Err(map_tombstone_protected_err(entity));
    }
    Ok(())
}

fn ensure_not_tombstoned_by_session(
    wtx: &WriteTransaction,
    session_id: &str,
    entity: &'static str,
) -> StorageResult<()> {
    let by_session = wtx
        .open_table(IMPORT_TOMBSTONES_BY_SESSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "open import_tombstones_by_session"))?;
    if by_session
        .get(session_id)
        .map_err(|e| map_redb_storage_err(e, "check import_tombstones_by_session"))?
        .is_some()
    {
        return Err(map_tombstone_protected_err(entity));
    }
    Ok(())
}

fn upsert_session_in_wtx(wtx: &WriteTransaction, rec: &AgentSessionRecord) -> StorageResult<()> {
    // RB-06c AC1: Enforce same-transaction agent_import_tombstone re-import rejection
    // on both (agent_kind, provider_session_id) and erased_session_id.
    ensure_not_tombstoned_by_provider(
        wtx,
        rec.agent_kind.as_str(),
        rec.provider_session_id.as_str(),
        "agent_session",
    )?;
    ensure_not_tombstoned_by_session(wtx, rec.session_id.as_str(), "agent_session")?;

    let mut sessions = wtx
        .open_table(AGENT_SESSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "open agent_session for write"))?;
    let mut by_time = wtx
        .open_table(SESSIONS_BY_TIME_TABLE)
        .map_err(|e| map_redb_table_err(e, "open sessions_by_time"))?;
    let mut by_provider = wtx
        .open_table(SESSIONS_BY_PROVIDER_TABLE)
        .map_err(|e| map_redb_table_err(e, "open sessions_by_provider"))?;

    // O(log N) lookup of existing row by session_id
    let existing = sessions
        .get(rec.session_id.as_str())
        .map_err(|e| map_redb_storage_err(e, "read existing agent_session"))?
        .map(|g| dec::<AgentSessionRecord>(g.value()))
        .transpose()?;

    // O(log N) UNIQUE(agent_kind, provider_session_id) enforcement
    let new_provider_key = (rec.agent_kind.as_str(), rec.provider_session_id.as_str());
    if let Some(owner_raw) = by_provider
        .get(new_provider_key)
        .map_err(|e| map_redb_storage_err(e, "lookup sessions_by_provider"))?
    {
        let owner: SessionProviderIndexEntry = dec(owner_raw.value())?;
        if owner.session_id != rec.session_id {
            return Err(StorageBackendError::BackendInternal {
                detail:
                    "agent_session unique constraint violated on (agent_kind, provider_session_id)"
                        .to_string(),
            });
        }
    }

    if let Some(old) = &existing {
        validate_scope_immutability(
            old.scope_state,
            old.repo_id.as_deref(),
            old.worktree_id.as_deref(),
            old.workspace_id.as_deref(),
            old.workspace_fence,
            rec.scope_state,
            rec.repo_id.as_deref(),
            rec.worktree_id.as_deref(),
            rec.workspace_id.as_deref(),
            rec.workspace_fence,
            "agent_session",
        )?;
        if old.started_at != rec.started_at {
            by_time
                .remove((
                    invert_capture_timestamp(old.started_at),
                    rec.session_id.as_str(),
                ))
                .map_err(|e| map_redb_storage_err(e, "prune stale sessions_by_time"))?;
        }
        if old.agent_kind != rec.agent_kind || old.provider_session_id != rec.provider_session_id {
            by_provider
                .remove((old.agent_kind.as_str(), old.provider_session_id.as_str()))
                .map_err(|e| map_redb_storage_err(e, "prune stale sessions_by_provider"))?;
        }
    }

    let bytes = enc(rec)?;
    sessions
        .insert(rec.session_id.as_str(), bytes.as_slice())
        .map_err(|e| map_redb_storage_err(e, "insert agent_session"))?;
    let marker = enc(&MarkerValue)?;
    by_time
        .insert(
            (
                invert_capture_timestamp(rec.started_at),
                rec.session_id.as_str(),
            ),
            marker.as_slice(),
        )
        .map_err(|e| map_redb_storage_err(e, "insert sessions_by_time"))?;
    let provider_idx = enc(&SessionProviderIndexEntry {
        session_id: rec.session_id.clone(),
    })?;
    by_provider
        .insert(new_provider_key, provider_idx.as_slice())
        .map_err(|e| map_redb_storage_err(e, "insert sessions_by_provider"))?;
    Ok(())
}

fn upsert_checkpoint_in_wtx(
    wtx: &WriteTransaction,
    rec: &AgentCheckpointRecord,
) -> StorageResult<()> {
    // RB-06c AC1: Enforce same-transaction agent_import_tombstone rejection for
    // checkpoints referencing an erased session.
    ensure_not_tombstoned_by_session(wtx, rec.session_id.as_str(), "agent_checkpoint")?;

    // FK: session_id must exist in agent_session (and its provider identity must not be tombstoned)
    let parent_session = {
        let sessions = wtx
            .open_table(AGENT_SESSION_TABLE)
            .map_err(|e| map_redb_table_err(e, "open agent_session for checkpoint FK"))?;
        let Some(raw) = sessions
            .get(rec.session_id.as_str())
            .map_err(|e| map_redb_storage_err(e, "check checkpoint session FK"))?
        else {
            return Err(StorageBackendError::BackendInternal {
                detail: "agent_checkpoint foreign key violation: session_id not found".to_string(),
            });
        };
        dec::<AgentSessionRecord>(raw.value())?
    };
    ensure_not_tombstoned_by_provider(
        wtx,
        parent_session.agent_kind.as_str(),
        parent_session.provider_session_id.as_str(),
        "agent_checkpoint",
    )?;

    let mut checkpoints = wtx
        .open_table(AGENT_CHECKPOINT_TABLE)
        .map_err(|e| map_redb_table_err(e, "open agent_checkpoint"))?;
    let mut by_session = wtx
        .open_table(CHECKPOINTS_BY_SESSION_TABLE)
        .map_err(|e| map_redb_table_err(e, "open checkpoints_by_session"))?;

    let existing = checkpoints
        .get(rec.checkpoint_id.as_str())
        .map_err(|e| map_redb_storage_err(e, "read existing checkpoint"))?
        .map(|g| dec::<AgentCheckpointRecord>(g.value()))
        .transpose()?;

    if let Some(old) = existing
        && (old.session_id != rec.session_id || old.created_at != rec.created_at)
    {
        by_session
            .remove((
                old.session_id.as_str(),
                old.created_at,
                old.checkpoint_id.as_str(),
            ))
            .map_err(|e| map_redb_storage_err(e, "prune stale checkpoints_by_session"))?;
    }

    let bytes = enc(rec)?;
    checkpoints
        .insert(rec.checkpoint_id.as_str(), bytes.as_slice())
        .map_err(|e| map_redb_storage_err(e, "insert agent_checkpoint"))?;
    let marker = enc(&MarkerValue)?;
    by_session
        .insert(
            (
                rec.session_id.as_str(),
                rec.created_at,
                rec.checkpoint_id.as_str(),
            ),
            marker.as_slice(),
        )
        .map_err(|e| map_redb_storage_err(e, "insert checkpoints_by_session"))?;
    Ok(())
}

fn put_session_commit_in_wtx(
    wtx: &WriteTransaction,
    rec: &AgentSessionCommitRecord,
) -> StorageResult<()> {
    {
        let sessions = wtx
            .open_table(AGENT_SESSION_TABLE)
            .map_err(|e| map_redb_table_err(e, "open agent_session for session_commit FK"))?;
        if sessions
            .get(rec.session_id.as_str())
            .map_err(|e| map_redb_storage_err(e, "check session_commit FK"))?
            .is_none()
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "agent_session_commit foreign key violation: session_id not found"
                    .to_string(),
            });
        }
    }
    let mut commits = wtx
        .open_table(AGENT_SESSION_COMMIT_TABLE)
        .map_err(|e| map_redb_table_err(e, "open agent_session_commit"))?;
    let key = (
        rec.session_id.as_str(),
        rec.hash_kind.as_str(),
        rec.commit_oid.as_str(),
    );
    if commits
        .get(key)
        .map_err(|e| map_redb_storage_err(e, "check session_commit"))?
        .is_none()
    {
        let marker = enc(&MarkerValue)?;
        commits
            .insert(key, marker.as_slice())
            .map_err(|e| map_redb_storage_err(e, "insert session_commit"))?;
    }
    Ok(())
}

fn put_usage_stats_in_wtx(
    wtx: &WriteTransaction,
    rec: &AgentUsageStatsRecord,
) -> StorageResult<()> {
    let mut tbl = wtx
        .open_table(AGENT_USAGE_STATS_TABLE)
        .map_err(|e| map_redb_table_err(e, "open agent_usage_stats"))?;
    let mut by_se = wtx
        .open_table(USAGE_STATS_BY_SESSION_EVENT_TABLE)
        .map_err(|e| map_redb_table_err(e, "open usage_stats_by_session_event"))?;

    let existing_self = tbl
        .get(rec.id.as_str())
        .map_err(|e| map_redb_storage_err(e, "read existing agent_usage_stats"))?
        .map(|g| dec::<AgentUsageStatsRecord>(g.value()))
        .transpose()?;

    // Enforce UNIQUE(session_id, event_id) in O(log N) via secondary index
    // whenever both columns are non-NULL (SQLite NULL-distinctness semantics).
    if let (Some(sid), Some(eid)) = (&rec.session_id, &rec.event_id)
        && let Some(owner_raw) = by_se
            .get((sid.as_str(), eid.as_str()))
            .map_err(|e| map_redb_storage_err(e, "lookup usage_stats_by_session_event"))?
    {
        let owner: UsageStatsIndexEntry = dec(owner_raw.value())?;
        if owner.usage_id != rec.id {
            return Err(StorageBackendError::BackendInternal {
                detail: "agent_usage_stats unique constraint violated on (session_id, event_id)"
                    .to_string(),
            });
        }
    }

    // Prune stale secondary index entry if updating an existing row's (session_id, event_id)
    if let Some(old) = existing_self
        && let (Some(old_sid), Some(old_eid)) = (&old.session_id, &old.event_id)
        && (old.session_id != rec.session_id || old.event_id != rec.event_id)
    {
        by_se
            .remove((old_sid.as_str(), old_eid.as_str()))
            .map_err(|e| map_redb_storage_err(e, "prune stale usage_stats_by_session_event"))?;
    }

    let bytes = enc(rec)?;
    tbl.insert(rec.id.as_str(), bytes.as_slice())
        .map_err(|e| map_redb_storage_err(e, "insert agent_usage_stats"))?;
    if let (Some(sid), Some(eid)) = (&rec.session_id, &rec.event_id) {
        let idx_bytes = enc(&UsageStatsIndexEntry {
            usage_id: rec.id.clone(),
        })?;
        by_se
            .insert((sid.as_str(), eid.as_str()), idx_bytes.as_slice())
            .map_err(|e| map_redb_storage_err(e, "insert usage_stats_by_session_event"))?;
    }
    Ok(())
}

fn append_audit_log_in_wtx(wtx: &WriteTransaction, rec: &AgentAuditLogRecord) -> StorageResult<()> {
    let mut tbl = wtx
        .open_table(AGENT_AUDIT_LOG_TABLE)
        .map_err(|e| map_redb_table_err(e, "open agent_audit_log"))?;
    if tbl
        .get(rec.audit_id.as_str())
        .map_err(|e| map_redb_storage_err(e, "check agent_audit_log"))?
        .is_some()
    {
        return Err(map_append_only_err("agent_audit_log", "overwrite"));
    }
    let bytes = enc(rec)?;
    tbl.insert(rec.audit_id.as_str(), bytes.as_slice())
        .map_err(|e| map_redb_storage_err(e, "insert agent_audit_log"))?;
    Ok(())
}

fn append_workspace_scope_audit_in_wtx(
    wtx: &WriteTransaction,
    rec: &AgentWorkspaceScopeAuditRecord,
) -> StorageResult<()> {
    let mut tbl = wtx
        .open_table(AGENT_WORKSPACE_SCOPE_AUDIT_TABLE)
        .map_err(|e| map_redb_table_err(e, "open agent_workspace_scope_audit"))?;
    if tbl
        .get(rec.audit_id.as_str())
        .map_err(|e| map_redb_storage_err(e, "check agent_workspace_scope_audit"))?
        .is_some()
    {
        return Err(map_append_only_err(
            "agent_workspace_scope_audit",
            "overwrite",
        ));
    }
    let bytes = enc(rec)?;
    tbl.insert(rec.audit_id.as_str(), bytes.as_slice())
        .map_err(|e| map_redb_storage_err(e, "insert agent_workspace_scope_audit"))?;
    Ok(())
}

fn delete_checkpoint_in_wtx(wtx: &WriteTransaction, checkpoint_id: &str) -> StorageResult<bool> {
    let existing = {
        let mut cps = wtx
            .open_table(AGENT_CHECKPOINT_TABLE)
            .map_err(|e| map_redb_table_err(e, "open agent_checkpoint"))?;
        cps.remove(checkpoint_id)
            .map_err(|e| map_redb_storage_err(e, "remove agent_checkpoint"))?
            .map(|g| dec::<AgentCheckpointRecord>(g.value()))
            .transpose()?
    };
    let Some(cp) = existing else {
        return Ok(false);
    };

    {
        let mut by_session = wtx
            .open_table(CHECKPOINTS_BY_SESSION_TABLE)
            .map_err(|e| map_redb_table_err(e, "open checkpoints_by_session"))?;
        by_session
            .remove((
                cp.session_id.as_str(),
                cp.created_at,
                cp.checkpoint_id.as_str(),
            ))
            .map_err(|e| map_redb_storage_err(e, "remove checkpoints_by_session entry"))?;
    }

    // Cascade delete agent_coverage_conflict rows referencing incumbent_checkpoint_id
    {
        let mut tbl = wtx
            .open_table(AGENT_COVERAGE_CONFLICT_TABLE)
            .map_err(|e| map_redb_table_err(e, "open agent_coverage_conflict"))?;
        let mut to_remove = Vec::new();
        for entry in tbl
            .iter()
            .map_err(|e| map_redb_storage_err(e, "scan agent_coverage_conflict"))?
        {
            let (k, v) =
                entry.map_err(|e| map_redb_storage_err(e, "iter agent_coverage_conflict"))?;
            let row: AgentCoverageConflictRecord = dec(v.value())?;
            if row.incumbent_checkpoint_id.as_deref() == Some(checkpoint_id) {
                let (a, b, c) = k.value();
                to_remove.push((a.to_string(), b.to_string(), c));
            }
        }
        for (a, b, c) in to_remove {
            tbl.remove((a.as_str(), b.as_str(), c))
                .map_err(|e| map_redb_storage_err(e, "cascade remove coverage conflict"))?;
        }
    }

    // Cascade delete agent_subagent_content_revision referencing checkpoint_id
    {
        let mut tbl = wtx
            .open_table(AGENT_SUBAGENT_CONTENT_REVISION_TABLE)
            .map_err(|e| map_redb_table_err(e, "open agent_subagent_content_revision"))?;
        let mut to_remove = Vec::new();
        for entry in tbl
            .iter()
            .map_err(|e| map_redb_storage_err(e, "scan subagent content revision"))?
        {
            let (k, v) =
                entry.map_err(|e| map_redb_storage_err(e, "iter subagent content revision"))?;
            let row: AgentSubagentContentRevisionRecord = dec(v.value())?;
            if row.checkpoint_id == checkpoint_id {
                let (a, b, c, d, e) = k.value();
                to_remove.push((a.to_string(), b.to_string(), c.to_string(), d, e));
            }
        }
        for (a, b, c, d, e) in to_remove {
            tbl.remove((a.as_str(), b.as_str(), c.as_str(), d, e))
                .map_err(|err| map_redb_storage_err(err, "cascade remove subagent revision"))?;
        }
    }

    // Cascade delete agent_subagent_link for content_checkpoint_id + mimic
    // trg_agent_subagent_boundary_delete for boundary_checkpoint_id
    {
        let mut tbl = wtx
            .open_table(AGENT_SUBAGENT_LINK_TABLE)
            .map_err(|e| map_redb_table_err(e, "open agent_subagent_link"))?;
        tbl.remove(checkpoint_id)
            .map_err(|e| map_redb_storage_err(e, "cascade remove subagent link"))?;
        let mut to_unresolve = Vec::new();
        for entry in tbl
            .iter()
            .map_err(|e| map_redb_storage_err(e, "scan subagent links"))?
        {
            let (_, v) = entry.map_err(|e| map_redb_storage_err(e, "iter subagent links"))?;
            let mut row: AgentSubagentLinkRecord = dec(v.value())?;
            if row.boundary_checkpoint_id.as_deref() == Some(checkpoint_id) {
                row.link_state = CaptureSubagentLinkState::Unresolved;
                row.boundary_checkpoint_id = None;
                row.sync_revision = row.sync_revision.saturating_add(1);
                // Match SQLite trigger `CAST(strftime('%s', 'now') AS INTEGER) * 1000`
                row.updated_at = chrono::Utc::now().timestamp().saturating_mul(1000);
                to_unresolve.push(row);
            }
        }
        for row in to_unresolve {
            let bytes = enc(&row)?;
            tbl.insert(row.content_checkpoint_id.as_str(), bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "unresolve subagent link boundary"))?;
        }
    }

    Ok(true)
}

/// RAII rollback guard for [`RedbCaptureStore::create`].
///
/// On Unix, performs cleanup strictly relative to the validated parent
/// directory descriptor (`unlinkat` + `fsync`) and verifies the target entry's
/// `(st_dev, st_ino)` matches `expected_inode` before unlinking so rollback can
/// never delete a replacement file planted by a concurrent actor (`RB-06d`).
#[cfg(unix)]
struct RedbCreatePublishGuard {
    dir_fd: OwnedFd,
    tmp_name: CString,
    dest_name: CString,
    expected_inode: (libc::dev_t, libc::ino_t),
    published: bool,
    committed: bool,
}

#[cfg(unix)]
fn unlinkat_if_matching_inode(
    dir_fd: &OwnedFd,
    cname: &CString,
    expected_inode: (libc::dev_t, libc::ino_t),
) -> bool {
    let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `dir_fd` is valid and `cname` is NUL-terminated.
    let rc = unsafe {
        libc::fstatat(
            dir_fd.as_raw_fd(),
            cname.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return false;
    }
    // SAFETY: `fstatat` succeeded.
    let st = unsafe { st.assume_init() };
    if (st.st_dev, st.st_ino) != expected_inode {
        return false;
    }
    // SAFETY: `dir_fd` is valid and `cname` is NUL-terminated.
    unsafe { libc::unlinkat(dir_fd.as_raw_fd(), cname.as_ptr(), 0) == 0 }
}

#[cfg(unix)]
impl Drop for RedbCreatePublishGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let _ = unlinkat_if_matching_inode(&self.dir_fd, &self.tmp_name, self.expected_inode);
        if self.published
            && unlinkat_if_matching_inode(&self.dir_fd, &self.dest_name, self.expected_inode)
        {
            // SAFETY: `dir_fd` is valid.
            unsafe {
                let _ = libc::fsync(self.dir_fd.as_raw_fd());
            }
        }
    }
}

#[cfg(not(unix))]
struct RedbCreatePublishGuard {
    tmp_path: PathBuf,
    dest_path: PathBuf,
    parent: PathBuf,
    published: bool,
    committed: bool,
}

#[cfg(not(unix))]
impl Drop for RedbCreatePublishGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let _ = fs::remove_file(&self.tmp_path);
        if self.published {
            let _ = fs::remove_file(&self.dest_path);
            let _ = fsync_dir(&self.parent);
        }
    }
}

struct CancelFlagGuard {
    cancelled: Arc<AtomicBool>,
    completed: bool,
}

impl Drop for CancelFlagGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

/// Expected effective UIDs checked against `.libra/private/` and
/// `capture.gen-<id>.redb` on Unix (`RB-06d`).
///
/// Production code always constructs [`Self::current_effective`] using
/// `libc::geteuid()`. Tests may override `dir_uid` or `file_uid` independently
/// when running unprivileged without `CAP_CHOWN`.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UnixExpectedOwners {
    pub(crate) dir_uid: u32,
    pub(crate) file_uid: u32,
}

#[cfg(unix)]
impl UnixExpectedOwners {
    pub(crate) fn current_effective() -> Self {
        // SAFETY: `geteuid(2)` is always safe to call and never fails.
        let uid = unsafe { libc::geteuid() };
        Self {
            dir_uid: uid,
            file_uid: uid,
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) type AfterTempCreateHook = Box<dyn FnOnce() -> StorageResult<()> + Send>;

#[cfg(all(test, unix))]
pub(crate) type AfterTmpInitHook = Box<dyn FnOnce(&Path) + Send>;

/// Deterministic fault/race injection hooks used solely by Unix security tests
/// (`RB-06d`). Compiled out in non-test builds.
#[cfg(all(test, unix))]
#[derive(Default)]
pub(crate) struct UnixCreateTestHooks {
    pub(crate) after_temp_create: Option<AfterTempCreateHook>,
    pub(crate) after_tmp_init: Option<AfterTmpInitHook>,
    pub(crate) before_publish: Option<Box<dyn FnOnce() + Send>>,
    pub(crate) after_publish: Option<Box<dyn FnOnce() + Send>>,
}

#[cfg(unix)]
fn osstr_to_cstring(s: &OsStr, context: &'static str) -> StorageResult<CString> {
    CString::new(s.as_bytes()).map_err(|_| StorageBackendError::BackendInternal {
        detail: format!("{context}: path component contains interior NUL byte"),
    })
}

#[cfg(unix)]
fn path_to_cstring(path: &Path, context: &'static str) -> StorageResult<CString> {
    osstr_to_cstring(path.as_os_str(), context)
}

#[cfg(unix)]
fn split_capture_target_path(target: &Path) -> StorageResult<(PathBuf, std::ffi::OsString)> {
    let file_name = target
        .file_name()
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "capture database path has no final file name".to_string(),
        })?
        .to_os_string();
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    Ok((parent, file_name))
}

#[cfg(unix)]
fn fstat_fd(fd: &OwnedFd, context: &'static str) -> StorageResult<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `fd` is a valid open file descriptor and `st` points to writable `libc::stat`.
    let rc = unsafe { libc::fstat(fd.as_raw_fd(), st.as_mut_ptr()) };
    if rc != 0 {
        return Err(StorageBackendError::io(context, io::Error::last_os_error()));
    }
    // SAFETY: `fstat` returned 0, fully initializing `st`.
    Ok(unsafe { st.assume_init() })
}

#[cfg(unix)]
fn unix_perm_bits(mode: libc::mode_t) -> u32 {
    // `mode_t` is `u16` on macOS/BSD and `u32` on Linux.
    #[allow(clippy::useless_conversion)]
    let raw = u32::from(mode);
    raw & 0o7777
}

#[cfg(unix)]
fn fchmod_fd(fd: &OwnedFd, mode: u32, context: &'static str) -> StorageResult<()> {
    // SAFETY: `fd` is a valid open file descriptor.
    let rc = unsafe { libc::fchmod(fd.as_raw_fd(), mode as libc::mode_t) };
    if rc != 0 {
        return Err(StorageBackendError::io(context, io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(unix)]
fn fsync_fd(fd: &OwnedFd, context: &'static str) -> StorageResult<()> {
    // SAFETY: `fd` is a valid open file descriptor.
    let rc = unsafe { libc::fsync(fd.as_raw_fd()) };
    if rc != 0 {
        return Err(StorageBackendError::io(context, io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(unix)]
fn is_symlink_errno(err: &io::Error, path: &Path) -> bool {
    match err.raw_os_error() {
        Some(libc::ELOOP) => true,
        // Some Unix kernels return ENOTDIR when O_DIRECTORY | O_NOFOLLOW hits a symlink.
        Some(libc::ENOTDIR) => {
            fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
        }
        _ => false,
    }
}

#[cfg(unix)]
fn is_symlink_errno_at(err: &io::Error, dir_fd: &OwnedFd, cname: &CString) -> bool {
    match err.raw_os_error() {
        Some(libc::ELOOP) => true,
        Some(libc::ENOTDIR) => {
            let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
            // SAFETY: `dir_fd` is valid and `cname` is NUL-terminated.
            let rc = unsafe {
                libc::fstatat(
                    dir_fd.as_raw_fd(),
                    cname.as_ptr(),
                    st.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc == 0 {
                // SAFETY: `fstatat` succeeded.
                let st = unsafe { st.assume_init() };
                (st.st_mode & libc::S_IFMT) == libc::S_IFLNK
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Open a directory using `O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC`,
/// failing closed on symlinks (AC3), non-directories, or non-owner UID (AC5).
#[cfg(unix)]
fn open_dir_nofollow(dir: &Path, expected_uid: u32) -> StorageResult<(OwnedFd, libc::stat)> {
    let cdir = path_to_cstring(dir, "open capture private directory")?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `cdir` is a valid NUL-terminated C string.
    let raw_fd = unsafe { libc::open(cdir.as_ptr(), flags) };
    if raw_fd < 0 {
        let err = io::Error::last_os_error();
        if is_symlink_errno(&err, dir) {
            return Err(map_capture_symlink_err("capture private directory"));
        }
        return Err(StorageBackendError::io(
            "open capture private directory",
            err,
        ));
    }
    // SAFETY: `raw_fd >= 0` is a newly opened descriptor owned exclusively here.
    let dir_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let st = fstat_fd(&dir_fd, "stat capture private directory")?;
    if (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture private directory path is not a directory".to_string(),
        });
    }
    if st.st_uid != expected_uid {
        return Err(map_capture_owner_mismatch_err("capture private directory"));
    }
    Ok((dir_fd, st))
}

/// Open a child directory relative to `parent_fd` using
/// `O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC`, verifying directory type
/// and expected owner UID.
#[cfg(unix)]
fn openat_dir_nofollow(
    parent_fd: &OwnedFd,
    dir_name: &OsStr,
    expected_uid: u32,
) -> StorageResult<(OwnedFd, libc::stat)> {
    let cname = osstr_to_cstring(dir_name, "open capture private directory")?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `parent_fd` is valid and `cname` is NUL-terminated.
    let raw_fd = unsafe { libc::openat(parent_fd.as_raw_fd(), cname.as_ptr(), flags) };
    if raw_fd < 0 {
        let err = io::Error::last_os_error();
        if is_symlink_errno_at(&err, parent_fd, &cname) {
            return Err(map_capture_symlink_err("capture private directory"));
        }
        return Err(StorageBackendError::io(
            "open capture private directory",
            err,
        ));
    }
    // SAFETY: `raw_fd >= 0` is owned exclusively here.
    let dir_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let st = fstat_fd(&dir_fd, "stat capture private directory")?;
    if (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture private directory path is not a directory".to_string(),
        });
    }
    if st.st_uid != expected_uid {
        return Err(map_capture_owner_mismatch_err("capture private directory"));
    }
    Ok((dir_fd, st))
}

/// Fix permissions on a newly `mkdirat`-created directory to exact `0700`
/// without ever following symlinks, even when a restrictive process `umask`
/// (such as `0777`) stripped owner bits (`0000`) or an ancestor setgid bit
/// added `2000` (`RB-06d` AC1).
#[cfg(unix)]
fn fix_newly_created_dir_mode_nofollow(
    grandparent_fd: &OwnedFd,
    dir_name: &OsStr,
    expected_uid: u32,
) -> StorageResult<()> {
    let cname = osstr_to_cstring(dir_name, "fix capture private directory mode")?;
    let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `grandparent_fd` is valid and `cname` is NUL-terminated.
    let rc = unsafe {
        libc::fstatat(
            grandparent_fd.as_raw_fd(),
            cname.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(StorageBackendError::io(
            "inspect newly created capture private directory",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: `fstatat` succeeded.
    let st = unsafe { st.assume_init() };
    if (st.st_mode & libc::S_IFMT) == libc::S_IFLNK {
        return Err(map_capture_symlink_err("capture private directory"));
    }
    if (st.st_mode & libc::S_IFMT) != libc::S_IFDIR {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture private directory path is not a directory".to_string(),
        });
    }
    if st.st_uid != expected_uid {
        return Err(map_capture_owner_mismatch_err("capture private directory"));
    }
    if unix_perm_bits(st.st_mode) == CAPTURE_PRIVATE_DIR_MODE {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        // Linux supports `O_PATH | O_DIRECTORY | O_NOFOLLOW` even on mode `0000`,
        // pinning the exact inode before updating permissions via `/proc/self/fd`.
        let path_flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `grandparent_fd` is valid and `cname` is NUL-terminated.
        let raw_fd =
            unsafe { libc::openat(grandparent_fd.as_raw_fd(), cname.as_ptr(), path_flags) };
        if raw_fd < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, grandparent_fd, &cname) {
                return Err(map_capture_symlink_err("capture private directory"));
            }
            return Err(StorageBackendError::io(
                "open newly created capture private directory O_PATH",
                err,
            ));
        }
        // SAFETY: `raw_fd >= 0` is owned exclusively here.
        let path_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let pst = fstat_fd(&path_fd, "stat newly created capture private directory")?;
        if (pst.st_mode & libc::S_IFMT) != libc::S_IFDIR {
            return Err(StorageBackendError::BackendInternal {
                detail: "capture private directory path is not a directory".to_string(),
            });
        }
        if pst.st_uid != expected_uid {
            return Err(map_capture_owner_mismatch_err("capture private directory"));
        }
        let proc_path =
            CString::new(format!("/proc/self/fd/{}", path_fd.as_raw_fd())).map_err(|_| {
                StorageBackendError::BackendInternal {
                    detail: "capture private directory proc fd path invalid".to_string(),
                }
            })?;
        // SAFETY: `proc_path` refers directly to our pinned `path_fd`.
        let chmod_rc =
            unsafe { libc::chmod(proc_path.as_ptr(), CAPTURE_PRIVATE_DIR_MODE as libc::mode_t) };
        if chmod_rc != 0 {
            return Err(StorageBackendError::io(
                "enforce 0700 mode on newly created capture private directory",
                io::Error::last_os_error(),
            ));
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS / BSD support `AT_SYMLINK_NOFOLLOW` natively on `fchmodat(2)`.
        // SAFETY: `grandparent_fd` is valid and `cname` is NUL-terminated.
        let chmod_rc = unsafe {
            libc::fchmodat(
                grandparent_fd.as_raw_fd(),
                cname.as_ptr(),
                CAPTURE_PRIVATE_DIR_MODE as libc::mode_t,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if chmod_rc != 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, grandparent_fd, &cname) {
                return Err(map_capture_symlink_err("capture private directory"));
            }
            return Err(StorageBackendError::io(
                "enforce 0700 mode on newly created capture private directory",
                err,
            ));
        }
    }
    Ok(())
}

/// Ensure all missing ancestor directories up to `grandparent` exist and remain
/// accessible to the owner even under owner-bit-stripping restrictive `umask`
/// (such as `0777`), returning an open directory descriptor to `grandparent`
/// (`RB-06d` AC1).
#[cfg(unix)]
fn open_or_create_ancestor_dir(grandparent: &Path, expected_uid: u32) -> StorageResult<OwnedFd> {
    let mut missing_components: Vec<std::ffi::OsString> = Vec::new();
    let mut cursor = grandparent.to_path_buf();

    let mut cur_fd = loop {
        let open_target = if cursor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            cursor.as_path()
        };
        let cpath = path_to_cstring(open_target, "open capture parent ancestor directory")?;
        // SAFETY: `cpath` is a valid NUL-terminated path.
        let raw_fd = unsafe {
            libc::open(
                cpath.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if raw_fd >= 0 {
            // SAFETY: `raw_fd >= 0` is owned exclusively here.
            break unsafe { OwnedFd::from_raw_fd(raw_fd) };
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::NotFound {
            return Err(StorageBackendError::io(
                "open capture parent ancestor directory",
                err,
            ));
        }
        let Some(name) = cursor.file_name() else {
            return Err(StorageBackendError::io(
                "open capture parent ancestor directory",
                err,
            ));
        };
        missing_components.push(name.to_os_string());
        match cursor.parent() {
            Some(parent) => cursor = parent.to_path_buf(),
            None => {
                return Err(StorageBackendError::io(
                    "open capture parent ancestor directory",
                    err,
                ));
            }
        }
    };

    for comp in missing_components.into_iter().rev() {
        let cname = osstr_to_cstring(&comp, "create capture parent ancestor directory")?;
        // SAFETY: `cur_fd` is valid and `cname` is NUL-terminated.
        let mkdir_rc = unsafe {
            libc::mkdirat(
                cur_fd.as_raw_fd(),
                cname.as_ptr(),
                CAPTURE_PRIVATE_DIR_MODE as libc::mode_t,
            )
        };
        let created_now = if mkdir_rc == 0 {
            true
        } else {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::AlreadyExists {
                false
            } else {
                return Err(StorageBackendError::io(
                    "create capture parent ancestor directory",
                    err,
                ));
            }
        };
        if created_now {
            fix_newly_created_dir_mode_nofollow(&cur_fd, &comp, expected_uid)?;
        }
        let next_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `cur_fd` is valid and `cname` is NUL-terminated.
        let next_raw = unsafe { libc::openat(cur_fd.as_raw_fd(), cname.as_ptr(), next_flags) };
        if next_raw < 0 {
            let err = io::Error::last_os_error();
            if is_symlink_errno_at(&err, &cur_fd, &cname) {
                return Err(map_capture_symlink_err("capture private directory"));
            }
            return Err(StorageBackendError::io(
                "open capture parent ancestor directory",
                err,
            ));
        }
        // SAFETY: `next_raw >= 0` is owned exclusively here.
        let next_fd = unsafe { OwnedFd::from_raw_fd(next_raw) };
        if created_now {
            fsync_fd(&cur_fd, "fsync capture parent ancestor directory")?;
        }
        cur_fd = next_fd;
    }

    Ok(cur_fd)
}

/// Open or create the parent directory for `RedbCaptureStore::create`.
///
/// Newly created directories are enforced to exact `0700` (resilient under
/// `umask(0777)` and inherited setgid). Already-existing directories are
/// validated (`O_NOFOLLOW`, `S_ISDIR`, effective-UID owner) and never mutated
/// (`RB-06d` AC1, AC3, AC5).
#[cfg(unix)]
fn open_or_create_private_dir_nofollow(
    parent: &Path,
    expected_uid: u32,
) -> StorageResult<(OwnedFd, libc::stat)> {
    match open_dir_nofollow(parent, expected_uid) {
        Ok((dir_fd, st)) => Ok((dir_fd, st)),
        Err(StorageBackendError::Io {
            kind: io::ErrorKind::NotFound,
            ..
        }) => {
            let dir_name =
                parent
                    .file_name()
                    .ok_or_else(|| StorageBackendError::BackendInternal {
                        detail: "capture private directory path has no final component".to_string(),
                    })?;
            let grandparent = parent
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let grandparent_fd = open_or_create_ancestor_dir(grandparent, expected_uid)?;
            let cname = osstr_to_cstring(dir_name, "create capture private directory")?;

            // SAFETY: `grandparent_fd` is valid and `cname` is NUL-terminated.
            let mkdir_rc = unsafe {
                libc::mkdirat(
                    grandparent_fd.as_raw_fd(),
                    cname.as_ptr(),
                    CAPTURE_PRIVATE_DIR_MODE as libc::mode_t,
                )
            };
            let created_now = if mkdir_rc == 0 {
                true
            } else {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::AlreadyExists {
                    false
                } else {
                    return Err(StorageBackendError::io(
                        "create capture private directory",
                        err,
                    ));
                }
            };

            if created_now {
                fix_newly_created_dir_mode_nofollow(&grandparent_fd, dir_name, expected_uid)?;
            }

            let (dir_fd, st) = openat_dir_nofollow(&grandparent_fd, dir_name, expected_uid)?;
            if created_now {
                fchmod_fd(
                    &dir_fd,
                    CAPTURE_PRIVATE_DIR_MODE,
                    "enforce 0700 mode on capture private directory",
                )?;
                let verified = fstat_fd(&dir_fd, "verify capture private directory mode")?;
                if unix_perm_bits(verified.st_mode) != CAPTURE_PRIVATE_DIR_MODE {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "capture private directory permissions failed to settle at 0700"
                            .to_string(),
                    });
                }
                fsync_fd(&grandparent_fd, "fsync capture parent ancestor directory")?;
                Ok((dir_fd, verified))
            } else {
                Ok((dir_fd, st))
            }
        }
        Err(err) => Err(err),
    }
}

/// Fail closed if `file_name` already exists inside `dir_fd`, distinguishing
/// symlinks (AC4) from regular `AlreadyExists`.
#[cfg(unix)]
fn ensure_target_absent_nofollow(dir_fd: &OwnedFd, file_name: &OsStr) -> StorageResult<()> {
    let cname = osstr_to_cstring(file_name, "inspect capture database file")?;
    let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `dir_fd` is valid, `cname` is NUL-terminated, `st` is writable.
    let rc = unsafe {
        libc::fstatat(
            dir_fd.as_raw_fd(),
            cname.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        // SAFETY: `fstatat` returned 0, fully initializing `st`.
        let st = unsafe { st.assume_init() };
        if (st.st_mode & libc::S_IFMT) == libc::S_IFLNK {
            return Err(map_capture_symlink_err("capture database file"));
        }
        return Err(StorageBackendError::Io {
            context: "create capture redb database",
            kind: io::ErrorKind::AlreadyExists,
            raw_os_error: None,
        });
    }
    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(StorageBackendError::io(
            "inspect capture database file",
            err,
        ))
    }
}

/// Atomically create a private `0600` regular temp file inside `dir_fd` via
/// `openat(..., O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)`.
///
/// Immediately captures `(st_dev, st_ino)` and returns the open descriptor
/// **paired with an inode-pinned [`RedbCreatePublishGuard`]** so even failures
/// in `fchmod` or subsequent initialization unlink only the exact created inode
/// relative to `dir_fd` with zero orphan leftovers (`RB-06d` AC2).
#[cfg(unix)]
fn create_private_temp_file_with_guard(
    dir_fd: &OwnedFd,
    tmp_name: &OsStr,
    dest_name: &OsStr,
    expected_uid: u32,
    #[cfg(test)] after_temp_create: Option<Box<dyn FnOnce() -> StorageResult<()> + Send>>,
) -> StorageResult<(fs::File, (libc::dev_t, libc::ino_t), RedbCreatePublishGuard)> {
    let ctmp = osstr_to_cstring(tmp_name, "create capture redb temp file")?;
    let cdest = osstr_to_cstring(dest_name, "publish capture redb target name")?;
    let guard_dir_fd = dir_fd
        .try_clone()
        .map_err(|err| StorageBackendError::io("clone capture directory descriptor", err))?;

    let flags = libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `dir_fd` is valid and `ctmp` is NUL-terminated; variadic `mode`
    // is promoted to `libc::c_uint` for portability across Linux and macOS.
    let raw_fd = unsafe {
        libc::openat(
            dir_fd.as_raw_fd(),
            ctmp.as_ptr(),
            flags,
            CAPTURE_DB_FILE_MODE as libc::c_uint,
        )
    };
    if raw_fd < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ELOOP) {
            return Err(map_capture_symlink_err("capture database file"));
        }
        return Err(StorageBackendError::io(
            "create capture redb temp file",
            err,
        ));
    }
    // SAFETY: `raw_fd >= 0` is a newly opened descriptor owned exclusively here.
    let file_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let st = match fstat_fd(&file_fd, "stat capture redb temp file") {
        Ok(st) => st,
        Err(err) => {
            // SAFETY: `guard_dir_fd` is valid and `ctmp` was just created with O_EXCL.
            unsafe {
                let _ = libc::unlinkat(guard_dir_fd.as_raw_fd(), ctmp.as_ptr(), 0);
            }
            return Err(err);
        }
    };
    let created_inode = (st.st_dev, st.st_ino);

    // Arm inode-pinned descriptor-relative rollback guard IMMEDIATELY.
    let guard = RedbCreatePublishGuard {
        dir_fd: guard_dir_fd,
        tmp_name: ctmp,
        dest_name: cdest,
        expected_inode: created_inode,
        published: false,
        committed: false,
    };

    #[cfg(test)]
    if let Some(hook) = after_temp_create {
        hook()?;
    }

    if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture database temp path is not a regular file".to_string(),
        });
    }
    if st.st_uid != expected_uid {
        return Err(map_capture_owner_mismatch_err("capture database file"));
    }
    fchmod_fd(
        &file_fd,
        CAPTURE_DB_FILE_MODE,
        "enforce 0600 mode on capture database temp file",
    )?;
    let verified = fstat_fd(&file_fd, "verify capture redb temp file mode")?;
    if unix_perm_bits(verified.st_mode) != CAPTURE_DB_FILE_MODE {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture database temp file permissions failed to settle at 0600".to_string(),
        });
    }
    Ok((fs::File::from(file_fd), created_inode, guard))
}

/// Re-open a file relative to `dir_fd` with `O_NOFOLLOW | O_NONBLOCK`, verify
/// regular-file type, owner UID, AND exact `(st_dev, st_ino)` identity BEFORE
/// enforcing `0600` mode (`RB-06d`).
#[cfg(unix)]
fn enforce_private_file_mode_in_dir(
    dir_fd: &OwnedFd,
    file_name: &OsStr,
    expected_uid: u32,
    expected_inode: (libc::dev_t, libc::ino_t),
) -> StorageResult<libc::stat> {
    let cname = osstr_to_cstring(file_name, "enforce capture database file mode")?;
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    // SAFETY: `dir_fd` is valid and `cname` is NUL-terminated.
    let raw_fd = unsafe { libc::openat(dir_fd.as_raw_fd(), cname.as_ptr(), flags) };
    if raw_fd < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ELOOP) {
            return Err(map_capture_symlink_err("capture database file"));
        }
        return Err(StorageBackendError::io(
            "open capture database file to enforce mode",
            err,
        ));
    }
    // SAFETY: `raw_fd >= 0` is owned exclusively here.
    let file_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let st = fstat_fd(&file_fd, "stat capture database file")?;
    if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture database path is not a regular file".to_string(),
        });
    }
    if st.st_uid != expected_uid {
        return Err(map_capture_owner_mismatch_err("capture database file"));
    }
    if (st.st_dev, st.st_ino) != expected_inode {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture database temp inode changed before publish".to_string(),
        });
    }
    fchmod_fd(
        &file_fd,
        CAPTURE_DB_FILE_MODE,
        "enforce 0600 mode on capture database file",
    )?;
    let verified = fstat_fd(&file_fd, "verify capture database file mode")?;
    if unix_perm_bits(verified.st_mode) != CAPTURE_DB_FILE_MODE {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture database file permissions failed to settle at 0600".to_string(),
        });
    }
    Ok(verified)
}

/// Atomically publish `tmp_name` to `target_name` inside the validated `dir_fd`
/// using `linkat` (non-overwriting), distinguishing symlink races (`RB-06d`).
#[cfg(unix)]
fn linkat_nofollow_in_dir(
    dir_fd: &OwnedFd,
    tmp_name: &OsStr,
    target_name: &OsStr,
) -> StorageResult<()> {
    let ctmp = osstr_to_cstring(tmp_name, "publish capture redb temp name")?;
    let ctarget = osstr_to_cstring(target_name, "publish capture redb target name")?;
    // SAFETY: `dir_fd` is valid and both names are NUL-terminated.
    let rc = unsafe {
        libc::linkat(
            dir_fd.as_raw_fd(),
            ctmp.as_ptr(),
            dir_fd.as_raw_fd(),
            ctarget.as_ptr(),
            0,
        )
    };
    if rc != 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EEXIST) {
            ensure_target_absent_nofollow(dir_fd, target_name)?;
        }
        return Err(StorageBackendError::io(
            "publish initial capture redb database",
            err,
        ));
    }
    Ok(())
}

/// Open an existing `capture.gen-<id>.redb` relative to `dir_fd` using
/// `O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK`, returning the verified [`fs::File`]
/// descriptor directly for [`redb::backends::FileBackend::new`] so there is
/// zero TOCTOU pathname reopen (`RB-06d` AC4, AC6).
#[cfg(unix)]
fn open_existing_file_in_dir(
    dir_fd: &OwnedFd,
    file_name: &OsStr,
    expected_uid: u32,
    writable: bool,
) -> StorageResult<(fs::File, libc::stat)> {
    let cname = osstr_to_cstring(file_name, "open capture database file")?;
    let access = if writable {
        libc::O_RDWR
    } else {
        libc::O_RDONLY
    };
    let flags = access | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    // SAFETY: `dir_fd` is valid and `cname` is NUL-terminated.
    let raw_fd = unsafe { libc::openat(dir_fd.as_raw_fd(), cname.as_ptr(), flags) };
    if raw_fd < 0 {
        let err = io::Error::last_os_error();
        if is_symlink_errno_at(&err, dir_fd, &cname) {
            return Err(map_capture_symlink_err("capture database file"));
        }
        let mut lst = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `dir_fd` is valid and `cname` is NUL-terminated.
        if unsafe {
            libc::fstatat(
                dir_fd.as_raw_fd(),
                cname.as_ptr(),
                lst.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            // SAFETY: `fstatat` succeeded.
            let lst = unsafe { lst.assume_init() };
            let ftype = lst.st_mode & libc::S_IFMT;
            if ftype == libc::S_IFLNK {
                return Err(map_capture_symlink_err("capture database file"));
            }
            if ftype != libc::S_IFREG {
                return Err(StorageBackendError::BackendInternal {
                    detail: "capture database path is not a regular file".to_string(),
                });
            }
            if lst.st_uid != expected_uid {
                return Err(map_capture_owner_mismatch_err("capture database file"));
            }
        }
        return Err(StorageBackendError::io(
            "open capture database file with O_NOFOLLOW",
            err,
        ));
    }
    // SAFETY: `raw_fd >= 0` is owned exclusively here.
    let file_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let st = fstat_fd(&file_fd, "stat capture database file")?;
    if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture database path is not a regular file".to_string(),
        });
    }
    if st.st_uid != expected_uid {
        return Err(map_capture_owner_mismatch_err("capture database file"));
    }
    if writable && (st.st_mode & 0o222) == 0 {
        return Err(StorageBackendError::Io {
            context: "verify capture redb write access before repair",
            kind: io::ErrorKind::PermissionDenied,
            raw_os_error: None,
        });
    }
    Ok((fs::File::from(file_fd), st))
}

/// Open a `redb` read-only handle directly from a descriptor-validated
/// [`fs::File`] (`MultiWriter`, `16 MiB` cache, zero write upgrades, zero
/// pathname reopen).
#[cfg(unix)]
fn open_redb_read_only_from_fd(file: fs::File) -> StorageResult<ReadOnlyDatabase> {
    let mut builder = redb::Builder::new();
    builder
        .set_concurrency_mode(redb::ConcurrencyMode::MultiWriter)
        .set_cache_size(CACHE_SIZE_READ_ONLY);
    builder
        .open_read_only_with_backend(redb::backends::FileBackend::new(file))
        .map_err(|err| {
            map_redb_database_err(err, DatabaseRole::Capture, "open capture redb read-only")
        })
}

/// Open or initialize a `redb` writable handle directly from a
/// descriptor-validated [`fs::File`] (`MultiWriter`, role-bounded cache, zero
/// pathname reopen).
#[cfg(unix)]
fn open_redb_writable_from_fd(
    file: fs::File,
    st: Option<libc::stat>,
    context: RedbRepairContext,
    allow_empty_create: bool,
) -> StorageResult<Database> {
    if !allow_empty_create
        && let Some(st) = st
        && st.st_size == 0
    {
        return Err(StorageBackendError::BackendInternal {
            detail: "open capture redb writable: database file is empty".to_string(),
        });
    }
    let mut builder = redb::Builder::new();
    builder
        .set_concurrency_mode(redb::ConcurrencyMode::MultiWriter)
        .set_cache_size(context.write_profile().cache_size_bytes());
    builder
        .create_with_backend(redb::backends::FileBackend::new(file))
        .map_err(|err| {
            map_redb_database_err(err, DatabaseRole::Capture, "open capture redb writable")
        })
}

/// Execute the blocking phase of [`RedbCaptureStore::create`], returning the
/// opened [`Database`] paired with an UNCOMMITTED descriptor-relative
/// [`RedbCreatePublishGuard`].
///
/// Every file create, redb initialization, `linkat`, `unlinkat`, `fsync`, and
/// post-publish open executes strictly relative to the validated `dir_fd` and
/// file descriptors — zero pathname reopens (`RB-06d`).
#[cfg(unix)]
fn create_redb_capture_blocking_with_owners(
    target: PathBuf,
    cancel_worker: Arc<AtomicBool>,
    owners: UnixExpectedOwners,
    #[cfg(test)] mut hooks: UnixCreateTestHooks,
) -> StorageResult<(Database, RedbCreatePublishGuard)> {
    let (parent, target_name) = split_capture_target_path(&target)?;
    let (dir_fd, _) = open_or_create_private_dir_nofollow(&parent, owners.dir_uid)?;
    ensure_target_absent_nofollow(&dir_fd, &target_name)?;

    let seq = CAPTURE_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name_str = format!(".capture.init.{}.{seq}.tmp", std::process::id());
    let tmp_name = OsStr::new(&tmp_name_str);

    // Atomically create `tmp_name` (`O_CREAT|O_EXCL|O_NOFOLLOW`, `0600`) and
    // arm an inode-pinned descriptor-relative rollback guard immediately.
    let (tmp_file, created_inode, mut guard) = create_private_temp_file_with_guard(
        &dir_fd,
        tmp_name,
        &target_name,
        owners.file_uid,
        #[cfg(test)]
        hooks.after_temp_create.take(),
    )?;

    let tmp_db = open_redb_writable_from_fd(tmp_file, None, RedbRepairContext::WriteCommand, true)?;
    let wtx = begin_write_bounded_blocking(
        &tmp_db,
        DatabaseRole::Capture,
        DEFAULT_CAPTURE_WRITE_TIMEOUT,
    )?;
    init_meta_schema(&wtx, DatabaseRole::Capture)?;
    open_all_capture_tables_for_init(&wtx)?;
    wtx.commit()
        .map_err(|err| map_redb_commit_err(err, "commit initial capture redb"))?;
    drop(tmp_db);

    #[cfg(test)]
    if let Some(hook) = hooks.after_tmp_init.take() {
        hook(&parent.join(tmp_name));
    }

    let _ = enforce_private_file_mode_in_dir(&dir_fd, tmp_name, owners.file_uid, created_inode)?;

    #[cfg(test)]
    if let Some(hook) = hooks.before_publish.take() {
        hook();
    }

    if cancel_worker.load(Ordering::Acquire) {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture redb create cancelled before publish".to_string(),
        });
    }

    linkat_nofollow_in_dir(&dir_fd, tmp_name, &target_name)?;
    guard.published = true;
    // Remove temp link strictly relative to `dir_fd` only if it still points
    // to our initialized inode, then fsync `dir_fd`.
    let _ = unlinkat_if_matching_inode(&dir_fd, &guard.tmp_name, created_inode);
    fsync_fd(&dir_fd, "fsync capture private directory")?;

    #[cfg(test)]
    if let Some(hook) = hooks.after_publish.take() {
        hook();
    }

    let (published_file, published_st) =
        open_existing_file_in_dir(&dir_fd, &target_name, owners.file_uid, true)?;
    if unix_perm_bits(published_st.st_mode) != CAPTURE_DB_FILE_MODE {
        return Err(StorageBackendError::BackendInternal {
            detail: "published capture database file permissions are not 0600".to_string(),
        });
    }
    if (published_st.st_dev, published_st.st_ino) != created_inode {
        return Err(StorageBackendError::BackendInternal {
            detail: "published capture database inode changed unexpectedly during create"
                .to_string(),
        });
    }
    drop(dir_fd);

    let db = open_redb_writable_from_fd(
        published_file,
        Some(published_st),
        RedbRepairContext::WriteCommand,
        false,
    )?;
    if cancel_worker.load(Ordering::Acquire) {
        drop(db);
        return Err(StorageBackendError::BackendInternal {
            detail: "capture redb create cancelled before return".to_string(),
        });
    }
    Ok((db, guard))
}

#[cfg(all(test, unix))]
fn create_redb_capture_blocking(
    target: PathBuf,
    cancel_worker: Arc<AtomicBool>,
) -> StorageResult<(Database, RedbCreatePublishGuard)> {
    create_redb_capture_blocking_with_owners(
        target,
        cancel_worker,
        UnixExpectedOwners::current_effective(),
        UnixCreateTestHooks::default(),
    )
}

#[cfg(not(unix))]
fn create_redb_capture_blocking(
    target: PathBuf,
    cancel_worker: Arc<AtomicBool>,
) -> StorageResult<(Database, RedbCreatePublishGuard)> {
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    fs::create_dir_all(&parent)
        .map_err(|err| StorageBackendError::io("create capture redb parent dir", err))?;

    if fs::symlink_metadata(&target).is_ok() {
        return Err(StorageBackendError::Io {
            context: "create capture redb database",
            kind: io::ErrorKind::AlreadyExists,
            raw_os_error: None,
        });
    }

    let seq = CAPTURE_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = parent.join(format!(".capture.init.{}.{seq}.tmp", std::process::id()));

    // Establish exclusive ownership of `tmp_path` before arming the RAII guard.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
        .map_err(|err| StorageBackendError::io("create capture redb temp file", err))?;

    let mut guard = RedbCreatePublishGuard {
        tmp_path: tmp_path.clone(),
        dest_path: target.clone(),
        parent: parent.clone(),
        published: false,
        committed: false,
    };

    let tmp_db = create_redb_writable(
        &tmp_path,
        DatabaseRole::Capture,
        RedbRepairContext::WriteCommand,
    )?;
    let wtx = begin_write_bounded_blocking(
        &tmp_db,
        DatabaseRole::Capture,
        DEFAULT_CAPTURE_WRITE_TIMEOUT,
    )?;
    init_meta_schema(&wtx, DatabaseRole::Capture)?;
    open_all_capture_tables_for_init(&wtx)?;
    wtx.commit()
        .map_err(|err| map_redb_commit_err(err, "commit initial capture redb"))?;
    drop(tmp_db);

    if cancel_worker.load(Ordering::Acquire) {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture redb create cancelled before publish".to_string(),
        });
    }

    fs::hard_link(&tmp_path, &target)
        .map_err(|err| StorageBackendError::io("publish initial capture redb database", err))?;
    guard.published = true;
    let _ = fs::remove_file(&tmp_path);
    fsync_dir(&parent)?;

    let db = open_redb_writable(
        &target,
        DatabaseRole::Capture,
        RedbRepairContext::WriteCommand,
    )?;
    if cancel_worker.load(Ordering::Acquire) {
        drop(db);
        return Err(StorageBackendError::BackendInternal {
            detail: "capture redb create cancelled before return".to_string(),
        });
    }
    Ok((db, guard))
}

impl RedbCaptureStore {
    /// Atomically create a new `capture.gen-<id>.redb` database with
    /// `DatabaseRole::Capture` `META_SCHEMA`, all 25 source tables, segregated
    /// internal/queue tables, secondary indexes, and Unix private security
    /// posture (`0700` dir, `0600` file, `O_NOFOLLOW`, effective-UID owner).
    pub async fn create(path: impl AsRef<Path>) -> StorageResult<Self> {
        #[cfg(unix)]
        {
            Self::create_with_expected_owners(path, UnixExpectedOwners::current_effective()).await
        }
        #[cfg(not(unix))]
        {
            let path = path.as_ref().to_path_buf();
            let target = path.clone();
            let cancelled = Arc::new(AtomicBool::new(false));
            let cancel_worker = Arc::clone(&cancelled);
            let mut cancel_guard = CancelFlagGuard {
                cancelled,
                completed: false,
            };
            let (db, mut publish_guard) = tokio::task::spawn_blocking(move || {
                create_redb_capture_blocking(target, cancel_worker)
            })
            .await
            .map_err(|err| StorageBackendError::BackendInternal {
                detail: format!("capture redb create task panicked or cancelled: {err}"),
            })??;
            // Commit publication ONLY after the async caller has received the handle.
            publish_guard.committed = true;
            cancel_guard.completed = true;

            Ok(Self {
                handle: Arc::new(RedbCaptureHandle::ReadWrite(Arc::new(db))),
                path,
                write_timeout: DEFAULT_CAPTURE_WRITE_TIMEOUT,
                write_tx_count: Arc::new(AtomicU64::new(0)),
            })
        }
    }

    #[cfg(unix)]
    pub(crate) async fn create_with_expected_owners(
        path: impl AsRef<Path>,
        owners: UnixExpectedOwners,
    ) -> StorageResult<Self> {
        #[cfg(test)]
        {
            Self::create_with_hooks(path, owners, UnixCreateTestHooks::default()).await
        }
        #[cfg(not(test))]
        {
            let path = path.as_ref().to_path_buf();
            let target = path.clone();
            let cancelled = Arc::new(AtomicBool::new(false));
            let cancel_worker = Arc::clone(&cancelled);
            let mut cancel_guard = CancelFlagGuard {
                cancelled,
                completed: false,
            };
            let (db, mut publish_guard) = tokio::task::spawn_blocking(move || {
                create_redb_capture_blocking_with_owners(target, cancel_worker, owners)
            })
            .await
            .map_err(|err| StorageBackendError::BackendInternal {
                detail: format!("capture redb create task panicked or cancelled: {err}"),
            })??;
            // Commit publication ONLY after the async caller has received the handle.
            publish_guard.committed = true;
            cancel_guard.completed = true;

            Ok(Self {
                handle: Arc::new(RedbCaptureHandle::ReadWrite(Arc::new(db))),
                path,
                write_timeout: DEFAULT_CAPTURE_WRITE_TIMEOUT,
                write_tx_count: Arc::new(AtomicU64::new(0)),
            })
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) async fn create_with_hooks(
        path: impl AsRef<Path>,
        owners: UnixExpectedOwners,
        hooks: UnixCreateTestHooks,
    ) -> StorageResult<Self> {
        let path = path.as_ref().to_path_buf();
        let target = path.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_worker = Arc::clone(&cancelled);
        let mut cancel_guard = CancelFlagGuard {
            cancelled,
            completed: false,
        };
        let (db, mut publish_guard) = tokio::task::spawn_blocking(move || {
            create_redb_capture_blocking_with_owners(target, cancel_worker, owners, hooks)
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("capture redb create task panicked or cancelled: {err}"),
        })??;
        // Commit publication ONLY after the async caller has received the handle.
        publish_guard.committed = true;
        cancel_guard.completed = true;

        Ok(Self {
            handle: Arc::new(RedbCaptureHandle::ReadWrite(Arc::new(db))),
            path,
            write_timeout: DEFAULT_CAPTURE_WRITE_TIMEOUT,
            write_tx_count: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Open an existing `capture.gen-<id>.redb` database for read-write access,
    /// enforcing descriptor-backed Unix `O_NOFOLLOW` & effective-UID ownership
    /// on both the private directory and DB file (`RB-06d`), plus `META_SCHEMA`
    /// (`DatabaseRole::Capture`, `LBR-AGENT-048` on future schema,
    /// `RepositoryMigrated` on tombstone) and required tables.
    pub async fn open_read_write(
        path: impl AsRef<Path>,
        context: RedbRepairContext,
    ) -> StorageResult<Self> {
        #[cfg(unix)]
        {
            Self::open_read_write_with_expected_owners(
                path,
                context,
                UnixExpectedOwners::current_effective(),
            )
            .await
        }
        #[cfg(not(unix))]
        {
            let path = path.as_ref().to_path_buf();
            let target = path.clone();
            let db = tokio::task::spawn_blocking(move || -> StorageResult<Database> {
                let db = open_redb_writable(&target, DatabaseRole::Capture, context)?;
                let rtx = begin_checked_read(&db, DatabaseRole::Capture)?;
                verify_required_capture_tables(&rtx)?;
                drop(rtx);
                ensure_tombstone_indexes_synced(&db)?;
                Ok(db)
            })
            .await
            .map_err(|err| StorageBackendError::BackendInternal {
                detail: format!("capture redb open_read_write task failed: {err}"),
            })??;

            Ok(Self {
                handle: Arc::new(RedbCaptureHandle::ReadWrite(Arc::new(db))),
                path,
                write_timeout: DEFAULT_CAPTURE_WRITE_TIMEOUT,
                write_tx_count: Arc::new(AtomicU64::new(0)),
            })
        }
    }

    #[cfg(unix)]
    pub(crate) async fn open_read_write_with_expected_owners(
        path: impl AsRef<Path>,
        context: RedbRepairContext,
        owners: UnixExpectedOwners,
    ) -> StorageResult<Self> {
        #[cfg(test)]
        {
            Self::open_read_write_with_hook(path, context, owners, None).await
        }
        #[cfg(not(test))]
        {
            let path = path.as_ref().to_path_buf();
            let target = path.clone();
            let db = tokio::task::spawn_blocking(move || -> StorageResult<Database> {
                let (parent, file_name) = split_capture_target_path(&target)?;
                let (dir_fd, _) = open_dir_nofollow(&parent, owners.dir_uid)?;
                let (file, st) =
                    open_existing_file_in_dir(&dir_fd, &file_name, owners.file_uid, true)?;
                drop(dir_fd);
                let db = open_redb_writable_from_fd(file, Some(st), context, false)?;
                let rtx = begin_checked_read(&db, DatabaseRole::Capture)?;
                verify_required_capture_tables(&rtx)?;
                drop(rtx);
                ensure_tombstone_indexes_synced(&db)?;
                Ok(db)
            })
            .await
            .map_err(|err| StorageBackendError::BackendInternal {
                detail: format!("capture redb open_read_write task failed: {err}"),
            })??;

            Ok(Self {
                handle: Arc::new(RedbCaptureHandle::ReadWrite(Arc::new(db))),
                path,
                write_timeout: DEFAULT_CAPTURE_WRITE_TIMEOUT,
                write_tx_count: Arc::new(AtomicU64::new(0)),
            })
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) async fn open_read_write_with_hook(
        path: impl AsRef<Path>,
        context: RedbRepairContext,
        owners: UnixExpectedOwners,
        after_dir_open: Option<Box<dyn FnOnce() + Send>>,
    ) -> StorageResult<Self> {
        let path = path.as_ref().to_path_buf();
        let target = path.clone();
        let db = tokio::task::spawn_blocking(move || -> StorageResult<Database> {
            let (parent, file_name) = split_capture_target_path(&target)?;
            let (dir_fd, _) = open_dir_nofollow(&parent, owners.dir_uid)?;
            if let Some(hook) = after_dir_open {
                hook();
            }
            let (file, st) = open_existing_file_in_dir(&dir_fd, &file_name, owners.file_uid, true)?;
            drop(dir_fd);
            let db = open_redb_writable_from_fd(file, Some(st), context, false)?;
            let rtx = begin_checked_read(&db, DatabaseRole::Capture)?;
            verify_required_capture_tables(&rtx)?;
            drop(rtx);
            ensure_tombstone_indexes_synced(&db)?;
            Ok(db)
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("capture redb open_read_write task failed: {err}"),
        })??;

        Ok(Self {
            handle: Arc::new(RedbCaptureHandle::ReadWrite(Arc::new(db))),
            path,
            write_timeout: DEFAULT_CAPTURE_WRITE_TIMEOUT,
            write_tx_count: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Open an existing `capture.gen-<id>.redb` database strictly read-only
    /// (`16 MiB` cache, `MultiWriter`, zero write upgrades, descriptor-backed
    /// Unix `O_NOFOLLOW` & effective-UID ownership, `LBR-AGENT-048` on future
    /// schema).
    pub async fn open_read_only(path: impl AsRef<Path>) -> StorageResult<Self> {
        #[cfg(unix)]
        {
            Self::open_read_only_with_expected_owners(path, UnixExpectedOwners::current_effective())
                .await
        }
        #[cfg(not(unix))]
        {
            let path = path.as_ref().to_path_buf();
            let target = path.clone();
            let db = tokio::task::spawn_blocking(move || -> StorageResult<ReadOnlyDatabase> {
                let db = open_redb_read_only(&target, DatabaseRole::Capture)?;
                let rtx = begin_checked_read(&db, DatabaseRole::Capture)?;
                verify_required_capture_tables(&rtx)?;
                drop(rtx);
                Ok(db)
            })
            .await
            .map_err(|err| StorageBackendError::BackendInternal {
                detail: format!("capture redb open_read_only task failed: {err}"),
            })??;

            Ok(Self {
                handle: Arc::new(RedbCaptureHandle::ReadOnly(Arc::new(db))),
                path,
                write_timeout: DEFAULT_CAPTURE_WRITE_TIMEOUT,
                write_tx_count: Arc::new(AtomicU64::new(0)),
            })
        }
    }

    #[cfg(unix)]
    pub(crate) async fn open_read_only_with_expected_owners(
        path: impl AsRef<Path>,
        owners: UnixExpectedOwners,
    ) -> StorageResult<Self> {
        #[cfg(test)]
        {
            Self::open_read_only_with_hook(path, owners, None).await
        }
        #[cfg(not(test))]
        {
            let path = path.as_ref().to_path_buf();
            let target = path.clone();
            let db = tokio::task::spawn_blocking(move || -> StorageResult<ReadOnlyDatabase> {
                let (parent, file_name) = split_capture_target_path(&target)?;
                let (dir_fd, _) = open_dir_nofollow(&parent, owners.dir_uid)?;
                let (file, _) =
                    open_existing_file_in_dir(&dir_fd, &file_name, owners.file_uid, false)?;
                drop(dir_fd);
                let db = open_redb_read_only_from_fd(file)?;
                let rtx = begin_checked_read(&db, DatabaseRole::Capture)?;
                verify_required_capture_tables(&rtx)?;
                drop(rtx);
                Ok(db)
            })
            .await
            .map_err(|err| StorageBackendError::BackendInternal {
                detail: format!("capture redb open_read_only task failed: {err}"),
            })??;

            Ok(Self {
                handle: Arc::new(RedbCaptureHandle::ReadOnly(Arc::new(db))),
                path,
                write_timeout: DEFAULT_CAPTURE_WRITE_TIMEOUT,
                write_tx_count: Arc::new(AtomicU64::new(0)),
            })
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) async fn open_read_only_with_hook(
        path: impl AsRef<Path>,
        owners: UnixExpectedOwners,
        after_dir_open: Option<Box<dyn FnOnce() + Send>>,
    ) -> StorageResult<Self> {
        let path = path.as_ref().to_path_buf();
        let target = path.clone();
        let db = tokio::task::spawn_blocking(move || -> StorageResult<ReadOnlyDatabase> {
            let (parent, file_name) = split_capture_target_path(&target)?;
            let (dir_fd, _) = open_dir_nofollow(&parent, owners.dir_uid)?;
            if let Some(hook) = after_dir_open {
                hook();
            }
            let (file, _) = open_existing_file_in_dir(&dir_fd, &file_name, owners.file_uid, false)?;
            drop(dir_fd);
            let db = open_redb_read_only_from_fd(file)?;
            let rtx = begin_checked_read(&db, DatabaseRole::Capture)?;
            verify_required_capture_tables(&rtx)?;
            drop(rtx);
            Ok(db)
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("capture redb open_read_only task failed: {err}"),
        })??;

        Ok(Self {
            handle: Arc::new(RedbCaptureHandle::ReadOnly(Arc::new(db))),
            path,
            write_timeout: DEFAULT_CAPTURE_WRITE_TIMEOUT,
            write_tx_count: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Path to the underlying `.redb` file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Override the bounded write-lock wait timeout.
    #[must_use]
    pub fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = timeout;
        self
    }

    /// Total number of committed redb [`WriteTransaction`]s executed through
    /// this store instance (diagnostic/test hook proving single-transaction
    /// batch commits).
    #[must_use]
    pub fn write_transaction_count(&self) -> u64 {
        self.write_tx_count.load(Ordering::Acquire)
    }

    /// Reject overwriting an `agent_audit_log` record inside a checked write
    /// transaction (`RB-06c` AC2).
    pub async fn update_audit_log(&self, record: &AgentAuditLogRecord) -> StorageResult<()> {
        validate_audit_log_record(record)?;
        let audit_id = record.audit_id.clone();
        self.write_tx("update_audit_log", move |wtx| {
            let tbl = wtx
                .open_table(AGENT_AUDIT_LOG_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_audit_log"))?;
            let _ = tbl
                .get(audit_id.as_str())
                .map_err(|e| map_redb_storage_err(e, "check agent_audit_log"))?;
            Err(map_append_only_err("agent_audit_log", "update"))
        })
        .await
    }

    /// Reject deleting an `agent_audit_log` record inside a checked write
    /// transaction (`RB-06c` AC3).
    pub async fn delete_audit_log(&self, audit_id: &str) -> StorageResult<()> {
        validate_non_empty_no_nul(audit_id, "audit_id")?;
        let id = audit_id.to_string();
        self.write_tx("delete_audit_log", move |wtx| {
            let tbl = wtx
                .open_table(AGENT_AUDIT_LOG_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_audit_log"))?;
            let _ = tbl
                .get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "check agent_audit_log"))?;
            Err(map_append_only_err("agent_audit_log", "delete"))
        })
        .await
    }

    /// Reject overwriting an `agent_workspace_scope_audit` record inside a
    /// checked write transaction (`RB-06c` AC4).
    pub async fn update_workspace_scope_audit(
        &self,
        record: &AgentWorkspaceScopeAuditRecord,
    ) -> StorageResult<()> {
        validate_workspace_scope_audit_record(record)?;
        let audit_id = record.audit_id.clone();
        self.write_tx("update_workspace_scope_audit", move |wtx| {
            let tbl = wtx
                .open_table(AGENT_WORKSPACE_SCOPE_AUDIT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_workspace_scope_audit"))?;
            let _ = tbl
                .get(audit_id.as_str())
                .map_err(|e| map_redb_storage_err(e, "check agent_workspace_scope_audit"))?;
            Err(map_append_only_err("agent_workspace_scope_audit", "update"))
        })
        .await
    }

    /// Reject deleting an `agent_workspace_scope_audit` record inside a checked
    /// write transaction (`RB-06c` AC5).
    pub async fn delete_workspace_scope_audit(&self, audit_id: &str) -> StorageResult<()> {
        validate_non_empty_no_nul(audit_id, "audit_id")?;
        let id = audit_id.to_string();
        self.write_tx("delete_workspace_scope_audit", move |wtx| {
            let tbl = wtx
                .open_table(AGENT_WORKSPACE_SCOPE_AUDIT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_workspace_scope_audit"))?;
            let _ = tbl
                .get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "check agent_workspace_scope_audit"))?;
            Err(map_append_only_err("agent_workspace_scope_audit", "delete"))
        })
        .await
    }

    /// Return all live table names currently registered inside this `.redb` file.
    pub fn list_live_table_names(&self) -> StorageResult<BTreeSet<String>> {
        let rtx = match self.handle.as_ref() {
            RedbCaptureHandle::ReadWrite(db) => {
                begin_checked_read(db.as_ref(), DatabaseRole::Capture)?
            }
            RedbCaptureHandle::ReadOnly(db) => {
                begin_checked_read(db.as_ref(), DatabaseRole::Capture)?
            }
        };
        let names = rtx
            .list_tables()
            .map_err(|e| map_redb_storage_err(e, "list live redb capture tables"))?
            .map(|h| h.name().to_string())
            .collect();
        Ok(names)
    }

    async fn read_tx<T, F>(&self, op: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&ReadTransaction) -> StorageResult<T> + Send + 'static,
    {
        let handle = Arc::clone(&self.handle);
        tokio::task::spawn_blocking(move || -> StorageResult<T> {
            let rtx = match handle.as_ref() {
                RedbCaptureHandle::ReadWrite(db) => {
                    begin_checked_read(db.as_ref(), DatabaseRole::Capture)?
                }
                RedbCaptureHandle::ReadOnly(db) => {
                    begin_checked_read(db.as_ref(), DatabaseRole::Capture)?
                }
            };
            op(&rtx)
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("capture redb read task failed: {err}"),
        })?
    }

    async fn write_tx<T, F>(&self, context: &'static str, op: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&WriteTransaction) -> StorageResult<T> + Send + 'static,
    {
        let db = match self.handle.as_ref() {
            RedbCaptureHandle::ReadWrite(db) => Arc::clone(db),
            RedbCaptureHandle::ReadOnly(_) => {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!("cannot mutate read-only redb capture store ({context})"),
                });
            }
        };
        let timeout = self.write_timeout;
        let counter = Arc::clone(&self.write_tx_count);
        tokio::task::spawn_blocking(move || -> StorageResult<T> {
            let wtx = begin_checked_write_blocking(db.as_ref(), DatabaseRole::Capture, timeout)?;
            let out = op(&wtx)?;
            wtx.commit()
                .map_err(|err| map_redb_commit_err(err, context))?;
            counter.fetch_add(1, Ordering::Release);
            Ok(out)
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("capture redb write task failed ({context}): {err}"),
        })?
    }
}

#[async_trait]
impl AgentBridgeStore for RedbCaptureStore {
    async fn open_bridge_session(
        &self,
        input: &BridgeSessionOpenInput,
    ) -> StorageResult<(AgentBridgeSessionRecord, bool)> {
        validate_bridge_session_open(input)?;
        let input = input.clone();
        self.write_tx("open_bridge_session", move |wtx| {
            let mut tbl = wtx
                .open_table(AGENT_BRIDGE_SESSION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_session"))?;
            if let Some(existing) = tbl
                .get(input.bridge_session_id.as_str())
                .map_err(|e| map_redb_storage_err(e, "read agent_bridge_session"))?
            {
                let row: AgentBridgeSessionRecord = dec(existing.value())?;
                return Ok((row, false));
            }
            let row = AgentBridgeSessionRecord {
                bridge_session_id: input.bridge_session_id,
                source: BRIDGE_SOURCE_DEEPSEEK_HARNESS.to_string(),
                repository_id: input.repository_id,
                worktree_id: input.worktree_id,
                workspace_id: input.workspace_id,
                parent_session_id: input.parent_session_id,
                agent_id: input.agent_id,
                session_state: BridgeSessionState::Open,
                schema_version: 1,
                last_event_seq: 0,
                last_acked_seq: 0,
                created_at: input.now_ms,
                updated_at: input.now_ms,
            };
            let bytes = enc(&row)?;
            tbl.insert(row.bridge_session_id.as_str(), bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert agent_bridge_session"))?;
            Ok((row, true))
        })
        .await
    }

    async fn get_bridge_session(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Option<AgentBridgeSessionRecord>> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let sid = bridge_session_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_BRIDGE_SESSION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_session"))?;
            tbl.get(sid.as_str())
                .map_err(|e| map_redb_storage_err(e, "get agent_bridge_session"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn set_bridge_session_state(
        &self,
        bridge_session_id: &str,
        state: BridgeSessionState,
        updated_at: i64,
    ) -> StorageResult<()> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let sid = bridge_session_id.to_string();
        self.write_tx("set_bridge_session_state", move |wtx| {
            let mut tbl = wtx
                .open_table(AGENT_BRIDGE_SESSION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_session"))?;
            let existing = tbl
                .get(sid.as_str())
                .map_err(|e| map_redb_storage_err(e, "read agent_bridge_session"))?
                .map(|g| dec::<AgentBridgeSessionRecord>(g.value()))
                .transpose()?;
            let Some(mut row) = existing else {
                return Err(StorageBackendError::NotFound {
                    resource: "agent_bridge_session".to_string(),
                });
            };
            row.session_state = state;
            row.updated_at = updated_at;
            let bytes = enc(&row)?;
            tbl.insert(sid.as_str(), bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "update agent_bridge_session"))?;
            Ok(())
        })
        .await
    }

    async fn delete_bridge_session(&self, bridge_session_id: &str) -> StorageResult<bool> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let sid = bridge_session_id.to_string();
        self.write_tx("delete_bridge_session", move |wtx| {
            let removed = {
                let mut tbl = wtx
                    .open_table(AGENT_BRIDGE_SESSION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_bridge_session"))?;
                tbl.remove(sid.as_str())
                    .map_err(|e| map_redb_storage_err(e, "remove agent_bridge_session"))?
                    .is_some()
            };
            if !removed {
                return Ok(false);
            }

            // Cascade delete bridge checkpoints
            {
                let mut tbl = wtx
                    .open_table(AGENT_BRIDGE_CHECKPOINT_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_bridge_checkpoint"))?;
                let mut keys = Vec::new();
                for entry in tbl
                    .iter()
                    .map_err(|e| map_redb_storage_err(e, "scan agent_bridge_checkpoint"))?
                {
                    let (k, v) = entry
                        .map_err(|e| map_redb_storage_err(e, "iter agent_bridge_checkpoint"))?;
                    let row: AgentBridgeCheckpointRecord = dec(v.value())?;
                    if row.bridge_session_id == sid {
                        keys.push(k.value().to_string());
                    }
                }
                for k in keys {
                    tbl.remove(k.as_str())
                        .map_err(|e| map_redb_storage_err(e, "remove bridge checkpoint"))?;
                }
            }

            // Cascade delete bridge events + BRIDGE_EVENTS_BY_SEQ + EVENTS_BY_TIME
            {
                let mut by_seq = wtx
                    .open_table(BRIDGE_EVENTS_BY_SEQ_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open bridge_events_by_seq"))?;
                let mut events_tbl = wtx
                    .open_table(AGENT_BRIDGE_EVENT_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_bridge_event"))?;
                let mut by_time = wtx
                    .open_table(EVENTS_BY_TIME_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open events_by_time"))?;

                let mut seq_entries = Vec::new();
                for entry in by_seq
                    .range((sid.as_str(), i64::MIN)..=(sid.as_str(), i64::MAX))
                    .map_err(|e| map_redb_storage_err(e, "range bridge_events_by_seq"))?
                {
                    let (k, v) =
                        entry.map_err(|e| map_redb_storage_err(e, "iter bridge_events_by_seq"))?;
                    let idx: BridgeEventSeqIndexEntry = dec(v.value())?;
                    seq_entries.push((k.value().1, idx.event_id));
                }
                for (seq, event_id) in seq_entries {
                    by_seq
                        .remove((sid.as_str(), seq))
                        .map_err(|e| map_redb_storage_err(e, "remove bridge_events_by_seq"))?;
                    if let Some(ev_bytes) = events_tbl
                        .remove(event_id)
                        .map_err(|e| map_redb_storage_err(e, "remove agent_bridge_event"))?
                    {
                        let ev: AgentBridgeEventRecord = dec(ev_bytes.value())?;
                        by_time
                            .remove((ev.created_at, sid.as_str(), ev.event_seq))
                            .map_err(|e| map_redb_storage_err(e, "remove events_by_time"))?;
                    }
                }
            }

            // Cascade delete bridge links and their BRIDGE_LINKS_BY_KEY entries
            {
                let mut tbl = wtx
                    .open_table(AGENT_BRIDGE_LINK_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_bridge_link"))?;
                let mut by_key = wtx
                    .open_table(BRIDGE_LINKS_BY_KEY_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open bridge_links_by_key"))?;
                let mut removed_rows = Vec::new();
                for entry in tbl
                    .iter()
                    .map_err(|e| map_redb_storage_err(e, "scan agent_bridge_link"))?
                {
                    let (k, v) =
                        entry.map_err(|e| map_redb_storage_err(e, "iter agent_bridge_link"))?;
                    let row: AgentBridgeLinkRecord = dec(v.value())?;
                    if row.bridge_session_id == sid {
                        removed_rows.push((k.value(), row));
                    }
                }
                for (id, row) in removed_rows {
                    tbl.remove(id)
                        .map_err(|e| map_redb_storage_err(e, "remove bridge link"))?;
                    by_key
                        .remove((
                            row.source_type.as_str(),
                            row.source_id.as_str(),
                            row.target_type.as_str(),
                            row.target_id.as_str(),
                        ))
                        .map_err(|e| map_redb_storage_err(e, "remove bridge_links_by_key"))?;
                }
            }

            // Cascade delete bridge operations
            {
                let mut tbl = wtx
                    .open_table(AGENT_BRIDGE_OPERATION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_bridge_operation"))?;
                let mut ids = Vec::new();
                for entry in tbl
                    .iter()
                    .map_err(|e| map_redb_storage_err(e, "scan agent_bridge_operation"))?
                {
                    let (k, v) = entry
                        .map_err(|e| map_redb_storage_err(e, "iter agent_bridge_operation"))?;
                    let row: AgentBridgeOperationRecord = dec(v.value())?;
                    if row.bridge_session_id == sid {
                        ids.push(k.value().to_string());
                    }
                }
                for id in ids {
                    tbl.remove(id.as_str())
                        .map_err(|e| map_redb_storage_err(e, "remove bridge operation"))?;
                }
            }

            Ok(true)
        })
        .await
    }

    async fn append_and_ack_events(
        &self,
        bridge_session_id: &str,
        events: &[BridgeEventAppendInput],
        now_ms: i64,
    ) -> StorageResult<BridgeEventAppendOutcome> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let sid = bridge_session_id.to_string();
        let events = events.to_vec();

        self.write_tx("append_and_ack_events", move |wtx| {
            let mut session = {
                let sessions = wtx
                    .open_table(AGENT_BRIDGE_SESSION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_bridge_session"))?;
                let Some(raw) = sessions
                    .get(sid.as_str())
                    .map_err(|e| map_redb_storage_err(e, "read agent_bridge_session"))?
                else {
                    return Err(StorageBackendError::NotFound {
                        resource: "agent_bridge_session".to_string(),
                    });
                };
                dec::<AgentBridgeSessionRecord>(raw.value())?
            };

            let mut per_event = Vec::with_capacity(events.len());
            let mut max_present: Option<i64> = None;

            for ev in &events {
                if ev.event_seq <= 0
                    || ev.event_type.is_empty()
                    || ev.event_type.contains('\0')
                    || ev.payload.contains('\0')
                    || ev.payload.len() > MAX_BRIDGE_EVENT_PAYLOAD_BYTES
                    || ev.operation_id.as_deref().is_some_and(|s| s.contains('\0'))
                {
                    per_event.push(BridgeEventStatus::Rejected);
                    continue;
                }

                let digest = capture_sha256_hex(ev.payload.as_bytes());
                let existing = {
                    let by_seq = wtx
                        .open_table(BRIDGE_EVENTS_BY_SEQ_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open bridge_events_by_seq"))?;
                    by_seq
                        .get((sid.as_str(), ev.event_seq))
                        .map_err(|e| map_redb_storage_err(e, "lookup bridge_events_by_seq"))?
                        .map(|g| dec::<BridgeEventSeqIndexEntry>(g.value()))
                        .transpose()?
                };

                if let Some(idx) = existing {
                    if idx.payload_sha256 == digest {
                        per_event.push(BridgeEventStatus::Duplicate);
                        max_present =
                            Some(max_present.map_or(ev.event_seq, |cur| cur.max(ev.event_seq)));
                    } else {
                        per_event.push(BridgeEventStatus::DigestConflict);
                    }
                    continue;
                }

                let event_id = next_sequence_id(wtx, SEQ_KEY_BRIDGE_EVENT)?;
                let record = AgentBridgeEventRecord {
                    event_id,
                    bridge_session_id: sid.clone(),
                    event_seq: ev.event_seq,
                    event_type: ev.event_type.clone(),
                    payload_sha256: digest.clone(),
                    payload: ev.payload.clone(),
                    operation_id: ev.operation_id.clone(),
                    created_at: now_ms,
                };
                let rec_bytes = enc(&record)?;
                let seq_idx_bytes = enc(&BridgeEventSeqIndexEntry {
                    event_id,
                    payload_sha256: digest,
                })?;
                let time_idx_bytes = enc(&EventTimeIndexEntry { event_id })?;

                {
                    let mut events_tbl = wtx
                        .open_table(AGENT_BRIDGE_EVENT_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open agent_bridge_event"))?;
                    events_tbl
                        .insert(event_id, rec_bytes.as_slice())
                        .map_err(|e| map_redb_storage_err(e, "insert agent_bridge_event"))?;
                }
                {
                    let mut by_seq = wtx
                        .open_table(BRIDGE_EVENTS_BY_SEQ_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open bridge_events_by_seq"))?;
                    by_seq
                        .insert((sid.as_str(), ev.event_seq), seq_idx_bytes.as_slice())
                        .map_err(|e| map_redb_storage_err(e, "insert bridge_events_by_seq"))?;
                }
                {
                    let mut by_time = wtx
                        .open_table(EVENTS_BY_TIME_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open events_by_time"))?;
                    by_time
                        .insert(
                            (now_ms, sid.as_str(), ev.event_seq),
                            time_idx_bytes.as_slice(),
                        )
                        .map_err(|e| map_redb_storage_err(e, "insert events_by_time"))?;
                }

                per_event.push(BridgeEventStatus::Accepted);
                max_present = Some(max_present.map_or(ev.event_seq, |cur| cur.max(ev.event_seq)));
            }

            // Advance contiguous ack cursor incrementally from persisted
            // `last_acked_seq` over composite `(bridge_session_id, event_seq)` key.
            let start_expect = session.last_acked_seq.max(0).saturating_add(1);
            let contiguous = {
                let by_seq = wtx
                    .open_table(BRIDGE_EVENTS_BY_SEQ_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open bridge_events_by_seq for ack"))?;
                let mut expect = start_expect;
                for entry in by_seq
                    .range((sid.as_str(), start_expect)..=(sid.as_str(), i64::MAX))
                    .map_err(|e| map_redb_storage_err(e, "range bridge_events_by_seq"))?
                {
                    let (k, _) =
                        entry.map_err(|e| map_redb_storage_err(e, "iter bridge_events_by_seq"))?;
                    let seq = k.value().1;
                    if seq == expect {
                        expect = expect.saturating_add(1);
                    } else if seq > expect {
                        break;
                    }
                }
                expect - 1
            };

            let new_last_event_seq =
                max_present.map_or(session.last_event_seq, |m| session.last_event_seq.max(m));
            let new_last_acked_seq = session.last_acked_seq.max(contiguous);

            if max_present.is_some() || new_last_acked_seq > session.last_acked_seq {
                session.last_event_seq = new_last_event_seq;
                session.last_acked_seq = new_last_acked_seq;
                session.updated_at = now_ms;
                let mut sessions = wtx
                    .open_table(AGENT_BRIDGE_SESSION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_bridge_session for ack"))?;
                let bytes = enc(&session)?;
                sessions
                    .insert(sid.as_str(), bytes.as_slice())
                    .map_err(|e| map_redb_storage_err(e, "update bridge session ack"))?;
            }

            Ok(BridgeEventAppendOutcome {
                per_event,
                last_event_seq: new_last_event_seq,
                last_acked_seq: new_last_acked_seq,
            })
        })
        .await
    }

    async fn get_bridge_event_by_seq(
        &self,
        bridge_session_id: &str,
        event_seq: i64,
    ) -> StorageResult<Option<AgentBridgeEventRecord>> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let sid = bridge_session_id.to_string();
        self.read_tx(move |rtx| {
            let by_seq = rtx
                .open_table(BRIDGE_EVENTS_BY_SEQ_TABLE)
                .map_err(|e| map_redb_table_err(e, "open bridge_events_by_seq"))?;
            let Some(idx_raw) = by_seq
                .get((sid.as_str(), event_seq))
                .map_err(|e| map_redb_storage_err(e, "get bridge_events_by_seq"))?
            else {
                return Ok(None);
            };
            let idx: BridgeEventSeqIndexEntry = dec(idx_raw.value())?;
            let events_tbl = rtx
                .open_table(AGENT_BRIDGE_EVENT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_event"))?;
            events_tbl
                .get(idx.event_id)
                .map_err(|e| map_redb_storage_err(e, "get agent_bridge_event"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn list_bridge_events(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Vec<AgentBridgeEventRecord>> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let sid = bridge_session_id.to_string();
        self.read_tx(move |rtx| {
            let by_seq = rtx
                .open_table(BRIDGE_EVENTS_BY_SEQ_TABLE)
                .map_err(|e| map_redb_table_err(e, "open bridge_events_by_seq"))?;
            let events_tbl = rtx
                .open_table(AGENT_BRIDGE_EVENT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_event"))?;
            let mut out = Vec::new();
            for entry in by_seq
                .range((sid.as_str(), i64::MIN)..=(sid.as_str(), i64::MAX))
                .map_err(|e| map_redb_storage_err(e, "range bridge_events_by_seq"))?
            {
                let (_, v) =
                    entry.map_err(|e| map_redb_storage_err(e, "iter bridge_events_by_seq"))?;
                let idx: BridgeEventSeqIndexEntry = dec(v.value())?;
                if let Some(raw) = events_tbl
                    .get(idx.event_id)
                    .map_err(|e| map_redb_storage_err(e, "get bridge event"))?
                {
                    out.push(dec(raw.value())?);
                }
            }
            Ok(out)
        })
        .await
    }

    async fn list_bridge_events_by_time(&self) -> StorageResult<Vec<AgentBridgeEventRecord>> {
        self.read_tx(|rtx| {
            let by_time = rtx
                .open_table(EVENTS_BY_TIME_TABLE)
                .map_err(|e| map_redb_table_err(e, "open events_by_time"))?;
            let events_tbl = rtx
                .open_table(AGENT_BRIDGE_EVENT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_event"))?;
            let mut out = Vec::new();
            for entry in by_time
                .iter()
                .map_err(|e| map_redb_storage_err(e, "iter events_by_time"))?
            {
                let (_, v) = entry.map_err(|e| map_redb_storage_err(e, "read events_by_time"))?;
                let idx: EventTimeIndexEntry = dec(v.value())?;
                if let Some(raw) = events_tbl
                    .get(idx.event_id)
                    .map_err(|e| map_redb_storage_err(e, "get bridge event by time"))?
                {
                    out.push(dec(raw.value())?);
                }
            }
            Ok(out)
        })
        .await
    }

    async fn put_bridge_checkpoint(
        &self,
        record: &AgentBridgeCheckpointRecord,
    ) -> StorageResult<()> {
        validate_bridge_checkpoint_record(record)?;
        let rec = record.clone();
        self.write_tx("put_bridge_checkpoint", move |wtx| {
            {
                let sessions = wtx
                    .open_table(AGENT_BRIDGE_SESSION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_bridge_session"))?;
                if sessions
                    .get(rec.bridge_session_id.as_str())
                    .map_err(|e| map_redb_storage_err(e, "check bridge checkpoint FK"))?
                    .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_bridge_checkpoint foreign key violation".to_string(),
                    });
                }
            }
            let mut tbl = wtx
                .open_table(AGENT_BRIDGE_CHECKPOINT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_checkpoint"))?;
            let bytes = enc(&rec)?;
            tbl.insert(rec.checkpoint_id.as_str(), bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert agent_bridge_checkpoint"))?;
            Ok(())
        })
        .await
    }

    async fn get_bridge_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> StorageResult<Option<AgentBridgeCheckpointRecord>> {
        validate_non_empty_no_nul(checkpoint_id, "checkpoint_id")?;
        let id = checkpoint_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_BRIDGE_CHECKPOINT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_checkpoint"))?;
            tbl.get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "get agent_bridge_checkpoint"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn list_bridge_checkpoints(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Vec<AgentBridgeCheckpointRecord>> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let sid = bridge_session_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_BRIDGE_CHECKPOINT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_checkpoint"))?;
            let mut out = Vec::new();
            for entry in tbl
                .iter()
                .map_err(|e| map_redb_storage_err(e, "iter agent_bridge_checkpoint"))?
            {
                let (_, v) =
                    entry.map_err(|e| map_redb_storage_err(e, "read agent_bridge_checkpoint"))?;
                let row: AgentBridgeCheckpointRecord = dec(v.value())?;
                if row.bridge_session_id == sid {
                    out.push(row);
                }
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.checkpoint_id.cmp(&b.checkpoint_id))
            });
            Ok(out)
        })
        .await
    }

    async fn put_bridge_operation(&self, record: &AgentBridgeOperationRecord) -> StorageResult<()> {
        validate_bridge_operation_record(record)?;
        let rec = record.clone();
        self.write_tx("put_bridge_operation", move |wtx| {
            {
                let sessions = wtx
                    .open_table(AGENT_BRIDGE_SESSION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_bridge_session"))?;
                if sessions
                    .get(rec.bridge_session_id.as_str())
                    .map_err(|e| map_redb_storage_err(e, "check bridge operation FK"))?
                    .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_bridge_operation foreign key violation".to_string(),
                    });
                }
            }
            let mut tbl = wtx
                .open_table(AGENT_BRIDGE_OPERATION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_operation"))?;
            let bytes = enc(&rec)?;
            tbl.insert(rec.operation_id.as_str(), bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert agent_bridge_operation"))?;
            Ok(())
        })
        .await
    }

    async fn get_bridge_operation(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<AgentBridgeOperationRecord>> {
        validate_non_empty_no_nul(operation_id, "operation_id")?;
        let id = operation_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_BRIDGE_OPERATION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_operation"))?;
            tbl.get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "get agent_bridge_operation"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn list_bridge_operations(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Vec<AgentBridgeOperationRecord>> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let sid = bridge_session_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_BRIDGE_OPERATION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_operation"))?;
            let mut out = Vec::new();
            for entry in tbl
                .iter()
                .map_err(|e| map_redb_storage_err(e, "iter agent_bridge_operation"))?
            {
                let (_, v) =
                    entry.map_err(|e| map_redb_storage_err(e, "read agent_bridge_operation"))?;
                let row: AgentBridgeOperationRecord = dec(v.value())?;
                if row.bridge_session_id == sid {
                    out.push(row);
                }
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.operation_id.cmp(&b.operation_id))
            });
            Ok(out)
        })
        .await
    }

    async fn insert_bridge_link(
        &self,
        link: &BridgeLinkWrite,
    ) -> StorageResult<(AgentBridgeLinkRecord, bool)> {
        validate_bridge_link_write(link)?;
        let link = link.clone();
        self.write_tx("insert_bridge_link", move |wtx| {
            {
                let sessions = wtx
                    .open_table(AGENT_BRIDGE_SESSION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_bridge_session"))?;
                if sessions
                    .get(link.bridge_session_id.as_str())
                    .map_err(|e| map_redb_storage_err(e, "check bridge link FK"))?
                    .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_bridge_link foreign key violation".to_string(),
                    });
                }
            }
            // O(log N) lookup via BRIDGE_LINKS_BY_KEY_TABLE
            let existing_link_id = {
                let by_key = wtx
                    .open_table(BRIDGE_LINKS_BY_KEY_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open bridge_links_by_key"))?;
                by_key
                    .get((
                        link.source_type.as_str(),
                        link.source_id.as_str(),
                        link.target_type.as_str(),
                        link.target_id.as_str(),
                    ))
                    .map_err(|e| map_redb_storage_err(e, "lookup bridge_links_by_key"))?
                    .map(|g| dec::<BridgeLinkIndexEntry>(g.value()).map(|entry| entry.link_id))
                    .transpose()?
            };
            if let Some(existing_id) = existing_link_id {
                let tbl = wtx
                    .open_table(AGENT_BRIDGE_LINK_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_bridge_link"))?;
                let raw = tbl
                    .get(existing_id)
                    .map_err(|e| map_redb_storage_err(e, "get existing bridge link"))?
                    .ok_or_else(|| StorageBackendError::BackendInternal {
                        detail: "bridge_links_by_key index points to missing link row".to_string(),
                    })?;
                let existing: AgentBridgeLinkRecord = dec(raw.value())?;
                return Ok((existing, false));
            }
            let link_id = next_sequence_id(wtx, SEQ_KEY_BRIDGE_LINK)?;
            let row = AgentBridgeLinkRecord {
                link_id,
                bridge_session_id: link.bridge_session_id,
                source_type: link.source_type,
                source_id: link.source_id,
                target_type: link.target_type,
                target_id: link.target_id,
                created_at: link.created_at,
            };
            let bytes = enc(&row)?;
            let idx_bytes = enc(&BridgeLinkIndexEntry { link_id })?;
            {
                let mut tbl = wtx
                    .open_table(AGENT_BRIDGE_LINK_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_bridge_link for write"))?;
                tbl.insert(link_id, bytes.as_slice())
                    .map_err(|e| map_redb_storage_err(e, "insert agent_bridge_link"))?;
            }
            {
                let mut by_key = wtx
                    .open_table(BRIDGE_LINKS_BY_KEY_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open bridge_links_by_key for write"))?;
                by_key
                    .insert(
                        (
                            row.source_type.as_str(),
                            row.source_id.as_str(),
                            row.target_type.as_str(),
                            row.target_id.as_str(),
                        ),
                        idx_bytes.as_slice(),
                    )
                    .map_err(|e| map_redb_storage_err(e, "insert bridge_links_by_key"))?;
            }
            Ok((row, true))
        })
        .await
    }

    async fn list_bridge_links(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Vec<AgentBridgeLinkRecord>> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let sid = bridge_session_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_BRIDGE_LINK_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_bridge_link"))?;
            let mut out = Vec::new();
            for entry in tbl
                .iter()
                .map_err(|e| map_redb_storage_err(e, "iter agent_bridge_link"))?
            {
                let (_, v) = entry.map_err(|e| map_redb_storage_err(e, "read bridge link"))?;
                let row: AgentBridgeLinkRecord = dec(v.value())?;
                if row.bridge_session_id == sid {
                    out.push(row);
                }
            }
            Ok(out)
        })
        .await
    }
}

#[async_trait]
impl CaptureStore for RedbCaptureStore {
    fn backend_kind(&self) -> StorageBackendKind {
        StorageBackendKind::Redb
    }

    fn is_read_only(&self) -> bool {
        matches!(self.handle.as_ref(), RedbCaptureHandle::ReadOnly(_))
    }

    async fn append_capture_batch(
        &self,
        items: &[CaptureBatchItem],
        highwater_advance: Option<(CaptureQueueLane, CaptureQueueHighwater)>,
    ) -> StorageResult<CaptureBatchOutcome> {
        for item in items {
            validate_capture_batch_item(item)?;
        }
        let items = items.to_vec();
        self.write_tx("append_capture_batch", move |wtx| {
            if let Some((lane, next_hw)) = highwater_advance {
                let current_hw = read_highwater_from_wtx(wtx, lane)?;
                if next_hw < current_hw {
                    return Err(StorageBackendError::CorruptCommittedFrame {
                        detail: format!(
                            "capture queue highwater regression on {} lane",
                            lane.as_str()
                        ),
                    });
                }
            }

            let marker = enc(&MarkerValue)?;
            let mut applied_count = 0usize;
            let mut deduplicated_count = 0usize;

            for item in &items {
                let already_present = {
                    let mut dedup = wtx
                        .open_table(CAPTURE_DEDUP_KEYS_TABLE)
                        .map_err(|e| map_redb_table_err(e, "open capture_dedup_keys"))?;
                    if dedup
                        .get(item.dedup_key.as_str())
                        .map_err(|e| map_redb_storage_err(e, "lookup capture_dedup_keys"))?
                        .is_some()
                    {
                        true
                    } else {
                        dedup
                            .insert(item.dedup_key.as_str(), marker.as_slice())
                            .map_err(|e| map_redb_storage_err(e, "insert capture_dedup_keys"))?;
                        false
                    }
                };
                if already_present {
                    deduplicated_count += 1;
                    continue;
                }

                match &item.payload {
                    CaptureBatchPayload::UpsertSession(rec) => upsert_session_in_wtx(wtx, rec)?,
                    CaptureBatchPayload::UpsertCheckpoint(rec) => {
                        upsert_checkpoint_in_wtx(wtx, rec)?;
                    }
                    CaptureBatchPayload::UpsertSessionCommit(rec) => {
                        put_session_commit_in_wtx(wtx, rec)?;
                    }
                    CaptureBatchPayload::InsertUsageStats(rec) => {
                        put_usage_stats_in_wtx(wtx, rec)?;
                    }
                    CaptureBatchPayload::AppendAuditLog(rec) => {
                        append_audit_log_in_wtx(wtx, rec)?;
                    }
                    CaptureBatchPayload::AppendWorkspaceScopeAudit(rec) => {
                        append_workspace_scope_audit_in_wtx(wtx, rec)?;
                    }
                }
                applied_count += 1;
            }

            let highwater = if let Some((lane, next_hw)) = highwater_advance {
                write_highwater_in_wtx(wtx, lane, next_hw)?;
                Some(next_hw)
            } else {
                None
            };

            Ok(CaptureBatchOutcome {
                applied_count,
                deduplicated_count,
                highwater,
            })
        })
        .await
    }

    async fn get_queue_highwater(
        &self,
        lane: CaptureQueueLane,
    ) -> StorageResult<CaptureQueueHighwater> {
        self.read_tx(move |rtx| read_highwater_from_rtx(rtx, lane))
            .await
    }

    async fn put_schema_version(&self, record: &CaptureSchemaVersionRecord) -> StorageResult<()> {
        validate_schema_version_record(record)?;
        let rec = record.clone();
        self.write_tx("put_schema_version", move |wtx| {
            let mut tbl = wtx
                .open_table(SCHEMA_VERSIONS_TABLE)
                .map_err(|e| map_redb_table_err(e, "open schema_versions"))?;
            let bytes = enc(&rec)?;
            tbl.insert(rec.version, bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert schema_versions"))?;
            Ok(())
        })
        .await
    }

    async fn list_schema_versions(&self) -> StorageResult<Vec<CaptureSchemaVersionRecord>> {
        self.read_tx(|rtx| {
            let tbl = rtx
                .open_table(SCHEMA_VERSIONS_TABLE)
                .map_err(|e| map_redb_table_err(e, "open schema_versions"))?;
            let mut out = Vec::new();
            for entry in tbl
                .iter()
                .map_err(|e| map_redb_storage_err(e, "iter schema_versions"))?
            {
                let (_, v) = entry.map_err(|e| map_redb_storage_err(e, "read schema_versions"))?;
                out.push(dec(v.value())?);
            }
            Ok(out)
        })
        .await
    }

    async fn put_copy_receipt(
        &self,
        record: &CaptureCatalogCopyReceiptRecord,
    ) -> StorageResult<()> {
        validate_copy_receipt_record(record)?;
        let rec = record.clone();
        self.write_tx("put_copy_receipt", move |wtx| {
            let mut tbl = wtx
                .open_table(CAPTURE_CATALOG_COPY_RECEIPT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open capture_catalog_copy_receipt"))?;
            let bytes = enc(&rec)?;
            tbl.insert(rec.id, bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert copy receipt"))?;
            Ok(())
        })
        .await
    }

    async fn get_copy_receipt(&self) -> StorageResult<Option<CaptureCatalogCopyReceiptRecord>> {
        self.read_tx(|rtx| {
            let tbl = rtx
                .open_table(CAPTURE_CATALOG_COPY_RECEIPT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open capture_catalog_copy_receipt"))?;
            tbl.get(1i64)
                .map_err(|e| map_redb_storage_err(e, "get copy receipt"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn append_audit_log(&self, record: &AgentAuditLogRecord) -> StorageResult<()> {
        validate_audit_log_record(record)?;
        let rec = record.clone();
        self.write_tx("append_audit_log", move |wtx| {
            append_audit_log_in_wtx(wtx, &rec)
        })
        .await
    }

    async fn get_audit_log(&self, audit_id: &str) -> StorageResult<Option<AgentAuditLogRecord>> {
        validate_non_empty_no_nul(audit_id, "audit_id")?;
        let id = audit_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_AUDIT_LOG_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_audit_log"))?;
            tbl.get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "get agent_audit_log"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn list_audit_logs_by_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> StorageResult<Vec<AgentAuditLogRecord>> {
        validate_non_empty_no_nul(checkpoint_id, "checkpoint_id")?;
        let cp_id = checkpoint_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_AUDIT_LOG_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_audit_log"))?;
            let mut out = Vec::new();
            for entry in tbl
                .iter()
                .map_err(|e| map_redb_storage_err(e, "iter agent_audit_log"))?
            {
                let (_, v) = entry.map_err(|e| map_redb_storage_err(e, "read agent_audit_log"))?;
                let row: AgentAuditLogRecord = dec(v.value())?;
                if row.checkpoint_id == cp_id {
                    out.push(row);
                }
            }
            out.sort_by(|a, b| {
                a.timestamp
                    .cmp(&b.timestamp)
                    .then_with(|| a.audit_id.cmp(&b.audit_id))
            });
            Ok(out)
        })
        .await
    }

    async fn put_cloud_base(&self, record: &AgentCaptureCloudBaseRecord) -> StorageResult<()> {
        validate_cloud_base_record(record)?;
        let rec = record.clone();
        self.write_tx("put_cloud_base", move |wtx| {
            let mut tbl = wtx
                .open_table(AGENT_CAPTURE_CLOUD_BASE_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_capture_cloud_base"))?;
            let bytes = enc(&rec)?;
            tbl.insert(rec.repo_id.as_str(), bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert cloud base"))?;
            Ok(())
        })
        .await
    }

    async fn get_cloud_base(
        &self,
        repo_id: &str,
    ) -> StorageResult<Option<AgentCaptureCloudBaseRecord>> {
        validate_non_empty_no_nul(repo_id, "repo_id")?;
        let id = repo_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_CAPTURE_CLOUD_BASE_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_capture_cloud_base"))?;
            tbl.get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "get cloud base"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn put_incarnation(&self, record: &AgentCaptureIncarnationRecord) -> StorageResult<()> {
        validate_incarnation_record(record)?;
        let rec = record.clone();
        self.write_tx("put_incarnation", move |wtx| {
            let mut tbl = wtx
                .open_table(AGENT_CAPTURE_INCARNATION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_capture_incarnation"))?;
            let bytes = enc(&rec)?;
            tbl.insert(
                (rec.agent_kind.as_str(), rec.provider_session_id.as_str()),
                bytes.as_slice(),
            )
            .map_err(|e| map_redb_storage_err(e, "insert incarnation"))?;
            Ok(())
        })
        .await
    }

    async fn get_incarnation(
        &self,
        agent_kind: &str,
        provider_session_id: &str,
    ) -> StorageResult<Option<AgentCaptureIncarnationRecord>> {
        validate_non_empty_no_nul(agent_kind, "agent_kind")?;
        validate_non_empty_no_nul(provider_session_id, "provider_session_id")?;
        let kind = agent_kind.to_string();
        let psid = provider_session_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_CAPTURE_INCARNATION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_capture_incarnation"))?;
            tbl.get((kind.as_str(), psid.as_str()))
                .map_err(|e| map_redb_storage_err(e, "get incarnation"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn upsert_session(&self, record: &AgentSessionRecord) -> StorageResult<()> {
        validate_agent_session_record(record)?;
        let rec = record.clone();
        self.write_tx("upsert_session", move |wtx| {
            upsert_session_in_wtx(wtx, &rec)
        })
        .await
    }

    async fn get_session(&self, session_id: &str) -> StorageResult<Option<AgentSessionRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        let sid = session_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_SESSION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_session"))?;
            tbl.get(sid.as_str())
                .map_err(|e| map_redb_storage_err(e, "get agent_session"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn list_sessions_by_started_desc(&self) -> StorageResult<Vec<AgentSessionRecord>> {
        self.read_tx(|rtx| {
            let by_time = rtx
                .open_table(SESSIONS_BY_TIME_TABLE)
                .map_err(|e| map_redb_table_err(e, "open sessions_by_time"))?;
            let sessions = rtx
                .open_table(AGENT_SESSION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_session"))?;
            let mut out = Vec::new();
            for entry in by_time
                .iter()
                .map_err(|e| map_redb_storage_err(e, "iter sessions_by_time"))?
            {
                let (k, _) = entry.map_err(|e| map_redb_storage_err(e, "read sessions_by_time"))?;
                let sid = k.value().1;
                if let Some(raw) = sessions
                    .get(sid)
                    .map_err(|e| map_redb_storage_err(e, "get session from index"))?
                {
                    out.push(dec(raw.value())?);
                }
            }
            Ok(out)
        })
        .await
    }

    async fn delete_session(&self, session_id: &str) -> StorageResult<bool> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        let sid = session_id.to_string();
        self.write_tx("delete_session", move |wtx| {
            let existing = {
                let mut sessions = wtx
                    .open_table(AGENT_SESSION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_session"))?;
                sessions
                    .remove(sid.as_str())
                    .map_err(|e| map_redb_storage_err(e, "remove agent_session"))?
                    .map(|g| dec::<AgentSessionRecord>(g.value()))
                    .transpose()?
            };
            let Some(sess) = existing else {
                return Ok(false);
            };

            {
                let mut by_time = wtx
                    .open_table(SESSIONS_BY_TIME_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open sessions_by_time"))?;
                by_time
                    .remove((invert_capture_timestamp(sess.started_at), sid.as_str()))
                    .map_err(|e| map_redb_storage_err(e, "remove sessions_by_time entry"))?;
            }
            {
                let mut by_provider = wtx
                    .open_table(SESSIONS_BY_PROVIDER_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open sessions_by_provider"))?;
                by_provider
                    .remove((sess.agent_kind.as_str(), sess.provider_session_id.as_str()))
                    .map_err(|e| map_redb_storage_err(e, "remove sessions_by_provider entry"))?;
            }

            // Collect and cascade delete all checkpoints belonging to session
            let cp_ids: Vec<String> = {
                let by_session = wtx
                    .open_table(CHECKPOINTS_BY_SESSION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open checkpoints_by_session"))?;
                let mut ids = Vec::new();
                for entry in by_session
                    .range((sid.as_str(), i64::MIN, "")..)
                    .map_err(|e| map_redb_storage_err(e, "range checkpoints_by_session"))?
                {
                    let (k, _) = entry
                        .map_err(|e| map_redb_storage_err(e, "iter checkpoints_by_session"))?;
                    let (key_sid, _, cp_id) = k.value();
                    if key_sid != sid.as_str() {
                        break;
                    }
                    ids.push(cp_id.to_string());
                }
                ids
            };
            for cp_id in cp_ids {
                let _ = delete_checkpoint_in_wtx(wtx, &cp_id)?;
            }

            // Cascade delete agent_session_commit
            {
                let mut tbl = wtx
                    .open_table(AGENT_SESSION_COMMIT_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_session_commit"))?;
                let mut keys = Vec::new();
                for entry in tbl
                    .range((sid.as_str(), "", "")..)
                    .map_err(|e| map_redb_storage_err(e, "range agent_session_commit"))?
                {
                    let (k, _) =
                        entry.map_err(|e| map_redb_storage_err(e, "iter agent_session_commit"))?;
                    let (a, b, c) = k.value();
                    if a != sid.as_str() {
                        break;
                    }
                    keys.push((a.to_string(), b.to_string(), c.to_string()));
                }
                for (a, b, c) in keys {
                    tbl.remove((a.as_str(), b.as_str(), c.as_str()))
                        .map_err(|e| map_redb_storage_err(e, "cascade remove session_commit"))?;
                }
            }

            // Cascade delete agent_coverage_conflict, agent_coverage_claim, agent_coverage_revision
            {
                let mut tbl = wtx
                    .open_table(AGENT_COVERAGE_CONFLICT_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_coverage_conflict"))?;
                let mut keys = Vec::new();
                for entry in tbl
                    .range((sid.as_str(), "", i64::MIN)..)
                    .map_err(|e| map_redb_storage_err(e, "range agent_coverage_conflict"))?
                {
                    let (k, _) = entry
                        .map_err(|e| map_redb_storage_err(e, "iter agent_coverage_conflict"))?;
                    let (a, b, c) = k.value();
                    if a != sid.as_str() {
                        break;
                    }
                    keys.push((a.to_string(), b.to_string(), c));
                }
                for (a, b, c) in keys {
                    tbl.remove((a.as_str(), b.as_str(), c))
                        .map_err(|e| map_redb_storage_err(e, "cascade remove coverage_conflict"))?;
                }
            }
            {
                let mut tbl = wtx
                    .open_table(AGENT_COVERAGE_CLAIM_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_coverage_claim"))?;
                let mut keys = Vec::new();
                for entry in tbl
                    .range((sid.as_str(), "", i64::MIN)..)
                    .map_err(|e| map_redb_storage_err(e, "range agent_coverage_claim"))?
                {
                    let (k, _) =
                        entry.map_err(|e| map_redb_storage_err(e, "iter agent_coverage_claim"))?;
                    let (a, b, c) = k.value();
                    if a != sid.as_str() {
                        break;
                    }
                    keys.push((a.to_string(), b.to_string(), c));
                }
                for (a, b, c) in keys {
                    tbl.remove((a.as_str(), b.as_str(), c))
                        .map_err(|e| map_redb_storage_err(e, "cascade remove coverage_claim"))?;
                }
            }
            {
                let mut tbl = wtx
                    .open_table(AGENT_COVERAGE_REVISION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_coverage_revision"))?;
                let mut keys = Vec::new();
                for entry in tbl
                    .range((sid.as_str(), "", i64::MIN, i64::MIN)..)
                    .map_err(|e| map_redb_storage_err(e, "range agent_coverage_revision"))?
                {
                    let (k, _) = entry
                        .map_err(|e| map_redb_storage_err(e, "iter agent_coverage_revision"))?;
                    let (a, b, c, d) = k.value();
                    if a != sid.as_str() {
                        break;
                    }
                    keys.push((a.to_string(), b.to_string(), c, d));
                }
                for (a, b, c, d) in keys {
                    tbl.remove((a.as_str(), b.as_str(), c, d))
                        .map_err(|e| map_redb_storage_err(e, "cascade remove coverage_revision"))?;
                }
            }

            // Cascade delete subagent content revisions, claims, and links
            {
                let mut tbl = wtx
                    .open_table(AGENT_SUBAGENT_CONTENT_REVISION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open subagent_content_revision"))?;
                let mut keys = Vec::new();
                for entry in tbl
                    .range((sid.as_str(), "", "", i64::MIN, i64::MIN)..)
                    .map_err(|e| map_redb_storage_err(e, "range subagent_content_revision"))?
                {
                    let (k, _) = entry
                        .map_err(|e| map_redb_storage_err(e, "iter subagent_content_revision"))?;
                    let (a, b, c, d, e) = k.value();
                    if a != sid.as_str() {
                        break;
                    }
                    keys.push((a.to_string(), b.to_string(), c.to_string(), d, e));
                }
                for (a, b, c, d, e) in keys {
                    tbl.remove((a.as_str(), b.as_str(), c.as_str(), d, e))
                        .map_err(|err| {
                            map_redb_storage_err(err, "cascade remove subagent_content_revision")
                        })?;
                }
            }
            {
                let mut tbl = wtx
                    .open_table(AGENT_SUBAGENT_CONTENT_CLAIM_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open subagent_content_claim"))?;
                let mut keys = Vec::new();
                for entry in tbl
                    .range((sid.as_str(), "", "", i64::MIN)..)
                    .map_err(|e| map_redb_storage_err(e, "range subagent_content_claim"))?
                {
                    let (k, _) = entry
                        .map_err(|e| map_redb_storage_err(e, "iter subagent_content_claim"))?;
                    let (a, b, c, d) = k.value();
                    if a != sid.as_str() {
                        break;
                    }
                    keys.push((a.to_string(), b.to_string(), c.to_string(), d));
                }
                for (a, b, c, d) in keys {
                    tbl.remove((a.as_str(), b.as_str(), c.as_str(), d))
                        .map_err(|e| {
                            map_redb_storage_err(e, "cascade remove subagent_content_claim")
                        })?;
                }
            }
            {
                let mut tbl = wtx
                    .open_table(AGENT_SUBAGENT_LINK_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_subagent_link"))?;
                let mut keys = Vec::new();
                for entry in tbl
                    .iter()
                    .map_err(|e| map_redb_storage_err(e, "scan agent_subagent_link"))?
                {
                    let (k, v) =
                        entry.map_err(|e| map_redb_storage_err(e, "iter agent_subagent_link"))?;
                    let row: AgentSubagentLinkRecord = dec(v.value())?;
                    if row.parent_session_id == sid {
                        keys.push(k.value().to_string());
                    }
                }
                for k in keys {
                    tbl.remove(k.as_str())
                        .map_err(|e| map_redb_storage_err(e, "cascade remove subagent_link"))?;
                }
            }

            Ok(true)
        })
        .await
    }

    async fn upsert_checkpoint(&self, record: &AgentCheckpointRecord) -> StorageResult<()> {
        validate_agent_checkpoint_record(record)?;
        let rec = record.clone();
        self.write_tx("upsert_checkpoint", move |wtx| {
            upsert_checkpoint_in_wtx(wtx, &rec)
        })
        .await
    }

    async fn get_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> StorageResult<Option<AgentCheckpointRecord>> {
        validate_non_empty_no_nul(checkpoint_id, "checkpoint_id")?;
        let id = checkpoint_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_CHECKPOINT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_checkpoint"))?;
            tbl.get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "get agent_checkpoint"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn list_checkpoints_by_session(
        &self,
        session_id: &str,
    ) -> StorageResult<Vec<AgentCheckpointRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        let sid = session_id.to_string();
        self.read_tx(move |rtx| {
            let by_session = rtx
                .open_table(CHECKPOINTS_BY_SESSION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open checkpoints_by_session"))?;
            let cps = rtx
                .open_table(AGENT_CHECKPOINT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_checkpoint"))?;
            let mut out = Vec::new();
            for entry in by_session
                .range((sid.as_str(), i64::MIN, "")..)
                .map_err(|e| map_redb_storage_err(e, "range checkpoints_by_session"))?
            {
                let (k, _) =
                    entry.map_err(|e| map_redb_storage_err(e, "iter checkpoints_by_session"))?;
                let (key_sid, _, cp_id) = k.value();
                if key_sid != sid.as_str() {
                    break;
                }
                if let Some(raw) = cps
                    .get(cp_id)
                    .map_err(|e| map_redb_storage_err(e, "get checkpoint from index"))?
                {
                    out.push(dec(raw.value())?);
                }
            }
            Ok(out)
        })
        .await
    }

    async fn delete_checkpoint(&self, checkpoint_id: &str) -> StorageResult<bool> {
        validate_non_empty_no_nul(checkpoint_id, "checkpoint_id")?;
        let id = checkpoint_id.to_string();
        self.write_tx("delete_checkpoint", move |wtx| {
            delete_checkpoint_in_wtx(wtx, &id)
        })
        .await
    }

    async fn put_checkpoint_prune_tombstone(
        &self,
        record: &AgentCheckpointPruneTombstoneRecord,
    ) -> StorageResult<()> {
        validate_checkpoint_prune_tombstone(record)?;
        let rec = record.clone();
        self.write_tx("put_checkpoint_prune_tombstone", move |wtx| {
            let mut tbl = wtx
                .open_table(AGENT_CHECKPOINT_PRUNE_TOMBSTONE_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_checkpoint_prune_tombstone"))?;
            let bytes = enc(&rec)?;
            tbl.insert(rec.checkpoint_id.as_str(), bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert prune tombstone"))?;
            Ok(())
        })
        .await
    }

    async fn get_checkpoint_prune_tombstone(
        &self,
        checkpoint_id: &str,
    ) -> StorageResult<Option<AgentCheckpointPruneTombstoneRecord>> {
        validate_non_empty_no_nul(checkpoint_id, "checkpoint_id")?;
        let id = checkpoint_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_CHECKPOINT_PRUNE_TOMBSTONE_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_checkpoint_prune_tombstone"))?;
            tbl.get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "get prune tombstone"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn upsert_coverage_claim(&self, record: &AgentCoverageClaimRecord) -> StorageResult<()> {
        validate_coverage_claim_record(record)?;
        let rec = record.clone();
        self.write_tx("upsert_coverage_claim", move |wtx| {
            {
                let sessions = wtx
                    .open_table(AGENT_SESSION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_session"))?;
                if sessions
                    .get(rec.session_id.as_str())
                    .map_err(|e| map_redb_storage_err(e, "check coverage_claim FK"))?
                    .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_coverage_claim foreign key violation".to_string(),
                    });
                }
            }
            let mut tbl = wtx
                .open_table(AGENT_COVERAGE_CLAIM_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_coverage_claim"))?;
            let bytes = enc(&rec)?;
            tbl.insert(
                (
                    rec.session_id.as_str(),
                    rec.logical_turn_key.as_str(),
                    rec.coverage_schema_version,
                ),
                bytes.as_slice(),
            )
            .map_err(|e| map_redb_storage_err(e, "insert agent_coverage_claim"))?;
            Ok(())
        })
        .await
    }

    async fn get_coverage_claim(
        &self,
        session_id: &str,
        logical_turn_key: &str,
        coverage_schema_version: i64,
    ) -> StorageResult<Option<AgentCoverageClaimRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        validate_non_empty_no_nul(logical_turn_key, "logical_turn_key")?;
        let sid = session_id.to_string();
        let turn = logical_turn_key.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_COVERAGE_CLAIM_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_coverage_claim"))?;
            tbl.get((sid.as_str(), turn.as_str(), coverage_schema_version))
                .map_err(|e| map_redb_storage_err(e, "get agent_coverage_claim"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn upsert_coverage_conflict(
        &self,
        record: &AgentCoverageConflictRecord,
    ) -> StorageResult<()> {
        validate_coverage_conflict_record(record)?;
        let rec = record.clone();
        self.write_tx("upsert_coverage_conflict", move |wtx| {
            {
                let claims = wtx
                    .open_table(AGENT_COVERAGE_CLAIM_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_coverage_claim"))?;
                if claims
                    .get((
                        rec.session_id.as_str(),
                        rec.logical_turn_key.as_str(),
                        rec.coverage_schema_version,
                    ))
                    .map_err(|e| map_redb_storage_err(e, "check coverage_conflict claim FK"))?
                    .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_coverage_conflict foreign key violation: claim not found"
                            .to_string(),
                    });
                }
            }
            if let Some(cp_id) = &rec.incumbent_checkpoint_id {
                let cps = wtx
                    .open_table(AGENT_CHECKPOINT_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_checkpoint"))?;
                if cps
                    .get(cp_id.as_str())
                    .map_err(|e| map_redb_storage_err(e, "check coverage_conflict checkpoint FK"))?
                    .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "agent_coverage_conflict foreign key violation: checkpoint not found"
                                .to_string(),
                    });
                }
            }
            let mut tbl = wtx
                .open_table(AGENT_COVERAGE_CONFLICT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_coverage_conflict"))?;
            let bytes = enc(&rec)?;
            tbl.insert(
                (
                    rec.session_id.as_str(),
                    rec.logical_turn_key.as_str(),
                    rec.coverage_schema_version,
                ),
                bytes.as_slice(),
            )
            .map_err(|e| map_redb_storage_err(e, "insert agent_coverage_conflict"))?;
            Ok(())
        })
        .await
    }

    async fn get_coverage_conflict(
        &self,
        session_id: &str,
        logical_turn_key: &str,
        coverage_schema_version: i64,
    ) -> StorageResult<Option<AgentCoverageConflictRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        validate_non_empty_no_nul(logical_turn_key, "logical_turn_key")?;
        let sid = session_id.to_string();
        let turn = logical_turn_key.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_COVERAGE_CONFLICT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_coverage_conflict"))?;
            tbl.get((sid.as_str(), turn.as_str(), coverage_schema_version))
                .map_err(|e| map_redb_storage_err(e, "get agent_coverage_conflict"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn put_coverage_revision(
        &self,
        record: &AgentCoverageRevisionRecord,
    ) -> StorageResult<()> {
        validate_coverage_revision_record(record)?;
        let rec = record.clone();
        self.write_tx("put_coverage_revision", move |wtx| {
            {
                let sessions = wtx
                    .open_table(AGENT_SESSION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_session"))?;
                if sessions
                    .get(rec.session_id.as_str())
                    .map_err(|e| map_redb_storage_err(e, "check coverage_revision FK"))?
                    .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_coverage_revision foreign key violation".to_string(),
                    });
                }
            }
            let mut tbl = wtx
                .open_table(AGENT_COVERAGE_REVISION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_coverage_revision"))?;
            let key = (
                rec.session_id.as_str(),
                rec.logical_turn_key.as_str(),
                rec.coverage_schema_version,
                rec.revision,
            );
            if let Some(existing_raw) = tbl
                .get(key)
                .map_err(|e| map_redb_storage_err(e, "check existing coverage revision"))?
            {
                let existing: AgentCoverageRevisionRecord = dec(existing_raw.value())?;
                if existing == rec {
                    return Ok(());
                }
                return Err(StorageBackendError::BackendInternal {
                    detail: "agent_coverage_revision is append-only: conflicting revision rejected"
                        .to_string(),
                });
            }
            let bytes = enc(&rec)?;
            tbl.insert(key, bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert agent_coverage_revision"))?;
            Ok(())
        })
        .await
    }

    async fn list_coverage_revisions(
        &self,
        session_id: &str,
        logical_turn_key: &str,
        coverage_schema_version: i64,
    ) -> StorageResult<Vec<AgentCoverageRevisionRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        validate_non_empty_no_nul(logical_turn_key, "logical_turn_key")?;
        let sid = session_id.to_string();
        let turn = logical_turn_key.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_COVERAGE_REVISION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_coverage_revision"))?;
            let mut out = Vec::new();
            for entry in tbl
                .range(
                    (
                        sid.as_str(),
                        turn.as_str(),
                        coverage_schema_version,
                        i64::MIN,
                    )
                        ..=(
                            sid.as_str(),
                            turn.as_str(),
                            coverage_schema_version,
                            i64::MAX,
                        ),
                )
                .map_err(|e| map_redb_storage_err(e, "range agent_coverage_revision"))?
            {
                let (_, v) =
                    entry.map_err(|e| map_redb_storage_err(e, "iter agent_coverage_revision"))?;
                out.push(dec(v.value())?);
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_export_job(&self, record: &AgentExportJobRecord) -> StorageResult<()> {
        validate_export_job_record(record)?;
        let rec = record.clone();
        self.write_tx("upsert_export_job", move |wtx| {
            let mut tbl = wtx
                .open_table(AGENT_EXPORT_JOB_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_export_job"))?;
            let mut existing_self: Option<AgentExportJobRecord> = None;
            for entry in tbl
                .iter()
                .map_err(|e| map_redb_storage_err(e, "scan agent_export_job"))?
            {
                let (k, v) = entry.map_err(|e| map_redb_storage_err(e, "iter agent_export_job"))?;
                let row: AgentExportJobRecord = dec(v.value())?;
                if k.value() == rec.job_id {
                    existing_self = Some(row);
                } else if row.agent_kind == rec.agent_kind
                    && row.provider_session_id == rec.provider_session_id
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_export_job unique constraint violated on (agent_kind, provider_session_id)"
                            .to_string(),
                    });
                }
            }
            if let Some(old) = &existing_self {
                validate_scope_immutability(
                    old.scope_state,
                    old.repo_id.as_deref(),
                    old.worktree_id.as_deref(),
                    old.workspace_id.as_deref(),
                    old.workspace_fence,
                    rec.scope_state,
                    rec.repo_id.as_deref(),
                    rec.worktree_id.as_deref(),
                    rec.workspace_id.as_deref(),
                    rec.workspace_fence,
                    "agent_export_job",
                )?;
            }
            let bytes = enc(&rec)?;
            tbl.insert(rec.job_id.as_str(), bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert agent_export_job"))?;
            Ok(())
        })
        .await
    }

    async fn get_export_job(&self, job_id: &str) -> StorageResult<Option<AgentExportJobRecord>> {
        validate_non_empty_no_nul(job_id, "job_id")?;
        let id = job_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_EXPORT_JOB_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_export_job"))?;
            tbl.get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "get agent_export_job"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn upsert_import_identity(
        &self,
        record: &AgentImportIdentityRecord,
    ) -> StorageResult<()> {
        validate_import_identity_record(record)?;
        let rec = record.clone();
        self.write_tx("upsert_import_identity", move |wtx| {
            // RB-06c AC1: Enforce same-transaction agent_import_tombstone rejection
            ensure_not_tombstoned_by_provider(
                wtx,
                rec.agent_kind.as_str(),
                rec.provider_session_id.as_str(),
                "agent_import_identity",
            )?;

            let mut tbl = wtx
                .open_table(AGENT_IMPORT_IDENTITY_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_import_identity"))?;
            let mut existing_self: Option<AgentImportIdentityRecord> = None;
            for entry in tbl
                .iter()
                .map_err(|e| map_redb_storage_err(e, "scan agent_import_identity"))?
            {
                let (k, v) =
                    entry.map_err(|e| map_redb_storage_err(e, "iter agent_import_identity"))?;
                let row: AgentImportIdentityRecord = dec(v.value())?;
                if k.value() == rec.identity_id {
                    existing_self = Some(row);
                } else if row.agent_kind == rec.agent_kind
                    && row.provider_session_id == rec.provider_session_id
                    && row.source_kind == rec.source_kind
                    && row.source_id == rec.source_id
                    && row.schema_version == rec.schema_version
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_import_identity unique constraint violated".to_string(),
                    });
                }
            }
            if let Some(old) = &existing_self {
                validate_scope_immutability(
                    old.scope_state,
                    old.repo_id.as_deref(),
                    old.worktree_id.as_deref(),
                    old.workspace_id.as_deref(),
                    old.workspace_fence,
                    rec.scope_state,
                    rec.repo_id.as_deref(),
                    rec.worktree_id.as_deref(),
                    rec.workspace_id.as_deref(),
                    rec.workspace_fence,
                    "agent_import_identity",
                )?;
            }
            let bytes = enc(&rec)?;
            tbl.insert(rec.identity_id.as_str(), bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert agent_import_identity"))?;
            Ok(())
        })
        .await
    }

    async fn get_import_identity(
        &self,
        identity_id: &str,
    ) -> StorageResult<Option<AgentImportIdentityRecord>> {
        validate_non_empty_no_nul(identity_id, "identity_id")?;
        let id = identity_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_IMPORT_IDENTITY_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_import_identity"))?;
            tbl.get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "get agent_import_identity"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn put_import_tombstone(&self, record: &AgentImportTombstoneRecord) -> StorageResult<()> {
        validate_import_tombstone_record(record)?;
        let rec = record.clone();
        self.write_tx("put_import_tombstone", move |wtx| {
            let mut tbl = wtx
                .open_table(AGENT_IMPORT_TOMBSTONE_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_import_tombstone"))?;
            let mut by_provider = wtx
                .open_table(IMPORT_TOMBSTONES_BY_PROVIDER_TABLE)
                .map_err(|e| map_redb_table_err(e, "open import_tombstones_by_provider"))?;
            let mut by_session = wtx
                .open_table(IMPORT_TOMBSTONES_BY_SESSION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open import_tombstones_by_session"))?;

            let existing_self = tbl
                .get(rec.tombstone_id.as_str())
                .map_err(|e| map_redb_storage_err(e, "read existing import tombstone"))?
                .map(|g| dec::<AgentImportTombstoneRecord>(g.value()))
                .transpose()?;

            let provider_key = (rec.agent_kind.as_str(), rec.provider_session_id.as_str());
            if let Some(owner_raw) = by_provider
                .get(provider_key)
                .map_err(|e| map_redb_storage_err(e, "lookup import_tombstones_by_provider"))?
            {
                let owner: TombstoneIndexEntry = dec(owner_raw.value())?;
                if owner.tombstone_id != rec.tombstone_id {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_import_tombstone unique constraint violated".to_string(),
                    });
                }
            }
            if let Some(owner_raw) = by_session
                .get(rec.erased_session_id.as_str())
                .map_err(|e| map_redb_storage_err(e, "lookup import_tombstones_by_session"))?
            {
                let owner: TombstoneIndexEntry = dec(owner_raw.value())?;
                if owner.tombstone_id != rec.tombstone_id {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_import_tombstone unique constraint violated".to_string(),
                    });
                }
            }

            if let Some(old) = existing_self {
                if old.agent_kind != rec.agent_kind
                    || old.provider_session_id != rec.provider_session_id
                {
                    by_provider
                        .remove((old.agent_kind.as_str(), old.provider_session_id.as_str()))
                        .map_err(|e| {
                            map_redb_storage_err(e, "prune stale import_tombstones_by_provider")
                        })?;
                }
                if old.erased_session_id != rec.erased_session_id {
                    by_session
                        .remove(old.erased_session_id.as_str())
                        .map_err(|e| {
                            map_redb_storage_err(e, "prune stale import_tombstones_by_session")
                        })?;
                }
            }

            let bytes = enc(&rec)?;
            tbl.insert(rec.tombstone_id.as_str(), bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert agent_import_tombstone"))?;
            let idx_bytes = enc(&TombstoneIndexEntry {
                tombstone_id: rec.tombstone_id.clone(),
            })?;
            by_provider
                .insert(provider_key, idx_bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert import_tombstones_by_provider"))?;
            by_session
                .insert(rec.erased_session_id.as_str(), idx_bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert import_tombstones_by_session"))?;
            Ok(())
        })
        .await
    }

    async fn get_import_tombstone(
        &self,
        tombstone_id: &str,
    ) -> StorageResult<Option<AgentImportTombstoneRecord>> {
        validate_non_empty_no_nul(tombstone_id, "tombstone_id")?;
        let id = tombstone_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_IMPORT_TOMBSTONE_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_import_tombstone"))?;
            tbl.get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "get agent_import_tombstone"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn list_import_tombstones(&self) -> StorageResult<Vec<AgentImportTombstoneRecord>> {
        self.read_tx(|rtx| {
            let tbl = rtx
                .open_table(AGENT_IMPORT_TOMBSTONE_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_import_tombstone"))?;
            let mut out = Vec::new();
            for entry in tbl
                .iter()
                .map_err(|e| map_redb_storage_err(e, "iter agent_import_tombstone"))?
            {
                let (_, v) =
                    entry.map_err(|e| map_redb_storage_err(e, "read agent_import_tombstone"))?;
                out.push(dec(v.value())?);
            }
            Ok(out)
        })
        .await
    }

    async fn put_session_commit(&self, record: &AgentSessionCommitRecord) -> StorageResult<()> {
        validate_session_commit_record(record)?;
        let rec = record.clone();
        self.write_tx("put_session_commit", move |wtx| {
            put_session_commit_in_wtx(wtx, &rec)
        })
        .await
    }

    async fn list_session_commits(
        &self,
        session_id: &str,
    ) -> StorageResult<Vec<AgentSessionCommitRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        let sid = session_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_SESSION_COMMIT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_session_commit"))?;
            let mut out = Vec::new();
            for entry in tbl
                .range((sid.as_str(), "", "")..)
                .map_err(|e| map_redb_storage_err(e, "range agent_session_commit"))?
            {
                let (k, _) =
                    entry.map_err(|e| map_redb_storage_err(e, "iter agent_session_commit"))?;
                let (s, hk, oid) = k.value();
                if s != sid.as_str() {
                    break;
                }
                out.push(AgentSessionCommitRecord {
                    session_id: s.to_string(),
                    hash_kind: CaptureHashKind::parse(hk)?,
                    commit_oid: oid.to_string(),
                });
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_subagent_content_claim(
        &self,
        record: &AgentSubagentContentClaimRecord,
    ) -> StorageResult<()> {
        validate_subagent_content_claim(record)?;
        let rec = record.clone();
        self.write_tx("upsert_subagent_content_claim", move |wtx| {
            {
                let sessions = wtx
                    .open_table(AGENT_SESSION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_session"))?;
                if sessions
                    .get(rec.parent_session_id.as_str())
                    .map_err(|e| map_redb_storage_err(e, "check subagent_content_claim FK"))?
                    .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_subagent_content_claim foreign key violation".to_string(),
                    });
                }
            }
            let mut tbl = wtx
                .open_table(AGENT_SUBAGENT_CONTENT_CLAIM_TABLE)
                .map_err(|e| map_redb_table_err(e, "open subagent_content_claim"))?;
            let bytes = enc(&rec)?;
            tbl.insert(
                (
                    rec.parent_session_id.as_str(),
                    rec.provider_kind.as_str(),
                    rec.source_key.as_str(),
                    rec.content_schema_version,
                ),
                bytes.as_slice(),
            )
            .map_err(|e| map_redb_storage_err(e, "insert subagent_content_claim"))?;
            Ok(())
        })
        .await
    }

    async fn get_subagent_content_claim(
        &self,
        parent_session_id: &str,
        provider_kind: &str,
        source_key: &str,
        content_schema_version: i64,
    ) -> StorageResult<Option<AgentSubagentContentClaimRecord>> {
        validate_non_empty_no_nul(parent_session_id, "parent_session_id")?;
        validate_non_empty_no_nul(provider_kind, "provider_kind")?;
        validate_non_empty_no_nul(source_key, "source_key")?;
        let psid = parent_session_id.to_string();
        let pk = provider_kind.to_string();
        let sk = source_key.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_SUBAGENT_CONTENT_CLAIM_TABLE)
                .map_err(|e| map_redb_table_err(e, "open subagent_content_claim"))?;
            tbl.get((
                psid.as_str(),
                pk.as_str(),
                sk.as_str(),
                content_schema_version,
            ))
            .map_err(|e| map_redb_storage_err(e, "get subagent_content_claim"))?
            .map(|g| dec(g.value()))
            .transpose()
        })
        .await
    }

    async fn put_subagent_content_revision(
        &self,
        record: &AgentSubagentContentRevisionRecord,
    ) -> StorageResult<()> {
        validate_subagent_content_revision(record)?;
        let rec = record.clone();
        self.write_tx("put_subagent_content_revision", move |wtx| {
            {
                let claims = wtx
                    .open_table(AGENT_SUBAGENT_CONTENT_CLAIM_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open subagent_content_claim"))?;
                if claims
                    .get((
                        rec.parent_session_id.as_str(),
                        rec.provider_kind.as_str(),
                        rec.source_key.as_str(),
                        rec.content_schema_version,
                    ))
                    .map_err(|e| map_redb_storage_err(e, "check subagent_revision claim FK"))?
                    .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "agent_subagent_content_revision foreign key violation: claim not found"
                                .to_string(),
                    });
                }
            }
            {
                let cps = wtx
                    .open_table(AGENT_CHECKPOINT_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_checkpoint"))?;
                if cps
                    .get(rec.checkpoint_id.as_str())
                    .map_err(|e| map_redb_storage_err(e, "check subagent_revision checkpoint FK"))?
                    .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_subagent_content_revision foreign key violation: checkpoint not found"
                            .to_string(),
                    });
                }
            }
            let mut tbl = wtx
                .open_table(AGENT_SUBAGENT_CONTENT_REVISION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open subagent_content_revision"))?;
            let target_key = (
                rec.parent_session_id.as_str(),
                rec.provider_kind.as_str(),
                rec.source_key.as_str(),
                rec.content_schema_version,
                rec.revision,
            );
            // Enforce UNIQUE(checkpoint_id)
            for entry in tbl
                .iter()
                .map_err(|e| map_redb_storage_err(e, "scan subagent_content_revision"))?
            {
                let (k, v) =
                    entry.map_err(|e| map_redb_storage_err(e, "iter subagent_content_revision"))?;
                if k.value() == target_key {
                    continue;
                }
                let existing: AgentSubagentContentRevisionRecord = dec(v.value())?;
                if existing.checkpoint_id == rec.checkpoint_id {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "agent_subagent_content_revision unique constraint violated on checkpoint_id"
                                .to_string(),
                    });
                }
            }
            let bytes = enc(&rec)?;
            tbl.insert(target_key, bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert subagent_content_revision"))?;
            Ok(())
        })
        .await
    }

    async fn list_subagent_content_revisions(
        &self,
        parent_session_id: &str,
        provider_kind: &str,
        source_key: &str,
        content_schema_version: i64,
    ) -> StorageResult<Vec<AgentSubagentContentRevisionRecord>> {
        validate_non_empty_no_nul(parent_session_id, "parent_session_id")?;
        validate_non_empty_no_nul(provider_kind, "provider_kind")?;
        validate_non_empty_no_nul(source_key, "source_key")?;
        let psid = parent_session_id.to_string();
        let pk = provider_kind.to_string();
        let sk = source_key.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_SUBAGENT_CONTENT_REVISION_TABLE)
                .map_err(|e| map_redb_table_err(e, "open subagent_content_revision"))?;
            let mut out = Vec::new();
            for entry in tbl
                .range(
                    (
                        psid.as_str(),
                        pk.as_str(),
                        sk.as_str(),
                        content_schema_version,
                        i64::MIN,
                    )
                        ..=(
                            psid.as_str(),
                            pk.as_str(),
                            sk.as_str(),
                            content_schema_version,
                            i64::MAX,
                        ),
                )
                .map_err(|e| map_redb_storage_err(e, "range subagent_content_revision"))?
            {
                let (_, v) =
                    entry.map_err(|e| map_redb_storage_err(e, "iter subagent_content_revision"))?;
                out.push(dec(v.value())?);
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_subagent_link(&self, record: &AgentSubagentLinkRecord) -> StorageResult<()> {
        validate_subagent_link_record(record)?;
        let rec = record.clone();
        self.write_tx("upsert_subagent_link", move |wtx| {
            {
                let sessions = wtx
                    .open_table(AGENT_SESSION_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_session"))?;
                if sessions
                    .get(rec.parent_session_id.as_str())
                    .map_err(|e| map_redb_storage_err(e, "check subagent_link session FK"))?
                    .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "agent_subagent_link foreign key violation: session not found"
                            .to_string(),
                    });
                }
            }
            {
                let cps = wtx
                    .open_table(AGENT_CHECKPOINT_TABLE)
                    .map_err(|e| map_redb_table_err(e, "open agent_checkpoint"))?;
                if cps
                    .get(rec.content_checkpoint_id.as_str())
                    .map_err(|e| map_redb_storage_err(e, "check subagent_link content cp FK"))?
                    .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "agent_subagent_link foreign key violation: content checkpoint not found"
                                .to_string(),
                    });
                }
                if let Some(boundary_id) = &rec.boundary_checkpoint_id
                    && cps
                        .get(boundary_id.as_str())
                        .map_err(|e| map_redb_storage_err(e, "check subagent_link boundary cp FK"))?
                        .is_none()
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail:
                            "agent_subagent_link foreign key violation: boundary checkpoint not found"
                                .to_string(),
                    });
                }
            }
            let mut tbl = wtx
                .open_table(AGENT_SUBAGENT_LINK_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_subagent_link"))?;
            let bytes = enc(&rec)?;
            tbl.insert(rec.content_checkpoint_id.as_str(), bytes.as_slice())
                .map_err(|e| map_redb_storage_err(e, "insert agent_subagent_link"))?;
            Ok(())
        })
        .await
    }

    async fn get_subagent_link(
        &self,
        content_checkpoint_id: &str,
    ) -> StorageResult<Option<AgentSubagentLinkRecord>> {
        validate_non_empty_no_nul(content_checkpoint_id, "content_checkpoint_id")?;
        let id = content_checkpoint_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_SUBAGENT_LINK_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_subagent_link"))?;
            tbl.get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "get agent_subagent_link"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn put_usage_stats(&self, record: &AgentUsageStatsRecord) -> StorageResult<()> {
        validate_usage_stats_record(record)?;
        let rec = record.clone();
        self.write_tx("put_usage_stats", move |wtx| {
            put_usage_stats_in_wtx(wtx, &rec)
        })
        .await
    }

    async fn get_usage_stats(&self, id: &str) -> StorageResult<Option<AgentUsageStatsRecord>> {
        validate_non_empty_no_nul(id, "id")?;
        let id = id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_USAGE_STATS_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_usage_stats"))?;
            tbl.get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "get agent_usage_stats"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn list_usage_stats_by_session(
        &self,
        session_id: &str,
    ) -> StorageResult<Vec<AgentUsageStatsRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        let sid = session_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_USAGE_STATS_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_usage_stats"))?;
            let mut out = Vec::new();
            for entry in tbl
                .iter()
                .map_err(|e| map_redb_storage_err(e, "iter agent_usage_stats"))?
            {
                let (_, v) =
                    entry.map_err(|e| map_redb_storage_err(e, "read agent_usage_stats"))?;
                let row: AgentUsageStatsRecord = dec(v.value())?;
                if row.session_id.as_deref() == Some(sid.as_str()) {
                    out.push(row);
                }
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.id.cmp(&b.id))
            });
            Ok(out)
        })
        .await
    }

    async fn append_workspace_scope_audit(
        &self,
        record: &AgentWorkspaceScopeAuditRecord,
    ) -> StorageResult<()> {
        validate_workspace_scope_audit_record(record)?;
        let rec = record.clone();
        self.write_tx("append_workspace_scope_audit", move |wtx| {
            append_workspace_scope_audit_in_wtx(wtx, &rec)
        })
        .await
    }

    async fn get_workspace_scope_audit(
        &self,
        audit_id: &str,
    ) -> StorageResult<Option<AgentWorkspaceScopeAuditRecord>> {
        validate_non_empty_no_nul(audit_id, "audit_id")?;
        let id = audit_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_WORKSPACE_SCOPE_AUDIT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_workspace_scope_audit"))?;
            tbl.get(id.as_str())
                .map_err(|e| map_redb_storage_err(e, "get agent_workspace_scope_audit"))?
                .map(|g| dec(g.value()))
                .transpose()
        })
        .await
    }

    async fn list_workspace_scope_audits(
        &self,
        agent_kind: &str,
        provider_session_id: &str,
    ) -> StorageResult<Vec<AgentWorkspaceScopeAuditRecord>> {
        validate_non_empty_no_nul(agent_kind, "agent_kind")?;
        validate_non_empty_no_nul(provider_session_id, "provider_session_id")?;
        let kind = agent_kind.to_string();
        let psid = provider_session_id.to_string();
        self.read_tx(move |rtx| {
            let tbl = rtx
                .open_table(AGENT_WORKSPACE_SCOPE_AUDIT_TABLE)
                .map_err(|e| map_redb_table_err(e, "open agent_workspace_scope_audit"))?;
            let mut out = Vec::new();
            for entry in tbl
                .iter()
                .map_err(|e| map_redb_storage_err(e, "iter agent_workspace_scope_audit"))?
            {
                let (_, v) = entry
                    .map_err(|e| map_redb_storage_err(e, "read agent_workspace_scope_audit"))?;
                let row: AgentWorkspaceScopeAuditRecord = dec(v.value())?;
                if row.agent_kind == kind && row.provider_session_id == psid {
                    out.push(row);
                }
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.audit_id.cmp(&b.audit_id))
            });
            Ok(out)
        })
        .await
    }
}

#[cfg(test)]
mod create_guard_tests {
    use super::*;

    #[test]
    fn redb_create_publish_guard_rolls_back_on_drop_and_keeps_on_commit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp = dir.path().join("tmp.redb");
        let dest = dir.path().join("dest.redb");
        fs::write(&tmp, b"tmp").expect("write tmp");
        fs::hard_link(&tmp, &dest).expect("link dest to tmp");

        #[cfg(unix)]
        let make_guard =
            |published: bool, committed: bool, expected_inode: (libc::dev_t, libc::ino_t)| {
                let (dir_fd, _) =
                    open_dir_nofollow(dir.path(), UnixExpectedOwners::current_effective().dir_uid)
                        .expect("open temp dir");
                RedbCreatePublishGuard {
                    dir_fd,
                    tmp_name: CString::new("tmp.redb").expect("tmp cstr"),
                    dest_name: CString::new("dest.redb").expect("dest cstr"),
                    expected_inode,
                    published,
                    committed,
                }
            };
        #[cfg(not(unix))]
        let make_guard = |published: bool, committed: bool| RedbCreatePublishGuard {
            tmp_path: tmp.clone(),
            dest_path: dest.clone(),
            parent: dir.path().to_path_buf(),
            published,
            committed,
        };

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = fs::symlink_metadata(&tmp).expect("stat tmp");
            let inode = (meta.dev() as libc::dev_t, meta.ino() as libc::ino_t);
            {
                let guard = make_guard(true, false, inode);
                drop(guard);
            }
            assert!(!tmp.exists(), "uncommitted drop must remove temp file");
            assert!(
                !dest.exists(),
                "uncommitted drop after publish must remove freshly published destination"
            );

            // Replacement destination with a different inode must NEVER be deleted by rollback!
            fs::write(&dest, b"replacement-other-inode").expect("write replacement dest");
            {
                let guard = make_guard(true, false, inode);
                drop(guard);
            }
            assert!(
                dest.exists(),
                "inode-pinned rollback guard must preserve replacement destination"
            );
            assert_eq!(
                fs::read(&dest).expect("read replacement"),
                b"replacement-other-inode"
            );

            let dest_meta = fs::symlink_metadata(&dest).expect("stat dest");
            let dest_inode = (
                dest_meta.dev() as libc::dev_t,
                dest_meta.ino() as libc::ino_t,
            );
            {
                let guard = make_guard(true, true, dest_inode);
                drop(guard);
            }
            assert!(dest.exists(), "committed guard must preserve destination");
        }
        #[cfg(not(unix))]
        {
            {
                let guard = make_guard(true, false);
                drop(guard);
            }
            assert!(!tmp.exists(), "uncommitted drop must remove temp file");
            assert!(
                !dest.exists(),
                "uncommitted drop after publish must remove freshly published destination"
            );

            fs::write(&dest, b"dest-ok").expect("write dest-ok");
            {
                let guard = make_guard(true, true);
                drop(guard);
            }
            assert!(dest.exists(), "committed guard must preserve destination");
        }
    }

    #[tokio::test]
    async fn redb_create_handoff_drop_rolls_back_and_allows_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("capture.gen-000001.redb");
        let cancel = Arc::new(AtomicBool::new(false));

        // Simulate caller cancellation right at the spawn_blocking handoff:
        // worker produced Ok((db, uncommitted_guard)) and the async caller drops it.
        {
            let handoff = create_redb_capture_blocking(dest.clone(), Arc::clone(&cancel))
                .expect("blocking create succeeds");
            assert!(dest.exists(), "destination exists prior to handoff drop");
            drop(handoff);
        }

        assert!(
            !dest.exists(),
            "dropping uncommitted (Database, RedbCreatePublishGuard) handoff must remove destination"
        );
        // Subsequent create succeeds cleanly instead of failing with AlreadyExists
        let created = RedbCaptureStore::create(&dest)
            .await
            .expect("retry create after handoff cancellation must succeed");
        assert!(created.path().exists());
    }
}
