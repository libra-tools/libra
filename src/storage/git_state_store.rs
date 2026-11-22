//! Object-safe [`GitStateStore`] repository workflow, worktree/workspace lease,
//! OpLog v2, change-lineage, dirty-cache, layer, and sparse-view storage trait
//! (`RB-05`).
//!
//! Covers the 21 active repository state tables assigned to `RB-05`:
//! - Workflow sequencers (3): `sequence_state`, `rebase_state`, `bisect_state`
//! - Worktree & workspace leases (4): `worktree_registry_capability`,
//!   `worktree_lifecycle`, `worktree_intent_journal`, `workspace_record`
//! - OpLog v2 (4): `operation`, `operation_parent`, `operation_head`,
//!   `operation_journal`
//! - Change lineage & AI operation link (4): `change_identity`,
//!   `change_revision`, `change_predecessor`, `ai_operation_link`
//! - Dirty cache, overlay layers & sparse view (6): `working_dirty`,
//!   `working_dirty_meta`, `layer`, `layer_path`, `sparse_view`,
//!   `sparse_view_meta`
//!
//! Implemented identically across [`SqliteGitStateStore`] and
//! [`RedbGitStateStore`].

use std::{
    collections::{BTreeSet, HashSet},
    fmt,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::storage::{
    backend::StorageBackendKind,
    error::{StorageBackendError, StorageResult},
    git_store::{validate_hex_oid, validate_non_empty_identifier},
};

#[cfg(feature = "storage-redb")]
#[path = "redb/git_state_store.rs"]
pub mod redb;
#[cfg(feature = "storage-sqlite")]
#[path = "sqlite/git_state_store.rs"]
pub mod sqlite;

#[cfg(feature = "storage-redb")]
pub use redb::RedbGitStateStore;
#[cfg(feature = "storage-sqlite")]
pub use sqlite::SqliteGitStateStore;

/// Reserved sentinel `op_id` row stored in `operation_head` so scope generation
/// counters remain monotonic even when the logical head set becomes empty
/// (`src/internal/operation/store.rs::HEAD_GENERATION_SENTINEL`).
pub const OP_HEAD_GENERATION_SENTINEL: &str = "__scope_generation__";

/// Baseline `worktree_registry_capability` versions seeded by repository schema
/// initialization (`2026072401`, `2026073005`, `2026101902`).
pub const INITIAL_WORKTREE_REGISTRY_CAPABILITIES: [i64; 3] = [2, 3, 4];

/// Static manifest of the 21 workflow, worktree/workspace, OpLog v2, change
/// lineage, and dirty/layer/sparse repository source tables implemented by
/// `RB-05`.
pub const RB05_WORKFLOW_STATE_SOURCE_TABLES: [&str; 21] = [
    "sequence_state",
    "rebase_state",
    "bisect_state",
    "worktree_registry_capability",
    "worktree_lifecycle",
    "worktree_intent_journal",
    "workspace_record",
    "operation",
    "operation_parent",
    "operation_head",
    "operation_journal",
    "change_identity",
    "change_revision",
    "change_predecessor",
    "ai_operation_link",
    "working_dirty",
    "working_dirty_meta",
    "layer",
    "layer_path",
    "sparse_view",
    "sparse_view_meta",
];

// =============================================================================
// Group 1: Sequencer / Workflow State (`sequence_state`, `rebase_state`, `bisect_state`)
// =============================================================================

/// Per-worktree active multi-step sequencer row (`sequence_state`).
///
/// `worktree_id == ""` identifies the main worktree; non-empty values identify
/// linked worktrees. Supports SHA-1, SHA-256, and all-zero OIDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceStateRecord {
    pub worktree_id: String,
    pub kind: String,
    pub head_name: String,
    pub head_orig: String,
    pub current_oid: String,
    pub todo: String,
    pub payload: String,
    pub updated_at: String,
}

/// Per-worktree active rebase state row (`rebase_state`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebaseStateRecord {
    pub worktree_id: String,
    pub head_name: String,
    pub onto: String,
    pub orig_head: String,
    pub current_head: String,
    pub todo: String,
    pub todo_actions: String,
    pub done: String,
    pub stopped_sha: Option<String>,
    pub autosquash: bool,
    pub empty_mode: String,
}

/// Per-worktree active bisect session row (`bisect_state`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BisectStateRecord {
    pub worktree_id: String,
    pub orig_head: String,
    pub orig_head_name: Option<String>,
    pub bad: Option<String>,
    pub good: String,
    pub current: Option<String>,
    pub skipped: Option<String>,
    pub steps: Option<i64>,
    pub completed: bool,
    pub first_parent: bool,
}

// =============================================================================
// Group 2: Worktree Registry, Lifecycle, Intent Journal & Workspace Lease
// =============================================================================

/// Lifecycle state stored in `worktree_lifecycle.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum WorktreeLifecycleState {
    DetachedFromRegistry,
    Tombstone,
}

impl WorktreeLifecycleState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DetachedFromRegistry => "detached_from_registry",
            Self::Tombstone => "tombstone",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "detached_from_registry" => Ok(Self::DetachedFromRegistry),
            "tombstone" => Ok(Self::Tombstone),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid worktree_lifecycle state value".to_string(),
            }),
        }
    }
}

/// One row in `worktree_lifecycle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeLifecycleRecord {
    pub worktree_id: String,
    pub state: WorktreeLifecycleState,
    pub path: String,
    pub reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Intent operation stored in `worktree_intent_journal.op`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum WorktreeIntentOp {
    Add,
    Move,
    Remove,
    Prune,
    Migrate,
}

impl WorktreeIntentOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Move => "move",
            Self::Remove => "remove",
            Self::Prune => "prune",
            Self::Migrate => "migrate",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "add" => Ok(Self::Add),
            "move" => Ok(Self::Move),
            "remove" => Ok(Self::Remove),
            "prune" => Ok(Self::Prune),
            "migrate" => Ok(Self::Migrate),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid worktree_intent_journal op value".to_string(),
            }),
        }
    }
}

/// Input payload for appending one entry to `worktree_intent_journal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeIntentWrite {
    pub op: WorktreeIntentOp,
    pub worktree_id: Option<String>,
    pub payload: String,
    pub stage: Option<String>,
    pub created_at: i64,
}

/// Materialized row in `worktree_intent_journal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeIntentRecord {
    pub id: i64,
    pub op: WorktreeIntentOp,
    pub worktree_id: Option<String>,
    pub payload: String,
    pub stage: Option<String>,
    pub created_at: i64,
}

/// Storage spelling matching `workspace_record.kind` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum WorkspaceRecordKind {
    Linked,
    TaskCopy,
    TaskFuse,
    Remote,
}

impl WorkspaceRecordKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linked => "linked",
            Self::TaskCopy => "task_copy",
            Self::TaskFuse => "task_fuse",
            Self::Remote => "remote",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "linked" => Ok(Self::Linked),
            "task_copy" => Ok(Self::TaskCopy),
            "task_fuse" => Ok(Self::TaskFuse),
            "remote" => Ok(Self::Remote),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid workspace_record kind value".to_string(),
            }),
        }
    }
}

/// Storage spelling matching `workspace_record.owner_kind` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum WorkspaceRecordOwnerKind {
    Human,
    Agent,
    Automation,
}

impl WorkspaceRecordOwnerKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
            Self::Automation => "automation",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "human" => Ok(Self::Human),
            "agent" => Ok(Self::Agent),
            "automation" => Ok(Self::Automation),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid workspace_record owner_kind value".to_string(),
            }),
        }
    }
}

/// Storage spelling matching `workspace_record.state` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum WorkspaceRecordState {
    Provisioning,
    Active,
    Releasing,
    Released,
    Orphaned,
}

impl WorkspaceRecordState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provisioning => "provisioning",
            Self::Active => "active",
            Self::Releasing => "releasing",
            Self::Released => "released",
            Self::Orphaned => "orphaned",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "provisioning" => Ok(Self::Provisioning),
            "active" => Ok(Self::Active),
            "releasing" => Ok(Self::Releasing),
            "released" => Ok(Self::Released),
            "orphaned" => Ok(Self::Orphaned),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid workspace_record state value".to_string(),
            }),
        }
    }

    /// True while the row occupies `(repo_id, worktree_id)` (for `Linked`) and
    /// `(repo_id, path)` in the partial unique indexes (`2026072501`).
    #[must_use]
    pub const fn holds_identity(self) -> bool {
        matches!(self, Self::Provisioning | Self::Active | Self::Releasing)
    }
}

/// One row in `workspace_record` (`2026072501_workspace_record.sql`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRecordRow {
    pub workspace_id: String,
    pub repo_id: String,
    pub kind: WorkspaceRecordKind,
    pub worktree_id: Option<String>,
    pub path: String,
    pub owner_kind: WorkspaceRecordOwnerKind,
    pub owner_id: Option<String>,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub base_commit: Option<String>,
    pub branch: Option<String>,
    pub state: WorkspaceRecordState,
    pub lease_owner: Option<String>,
    pub lease_fence: i64,
    pub lease_expires_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Result of [`GitStateStore::acquire_workspace_lease`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceLeaseAcquireOutcome {
    /// Lease record inserted cleanly with all partial-unique invariants satisfied.
    Acquired(Box<WorkspaceRecordRow>),
    /// Another live workspace already holds the identity or canonical path.
    Conflict {
        conflicting_workspace_id: String,
        path_conflict: bool,
    },
}

/// Result of a conditional owner+fence or expiry-reclaim lease mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceLeaseMutationOutcome {
    /// Mutation succeeded atomically.
    Updated(Box<WorkspaceRecordRow>),
    /// Row exists, but its current state, owner, fence, or expiry rejected the mutation.
    LeaseLost {
        actual_state: WorkspaceRecordState,
        actual_owner: Option<String>,
        actual_fence: i64,
    },
    /// No `workspace_record` exists with that `workspace_id`.
    NotFound,
}

// =============================================================================
// Group 3: OpLog v2 (`operation`, `operation_parent`, `operation_head`, `operation_journal`)
// =============================================================================

/// Storage spelling matching `operation_scope_kind_domain_*` trigger (`2026091901`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum OperationScopeKind {
    Main,
    Linked,
    Repository,
    Unknown,
}

impl OperationScopeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Linked => "linked",
            Self::Repository => "repository",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "main" => Ok(Self::Main),
            "linked" => Ok(Self::Linked),
            "repository" => Ok(Self::Repository),
            "unknown" => Ok(Self::Unknown),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid operation scope_kind value".to_string(),
            }),
        }
    }
}

/// Storage spelling matching `operation_scope_provenance_domain_*` trigger (`2026091901`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum OperationScopeProvenance {
    Declared,
    Unknown,
}

impl OperationScopeProvenance {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "declared" => Ok(Self::Declared),
            "unknown" => Ok(Self::Unknown),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid operation scope_provenance value".to_string(),
            }),
        }
    }
}

/// One row in `operation` (`2026090101` + `2026091901`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRecord {
    pub op_id: String,
    pub repo_id: String,
    pub format_version: i64,
    pub kind: String,
    pub status: String,
    pub command_name: Option<String>,
    pub description: Option<String>,
    pub args_digest: Option<String>,
    pub actor: Option<String>,
    pub worktree_id: Option<String>,
    pub scope_kind: OperationScopeKind,
    pub scope_provenance: OperationScopeProvenance,
    pub pre_view_oid: String,
    pub post_view_oid: String,
    pub restores_op_id: Option<String>,
    pub reverts_op_id: Option<String>,
    pub predecessor_map_oid: Option<String>,
    pub causal_context_id: Option<String>,
    pub restorable: bool,
    pub control_slot: Option<String>,
    pub claim_owner: Option<String>,
    pub start_ts: i64,
    pub end_ts: Option<i64>,
}

/// One row in `operation_journal` (`2026090101`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationJournalRecord {
    pub journal_id: String,
    pub op_id: String,
    pub phase: String,
    pub pre_view_oid: Option<String>,
    pub target_view_oid: Option<String>,
    pub owner: String,
    pub updated_at: i64,
    pub recovery_payload: Option<String>,
}

/// Current head set and monotonic scope generation for `(repo_id, scope_key)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationHeadsSnapshot {
    pub generation: u64,
    pub heads: Vec<String>,
}

/// Result of [`GitStateStore::cas_update_operation_heads`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationHeadCasOutcome {
    Applied {
        generation: u64,
        heads: Vec<String>,
    },
    CasConflict {
        actual_generation: u64,
        actual_heads: Vec<String>,
    },
}

/// Result of [`GitStateStore::converge_operation_heads`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationHeadConvergeOutcome {
    pub generation: u64,
    pub heads: Vec<String>,
    /// `true` when the live head set matched `expected_heads` exactly;
    /// `false` when concurrent sibling heads were retained alongside `replacement_heads`.
    pub converged_cleanly: bool,
}

// =============================================================================
// Group 4: Change Lineage & AI Operation Link
// =============================================================================

/// One row in `change_identity`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeIdentityRecord {
    pub change_id: String,
    pub repo_id: String,
    pub origin: String,
    pub created_op_id: String,
    pub created_at: i64,
}

/// One row in `change_revision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRevisionRecord {
    pub change_id: String,
    pub commit_oid: String,
    pub created_op_id: String,
    pub visibility: String,
    pub revision_ordinal: i64,
}

/// One row in `change_predecessor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangePredecessorRecord {
    pub successor_oid: String,
    pub predecessor_oid: String,
    pub op_id: String,
    pub relation_kind: String,
    pub ordinal: i64,
}

/// One row in `ai_operation_link` (`2026090101` + `2026090802`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiOperationLinkRecord {
    pub operation_id: String,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub tool_invocation_id: Option<String>,
    pub intent_id: Option<String>,
    pub repo_id: String,
    pub worktree_id: Option<String>,
    pub workspace_id: Option<String>,
    pub lease_generation: Option<i64>,
    pub config_provenance_digest: Option<String>,
    pub redaction_version: String,
    pub change_id: Option<String>,
}

// =============================================================================
// Group 5: Dirty Cache, Layers & Sparse View (6 worktree-scoped tables)
// =============================================================================

/// One row in `working_dirty`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingDirtyEntryRecord {
    pub worktree_id: String,
    pub path: String,
    pub kind: String,
    pub source: String,
    pub marked_at: String,
    pub verified_at: Option<String>,
}

/// One row in `working_dirty_meta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingDirtyMetaRecord {
    pub worktree_id: String,
    pub state: String,
    pub index_fingerprint: Option<String>,
    pub head_oid: Option<String>,
    pub scanned_at: Option<String>,
    pub scan_lock_pid: Option<i64>,
    pub scan_lock_at: Option<String>,
}

/// One row in `layer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerRecord {
    pub worktree_id: String,
    pub name: String,
    pub source: String,
    pub priority: i64,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// One row in `layer_path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerPathRecord {
    pub worktree_id: String,
    pub layer_name: String,
    pub path: String,
    pub content_hash: String,
    pub materialized_at: String,
}

/// Combined per-worktree snapshot of `sparse_view_meta` + ordered `sparse_view` rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SparseViewSnapshot {
    pub worktree_id: String,
    pub enabled: bool,
    pub patterns: Vec<String>,
}

// =============================================================================
// Validation helpers shared by SQLite and redb implementations
// =============================================================================

pub(crate) fn validate_no_nul(value: &str, label: &'static str) -> StorageResult<()> {
    if value.contains('\0') {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("{label} must not contain NUL bytes"),
        });
    }
    Ok(())
}

pub(crate) fn validate_opt_no_nul(value: Option<&str>, label: &'static str) -> StorageResult<()> {
    if let Some(v) = value {
        validate_no_nul(v, label)?;
    }
    Ok(())
}

pub(crate) fn validate_opt_hex_oid(value: Option<&str>, label: &'static str) -> StorageResult<()> {
    if let Some(v) = value {
        validate_hex_oid(v, label)?;
    }
    Ok(())
}

pub(crate) fn validate_sequence_state(rec: &SequenceStateRecord) -> StorageResult<()> {
    validate_no_nul(&rec.worktree_id, "sequence_state worktree_id")?;
    validate_non_empty_identifier(&rec.kind, "sequence_state kind")?;
    validate_no_nul(&rec.head_name, "sequence_state head_name")?;
    validate_hex_oid(&rec.head_orig, "sequence_state head_orig")?;
    validate_hex_oid(&rec.current_oid, "sequence_state current_oid")?;
    validate_no_nul(&rec.todo, "sequence_state todo")?;
    validate_no_nul(&rec.payload, "sequence_state payload")?;
    validate_non_empty_identifier(&rec.updated_at, "sequence_state updated_at")
}

pub(crate) fn validate_rebase_state(rec: &RebaseStateRecord) -> StorageResult<()> {
    validate_no_nul(&rec.worktree_id, "rebase_state worktree_id")?;
    validate_no_nul(&rec.head_name, "rebase_state head_name")?;
    validate_hex_oid(&rec.onto, "rebase_state onto")?;
    validate_hex_oid(&rec.orig_head, "rebase_state orig_head")?;
    validate_hex_oid(&rec.current_head, "rebase_state current_head")?;
    validate_no_nul(&rec.todo, "rebase_state todo")?;
    validate_no_nul(&rec.todo_actions, "rebase_state todo_actions")?;
    validate_no_nul(&rec.done, "rebase_state done")?;
    validate_opt_hex_oid(rec.stopped_sha.as_deref(), "rebase_state stopped_sha")?;
    validate_non_empty_identifier(&rec.empty_mode, "rebase_state empty_mode")
}

pub(crate) fn validate_bisect_state(rec: &BisectStateRecord) -> StorageResult<()> {
    validate_no_nul(&rec.worktree_id, "bisect_state worktree_id")?;
    validate_hex_oid(&rec.orig_head, "bisect_state orig_head")?;
    validate_opt_no_nul(rec.orig_head_name.as_deref(), "bisect_state orig_head_name")?;
    validate_opt_hex_oid(rec.bad.as_deref(), "bisect_state bad")?;
    validate_no_nul(&rec.good, "bisect_state good")?;
    validate_opt_hex_oid(rec.current.as_deref(), "bisect_state current")?;
    validate_opt_no_nul(rec.skipped.as_deref(), "bisect_state skipped")
}

pub(crate) fn validate_worktree_lifecycle(rec: &WorktreeLifecycleRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.worktree_id, "worktree_lifecycle worktree_id")?;
    validate_non_empty_identifier(&rec.path, "worktree_lifecycle path")?;
    validate_opt_no_nul(rec.reason.as_deref(), "worktree_lifecycle reason")
}

pub(crate) fn validate_worktree_intent_write(req: &WorktreeIntentWrite) -> StorageResult<()> {
    if let Some(wt) = req.worktree_id.as_deref() {
        validate_non_empty_identifier(wt, "worktree_intent_journal worktree_id")?;
    }
    validate_no_nul(&req.payload, "worktree_intent_journal payload")?;
    validate_opt_no_nul(req.stage.as_deref(), "worktree_intent_journal stage")
}

pub(crate) fn validate_workspace_record(rec: &WorkspaceRecordRow) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.workspace_id, "workspace_record workspace_id")?;
    validate_non_empty_identifier(&rec.repo_id, "workspace_record repo_id")?;
    if rec.kind == WorkspaceRecordKind::Linked {
        match rec.worktree_id.as_deref() {
            Some(wt) => validate_non_empty_identifier(wt, "workspace_record worktree_id")?,
            None => {
                return Err(StorageBackendError::BackendInternal {
                    detail: "linked workspace_record requires a non-empty worktree_id".to_string(),
                });
            }
        }
    } else if let Some(wt) = rec.worktree_id.as_deref() {
        validate_non_empty_identifier(wt, "workspace_record worktree_id")?;
    }
    validate_non_empty_identifier(&rec.path, "workspace_record path")?;
    validate_opt_no_nul(rec.owner_id.as_deref(), "workspace_record owner_id")?;
    validate_opt_no_nul(rec.task_id.as_deref(), "workspace_record task_id")?;
    validate_opt_no_nul(rec.session_id.as_deref(), "workspace_record session_id")?;
    validate_opt_hex_oid(rec.base_commit.as_deref(), "workspace_record base_commit")?;
    validate_opt_no_nul(rec.branch.as_deref(), "workspace_record branch")?;
    validate_opt_no_nul(rec.lease_owner.as_deref(), "workspace_record lease_owner")?;
    if rec.lease_fence < 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: "workspace_record lease_fence must be non-negative".to_string(),
        });
    }
    Ok(())
}

pub(crate) fn validate_operation_record(
    op: &OperationRecord,
    parent_op_ids: &[String],
) -> StorageResult<()> {
    validate_non_empty_identifier(&op.op_id, "operation op_id")?;
    if op.op_id == OP_HEAD_GENERATION_SENTINEL {
        return Err(StorageBackendError::BackendInternal {
            detail: "operation op_id must not use reserved generation sentinel".to_string(),
        });
    }
    validate_non_empty_identifier(&op.repo_id, "operation repo_id")?;
    validate_non_empty_identifier(&op.kind, "operation kind")?;
    validate_non_empty_identifier(&op.status, "operation status")?;
    validate_opt_no_nul(op.command_name.as_deref(), "operation command_name")?;
    validate_opt_no_nul(op.description.as_deref(), "operation description")?;
    validate_opt_no_nul(op.args_digest.as_deref(), "operation args_digest")?;
    validate_opt_no_nul(op.actor.as_deref(), "operation actor")?;
    validate_opt_no_nul(op.worktree_id.as_deref(), "operation worktree_id")?;
    validate_hex_oid(&op.pre_view_oid, "operation pre_view_oid")?;
    validate_hex_oid(&op.post_view_oid, "operation post_view_oid")?;
    validate_opt_no_nul(op.restores_op_id.as_deref(), "operation restores_op_id")?;
    validate_opt_no_nul(op.reverts_op_id.as_deref(), "operation reverts_op_id")?;
    validate_opt_hex_oid(
        op.predecessor_map_oid.as_deref(),
        "operation predecessor_map_oid",
    )?;
    validate_opt_no_nul(
        op.causal_context_id.as_deref(),
        "operation causal_context_id",
    )?;
    validate_opt_no_nul(op.control_slot.as_deref(), "operation control_slot")?;
    validate_opt_no_nul(op.claim_owner.as_deref(), "operation claim_owner")?;

    let mut seen_parents = HashSet::with_capacity(parent_op_ids.len());
    for parent in parent_op_ids {
        validate_non_empty_identifier(parent, "operation parent_op_id")?;
        if parent == &op.op_id {
            return Err(StorageBackendError::BackendInternal {
                detail: "operation cannot list itself as a parent".to_string(),
            });
        }
        if !seen_parents.insert(parent.as_str()) {
            return Err(StorageBackendError::BackendInternal {
                detail: "operation parent_op_ids must not contain duplicates".to_string(),
            });
        }
    }
    Ok(())
}

pub(crate) fn validate_operation_journal(entry: &OperationJournalRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&entry.journal_id, "operation_journal journal_id")?;
    validate_non_empty_identifier(&entry.op_id, "operation_journal op_id")?;
    validate_non_empty_identifier(&entry.phase, "operation_journal phase")?;
    validate_opt_hex_oid(
        entry.pre_view_oid.as_deref(),
        "operation_journal pre_view_oid",
    )?;
    validate_opt_hex_oid(
        entry.target_view_oid.as_deref(),
        "operation_journal target_view_oid",
    )?;
    validate_non_empty_identifier(&entry.owner, "operation_journal owner")?;
    validate_opt_no_nul(
        entry.recovery_payload.as_deref(),
        "operation_journal recovery_payload",
    )
}

pub(crate) fn normalize_op_heads(heads: &[String]) -> StorageResult<Vec<String>> {
    let mut set = BTreeSet::new();
    for h in heads {
        validate_non_empty_identifier(h, "operation_head op_id")?;
        if h == OP_HEAD_GENERATION_SENTINEL {
            return Err(StorageBackendError::BackendInternal {
                detail: "operation_head op_id must not use reserved generation sentinel"
                    .to_string(),
            });
        }
        if !set.insert(h.clone()) {
            return Err(StorageBackendError::BackendInternal {
                detail: "operation_head list must not contain duplicates".to_string(),
            });
        }
    }
    Ok(set.into_iter().collect())
}

pub(crate) fn validate_change_identity(rec: &ChangeIdentityRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.change_id, "change_identity change_id")?;
    validate_non_empty_identifier(&rec.repo_id, "change_identity repo_id")?;
    validate_non_empty_identifier(&rec.origin, "change_identity origin")?;
    validate_non_empty_identifier(&rec.created_op_id, "change_identity created_op_id")
}

pub(crate) fn validate_change_revision(rec: &ChangeRevisionRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.change_id, "change_revision change_id")?;
    validate_hex_oid(&rec.commit_oid, "change_revision commit_oid")?;
    validate_non_empty_identifier(&rec.created_op_id, "change_revision created_op_id")?;
    validate_non_empty_identifier(&rec.visibility, "change_revision visibility")
}

pub(crate) fn validate_change_predecessor(rec: &ChangePredecessorRecord) -> StorageResult<()> {
    validate_hex_oid(&rec.successor_oid, "change_predecessor successor_oid")?;
    validate_hex_oid(&rec.predecessor_oid, "change_predecessor predecessor_oid")?;
    validate_non_empty_identifier(&rec.op_id, "change_predecessor op_id")?;
    validate_non_empty_identifier(&rec.relation_kind, "change_predecessor relation_kind")
}

pub(crate) fn validate_ai_operation_link(rec: &AiOperationLinkRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.operation_id, "ai_operation_link operation_id")?;
    validate_non_empty_identifier(&rec.repo_id, "ai_operation_link repo_id")?;
    validate_non_empty_identifier(
        &rec.redaction_version,
        "ai_operation_link redaction_version",
    )?;
    validate_opt_no_nul(rec.session_id.as_deref(), "ai_operation_link session_id")?;
    validate_opt_no_nul(rec.run_id.as_deref(), "ai_operation_link run_id")?;
    validate_opt_no_nul(
        rec.tool_invocation_id.as_deref(),
        "ai_operation_link tool_invocation_id",
    )?;
    validate_opt_no_nul(rec.intent_id.as_deref(), "ai_operation_link intent_id")?;
    validate_opt_no_nul(rec.worktree_id.as_deref(), "ai_operation_link worktree_id")?;
    validate_opt_no_nul(
        rec.workspace_id.as_deref(),
        "ai_operation_link workspace_id",
    )?;
    validate_opt_no_nul(
        rec.config_provenance_digest.as_deref(),
        "ai_operation_link config_provenance_digest",
    )?;
    validate_opt_no_nul(rec.change_id.as_deref(), "ai_operation_link change_id")
}

pub(crate) fn validate_working_dirty_entry(rec: &WorkingDirtyEntryRecord) -> StorageResult<()> {
    validate_no_nul(&rec.worktree_id, "working_dirty worktree_id")?;
    validate_non_empty_identifier(&rec.path, "working_dirty path")?;
    validate_non_empty_identifier(&rec.kind, "working_dirty kind")?;
    validate_non_empty_identifier(&rec.source, "working_dirty source")?;
    validate_non_empty_identifier(&rec.marked_at, "working_dirty marked_at")?;
    validate_opt_no_nul(rec.verified_at.as_deref(), "working_dirty verified_at")
}

pub(crate) fn validate_working_dirty_meta(rec: &WorkingDirtyMetaRecord) -> StorageResult<()> {
    validate_no_nul(&rec.worktree_id, "working_dirty_meta worktree_id")?;
    validate_non_empty_identifier(&rec.state, "working_dirty_meta state")?;
    validate_opt_no_nul(
        rec.index_fingerprint.as_deref(),
        "working_dirty_meta index_fingerprint",
    )?;
    validate_opt_hex_oid(rec.head_oid.as_deref(), "working_dirty_meta head_oid")?;
    validate_opt_no_nul(rec.scanned_at.as_deref(), "working_dirty_meta scanned_at")?;
    validate_opt_no_nul(
        rec.scan_lock_at.as_deref(),
        "working_dirty_meta scan_lock_at",
    )
}

pub(crate) fn validate_layer_record(rec: &LayerRecord) -> StorageResult<()> {
    validate_no_nul(&rec.worktree_id, "layer worktree_id")?;
    validate_non_empty_identifier(&rec.name, "layer name")?;
    validate_non_empty_identifier(&rec.source, "layer source")?;
    validate_non_empty_identifier(&rec.created_at, "layer created_at")?;
    validate_non_empty_identifier(&rec.updated_at, "layer updated_at")
}

pub(crate) fn validate_layer_path_record(rec: &LayerPathRecord) -> StorageResult<()> {
    validate_no_nul(&rec.worktree_id, "layer_path worktree_id")?;
    validate_non_empty_identifier(&rec.layer_name, "layer_path layer_name")?;
    validate_non_empty_identifier(&rec.path, "layer_path path")?;
    validate_non_empty_identifier(&rec.content_hash, "layer_path content_hash")?;
    validate_non_empty_identifier(&rec.materialized_at, "layer_path materialized_at")
}

// =============================================================================
// Object-Safe `GitStateStore` Trait
// =============================================================================

/// Object-safe dual-backend repository state store (`RB-05`).
#[async_trait]
pub trait GitStateStore: Send + Sync + fmt::Debug {
    /// Active storage backend kind (`Sqlite` or `Redb`).
    fn backend(&self) -> StorageBackendKind;

    // -------------------------------------------------------------------------
    // AC1: Sequencer / Workflow State (`sequence_state`, `rebase_state`, `bisect_state`)
    // -------------------------------------------------------------------------

    async fn get_sequence_state(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<SequenceStateRecord>>;
    async fn put_sequence_state(&self, record: &SequenceStateRecord) -> StorageResult<()>;
    async fn delete_sequence_state(&self, worktree_id: &str) -> StorageResult<bool>;
    async fn list_sequence_states(&self) -> StorageResult<Vec<SequenceStateRecord>>;

    async fn get_rebase_state(&self, worktree_id: &str)
    -> StorageResult<Option<RebaseStateRecord>>;
    async fn put_rebase_state(&self, record: &RebaseStateRecord) -> StorageResult<()>;
    async fn delete_rebase_state(&self, worktree_id: &str) -> StorageResult<bool>;
    async fn list_rebase_states(&self) -> StorageResult<Vec<RebaseStateRecord>>;

    async fn get_bisect_state(&self, worktree_id: &str)
    -> StorageResult<Option<BisectStateRecord>>;
    async fn put_bisect_state(&self, record: &BisectStateRecord) -> StorageResult<()>;
    async fn delete_bisect_state(&self, worktree_id: &str) -> StorageResult<bool>;
    async fn list_bisect_states(&self) -> StorageResult<Vec<BisectStateRecord>>;

    // -------------------------------------------------------------------------
    // AC2: Worktree Registry, Lifecycle, Intent Journal & Workspace Leases
    // -------------------------------------------------------------------------

    async fn record_worktree_registry_capability(&self, version: i64) -> StorageResult<()>;
    async fn list_worktree_registry_capabilities(&self) -> StorageResult<Vec<i64>>;

    async fn upsert_worktree_lifecycle(
        &self,
        record: &WorktreeLifecycleRecord,
    ) -> StorageResult<()>;
    async fn get_worktree_lifecycle(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<WorktreeLifecycleRecord>>;
    async fn delete_worktree_lifecycle(&self, worktree_id: &str) -> StorageResult<bool>;
    async fn list_worktree_lifecycles(&self) -> StorageResult<Vec<WorktreeLifecycleRecord>>;

    async fn append_worktree_intent(
        &self,
        intent: &WorktreeIntentWrite,
    ) -> StorageResult<WorktreeIntentRecord>;
    async fn update_worktree_intent_stage(
        &self,
        id: i64,
        stage: Option<&str>,
    ) -> StorageResult<bool>;
    async fn resolve_worktree_intent(&self, id: i64) -> StorageResult<bool>;
    async fn list_worktree_intents(&self) -> StorageResult<Vec<WorktreeIntentRecord>>;

    async fn acquire_workspace_lease(
        &self,
        record: &WorkspaceRecordRow,
    ) -> StorageResult<WorkspaceLeaseAcquireOutcome>;
    async fn renew_workspace_lease(
        &self,
        workspace_id: &str,
        expected_owner: &str,
        expected_fence: i64,
        new_expires_at: Option<i64>,
        updated_at: i64,
    ) -> StorageResult<WorkspaceLeaseMutationOutcome>;
    async fn transition_workspace_lease(
        &self,
        workspace_id: &str,
        expected_owner: &str,
        expected_fence: i64,
        new_state: WorkspaceRecordState,
        updated_at: i64,
    ) -> StorageResult<WorkspaceLeaseMutationOutcome>;
    async fn reclaim_expired_workspace_lease(
        &self,
        workspace_id: &str,
        now_ts: i64,
        new_owner: &str,
        new_expires_at: Option<i64>,
        new_state: WorkspaceRecordState,
        updated_at: i64,
    ) -> StorageResult<WorkspaceLeaseMutationOutcome>;
    async fn get_workspace_record(
        &self,
        workspace_id: &str,
    ) -> StorageResult<Option<WorkspaceRecordRow>>;
    async fn list_workspace_records(
        &self,
        repo_id: &str,
        state_filter: Option<WorkspaceRecordState>,
    ) -> StorageResult<Vec<WorkspaceRecordRow>>;

    // -------------------------------------------------------------------------
    // AC3: OpLog v2 (`operation`, `operation_parent`, `operation_head`, `operation_journal`)
    // -------------------------------------------------------------------------

    async fn append_operation_atomic(
        &self,
        op: &OperationRecord,
        parent_op_ids: &[String],
        journal: Option<&OperationJournalRecord>,
    ) -> StorageResult<()>;
    /// Atomically appends an `operation` row, its ordered `operation_parent`
    /// edges, an optional `operation_journal` row, AND converges `operation_head`
    /// for `(op.repo_id, scope_key)` inside a single backend transaction.
    async fn append_operation_and_converge_heads(
        &self,
        op: &OperationRecord,
        parent_op_ids: &[String],
        journal: Option<&OperationJournalRecord>,
        scope_key: &str,
        expected_heads: &[String],
        replacement_heads: &[String],
    ) -> StorageResult<OperationHeadConvergeOutcome>;
    async fn update_operation_status(
        &self,
        repo_id: &str,
        op_id: &str,
        status: &str,
        post_view_oid: Option<&str>,
        end_ts: Option<i64>,
    ) -> StorageResult<bool>;
    async fn get_operation(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> StorageResult<Option<(OperationRecord, Vec<String>)>>;
    async fn list_operations(
        &self,
        repo_id: &str,
    ) -> StorageResult<Vec<(OperationRecord, Vec<String>)>>;

    async fn upsert_operation_journal(&self, entry: &OperationJournalRecord) -> StorageResult<()>;
    async fn list_operation_journals(
        &self,
        op_id: &str,
    ) -> StorageResult<Vec<OperationJournalRecord>>;
    async fn delete_operation_journal(&self, journal_id: &str) -> StorageResult<bool>;

    async fn get_operation_heads(
        &self,
        repo_id: &str,
        scope_key: &str,
    ) -> StorageResult<OperationHeadsSnapshot>;
    async fn cas_update_operation_heads(
        &self,
        repo_id: &str,
        scope_key: &str,
        expected_generation: Option<u64>,
        expected_heads: &[String],
        new_heads: &[String],
    ) -> StorageResult<OperationHeadCasOutcome>;
    async fn converge_operation_heads(
        &self,
        repo_id: &str,
        scope_key: &str,
        expected_heads: &[String],
        replacement_heads: &[String],
    ) -> StorageResult<OperationHeadConvergeOutcome>;

    // -------------------------------------------------------------------------
    // AC4: Change Lineage & AI Operation Link
    // -------------------------------------------------------------------------

    async fn upsert_change_identity(&self, record: &ChangeIdentityRecord) -> StorageResult<()>;
    async fn get_change_identity(
        &self,
        change_id: &str,
    ) -> StorageResult<Option<ChangeIdentityRecord>>;
    async fn list_change_identities_by_prefix(
        &self,
        repo_id: &str,
        prefix: &str,
    ) -> StorageResult<Vec<ChangeIdentityRecord>>;

    async fn upsert_change_revision(&self, record: &ChangeRevisionRecord) -> StorageResult<()>;
    async fn list_change_revisions(
        &self,
        change_id: &str,
    ) -> StorageResult<Vec<ChangeRevisionRecord>>;
    async fn find_change_revisions_by_commit(
        &self,
        commit_oid: &str,
    ) -> StorageResult<Vec<ChangeRevisionRecord>>;

    async fn upsert_change_predecessor(
        &self,
        record: &ChangePredecessorRecord,
    ) -> StorageResult<()>;
    async fn list_change_predecessors(
        &self,
        successor_oid: &str,
    ) -> StorageResult<Vec<ChangePredecessorRecord>>;

    async fn upsert_ai_operation_link(&self, record: &AiOperationLinkRecord) -> StorageResult<()>;
    async fn get_ai_operation_link(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<AiOperationLinkRecord>>;
    async fn list_ai_operation_links_by_change(
        &self,
        repo_id: &str,
        change_id: &str,
    ) -> StorageResult<Vec<AiOperationLinkRecord>>;

    // -------------------------------------------------------------------------
    // AC5: Dirty Cache, Layers & Sparse View (6 worktree-scoped tables)
    // -------------------------------------------------------------------------

    async fn replace_working_dirty_snapshot(
        &self,
        meta: &WorkingDirtyMetaRecord,
        entries: &[WorkingDirtyEntryRecord],
    ) -> StorageResult<()>;
    async fn upsert_working_dirty_entry(
        &self,
        entry: &WorkingDirtyEntryRecord,
    ) -> StorageResult<()>;
    async fn get_working_dirty_meta(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<WorkingDirtyMetaRecord>>;
    async fn list_working_dirty_entries(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Vec<WorkingDirtyEntryRecord>>;

    async fn upsert_layer(&self, record: &LayerRecord) -> StorageResult<()>;
    async fn list_layers(&self, worktree_id: &str) -> StorageResult<Vec<LayerRecord>>;
    async fn delete_layer(&self, worktree_id: &str, name: &str) -> StorageResult<bool>;
    async fn upsert_layer_path(&self, record: &LayerPathRecord) -> StorageResult<()>;
    async fn list_layer_paths(
        &self,
        worktree_id: &str,
        layer_name: Option<&str>,
    ) -> StorageResult<Vec<LayerPathRecord>>;

    async fn replace_sparse_view(
        &self,
        worktree_id: &str,
        enabled: bool,
        patterns: &[String],
    ) -> StorageResult<()>;
    async fn get_sparse_view(&self, worktree_id: &str)
    -> StorageResult<Option<SparseViewSnapshot>>;
    async fn clear_sparse_view(&self, worktree_id: &str) -> StorageResult<()>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;

    use super::*;

    fn sha1(n: u64) -> String {
        format!("{n:040x}")
    }

    fn sha256(n: u64) -> String {
        format!("{n:064x}")
    }

    async fn run_all_rb05_ac_assertions(stores: &[Arc<dyn GitStateStore>]) {
        assert!(!stores.is_empty());
        let zero40 = "0".repeat(40);
        let zero64 = "0".repeat(64);
        let repo_id = "repo-rb05-001";

        // =====================================================================
        // AC1: `sequence_state`, `rebase_state`, `bisect_state` isolated by
        // `worktree_id` + exact all-zero SHA-1 & SHA-256 OID round-trip.
        // =====================================================================
        let seq_main = SequenceStateRecord {
            worktree_id: String::new(),
            kind: "cherry-pick".to_string(),
            head_name: "refs/heads/main".to_string(),
            head_orig: zero40.clone(),
            current_oid: sha1(10),
            todo: format!("{}\n{}", sha1(11), zero40),
            payload: "{\"no_commit\":false}".to_string(),
            updated_at: "2026-11-22T18:40:00Z".to_string(),
        };
        let seq_wt1 = SequenceStateRecord {
            worktree_id: "wt-alpha".to_string(),
            kind: "revert".to_string(),
            head_name: "refs/heads/feature".to_string(),
            head_orig: zero64.clone(),
            current_oid: zero64.clone(),
            todo: sha256(20),
            payload: String::new(),
            updated_at: "2026-11-22T18:40:01Z".to_string(),
        };

        let rebase_main = RebaseStateRecord {
            worktree_id: String::new(),
            head_name: "refs/heads/main".to_string(),
            onto: zero40.clone(),
            orig_head: sha1(30),
            current_head: zero40.clone(),
            todo: sha1(31),
            todo_actions: "pick".to_string(),
            done: String::new(),
            stopped_sha: Some(zero40.clone()),
            autosquash: true,
            empty_mode: "drop".to_string(),
        };
        let rebase_wt1 = RebaseStateRecord {
            worktree_id: "wt-alpha".to_string(),
            head_name: "refs/heads/topic".to_string(),
            onto: zero64.clone(),
            orig_head: zero64.clone(),
            current_head: sha256(40),
            todo: sha256(41),
            todo_actions: "reword".to_string(),
            done: sha256(39),
            stopped_sha: None,
            autosquash: false,
            empty_mode: "keep".to_string(),
        };

        let bisect_main = BisectStateRecord {
            worktree_id: String::new(),
            orig_head: zero40.clone(),
            orig_head_name: Some("refs/heads/main".to_string()),
            bad: Some(zero40.clone()),
            good: format!("{},{}", sha1(50), zero40),
            current: Some(sha1(51)),
            skipped: None,
            steps: Some(4),
            completed: false,
            first_parent: true,
        };
        let bisect_wt1 = BisectStateRecord {
            worktree_id: "wt-alpha".to_string(),
            orig_head: zero64.clone(),
            orig_head_name: None,
            bad: Some(sha256(60)),
            good: zero64.clone(),
            current: Some(zero64.clone()),
            skipped: Some(sha256(61)),
            steps: None,
            completed: true,
            first_parent: false,
        };

        for store in stores {
            store.put_sequence_state(&seq_main).await.expect("seq main");
            store.put_sequence_state(&seq_wt1).await.expect("seq wt1");
            assert_eq!(
                store.get_sequence_state("").await.expect("get seq main"),
                Some(seq_main.clone())
            );
            assert_eq!(
                store
                    .get_sequence_state("wt-alpha")
                    .await
                    .expect("get seq wt1"),
                Some(seq_wt1.clone())
            );
            assert_eq!(
                store
                    .get_sequence_state("wt-other")
                    .await
                    .expect("get seq none"),
                None
            );

            store
                .put_rebase_state(&rebase_main)
                .await
                .expect("rebase main");
            store
                .put_rebase_state(&rebase_wt1)
                .await
                .expect("rebase wt1");
            assert_eq!(
                store.get_rebase_state("").await.expect("get rebase main"),
                Some(rebase_main.clone())
            );
            assert_eq!(
                store
                    .get_rebase_state("wt-alpha")
                    .await
                    .expect("get rebase wt1"),
                Some(rebase_wt1.clone())
            );

            store
                .put_bisect_state(&bisect_main)
                .await
                .expect("bisect main");
            store
                .put_bisect_state(&bisect_wt1)
                .await
                .expect("bisect wt1");
            assert_eq!(
                store.get_bisect_state("").await.expect("get bisect main"),
                Some(bisect_main.clone())
            );
            assert_eq!(
                store
                    .get_bisect_state("wt-alpha")
                    .await
                    .expect("get bisect wt1"),
                Some(bisect_wt1.clone())
            );

            // Deleting main worktree state must NEVER touch linked worktree state.
            assert!(store.delete_sequence_state("").await.expect("del seq main"));
            assert!(
                !store
                    .delete_sequence_state("")
                    .await
                    .expect("del seq absent")
            );
            assert_eq!(
                store
                    .get_sequence_state("wt-alpha")
                    .await
                    .expect("wt1 intact"),
                Some(seq_wt1.clone())
            );
            store
                .put_sequence_state(&seq_main)
                .await
                .expect("restore seq main");
        }

        // =====================================================================
        // AC2: `workspace_record` + `worktree_*` (4 tables): capability,
        // lifecycle, intent journal, and workspace lease acquire/renew/revoke/reclaim.
        // =====================================================================
        let lifecycle_row = WorktreeLifecycleRecord {
            worktree_id: "wt-detached".to_string(),
            state: WorktreeLifecycleState::DetachedFromRegistry,
            path: "/repo/wt-detached".to_string(),
            reason: Some("removed keep-dir".to_string()),
            created_at: 1_795_376_000,
            updated_at: 1_795_376_001,
        };
        let intent_add = WorktreeIntentWrite {
            op: WorktreeIntentOp::Migrate,
            worktree_id: Some("wt-alpha".to_string()),
            payload: "{\"from\":\"v3\",\"to\":\"v4\"}".to_string(),
            stage: Some("copying".to_string()),
            created_at: 1_795_376_010,
        };

        let ws_linked = WorkspaceRecordRow {
            workspace_id: "ws-001".to_string(),
            repo_id: repo_id.to_string(),
            kind: WorkspaceRecordKind::Linked,
            worktree_id: Some("wt-alpha".to_string()),
            path: "/work/wt-alpha".to_string(),
            owner_kind: WorkspaceRecordOwnerKind::Agent,
            owner_id: Some("agent-1".to_string()),
            task_id: Some("task-1".to_string()),
            session_id: Some("sess-1".to_string()),
            base_commit: Some(sha1(100)),
            branch: Some("feature/rb05".to_string()),
            state: WorkspaceRecordState::Provisioning,
            lease_owner: Some("worker-A".to_string()),
            lease_fence: 1,
            lease_expires_at: Some(1_000),
            created_at: 900,
            updated_at: 900,
        };

        for store in stores {
            assert_eq!(
                store
                    .list_worktree_registry_capabilities()
                    .await
                    .expect("initial capabilities"),
                vec![2, 3, 4]
            );
            store
                .record_worktree_registry_capability(5)
                .await
                .expect("record cap 5");
            store
                .record_worktree_registry_capability(5)
                .await
                .expect("idempotent cap 5");
            assert_eq!(
                store
                    .list_worktree_registry_capabilities()
                    .await
                    .expect("capabilities after 5"),
                vec![2, 3, 4, 5]
            );

            store
                .upsert_worktree_lifecycle(&lifecycle_row)
                .await
                .expect("upsert lifecycle");
            assert_eq!(
                store
                    .get_worktree_lifecycle("wt-detached")
                    .await
                    .expect("get lifecycle"),
                Some(lifecycle_row.clone())
            );

            let intent_rec = store
                .append_worktree_intent(&intent_add)
                .await
                .expect("append intent");
            assert_eq!(intent_rec.op, WorktreeIntentOp::Migrate);
            assert!(
                store
                    .update_worktree_intent_stage(intent_rec.id, Some("switching"))
                    .await
                    .expect("update intent stage")
            );
            let intents = store.list_worktree_intents().await.expect("list intents");
            assert_eq!(intents.len(), 1);
            assert_eq!(intents[0].stage.as_deref(), Some("switching"));
            assert!(
                store
                    .resolve_worktree_intent(intent_rec.id)
                    .await
                    .expect("resolve intent")
            );
            assert!(
                store
                    .list_worktree_intents()
                    .await
                    .expect("intents empty")
                    .is_empty()
            );

            // Workspace lease registration.
            let acq = store
                .acquire_workspace_lease(&ws_linked)
                .await
                .expect("acquire ws-001");
            assert_eq!(
                acq,
                WorkspaceLeaseAcquireOutcome::Acquired(Box::new(ws_linked.clone()))
            );

            // Contested identity conflict on `(repo_id, worktree_id)` for Linked workspace.
            let dup_identity = WorkspaceRecordRow {
                workspace_id: "ws-002".to_string(),
                path: "/work/wt-alpha-alias".to_string(),
                ..ws_linked.clone()
            };
            assert_eq!(
                store
                    .acquire_workspace_lease(&dup_identity)
                    .await
                    .expect("identity conflict"),
                WorkspaceLeaseAcquireOutcome::Conflict {
                    conflicting_workspace_id: "ws-001".to_string(),
                    path_conflict: false,
                }
            );

            // Contested active path conflict on `(repo_id, path)` across workspace kinds.
            let dup_path = WorkspaceRecordRow {
                workspace_id: "ws-003".to_string(),
                kind: WorkspaceRecordKind::TaskCopy,
                worktree_id: None,
                ..ws_linked.clone()
            };
            assert_eq!(
                store
                    .acquire_workspace_lease(&dup_path)
                    .await
                    .expect("path conflict"),
                WorkspaceLeaseAcquireOutcome::Conflict {
                    conflicting_workspace_id: "ws-001".to_string(),
                    path_conflict: true,
                }
            );

            // Transition Provisioning -> Active + renew with matching owner & fence.
            let activated = store
                .transition_workspace_lease(
                    "ws-001",
                    "worker-A",
                    1,
                    WorkspaceRecordState::Active,
                    910,
                )
                .await
                .expect("activate ws-001");
            assert!(matches!(
                activated,
                WorkspaceLeaseMutationOutcome::Updated(ref r)
                    if r.state == WorkspaceRecordState::Active && r.lease_fence == 1
            ));
            let renewed = store
                .renew_workspace_lease("ws-001", "worker-A", 1, Some(1_500), 920)
                .await
                .expect("renew ws-001");
            assert!(matches!(
                renewed,
                WorkspaceLeaseMutationOutcome::Updated(ref r)
                    if r.lease_expires_at == Some(1_500)
            ));

            // Reclaim before expiry must fail with LeaseLost!
            let early_reclaim = store
                .reclaim_expired_workspace_lease(
                    "ws-001",
                    1_499,
                    "worker-B",
                    Some(2_500),
                    WorkspaceRecordState::Active,
                    1_499,
                )
                .await
                .expect("early reclaim rejected");
            assert!(matches!(
                early_reclaim,
                WorkspaceLeaseMutationOutcome::LeaseLost {
                    actual_fence: 1,
                    ..
                }
            ));

            // Reclaim at/after expiry bumps fence 1 -> 2 and switches owner to worker-B.
            let reclaimed = store
                .reclaim_expired_workspace_lease(
                    "ws-001",
                    1_500,
                    "worker-B",
                    Some(2_500),
                    WorkspaceRecordState::Active,
                    1_500,
                )
                .await
                .expect("reclaim expired lease");
            assert!(matches!(
                reclaimed,
                WorkspaceLeaseMutationOutcome::Updated(ref r)
                    if r.lease_owner.as_deref() == Some("worker-B") && r.lease_fence == 2
            ));

            // Stale owner worker-A (fence 1) CANNOT renew or revoke worker-B's lease!
            assert!(matches!(
                store
                    .renew_workspace_lease("ws-001", "worker-A", 1, Some(3_000), 1_510)
                    .await
                    .expect("stale renew"),
                WorkspaceLeaseMutationOutcome::LeaseLost {
                    actual_fence: 2,
                    ..
                }
            ));

            // New owner worker-B (fence 2) revokes/releases ws-001 -> frees identity & path!
            let released = store
                .transition_workspace_lease(
                    "ws-001",
                    "worker-B",
                    2,
                    WorkspaceRecordState::Released,
                    1_520,
                )
                .await
                .expect("release ws-001");
            assert!(matches!(
                released,
                WorkspaceLeaseMutationOutcome::Updated(ref r)
                    if r.state == WorkspaceRecordState::Released
            ));

            // Now a new workspace can legally claim the freed `(repo_id, worktree_id)` & path!
            let ws_reacquired = WorkspaceRecordRow {
                workspace_id: "ws-004".to_string(),
                state: WorkspaceRecordState::Active,
                lease_owner: Some("worker-C".to_string()),
                lease_fence: 1,
                lease_expires_at: Some(5_000),
                created_at: 1_530,
                updated_at: 1_530,
                ..ws_linked.clone()
            };
            assert_eq!(
                store
                    .acquire_workspace_lease(&ws_reacquired)
                    .await
                    .expect("reacquire after release"),
                WorkspaceLeaseAcquireOutcome::Acquired(Box::new(ws_reacquired))
            );
        }

        // =====================================================================
        // AC3: OpLog v2 (4 tables): single-tx atomic append + control-slot
        // exclusivity + strict CAS generation fence + concurrent head convergence.
        // =====================================================================
        let op1 = OperationRecord {
            op_id: "op-001".to_string(),
            repo_id: repo_id.to_string(),
            format_version: 2,
            kind: "command".to_string(),
            status: "running".to_string(),
            command_name: Some("commit".to_string()),
            description: Some("initial op".to_string()),
            args_digest: Some("digest-1".to_string()),
            actor: Some("tester".to_string()),
            worktree_id: None,
            scope_kind: OperationScopeKind::Main,
            scope_provenance: OperationScopeProvenance::Declared,
            pre_view_oid: zero40.clone(),
            post_view_oid: sha1(201),
            restores_op_id: None,
            reverts_op_id: None,
            predecessor_map_oid: None,
            causal_context_id: Some("ctx-1".to_string()),
            restorable: true,
            control_slot: Some("slot-main".to_string()),
            claim_owner: Some("pid-1".to_string()),
            start_ts: 1_795_376_100,
            end_ts: None,
        };
        let journal1 = OperationJournalRecord {
            journal_id: "jrn-001".to_string(),
            op_id: "op-001".to_string(),
            phase: "pre_view".to_string(),
            pre_view_oid: Some(zero40.clone()),
            target_view_oid: Some(sha1(201)),
            owner: "pid-1".to_string(),
            updated_at: 1_795_376_101,
            recovery_payload: Some("{}".to_string()),
        };

        let op2a = OperationRecord {
            op_id: "op-002a".to_string(),
            status: "success".to_string(),
            pre_view_oid: sha1(201),
            post_view_oid: sha1(202),
            control_slot: None,
            claim_owner: None,
            start_ts: 1_795_376_110,
            end_ts: Some(1_795_376_111),
            ..op1.clone()
        };
        let op2b = OperationRecord {
            op_id: "op-002b".to_string(),
            status: "success".to_string(),
            pre_view_oid: sha1(201),
            post_view_oid: sha1(203),
            control_slot: None,
            claim_owner: None,
            start_ts: 1_795_376_112,
            end_ts: Some(1_795_376_113),
            ..op1.clone()
        };
        let op3_merge = OperationRecord {
            op_id: "op-003-merge".to_string(),
            kind: "reconcile".to_string(),
            status: "success".to_string(),
            pre_view_oid: sha1(202),
            post_view_oid: sha1(204),
            predecessor_map_oid: Some(sha1(205)),
            control_slot: None,
            claim_owner: None,
            start_ts: 1_795_376_120,
            end_ts: Some(1_795_376_121),
            ..op1.clone()
        };

        for store in stores {
            store
                .append_operation_atomic(&op1, &[], Some(&journal1))
                .await
                .expect("append op1 atomically");

            // Duplicate active running control_slot on same `(repo_id, worktree_id='')` MUST fail atomically!
            let conflicting_slot_op = OperationRecord {
                op_id: "op-slot-conflict".to_string(),
                ..op1.clone()
            };
            let conflict_jrn = OperationJournalRecord {
                journal_id: "jrn-slot-conflict".to_string(),
                op_id: "op-slot-conflict".to_string(),
                ..journal1.clone()
            };
            assert!(
                store
                    .append_operation_atomic(
                        &conflicting_slot_op,
                        &["op-001".to_string()],
                        Some(&conflict_jrn)
                    )
                    .await
                    .is_err(),
                "running control_slot conflict must fail closed"
            );
            assert!(
                store
                    .list_operation_journals("op-slot-conflict")
                    .await
                    .expect("zero orphan journal on rollback")
                    .is_empty()
            );

            // Transition op1 -> success frees the running control slot.
            assert!(
                store
                    .update_operation_status(
                        repo_id,
                        "op-001",
                        "success",
                        Some(&sha1(201)),
                        Some(1_795_376_105),
                    )
                    .await
                    .expect("finish op1")
            );

            // Append concurrent siblings op2a & op2b + converging merge op3_merge with 2 ordered parents.
            store
                .append_operation_atomic(&op2a, &["op-001".to_string()], None)
                .await
                .expect("append op2a");
            store
                .append_operation_atomic(&op2b, &["op-001".to_string()], None)
                .await
                .expect("append op2b");
            store
                .append_operation_atomic(
                    &op3_merge,
                    &["op-002b".to_string(), "op-002a".to_string()],
                    None,
                )
                .await
                .expect("append merge op3");
            let (_, parents) = store
                .get_operation(repo_id, "op-003-merge")
                .await
                .expect("get op3")
                .expect("op3 exists");
            assert_eq!(parents, vec!["op-002b".to_string(), "op-002a".to_string()]);

            // Operation head strict CAS & monotonic generation fence across empty head set.
            let scope = "scope:main";
            assert_eq!(
                store
                    .get_operation_heads(repo_id, scope)
                    .await
                    .expect("initial heads"),
                OperationHeadsSnapshot {
                    generation: 0,
                    heads: Vec::new(),
                }
            );
            let cas1 = store
                .cas_update_operation_heads(repo_id, scope, Some(0), &[], &["op-001".to_string()])
                .await
                .expect("cas init op1");
            assert_eq!(
                cas1,
                OperationHeadCasOutcome::Applied {
                    generation: 1,
                    heads: vec!["op-001".to_string()],
                }
            );
            // Stale CAS returns CasConflict with zero mutation.
            let stale = store
                .cas_update_operation_heads(repo_id, scope, Some(0), &[], &["op-002a".to_string()])
                .await
                .expect("stale cas");
            assert_eq!(
                stale,
                OperationHeadCasOutcome::CasConflict {
                    actual_generation: 1,
                    actual_heads: vec!["op-001".to_string()],
                }
            );
            // Concurrent head convergence: writer A advances op-001 -> op-002a cleanly,
            // writer B still expects op-001 -> retains sibling [op-002a, op-002b],
            // then reconcile op-003-merge converges both siblings -> [op-003-merge]!
            let conv_a = store
                .converge_operation_heads(
                    repo_id,
                    scope,
                    &["op-001".to_string()],
                    &["op-002a".to_string()],
                )
                .await
                .expect("converge A");
            assert!(conv_a.converged_cleanly);
            assert_eq!(conv_a.heads, vec!["op-002a".to_string()]);

            let conv_b = store
                .converge_operation_heads(
                    repo_id,
                    scope,
                    &["op-001".to_string()],
                    &["op-002b".to_string()],
                )
                .await
                .expect("converge B sibling retention");
            assert!(!conv_b.converged_cleanly);
            assert_eq!(
                conv_b.heads,
                vec!["op-002a".to_string(), "op-002b".to_string()]
            );

            let conv_merge = store
                .converge_operation_heads(
                    repo_id,
                    scope,
                    &["op-002a".to_string(), "op-002b".to_string()],
                    &["op-003-merge".to_string()],
                )
                .await
                .expect("converge siblings to single head");
            assert!(conv_merge.converged_cleanly);
            assert_eq!(conv_merge.heads, vec!["op-003-merge".to_string()]);
            assert_eq!(conv_merge.generation, 4);

            // Journal update & delete.
            let mut jrn_upd = journal1.clone();
            jrn_upd.phase = "publish".to_string();
            jrn_upd.updated_at = 1_795_376_150;
            store
                .upsert_operation_journal(&jrn_upd)
                .await
                .expect("update journal");
            assert_eq!(
                store
                    .list_operation_journals("op-001")
                    .await
                    .expect("list journal"),
                vec![jrn_upd]
            );
            assert!(
                store
                    .delete_operation_journal("jrn-001")
                    .await
                    .expect("delete journal")
            );

            // Single-transaction 4-table atomic append + head convergence AND rollback check.
            let conc_scope = "scope:concurrent";
            let base_op = OperationRecord {
                op_id: "op-conc-base".to_string(),
                status: "succeeded".to_string(),
                control_slot: None,
                claim_owner: None,
                start_ts: 1_795_376_500,
                end_ts: Some(1_795_376_501),
                ..op1.clone()
            };
            let base_jrn = OperationJournalRecord {
                journal_id: "jrn-conc-base".to_string(),
                op_id: "op-conc-base".to_string(),
                phase: "complete".to_string(),
                pre_view_oid: Some(sha1(1)),
                target_view_oid: Some(sha1(2)),
                owner: "worker-base".to_string(),
                updated_at: 1_795_376_501,
                recovery_payload: None,
            };
            let base_out = store
                .append_operation_and_converge_heads(
                    &base_op,
                    &[],
                    Some(&base_jrn),
                    conc_scope,
                    &[],
                    &["op-conc-base".to_string()],
                )
                .await
                .expect("atomic 4-table base append");
            assert!(base_out.converged_cleanly);
            assert_eq!(base_out.generation, 1);
            assert_eq!(base_out.heads, vec!["op-conc-base".to_string()]);

            // Verify rollback atomicity: colliding journal_id must abort the entire
            // 4-table transaction (neither operation nor operation_head mutates).
            let abort_op = OperationRecord {
                op_id: "op-conc-aborted".to_string(),
                start_ts: 1_795_376_502,
                ..base_op.clone()
            };
            let collide_jrn = OperationJournalRecord {
                journal_id: "jrn-conc-base".to_string(),
                op_id: "op-conc-aborted".to_string(),
                ..base_jrn.clone()
            };
            assert!(
                store
                    .append_operation_and_converge_heads(
                        &abort_op,
                        &["op-conc-base".to_string()],
                        Some(&collide_jrn),
                        conc_scope,
                        &["op-conc-base".to_string()],
                        &["op-conc-aborted".to_string()],
                    )
                    .await
                    .is_err(),
                "duplicate journal_id must roll back 4-table OpLog transaction"
            );
            assert_eq!(
                store
                    .get_operation(repo_id, "op-conc-aborted")
                    .await
                    .expect("aborted op absent"),
                None
            );
            assert_eq!(
                store
                    .get_operation_heads(repo_id, conc_scope)
                    .await
                    .expect("heads unchanged after rollback"),
                OperationHeadsSnapshot {
                    generation: 1,
                    heads: vec!["op-conc-base".to_string()],
                }
            );

            // Real simultaneous multi-task concurrent head convergence stress check:
            // 6 writers launch behind a barrier with identical expected head `op-conc-base`.
            const CONCURRENT_WRITERS: usize = 6;
            let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENT_WRITERS));
            let mut handles = Vec::with_capacity(CONCURRENT_WRITERS);
            for i in 0..CONCURRENT_WRITERS {
                let s = Arc::clone(store);
                let b = Arc::clone(&barrier);
                let template = base_op.clone();
                handles.push(tokio::spawn(async move {
                    let op_id = format!("op-conc-w{i}");
                    let op = OperationRecord {
                        op_id: op_id.clone(),
                        start_ts: 1_795_376_600 + i as i64,
                        end_ts: Some(1_795_376_601 + i as i64),
                        ..template
                    };
                    let jrn = OperationJournalRecord {
                        journal_id: format!("jrn-conc-w{i}"),
                        op_id: op_id.clone(),
                        phase: "complete".to_string(),
                        pre_view_oid: Some(sha1(2)),
                        target_view_oid: Some(sha1(20 + i as u64)),
                        owner: format!("worker-{i}"),
                        updated_at: 1_795_376_601 + i as i64,
                        recovery_payload: None,
                    };
                    b.wait().await;
                    s.append_operation_and_converge_heads(
                        &op,
                        &["op-conc-base".to_string()],
                        Some(&jrn),
                        conc_scope,
                        &["op-conc-base".to_string()],
                        &[op_id],
                    )
                    .await
                }));
            }
            let mut clean_wins = 0usize;
            for h in handles {
                let outcome = h
                    .await
                    .expect("concurrent writer task join")
                    .expect("concurrent append_operation_and_converge_heads");
                if outcome.converged_cleanly {
                    clean_wins += 1;
                }
            }
            assert_eq!(
                clean_wins, 1,
                "exactly one concurrent writer may win clean convergence from base head"
            );
            let expected_siblings: Vec<String> = (0..CONCURRENT_WRITERS)
                .map(|i| format!("op-conc-w{i}"))
                .collect();
            let post_conc = store
                .get_operation_heads(repo_id, conc_scope)
                .await
                .expect("read post-concurrent heads");
            assert_eq!(post_conc.generation, 1 + CONCURRENT_WRITERS as u64);
            assert_eq!(post_conc.heads, expected_siblings);
            for i in 0..CONCURRENT_WRITERS {
                let op_id = format!("op-conc-w{i}");
                let (_, parents) = store
                    .get_operation(repo_id, &op_id)
                    .await
                    .expect("get concurrent op")
                    .expect("concurrent op present");
                assert_eq!(parents, vec!["op-conc-base".to_string()]);
                assert_eq!(
                    store
                        .list_operation_journals(&op_id)
                        .await
                        .expect("list concurrent journal")
                        .len(),
                    1
                );
            }

            // Reconcile all 6 concurrent sibling heads into one final head atomically.
            let final_op = OperationRecord {
                op_id: "op-conc-final".to_string(),
                start_ts: 1_795_376_700,
                end_ts: Some(1_795_376_701),
                ..base_op.clone()
            };
            let final_out = store
                .append_operation_and_converge_heads(
                    &final_op,
                    &expected_siblings,
                    None,
                    conc_scope,
                    &expected_siblings,
                    &["op-conc-final".to_string()],
                )
                .await
                .expect("converge all concurrent sibling heads");
            assert!(final_out.converged_cleanly);
            assert_eq!(final_out.generation, 2 + CONCURRENT_WRITERS as u64);
            assert_eq!(final_out.heads, vec!["op-conc-final".to_string()]);
        }

        // =====================================================================
        // AC4: `change_identity`, `change_revision`, `change_predecessor`,
        // `ai_operation_link` (4 tables).
        // =====================================================================
        let ch1 = ChangeIdentityRecord {
            change_id: "chg-alpha-0001".to_string(),
            repo_id: repo_id.to_string(),
            origin: "commit".to_string(),
            created_op_id: "op-001".to_string(),
            created_at: 1_795_376_200,
        };
        let ch2 = ChangeIdentityRecord {
            change_id: "chg-alpha-0002".to_string(),
            repo_id: repo_id.to_string(),
            origin: "amend".to_string(),
            created_op_id: "op-002a".to_string(),
            created_at: 1_795_376_201,
        };
        let rev1 = ChangeRevisionRecord {
            change_id: "chg-alpha-0001".to_string(),
            commit_oid: sha1(301),
            created_op_id: "op-001".to_string(),
            visibility: "hidden".to_string(),
            revision_ordinal: 1,
        };
        let rev2 = ChangeRevisionRecord {
            change_id: "chg-alpha-0001".to_string(),
            commit_oid: sha1(302),
            created_op_id: "op-002a".to_string(),
            visibility: "visible".to_string(),
            revision_ordinal: 2,
        };
        let pred = ChangePredecessorRecord {
            successor_oid: sha1(302),
            predecessor_oid: sha1(301),
            op_id: "op-002a".to_string(),
            relation_kind: "amend".to_string(),
            ordinal: 0,
        };
        let ai_link = AiOperationLinkRecord {
            operation_id: "op-002a".to_string(),
            session_id: Some("sess-1".to_string()),
            run_id: Some("run-9".to_string()),
            tool_invocation_id: Some("tool-3".to_string()),
            intent_id: Some("intent-7".to_string()),
            repo_id: repo_id.to_string(),
            worktree_id: Some("wt-alpha".to_string()),
            workspace_id: Some("ws-004".to_string()),
            lease_generation: Some(2),
            config_provenance_digest: Some("sha256:abc".to_string()),
            redaction_version: "v1".to_string(),
            change_id: Some("chg-alpha-0001".to_string()),
        };

        for store in stores {
            store.upsert_change_identity(&ch1).await.expect("ch1");
            store.upsert_change_identity(&ch2).await.expect("ch2");
            store.upsert_change_revision(&rev1).await.expect("rev1");
            store.upsert_change_revision(&rev2).await.expect("rev2");
            store.upsert_change_predecessor(&pred).await.expect("pred");
            store
                .upsert_ai_operation_link(&ai_link)
                .await
                .expect("ai link");

            assert_eq!(
                store
                    .list_change_identities_by_prefix(repo_id, "chg-alpha-")
                    .await
                    .expect("prefix query"),
                vec![ch1.clone(), ch2.clone()]
            );
            assert_eq!(
                store
                    .list_change_revisions("chg-alpha-0001")
                    .await
                    .expect("list revisions"),
                vec![rev1.clone(), rev2.clone()]
            );
            assert_eq!(
                store
                    .find_change_revisions_by_commit(&sha1(302))
                    .await
                    .expect("find rev by commit"),
                vec![rev2.clone()]
            );
            assert_eq!(
                store
                    .list_change_predecessors(&sha1(302))
                    .await
                    .expect("list preds"),
                vec![pred.clone()]
            );
            assert_eq!(
                store
                    .get_ai_operation_link("op-002a")
                    .await
                    .expect("get ai link"),
                Some(ai_link.clone())
            );
            assert_eq!(
                store
                    .list_ai_operation_links_by_change(repo_id, "chg-alpha-0001")
                    .await
                    .expect("links by change"),
                vec![ai_link.clone()]
            );
        }

        // =====================================================================
        // AC5: `working_dirty*`, `layer*`, `sparse_view*` (6 worktree-scoped tables).
        // =====================================================================
        let dirty_meta_main = WorkingDirtyMetaRecord {
            worktree_id: String::new(),
            state: "fresh".to_string(),
            index_fingerprint: Some("fp-main".to_string()),
            head_oid: Some(sha1(401)),
            scanned_at: Some("2026-11-22T18:45:00.000000Z".to_string()),
            scan_lock_pid: None,
            scan_lock_at: None,
        };
        let dirty_entries_main = vec![
            WorkingDirtyEntryRecord {
                worktree_id: String::new(),
                path: "src/a.rs".to_string(),
                kind: "modified".to_string(),
                source: "scan".to_string(),
                marked_at: "2026-11-22T18:45:00.000000Z".to_string(),
                verified_at: Some("2026-11-22T18:45:00.000000Z".to_string()),
            },
            WorkingDirtyEntryRecord {
                worktree_id: String::new(),
                path: "src/b.rs".to_string(),
                kind: "staged_new".to_string(),
                source: "scan".to_string(),
                marked_at: "2026-11-22T18:45:00.000000Z".to_string(),
                verified_at: None,
            },
        ];
        let dirty_meta_wt1 = WorkingDirtyMetaRecord {
            worktree_id: "wt-alpha".to_string(),
            state: "stale".to_string(),
            index_fingerprint: Some("fp-wt1".to_string()),
            head_oid: Some(zero40.clone()),
            scanned_at: None,
            scan_lock_pid: Some(4242),
            scan_lock_at: Some("2026-11-22T18:45:01.000000Z".to_string()),
        };

        let layer_main = LayerRecord {
            worktree_id: String::new(),
            name: "env-local".to_string(),
            source: "/layers/env-local".to_string(),
            priority: 10,
            enabled: true,
            created_at: "2026-11-22T18:46:00Z".to_string(),
            updated_at: "2026-11-22T18:46:00Z".to_string(),
        };
        let layer_wt1 = LayerRecord {
            worktree_id: "wt-alpha".to_string(),
            name: "env-local".to_string(), // same name allowed in different worktree!
            source: "/layers/wt-alpha-env".to_string(),
            priority: 5,
            enabled: false,
            created_at: "2026-11-22T18:46:01Z".to_string(),
            updated_at: "2026-11-22T18:46:01Z".to_string(),
        };
        let layer_path_main = LayerPathRecord {
            worktree_id: String::new(),
            layer_name: "env-local".to_string(),
            path: ".env.local".to_string(),
            content_hash: sha256(501),
            materialized_at: "2026-11-22T18:46:02Z".to_string(),
        };
        let layer_path_wt1 = LayerPathRecord {
            worktree_id: "wt-alpha".to_string(),
            layer_name: "env-local".to_string(),
            path: ".env.local".to_string(),
            content_hash: sha256(502),
            materialized_at: "2026-11-22T18:46:03Z".to_string(),
        };

        for store in stores {
            store
                .replace_working_dirty_snapshot(&dirty_meta_main, &dirty_entries_main)
                .await
                .expect("replace main dirty");
            store
                .replace_working_dirty_snapshot(&dirty_meta_wt1, &[])
                .await
                .expect("replace wt1 dirty");
            let manual_mark = WorkingDirtyEntryRecord {
                worktree_id: "wt-alpha".to_string(),
                path: "README.md".to_string(),
                kind: "unknown".to_string(),
                source: "manual".to_string(),
                marked_at: "2026-11-22T18:45:05.000000Z".to_string(),
                verified_at: None,
            };
            store
                .upsert_working_dirty_entry(&manual_mark)
                .await
                .expect("manual dirty mark");

            store.upsert_layer(&layer_main).await.expect("layer main");
            store.upsert_layer(&layer_wt1).await.expect("layer wt1");
            store
                .upsert_layer_path(&layer_path_main)
                .await
                .expect("layer path main");
            store
                .upsert_layer_path(&layer_path_wt1)
                .await
                .expect("layer path wt1");

            store
                .replace_sparse_view("", true, &["src/".to_string(), "Cargo.toml".to_string()])
                .await
                .expect("sparse main");
            store
                .replace_sparse_view("wt-alpha", false, &["docs/".to_string()])
                .await
                .expect("sparse wt1");

            assert_eq!(
                store.get_sparse_view("").await.expect("get sparse main"),
                Some(SparseViewSnapshot {
                    worktree_id: String::new(),
                    enabled: true,
                    patterns: vec!["src/".to_string(), "Cargo.toml".to_string()],
                })
            );
            assert_eq!(
                store
                    .get_sparse_view("wt-alpha")
                    .await
                    .expect("get sparse wt1"),
                Some(SparseViewSnapshot {
                    worktree_id: "wt-alpha".to_string(),
                    enabled: false,
                    patterns: vec!["docs/".to_string()],
                })
            );

            // Clearing wt-alpha sparse view leaves main intact.
            store
                .clear_sparse_view("wt-alpha")
                .await
                .expect("clear wt1 sparse");
            assert_eq!(
                store
                    .get_sparse_view("wt-alpha")
                    .await
                    .expect("wt1 cleared"),
                None
            );
            assert!(
                store
                    .get_sparse_view("")
                    .await
                    .expect("main intact")
                    .is_some()
            );

            // Deleting main layer removes its paths while leaving wt-alpha untouched.
            assert!(
                store
                    .delete_layer("", "env-local")
                    .await
                    .expect("delete main layer")
            );
            assert!(
                store
                    .list_layer_paths("", None)
                    .await
                    .expect("main layer paths gone")
                    .is_empty()
            );
            assert_eq!(
                store
                    .list_layer_paths("wt-alpha", None)
                    .await
                    .expect("wt1 layer path intact"),
                vec![layer_path_wt1.clone()]
            );
            // Re-insert main layer & path for final cross-backend diff check.
            store
                .upsert_layer(&layer_main)
                .await
                .expect("restore layer main");
            store
                .upsert_layer_path(&layer_path_main)
                .await
                .expect("restore layer path main");

            // Concurrent sparse-view snapshot consistency check: a writer alternates
            // between `(true, ["src/", "docs/"])` and `(false, [])` while a concurrent
            // reader verifies no mixed `(true, [])` or `(false, ["src/", "docs/"])` view.
            let sparse_wt = "wt-sparse-snap";
            store
                .replace_sparse_view(sparse_wt, false, &[])
                .await
                .expect("seed sparse snap worktree");
            let writer_store = Arc::clone(store);
            let reader_store = Arc::clone(store);
            let writer_task = tokio::spawn(async move {
                for step in 0..24usize {
                    if step % 2 == 0 {
                        writer_store
                            .replace_sparse_view(
                                sparse_wt,
                                true,
                                &["src/".to_string(), "docs/".to_string()],
                            )
                            .await
                            .expect("toggle sparse on");
                    } else {
                        writer_store
                            .replace_sparse_view(sparse_wt, false, &[])
                            .await
                            .expect("toggle sparse off");
                    }
                }
            });
            let reader_task = tokio::spawn(async move {
                for _ in 0..48usize {
                    let snap = reader_store
                        .get_sparse_view(sparse_wt)
                        .await
                        .expect("snapshot read sparse_view")
                        .expect("sparse_view exists");
                    let expected_on = vec!["src/".to_string(), "docs/".to_string()];
                    assert!(
                        (snap.enabled && snap.patterns == expected_on)
                            || (!snap.enabled && snap.patterns.is_empty()),
                        "observed torn sparse_view snapshot: {snap:?}"
                    );
                }
            });
            writer_task.await.expect("sparse writer join");
            reader_task.await.expect("sparse reader join");
            store
                .clear_sparse_view(sparse_wt)
                .await
                .expect("clean up sparse snap worktree");
        }

        // Cross-backend exact equivalence assertions when running dual-feature!
        if stores.len() == 2 {
            let sq = &stores[0];
            let rb = &stores[1];
            assert_eq!(
                sq.list_sequence_states().await.expect("sq"),
                rb.list_sequence_states().await.expect("rb")
            );
            assert_eq!(
                sq.list_rebase_states().await.expect("sq"),
                rb.list_rebase_states().await.expect("rb")
            );
            assert_eq!(
                sq.list_bisect_states().await.expect("sq"),
                rb.list_bisect_states().await.expect("rb")
            );
            assert_eq!(
                sq.list_worktree_registry_capabilities().await.expect("sq"),
                rb.list_worktree_registry_capabilities().await.expect("rb")
            );
            assert_eq!(
                sq.list_worktree_lifecycles().await.expect("sq"),
                rb.list_worktree_lifecycles().await.expect("rb")
            );
            assert_eq!(
                sq.list_worktree_intents().await.expect("sq"),
                rb.list_worktree_intents().await.expect("rb")
            );
            assert_eq!(
                sq.list_workspace_records(repo_id, None).await.expect("sq"),
                rb.list_workspace_records(repo_id, None).await.expect("rb")
            );
            assert_eq!(
                sq.list_operations(repo_id).await.expect("sq"),
                rb.list_operations(repo_id).await.expect("rb")
            );
            assert_eq!(
                sq.get_operation_heads(repo_id, "scope:main")
                    .await
                    .expect("sq"),
                rb.get_operation_heads(repo_id, "scope:main")
                    .await
                    .expect("rb")
            );
            assert_eq!(
                sq.get_operation_heads(repo_id, "scope:concurrent")
                    .await
                    .expect("sq"),
                rb.get_operation_heads(repo_id, "scope:concurrent")
                    .await
                    .expect("rb")
            );
            assert_eq!(
                sq.list_change_identities_by_prefix(repo_id, "")
                    .await
                    .expect("sq"),
                rb.list_change_identities_by_prefix(repo_id, "")
                    .await
                    .expect("rb")
            );
            assert_eq!(
                sq.list_change_revisions("chg-alpha-0001")
                    .await
                    .expect("sq"),
                rb.list_change_revisions("chg-alpha-0001")
                    .await
                    .expect("rb")
            );
            assert_eq!(
                sq.list_change_predecessors(&sha1(302)).await.expect("sq"),
                rb.list_change_predecessors(&sha1(302)).await.expect("rb")
            );
            assert_eq!(
                sq.list_ai_operation_links_by_change(repo_id, "chg-alpha-0001")
                    .await
                    .expect("sq"),
                rb.list_ai_operation_links_by_change(repo_id, "chg-alpha-0001")
                    .await
                    .expect("rb")
            );
            for wt in ["", "wt-alpha"] {
                assert_eq!(
                    sq.get_working_dirty_meta(wt).await.expect("sq"),
                    rb.get_working_dirty_meta(wt).await.expect("rb")
                );
                assert_eq!(
                    sq.list_working_dirty_entries(wt).await.expect("sq"),
                    rb.list_working_dirty_entries(wt).await.expect("rb")
                );
                assert_eq!(
                    sq.list_layers(wt).await.expect("sq"),
                    rb.list_layers(wt).await.expect("rb")
                );
                assert_eq!(
                    sq.list_layer_paths(wt, None).await.expect("sq"),
                    rb.list_layer_paths(wt, None).await.expect("rb")
                );
                assert_eq!(
                    sq.get_sparse_view(wt).await.expect("sq"),
                    rb.get_sparse_view(wt).await.expect("rb")
                );
            }
        }
    }

    /// `RB-05` (AC1–AC5): Runs dual-backend equivalence (`SqliteGitStateStore`
    /// vs `RedbGitStateStore`) when both features are enabled, and full
    /// `RedbGitStateStore` AC1–AC5 + RB-04b upgrade compatibility regression
    /// under `--no-default-features --features storage-redb,storage-queue`.
    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    async fn dual_backend_22_workflow_worktree_oplog_tables_equivalence() {
        let dir = tempdir().expect("create RB-05 tempdir");
        let redb_path = dir.path().join("git.redb");
        let redb_store = RedbGitStateStore::create(&redb_path).expect("create RedbGitStateStore");
        assert_eq!(redb_store.backend(), StorageBackendKind::Redb);

        let mut stores: Vec<Arc<dyn GitStateStore>> = Vec::new();
        #[cfg(feature = "storage-sqlite")]
        {
            let sqlite_store = SqliteGitStateStore::create(dir.path().join("libra.db"))
                .await
                .expect("create SqliteGitStateStore");
            assert_eq!(sqlite_store.backend(), StorageBackendKind::Sqlite);
            stores.push(Arc::new(sqlite_store));
        }
        stores.push(Arc::new(redb_store));

        run_all_rb05_ac_assertions(&stores).await;

        // Verify backward-compatible reopen & atomic upgrade on an existing
        // `RedbGitStore` (`RB-04`/`RB-04b`) file that does not yet have `RB-05` tables.
        let shared_path = dir.path().join("shared_git.redb");
        let git_store = crate::storage::git_store::RedbGitStore::create(&shared_path)
            .expect("create RB-04b RedbGitStore");
        let main_ref = crate::storage::git_store::RefKey::LocalBranch {
            name: "main".to_string(),
        };
        let c1 = crate::storage::git_store::RefTarget::Direct(sha1(901));
        crate::storage::git_store::GitStore::update_ref_cas(
            &git_store,
            &main_ref,
            None,
            Some(&c1),
            &[],
        )
        .await
        .expect("seed main ref in shared git.redb");
        drop(git_store);

        // Read-only open before RB-05 upgrade must succeed cleanly without writing disk
        // and report identical baseline worktree capabilities as after writable upgrade.
        let ro_before = RedbGitStateStore::open_read_only(&shared_path)
            .expect("open_read_only on pre-RB-05 git.redb");
        assert_eq!(
            ro_before.get_sequence_state("").await.expect("empty seq"),
            None
        );
        assert_eq!(
            ro_before
                .list_worktree_registry_capabilities()
                .await
                .expect("baseline capabilities on pre-RB-05 read-only open"),
            INITIAL_WORKTREE_REGISTRY_CAPABILITIES.to_vec()
        );
        assert!(
            ro_before
                .list_operations("r")
                .await
                .expect("empty ops")
                .is_empty()
        );
        drop(ro_before);

        // Writable open upgrades cleanly without disturbing existing RB-04b refs.
        let rw_upgraded = RedbGitStateStore::open_read_write(&shared_path)
            .expect("upgrade shared git.redb with RB-05 tables");
        assert_eq!(
            rw_upgraded
                .list_worktree_registry_capabilities()
                .await
                .expect("baseline capabilities after RB-05 upgrade"),
            INITIAL_WORKTREE_REGISTRY_CAPABILITIES.to_vec()
        );
        rw_upgraded
            .replace_sparse_view("", true, &["src/".to_string()])
            .await
            .expect("write sparse view after upgrade");
        drop(rw_upgraded);

        let git_reopened = crate::storage::git_store::RedbGitStore::open_read_only(&shared_path)
            .expect("reopen RB-04b RedbGitStore after RB-05 upgrade");
        assert_eq!(
            crate::storage::git_store::GitStore::get_ref(&git_reopened, &main_ref)
                .await
                .expect("read preserved ref"),
            Some(c1)
        );
    }

    /// `RB-05` single-feature SQLite regression smoke test covering AC1–AC5
    /// without requiring `storage-redb`.
    #[cfg(feature = "storage-sqlite")]
    #[tokio::test]
    async fn sqlite_workflow_state_tables_smoke() {
        let dir = tempdir().expect("create RB-05 sqlite smoke tempdir");
        let db_path = dir.path().join("libra.db");
        let sqlite_store = SqliteGitStateStore::create(&db_path)
            .await
            .expect("create SqliteGitStateStore");
        assert_eq!(sqlite_store.backend(), StorageBackendKind::Sqlite);
        let stores: [Arc<dyn GitStateStore>; 1] = [Arc::new(sqlite_store)];
        run_all_rb05_ac_assertions(&stores).await;

        // Verify read-only SQLite connection supports transactional snapshot reads.
        let ro = SqliteGitStateStore::open_read_only(&db_path)
            .await
            .expect("open_read_only SqliteGitStateStore");
        assert!(ro.get_sparse_view("").await.expect("ro sparse").is_some());
        assert!(
            ro.get_operation("repo-rb05-001", "op-conc-final")
                .await
                .expect("ro op")
                .is_some()
        );
    }
}
