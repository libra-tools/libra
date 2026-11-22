//! Dual-backend Capture catalog and Agent Bridge storage abstractions (`RB-06`).
//!
//! Covers all **25** canonical Capture catalog source tables declared in
//! `sql/migrations/2026111601_capture_database.sql` (2 ledger/receipt tables +
//! 23 `agent_*` catalog tables), while explicitly segregating internal metadata
//! (`META_SCHEMA`, `META_GENERATION`), queue highwater ledgers
//! (`QUEUE_HIGHWATER_PRIORITY`, `QUEUE_HIGHWATER_NORMAL`), deduplication/sequence
//! bookkeeping tables, and same-transaction secondary indexes so they are never
//! conflated with the 25 source tables.
//!
//! Exposes two object-safe async traits:
//! - [`AgentBridgeStore`]: `agent_bridge_*` session/event/checkpoint/operation/link
//!   operations, including single-transaction [`AgentBridgeStore::append_and_ack_events`]
//!   keyed on `(bridge_session_id, event_seq)` that atomically advances contiguous
//!   `last_acked_seq`.
//! - [`CaptureStore`]: supertrait extending [`AgentBridgeStore`] across all 25
//!   Capture source tables plus single-transaction idempotent batch ingestion
//!   ([`CaptureStore::append_capture_batch`]) keyed on full UTF-8 semantic
//!   `dedup_key` with optional atomic `QUEUE_HIGHWATER_{PRIORITY,NORMAL}` advance.

use std::{collections::BTreeSet, fmt::Write as _};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::storage::{
    backend::StorageBackendKind,
    error::{StorageBackendError, StorageResult},
};

#[cfg(feature = "storage-redb")]
#[path = "redb/capture_store.rs"]
pub mod redb_capture_store;

#[cfg(feature = "storage-sqlite")]
#[path = "sqlite/capture_store.rs"]
pub mod sqlite_capture_store;

#[cfg(feature = "storage-redb")]
pub use redb_capture_store::RedbCaptureStore;
#[cfg(feature = "storage-sqlite")]
pub use sqlite_capture_store::SqliteCaptureStore;

/// Exact count of Capture source tables defined in
/// `sql/migrations/2026111601_capture_database.sql`.
pub const CAPTURE_SOURCE_TABLES_COUNT: usize = 25;

/// Authoritative manifest of all **25** Capture catalog source tables
/// (2 ledger/receipt tables + 23 `agent_*` catalog tables).
pub const CAPTURE_SOURCE_TABLES: [&str; CAPTURE_SOURCE_TABLES_COUNT] = [
    "schema_versions",
    "capture_catalog_copy_receipt",
    "agent_audit_log",
    "agent_bridge_checkpoint",
    "agent_bridge_event",
    "agent_bridge_link",
    "agent_bridge_operation",
    "agent_bridge_session",
    "agent_capture_cloud_base",
    "agent_capture_incarnation",
    "agent_checkpoint",
    "agent_checkpoint_prune_tombstone",
    "agent_coverage_claim",
    "agent_coverage_conflict",
    "agent_coverage_revision",
    "agent_export_job",
    "agent_import_identity",
    "agent_import_tombstone",
    "agent_session",
    "agent_session_commit",
    "agent_subagent_content_claim",
    "agent_subagent_content_revision",
    "agent_subagent_link",
    "agent_usage_stats",
    "agent_workspace_scope_audit",
];

/// Internal `META_*`, `QUEUE_HIGHWATER_*`, deduplication, and sequence tables
/// maintained in `capture.gen-<id>.redb`. Explicitly segregated from
/// [`CAPTURE_SOURCE_TABLES`].
pub const CAPTURE_INTERNAL_META_AND_QUEUE_TABLES: &[&str] = &[
    "meta_schema",
    "meta_generation",
    "queue_highwater_priority",
    "queue_highwater_normal",
    "capture_dedup_keys",
    "capture_sequences",
];

/// Same-transaction secondary index tables maintained in `capture.gen-<id>.redb`.
/// Explicitly segregated from [`CAPTURE_SOURCE_TABLES`].
pub const CAPTURE_SECONDARY_INDEX_TABLES: &[&str] = &[
    "sessions_by_time",
    "sessions_by_provider",
    "checkpoints_by_session",
    "events_by_time",
    "bridge_events_by_seq",
    "bridge_links_by_key",
    "usage_stats_by_session_event",
];

/// Maximum UTF-8 byte length of a single bridge event payload (`256 KiB`,
/// matching `agent_bridge_event` `CHECK(length(payload) <= 262144)` and
/// `MAX_EVENT_PAYLOAD_BYTES`).
pub const MAX_BRIDGE_EVENT_PAYLOAD_BYTES: usize = 256 * 1024;

/// Maximum UTF-8 byte length of a semantic capture queue `dedup_key` (`ADR-RB-05` #2).
pub const MAX_CAPTURE_DEDUP_KEY_BYTES: usize = 512;

/// Fixed `source` discriminator enforced on `agent_bridge_session` (`ADR-LB-02`).
pub const BRIDGE_SOURCE_DEEPSEEK_HARNESS: &str = "deepseek-harness";

/// Fixed `action` discriminator enforced on `agent_workspace_scope_audit`.
pub const WORKSPACE_SCOPE_AUDIT_ACTION: &str = "adopt_legacy_capture_scope";

/// Expected character/byte length of `agent_capture_incarnation.source_namespace`.
pub const CAPTURE_SOURCE_NAMESPACE_LEN: usize = 32;

/// Parse every `CREATE TABLE` name declared in `CAPTURE_DATABASE_SQL`.
#[must_use]
pub fn capture_source_tables_from_sql() -> BTreeSet<String> {
    let mut tables = BTreeSet::new();
    for line in crate::internal::db::schema::CAPTURE_DATABASE_SQL.lines() {
        let trimmed = line.trim();
        let rest = trimmed
            .strip_prefix("CREATE TABLE IF NOT EXISTS ")
            .or_else(|| trimmed.strip_prefix("CREATE TABLE "));
        let Some(rest) = rest else {
            continue;
        };
        let name = rest
            .trim_start_matches('`')
            .trim_start_matches('"')
            .split(['`', '"', ' ', '('])
            .next()
            .unwrap_or("");
        if !name.is_empty() {
            tables.insert(name.to_owned());
        }
    }
    tables
}

/// Union of all non-source internal/metadata/queue/index tables registered for
/// `capture.gen-<id>.redb`.
#[must_use]
pub fn capture_declared_non_source_tables() -> BTreeSet<&'static str> {
    CAPTURE_INTERNAL_META_AND_QUEUE_TABLES
        .iter()
        .chain(CAPTURE_SECONDARY_INDEX_TABLES.iter())
        .copied()
        .collect()
}

/// Invert a signed `i64` timestamp into an order-reversing `u64` key so
/// ascending B-tree iteration yields `timestamp DESC` order.
#[must_use]
pub const fn invert_capture_timestamp(ts: i64) -> u64 {
    u64::MAX - ((ts as u64) ^ (1u64 << 63))
}

/// Compute lowercase 64-char SHA-256 hex digest matching `agent_bridge/storage.rs`.
#[must_use]
pub fn capture_sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

// ============================================================================
// Queue batch & highwater types (AC3, AC4, ADR-RB-05 #4)
// ============================================================================

/// Capture queue lane tracked by `QUEUE_HIGHWATER_PRIORITY` and
/// `QUEUE_HIGHWATER_NORMAL` (`ADR-RB-05`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureQueueLane {
    Priority,
    Normal,
}

impl CaptureQueueLane {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Normal => "normal",
        }
    }
}

/// Monotonic `(segment_id, byte_offset)` cursor stored in
/// `QUEUE_HIGHWATER_{PRIORITY,NORMAL}`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct CaptureQueueHighwater {
    pub segment_id: u32,
    pub byte_offset: u64,
}

/// Domain payload variants applied atomically inside [`CaptureStore::append_capture_batch`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CaptureBatchPayload {
    UpsertSession(AgentSessionRecord),
    UpsertCheckpoint(AgentCheckpointRecord),
    UpsertSessionCommit(AgentSessionCommitRecord),
    InsertUsageStats(AgentUsageStatsRecord),
    AppendAuditLog(AgentAuditLogRecord),
    AppendWorkspaceScopeAudit(AgentWorkspaceScopeAuditRecord),
}

/// Single event frame submitted to [`CaptureStore::append_capture_batch`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureBatchItem {
    /// Full UTF-8 semantic deduplication key (`1..=512` bytes, no NUL).
    pub dedup_key: String,
    /// Domain payload applied once for this `dedup_key`.
    pub payload: CaptureBatchPayload,
}

/// Result returned by [`CaptureStore::append_capture_batch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureBatchOutcome {
    /// Number of items newly persisted in this write transaction.
    pub applied_count: usize,
    /// Number of items skipped because their `dedup_key` was already present.
    pub deduplicated_count: usize,
    /// Resulting authoritative queue highwater if a lane advance was requested.
    pub highwater: Option<CaptureQueueHighwater>,
}

// ============================================================================
// Domain enums & records for all 25 Capture source tables
// ============================================================================

/// Row in `schema_versions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureSchemaVersionRecord {
    pub version: i64,
    pub name: String,
    pub applied_at: String,
}

/// Row in `capture_catalog_copy_receipt` (`CHECK (id = 1)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureCatalogCopyReceiptRecord {
    pub id: i64,
    pub copied_at: String,
}

/// Row in `agent_audit_log`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAuditLogRecord {
    pub audit_id: String,
    pub timestamp: String,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    pub action: String,
    pub checkpoint_id: String,
    pub scope: String,
    pub export_path: Option<String>,
    pub justification: Option<String>,
    pub granted: bool,
}

/// Allowed values of `agent_bridge_session.session_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BridgeSessionState {
    Open,
    Flushing,
    Closed,
    Quarantined,
}

impl BridgeSessionState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Flushing => "flushing",
            Self::Closed => "closed",
            Self::Quarantined => "quarantined",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "open" => Ok(Self::Open),
            "flushing" => Ok(Self::Flushing),
            "closed" => Ok(Self::Closed),
            "quarantined" => Ok(Self::Quarantined),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_bridge_session.session_state value".to_string(),
            }),
        }
    }
}

/// Row in `agent_bridge_session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBridgeSessionRecord {
    pub bridge_session_id: String,
    pub source: String,
    pub repository_id: String,
    pub worktree_id: Option<String>,
    pub workspace_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub agent_id: Option<String>,
    pub session_state: BridgeSessionState,
    pub schema_version: i64,
    pub last_event_seq: i64,
    pub last_acked_seq: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Input parameters for [`AgentBridgeStore::open_bridge_session`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeSessionOpenInput {
    pub bridge_session_id: String,
    pub repository_id: String,
    pub worktree_id: Option<String>,
    pub workspace_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub agent_id: Option<String>,
    pub now_ms: i64,
}

/// Row in `agent_bridge_checkpoint`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBridgeCheckpointRecord {
    pub checkpoint_id: String,
    pub bridge_session_id: String,
    pub agent_checkpoint_id: Option<String>,
    pub target_oid: Option<String>,
    pub created_at: i64,
}

/// Row in `agent_bridge_event`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBridgeEventRecord {
    pub event_id: i64,
    pub bridge_session_id: String,
    pub event_seq: i64,
    pub event_type: String,
    pub payload_sha256: String,
    pub payload: String,
    pub operation_id: Option<String>,
    pub created_at: i64,
}

/// Event input submitted to [`AgentBridgeStore::append_and_ack_events`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeEventAppendInput {
    pub event_seq: i64,
    pub event_type: String,
    pub payload: String,
    pub operation_id: Option<String>,
}

/// Per-event outcome returned by [`AgentBridgeStore::append_and_ack_events`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BridgeEventStatus {
    Accepted,
    Duplicate,
    DigestConflict,
    Rejected,
}

/// Atomic batch outcome returned by [`AgentBridgeStore::append_and_ack_events`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeEventAppendOutcome {
    pub per_event: Vec<BridgeEventStatus>,
    pub last_event_seq: i64,
    pub last_acked_seq: i64,
}

/// Allowed values of `agent_bridge_link.source_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BridgeLinkSourceType {
    Event,
    Operation,
    Checkpoint,
    Evidence,
    Provenance,
    Commit,
    Restore,
    Review,
}

impl BridgeLinkSourceType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Operation => "operation",
            Self::Checkpoint => "checkpoint",
            Self::Evidence => "evidence",
            Self::Provenance => "provenance",
            Self::Commit => "commit",
            Self::Restore => "restore",
            Self::Review => "review",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "event" => Ok(Self::Event),
            "operation" => Ok(Self::Operation),
            "checkpoint" => Ok(Self::Checkpoint),
            "evidence" => Ok(Self::Evidence),
            "provenance" => Ok(Self::Provenance),
            "commit" => Ok(Self::Commit),
            "restore" => Ok(Self::Restore),
            "review" => Ok(Self::Review),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_bridge_link.source_type value".to_string(),
            }),
        }
    }
}

/// Input for [`AgentBridgeStore::insert_bridge_link`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeLinkWrite {
    pub bridge_session_id: String,
    pub source_type: BridgeLinkSourceType,
    pub source_id: String,
    pub target_type: String,
    pub target_id: String,
    pub created_at: i64,
}

/// Row in `agent_bridge_link`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBridgeLinkRecord {
    pub link_id: i64,
    pub bridge_session_id: String,
    pub source_type: BridgeLinkSourceType,
    pub source_id: String,
    pub target_type: String,
    pub target_id: String,
    pub created_at: i64,
}

/// Allowed values of `agent_bridge_operation.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BridgeOperationStatus {
    Pending,
    Applied,
    Failed,
    Quarantined,
}

impl BridgeOperationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Failed => "failed",
            Self::Quarantined => "quarantined",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "applied" => Ok(Self::Applied),
            "failed" => Ok(Self::Failed),
            "quarantined" => Ok(Self::Quarantined),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_bridge_operation.status value".to_string(),
            }),
        }
    }
}

/// Row in `agent_bridge_operation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBridgeOperationRecord {
    pub operation_id: String,
    pub bridge_session_id: String,
    pub method: String,
    pub params_digest: String,
    pub repository_id: String,
    pub workspace_id: Option<String>,
    pub status: BridgeOperationStatus,
    pub result_digest: Option<String>,
    pub created_at: i64,
    pub completed_at: Option<i64>,
}

/// Row in `agent_capture_cloud_base`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCaptureCloudBaseRecord {
    pub repo_id: String,
    pub remote_generation: i64,
    pub updated_at: i64,
}

/// Row in `agent_capture_incarnation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCaptureIncarnationRecord {
    pub agent_kind: String,
    pub provider_session_id: String,
    pub next_session_sync_revision: i64,
    pub source_namespace: String,
    pub updated_at: i64,
}

/// Allowed values of `agent_session.agent_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureAgentKind {
    ClaudeCode,
    Cursor,
    Codex,
    Gemini,
    Opencode,
    Copilot,
    FactoryAi,
    Grok,
    Hermes,
    Pi,
}

impl CaptureAgentKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Opencode => "opencode",
            Self::Copilot => "copilot",
            Self::FactoryAi => "factory_ai",
            Self::Grok => "grok",
            Self::Hermes => "hermes",
            Self::Pi => "pi",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "claude_code" => Ok(Self::ClaudeCode),
            "cursor" => Ok(Self::Cursor),
            "codex" => Ok(Self::Codex),
            "gemini" => Ok(Self::Gemini),
            "opencode" => Ok(Self::Opencode),
            "copilot" => Ok(Self::Copilot),
            "factory_ai" => Ok(Self::FactoryAi),
            "grok" => Ok(Self::Grok),
            "hermes" => Ok(Self::Hermes),
            "pi" => Ok(Self::Pi),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_session.agent_kind value".to_string(),
            }),
        }
    }
}

/// Allowed values of `agent_session.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureSessionState {
    Pending,
    Active,
    Condensed,
    Stopped,
    Quarantined,
}

impl CaptureSessionState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Condensed => "condensed",
            Self::Stopped => "stopped",
            Self::Quarantined => "quarantined",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "active" => Ok(Self::Active),
            "condensed" => Ok(Self::Condensed),
            "stopped" => Ok(Self::Stopped),
            "quarantined" => Ok(Self::Quarantined),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_session.state value".to_string(),
            }),
        }
    }
}

/// Allowed values of `scope_state` on `agent_session`, `agent_export_job`, and
/// `agent_import_identity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureScopeState {
    LegacyUnknown,
    Scoped,
}

impl CaptureScopeState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LegacyUnknown => "legacy_unknown",
            Self::Scoped => "scoped",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "legacy_unknown" => Ok(Self::LegacyUnknown),
            "scoped" => Ok(Self::Scoped),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid capture scope_state value".to_string(),
            }),
        }
    }
}

/// Row in `agent_session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionRecord {
    pub session_id: String,
    pub agent_kind: CaptureAgentKind,
    pub provider_session_id: String,
    pub thread_id: Option<String>,
    pub state: CaptureSessionState,
    pub working_dir: String,
    pub worktree_id: Option<String>,
    pub parent_commit: Option<String>,
    pub parent_session_id: Option<String>,
    pub metadata_json: String,
    pub redaction_report: String,
    pub started_at: i64,
    pub last_event_at: i64,
    pub stopped_at: Option<i64>,
    pub schema_version: i64,
    pub sync_revision: i64,
    pub repo_id: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_fence: Option<i64>,
    pub scope_state: CaptureScopeState,
}

/// Allowed values of `agent_checkpoint.scope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureCheckpointScope {
    Temporary,
    Committed,
    Subagent,
}

impl CaptureCheckpointScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Temporary => "temporary",
            Self::Committed => "committed",
            Self::Subagent => "subagent",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "temporary" => Ok(Self::Temporary),
            "committed" => Ok(Self::Committed),
            "subagent" => Ok(Self::Subagent),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_checkpoint.scope value".to_string(),
            }),
        }
    }
}

/// Row in `agent_checkpoint`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCheckpointRecord {
    pub checkpoint_id: String,
    pub session_id: String,
    pub parent_checkpoint_id: Option<String>,
    pub scope: CaptureCheckpointScope,
    pub parent_commit: Option<String>,
    pub tree_oid: String,
    pub metadata_blob_oid: String,
    pub traces_commit: String,
    pub tool_use_id: Option<String>,
    pub subagent_session_id: Option<String>,
    pub description: Option<String>,
    pub created_at: i64,
    pub sync_revision: i64,
}

/// Row in `agent_checkpoint_prune_tombstone`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCheckpointPruneTombstoneRecord {
    pub checkpoint_id: String,
    pub session_id: String,
    pub pruned_at: i64,
}

/// Allowed values of `completeness` on coverage tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureCompleteness {
    Incomplete,
    Complete,
}

impl CaptureCompleteness {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Incomplete => "incomplete",
            Self::Complete => "complete",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "incomplete" => Ok(Self::Incomplete),
            "complete" => Ok(Self::Complete),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid coverage completeness value".to_string(),
            }),
        }
    }
}

/// Allowed values of `agent_coverage_claim.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureCoverageClaimState {
    ReservedLive,
    ReservedImport,
    CatalogCommitted,
    Abandoned,
    Conflicted,
}

impl CaptureCoverageClaimState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReservedLive => "reserved_live",
            Self::ReservedImport => "reserved_import",
            Self::CatalogCommitted => "catalog_committed",
            Self::Abandoned => "abandoned",
            Self::Conflicted => "conflicted",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "reserved_live" => Ok(Self::ReservedLive),
            "reserved_import" => Ok(Self::ReservedImport),
            "catalog_committed" => Ok(Self::CatalogCommitted),
            "abandoned" => Ok(Self::Abandoned),
            "conflicted" => Ok(Self::Conflicted),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_coverage_claim.state value".to_string(),
            }),
        }
    }
}

/// Allowed values of `source_channel` on coverage tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureSourceChannel {
    Live,
    Import,
    Export,
}

impl CaptureSourceChannel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Import => "import",
            Self::Export => "export",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "live" => Ok(Self::Live),
            "import" => Ok(Self::Import),
            "export" => Ok(Self::Export),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid coverage source_channel value".to_string(),
            }),
        }
    }
}

/// Row in `agent_coverage_claim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCoverageClaimRecord {
    pub session_id: String,
    pub logical_turn_key: String,
    pub coverage_schema_version: i64,
    pub coverage_digest: String,
    pub completeness: CaptureCompleteness,
    pub revision: i64,
    pub state: CaptureCoverageClaimState,
    pub attempt_checkpoint_id: Option<String>,
    pub owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub fence_token: Option<i64>,
    pub checkpoint_id: Option<String>,
    pub traces_commit: Option<String>,
    pub source_channel: CaptureSourceChannel,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Row in `agent_coverage_conflict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCoverageConflictRecord {
    pub session_id: String,
    pub logical_turn_key: String,
    pub coverage_schema_version: i64,
    pub incumbent_revision: i64,
    pub incumbent_digest: String,
    pub incumbent_checkpoint_id: Option<String>,
    pub incoming_digest: String,
    pub incoming_source_channel: CaptureSourceChannel,
    pub incoming_observed_at: i64,
    pub incoming_canonical_json: String,
    pub incoming_redaction_report_json: String,
}

/// Row in `agent_coverage_revision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCoverageRevisionRecord {
    pub session_id: String,
    pub logical_turn_key: String,
    pub coverage_schema_version: i64,
    pub revision: i64,
    pub checkpoint_id: String,
    pub coverage_digest: String,
    pub completeness: CaptureCompleteness,
    pub source_channel: CaptureSourceChannel,
    pub created_at: i64,
}

/// Allowed values of `agent_export_job.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureExportJobState {
    Idle,
    Inflight,
    Dirty,
    Failed,
}

impl CaptureExportJobState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Inflight => "inflight",
            Self::Dirty => "dirty",
            Self::Failed => "failed",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "idle" => Ok(Self::Idle),
            "inflight" => Ok(Self::Inflight),
            "dirty" => Ok(Self::Dirty),
            "failed" => Ok(Self::Failed),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_export_job.state value".to_string(),
            }),
        }
    }
}

/// Row in `agent_export_job`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentExportJobRecord {
    pub job_id: String,
    pub agent_kind: String,
    pub provider_session_id: String,
    pub owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub fence_token: Option<i64>,
    pub observed_generation: i64,
    pub processed_generation: i64,
    pub state: CaptureExportJobState,
    pub last_error_code: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub ttl_expires_at: i64,
    pub repo_id: Option<String>,
    pub worktree_id: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_fence: Option<i64>,
    pub scope_state: CaptureScopeState,
}

/// Allowed values of `agent_import_identity.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureImportIdentityState {
    Discovered,
    Leased,
    Writing,
    Partial,
    Committed,
    Failed,
}

impl CaptureImportIdentityState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::Leased => "leased",
            Self::Writing => "writing",
            Self::Partial => "partial",
            Self::Committed => "committed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "discovered" => Ok(Self::Discovered),
            "leased" => Ok(Self::Leased),
            "writing" => Ok(Self::Writing),
            "partial" => Ok(Self::Partial),
            "committed" => Ok(Self::Committed),
            "failed" => Ok(Self::Failed),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_import_identity.state value".to_string(),
            }),
        }
    }
}

/// Row in `agent_import_identity`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentImportIdentityRecord {
    pub identity_id: String,
    pub agent_kind: String,
    pub provider_session_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub schema_version: i64,
    pub observed_digest: Option<String>,
    pub committed_digest: Option<String>,
    pub attempt_id: Option<String>,
    pub attempt_checkpoint_id: Option<String>,
    pub next_ordinal: i64,
    pub state: CaptureImportIdentityState,
    pub owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub fence_token: Option<i64>,
    pub last_error_code: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub repo_id: Option<String>,
    pub worktree_id: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_fence: Option<i64>,
    pub scope_state: CaptureScopeState,
}

/// Row in `agent_import_tombstone`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentImportTombstoneRecord {
    pub tombstone_id: String,
    pub agent_kind: String,
    pub provider_session_id: String,
    pub erased_session_id: String,
    pub source_fingerprint: Option<String>,
    pub erased_at: i64,
}

/// Allowed values of `agent_session_commit.hash_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CaptureHashKind {
    Sha1,
    Sha256,
    Blake3,
}

impl CaptureHashKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Blake3 => "blake3",
        }
    }

    #[must_use]
    pub const fn expected_hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 | Self::Blake3 => 64,
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "sha1" => Ok(Self::Sha1),
            "sha256" => Ok(Self::Sha256),
            "blake3" => Ok(Self::Blake3),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_session_commit.hash_kind value".to_string(),
            }),
        }
    }
}

/// Row in `agent_session_commit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionCommitRecord {
    pub session_id: String,
    pub hash_kind: CaptureHashKind,
    pub commit_oid: String,
}

/// Allowed values of `agent_subagent_content_claim.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureSubagentClaimState {
    Idle,
    Reserved,
}

impl CaptureSubagentClaimState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Reserved => "reserved",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "idle" => Ok(Self::Idle),
            "reserved" => Ok(Self::Reserved),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_subagent_content_claim.state value".to_string(),
            }),
        }
    }
}

/// Row in `agent_subagent_content_claim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSubagentContentClaimRecord {
    pub parent_session_id: String,
    pub provider_kind: String,
    pub source_key: String,
    pub content_schema_version: i64,
    pub current_revision: i64,
    pub current_checkpoint_id: Option<String>,
    pub current_digest: Option<String>,
    pub state: CaptureSubagentClaimState,
    pub attempt_digest: Option<String>,
    pub attempt_checkpoint_id: Option<String>,
    pub owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub fence_token: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub revision_cursor: i64,
    pub sync_revision: i64,
}

/// Allowed values of `agent_subagent_content_revision.source_channel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureSubagentSourceChannel {
    Live,
    Import,
}

impl CaptureSubagentSourceChannel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Import => "import",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "live" => Ok(Self::Live),
            "import" => Ok(Self::Import),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_subagent_content_revision.source_channel value".to_string(),
            }),
        }
    }
}

/// Row in `agent_subagent_content_revision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSubagentContentRevisionRecord {
    pub parent_session_id: String,
    pub provider_kind: String,
    pub source_key: String,
    pub content_schema_version: i64,
    pub revision: i64,
    pub checkpoint_id: String,
    pub content_digest: String,
    pub source_channel: CaptureSubagentSourceChannel,
    pub partial: bool,
    pub created_at: i64,
}

/// Allowed values of `agent_subagent_link.link_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureSubagentLinkState {
    Resolved,
    Unresolved,
}

impl CaptureSubagentLinkState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Unresolved => "unresolved",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "resolved" => Ok(Self::Resolved),
            "unresolved" => Ok(Self::Unresolved),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_subagent_link.link_state value".to_string(),
            }),
        }
    }
}

/// Row in `agent_subagent_link`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSubagentLinkRecord {
    pub content_checkpoint_id: String,
    pub parent_session_id: String,
    pub link_state: CaptureSubagentLinkState,
    pub boundary_checkpoint_id: Option<String>,
    pub stable_subagent_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub sync_revision: i64,
}

/// Row in `agent_usage_stats`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentUsageStatsRecord {
    pub id: String,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub run_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub request_kind: String,
    pub intent: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
    pub tool_call_count: i64,
    pub wall_clock_ms: i64,
    pub provider_latency_ms: Option<i64>,
    pub cost_estimate_micro_dollars: Option<i64>,
    pub cost_usd: Option<f64>,
    pub usage_estimated: bool,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub success: bool,
    pub error_kind: Option<String>,
    pub schema_version: i64,
    pub created_at: String,
    pub agent_name: Option<String>,
    pub repo_id: Option<String>,
    pub turn_id: Option<String>,
    pub event_id: Option<String>,
}

/// Row in `agent_workspace_scope_audit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkspaceScopeAuditRecord {
    pub audit_id: String,
    pub action: String,
    pub agent_kind: String,
    pub provider_session_id: String,
    pub repo_id: String,
    pub worktree_id: String,
    pub workspace_id: Option<String>,
    pub workspace_fence: Option<i64>,
    pub actor: Option<String>,
    pub created_at: i64,
}

// ============================================================================
// Validation helpers shared across SQLite and redb backends
// ============================================================================

pub(crate) fn validate_non_empty_no_nul(value: &str, field: &'static str) -> StorageResult<()> {
    if value.is_empty() || value.contains('\0') {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("invalid capture field `{field}`: must be non-empty without NUL"),
        });
    }
    Ok(())
}

pub(crate) fn validate_no_nul(value: &str, field: &'static str) -> StorageResult<()> {
    if value.contains('\0') {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("invalid capture field `{field}`: NUL byte is not allowed"),
        });
    }
    Ok(())
}

pub(crate) fn validate_opt_no_nul(value: Option<&str>, field: &'static str) -> StorageResult<()> {
    if let Some(s) = value {
        validate_no_nul(s, field)?;
    }
    Ok(())
}

pub(crate) fn validate_capture_dedup_key(dedup_key: &str) -> StorageResult<()> {
    if dedup_key.is_empty()
        || dedup_key.len() > MAX_CAPTURE_DEDUP_KEY_BYTES
        || dedup_key.contains('\0')
    {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "invalid capture dedup_key: must be 1..={MAX_CAPTURE_DEDUP_KEY_BYTES} UTF-8 bytes without NUL"
            ),
        });
    }
    Ok(())
}

pub(crate) fn validate_scoped_fields(
    scope_state: CaptureScopeState,
    repo_id: Option<&str>,
    worktree_id: Option<&str>,
    workspace_id: Option<&str>,
    workspace_fence: Option<i64>,
    table: &'static str,
) -> StorageResult<()> {
    validate_opt_no_nul(repo_id, "repo_id")?;
    validate_opt_no_nul(worktree_id, "worktree_id")?;
    validate_opt_no_nul(workspace_id, "workspace_id")?;
    if scope_state == CaptureScopeState::Scoped {
        let missing_repo = repo_id.is_none_or(str::is_empty);
        let missing_wt = worktree_id.is_none();
        let bad_ws_without_id = workspace_id.is_none() && workspace_fence.is_some();
        let bad_ws_with_id =
            workspace_id.is_some_and(|ws| ws.is_empty() || workspace_fence.is_none());
        if missing_repo || missing_wt || bad_ws_without_id || bad_ws_with_id {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "scoped {table} record is missing repository, worktree, or workspace fence"
                ),
            });
        }
    }
    Ok(())
}

#[cfg(feature = "storage-redb")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_scope_immutability(
    old_scope_state: CaptureScopeState,
    old_repo_id: Option<&str>,
    old_worktree_id: Option<&str>,
    old_workspace_id: Option<&str>,
    old_workspace_fence: Option<i64>,
    new_scope_state: CaptureScopeState,
    new_repo_id: Option<&str>,
    new_worktree_id: Option<&str>,
    new_workspace_id: Option<&str>,
    new_workspace_fence: Option<i64>,
    table: &'static str,
) -> StorageResult<()> {
    if old_scope_state == CaptureScopeState::Scoped
        && (new_scope_state != old_scope_state
            || new_repo_id != old_repo_id
            || new_worktree_id != old_worktree_id
            || new_workspace_id != old_workspace_id
            || new_workspace_fence != old_workspace_fence)
    {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("scoped {table} ownership is immutable"),
        });
    }
    Ok(())
}

pub(crate) fn validate_schema_version_record(
    rec: &CaptureSchemaVersionRecord,
) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.name, "schema_versions.name")?;
    validate_non_empty_no_nul(&rec.applied_at, "schema_versions.applied_at")?;
    Ok(())
}

pub(crate) fn validate_copy_receipt_record(
    rec: &CaptureCatalogCopyReceiptRecord,
) -> StorageResult<()> {
    if rec.id != 1 {
        return Err(StorageBackendError::BackendInternal {
            detail: "capture_catalog_copy_receipt.id must equal 1".to_string(),
        });
    }
    validate_non_empty_no_nul(&rec.copied_at, "capture_catalog_copy_receipt.copied_at")?;
    Ok(())
}

pub(crate) fn validate_audit_log_record(rec: &AgentAuditLogRecord) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.audit_id, "agent_audit_log.audit_id")?;
    validate_non_empty_no_nul(&rec.timestamp, "agent_audit_log.timestamp")?;
    validate_opt_no_nul(rec.user_id.as_deref(), "agent_audit_log.user_id")?;
    validate_opt_no_nul(rec.user_name.as_deref(), "agent_audit_log.user_name")?;
    validate_non_empty_no_nul(&rec.action, "agent_audit_log.action")?;
    validate_non_empty_no_nul(&rec.checkpoint_id, "agent_audit_log.checkpoint_id")?;
    validate_non_empty_no_nul(&rec.scope, "agent_audit_log.scope")?;
    validate_opt_no_nul(rec.export_path.as_deref(), "agent_audit_log.export_path")?;
    validate_opt_no_nul(
        rec.justification.as_deref(),
        "agent_audit_log.justification",
    )?;
    Ok(())
}

pub(crate) fn validate_bridge_session_open(input: &BridgeSessionOpenInput) -> StorageResult<()> {
    validate_non_empty_no_nul(&input.bridge_session_id, "bridge_session_id")?;
    validate_non_empty_no_nul(&input.repository_id, "repository_id")?;
    validate_opt_no_nul(input.worktree_id.as_deref(), "worktree_id")?;
    validate_opt_no_nul(input.workspace_id.as_deref(), "workspace_id")?;
    validate_opt_no_nul(input.parent_session_id.as_deref(), "parent_session_id")?;
    validate_opt_no_nul(input.agent_id.as_deref(), "agent_id")?;
    Ok(())
}

pub(crate) fn validate_bridge_checkpoint_record(
    rec: &AgentBridgeCheckpointRecord,
) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.checkpoint_id, "agent_bridge_checkpoint.checkpoint_id")?;
    validate_non_empty_no_nul(
        &rec.bridge_session_id,
        "agent_bridge_checkpoint.bridge_session_id",
    )?;
    validate_opt_no_nul(
        rec.agent_checkpoint_id.as_deref(),
        "agent_bridge_checkpoint.agent_checkpoint_id",
    )?;
    validate_opt_no_nul(
        rec.target_oid.as_deref(),
        "agent_bridge_checkpoint.target_oid",
    )?;
    Ok(())
}

pub(crate) fn validate_bridge_link_write(link: &BridgeLinkWrite) -> StorageResult<()> {
    validate_non_empty_no_nul(
        &link.bridge_session_id,
        "agent_bridge_link.bridge_session_id",
    )?;
    validate_non_empty_no_nul(&link.source_id, "agent_bridge_link.source_id")?;
    validate_non_empty_no_nul(&link.target_type, "agent_bridge_link.target_type")?;
    validate_non_empty_no_nul(&link.target_id, "agent_bridge_link.target_id")?;
    Ok(())
}

pub(crate) fn validate_bridge_operation_record(
    rec: &AgentBridgeOperationRecord,
) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.operation_id, "agent_bridge_operation.operation_id")?;
    validate_non_empty_no_nul(
        &rec.bridge_session_id,
        "agent_bridge_operation.bridge_session_id",
    )?;
    validate_non_empty_no_nul(&rec.method, "agent_bridge_operation.method")?;
    validate_non_empty_no_nul(&rec.params_digest, "agent_bridge_operation.params_digest")?;
    validate_non_empty_no_nul(&rec.repository_id, "agent_bridge_operation.repository_id")?;
    validate_opt_no_nul(
        rec.workspace_id.as_deref(),
        "agent_bridge_operation.workspace_id",
    )?;
    validate_opt_no_nul(
        rec.result_digest.as_deref(),
        "agent_bridge_operation.result_digest",
    )?;
    Ok(())
}

pub(crate) fn validate_cloud_base_record(rec: &AgentCaptureCloudBaseRecord) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.repo_id, "agent_capture_cloud_base.repo_id")?;
    if rec.remote_generation <= 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: "agent_capture_cloud_base.remote_generation must be > 0".to_string(),
        });
    }
    Ok(())
}

pub(crate) fn validate_incarnation_record(
    rec: &AgentCaptureIncarnationRecord,
) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.agent_kind, "agent_capture_incarnation.agent_kind")?;
    validate_non_empty_no_nul(
        &rec.provider_session_id,
        "agent_capture_incarnation.provider_session_id",
    )?;
    if rec.next_session_sync_revision <= 1 {
        return Err(StorageBackendError::BackendInternal {
            detail: "agent_capture_incarnation.next_session_sync_revision must be > 1".to_string(),
        });
    }
    validate_no_nul(
        &rec.source_namespace,
        "agent_capture_incarnation.source_namespace",
    )?;
    if rec.source_namespace.chars().count() != CAPTURE_SOURCE_NAMESPACE_LEN {
        return Err(StorageBackendError::BackendInternal {
            detail: "agent_capture_incarnation.source_namespace must have length 32".to_string(),
        });
    }
    Ok(())
}

pub(crate) fn validate_agent_session_record(rec: &AgentSessionRecord) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.session_id, "agent_session.session_id")?;
    validate_non_empty_no_nul(
        &rec.provider_session_id,
        "agent_session.provider_session_id",
    )?;
    validate_opt_no_nul(rec.thread_id.as_deref(), "agent_session.thread_id")?;
    validate_no_nul(&rec.working_dir, "agent_session.working_dir")?;
    validate_opt_no_nul(rec.parent_commit.as_deref(), "agent_session.parent_commit")?;
    validate_opt_no_nul(
        rec.parent_session_id.as_deref(),
        "agent_session.parent_session_id",
    )?;
    validate_no_nul(&rec.metadata_json, "agent_session.metadata_json")?;
    validate_no_nul(&rec.redaction_report, "agent_session.redaction_report")?;
    validate_scoped_fields(
        rec.scope_state,
        rec.repo_id.as_deref(),
        rec.worktree_id.as_deref(),
        rec.workspace_id.as_deref(),
        rec.workspace_fence,
        "agent_session",
    )?;
    Ok(())
}

pub(crate) fn validate_agent_checkpoint_record(rec: &AgentCheckpointRecord) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.checkpoint_id, "agent_checkpoint.checkpoint_id")?;
    validate_non_empty_no_nul(&rec.session_id, "agent_checkpoint.session_id")?;
    validate_opt_no_nul(
        rec.parent_checkpoint_id.as_deref(),
        "agent_checkpoint.parent_checkpoint_id",
    )?;
    validate_opt_no_nul(
        rec.parent_commit.as_deref(),
        "agent_checkpoint.parent_commit",
    )?;
    validate_non_empty_no_nul(&rec.tree_oid, "agent_checkpoint.tree_oid")?;
    validate_non_empty_no_nul(&rec.metadata_blob_oid, "agent_checkpoint.metadata_blob_oid")?;
    validate_non_empty_no_nul(&rec.traces_commit, "agent_checkpoint.traces_commit")?;
    validate_opt_no_nul(rec.tool_use_id.as_deref(), "agent_checkpoint.tool_use_id")?;
    validate_opt_no_nul(
        rec.subagent_session_id.as_deref(),
        "agent_checkpoint.subagent_session_id",
    )?;
    validate_opt_no_nul(rec.description.as_deref(), "agent_checkpoint.description")?;
    Ok(())
}

pub(crate) fn validate_checkpoint_prune_tombstone(
    rec: &AgentCheckpointPruneTombstoneRecord,
) -> StorageResult<()> {
    validate_non_empty_no_nul(
        &rec.checkpoint_id,
        "agent_checkpoint_prune_tombstone.checkpoint_id",
    )?;
    validate_non_empty_no_nul(
        &rec.session_id,
        "agent_checkpoint_prune_tombstone.session_id",
    )?;
    Ok(())
}

pub(crate) fn validate_coverage_claim_record(rec: &AgentCoverageClaimRecord) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.session_id, "agent_coverage_claim.session_id")?;
    validate_non_empty_no_nul(
        &rec.logical_turn_key,
        "agent_coverage_claim.logical_turn_key",
    )?;
    validate_non_empty_no_nul(&rec.coverage_digest, "agent_coverage_claim.coverage_digest")?;
    validate_opt_no_nul(
        rec.attempt_checkpoint_id.as_deref(),
        "agent_coverage_claim.attempt_checkpoint_id",
    )?;
    validate_opt_no_nul(rec.owner.as_deref(), "agent_coverage_claim.owner")?;
    validate_opt_no_nul(
        rec.checkpoint_id.as_deref(),
        "agent_coverage_claim.checkpoint_id",
    )?;
    validate_opt_no_nul(
        rec.traces_commit.as_deref(),
        "agent_coverage_claim.traces_commit",
    )?;
    Ok(())
}

pub(crate) fn validate_coverage_conflict_record(
    rec: &AgentCoverageConflictRecord,
) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.session_id, "agent_coverage_conflict.session_id")?;
    validate_non_empty_no_nul(
        &rec.logical_turn_key,
        "agent_coverage_conflict.logical_turn_key",
    )?;
    validate_non_empty_no_nul(
        &rec.incumbent_digest,
        "agent_coverage_conflict.incumbent_digest",
    )?;
    validate_opt_no_nul(
        rec.incumbent_checkpoint_id.as_deref(),
        "agent_coverage_conflict.incumbent_checkpoint_id",
    )?;
    validate_non_empty_no_nul(
        &rec.incoming_digest,
        "agent_coverage_conflict.incoming_digest",
    )?;
    validate_no_nul(
        &rec.incoming_canonical_json,
        "agent_coverage_conflict.incoming_canonical_json",
    )?;
    validate_no_nul(
        &rec.incoming_redaction_report_json,
        "agent_coverage_conflict.incoming_redaction_report_json",
    )?;
    Ok(())
}

pub(crate) fn validate_coverage_revision_record(
    rec: &AgentCoverageRevisionRecord,
) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.session_id, "agent_coverage_revision.session_id")?;
    validate_non_empty_no_nul(
        &rec.logical_turn_key,
        "agent_coverage_revision.logical_turn_key",
    )?;
    validate_non_empty_no_nul(&rec.checkpoint_id, "agent_coverage_revision.checkpoint_id")?;
    validate_non_empty_no_nul(
        &rec.coverage_digest,
        "agent_coverage_revision.coverage_digest",
    )?;
    Ok(())
}

pub(crate) fn validate_export_job_record(rec: &AgentExportJobRecord) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.job_id, "agent_export_job.job_id")?;
    validate_non_empty_no_nul(&rec.agent_kind, "agent_export_job.agent_kind")?;
    validate_non_empty_no_nul(
        &rec.provider_session_id,
        "agent_export_job.provider_session_id",
    )?;
    validate_opt_no_nul(rec.owner.as_deref(), "agent_export_job.owner")?;
    validate_opt_no_nul(
        rec.last_error_code.as_deref(),
        "agent_export_job.last_error_code",
    )?;
    validate_scoped_fields(
        rec.scope_state,
        rec.repo_id.as_deref(),
        rec.worktree_id.as_deref(),
        rec.workspace_id.as_deref(),
        rec.workspace_fence,
        "agent_export_job",
    )?;
    Ok(())
}

pub(crate) fn validate_import_identity_record(
    rec: &AgentImportIdentityRecord,
) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.identity_id, "agent_import_identity.identity_id")?;
    validate_non_empty_no_nul(&rec.agent_kind, "agent_import_identity.agent_kind")?;
    validate_non_empty_no_nul(
        &rec.provider_session_id,
        "agent_import_identity.provider_session_id",
    )?;
    validate_non_empty_no_nul(&rec.source_kind, "agent_import_identity.source_kind")?;
    validate_non_empty_no_nul(&rec.source_id, "agent_import_identity.source_id")?;
    validate_opt_no_nul(
        rec.observed_digest.as_deref(),
        "agent_import_identity.observed_digest",
    )?;
    validate_opt_no_nul(
        rec.committed_digest.as_deref(),
        "agent_import_identity.committed_digest",
    )?;
    validate_opt_no_nul(
        rec.attempt_id.as_deref(),
        "agent_import_identity.attempt_id",
    )?;
    validate_opt_no_nul(
        rec.attempt_checkpoint_id.as_deref(),
        "agent_import_identity.attempt_checkpoint_id",
    )?;
    validate_opt_no_nul(rec.owner.as_deref(), "agent_import_identity.owner")?;
    validate_opt_no_nul(
        rec.last_error_code.as_deref(),
        "agent_import_identity.last_error_code",
    )?;
    validate_scoped_fields(
        rec.scope_state,
        rec.repo_id.as_deref(),
        rec.worktree_id.as_deref(),
        rec.workspace_id.as_deref(),
        rec.workspace_fence,
        "agent_import_identity",
    )?;
    Ok(())
}

pub(crate) fn validate_import_tombstone_record(
    rec: &AgentImportTombstoneRecord,
) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.tombstone_id, "agent_import_tombstone.tombstone_id")?;
    validate_non_empty_no_nul(&rec.agent_kind, "agent_import_tombstone.agent_kind")?;
    validate_non_empty_no_nul(
        &rec.provider_session_id,
        "agent_import_tombstone.provider_session_id",
    )?;
    validate_non_empty_no_nul(
        &rec.erased_session_id,
        "agent_import_tombstone.erased_session_id",
    )?;
    validate_opt_no_nul(
        rec.source_fingerprint.as_deref(),
        "agent_import_tombstone.source_fingerprint",
    )?;
    Ok(())
}

pub(crate) fn validate_session_commit_record(rec: &AgentSessionCommitRecord) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.session_id, "agent_session_commit.session_id")?;
    let expected_len = rec.hash_kind.expected_hex_len();
    if rec.commit_oid.len() != expected_len
        || !rec
            .commit_oid
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "invalid agent_session_commit.commit_oid for {}: expected {expected_len} lowercase hex chars",
                rec.hash_kind.as_str()
            ),
        });
    }
    Ok(())
}

pub(crate) fn validate_subagent_content_claim(
    rec: &AgentSubagentContentClaimRecord,
) -> StorageResult<()> {
    validate_non_empty_no_nul(
        &rec.parent_session_id,
        "agent_subagent_content_claim.parent_session_id",
    )?;
    validate_non_empty_no_nul(
        &rec.provider_kind,
        "agent_subagent_content_claim.provider_kind",
    )?;
    validate_non_empty_no_nul(&rec.source_key, "agent_subagent_content_claim.source_key")?;
    validate_opt_no_nul(
        rec.current_checkpoint_id.as_deref(),
        "agent_subagent_content_claim.current_checkpoint_id",
    )?;
    validate_opt_no_nul(
        rec.current_digest.as_deref(),
        "agent_subagent_content_claim.current_digest",
    )?;
    validate_opt_no_nul(
        rec.attempt_digest.as_deref(),
        "agent_subagent_content_claim.attempt_digest",
    )?;
    validate_opt_no_nul(
        rec.attempt_checkpoint_id.as_deref(),
        "agent_subagent_content_claim.attempt_checkpoint_id",
    )?;
    validate_opt_no_nul(rec.owner.as_deref(), "agent_subagent_content_claim.owner")?;

    let rev_ok = (rec.current_revision == 0
        && rec.current_checkpoint_id.is_none()
        && rec.current_digest.is_none())
        || (rec.current_revision > 0
            && rec.current_checkpoint_id.is_some()
            && rec.current_digest.is_some());
    if !rev_ok {
        return Err(StorageBackendError::BackendInternal {
            detail: "agent_subagent_content_claim violates current_revision invariant".to_string(),
        });
    }

    let state_ok = match rec.state {
        CaptureSubagentClaimState::Idle => {
            rec.attempt_digest.is_none()
                && rec.attempt_checkpoint_id.is_none()
                && rec.owner.is_none()
                && rec.lease_expires_at.is_none()
        }
        CaptureSubagentClaimState::Reserved => {
            rec.attempt_digest.is_some() && rec.owner.is_some() && rec.lease_expires_at.is_some()
        }
    };
    if !state_ok {
        return Err(StorageBackendError::BackendInternal {
            detail: "agent_subagent_content_claim violates state lease invariant".to_string(),
        });
    }
    Ok(())
}

pub(crate) fn validate_subagent_content_revision(
    rec: &AgentSubagentContentRevisionRecord,
) -> StorageResult<()> {
    validate_non_empty_no_nul(
        &rec.parent_session_id,
        "agent_subagent_content_revision.parent_session_id",
    )?;
    validate_non_empty_no_nul(
        &rec.provider_kind,
        "agent_subagent_content_revision.provider_kind",
    )?;
    validate_non_empty_no_nul(
        &rec.source_key,
        "agent_subagent_content_revision.source_key",
    )?;
    validate_non_empty_no_nul(
        &rec.checkpoint_id,
        "agent_subagent_content_revision.checkpoint_id",
    )?;
    validate_non_empty_no_nul(
        &rec.content_digest,
        "agent_subagent_content_revision.content_digest",
    )?;
    Ok(())
}

pub(crate) fn validate_subagent_link_record(rec: &AgentSubagentLinkRecord) -> StorageResult<()> {
    validate_non_empty_no_nul(
        &rec.content_checkpoint_id,
        "agent_subagent_link.content_checkpoint_id",
    )?;
    validate_non_empty_no_nul(
        &rec.parent_session_id,
        "agent_subagent_link.parent_session_id",
    )?;
    validate_opt_no_nul(
        rec.boundary_checkpoint_id.as_deref(),
        "agent_subagent_link.boundary_checkpoint_id",
    )?;
    validate_opt_no_nul(
        rec.stable_subagent_id.as_deref(),
        "agent_subagent_link.stable_subagent_id",
    )?;
    let link_ok = match rec.link_state {
        CaptureSubagentLinkState::Resolved => {
            rec.boundary_checkpoint_id.is_some() && rec.stable_subagent_id.is_some()
        }
        CaptureSubagentLinkState::Unresolved => rec.boundary_checkpoint_id.is_none(),
    };
    if !link_ok {
        return Err(StorageBackendError::BackendInternal {
            detail: "agent_subagent_link violates link_state/boundary invariant".to_string(),
        });
    }
    Ok(())
}

pub(crate) fn validate_usage_stats_record(rec: &AgentUsageStatsRecord) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.id, "agent_usage_stats.id")?;
    validate_opt_no_nul(rec.session_id.as_deref(), "agent_usage_stats.session_id")?;
    validate_opt_no_nul(rec.thread_id.as_deref(), "agent_usage_stats.thread_id")?;
    validate_opt_no_nul(
        rec.agent_run_id.as_deref(),
        "agent_usage_stats.agent_run_id",
    )?;
    validate_opt_no_nul(rec.run_id.as_deref(), "agent_usage_stats.run_id")?;
    validate_non_empty_no_nul(&rec.provider, "agent_usage_stats.provider")?;
    validate_non_empty_no_nul(&rec.model, "agent_usage_stats.model")?;
    validate_non_empty_no_nul(&rec.request_kind, "agent_usage_stats.request_kind")?;
    validate_opt_no_nul(rec.intent.as_deref(), "agent_usage_stats.intent")?;
    if let Some(cost) = rec.cost_usd
        && !cost.is_finite()
    {
        return Err(StorageBackendError::BackendInternal {
            detail: "agent_usage_stats.cost_usd must be finite".to_string(),
        });
    }
    validate_opt_no_nul(rec.started_at.as_deref(), "agent_usage_stats.started_at")?;
    validate_opt_no_nul(rec.finished_at.as_deref(), "agent_usage_stats.finished_at")?;
    validate_opt_no_nul(rec.error_kind.as_deref(), "agent_usage_stats.error_kind")?;
    validate_non_empty_no_nul(&rec.created_at, "agent_usage_stats.created_at")?;
    validate_opt_no_nul(rec.agent_name.as_deref(), "agent_usage_stats.agent_name")?;
    validate_opt_no_nul(rec.repo_id.as_deref(), "agent_usage_stats.repo_id")?;
    validate_opt_no_nul(rec.turn_id.as_deref(), "agent_usage_stats.turn_id")?;
    validate_opt_no_nul(rec.event_id.as_deref(), "agent_usage_stats.event_id")?;
    Ok(())
}

pub(crate) fn validate_workspace_scope_audit_record(
    rec: &AgentWorkspaceScopeAuditRecord,
) -> StorageResult<()> {
    validate_non_empty_no_nul(&rec.audit_id, "agent_workspace_scope_audit.audit_id")?;
    if rec.action != WORKSPACE_SCOPE_AUDIT_ACTION {
        return Err(StorageBackendError::BackendInternal {
            detail: "agent_workspace_scope_audit.action must be `adopt_legacy_capture_scope`"
                .to_string(),
        });
    }
    validate_non_empty_no_nul(&rec.agent_kind, "agent_workspace_scope_audit.agent_kind")?;
    validate_non_empty_no_nul(
        &rec.provider_session_id,
        "agent_workspace_scope_audit.provider_session_id",
    )?;
    validate_non_empty_no_nul(&rec.repo_id, "agent_workspace_scope_audit.repo_id")?;
    validate_no_nul(&rec.worktree_id, "agent_workspace_scope_audit.worktree_id")?;
    validate_opt_no_nul(
        rec.workspace_id.as_deref(),
        "agent_workspace_scope_audit.workspace_id",
    )?;
    validate_opt_no_nul(rec.actor.as_deref(), "agent_workspace_scope_audit.actor")?;
    Ok(())
}

pub(crate) fn validate_capture_batch_item(item: &CaptureBatchItem) -> StorageResult<()> {
    validate_capture_dedup_key(&item.dedup_key)?;
    match &item.payload {
        CaptureBatchPayload::UpsertSession(rec) => validate_agent_session_record(rec),
        CaptureBatchPayload::UpsertCheckpoint(rec) => validate_agent_checkpoint_record(rec),
        CaptureBatchPayload::UpsertSessionCommit(rec) => validate_session_commit_record(rec),
        CaptureBatchPayload::InsertUsageStats(rec) => validate_usage_stats_record(rec),
        CaptureBatchPayload::AppendAuditLog(rec) => validate_audit_log_record(rec),
        CaptureBatchPayload::AppendWorkspaceScopeAudit(rec) => {
            validate_workspace_scope_audit_record(rec)
        }
    }
}

// ============================================================================
// Object-safe traits: AgentBridgeStore & CaptureStore
// ============================================================================

/// Object-safe storage abstraction for the 5 `agent_bridge_*` tables inside the
/// Capture database (`RB-06`).
#[async_trait]
pub trait AgentBridgeStore: Send + Sync {
    /// Open or idempotently fetch an `agent_bridge_session` row.
    async fn open_bridge_session(
        &self,
        input: &BridgeSessionOpenInput,
    ) -> StorageResult<(AgentBridgeSessionRecord, bool)>;

    /// Fetch an `agent_bridge_session` by primary key.
    async fn get_bridge_session(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Option<AgentBridgeSessionRecord>>;

    /// Update `session_state` and `updated_at` for an existing bridge session.
    async fn set_bridge_session_state(
        &self,
        bridge_session_id: &str,
        state: BridgeSessionState,
        updated_at: i64,
    ) -> StorageResult<()>;

    /// Delete a bridge session and cascade to its child checkpoints, events,
    /// links, and operations in one write transaction.
    async fn delete_bridge_session(&self, bridge_session_id: &str) -> StorageResult<bool>;

    /// Atomically append bridge events keyed by `(bridge_session_id, event_seq)`
    /// and advance `last_event_seq` + contiguous `last_acked_seq` in a SINGLE
    /// write transaction (`RB-06` AC5).
    async fn append_and_ack_events(
        &self,
        bridge_session_id: &str,
        events: &[BridgeEventAppendInput],
        now_ms: i64,
    ) -> StorageResult<BridgeEventAppendOutcome>;

    /// Fetch a single bridge event by composite key `(bridge_session_id, event_seq)`.
    async fn get_bridge_event_by_seq(
        &self,
        bridge_session_id: &str,
        event_seq: i64,
    ) -> StorageResult<Option<AgentBridgeEventRecord>>;

    /// List all bridge events for a session ordered by `event_seq ASC`.
    async fn list_bridge_events(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Vec<AgentBridgeEventRecord>>;

    /// List all bridge events across sessions ordered by
    /// `(created_at ASC, bridge_session_id ASC, event_seq ASC)` (`EVENTS_BY_TIME`).
    async fn list_bridge_events_by_time(&self) -> StorageResult<Vec<AgentBridgeEventRecord>>;

    /// Upsert an `agent_bridge_checkpoint` row.
    async fn put_bridge_checkpoint(
        &self,
        record: &AgentBridgeCheckpointRecord,
    ) -> StorageResult<()>;

    /// Fetch an `agent_bridge_checkpoint` by primary key.
    async fn get_bridge_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> StorageResult<Option<AgentBridgeCheckpointRecord>>;

    /// List bridge checkpoints for a session ordered by `(created_at ASC, checkpoint_id ASC)`.
    async fn list_bridge_checkpoints(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Vec<AgentBridgeCheckpointRecord>>;

    /// Upsert an `agent_bridge_operation` row.
    async fn put_bridge_operation(&self, record: &AgentBridgeOperationRecord) -> StorageResult<()>;

    /// Fetch an `agent_bridge_operation` by primary key.
    async fn get_bridge_operation(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<AgentBridgeOperationRecord>>;

    /// List bridge operations for a session ordered by `(created_at ASC, operation_id ASC)`.
    async fn list_bridge_operations(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Vec<AgentBridgeOperationRecord>>;

    /// Idempotently insert an `agent_bridge_link` row on
    /// `UNIQUE(source_type, source_id, target_type, target_id)`.
    async fn insert_bridge_link(
        &self,
        link: &BridgeLinkWrite,
    ) -> StorageResult<(AgentBridgeLinkRecord, bool)>;

    /// List bridge links for a session ordered by `link_id ASC`.
    async fn list_bridge_links(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Vec<AgentBridgeLinkRecord>>;
}

/// Object-safe storage abstraction covering all **25** Capture catalog source
/// tables plus single-transaction batch deduplication and queue highwater tracking
/// (`RB-06`, `ADR-RB-04`, `ADR-RB-05`).
#[async_trait]
pub trait CaptureStore: AgentBridgeStore + Send + Sync {
    /// Underlying concrete storage backend kind (`Sqlite` or `Redb`).
    fn backend_kind(&self) -> StorageBackendKind;

    /// Returns `true` when this handle is strictly read-only.
    fn is_read_only(&self) -> bool;

    // -- Single-tx batch ingestion + semantic dedup + queue highwater (AC3/AC4) --

    /// Atomically ingest `items` (including empty batches for sealed-segment
    /// highwater advance or batches of >= 128 events) inside a SINGLE write
    /// transaction, idempotently deduplicating on full semantic `dedup_key` and
    /// optionally advancing `QUEUE_HIGHWATER_{PRIORITY,NORMAL}` monotonically.
    async fn append_capture_batch(
        &self,
        items: &[CaptureBatchItem],
        highwater_advance: Option<(CaptureQueueLane, CaptureQueueHighwater)>,
    ) -> StorageResult<CaptureBatchOutcome>;

    /// Read the authoritative committed queue highwater for `lane`
    /// (`(0, 0)` when no batch has advanced it yet).
    async fn get_queue_highwater(
        &self,
        lane: CaptureQueueLane,
    ) -> StorageResult<CaptureQueueHighwater>;

    // -- Table 1: schema_versions --

    async fn put_schema_version(&self, record: &CaptureSchemaVersionRecord) -> StorageResult<()>;
    async fn list_schema_versions(&self) -> StorageResult<Vec<CaptureSchemaVersionRecord>>;

    // -- Table 2: capture_catalog_copy_receipt --

    async fn put_copy_receipt(&self, record: &CaptureCatalogCopyReceiptRecord)
    -> StorageResult<()>;
    async fn get_copy_receipt(&self) -> StorageResult<Option<CaptureCatalogCopyReceiptRecord>>;

    // -- Table 3: agent_audit_log --

    async fn append_audit_log(&self, record: &AgentAuditLogRecord) -> StorageResult<()>;
    async fn get_audit_log(&self, audit_id: &str) -> StorageResult<Option<AgentAuditLogRecord>>;
    async fn list_audit_logs_by_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> StorageResult<Vec<AgentAuditLogRecord>>;

    // -- Tables 4..8 are provided by the AgentBridgeStore supertrait --

    // -- Table 9: agent_capture_cloud_base --

    async fn put_cloud_base(&self, record: &AgentCaptureCloudBaseRecord) -> StorageResult<()>;
    async fn get_cloud_base(
        &self,
        repo_id: &str,
    ) -> StorageResult<Option<AgentCaptureCloudBaseRecord>>;

    // -- Table 10: agent_capture_incarnation --

    async fn put_incarnation(&self, record: &AgentCaptureIncarnationRecord) -> StorageResult<()>;
    async fn get_incarnation(
        &self,
        agent_kind: &str,
        provider_session_id: &str,
    ) -> StorageResult<Option<AgentCaptureIncarnationRecord>>;

    // -- Table 11: agent_session (plus SESSIONS_BY_TIME secondary index) --

    async fn upsert_session(&self, record: &AgentSessionRecord) -> StorageResult<()>;
    async fn get_session(&self, session_id: &str) -> StorageResult<Option<AgentSessionRecord>>;
    async fn list_sessions_by_started_desc(&self) -> StorageResult<Vec<AgentSessionRecord>>;
    async fn delete_session(&self, session_id: &str) -> StorageResult<bool>;

    // -- Table 12: agent_checkpoint (plus CHECKPOINTS_BY_SESSION secondary index) --

    async fn upsert_checkpoint(&self, record: &AgentCheckpointRecord) -> StorageResult<()>;
    async fn get_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> StorageResult<Option<AgentCheckpointRecord>>;
    async fn list_checkpoints_by_session(
        &self,
        session_id: &str,
    ) -> StorageResult<Vec<AgentCheckpointRecord>>;
    async fn delete_checkpoint(&self, checkpoint_id: &str) -> StorageResult<bool>;

    // -- Table 13: agent_checkpoint_prune_tombstone --

    async fn put_checkpoint_prune_tombstone(
        &self,
        record: &AgentCheckpointPruneTombstoneRecord,
    ) -> StorageResult<()>;
    async fn get_checkpoint_prune_tombstone(
        &self,
        checkpoint_id: &str,
    ) -> StorageResult<Option<AgentCheckpointPruneTombstoneRecord>>;

    // -- Table 14: agent_coverage_claim --

    async fn upsert_coverage_claim(&self, record: &AgentCoverageClaimRecord) -> StorageResult<()>;
    async fn get_coverage_claim(
        &self,
        session_id: &str,
        logical_turn_key: &str,
        coverage_schema_version: i64,
    ) -> StorageResult<Option<AgentCoverageClaimRecord>>;

    // -- Table 15: agent_coverage_conflict --

    async fn upsert_coverage_conflict(
        &self,
        record: &AgentCoverageConflictRecord,
    ) -> StorageResult<()>;
    async fn get_coverage_conflict(
        &self,
        session_id: &str,
        logical_turn_key: &str,
        coverage_schema_version: i64,
    ) -> StorageResult<Option<AgentCoverageConflictRecord>>;

    // -- Table 16: agent_coverage_revision --

    async fn put_coverage_revision(
        &self,
        record: &AgentCoverageRevisionRecord,
    ) -> StorageResult<()>;
    async fn list_coverage_revisions(
        &self,
        session_id: &str,
        logical_turn_key: &str,
        coverage_schema_version: i64,
    ) -> StorageResult<Vec<AgentCoverageRevisionRecord>>;

    // -- Table 17: agent_export_job --

    async fn upsert_export_job(&self, record: &AgentExportJobRecord) -> StorageResult<()>;
    async fn get_export_job(&self, job_id: &str) -> StorageResult<Option<AgentExportJobRecord>>;

    // -- Table 18: agent_import_identity --

    async fn upsert_import_identity(&self, record: &AgentImportIdentityRecord)
    -> StorageResult<()>;
    async fn get_import_identity(
        &self,
        identity_id: &str,
    ) -> StorageResult<Option<AgentImportIdentityRecord>>;

    // -- Table 19: agent_import_tombstone --

    async fn put_import_tombstone(&self, record: &AgentImportTombstoneRecord) -> StorageResult<()>;
    async fn get_import_tombstone(
        &self,
        tombstone_id: &str,
    ) -> StorageResult<Option<AgentImportTombstoneRecord>>;
    async fn list_import_tombstones(&self) -> StorageResult<Vec<AgentImportTombstoneRecord>>;

    // -- Table 20: agent_session_commit --

    async fn put_session_commit(&self, record: &AgentSessionCommitRecord) -> StorageResult<()>;
    async fn list_session_commits(
        &self,
        session_id: &str,
    ) -> StorageResult<Vec<AgentSessionCommitRecord>>;

    // -- Table 21: agent_subagent_content_claim --

    async fn upsert_subagent_content_claim(
        &self,
        record: &AgentSubagentContentClaimRecord,
    ) -> StorageResult<()>;
    async fn get_subagent_content_claim(
        &self,
        parent_session_id: &str,
        provider_kind: &str,
        source_key: &str,
        content_schema_version: i64,
    ) -> StorageResult<Option<AgentSubagentContentClaimRecord>>;

    // -- Table 22: agent_subagent_content_revision --

    async fn put_subagent_content_revision(
        &self,
        record: &AgentSubagentContentRevisionRecord,
    ) -> StorageResult<()>;
    async fn list_subagent_content_revisions(
        &self,
        parent_session_id: &str,
        provider_kind: &str,
        source_key: &str,
        content_schema_version: i64,
    ) -> StorageResult<Vec<AgentSubagentContentRevisionRecord>>;

    // -- Table 23: agent_subagent_link --

    async fn upsert_subagent_link(&self, record: &AgentSubagentLinkRecord) -> StorageResult<()>;
    async fn get_subagent_link(
        &self,
        content_checkpoint_id: &str,
    ) -> StorageResult<Option<AgentSubagentLinkRecord>>;

    // -- Table 24: agent_usage_stats --

    async fn put_usage_stats(&self, record: &AgentUsageStatsRecord) -> StorageResult<()>;
    async fn get_usage_stats(&self, id: &str) -> StorageResult<Option<AgentUsageStatsRecord>>;
    async fn list_usage_stats_by_session(
        &self,
        session_id: &str,
    ) -> StorageResult<Vec<AgentUsageStatsRecord>>;

    // -- Table 25: agent_workspace_scope_audit --

    async fn append_workspace_scope_audit(
        &self,
        record: &AgentWorkspaceScopeAuditRecord,
    ) -> StorageResult<()>;
    async fn get_workspace_scope_audit(
        &self,
        audit_id: &str,
    ) -> StorageResult<Option<AgentWorkspaceScopeAuditRecord>>;
    async fn list_workspace_scope_audits(
        &self,
        agent_kind: &str,
        provider_session_id: &str,
    ) -> StorageResult<Vec<AgentWorkspaceScopeAuditRecord>>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[allow(dead_code)]
    fn assert_traits_object_safe(
        _capture: Arc<dyn CaptureStore>,
        _bridge: Arc<dyn AgentBridgeStore>,
    ) {
    }

    fn sample_session(id: &str, provider_sid: &str, started_at: i64) -> AgentSessionRecord {
        AgentSessionRecord {
            session_id: id.to_string(),
            agent_kind: CaptureAgentKind::Codex,
            provider_session_id: provider_sid.to_string(),
            thread_id: Some("thread-001".to_string()),
            state: CaptureSessionState::Active,
            working_dir: "/repo/work".to_string(),
            worktree_id: Some("wt-main".to_string()),
            parent_commit: Some("a".repeat(40)),
            parent_session_id: None,
            metadata_json: r#"{"mode":"live"}"#.to_string(),
            redaction_report: r#"{"redacted":0}"#.to_string(),
            started_at,
            last_event_at: started_at + 100,
            stopped_at: None,
            schema_version: 1,
            sync_revision: 1,
            repo_id: Some("repo-rb06".to_string()),
            workspace_id: Some("ws-1".to_string()),
            workspace_fence: Some(7),
            scope_state: CaptureScopeState::Scoped,
        }
    }

    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    fn sample_checkpoint(id: &str, session_id: &str, created_at: i64) -> AgentCheckpointRecord {
        AgentCheckpointRecord {
            checkpoint_id: id.to_string(),
            session_id: session_id.to_string(),
            parent_checkpoint_id: None,
            scope: CaptureCheckpointScope::Committed,
            parent_commit: Some("a".repeat(40)),
            tree_oid: "b".repeat(40),
            metadata_blob_oid: "c".repeat(40),
            traces_commit: "d".repeat(40),
            tool_use_id: Some("tool-1".to_string()),
            subagent_session_id: None,
            description: Some("turn checkpoint".to_string()),
            created_at,
            sync_revision: 1,
        }
    }

    #[cfg(feature = "storage-redb")]
    fn sample_usage(id: &str, session_id: &str, event_id: &str) -> AgentUsageStatsRecord {
        AgentUsageStatsRecord {
            id: id.to_string(),
            session_id: Some(session_id.to_string()),
            thread_id: Some("thread-001".to_string()),
            agent_run_id: Some("arun-1".to_string()),
            run_id: Some("run-1".to_string()),
            provider: "openai".to_string(),
            model: "gpt-5.4".to_string(),
            request_kind: "completion".to_string(),
            intent: Some("code".to_string()),
            prompt_tokens: 120,
            completion_tokens: 45,
            cached_tokens: 30,
            reasoning_tokens: 10,
            total_tokens: 165,
            tool_call_count: 2,
            wall_clock_ms: 850,
            provider_latency_ms: Some(410),
            cost_estimate_micro_dollars: Some(1250),
            cost_usd: Some(0.00125),
            usage_estimated: false,
            started_at: Some("2026-11-22T10:00:00Z".to_string()),
            finished_at: Some("2026-11-22T10:00:01Z".to_string()),
            success: true,
            error_kind: None,
            schema_version: 1,
            created_at: "2026-11-22T10:00:01Z".to_string(),
            agent_name: Some("codex".to_string()),
            repo_id: Some("repo-rb06".to_string()),
            turn_id: Some("turn-1".to_string()),
            event_id: Some(event_id.to_string()),
        }
    }

    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[derive(Debug, PartialEq)]
    struct Capture25TablesSnapshot {
        schema_versions: Vec<CaptureSchemaVersionRecord>,
        copy_receipt: Option<CaptureCatalogCopyReceiptRecord>,
        audit_logs: Vec<AgentAuditLogRecord>,
        bridge_session: Option<AgentBridgeSessionRecord>,
        bridge_checkpoints: Vec<AgentBridgeCheckpointRecord>,
        bridge_events: Vec<AgentBridgeEventRecord>,
        bridge_events_by_time: Vec<AgentBridgeEventRecord>,
        bridge_links: Vec<AgentBridgeLinkRecord>,
        bridge_operations: Vec<AgentBridgeOperationRecord>,
        cloud_base: Option<AgentCaptureCloudBaseRecord>,
        incarnation: Option<AgentCaptureIncarnationRecord>,
        sessions_by_started_desc: Vec<AgentSessionRecord>,
        checkpoints_by_session: Vec<AgentCheckpointRecord>,
        prune_tombstone: Option<AgentCheckpointPruneTombstoneRecord>,
        coverage_claim: Option<AgentCoverageClaimRecord>,
        coverage_conflict: Option<AgentCoverageConflictRecord>,
        coverage_revisions: Vec<AgentCoverageRevisionRecord>,
        export_job: Option<AgentExportJobRecord>,
        import_identity: Option<AgentImportIdentityRecord>,
        import_tombstones: Vec<AgentImportTombstoneRecord>,
        session_commits: Vec<AgentSessionCommitRecord>,
        subagent_claim: Option<AgentSubagentContentClaimRecord>,
        subagent_revisions: Vec<AgentSubagentContentRevisionRecord>,
        subagent_link: Option<AgentSubagentLinkRecord>,
        usage_stats: Vec<AgentUsageStatsRecord>,
        scope_audits: Vec<AgentWorkspaceScopeAuditRecord>,
        batch_outcome_first: CaptureBatchOutcome,
        batch_outcome_dup: CaptureBatchOutcome,
        highwater_normal: CaptureQueueHighwater,
        highwater_priority: CaptureQueueHighwater,
        bridge_ack_gap: BridgeEventAppendOutcome,
        bridge_ack_closed: BridgeEventAppendOutcome,
    }

    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    async fn exercise_all_25_tables(store: &dyn CaptureStore) -> Capture25TablesSnapshot {
        // 1. schema_versions (keep canonical capture schema receipt + extra version row)
        store
            .put_schema_version(&CaptureSchemaVersionRecord {
                version: 2026111602,
                name: "capture_test_probe".to_string(),
                applied_at: "2026-11-22T12:00:00Z".to_string(),
            })
            .await
            .expect("put schema_versions");

        // 2. capture_catalog_copy_receipt
        store
            .put_copy_receipt(&CaptureCatalogCopyReceiptRecord {
                id: 1,
                copied_at: "2026-11-22T12:00:01Z".to_string(),
            })
            .await
            .expect("put copy receipt");

        // 3. agent_audit_log
        store
            .append_audit_log(&AgentAuditLogRecord {
                audit_id: "audit-1".to_string(),
                timestamp: "2026-11-22T12:00:02Z".to_string(),
                user_id: Some("eli@patch.sh".to_string()),
                user_name: Some("Eli".to_string()),
                action: "raw_export".to_string(),
                checkpoint_id: "cp-1".to_string(),
                scope: "transcript".to_string(),
                export_path: Some("/tmp/redacted.json".to_string()),
                justification: Some("compliance check".to_string()),
                granted: true,
            })
            .await
            .expect("append audit log");

        // 4..8. AgentBridgeStore tables: session, checkpoint, event, link, operation
        let (bsess, created) = store
            .open_bridge_session(&BridgeSessionOpenInput {
                bridge_session_id: "bsess-1".to_string(),
                repository_id: "repo-rb06".to_string(),
                worktree_id: Some("wt-main".to_string()),
                workspace_id: Some("ws-1".to_string()),
                parent_session_id: None,
                agent_id: Some("agent-1".to_string()),
                now_ms: 1000,
            })
            .await
            .expect("open bridge session");
        assert!(created);
        assert_eq!(bsess.source, BRIDGE_SOURCE_DEEPSEEK_HARNESS);
        let (_bsess_again, created_again) = store
            .open_bridge_session(&BridgeSessionOpenInput {
                bridge_session_id: "bsess-1".to_string(),
                repository_id: "repo-rb06".to_string(),
                worktree_id: Some("wt-main".to_string()),
                workspace_id: Some("ws-1".to_string()),
                parent_session_id: None,
                agent_id: Some("agent-1".to_string()),
                now_ms: 1005,
            })
            .await
            .expect("idempotent open bridge session");
        assert!(!created_again);

        let bridge_ack_gap = store
            .append_and_ack_events(
                "bsess-1",
                &[
                    BridgeEventAppendInput {
                        event_seq: 1,
                        event_type: "turn.start".to_string(),
                        payload: r#"{"step":1}"#.to_string(),
                        operation_id: Some("op-1".to_string()),
                    },
                    BridgeEventAppendInput {
                        event_seq: 3,
                        event_type: "turn.finish".to_string(),
                        payload: r#"{"step":3}"#.to_string(),
                        operation_id: None,
                    },
                ],
                1010,
            )
            .await
            .expect("append bridge events with gap");
        assert_eq!(bridge_ack_gap.last_acked_seq, 1);
        assert_eq!(bridge_ack_gap.last_event_seq, 3);

        let bridge_ack_closed = store
            .append_and_ack_events(
                "bsess-1",
                &[
                    BridgeEventAppendInput {
                        event_seq: 2,
                        event_type: "turn.delta".to_string(),
                        payload: r#"{"step":2}"#.to_string(),
                        operation_id: None,
                    },
                    BridgeEventAppendInput {
                        event_seq: 1,
                        event_type: "turn.start".to_string(),
                        payload: r#"{"step":1}"#.to_string(),
                        operation_id: Some("op-1".to_string()),
                    },
                ],
                1020,
            )
            .await
            .expect("close bridge gap");
        assert_eq!(bridge_ack_closed.last_acked_seq, 3);
        assert_eq!(
            bridge_ack_closed.per_event,
            vec![BridgeEventStatus::Accepted, BridgeEventStatus::Duplicate]
        );

        store
            .set_bridge_session_state("bsess-1", BridgeSessionState::Flushing, 1030)
            .await
            .expect("set bridge session state");

        store
            .put_bridge_checkpoint(&AgentBridgeCheckpointRecord {
                checkpoint_id: "bcp-1".to_string(),
                bridge_session_id: "bsess-1".to_string(),
                agent_checkpoint_id: Some("cp-1".to_string()),
                target_oid: Some("e".repeat(40)),
                created_at: 1035,
            })
            .await
            .expect("put bridge checkpoint");

        store
            .put_bridge_operation(&AgentBridgeOperationRecord {
                operation_id: "op-1".to_string(),
                bridge_session_id: "bsess-1".to_string(),
                method: "commit.create".to_string(),
                params_digest: "1".repeat(64),
                repository_id: "repo-rb06".to_string(),
                workspace_id: Some("ws-1".to_string()),
                status: BridgeOperationStatus::Applied,
                result_digest: Some("2".repeat(64)),
                created_at: 1040,
                completed_at: Some(1045),
            })
            .await
            .expect("put bridge operation");

        let (_link1, link_inserted) = store
            .insert_bridge_link(&BridgeLinkWrite {
                bridge_session_id: "bsess-1".to_string(),
                source_type: BridgeLinkSourceType::Operation,
                source_id: "op-1".to_string(),
                target_type: "commit".to_string(),
                target_id: "e".repeat(40),
                created_at: 1050,
            })
            .await
            .expect("insert bridge link");
        assert!(link_inserted);
        let (_link_dup, link_dup_inserted) = store
            .insert_bridge_link(&BridgeLinkWrite {
                bridge_session_id: "bsess-1".to_string(),
                source_type: BridgeLinkSourceType::Operation,
                source_id: "op-1".to_string(),
                target_type: "commit".to_string(),
                target_id: "e".repeat(40),
                created_at: 1051,
            })
            .await
            .expect("idempotent bridge link");
        assert!(!link_dup_inserted);

        // 9. agent_capture_cloud_base
        store
            .put_cloud_base(&AgentCaptureCloudBaseRecord {
                repo_id: "repo-rb06".to_string(),
                remote_generation: 4,
                updated_at: 2000,
            })
            .await
            .expect("put cloud base");

        // 10. agent_capture_incarnation
        store
            .put_incarnation(&AgentCaptureIncarnationRecord {
                agent_kind: "codex".to_string(),
                provider_session_id: "prov-1".to_string(),
                next_session_sync_revision: 3,
                source_namespace: "0123456789abcdef0123456789abcdef".to_string(),
                updated_at: 2005,
            })
            .await
            .expect("put incarnation");

        // 11. agent_session (2 sessions + timestamp update to verify SESSIONS_BY_TIME)
        store
            .upsert_session(&sample_session("sess-1", "prov-1", 3000))
            .await
            .expect("upsert sess-1");
        store
            .upsert_session(&sample_session("sess-2", "prov-2", 2500))
            .await
            .expect("upsert sess-2");
        // Update sess-2 started_at to sort ahead of sess-1
        store
            .upsert_session(&sample_session("sess-2", "prov-2", 4000))
            .await
            .expect("update sess-2 started_at");

        // 12. agent_checkpoint (2 checkpoints + CHECKPOINTS_BY_SESSION)
        store
            .upsert_checkpoint(&sample_checkpoint("cp-1", "sess-1", 3100))
            .await
            .expect("upsert cp-1");
        store
            .upsert_checkpoint(&sample_checkpoint("cp-2", "sess-1", 3200))
            .await
            .expect("upsert cp-2");

        // 13. agent_checkpoint_prune_tombstone
        store
            .put_checkpoint_prune_tombstone(&AgentCheckpointPruneTombstoneRecord {
                checkpoint_id: "cp-pruned-1".to_string(),
                session_id: "sess-1".to_string(),
                pruned_at: 3300,
            })
            .await
            .expect("put prune tombstone");

        // 14. agent_coverage_claim
        store
            .upsert_coverage_claim(&AgentCoverageClaimRecord {
                session_id: "sess-1".to_string(),
                logical_turn_key: "turn-001".to_string(),
                coverage_schema_version: 1,
                coverage_digest: "3".repeat(64),
                completeness: CaptureCompleteness::Complete,
                revision: 1,
                state: CaptureCoverageClaimState::CatalogCommitted,
                attempt_checkpoint_id: None,
                owner: None,
                lease_expires_at: None,
                fence_token: Some(1),
                checkpoint_id: Some("cp-1".to_string()),
                traces_commit: Some("d".repeat(40)),
                source_channel: CaptureSourceChannel::Live,
                created_at: 3110,
                updated_at: 3120,
            })
            .await
            .expect("upsert coverage claim");

        // 15. agent_coverage_conflict
        store
            .upsert_coverage_conflict(&AgentCoverageConflictRecord {
                session_id: "sess-1".to_string(),
                logical_turn_key: "turn-001".to_string(),
                coverage_schema_version: 1,
                incumbent_revision: 1,
                incumbent_digest: "3".repeat(64),
                incumbent_checkpoint_id: Some("cp-1".to_string()),
                incoming_digest: "4".repeat(64),
                incoming_source_channel: CaptureSourceChannel::Import,
                incoming_observed_at: 3130,
                incoming_canonical_json: r#"{"turn":1}"#.to_string(),
                incoming_redaction_report_json: r#"{"ok":true}"#.to_string(),
            })
            .await
            .expect("upsert coverage conflict");

        // 16. agent_coverage_revision
        store
            .put_coverage_revision(&AgentCoverageRevisionRecord {
                session_id: "sess-1".to_string(),
                logical_turn_key: "turn-001".to_string(),
                coverage_schema_version: 1,
                revision: 1,
                checkpoint_id: "cp-1".to_string(),
                coverage_digest: "3".repeat(64),
                completeness: CaptureCompleteness::Complete,
                source_channel: CaptureSourceChannel::Live,
                created_at: 3120,
            })
            .await
            .expect("put coverage revision");

        // 17. agent_export_job
        store
            .upsert_export_job(&AgentExportJobRecord {
                job_id: "job-1".to_string(),
                agent_kind: "codex".to_string(),
                provider_session_id: "prov-1".to_string(),
                owner: Some("worker-1".to_string()),
                lease_expires_at: Some(9000),
                fence_token: Some(2),
                observed_generation: 5,
                processed_generation: 4,
                state: CaptureExportJobState::Dirty,
                last_error_code: None,
                created_at: 3400,
                updated_at: 3410,
                ttl_expires_at: 99999,
                repo_id: Some("repo-rb06".to_string()),
                worktree_id: Some("wt-main".to_string()),
                workspace_id: Some("ws-1".to_string()),
                workspace_fence: Some(7),
                scope_state: CaptureScopeState::Scoped,
            })
            .await
            .expect("upsert export job");

        // 18. agent_import_identity
        store
            .upsert_import_identity(&AgentImportIdentityRecord {
                identity_id: "imp-1".to_string(),
                agent_kind: "codex".to_string(),
                provider_session_id: "prov-1".to_string(),
                source_kind: "jsonl".to_string(),
                source_id: "rel/session-1.jsonl".to_string(),
                schema_version: 1,
                observed_digest: Some("5".repeat(64)),
                committed_digest: Some("5".repeat(64)),
                attempt_id: Some("att-1".to_string()),
                attempt_checkpoint_id: Some("cp-1".to_string()),
                next_ordinal: 4,
                state: CaptureImportIdentityState::Committed,
                owner: None,
                lease_expires_at: None,
                fence_token: Some(1),
                last_error_code: None,
                created_at: 3450,
                updated_at: 3460,
                repo_id: Some("repo-rb06".to_string()),
                worktree_id: Some("wt-main".to_string()),
                workspace_id: Some("ws-1".to_string()),
                workspace_fence: Some(7),
                scope_state: CaptureScopeState::Scoped,
            })
            .await
            .expect("upsert import identity");

        // 19. agent_import_tombstone (for a distinct erased session)
        store
            .put_import_tombstone(&AgentImportTombstoneRecord {
                tombstone_id: "tomb-1".to_string(),
                agent_kind: "claude_code".to_string(),
                provider_session_id: "erased-prov-9".to_string(),
                erased_session_id: "claude_code__erased-prov-9".to_string(),
                source_fingerprint: Some("fp-hash-9".to_string()),
                erased_at: 3500,
            })
            .await
            .expect("put import tombstone");

        // 20. agent_session_commit
        store
            .put_session_commit(&AgentSessionCommitRecord {
                session_id: "sess-1".to_string(),
                hash_kind: CaptureHashKind::Sha1,
                commit_oid: "0123456789abcdef0123456789abcdef01234567".to_string(),
            })
            .await
            .expect("put session commit");

        // 21. agent_subagent_content_claim
        store
            .upsert_subagent_content_claim(&AgentSubagentContentClaimRecord {
                parent_session_id: "sess-1".to_string(),
                provider_kind: "codex".to_string(),
                source_key: "subagent-a".to_string(),
                content_schema_version: 1,
                current_revision: 1,
                current_checkpoint_id: Some("cp-2".to_string()),
                current_digest: Some("6".repeat(64)),
                state: CaptureSubagentClaimState::Idle,
                attempt_digest: None,
                attempt_checkpoint_id: None,
                owner: None,
                lease_expires_at: None,
                fence_token: 3,
                created_at: 3600,
                updated_at: 3610,
                revision_cursor: 1,
                sync_revision: 2,
            })
            .await
            .expect("upsert subagent content claim");

        // 22. agent_subagent_content_revision
        store
            .put_subagent_content_revision(&AgentSubagentContentRevisionRecord {
                parent_session_id: "sess-1".to_string(),
                provider_kind: "codex".to_string(),
                source_key: "subagent-a".to_string(),
                content_schema_version: 1,
                revision: 1,
                checkpoint_id: "cp-2".to_string(),
                content_digest: "6".repeat(64),
                source_channel: CaptureSubagentSourceChannel::Live,
                partial: false,
                created_at: 3610,
            })
            .await
            .expect("put subagent content revision");

        // 23. agent_subagent_link
        store
            .upsert_subagent_link(&AgentSubagentLinkRecord {
                content_checkpoint_id: "cp-2".to_string(),
                parent_session_id: "sess-1".to_string(),
                link_state: CaptureSubagentLinkState::Resolved,
                boundary_checkpoint_id: Some("cp-1".to_string()),
                stable_subagent_id: Some("sub-stable-1".to_string()),
                created_at: 3620,
                updated_at: 3625,
                sync_revision: 1,
            })
            .await
            .expect("upsert subagent link");

        // 24. agent_usage_stats
        store
            .put_usage_stats(&sample_usage("usage-1", "sess-1", "ev-1"))
            .await
            .expect("put usage stats");

        // 25. agent_workspace_scope_audit
        store
            .append_workspace_scope_audit(&AgentWorkspaceScopeAuditRecord {
                audit_id: "wsa-1".to_string(),
                action: WORKSPACE_SCOPE_AUDIT_ACTION.to_string(),
                agent_kind: "codex".to_string(),
                provider_session_id: "prov-1".to_string(),
                repo_id: "repo-rb06".to_string(),
                worktree_id: "wt-main".to_string(),
                workspace_id: Some("ws-1".to_string()),
                workspace_fence: Some(7),
                actor: Some("eli".to_string()),
                created_at: 3700,
            })
            .await
            .expect("append workspace scope audit");

        // Batch ingestion with dedup + queue highwater advance on both backends
        let batch_items = vec![
            CaptureBatchItem {
                dedup_key: "codex:sess-1:usage:ev-batch-1".to_string(),
                payload: CaptureBatchPayload::InsertUsageStats(sample_usage(
                    "usage-b1",
                    "sess-1",
                    "ev-batch-1",
                )),
            },
            CaptureBatchItem {
                dedup_key: "codex:sess-1:usage:ev-batch-2".to_string(),
                payload: CaptureBatchPayload::InsertUsageStats(sample_usage(
                    "usage-b2",
                    "sess-1",
                    "ev-batch-2",
                )),
            },
        ];
        let batch_outcome_first = store
            .append_capture_batch(
                &batch_items,
                Some((
                    CaptureQueueLane::Normal,
                    CaptureQueueHighwater {
                        segment_id: 0,
                        byte_offset: 256,
                    },
                )),
            )
            .await
            .expect("first capture batch");
        let batch_outcome_dup = store
            .append_capture_batch(
                &batch_items,
                Some((
                    CaptureQueueLane::Priority,
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 0,
                    },
                )),
            )
            .await
            .expect("duplicate capture batch");

        Capture25TablesSnapshot {
            schema_versions: store
                .list_schema_versions()
                .await
                .expect("list schema versions")
                .into_iter()
                .map(|mut rec| {
                    // Normalize the bootstrap schema receipt timestamp across backends
                    if rec.version == crate::internal::db::schema::CAPTURE_SCHEMA_VERSION {
                        rec.applied_at = "<bootstrap>".to_string();
                    }
                    rec
                })
                .collect(),
            copy_receipt: store.get_copy_receipt().await.expect("get copy receipt"),
            audit_logs: store
                .list_audit_logs_by_checkpoint("cp-1")
                .await
                .expect("list audit logs"),
            bridge_session: store
                .get_bridge_session("bsess-1")
                .await
                .expect("get bridge session"),
            bridge_checkpoints: store
                .list_bridge_checkpoints("bsess-1")
                .await
                .expect("list bridge checkpoints"),
            bridge_events: store
                .list_bridge_events("bsess-1")
                .await
                .expect("list bridge events"),
            bridge_events_by_time: store
                .list_bridge_events_by_time()
                .await
                .expect("list bridge events by time"),
            bridge_links: store
                .list_bridge_links("bsess-1")
                .await
                .expect("list bridge links"),
            bridge_operations: store
                .list_bridge_operations("bsess-1")
                .await
                .expect("list bridge operations"),
            cloud_base: store
                .get_cloud_base("repo-rb06")
                .await
                .expect("get cloud base"),
            incarnation: store
                .get_incarnation("codex", "prov-1")
                .await
                .expect("get incarnation"),
            sessions_by_started_desc: store
                .list_sessions_by_started_desc()
                .await
                .expect("list sessions"),
            checkpoints_by_session: store
                .list_checkpoints_by_session("sess-1")
                .await
                .expect("list checkpoints"),
            prune_tombstone: store
                .get_checkpoint_prune_tombstone("cp-pruned-1")
                .await
                .expect("get prune tombstone"),
            coverage_claim: store
                .get_coverage_claim("sess-1", "turn-001", 1)
                .await
                .expect("get coverage claim"),
            coverage_conflict: store
                .get_coverage_conflict("sess-1", "turn-001", 1)
                .await
                .expect("get coverage conflict"),
            coverage_revisions: store
                .list_coverage_revisions("sess-1", "turn-001", 1)
                .await
                .expect("list coverage revisions"),
            export_job: store.get_export_job("job-1").await.expect("get export job"),
            import_identity: store
                .get_import_identity("imp-1")
                .await
                .expect("get import identity"),
            import_tombstones: store
                .list_import_tombstones()
                .await
                .expect("list import tombstones"),
            session_commits: store
                .list_session_commits("sess-1")
                .await
                .expect("list session commits"),
            subagent_claim: store
                .get_subagent_content_claim("sess-1", "codex", "subagent-a", 1)
                .await
                .expect("get subagent claim"),
            subagent_revisions: store
                .list_subagent_content_revisions("sess-1", "codex", "subagent-a", 1)
                .await
                .expect("list subagent revisions"),
            subagent_link: store
                .get_subagent_link("cp-2")
                .await
                .expect("get subagent link"),
            usage_stats: store
                .list_usage_stats_by_session("sess-1")
                .await
                .expect("list usage stats"),
            scope_audits: store
                .list_workspace_scope_audits("codex", "prov-1")
                .await
                .expect("list scope audits"),
            batch_outcome_first,
            batch_outcome_dup,
            highwater_normal: store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .expect("normal highwater"),
            highwater_priority: store
                .get_queue_highwater(CaptureQueueLane::Priority)
                .await
                .expect("priority highwater"),
            bridge_ack_gap,
            bridge_ack_closed,
        }
    }

    /// AC1 & AC2: Static 25-table manifest guard (separating `META_*`,
    /// `QUEUE_HIGHWATER_*`, and secondary indexes) + full dual-backend
    /// equivalence across all 25 Capture source tables.
    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test]
    async fn dual_backend_25_source_tables_equivalence() {
        let dir = tempfile::tempdir().expect("tempdir");

        // --- AC1: Static manifest & live on-disk table set guards ---
        assert_eq!(CAPTURE_SOURCE_TABLES.len(), CAPTURE_SOURCE_TABLES_COUNT);
        let declared_source: BTreeSet<String> = CAPTURE_SOURCE_TABLES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(
            declared_source.len(),
            25,
            "CAPTURE_SOURCE_TABLES must contain 25 distinct source table names"
        );

        let sql_source = capture_source_tables_from_sql();
        assert_eq!(
            declared_source, sql_source,
            "CAPTURE_SOURCE_TABLES must exactly equal the 25 CREATE TABLE definitions in 2026111601_capture_database.sql"
        );

        let internal_set: BTreeSet<String> = CAPTURE_INTERNAL_META_AND_QUEUE_TABLES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let index_set: BTreeSet<String> = CAPTURE_SECONDARY_INDEX_TABLES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert!(
            declared_source.is_disjoint(&internal_set),
            "META_* / QUEUE_HIGHWATER_* internal tables must never appear in CAPTURE_SOURCE_TABLES"
        );
        assert!(
            declared_source.is_disjoint(&index_set),
            "secondary index tables must never appear in CAPTURE_SOURCE_TABLES"
        );
        assert!(
            internal_set.is_disjoint(&index_set),
            "internal meta/queue tables and secondary index tables must be disjoint"
        );

        // Create both stores and verify live on-disk table sets BEFORE writing
        let sqlite_path = dir.path().join("capture.db");
        let sqlite_store = SqliteCaptureStore::create(&sqlite_path)
            .await
            .expect("create SqliteCaptureStore");
        let sqlite_live_tables = sqlite_store
            .list_live_source_tables()
            .await
            .expect("list sqlite tables");
        assert_eq!(
            sqlite_live_tables, declared_source,
            "fresh SqliteCaptureStore must materialize the exact 25 Capture source tables"
        );

        let redb_path = dir.path().join("capture.gen-000001.redb");
        let redb_store = RedbCaptureStore::create(&redb_path)
            .await
            .expect("create RedbCaptureStore");
        let redb_all_tables = redb_store
            .list_live_table_names()
            .expect("list redb tables");
        let non_source_declared: BTreeSet<String> = capture_declared_non_source_tables()
            .into_iter()
            .map(str::to_string)
            .collect();
        for tbl in &non_source_declared {
            assert!(
                redb_all_tables.contains(tbl),
                "RedbCaptureStore missing declared non-source table `{tbl}`"
            );
        }
        let redb_source_only: BTreeSet<String> = redb_all_tables
            .difference(&non_source_declared)
            .cloned()
            .collect();
        assert_eq!(
            redb_source_only, declared_source,
            "RedbCaptureStore live tables minus segregated META_*/QUEUE_HIGHWATER_*/index tables must equal the exact 25 Capture source tables"
        );

        // --- AC2: Dual-backend equivalence across all 25 tables ---
        let sqlite_snap = exercise_all_25_tables(&sqlite_store).await;
        let redb_snap = exercise_all_25_tables(&redb_store).await;
        assert_eq!(
            sqlite_snap, redb_snap,
            "SqliteCaptureStore and RedbCaptureStore must return identical results across all 25 Capture source tables"
        );
        assert_eq!(sqlite_snap.sessions_by_started_desc[0].session_id, "sess-2");
        assert_eq!(sqlite_snap.sessions_by_started_desc[1].session_id, "sess-1");
        assert_eq!(sqlite_snap.batch_outcome_first.applied_count, 2);
        assert_eq!(sqlite_snap.batch_outcome_first.deduplicated_count, 0);
        assert_eq!(sqlite_snap.batch_outcome_dup.applied_count, 0);
        assert_eq!(sqlite_snap.batch_outcome_dup.deduplicated_count, 2);

        // Verify append-only committed history on agent_coverage_revision (identical retry OK, conflict rejected)
        let same_cov_rev = AgentCoverageRevisionRecord {
            session_id: "sess-1".to_string(),
            logical_turn_key: "turn-001".to_string(),
            coverage_schema_version: 1,
            revision: 1,
            checkpoint_id: "cp-1".to_string(),
            coverage_digest: "3".repeat(64),
            completeness: CaptureCompleteness::Complete,
            source_channel: CaptureSourceChannel::Live,
            created_at: 3120,
        };
        let mut conflicting_cov_rev = same_cov_rev.clone();
        conflicting_cov_rev.coverage_digest = "9".repeat(64);
        for backend in [
            &sqlite_store as &dyn CaptureStore,
            &redb_store as &dyn CaptureStore,
        ] {
            backend
                .put_coverage_revision(&same_cov_rev)
                .await
                .expect("identical coverage revision retry must be idempotent");
            backend
                .put_coverage_revision(&conflicting_cov_rev)
                .await
                .expect_err("conflicting coverage revision rewrite must be rejected");
        }

        // Verify mid-batch failure atomically rolls back earlier rows, dedup_keys,
        // secondary indexes, and queue highwater on BOTH SQLite and redb, and allows retry.
        for backend in [
            &sqlite_store as &dyn CaptureStore,
            &redb_store as &dyn CaptureStore,
        ] {
            let hw_before = backend
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .expect("hw before failed batch");
            let failing_batch = vec![
                CaptureBatchItem {
                    dedup_key: "rollback-probe:session".to_string(),
                    payload: CaptureBatchPayload::UpsertSession(sample_session(
                        "sess-rollback",
                        "prov-rollback",
                        9999,
                    )),
                },
                CaptureBatchItem {
                    dedup_key: "rollback-probe:usage".to_string(),
                    payload: CaptureBatchPayload::InsertUsageStats(sample_usage(
                        "usage-rollback",
                        "sess-rollback",
                        "ev-rollback",
                    )),
                },
                // Fails FK check AFTER first 2 items have been written inside the tx
                CaptureBatchItem {
                    dedup_key: "rollback-probe:bad-cp".to_string(),
                    payload: CaptureBatchPayload::UpsertCheckpoint(sample_checkpoint(
                        "cp-orphan",
                        "sess-does-not-exist",
                        9999,
                    )),
                },
            ];
            backend
                .append_capture_batch(
                    &failing_batch,
                    Some((
                        CaptureQueueLane::Normal,
                        CaptureQueueHighwater {
                            segment_id: 9,
                            byte_offset: 9999,
                        },
                    )),
                )
                .await
                .expect_err("mid-batch FK failure must abort batch");

            assert!(
                backend
                    .get_session("sess-rollback")
                    .await
                    .expect("get rolled-back session")
                    .is_none(),
                "earlier batch session row must roll back"
            );
            assert!(
                backend
                    .list_sessions_by_started_desc()
                    .await
                    .expect("list sessions after rollback")
                    .iter()
                    .all(|s| s.session_id != "sess-rollback"),
                "SESSIONS_BY_TIME index entry must roll back"
            );
            assert!(
                backend
                    .get_usage_stats("usage-rollback")
                    .await
                    .expect("get rolled-back usage")
                    .is_none(),
                "earlier batch usage row must roll back"
            );
            assert_eq!(
                backend
                    .get_queue_highwater(CaptureQueueLane::Normal)
                    .await
                    .expect("hw after rollback"),
                hw_before,
                "highwater must not advance when batch aborts"
            );

            // Retrying with a DIFFERENT usage ID claiming the rolled-back `(session_id, event_id)`
            // proves `usage_stats_by_session_event` secondary index rolled back cleanly too!
            let retry_items = vec![
                failing_batch[0].clone(),
                CaptureBatchItem {
                    dedup_key: "rollback-probe:usage".to_string(),
                    payload: CaptureBatchPayload::InsertUsageStats(sample_usage(
                        "usage-rollback-alt-id",
                        "sess-rollback",
                        "ev-rollback",
                    )),
                },
            ];
            let retry_outcome = backend
                .append_capture_batch(&retry_items, None)
                .await
                .expect("retry after rollback must succeed with distinct usage ID");
            assert_eq!(retry_outcome.applied_count, 2);
            assert_eq!(retry_outcome.deduplicated_count, 0);
            assert!(
                backend
                    .get_session("sess-rollback")
                    .await
                    .expect("get retried session")
                    .is_some()
            );
            assert!(
                backend
                    .get_usage_stats("usage-rollback-alt-id")
                    .await
                    .expect("get retried usage")
                    .is_some()
            );

            // Exercise usage_stats_by_session_event uniqueness, stale-key update/reuse,
            // and SQLite NULL-distinct semantics on BOTH backends.
            backend
                .put_usage_stats(&sample_usage(
                    "usage-dup-conflict",
                    "sess-rollback",
                    "ev-rollback",
                ))
                .await
                .expect_err("distinct usage ID claiming existing (session_id, event_id) must fail");

            // Update existing row's event_id -> old (sess-rollback, ev-rollback) becomes free!
            backend
                .put_usage_stats(&sample_usage(
                    "usage-rollback-alt-id",
                    "sess-rollback",
                    "ev-rollback-moved",
                ))
                .await
                .expect("updating usage row event_id must succeed");
            backend
                .put_usage_stats(&sample_usage(
                    "usage-reclaimed-old-key",
                    "sess-rollback",
                    "ev-rollback",
                ))
                .await
                .expect("reclaiming old (session_id, event_id) after key update must succeed");

            // NULL-distinct semantics: multiple rows with NULL event_id or NULL session_id allowed
            let mut null_ev_1 = sample_usage("usage-null-ev-1", "sess-rollback", "unused");
            null_ev_1.event_id = None;
            let mut null_ev_2 = sample_usage("usage-null-ev-2", "sess-rollback", "unused");
            null_ev_2.event_id = None;
            backend
                .put_usage_stats(&null_ev_1)
                .await
                .expect("first NULL event_id usage row");
            backend
                .put_usage_stats(&null_ev_2)
                .await
                .expect("second NULL event_id usage row (SQLite NULL-distinct)");

            let mut null_sess_1 = sample_usage("usage-null-sess-1", "unused", "ev-shared-null");
            null_sess_1.session_id = None;
            let mut null_sess_2 = sample_usage("usage-null-sess-2", "unused", "ev-shared-null");
            null_sess_2.session_id = None;
            backend
                .put_usage_stats(&null_sess_1)
                .await
                .expect("first NULL session_id usage row");
            backend
                .put_usage_stats(&null_sess_2)
                .await
                .expect("second NULL session_id usage row (SQLite NULL-distinct)");

            // Exercise sessions_by_provider uniqueness, stale-key update/reuse, and delete cleanup
            backend
                .upsert_session(&sample_session("sess-dup-provider", "prov-rollback", 10001))
                .await
                .expect_err("distinct session_id claiming existing (agent_kind, provider_session_id) must fail");
            backend
                .upsert_session(&sample_session(
                    "sess-rollback",
                    "prov-rollback-moved",
                    10002,
                ))
                .await
                .expect("updating session provider_session_id must succeed");
            backend
                .upsert_session(&sample_session(
                    "sess-reclaimed-prov",
                    "prov-rollback",
                    10003,
                ))
                .await
                .expect("reclaiming old provider_session_id after update must succeed");
            assert!(
                backend
                    .delete_session("sess-reclaimed-prov")
                    .await
                    .expect("delete reclaimed session")
            );
            backend
                .upsert_session(&sample_session(
                    "sess-reclaimed-prov-2",
                    "prov-rollback",
                    10004,
                ))
                .await
                .expect("reclaiming provider_session_id after delete_session must succeed");

            // Exercise persistent gap at seq 1 with a large unacked tail (200 events)
            backend
                .open_bridge_session(&BridgeSessionOpenInput {
                    bridge_session_id: "bsess-large-tail".to_string(),
                    repository_id: "repo-rb06".to_string(),
                    worktree_id: None,
                    workspace_id: None,
                    parent_session_id: None,
                    agent_id: None,
                    now_ms: 8000,
                })
                .await
                .expect("open large-tail bridge session");
            let tail_events: Vec<BridgeEventAppendInput> = (2..=201)
                .map(|seq| BridgeEventAppendInput {
                    event_seq: seq,
                    event_type: "tail".to_string(),
                    payload: format!(r#"{{"seq":{seq}}}"#),
                    operation_id: None,
                })
                .collect();
            let gap_tail = backend
                .append_and_ack_events("bsess-large-tail", &tail_events, 8010)
                .await
                .expect("append 200-event out-of-order tail");
            assert_eq!(gap_tail.last_event_seq, 201);
            assert_eq!(gap_tail.last_acked_seq, 0);

            let close_gap = backend
                .append_and_ack_events(
                    "bsess-large-tail",
                    &[BridgeEventAppendInput {
                        event_seq: 1,
                        event_type: "tail".to_string(),
                        payload: r#"{"seq":1}"#.to_string(),
                        operation_id: None,
                    }],
                    8020,
                )
                .await
                .expect("close persistent seq-1 gap");
            assert_eq!(close_gap.last_event_seq, 201);
            assert_eq!(close_gap.last_acked_seq, 201);
            assert!(
                backend
                    .delete_bridge_session("bsess-large-tail")
                    .await
                    .expect("cleanup large-tail bridge session")
            );
        }

        // Verify boundary checkpoint delete cascade & second-aligned timestamp parity on BOTH backends
        assert!(
            sqlite_store
                .delete_checkpoint("cp-1")
                .await
                .expect("sqlite delete boundary checkpoint")
        );
        assert!(
            redb_store
                .delete_checkpoint("cp-1")
                .await
                .expect("redb delete boundary checkpoint")
        );
        let sq_link_after = sqlite_store
            .get_subagent_link("cp-2")
            .await
            .expect("sqlite subagent link after boundary delete")
            .expect("subagent link still exists");
        let rb_link_after = redb_store
            .get_subagent_link("cp-2")
            .await
            .expect("redb subagent link after boundary delete")
            .expect("subagent link still exists");
        assert_eq!(
            sq_link_after.link_state,
            CaptureSubagentLinkState::Unresolved
        );
        assert_eq!(
            rb_link_after.link_state,
            CaptureSubagentLinkState::Unresolved
        );
        assert_eq!(sq_link_after.boundary_checkpoint_id, None);
        assert_eq!(rb_link_after.boundary_checkpoint_id, None);
        assert_eq!(sq_link_after.sync_revision, 2);
        assert_eq!(rb_link_after.sync_revision, 2);
        assert_eq!(sq_link_after.updated_at % 1000, 0);
        assert_eq!(rb_link_after.updated_at % 1000, 0);

        // Verify `\u{10ffff}` boundary-character keys are NOT missed by range queries or session cascade delete
        let max_char_turn = "\u{10ffff}z-boundary-turn";
        let max_char_cp = "\u{10ffff}z-boundary-cp";
        for backend in [
            &sqlite_store as &dyn CaptureStore,
            &redb_store as &dyn CaptureStore,
        ] {
            backend
                .upsert_checkpoint(&sample_checkpoint(max_char_cp, "sess-1", 3999))
                .await
                .expect("upsert max-char checkpoint");
            backend
                .upsert_coverage_claim(&AgentCoverageClaimRecord {
                    session_id: "sess-1".to_string(),
                    logical_turn_key: max_char_turn.to_string(),
                    coverage_schema_version: 1,
                    coverage_digest: "7".repeat(64),
                    completeness: CaptureCompleteness::Complete,
                    revision: 1,
                    state: CaptureCoverageClaimState::CatalogCommitted,
                    attempt_checkpoint_id: None,
                    owner: None,
                    lease_expires_at: None,
                    fence_token: None,
                    checkpoint_id: Some(max_char_cp.to_string()),
                    traces_commit: Some("d".repeat(40)),
                    source_channel: CaptureSourceChannel::Live,
                    created_at: 3999,
                    updated_at: 3999,
                })
                .await
                .expect("upsert max-char coverage claim");
            backend
                .put_coverage_revision(&AgentCoverageRevisionRecord {
                    session_id: "sess-1".to_string(),
                    logical_turn_key: max_char_turn.to_string(),
                    coverage_schema_version: 1,
                    revision: 1,
                    checkpoint_id: max_char_cp.to_string(),
                    coverage_digest: "7".repeat(64),
                    completeness: CaptureCompleteness::Complete,
                    source_channel: CaptureSourceChannel::Live,
                    created_at: 3999,
                })
                .await
                .expect("put max-char coverage revision");
            assert!(
                backend
                    .list_checkpoints_by_session("sess-1")
                    .await
                    .expect("list checkpoints incl max-char")
                    .iter()
                    .any(|cp| cp.checkpoint_id == max_char_cp),
                "list_checkpoints_by_session must include keys starting with U+10FFFF"
            );
        }

        // Verify cascade delete parity on both session and bridge session
        assert!(
            sqlite_store
                .delete_bridge_session("bsess-1")
                .await
                .expect("sqlite delete bridge session")
        );
        assert!(
            redb_store
                .delete_bridge_session("bsess-1")
                .await
                .expect("redb delete bridge session")
        );
        assert!(
            sqlite_store
                .list_bridge_events("bsess-1")
                .await
                .expect("sqlite events after delete")
                .is_empty()
        );
        assert!(
            redb_store
                .list_bridge_events("bsess-1")
                .await
                .expect("redb events after delete")
                .is_empty()
        );
        assert!(
            sqlite_store
                .list_bridge_events_by_time()
                .await
                .expect("sqlite events_by_time after delete")
                .is_empty()
        );
        assert!(
            redb_store
                .list_bridge_events_by_time()
                .await
                .expect("redb events_by_time after delete")
                .is_empty()
        );

        // Verify bridge_links_by_key cleanup: deleting `bsess-1` clears its links
        // and allows a recreated session to re-insert the exact same semantic link key.
        let mut reinserted_links = Vec::new();
        for backend in [
            &sqlite_store as &dyn CaptureStore,
            &redb_store as &dyn CaptureStore,
        ] {
            assert!(
                backend
                    .list_bridge_links("bsess-1")
                    .await
                    .expect("bridge links empty after delete")
                    .is_empty()
            );
            let (_reopened, reopened_created) = backend
                .open_bridge_session(&BridgeSessionOpenInput {
                    bridge_session_id: "bsess-1-reborn".to_string(),
                    repository_id: "repo-rb06".to_string(),
                    worktree_id: None,
                    workspace_id: None,
                    parent_session_id: None,
                    agent_id: None,
                    now_ms: 9000,
                })
                .await
                .expect("open reborn bridge session");
            assert!(reopened_created);
            let (relink, relink_inserted) = backend
                .insert_bridge_link(&BridgeLinkWrite {
                    bridge_session_id: "bsess-1-reborn".to_string(),
                    source_type: BridgeLinkSourceType::Operation,
                    source_id: "op-1".to_string(),
                    target_type: "commit".to_string(),
                    target_id: "e".repeat(40),
                    created_at: 9010,
                })
                .await
                .expect("reinsert semantic bridge link key after cascade delete");
            assert!(
                relink_inserted,
                "semantic bridge link key must be newly inserted after owning session delete"
            );
            reinserted_links.push((relink, relink_inserted));
        }
        assert_eq!(
            reinserted_links[0], reinserted_links[1],
            "re-inserted bridge link record and status must match across SQLite and redb"
        );

        assert!(
            sqlite_store
                .delete_session("sess-1")
                .await
                .expect("sqlite delete session")
        );
        assert!(
            redb_store
                .delete_session("sess-1")
                .await
                .expect("redb delete session")
        );
        for backend in [
            &sqlite_store as &dyn CaptureStore,
            &redb_store as &dyn CaptureStore,
        ] {
            assert!(
                backend
                    .list_checkpoints_by_session("sess-1")
                    .await
                    .expect("checkpoints after delete")
                    .is_empty()
            );
            assert!(
                backend
                    .get_checkpoint(max_char_cp)
                    .await
                    .expect("max-char cp after delete")
                    .is_none()
            );
            assert!(
                backend
                    .get_coverage_claim("sess-1", max_char_turn, 1)
                    .await
                    .expect("max-char coverage claim after delete")
                    .is_none()
            );
            assert!(
                backend
                    .list_coverage_revisions("sess-1", max_char_turn, 1)
                    .await
                    .expect("max-char coverage revisions after delete")
                    .is_empty()
            );
        }
    }

    /// AC3, AC4 & AC5: Single-transaction `append_capture_batch` (>= 128 events),
    /// full semantic `dedup_key` idempotency, and single-transaction
    /// `AgentBridgeStore::append_and_ack_events` contiguous ack advancement.
    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    async fn redb_bridge_contiguous_ack_and_batch_dedup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let redb_path = dir.path().join("capture.gen-000001.redb");
        let store = RedbCaptureStore::create(&redb_path)
            .await
            .expect("create RedbCaptureStore");

        // Seed parent session for checkpoints & usage stats
        store
            .upsert_session(&sample_session("sess-batch", "prov-batch", 1000))
            .await
            .expect("seed session");

        // --- AC3: Single WriteTransaction batch write of >= 128 events (160 events) ---
        let mut batch_160 = Vec::with_capacity(160);
        for i in 0..160usize {
            let dedup_key = format!("v1:codex:sess-batch:usage:turn-{i:04}:event-{i:04}");
            let usage = sample_usage(
                &format!("usage-{i:04}"),
                "sess-batch",
                &format!("ev-{i:04}"),
            );
            batch_160.push(CaptureBatchItem {
                dedup_key,
                payload: CaptureBatchPayload::InsertUsageStats(usage),
            });
        }

        let tx_before = store.write_transaction_count();
        let first_outcome = store
            .append_capture_batch(
                &batch_160,
                Some((
                    CaptureQueueLane::Normal,
                    CaptureQueueHighwater {
                        segment_id: 0,
                        byte_offset: 65_536,
                    },
                )),
            )
            .await
            .expect("append 160-event batch");
        let tx_after = store.write_transaction_count();

        assert_eq!(
            tx_after - tx_before,
            1,
            "append_capture_batch must commit all 160 events in a SINGLE redb WriteTransaction"
        );
        assert_eq!(first_outcome.applied_count, 160);
        assert_eq!(first_outcome.deduplicated_count, 0);
        assert_eq!(
            first_outcome.highwater,
            Some(CaptureQueueHighwater {
                segment_id: 0,
                byte_offset: 65_536,
            })
        );
        assert_eq!(
            store
                .list_usage_stats_by_session("sess-batch")
                .await
                .expect("list 160 usage rows")
                .len(),
            160
        );

        // --- AC4: Full semantic dedup_key idempotency (both cross-batch & intra-batch) ---
        // Build a mixed batch: 160 previously seen keys (even with distinct row IDs to prove
        // deduplication happens by full semantic `dedup_key`, not merely PK conflict!) + 4 new
        // keys, plus 2 duplicate occurrences of a new key inside the SAME batch.
        let mut replay_and_new = Vec::with_capacity(166);
        for i in 0..160usize {
            replay_and_new.push(CaptureBatchItem {
                dedup_key: format!("v1:codex:sess-batch:usage:turn-{i:04}:event-{i:04}"),
                payload: CaptureBatchPayload::InsertUsageStats(sample_usage(
                    &format!("usage-dup-attempt-{i:04}"),
                    "sess-batch",
                    &format!("ev-dup-attempt-{i:04}"),
                )),
            });
        }
        // Long semantic dedup_keys sharing an identical 200-byte prefix (proving NO truncation!)
        let shared_prefix = "p".repeat(200);
        for suffix in ["alpha", "beta", "gamma", "delta"] {
            replay_and_new.push(CaptureBatchItem {
                dedup_key: format!("{shared_prefix}:{suffix}"),
                payload: CaptureBatchPayload::InsertUsageStats(sample_usage(
                    &format!("usage-long-{suffix}"),
                    "sess-batch",
                    &format!("ev-long-{suffix}"),
                )),
            });
        }
        // Intra-batch duplicates of the long keys
        for suffix in ["alpha", "delta"] {
            replay_and_new.push(CaptureBatchItem {
                dedup_key: format!("{shared_prefix}:{suffix}"),
                payload: CaptureBatchPayload::InsertUsageStats(sample_usage(
                    &format!("usage-intrabatch-dup-{suffix}"),
                    "sess-batch",
                    &format!("ev-intrabatch-dup-{suffix}"),
                )),
            });
        }

        let tx_before_dup = store.write_transaction_count();
        let dup_outcome = store
            .append_capture_batch(
                &replay_and_new,
                Some((
                    CaptureQueueLane::Normal,
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 0,
                    },
                )),
            )
            .await
            .expect("replay + long-key batch");
        assert_eq!(store.write_transaction_count() - tx_before_dup, 1);
        assert_eq!(dup_outcome.applied_count, 4);
        assert_eq!(dup_outcome.deduplicated_count, 162);
        assert_eq!(
            store
                .list_usage_stats_by_session("sess-batch")
                .await
                .expect("list 164 usage rows")
                .len(),
            164
        );

        // Empty-batch highwater advance (ADR-RB-05 #4(c)) and regression guard
        let empty_adv = store
            .append_capture_batch(
                &[],
                Some((
                    CaptureQueueLane::Normal,
                    CaptureQueueHighwater {
                        segment_id: 2,
                        byte_offset: 0,
                    },
                )),
            )
            .await
            .expect("empty-batch highwater advance");
        assert_eq!(empty_adv.applied_count, 0);
        assert_eq!(
            store
                .get_queue_highwater(CaptureQueueLane::Normal)
                .await
                .expect("read highwater"),
            CaptureQueueHighwater {
                segment_id: 2,
                byte_offset: 0,
            }
        );
        let regressed = store
            .append_capture_batch(
                &[],
                Some((
                    CaptureQueueLane::Normal,
                    CaptureQueueHighwater {
                        segment_id: 1,
                        byte_offset: 999,
                    },
                )),
            )
            .await
            .expect_err("highwater regression must fail closed");
        assert!(matches!(
            regressed,
            StorageBackendError::CorruptCommittedFrame { .. }
        ));

        // --- AC5: AgentBridgeStore::append_and_ack_events composite key & single-tx contiguous ack ---
        let bridge: &dyn AgentBridgeStore = &store;
        bridge
            .open_bridge_session(&BridgeSessionOpenInput {
                bridge_session_id: "bridge-ack-1".to_string(),
                repository_id: "repo-rb06".to_string(),
                worktree_id: None,
                workspace_id: None,
                parent_session_id: None,
                agent_id: Some("agent-ack".to_string()),
                now_ms: 5000,
            })
            .await
            .expect("open bridge session");

        // Batch 1: deliver seqs 1, 2, 4, 5 (missing 3) in one single WriteTransaction -> ack stops at 2!
        let tx_b1 = store.write_transaction_count();
        let out1 = bridge
            .append_and_ack_events(
                "bridge-ack-1",
                &[
                    BridgeEventAppendInput {
                        event_seq: 1,
                        event_type: "e".to_string(),
                        payload: r#"{"n":1}"#.to_string(),
                        operation_id: None,
                    },
                    BridgeEventAppendInput {
                        event_seq: 2,
                        event_type: "e".to_string(),
                        payload: r#"{"n":2}"#.to_string(),
                        operation_id: None,
                    },
                    BridgeEventAppendInput {
                        event_seq: 4,
                        event_type: "e".to_string(),
                        payload: r#"{"n":4}"#.to_string(),
                        operation_id: None,
                    },
                    BridgeEventAppendInput {
                        event_seq: 5,
                        event_type: "e".to_string(),
                        payload: r#"{"n":5}"#.to_string(),
                        operation_id: None,
                    },
                ],
                5010,
            )
            .await
            .expect("bridge batch 1");
        assert_eq!(store.write_transaction_count() - tx_b1, 1);
        assert_eq!(
            out1.per_event,
            vec![
                BridgeEventStatus::Accepted,
                BridgeEventStatus::Accepted,
                BridgeEventStatus::Accepted,
                BridgeEventStatus::Accepted,
            ]
        );
        assert_eq!(out1.last_event_seq, 5);
        assert_eq!(out1.last_acked_seq, 2);

        // Batch 2: close gap (seq 3), replay seq 2 (Duplicate), conflict seq 4 (DigestConflict),
        // reject seq 0 & oversize payload, accept seq 6 -> contiguous ack jumps atomically from 2 -> 6!
        let tx_b2 = store.write_transaction_count();
        let out2 = bridge
            .append_and_ack_events(
                "bridge-ack-1",
                &[
                    BridgeEventAppendInput {
                        event_seq: 2,
                        event_type: "e".to_string(),
                        payload: r#"{"n":2}"#.to_string(),
                        operation_id: None,
                    },
                    BridgeEventAppendInput {
                        event_seq: 4,
                        event_type: "e".to_string(),
                        payload: r#"{"n":"tampered"}"#.to_string(),
                        operation_id: None,
                    },
                    BridgeEventAppendInput {
                        event_seq: 3,
                        event_type: "e".to_string(),
                        payload: r#"{"n":3}"#.to_string(),
                        operation_id: None,
                    },
                    BridgeEventAppendInput {
                        event_seq: 0,
                        event_type: "e".to_string(),
                        payload: r#"{"n":0}"#.to_string(),
                        operation_id: None,
                    },
                    BridgeEventAppendInput {
                        event_seq: 7,
                        event_type: "e".to_string(),
                        payload: "x".repeat(MAX_BRIDGE_EVENT_PAYLOAD_BYTES + 1),
                        operation_id: None,
                    },
                    BridgeEventAppendInput {
                        event_seq: 6,
                        event_type: "e".to_string(),
                        payload: r#"{"n":6}"#.to_string(),
                        operation_id: None,
                    },
                ],
                5020,
            )
            .await
            .expect("bridge batch 2");
        assert_eq!(store.write_transaction_count() - tx_b2, 1);
        assert_eq!(
            out2.per_event,
            vec![
                BridgeEventStatus::Duplicate,
                BridgeEventStatus::DigestConflict,
                BridgeEventStatus::Accepted,
                BridgeEventStatus::Rejected,
                BridgeEventStatus::Rejected,
                BridgeEventStatus::Accepted,
            ]
        );
        assert_eq!(out2.last_event_seq, 6);
        assert_eq!(out2.last_acked_seq, 6);

        let persisted_session = bridge
            .get_bridge_session("bridge-ack-1")
            .await
            .expect("get bridge session")
            .expect("bridge session exists");
        assert_eq!(persisted_session.last_event_seq, 6);
        assert_eq!(persisted_session.last_acked_seq, 6);

        // Verify seq 4 kept original payload (DigestConflict did not overwrite)
        let ev4 = bridge
            .get_bridge_event_by_seq("bridge-ack-1", 4)
            .await
            .expect("get seq 4")
            .expect("seq 4 exists");
        assert_eq!(ev4.payload, r#"{"n":4}"#);
    }

    /// AC6: When `capture.redb` carries a future `META_SCHEMA`
    /// (`min_reader_version > MAX_SUPPORTED_READER_VERSION`), opening or accessing
    /// `RedbCaptureStore` fails closed with `LBR-AGENT-048` (`SchemaTooNew`)
    /// without blocking normal `RedbGitStore` read/write operations on `git.redb`.
    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    async fn redb_capture_future_schema_isolated_from_git_store() {
        use crate::{
            internal::db::schema::DatabaseRole,
            storage::{
                RedbGitStore,
                codec::encode_versioned,
                git_store::{GitStore, RefCasOutcome, RefKey, RefTarget},
                meta::{
                    MAX_SUPPORTED_READER_VERSION, META_SCHEMA_CODEC_VERSION,
                    META_SCHEMA_SINGLETON_KEY, META_SCHEMA_TABLE, MetaSchemaRecord, MetaSchemaRole,
                },
                opener::{RedbRepairContext, open_redb_writable},
            },
        };

        let dir = tempfile::tempdir().expect("tempdir");
        let git_path = dir.path().join("redb").join("git.gen-000001.redb");
        let capture_path = dir.path().join("private").join("capture.gen-000001.redb");
        std::fs::create_dir_all(git_path.parent().expect("git parent")).expect("mkdir redb");
        std::fs::create_dir_all(capture_path.parent().expect("capture parent"))
            .expect("mkdir private");

        // Create healthy git.redb and capture.redb
        let git_store = RedbGitStore::create(&git_path).expect("create RedbGitStore");
        let capture_store = RedbCaptureStore::create(&capture_path)
            .await
            .expect("create RedbCaptureStore");
        drop(capture_store);

        // Advance capture.redb META_SCHEMA to a future unsupported min_reader_version
        let future_ver = MAX_SUPPORTED_READER_VERSION + 7;
        {
            let raw_db = open_redb_writable(
                &capture_path,
                DatabaseRole::Capture,
                RedbRepairContext::WriteCommand,
            )
            .expect("raw open capture.redb");
            let wtx = raw_db.begin_write().expect("begin write");
            {
                let mut meta = wtx.open_table(META_SCHEMA_TABLE).expect("open meta_schema");
                let encoded = encode_versioned(
                    META_SCHEMA_CODEC_VERSION,
                    &MetaSchemaRecord {
                        role: MetaSchemaRole::Capture,
                        format_version: future_ver,
                        min_reader_version: future_ver,
                    },
                )
                .expect("encode future META_SCHEMA");
                meta.insert(META_SCHEMA_SINGLETON_KEY, encoded.as_slice())
                    .expect("write future META_SCHEMA");
            }
            wtx.commit().expect("commit future META_SCHEMA");
        }

        // Both writable and read-only RedbCaptureStore opens must fail closed with LBR-AGENT-048
        let rw_err =
            RedbCaptureStore::open_read_write(&capture_path, RedbRepairContext::WriteCommand)
                .await
                .expect_err("future capture.redb rw open must fail closed");
        assert!(matches!(
            rw_err,
            StorageBackendError::SchemaTooNew {
                role: DatabaseRole::Capture,
                found,
                max_supported: MAX_SUPPORTED_READER_VERSION,
            } if found == future_ver
        ));
        assert_eq!(rw_err.stable_code_str(), "LBR-AGENT-048");

        let ro_err = RedbCaptureStore::open_read_only(&capture_path)
            .await
            .expect_err("future capture.redb ro open must fail closed");
        assert!(matches!(
            ro_err,
            StorageBackendError::SchemaTooNew {
                role: DatabaseRole::Capture,
                found,
                max_supported: MAX_SUPPORTED_READER_VERSION,
            } if found == future_ver
        ));
        assert_eq!(ro_err.stable_code_str(), "LBR-AGENT-048");

        // Meanwhile, RedbGitStore on git.redb continues normal read/write unimpeded!
        let main_key = RefKey::LocalBranch {
            name: "main".to_string(),
        };
        let oid = "1111111111111111111111111111111111111111".to_string();
        let target = RefTarget::Direct(oid);
        let cas = git_store
            .update_ref_cas(&main_key, None, Some(&target), &[])
            .await
            .expect("git_store write must succeed while capture.redb is SchemaTooNew");
        assert_eq!(
            cas,
            RefCasOutcome::Applied {
                previous: None,
                current: Some(target.clone()),
            }
        );
        let read_back = git_store
            .get_ref(&main_key)
            .await
            .expect("git_store read must succeed")
            .expect("branch exists");
        assert_eq!(read_back, target);

        let git_ro =
            RedbGitStore::open_read_only(&git_path).expect("git_store read-only open must succeed");
        assert_eq!(
            git_ro.get_ref(&main_key).await.expect("git_ro read"),
            Some(target)
        );
    }

    /// Smoke test ensuring `SqliteCaptureStore` builds and works cleanly under
    /// `--no-default-features --features storage-sqlite`.
    #[cfg(feature = "storage-sqlite")]
    #[tokio::test]
    async fn sqlite_capture_store_smoke() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sqlite_path = dir.path().join("capture.db");
        let store = SqliteCaptureStore::create(&sqlite_path)
            .await
            .expect("create SqliteCaptureStore");
        assert_eq!(store.backend_kind(), StorageBackendKind::Sqlite);
        assert!(!store.is_read_only());

        let live = store
            .list_live_source_tables()
            .await
            .expect("list live sqlite tables");
        assert_eq!(live.len(), CAPTURE_SOURCE_TABLES_COUNT);

        store
            .upsert_session(&sample_session("sess-sq", "prov-sq", 100))
            .await
            .expect("upsert sqlite session");
        assert!(
            store
                .get_session("sess-sq")
                .await
                .expect("get sqlite session")
                .is_some()
        );
    }
}
